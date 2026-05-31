//! SQL-level `PRAGMA` dispatcher (SQLR-13).
//!
//! sqlparser-rs already produces a `Statement::Pragma` AST variant, but
//! its pragma-value parser is narrow: only numbers, single/double quoted
//! strings, and `?` placeholders are accepted. Bare identifiers like
//! `OFF` / `NONE` (which SQLite has historically accepted in PRAGMA
//! position) get rejected before the dispatcher ever sees them.
//!
//! We bypass that constraint by intercepting `PRAGMA` statements before
//! `Parser::parse_sql` runs: peek the first non-whitespace token, and
//! if it's the `PRAGMA` keyword, route through this module's tokenizer
//! pass instead. Non-PRAGMA input falls straight through (returns
//! `Ok(None)`).
//!
//! This is the first SQL pragma SQLRite ships. The dispatcher is a
//! single function with a `match` on pragma name; switch to a registry
//! when the second pragma lands.

use prettytable::{Cell as PrintCell, Row as PrintRow, Table as PrintTable};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::error::{Result, SQLRiteError};
use crate::mvcc::JournalMode;
use crate::sql::CommandOutput;
use crate::sql::db::database::Database;

/// Parsed pragma value.
///
/// We distinguish between bare identifiers and quoted strings so the
/// per-pragma handler can reject ambiguous shapes (e.g. a future numeric
/// pragma can refuse `'42'` while accepting `42`). Numbers are kept as
/// the raw lexeme so the handler can pick its own integer / float
/// parsing strategy.
#[derive(Debug, Clone, PartialEq)]
pub enum PragmaValue {
    /// Numeric literal (with an optional leading `-` folded into the lexeme).
    Number(String),
    /// Bare identifier, e.g. `OFF` / `NONE` / `WAL`.
    Identifier(String),
    /// Quoted string literal, e.g. `'OFF'` or `"NONE"`.
    String(String),
}

/// One parsed `PRAGMA` statement. `value` is `None` for the read form
/// (`PRAGMA name;`).
#[derive(Debug, Clone, PartialEq)]
pub struct PragmaStatement {
    pub name: String,
    pub value: Option<PragmaValue>,
}

/// Returns `Ok(Some(stmt))` when `sql` is a `PRAGMA` statement,
/// `Ok(None)` otherwise. Errors only when the input *is* shaped like
/// `PRAGMA …` but malformed — that path used to surface as a sqlparser
/// `ParserError`; with this module taking over, the error becomes a
/// typed `SQLRiteError::General` so SDK consumers see a stable shape.
pub fn try_parse_pragma(sql: &str) -> Result<Option<PragmaStatement>> {
    let dialect = SQLiteDialect {};
    let tokens = Tokenizer::new(&dialect, sql)
        .tokenize()
        .map_err(|e| SQLRiteError::General(format!("PRAGMA tokenize error: {e}")))?;

    let mut iter = tokens
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .peekable();

    // First non-whitespace token must be the PRAGMA keyword. Anything
    // else means this isn't ours — let sqlparser take over.
    match iter.peek() {
        Some(Token::Word(w)) if w.keyword == Keyword::PRAGMA => {
            iter.next();
        }
        _ => return Ok(None),
    }

    let name = match iter.next() {
        Some(Token::Word(w)) => w.value,
        Some(other) => {
            return Err(SQLRiteError::General(format!(
                "PRAGMA: expected pragma name, got {other:?}"
            )));
        }
        None => {
            return Err(SQLRiteError::General(
                "PRAGMA: missing pragma name".to_string(),
            ));
        }
    };

    let value = match iter.peek() {
        None | Some(Token::SemiColon) => None,
        Some(Token::Eq) => {
            iter.next();
            Some(read_pragma_value(&mut iter)?)
        }
        Some(Token::LParen) => {
            iter.next();
            let v = read_pragma_value(&mut iter)?;
            match iter.next() {
                Some(Token::RParen) => {}
                Some(other) => {
                    return Err(SQLRiteError::General(format!(
                        "PRAGMA: expected ')' to close parenthesised value, got {other:?}"
                    )));
                }
                None => {
                    return Err(SQLRiteError::General(
                        "PRAGMA: expected ')' to close parenthesised value".to_string(),
                    ));
                }
            }
            Some(v)
        }
        Some(other) => {
            return Err(SQLRiteError::General(format!(
                "PRAGMA: expected '=', '(', ';' or end of statement after name, got {other:?}"
            )));
        }
    };

    // Optional terminating semicolon. Anything after that is a multi-
    // statement string, which the regular dispatcher already rejects;
    // mirror that policy here so PRAGMA isn't a sneaky bypass.
    if matches!(iter.peek(), Some(Token::SemiColon)) {
        iter.next();
    }
    if let Some(extra) = iter.next() {
        return Err(SQLRiteError::General(format!(
            "PRAGMA: unexpected trailing content {extra:?}"
        )));
    }

    Ok(Some(PragmaStatement { name, value }))
}

