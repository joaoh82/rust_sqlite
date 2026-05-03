use std::fmt;
use std::path::{Path, PathBuf};

use rustyline::Editor;
use rustyline::history::DefaultHistory;

use crate::repl::REPLHelper;
use sqlrite::error::{Result, SQLRiteError};
use sqlrite::sql::db::database::Database;
use sqlrite::sql::pager::{open_database, save_database};
use sqlrite::{ask::ask_with_database, process_command_with_render};
use sqlrite_ask::AskConfig;

#[derive(Debug, PartialEq)]
pub enum MetaCommand {
    Exit,
    Help,
    /// `.open FILENAME` — create or load a persistent database.
    Open(PathBuf),
    /// `.save FILENAME` — write the current database to disk.
    Save(PathBuf),
    /// `.tables` — list the tables in the current database.
    Tables,
    /// `.ask <question>` — natural-language → SQL via the
    /// configured LLM. The rest of the line after `.ask ` becomes
    /// the question text (verbatim — including punctuation, quotes,
    /// etc.). See [`handle_ask`] for the confirm-and-run UX.
    Ask(String),
    /// Parsed line that didn't match any known meta-command.
    Unknown,
}

/// Trait responsible for translating type into a formated text.
impl fmt::Display for MetaCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MetaCommand::Exit => f.write_str(".exit"),
            MetaCommand::Help => f.write_str(".help"),
            MetaCommand::Open(_) => f.write_str(".open"),
            MetaCommand::Save(_) => f.write_str(".save"),
            MetaCommand::Tables => f.write_str(".tables"),
            MetaCommand::Ask(_) => f.write_str(".ask"),
            MetaCommand::Unknown => f.write_str("Unknown command"),
        }
    }
}

impl MetaCommand {
    pub fn new(command: String) -> MetaCommand {
        let trimmed = command.trim_end();
        // `.ask` is parsed by stripping the prefix and keeping the
        // rest of the line verbatim — every other meta-command splits
        // on whitespace, but a natural-language question can contain
        // arbitrary punctuation, multiple spaces, quoted phrases,
        // etc., and we don't want to molest any of it.
        if let Some(rest) = trimmed.strip_prefix(".ask") {
            // Require at least one whitespace between `.ask` and the
            // question — `.askfoo` is Unknown, `.ask foo` is the
            // question "foo".
            return match rest.chars().next() {
                Some(c) if c.is_whitespace() => {
                    let q = rest.trim().to_string();
                    if q.is_empty() {
                        MetaCommand::Unknown
                    } else {
                        MetaCommand::Ask(q)
                    }
                }
                None => MetaCommand::Unknown, // bare ".ask" with no question
                Some(_) => MetaCommand::Unknown, // ".askfoo"
            };
        }

        let args: Vec<&str> = trimmed.split_whitespace().collect();
        let Some(cmd) = args.first() else {
            return MetaCommand::Unknown;
        };
        match *cmd {
            ".exit" => MetaCommand::Exit,
            ".help" => MetaCommand::Help,
            ".open" => match args.get(1) {
                Some(path) => MetaCommand::Open(PathBuf::from(path)),
                None => MetaCommand::Unknown,
            },
            ".save" => match args.get(1) {
                Some(path) => MetaCommand::Save(PathBuf::from(path)),
                None => MetaCommand::Unknown,
            },
            ".tables" => MetaCommand::Tables,
            _ => MetaCommand::Unknown,
        }
    }
}