fn read_pragma_value<I>(iter: &mut std::iter::Peekable<I>) -> Result<PragmaValue>
where
    I: Iterator<Item = Token>,
{
    // `PRAGMA name = -0.5;` / `PRAGMA name = -1;` — fold a leading sign
    // into the number lexeme so the handler's parse() sees it as one
    // token. The setter validates the range; we just preserve the
    // sign here.
    let mut neg = false;
    let first = iter.next().ok_or_else(|| {
        SQLRiteError::General("PRAGMA: missing value after '=' or '('".to_string())
    })?;

    let tok = if matches!(first, Token::Minus) {
        neg = true;
        iter.next()
            .ok_or_else(|| SQLRiteError::General("PRAGMA: missing value after '-'".to_string()))?
    } else {
        first
    };

    Ok(match tok {
        Token::Number(s, _) => {
            if neg {
                PragmaValue::Number(format!("-{s}"))
            } else {
                PragmaValue::Number(s)
            }
        }
        Token::SingleQuotedString(s) | Token::DoubleQuotedString(s) => {
            if neg {
                return Err(SQLRiteError::General(
                    "PRAGMA: unary '-' is only valid in front of a number".to_string(),
                ));
            }
            PragmaValue::String(s)
        }
        Token::Word(w) => {
            if neg {
                return Err(SQLRiteError::General(
                    "PRAGMA: unary '-' is only valid in front of a number".to_string(),
                ));
            }
            PragmaValue::Identifier(w.value)
        }
        other => {
            return Err(SQLRiteError::General(format!(
                "PRAGMA: unsupported value token {other:?}"
            )));
        }
    })
}

/// Dispatch a parsed `PRAGMA` statement against the database. New
/// pragmas plug in here.
pub fn execute_pragma(stmt: PragmaStatement, db: &mut Database) -> Result<CommandOutput> {
    match stmt.name.to_ascii_lowercase().as_str() {
        "auto_vacuum" => pragma_auto_vacuum(stmt.value, db),
        "journal_mode" => pragma_journal_mode(stmt.value, db),
        "table_list" => pragma_table_list(stmt.value, db),
        other => Err(SQLRiteError::NotImplemented(format!(
            "PRAGMA '{other}' is not supported"
        ))),
    }
}

/// `PRAGMA journal_mode;` (read) or `PRAGMA journal_mode = wal | mvcc;`
/// (write). Phase 11.3 — the toggle is observable but doesn't change
/// query behaviour yet; 11.4 wires `Mvcc` mode into the read/write
/// paths. The set form returns the new mode (SQLite parity); the
/// read form returns the current mode.
fn pragma_journal_mode(value: Option<PragmaValue>, db: &mut Database) -> Result<CommandOutput> {
    match value {
        None => render_journal_mode(db.journal_mode()),
        Some(v) => {
            let target = parse_journal_mode_target(&v)?;
            db.set_journal_mode(target)?;
            // SQLite renders the post-set mode as a result row;
            // mirror that so callers can confirm the toggle landed.
            render_journal_mode(db.journal_mode())
        }
    }
}

fn render_journal_mode(mode: JournalMode) -> Result<CommandOutput> {
    let mut t = PrintTable::new();
    t.add_row(PrintRow::new(vec![PrintCell::new("journal_mode")]));
    t.add_row(PrintRow::new(vec![PrintCell::new(mode.as_str())]));
    Ok(CommandOutput {
        status: "PRAGMA journal_mode executed. 1 row returned.".to_string(),
        rendered: Some(t.to_string()),
    })
}

fn parse_journal_mode_target(value: &PragmaValue) -> Result<JournalMode> {
    let s = match value {
        PragmaValue::Identifier(s) | PragmaValue::String(s) => s.as_str(),
        PragmaValue::Number(s) => {
            return Err(SQLRiteError::General(format!(
                "PRAGMA journal_mode: expected 'wal' or 'mvcc', got numeric '{s}'"
            )));
        }
    };
    JournalMode::from_str_lossless(s).ok_or_else(|| {
        SQLRiteError::General(format!(
            "PRAGMA journal_mode: unknown mode '{s}' (supported: 'wal', 'mvcc')"
        ))
    })
}

/// `PRAGMA table_list;` (SQLR-10) — lists the tables in the database so
/// embedding SDKs can introspect the catalog (discover existing tables
/// for idempotent migrations) without parsing a rendered `sqlrite_master`
/// query. Read-only: the write form is rejected.
///
/// Columns mirror SQLite's `PRAGMA table_list`: `schema`, `name`, `type`,
/// `ncol`, `wr`, `strict`. SQLRite has a single schema (`main`), no
/// WITHOUT ROWID tables, and no STRICT tables, so `wr` and `strict` are
/// always `0`. The synthetic catalog table `sqlrite_master` is listed
/// last (matching SQLite, which lists `sqlite_schema`).
fn pragma_table_list(value: Option<PragmaValue>, db: &Database) -> Result<CommandOutput> {
    if value.is_some() {
        return Err(SQLRiteError::General(
            "PRAGMA table_list does not take a value".to_string(),
        ));
    }

    let mut t = PrintTable::new();
    t.add_row(PrintRow::new(vec![
        PrintCell::new("schema"),
        PrintCell::new("name"),
        PrintCell::new("type"),
        PrintCell::new("ncol"),
        PrintCell::new("wr"),
        PrintCell::new("strict"),
    ]));

    let mut names: Vec<&String> = db.tables.keys().collect();
    names.sort();
    let mut row_count = 0usize;
    for name in names {
        let ncol = db.tables[name].columns.len();
        t.add_row(PrintRow::new(vec![
            PrintCell::new("main"),
            PrintCell::new(name),
            PrintCell::new("table"),
            PrintCell::new(&ncol.to_string()),
            PrintCell::new("0"),
            PrintCell::new("0"),
        ]));
        row_count += 1;
    }

    // The catalog table itself, listed last (SQLite lists sqlite_schema).
    t.add_row(PrintRow::new(vec![
        PrintCell::new("main"),
        PrintCell::new(crate::sql::pager::MASTER_TABLE_NAME),
        PrintCell::new("table"),
        PrintCell::new("5"),
        PrintCell::new("0"),
        PrintCell::new("0"),
    ]));
    row_count += 1;

    Ok(CommandOutput {
        status: format!("PRAGMA table_list executed. {row_count} rows returned."),
        rendered: Some(t.to_string()),
    })
}