/// Executes a parsed meta-command. May mutate `db` — `.open` replaces it
/// with the loaded file's database; `.save` just reads it.
pub fn handle_meta_command(
    command: MetaCommand,
    repl: &mut Editor<REPLHelper, DefaultHistory>,
    db: &mut Database,
) -> Result<String> {
    match command {
        MetaCommand::Exit => {
            repl.append_history("history").unwrap();
            std::process::exit(0)
        }
        MetaCommand::Help => Ok(format!(
            "{}{}{}{}{}{}{}{}",
            "Special commands:\n",
            ".help            - Display this message\n",
            ".open <FILENAME> - Open a SQLRite database file (creates it if missing)\n",
            ".save <FILENAME> - Write the current in-memory database to FILENAME\n",
            ".tables          - List tables in the current database\n",
            ".ask <QUESTION>  - Generate SQL from a natural-language question (LLM)\n",
            ".exit            - Quit this application\n",
            "\nOther meta commands (.read, .ast) are not implemented yet.\n\
             For .ask, set SQLRITE_LLM_API_KEY in your environment first."
        )),
        MetaCommand::Open(path) => handle_open(&path, db),
        MetaCommand::Save(path) => handle_save(&path, db),
        MetaCommand::Tables => handle_tables(db),
        MetaCommand::Ask(question) => handle_ask(&question, repl, db),
        MetaCommand::Unknown => Err(SQLRiteError::UnknownCommand(
            "Unknown command or invalid arguments. Enter '.help'".to_string(),
        )),
    }
}

fn handle_open(path: &Path, db: &mut Database) -> Result<String> {
    let db_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("db")
        .to_string();
    if path.exists() {
        let loaded = open_database(path, db_name)?;
        let table_count = loaded.tables.len();
        *db = loaded;
        Ok(format!(
            "Opened '{}' ({table_count} table{s} loaded). Auto-save enabled.",
            path.display(),
            s = if table_count == 1 { "" } else { "s" }
        ))
    } else {
        // Same behavior as SQLite: `.open` on a missing file creates a fresh
        // DB that will be materialized on the next committing statement.
        let mut fresh = Database::new(db_name);
        fresh.source_path = Some(path.to_path_buf());
        // Touch the file with a valid empty DB so the path now exists and a
        // subsequent `.open` finds it. This also catches permission errors early
        // and attaches the long-lived pager to the fresh database.
        save_database(&mut fresh, path)?;
        *db = fresh;
        Ok(format!(
            "Opened '{}' (new database). Auto-save enabled.",
            path.display()
        ))
    }
}

fn handle_save(path: &Path, db: &mut Database) -> Result<String> {
    save_database(db, path)?;
    if db.source_path.as_deref() == Some(path) {
        Ok(format!(
            "Flushed database to '{}' (auto-save is already on).",
            path.display()
        ))
    } else {
        Ok(format!("Saved database to '{}'.", path.display()))
    }
}

fn handle_tables(db: &Database) -> Result<String> {
    if db.tables.is_empty() {
        return Ok("(no tables)".to_string());
    }
    // Sort for deterministic output — HashMap iteration order is arbitrary.
    let mut names: Vec<&String> = db.tables.keys().collect();
    names.sort();
    Ok(names
        .into_iter()
        .map(|s| s.as_str())
        .collect::<Vec<&str>>()
        .join("\n"))
}