/// `PRAGMA auto_vacuum;` (read) or `PRAGMA auto_vacuum = N | OFF | NONE;`
/// (write). Reuses [`Database::set_auto_vacuum_threshold`] so the range
/// validation lives in exactly one place.
fn pragma_auto_vacuum(value: Option<PragmaValue>, db: &mut Database) -> Result<CommandOutput> {
    match value {
        None => {
            // Read form: render as a single-row, single-column result
            // set so `Connection::execute` (and the REPL) produce
            // SQLite-shaped output. SDK callers driving a typed-row API
            // would normally use `Connection::prepare` for this — but
            // PRAGMA reads aren't on the prepared-statement path yet,
            // so for now consumers parse the rendered table or call
            // `Connection::auto_vacuum_threshold` directly.
            let mut t = PrintTable::new();
            t.add_row(PrintRow::new(vec![PrintCell::new("auto_vacuum")]));
            let cell_value = match db.auto_vacuum_threshold() {
                Some(v) => format!("{v}"),
                None => "OFF".to_string(),
            };
            t.add_row(PrintRow::new(vec![PrintCell::new(&cell_value)]));
            Ok(CommandOutput {
                status: "PRAGMA auto_vacuum executed. 1 row returned.".to_string(),
                rendered: Some(t.to_string()),
            })
        }
        Some(v) => {
            let new_threshold = parse_auto_vacuum_target(&v)?;
            db.set_auto_vacuum_threshold(new_threshold)?;
            Ok(CommandOutput {
                status: "PRAGMA auto_vacuum executed.".to_string(),
                rendered: None,
            })
        }
    }
}

/// Maps a PRAGMA value to the threshold argument expected by
/// `Database::set_auto_vacuum_threshold`. `OFF` and `NONE` (bare or
/// quoted, case-insensitive) disable the trigger; numeric values pass
/// through to the setter for range validation.
fn parse_auto_vacuum_target(value: &PragmaValue) -> Result<Option<f32>> {
    match value {
        PragmaValue::Identifier(s) | PragmaValue::String(s) => {
            match s.to_ascii_lowercase().as_str() {
                "off" | "none" => Ok(None),
                _ => Err(SQLRiteError::General(format!(
                    "PRAGMA auto_vacuum: expected a number in 0.0..=1.0 or OFF/NONE, got '{s}'"
                ))),
            }
        }
        PragmaValue::Number(s) => {
            let f: f32 = s.parse().map_err(|_| {
                SQLRiteError::General(format!("PRAGMA auto_vacuum: '{s}' is not a valid number"))
            })?;
            Ok(Some(f))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_parse_pragma_returns_none_for_non_pragma() {
        assert!(try_parse_pragma("SELECT 1;").unwrap().is_none());
        assert!(
            try_parse_pragma("CREATE TABLE t (id INTEGER);")
                .unwrap()
                .is_none()
        );
        // Empty / whitespace / comment-only inputs aren't pragmas.
        assert!(try_parse_pragma("").unwrap().is_none());
        assert!(try_parse_pragma("   \n\t  ").unwrap().is_none());
        assert!(try_parse_pragma("-- hello\n").unwrap().is_none());
    }

    #[test]
    fn try_parse_pragma_read_form() {
        let stmt = try_parse_pragma("PRAGMA auto_vacuum;").unwrap().unwrap();
        assert_eq!(stmt.name, "auto_vacuum");
        assert_eq!(stmt.value, None);

        // Trailing whitespace / no semicolon.
        let stmt = try_parse_pragma("  PRAGMA auto_vacuum  ").unwrap().unwrap();
        assert_eq!(stmt.name, "auto_vacuum");
        assert_eq!(stmt.value, None);

        // Case-insensitive PRAGMA keyword.
        let stmt = try_parse_pragma("pragma auto_vacuum;").unwrap().unwrap();
        assert_eq!(stmt.name, "auto_vacuum");
    }

    #[test]
    fn try_parse_pragma_eq_number() {
        let stmt = try_parse_pragma("PRAGMA auto_vacuum = 0.5;")
            .unwrap()
            .unwrap();
        assert_eq!(stmt.name, "auto_vacuum");
        assert_eq!(stmt.value, Some(PragmaValue::Number("0.5".to_string())));

        let stmt = try_parse_pragma("PRAGMA auto_vacuum = 0;")
            .unwrap()
            .unwrap();
        assert_eq!(stmt.value, Some(PragmaValue::Number("0".to_string())));

        // Negative — surfaces from the setter as a range error, but
        // tokenization should round-trip the sign.
        let stmt = try_parse_pragma("PRAGMA auto_vacuum = -0.1;")
            .unwrap()
            .unwrap();
        assert_eq!(stmt.value, Some(PragmaValue::Number("-0.1".to_string())));
    }

    #[test]
    fn try_parse_pragma_eq_identifier() {
        let stmt = try_parse_pragma("PRAGMA auto_vacuum = OFF;")
            .unwrap()
            .unwrap();
        assert_eq!(stmt.value, Some(PragmaValue::Identifier("OFF".to_string())));

        let stmt = try_parse_pragma("PRAGMA auto_vacuum = none;")
            .unwrap()
            .unwrap();
        assert_eq!(
            stmt.value,
            Some(PragmaValue::Identifier("none".to_string()))
        );
    }

    #[test]
    fn try_parse_pragma_eq_string() {
        // Single-quoted strings are unambiguous string literals.
        let stmt = try_parse_pragma("PRAGMA auto_vacuum = 'OFF';")
            .unwrap()
            .unwrap();
        assert_eq!(stmt.value, Some(PragmaValue::String("OFF".to_string())));

        // SQLite's tokenizer treats `"NONE"` as a delimited identifier
        // (not a string literal) — it surfaces here as `Identifier`.
        // Both shapes funnel through `parse_auto_vacuum_target`'s
        // case-insensitive OFF/NONE arm, so the user-visible behavior
        // is identical.
        let stmt = try_parse_pragma("PRAGMA auto_vacuum = \"NONE\";")
            .unwrap()
            .unwrap();
        assert_eq!(
            stmt.value,
            Some(PragmaValue::Identifier("NONE".to_string()))
        );
    }

    #[test]
    fn try_parse_pragma_paren_form() {
        let stmt = try_parse_pragma("PRAGMA auto_vacuum(0.5);")
            .unwrap()
            .unwrap();
        assert_eq!(stmt.value, Some(PragmaValue::Number("0.5".to_string())));

        let stmt = try_parse_pragma("PRAGMA auto_vacuum (OFF);")
            .unwrap()
            .unwrap();
        assert_eq!(stmt.value, Some(PragmaValue::Identifier("OFF".to_string())));
    }

    #[test]
    fn try_parse_pragma_rejects_malformed() {
        assert!(try_parse_pragma("PRAGMA;").is_err());
        assert!(try_parse_pragma("PRAGMA = 0.5;").is_err());
        assert!(try_parse_pragma("PRAGMA auto_vacuum =;").is_err());
        assert!(try_parse_pragma("PRAGMA auto_vacuum (0.5;").is_err());
        // Multi-statement is rejected here just like the regular path.
        assert!(try_parse_pragma("PRAGMA auto_vacuum; SELECT 1;").is_err());
        // `--` is a binary minus on a string token, which we reject.
        assert!(try_parse_pragma("PRAGMA auto_vacuum = -'OFF';").is_err());
    }

    #[test]
    fn parse_auto_vacuum_target_disables_on_off_or_none() {
        for raw in ["OFF", "off", "Off", "NONE", "none"] {
            assert_eq!(
                parse_auto_vacuum_target(&PragmaValue::Identifier(raw.to_string())).unwrap(),
                None
            );
            assert_eq!(
                parse_auto_vacuum_target(&PragmaValue::String(raw.to_string())).unwrap(),
                None
            );
        }
    }

    #[test]
    fn parse_auto_vacuum_target_passes_numbers_through() {
        assert_eq!(
            parse_auto_vacuum_target(&PragmaValue::Number("0.5".to_string())).unwrap(),
            Some(0.5_f32)
        );
        assert_eq!(
            parse_auto_vacuum_target(&PragmaValue::Number("0".to_string())).unwrap(),
            Some(0.0_f32)
        );
        // Out-of-range numbers parse OK at this layer; the setter
        // validates the range.
        assert_eq!(
            parse_auto_vacuum_target(&PragmaValue::Number("1.5".to_string())).unwrap(),
            Some(1.5_f32)
        );
    }

    #[test]
    fn parse_auto_vacuum_target_rejects_unknown_strings() {
        let err =
            parse_auto_vacuum_target(&PragmaValue::Identifier("WAL".to_string())).unwrap_err();
        assert!(format!("{err}").contains("OFF/NONE"));
    }

    #[test]
    fn execute_pragma_unknown_returns_not_implemented() {
        // `journal_mode` was the canary unknown pragma here before
        // Phase 11.3 added it. Use a name that's still unsupported.
        let mut db = Database::new("t".to_string());
        let err = execute_pragma(
            PragmaStatement {
                name: "synchronous".to_string(),
                value: None,
            },
            &mut db,
        )
        .unwrap_err();
        assert!(matches!(err, SQLRiteError::NotImplemented(_)));
    }

    #[test]
    fn execute_pragma_auto_vacuum_set_and_read() {
        let mut db = Database::new("t".to_string());

        // Set to 0.5, read returns 0.5 in the rendered cell.
        let out = execute_pragma(
            PragmaStatement {
                name: "auto_vacuum".to_string(),
                value: Some(PragmaValue::Number("0.5".to_string())),
            },
            &mut db,
        )
        .unwrap();
        assert!(out.rendered.is_none());
        assert_eq!(db.auto_vacuum_threshold(), Some(0.5));

        let out = execute_pragma(
            PragmaStatement {
                name: "auto_vacuum".to_string(),
                value: None,
            },
            &mut db,
        )
        .unwrap();
        let rendered = out.rendered.expect("read form must render rows");
        assert!(rendered.contains("auto_vacuum"));
        assert!(rendered.contains("0.5"));

        // Disable via OFF (bare identifier).
        execute_pragma(
            PragmaStatement {
                name: "auto_vacuum".to_string(),
                value: Some(PragmaValue::Identifier("OFF".to_string())),
            },
            &mut db,
        )
        .unwrap();
        assert_eq!(db.auto_vacuum_threshold(), None);

        // Read after OFF — rendered cell shows OFF, not a number.
        let out = execute_pragma(
            PragmaStatement {
                name: "auto_vacuum".to_string(),
                value: None,
            },
            &mut db,
        )
        .unwrap();
        let rendered = out.rendered.unwrap();
        assert!(rendered.contains("OFF"));
    }

    #[test]
    fn execute_pragma_auto_vacuum_rejects_out_of_range() {
        let mut db = Database::new("t".to_string());
        let err = execute_pragma(
            PragmaStatement {
                name: "auto_vacuum".to_string(),
                value: Some(PragmaValue::Number("1.5".to_string())),
            },
            &mut db,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("auto_vacuum_threshold"));

        // Default survived the rejected set.
        assert_eq!(db.auto_vacuum_threshold(), Some(0.25));
    }

    #[test]
    fn execute_pragma_table_list_lists_tables_and_catalog() {
        use crate::sql::process_command;

        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT);",
            &mut db,
        )
        .unwrap();
        process_command("CREATE TABLE posts (id INTEGER PRIMARY KEY);", &mut db).unwrap();

        let out = execute_pragma(
            PragmaStatement {
                name: "table_list".to_string(),
                value: None,
            },
            &mut db,
        )
        .unwrap();
        let rendered = out.rendered.expect("table_list renders rows");
        assert!(rendered.contains("users"), "lists user table 'users'");
        assert!(rendered.contains("posts"), "lists user table 'posts'");
        assert!(
            rendered.contains("sqlrite_master"),
            "lists the catalog table"
        );
        // Header columns present.
        assert!(rendered.contains("ncol"));
        // 2 user tables + sqlrite_master.
        assert!(out.status.contains("3 rows"), "status: {}", out.status);
    }

    #[test]
    fn execute_pragma_table_list_rejects_value() {
        let mut db = Database::new("t".to_string());
        let err = execute_pragma(
            PragmaStatement {
                name: "table_list".to_string(),
                value: Some(PragmaValue::Identifier("x".to_string())),
            },
            &mut db,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("does not take a value"));
    }

    #[test]
    fn execute_pragma_auto_vacuum_rejects_negative() {
        let mut db = Database::new("t".to_string());
        let err = execute_pragma(
            PragmaStatement {
                name: "auto_vacuum".to_string(),
                value: Some(PragmaValue::Number("-0.1".to_string())),
            },
            &mut db,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("auto_vacuum_threshold"));
    }
}