/// Handle `.ask <question>` — confirm-and-run UX:
///
/// 1. Build an `AskConfig` from the environment (`SQLRITE_LLM_*` vars).
/// 2. Call into [`sqlrite::ask::ask_with_database`] — generates SQL.
/// 3. Print the generated SQL + the model's one-sentence rationale.
/// 4. Prompt `Run? [Y/n] ` via rustyline. Empty / `y` / `yes` → run,
///    `n` / `no` → skip. Anything else also skips (paranoid default).
/// 5. If confirmed, run the SQL through `process_command` (the same
///    pipeline as a typed-out `SELECT` / `INSERT` / etc.) and return
///    its result string. If skipped, return a short "skipped" note.
///
/// Returns the rendered output string for the outer dispatch loop to
/// print. The rendered output already includes the SQL preview, the
/// rationale, and either the query result table or the skip message.
fn handle_ask(
    question: &str,
    repl: &mut Editor<REPLHelper, DefaultHistory>,
    db: &mut Database,
) -> Result<String> {
    // Read env-var config. Surfaces a friendly error if e.g.
    // SQLRITE_LLM_CACHE_TTL holds an unrecognized value. A missing
    // SQLRITE_LLM_API_KEY is *not* surfaced here — `from_env` returns
    // Ok(_) with `api_key: None`, and `ask_with_database` then fails
    // with `AskError::MissingApiKey` so the user gets a clear
    // "missing API key (set SQLRITE_LLM_API_KEY)" message instead
    // of "config error".
    let cfg: AskConfig =
        AskConfig::from_env().map_err(|e| SQLRiteError::General(format!("ask: {e}")))?;

    let resp = ask_with_database(db, question, &cfg)
        .map_err(|e| SQLRiteError::General(format!("ask: {e}")))?;

    if resp.sql.trim().is_empty() {
        // Model decided the schema can't answer this question — surface
        // its explanation rather than silently producing nothing.
        return Ok(format!(
            "The model declined to generate SQL for that question.\n\
             Reason: {}",
            if resp.explanation.is_empty() {
                "(no explanation provided)"
            } else {
                resp.explanation.as_str()
            }
        ));
    }

    println!("Generated SQL:");
    println!("  {}", resp.sql);
    if !resp.explanation.is_empty() {
        println!("Rationale: {}", resp.explanation);
    }

    // Confirm-and-run prompt. We use the same rustyline editor so
    // history works across the prompt; `readline` blocks until the
    // user submits a line. Ctrl-C / EOF map to the same "skip" path
    // as a `n` answer — refusing on interrupt is the safer default
    // when running LLM-generated SQL.
    let answer = match repl.readline("Run? [Y/n] ") {
        Ok(s) => s.trim().to_lowercase(),
        Err(_) => return Ok("Skipped (input interrupted).".to_string()),
    };
    let confirmed = matches!(answer.as_str(), "" | "y" | "yes");
    if !confirmed {
        return Ok("Skipped.".to_string());
    }

    // Run the generated SQL through the same pipeline as a typed
    // statement. We use the `_with_render` variant so SELECTs come back
    // with their rendered prettytable; concatenate it above the status
    // line so the REPL's outer dispatch (which just prints whatever
    // string we return) shows both. DDL/DML statements return only a
    // status — `rendered` is `None` and we skip the prepend.
    let output = process_command_with_render(&resp.sql, db)?;
    Ok(match output.rendered {
        Some(rendered) => format!("{rendered}{}", output.status),
        None => output.status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::{REPLHelper, get_config};
    use sqlrite::process_command;

    fn new_editor() -> Editor<REPLHelper, DefaultHistory> {
        let config = get_config();
        let helper = REPLHelper::default();
        let mut repl: Editor<REPLHelper, DefaultHistory> =
            Editor::with_config(config).expect("failed to build rustyline editor");
        repl.set_helper(Some(helper));
        repl
    }

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-meta-{pid}-{nanos}-{name}.sqlrite"));
        p
    }

    /// Phase 4c: every .sqlrite has a `-wal` sidecar now. Delete both so
    /// `/tmp` doesn't accumulate orphan WALs across test runs.
    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(PathBuf::from(wal));
    }

    #[test]
    fn help_works() {
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());
        let result = handle_meta_command(MetaCommand::Help, &mut repl, &mut db);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_open_requires_argument() {
        assert_eq!(MetaCommand::new(".open".to_string()), MetaCommand::Unknown);
        assert_eq!(
            MetaCommand::new(".open my.sqlrite".to_string()),
            MetaCommand::Open(PathBuf::from("my.sqlrite"))
        );
    }

    #[test]
    fn parse_save_requires_argument() {
        assert_eq!(MetaCommand::new(".save".to_string()), MetaCommand::Unknown);
        assert_eq!(
            MetaCommand::new(".save my.sqlrite".to_string()),
            MetaCommand::Save(PathBuf::from("my.sqlrite"))
        );
    }

    #[test]
    fn parse_ask_captures_question_verbatim() {
        // Bare `.ask` is invalid — must have a question.
        assert_eq!(MetaCommand::new(".ask".to_string()), MetaCommand::Unknown);
        // `.ask` with empty trailing whitespace is also invalid.
        assert_eq!(
            MetaCommand::new(".ask   ".to_string()),
            MetaCommand::Unknown
        );
        // Valid question — captured verbatim, including punctuation.
        assert_eq!(
            MetaCommand::new(".ask How many users are over 30?".to_string()),
            MetaCommand::Ask("How many users are over 30?".to_string())
        );
        // Multiple internal spaces are preserved (after the leading
        // ".ask " strip + trim).
        assert_eq!(
            MetaCommand::new(".ask  show me   users".to_string()),
            MetaCommand::Ask("show me   users".to_string())
        );
        // Tab separator works.
        assert_eq!(
            MetaCommand::new(".ask\tcount rows".to_string()),
            MetaCommand::Ask("count rows".to_string())
        );
    }

    #[test]
    fn parse_ask_rejects_no_separator() {
        // `.askfoo` should NOT match `.ask` — it's a typo, not a
        // question. Without this guard, every `.askXXX` line would be
        // treated as the question "XXX" with no separator.
        assert_eq!(
            MetaCommand::new(".askfoo".to_string()),
            MetaCommand::Unknown
        );
        assert_eq!(
            MetaCommand::new(".asking".to_string()),
            MetaCommand::Unknown
        );
    }

    #[test]
    fn ask_meta_command_displays_as_dotask() {
        let cmd = MetaCommand::Ask("anything".to_string());
        assert_eq!(format!("{cmd}"), ".ask");
    }

    #[test]
    fn tables_meta_command() {
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());
        // Empty case.
        let msg = handle_meta_command(MetaCommand::Tables, &mut repl, &mut db).unwrap();
        assert_eq!(msg, "(no tables)");

        // Populated case — two tables, output should be sorted.
        process_command("CREATE TABLE zebras (id INTEGER PRIMARY KEY);", &mut db).unwrap();
        process_command("CREATE TABLE apples (id INTEGER PRIMARY KEY);", &mut db).unwrap();
        let msg = handle_meta_command(MetaCommand::Tables, &mut repl, &mut db).unwrap();
        assert_eq!(msg, "apples\nzebras");
    }

    #[test]
    fn save_then_open_round_trips_through_meta_commands() {
        use sqlrite::sql::db::table::Value;

        let path = tmp_path("meta_roundtrip");
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());

        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO users (name) VALUES ('alice');", &mut db).unwrap();

        handle_meta_command(MetaCommand::Save(path.clone()), &mut repl, &mut db).expect("save");

        // Replace db with a fresh one, then .open the file.
        db = Database::new("fresh".to_string());
        let msg =
            handle_meta_command(MetaCommand::Open(path.clone()), &mut repl, &mut db).expect("open");
        assert!(msg.contains("1 table loaded"));

        let users = db.get_table("users".to_string()).unwrap();
        let rowids = users.rowids();
        assert_eq!(rowids.len(), 1);
        assert_eq!(
            users.get_value("name", rowids[0]),
            Some(Value::Text("alice".to_string()))
        );

        cleanup(&path);
    }

    #[test]
    fn open_missing_file_creates_fresh_db_and_materializes_file() {
        let path = tmp_path("missing");
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());

        let msg =
            handle_meta_command(MetaCommand::Open(path.clone()), &mut repl, &mut db).expect("open");
        assert!(msg.contains("new database"));
        assert_eq!(db.tables.len(), 0);
        // Auto-save expects a file to exist to auto-flush into, so open-of-missing
        // touches the file with a valid empty DB.
        assert!(path.exists());
        assert_eq!(db.source_path.as_deref(), Some(path.as_path()));

        cleanup(&path);
    }

    #[test]
    fn auto_save_persists_writes_without_explicit_save() {
        use sqlrite::sql::db::table::Value;

        let path = tmp_path("autosave");
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());

        handle_meta_command(MetaCommand::Open(path.clone()), &mut repl, &mut db).expect("open");

        // The first write should auto-flush to disk.
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO users (name) VALUES ('alice');", &mut db).unwrap();

        // Drop the first Database so its exclusive lock releases before we
        // reopen the same file for verification.
        drop(db);

        // Reopen the file from scratch in a fresh Database — no manual .save was called.
        let fresh = sqlrite::sql::pager::open_database(&path, "x".to_string())
            .expect("open after auto-save");
        let users = fresh.get_table("users".to_string()).expect("users table");
        let rowids = users.rowids();
        assert_eq!(rowids.len(), 1);
        assert_eq!(
            users.get_value("name", rowids[0]),
            Some(Value::Text("alice".to_string()))
        );

        cleanup(&path);
    }
}
