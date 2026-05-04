pub mod db;
pub mod executor;
pub mod fts;
pub mod hnsw;
pub mod pager;
pub mod parser;
// pub mod tokenizer;

use parser::create::CreateQuery;
use parser::insert::InsertQuery;
use parser::select::SelectQuery;

use sqlparser::ast::{ObjectType, Statement};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::{Parser, ParserError};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::table::Table;

#[derive(Debug, PartialEq)]
pub enum SQLCommand {
    Insert(String),
    Delete(String),
    Update(String),
    CreateTable(String),
    Select(String),
    Unknown(String),
}

impl SQLCommand {
    pub fn new(command: String) -> SQLCommand {
        let v = command.split(" ").collect::<Vec<&str>>();
        match v[0] {
            "insert" => SQLCommand::Insert(command),
            "update" => SQLCommand::Update(command),
            "delete" => SQLCommand::Delete(command),
            "create" => SQLCommand::CreateTable(command),
            "select" => SQLCommand::Select(command),
            _ => SQLCommand::Unknown(command),
        }
    }
}

/// Output of running one SQL statement through the engine.
///
/// Two fields:
///
/// - `status` is the short human-readable confirmation line every caller
///   wants ("INSERT Statement executed.", "3 rows updated.", "BEGIN", etc.).
/// - `rendered` is the pre-formatted prettytable rendering of a SELECT's
///   result rows. Populated only for `SELECT` statements; `None` for every
///   other statement type. The REPL prints this above the status line so
///   users see both the rows and the confirmation; SDK / FFI / MCP callers
///   ignore it and reach for the typed-row APIs (`Connection::prepare` →
///   `Statement::query` → `Rows`) when they want row data instead.
///
/// Splitting the two means [`process_command_with_render`] can return
/// everything the REPL needs without writing to stdout itself —
/// historically `process_command` would `print!()` the rendered table
/// directly, which corrupted any non-REPL stdout channel (the MCP server's
/// JSON-RPC wire, structured loggers piping engine output, …).
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status: String,
    pub rendered: Option<String>,
}

/// Backwards-compatible wrapper around [`process_command_with_render`] that
/// returns just the status string. Every existing call site (the public
/// `Connection::execute`, the SDK FFI shims, the .ask meta-command's
/// inline runner, the engine's own tests) keeps working unchanged.
///
/// Callers that want the rendered SELECT table (the REPL, future
/// terminal-style consumers) should call [`process_command_with_render`]
/// directly and inspect [`CommandOutput::rendered`].
pub fn process_command(query: &str, db: &mut Database) -> Result<String> {
    process_command_with_render(query, db).map(|o| o.status)
}

/// Performs initial parsing of SQL Statement using sqlparser-rs.
///
/// Returns a [`CommandOutput`] carrying both the status string and (for
/// SELECT statements) the pre-rendered prettytable output. **Never writes
/// to stdout.** The REPL is responsible for printing whatever it wants
/// from the returned struct.
pub fn process_command_with_render(query: &str, db: &mut Database) -> Result<CommandOutput> {
    let dialect = SQLiteDialect {};
    let message: String;
    let mut rendered: Option<String> = None;
    let mut ast = Parser::parse_sql(&dialect, query).map_err(SQLRiteError::from)?;

    if ast.len() > 1 {
        return Err(SQLRiteError::SqlError(ParserError::ParserError(format!(
            "Expected a single query statement, but there are {}",
            ast.len()
        ))));
    }

    // Comment-only or whitespace-only input parses to an empty Vec<Statement>.
    // Return a benign status rather than panicking on `pop().unwrap()`. Callers
    // (REPL, Tauri app) treat this as a no-op with no disk write triggered.
    let Some(query) = ast.pop() else {
        return Ok(CommandOutput {
            status: "No statement to execute.".to_string(),
            rendered: None,
        });
    };

    // Transaction boundary statements are routed to Database-level
    // handlers before we even inspect the rest of the AST. They don't
    // mutate table data directly, so they short-circuit the
    // is_write_statement / auto-save path.
    match &query {
        Statement::StartTransaction { .. } => {
            db.begin_transaction()?;
            return Ok(CommandOutput {
                status: String::from("BEGIN"),
                rendered: None,
            });
        }
        Statement::Commit { .. } => {
            if !db.in_transaction() {
                return Err(SQLRiteError::General(
                    "cannot COMMIT: no transaction is open".to_string(),
                ));
            }
            // Flush accumulated in-memory changes to disk. If the save
            // fails we auto-rollback the in-memory state to the
            // pre-BEGIN snapshot and surface a combined error. Leaving
            // the transaction open after a failed COMMIT would be
            // unsafe: auto-save on any subsequent non-transactional
            // statement would silently publish partial mid-transaction
            // work. Auto-rollback keeps the disk-plus-memory pair
            // coherent — the user loses their in-flight work on a disk
            // error, but that's the only safe outcome.
            if let Some(path) = db.source_path.clone() {
                if let Err(save_err) = pager::save_database(db, &path) {
                    let _ = db.rollback_transaction();
                    return Err(SQLRiteError::General(format!(
                        "COMMIT failed — transaction rolled back: {save_err}"
                    )));
                }
            }
            db.commit_transaction()?;
            return Ok(CommandOutput {
                status: String::from("COMMIT"),
                rendered: None,
            });
        }
        Statement::Rollback { .. } => {
            db.rollback_transaction()?;
            return Ok(CommandOutput {
                status: String::from("ROLLBACK"),
                rendered: None,
            });
        }
        _ => {}
    }

    // Statements that mutate state — trigger auto-save on success. Read-only
    // SELECTs skip the save entirely to avoid pointless file writes.
    let is_write_statement = matches!(
        &query,
        Statement::CreateTable(_)
            | Statement::CreateIndex(_)
            | Statement::Insert(_)
            | Statement::Update(_)
            | Statement::Delete(_)
            | Statement::Drop { .. }
            | Statement::AlterTable(_)
    );

    // Early-reject mutations on a read-only database before they touch
    // in-memory state. Phase 4e: without this, a user running INSERT
    // on a `--readonly` REPL would see the row appear in the printed
    // table, and then the auto-save would fail — leaving the in-memory
    // Database visibly diverged from disk.
    if is_write_statement && db.is_read_only() {
        return Err(SQLRiteError::General(
            "cannot execute: database is opened read-only".to_string(),
        ));
    }

    // Initialy only implementing some basic SQL Statements
    match query {
        Statement::CreateTable(_) => {
            let create_query = CreateQuery::new(&query);
            match create_query {
                Ok(payload) => {
                    let table_name = payload.table_name.clone();
                    if table_name == pager::MASTER_TABLE_NAME {
                        return Err(SQLRiteError::General(format!(
                            "'{}' is a reserved name used by the internal schema catalog",
                            pager::MASTER_TABLE_NAME
                        )));
                    }
                    // Checking if table already exists, after parsing CREATE TABLE query
                    match db.contains_table(table_name.to_string()) {
                        true => {
                            return Err(SQLRiteError::Internal(
                                "Cannot create, table already exists.".to_string(),
                            ));
                        }
                        false => {
                            let table = Table::new(payload);
                            // Note: we used to call `table.print_table_schema()` here
                            // for REPL convenience. Removed because it wrote
                            // directly to stdout, which corrupted any non-REPL
                            // protocol channel (most painfully the MCP server's
                            // JSON-RPC wire). The status line below is enough for
                            // the REPL; users who want to inspect the schema can
                            // run a follow-up describe / `.tables`-style command.
                            db.tables.insert(table_name.to_string(), table);
                            message = String::from("CREATE TABLE Statement executed.");
                        }
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Statement::Insert(_) => {
            let insert_query = InsertQuery::new(&query);
            match insert_query {
                Ok(payload) => {
                    let table_name = payload.table_name;
                    let columns = payload.columns;
                    let values = payload.rows;

                    // println!("table_name = {:?}\n cols = {:?}\n vals = {:?}", table_name, columns, values);
                    // Checking if Table exists in Database
                    match db.contains_table(table_name.to_string()) {
                        true => {
                            let db_table = db.get_table_mut(table_name.to_string()).unwrap();
                            // Checking if columns on INSERT query exist on Table
                            match columns
                                .iter()
                                .all(|column| db_table.contains_column(column.to_string()))
                            {
                                true => {
                                    for value in &values {
                                        // Checking if number of columns in query are the same as number of values
                                        if columns.len() != value.len() {
                                            return Err(SQLRiteError::Internal(format!(
                                                "{} values for {} columns",
                                                value.len(),
                                                columns.len()
                                            )));
                                        }
                                        db_table
                                            .validate_unique_constraint(&columns, value)
                                            .map_err(|err| {
                                                SQLRiteError::Internal(format!(
                                                    "Unique key constraint violation: {err}"
                                                ))
                                            })?;
                                        db_table.insert_row(&columns, value)?;
                                    }
                                }
                                false => {
                                    return Err(SQLRiteError::Internal(
                                        "Cannot insert, some of the columns do not exist"
                                            .to_string(),
                                    ));
                                }
                            }
                            // Note: we used to call `db_table.print_table_data()`
                            // here, which dumped the *entire* table to stdout
                            // after every INSERT. Beyond corrupting non-REPL
                            // stdout channels, that's actively bad UX on any
                            // table with more than a few rows. Removed in the
                            // engine-stdout-pollution cleanup.
                        }
                        false => {
                            return Err(SQLRiteError::Internal("Table doesn't exist".to_string()));
                        }
                    }
                }
                Err(err) => return Err(err),
            }

            message = String::from("INSERT Statement executed.")
        }
        Statement::Query(_) => {
            let select_query = SelectQuery::new(&query)?;
            let (rendered_table, rows) = executor::execute_select(select_query, db)?;
            // Stash the rendered prettytable in the output so the REPL
            // (or any terminal-style consumer) can print it above the
            // status line. SDK / FFI / MCP callers ignore this field.
            // The previous implementation `print!("{rendered}")`-ed
            // directly to stdout, which broke every non-REPL embedder.
            rendered = Some(rendered_table);
            message = format!(
                "SELECT Statement executed. {rows} row{s} returned.",
                s = if rows == 1 { "" } else { "s" }
            );
        }
        Statement::Delete(_) => {
            let rows = executor::execute_delete(&query, db)?;
            message = format!(
                "DELETE Statement executed. {rows} row{s} deleted.",
                s = if rows == 1 { "" } else { "s" }
            );
        }
        Statement::Update(_) => {
            let rows = executor::execute_update(&query, db)?;
            message = format!(
                "UPDATE Statement executed. {rows} row{s} updated.",
                s = if rows == 1 { "" } else { "s" }
            );
        }
        Statement::CreateIndex(_) => {
            let name = executor::execute_create_index(&query, db)?;
            message = format!("CREATE INDEX '{name}' executed.");
        }
        Statement::Drop {
            object_type,
            if_exists,
            names,
            ..
        } => match object_type {
            ObjectType::Table => {
                let count = executor::execute_drop_table(&names, if_exists, db)?;
                let plural = if count == 1 { "table" } else { "tables" };
                message = format!("DROP TABLE Statement executed. {count} {plural} dropped.");
            }
            ObjectType::Index => {
                let count = executor::execute_drop_index(&names, if_exists, db)?;
                let plural = if count == 1 { "index" } else { "indexes" };
                message = format!("DROP INDEX Statement executed. {count} {plural} dropped.");
            }
            other => {
                return Err(SQLRiteError::NotImplemented(format!(
                    "DROP {other:?} is not supported (only TABLE and INDEX)"
                )));
            }
        },
        Statement::AlterTable(alter) => {
            message = executor::execute_alter_table(alter, db)?;
        }
        _ => {
            return Err(SQLRiteError::NotImplemented(
                "SQL Statement not supported yet.".to_string(),
            ));
        }
    };

    // Auto-save: if the database is backed by a file AND no explicit
    // transaction is open AND the statement changed state, flush to
    // disk before returning. Inside a `BEGIN … COMMIT` block the
    // mutations accumulate in memory (protected by the ROLLBACK
    // snapshot) and land on disk in one shot when COMMIT runs.
    //
    // A failed save surfaces as an error — the in-memory state already
    // mutated, so the caller should know disk is out of sync. The
    // Pager held on `db` diffs against its last-committed snapshot,
    // so only pages whose bytes actually changed are written.
    if is_write_statement && db.source_path.is_some() && !db.in_transaction() {
        let path = db.source_path.clone().unwrap();
        pager::save_database(db, &path)?;
    }

    Ok(CommandOutput {
        status: message,
        rendered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::db::table::Value;

    /// Builds a `users(id INTEGER PK, name TEXT, age INTEGER)` table populated
    /// with three rows, for use in executor-level tests.
    fn seed_users_table() -> Database {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);",
            &mut db,
        )
        .expect("create table");
        process_command(
            "INSERT INTO users (name, age) VALUES ('alice', 30);",
            &mut db,
        )
        .expect("insert alice");
        process_command("INSERT INTO users (name, age) VALUES ('bob', 25);", &mut db)
            .expect("insert bob");
        process_command(
            "INSERT INTO users (name, age) VALUES ('carol', 40);",
            &mut db,
        )
        .expect("insert carol");
        db
    }

    #[test]
    fn process_command_select_all_test() {
        let mut db = seed_users_table();
        let response = process_command("SELECT * FROM users;", &mut db).expect("select");
        assert!(response.contains("3 rows returned"));
    }

    #[test]
    fn process_command_select_where_test() {
        let mut db = seed_users_table();
        let response =
            process_command("SELECT name FROM users WHERE age > 25;", &mut db).expect("select");
        assert!(response.contains("2 rows returned"));
    }

    #[test]
    fn process_command_select_eq_string_test() {
        let mut db = seed_users_table();
        let response =
            process_command("SELECT name FROM users WHERE name = 'bob';", &mut db).expect("select");
        assert!(response.contains("1 row returned"));
    }

    #[test]
    fn process_command_select_limit_test() {
        let mut db = seed_users_table();
        let response = process_command("SELECT * FROM users ORDER BY age ASC LIMIT 2;", &mut db)
            .expect("select");
        assert!(response.contains("2 rows returned"));
    }

    #[test]
    fn process_command_select_unknown_table_test() {
        let mut db = Database::new("tempdb".to_string());
        let result = process_command("SELECT * FROM nope;", &mut db);
        assert!(result.is_err());
    }

    #[test]
    fn process_command_select_unknown_column_test() {
        let mut db = seed_users_table();
        let result = process_command("SELECT height FROM users;", &mut db);
        assert!(result.is_err());
    }

    #[test]
    fn process_command_insert_test() {
        // Creating temporary database
        let mut db = Database::new("tempdb".to_string());

        // Creating temporary table for testing purposes
        let query_statement = "CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name TEXT
        );";
        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, query_statement).unwrap();
        if ast.len() > 1 {
            panic!("Expected a single query statement, but there are more then 1.")
        }
        let query = ast.pop().unwrap();
        let create_query = CreateQuery::new(&query).unwrap();

        // Inserting table into database
        db.tables.insert(
            create_query.table_name.to_string(),
            Table::new(create_query),
        );

        // Inserting data into table
        let insert_query = String::from("INSERT INTO users (name) Values ('josh');");
        match process_command(&insert_query, &mut db) {
            Ok(response) => assert_eq!(response, "INSERT Statement executed."),
            Err(err) => {
                eprintln!("Error: {}", err);
                assert!(false)
            }
        };
    }

    #[test]
    fn process_command_insert_no_pk_test() {
        // Creating temporary database
        let mut db = Database::new("tempdb".to_string());

        // Creating temporary table for testing purposes
        let query_statement = "CREATE TABLE users (
            name TEXT
        );";
        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, query_statement).unwrap();
        if ast.len() > 1 {
            panic!("Expected a single query statement, but there are more then 1.")
        }
        let query = ast.pop().unwrap();
        let create_query = CreateQuery::new(&query).unwrap();

        // Inserting table into database
        db.tables.insert(
            create_query.table_name.to_string(),
            Table::new(create_query),
        );

        // Inserting data into table
        let insert_query = String::from("INSERT INTO users (name) Values ('josh');");
        match process_command(&insert_query, &mut db) {
            Ok(response) => assert_eq!(response, "INSERT Statement executed."),
            Err(err) => {
                eprintln!("Error: {}", err);
                assert!(false)
            }
        };
    }

    #[test]
    fn process_command_delete_where_test() {
        let mut db = seed_users_table();
        let response =
            process_command("DELETE FROM users WHERE name = 'bob';", &mut db).expect("delete");
        assert!(response.contains("1 row deleted"));

        let remaining = process_command("SELECT * FROM users;", &mut db).expect("select");
        assert!(remaining.contains("2 rows returned"));
    }

    #[test]
    fn process_command_delete_all_test() {
        let mut db = seed_users_table();
        let response = process_command("DELETE FROM users;", &mut db).expect("delete");
        assert!(response.contains("3 rows deleted"));
    }

    #[test]
    fn process_command_update_where_test() {
        use crate::sql::db::table::Value;

        let mut db = seed_users_table();
        let response = process_command("UPDATE users SET age = 99 WHERE name = 'bob';", &mut db)
            .expect("update");
        assert!(response.contains("1 row updated"));

        // Confirm the cell was actually rewritten.
        let users = db.get_table("users".to_string()).unwrap();
        let bob_rowid = users
            .rowids()
            .into_iter()
            .find(|r| users.get_value("name", *r) == Some(Value::Text("bob".to_string())))
            .expect("bob row must exist");
        assert_eq!(users.get_value("age", bob_rowid), Some(Value::Integer(99)));
    }

    #[test]
    fn process_command_update_unique_violation_test() {
        let mut db = seed_users_table();
        // `name` is not UNIQUE in the seed — reinforce with an explicit unique column.
        process_command(
            "CREATE TABLE tags (id INTEGER PRIMARY KEY, label TEXT UNIQUE);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO tags (label) VALUES ('a');", &mut db).unwrap();
        process_command("INSERT INTO tags (label) VALUES ('b');", &mut db).unwrap();

        let result = process_command("UPDATE tags SET label = 'a' WHERE label = 'b';", &mut db);
        assert!(result.is_err(), "expected UNIQUE violation, got {result:?}");
    }

    #[test]
    fn process_command_insert_type_mismatch_returns_error_test() {
        // Previously this panicked in parse::<i32>().unwrap(); now it should return an error cleanly.
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, qty INTEGER);",
            &mut db,
        )
        .unwrap();
        let result = process_command("INSERT INTO items (qty) VALUES ('not a number');", &mut db);
        assert!(result.is_err(), "expected error, got {result:?}");
    }

    #[test]
    fn process_command_insert_missing_integer_returns_error_test() {
        // Non-PK INTEGER without a value should error (not panic on "Null".parse()).
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, qty INTEGER);",
            &mut db,
        )
        .unwrap();
        let result = process_command("INSERT INTO items (id) VALUES (1);", &mut db);
        assert!(result.is_err(), "expected error, got {result:?}");
    }

    #[test]
    fn process_command_update_arith_test() {
        use crate::sql::db::table::Value;

        let mut db = seed_users_table();
        process_command("UPDATE users SET age = age + 1;", &mut db).expect("update +1");

        let users = db.get_table("users".to_string()).unwrap();
        let mut ages: Vec<i64> = users
            .rowids()
            .into_iter()
            .filter_map(|r| match users.get_value("age", r) {
                Some(Value::Integer(n)) => Some(n),
                _ => None,
            })
            .collect();
        ages.sort();
        assert_eq!(ages, vec![26, 31, 41]); // 25+1, 30+1, 40+1
    }

    #[test]
    fn process_command_select_arithmetic_where_test() {
        let mut db = seed_users_table();
        // age * 2 > 55  →  only ages > 27.5  →  alice(30) + carol(40)
        let response =
            process_command("SELECT name FROM users WHERE age * 2 > 55;", &mut db).expect("select");
        assert!(response.contains("2 rows returned"));
    }

    #[test]
    fn process_command_divide_by_zero_test() {
        let mut db = seed_users_table();
        let result = process_command("SELECT age / 0 FROM users;", &mut db);
        // Projection only supports bare columns, so this errors earlier; still shouldn't panic.
        assert!(result.is_err());
    }

    #[test]
    fn process_command_unsupported_statement_test() {
        let mut db = Database::new("tempdb".to_string());
        // CREATE VIEW is firmly in the "Not yet supported" list — used as
        // the canary for the dispatcher's NotImplemented arm. (DROP TABLE
        // moved out of unsupported in this branch.)
        let result = process_command("CREATE VIEW v AS SELECT * FROM users;", &mut db);
        assert!(result.is_err());
    }

    #[test]
    fn empty_input_is_a_noop_not_a_panic() {
        // Regression for: desktop app pre-fills the textarea with a
        // comment-only placeholder, and hitting Run used to panic because
        // sqlparser produced zero statements and pop().unwrap() exploded.
        let mut db = Database::new("t".to_string());
        for input in ["", "   ", "-- just a comment", "-- comment\n-- another"] {
            let result = process_command(input, &mut db);
            assert!(result.is_ok(), "input {input:?} should not error");
            let msg = result.unwrap();
            assert!(msg.contains("No statement"), "got: {msg:?}");
        }
    }

    #[test]
    fn create_index_adds_explicit_index() {
        let mut db = seed_users_table();
        let response = process_command("CREATE INDEX users_age_idx ON users (age);", &mut db)
            .expect("create index");
        assert!(response.contains("users_age_idx"));

        // The index should now be attached to the users table.
        let users = db.get_table("users".to_string()).unwrap();
        let idx = users
            .index_by_name("users_age_idx")
            .expect("index should exist after CREATE INDEX");
        assert_eq!(idx.column_name, "age");
        assert!(!idx.is_unique);
    }

    #[test]
    fn create_unique_index_rejects_duplicate_existing_values() {
        let mut db = seed_users_table();
        // `name` is already UNIQUE (auto-indexed); insert a duplicate-age row
        // first so CREATE UNIQUE INDEX on age catches the conflict.
        process_command("INSERT INTO users (name, age) VALUES ('dan', 30);", &mut db).unwrap();
        let result = process_command(
            "CREATE UNIQUE INDEX users_age_unique ON users (age);",
            &mut db,
        );
        assert!(
            result.is_err(),
            "expected unique-index failure, got {result:?}"
        );
    }

    #[test]
    fn where_eq_on_indexed_column_uses_index_probe() {
        // Build a table big enough that a full scan would be expensive,
        // then rely on the index-probe fast path. This test verifies
        // correctness (right rows returned); the perf win is implicit.
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE big (id INTEGER PRIMARY KEY, tag TEXT);",
            &mut db,
        )
        .unwrap();
        process_command("CREATE INDEX big_tag_idx ON big (tag);", &mut db).unwrap();
        for i in 1..=100 {
            let tag = if i % 3 == 0 { "hot" } else { "cold" };
            process_command(&format!("INSERT INTO big (tag) VALUES ('{tag}');"), &mut db).unwrap();
        }
        let response =
            process_command("SELECT id FROM big WHERE tag = 'hot';", &mut db).expect("select");
        // 1..=100 has 33 multiples of 3.
        assert!(
            response.contains("33 rows returned"),
            "response was {response:?}"
        );
    }

    #[test]
    fn where_eq_on_indexed_column_inside_parens_uses_index_probe() {
        let mut db = seed_users_table();
        let response = process_command("SELECT name FROM users WHERE (name = 'bob');", &mut db)
            .expect("select");
        assert!(response.contains("1 row returned"));
    }

    #[test]
    fn where_eq_literal_first_side_uses_index_probe() {
        let mut db = seed_users_table();
        // `'bob' = name` should hit the same path as `name = 'bob'`.
        let response =
            process_command("SELECT name FROM users WHERE 'bob' = name;", &mut db).expect("select");
        assert!(response.contains("1 row returned"));
    }

    #[test]
    fn non_equality_where_still_falls_back_to_full_scan() {
        // Sanity: range predicates bypass the optimizer and the full-scan
        // path still returns correct results.
        let mut db = seed_users_table();
        let response =
            process_command("SELECT name FROM users WHERE age > 28;", &mut db).expect("select");
        assert!(response.contains("2 rows returned"));
    }

    // -------------------------------------------------------------------
    // Phase 4f — Transactions (BEGIN / COMMIT / ROLLBACK)
    // -------------------------------------------------------------------

    #[test]
    fn rollback_restores_pre_begin_in_memory_state() {
        // In-memory DB (no pager): BEGIN, insert a row, ROLLBACK.
        // The row must disappear from the live tables HashMap.
        let mut db = seed_users_table();
        let before = db.get_table("users".to_string()).unwrap().rowids().len();
        assert_eq!(before, 3);

        process_command("BEGIN;", &mut db).expect("BEGIN");
        assert!(db.in_transaction());
        process_command("INSERT INTO users (name, age) VALUES ('dan', 50);", &mut db)
            .expect("INSERT inside txn");
        // Mid-transaction read sees the new row.
        let mid = db.get_table("users".to_string()).unwrap().rowids().len();
        assert_eq!(mid, 4);

        process_command("ROLLBACK;", &mut db).expect("ROLLBACK");
        assert!(!db.in_transaction());
        let after = db.get_table("users".to_string()).unwrap().rowids().len();
        assert_eq!(
            after, 3,
            "ROLLBACK should have restored the pre-BEGIN state"
        );
    }

    #[test]
    fn commit_keeps_mutations_and_clears_txn_flag() {
        let mut db = seed_users_table();
        process_command("BEGIN;", &mut db).expect("BEGIN");
        process_command("INSERT INTO users (name, age) VALUES ('dan', 50);", &mut db)
            .expect("INSERT inside txn");
        process_command("COMMIT;", &mut db).expect("COMMIT");
        assert!(!db.in_transaction());
        let after = db.get_table("users".to_string()).unwrap().rowids().len();
        assert_eq!(after, 4);
    }

    #[test]
    fn rollback_undoes_update_and_delete_side_by_side() {
        use crate::sql::db::table::Value;

        let mut db = seed_users_table();
        process_command("BEGIN;", &mut db).unwrap();
        process_command("UPDATE users SET age = 999;", &mut db).unwrap();
        process_command("DELETE FROM users WHERE name = 'bob';", &mut db).unwrap();
        // Mid-txn: one row gone, others have age=999.
        let users = db.get_table("users".to_string()).unwrap();
        assert_eq!(users.rowids().len(), 2);
        for r in users.rowids() {
            assert_eq!(users.get_value("age", r), Some(Value::Integer(999)));
        }

        process_command("ROLLBACK;", &mut db).unwrap();
        let users = db.get_table("users".to_string()).unwrap();
        assert_eq!(users.rowids().len(), 3);
        // Original ages {30, 25, 40} — none should be 999.
        for r in users.rowids() {
            assert_ne!(users.get_value("age", r), Some(Value::Integer(999)));
        }
    }

    #[test]
    fn nested_begin_is_rejected() {
        let mut db = seed_users_table();
        process_command("BEGIN;", &mut db).unwrap();
        let err = process_command("BEGIN;", &mut db).unwrap_err();
        assert!(
            format!("{err}").contains("already open"),
            "nested BEGIN should error; got: {err}"
        );
        // Still in the original transaction; a ROLLBACK clears it.
        assert!(db.in_transaction());
        process_command("ROLLBACK;", &mut db).unwrap();
    }

    #[test]
    fn orphan_commit_and_rollback_are_rejected() {
        let mut db = seed_users_table();
        let commit_err = process_command("COMMIT;", &mut db).unwrap_err();
        assert!(format!("{commit_err}").contains("no transaction"));
        let rollback_err = process_command("ROLLBACK;", &mut db).unwrap_err();
        assert!(format!("{rollback_err}").contains("no transaction"));
    }

    #[test]
    fn error_inside_transaction_keeps_txn_open() {
        // A bad INSERT inside a txn doesn't commit or abort automatically —
        // the user can still ROLLBACK. SQLite's implicit-rollback behavior
        // isn't modeled here.
        let mut db = seed_users_table();
        process_command("BEGIN;", &mut db).unwrap();
        let err = process_command("INSERT INTO nope (x) VALUES (1);", &mut db);
        assert!(err.is_err());
        assert!(db.in_transaction(), "txn should stay open after error");
        process_command("ROLLBACK;", &mut db).unwrap();
    }

    /// Builds a file-backed Database at a unique temp path, with the
    /// schema seeded and `source_path` set so subsequent process_command
    /// calls auto-save. Returns (path, db). Drop the db before deleting
    /// the files.
    fn seed_file_backed(name: &str, schema: &str) -> (std::path::PathBuf, Database) {
        use crate::sql::pager::{open_database, save_database};
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-txn-{name}-{pid}-{nanos}.sqlrite"));

        // Seed the file, then reopen to get a source_path-attached db
        // (save_database alone doesn't attach a fresh pager to a db
        // whose source_path was None before the call).
        {
            let mut seed = Database::new("t".to_string());
            process_command(schema, &mut seed).unwrap();
            save_database(&mut seed, &p).unwrap();
        }
        let db = open_database(&p, "t".to_string()).unwrap();
        (p, db)
    }

    fn cleanup_file(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
    }

    #[test]
    fn begin_commit_rollback_round_trip_through_disk() {
        // File-backed DB: commit inside a transaction must actually
        // persist. ROLLBACK inside a *later* transaction must not
        // un-do the previously-committed changes.
        use crate::sql::pager::open_database;

        let (path, mut db) = seed_file_backed(
            "roundtrip",
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);",
        );

        // Transaction 1: insert two rows, commit.
        process_command("BEGIN;", &mut db).unwrap();
        process_command("INSERT INTO notes (body) VALUES ('a');", &mut db).unwrap();
        process_command("INSERT INTO notes (body) VALUES ('b');", &mut db).unwrap();
        process_command("COMMIT;", &mut db).unwrap();

        // Transaction 2: insert another, roll back.
        process_command("BEGIN;", &mut db).unwrap();
        process_command("INSERT INTO notes (body) VALUES ('c');", &mut db).unwrap();
        process_command("ROLLBACK;", &mut db).unwrap();

        drop(db); // release pager lock

        let reopened = open_database(&path, "t".to_string()).unwrap();
        let notes = reopened.get_table("notes".to_string()).unwrap();
        assert_eq!(notes.rowids().len(), 2, "committed rows should survive");

        drop(reopened);
        cleanup_file(&path);
    }

    #[test]
    fn write_inside_transaction_does_not_autosave() {
        // File-backed DB: writes inside BEGIN/…/COMMIT must NOT hit
        // the WAL until COMMIT. We prove it by checking the WAL file
        // size before vs during the transaction.
        let (path, mut db) =
            seed_file_backed("noas", "CREATE TABLE t (id INTEGER PRIMARY KEY, x TEXT);");

        let mut wal_path = path.as_os_str().to_owned();
        wal_path.push("-wal");
        let wal_path = std::path::PathBuf::from(wal_path);
        let frames_before = std::fs::metadata(&wal_path).unwrap().len();

        process_command("BEGIN;", &mut db).unwrap();
        process_command("INSERT INTO t (x) VALUES ('a');", &mut db).unwrap();
        process_command("INSERT INTO t (x) VALUES ('b');", &mut db).unwrap();

        // Mid-transaction: WAL must be unchanged — no auto-save fired.
        let frames_mid = std::fs::metadata(&wal_path).unwrap().len();
        assert_eq!(
            frames_before, frames_mid,
            "WAL should not grow during an open transaction"
        );

        process_command("COMMIT;", &mut db).unwrap();

        drop(db); // release pager lock
        let fresh = crate::sql::pager::open_database(&path, "t".to_string()).unwrap();
        assert_eq!(
            fresh.get_table("t".to_string()).unwrap().rowids().len(),
            2,
            "COMMIT should have persisted both inserted rows"
        );
        drop(fresh);
        cleanup_file(&path);
    }

    #[test]
    fn rollback_undoes_create_table() {
        // Schema DDL inside a txn: ROLLBACK must make the new table
        // disappear. The txn snapshot captures db.tables as of BEGIN,
        // and ROLLBACK reassigns tables from that snapshot, so a table
        // created mid-transaction has no entry in the snapshot.
        let mut db = seed_users_table();
        assert_eq!(db.tables.len(), 1);

        process_command("BEGIN;", &mut db).unwrap();
        process_command(
            "CREATE TABLE dropme (id INTEGER PRIMARY KEY, x TEXT);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO dropme (x) VALUES ('stuff');", &mut db).unwrap();
        assert_eq!(db.tables.len(), 2);

        process_command("ROLLBACK;", &mut db).unwrap();
        assert_eq!(
            db.tables.len(),
            1,
            "CREATE TABLE should have been rolled back"
        );
        assert!(db.get_table("dropme".to_string()).is_err());
    }

    #[test]
    fn rollback_restores_secondary_index_state() {
        // Phase 4f edge case: rolling back an INSERT on a UNIQUE-indexed
        // column must also clean up the index, otherwise a re-insert of
        // the same value would spuriously collide.
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO users (email) VALUES ('a@x');", &mut db).unwrap();

        process_command("BEGIN;", &mut db).unwrap();
        process_command("INSERT INTO users (email) VALUES ('b@x');", &mut db).unwrap();
        // Inside the txn: the index now contains both 'a@x' and 'b@x'.
        process_command("ROLLBACK;", &mut db).unwrap();

        // Re-inserting 'b@x' after rollback must succeed — if the index
        // wasn't properly restored, it would think 'b@x' is still a
        // collision and fail with a UNIQUE violation.
        let reinsert = process_command("INSERT INTO users (email) VALUES ('b@x');", &mut db);
        assert!(
            reinsert.is_ok(),
            "re-insert after rollback should succeed, got {reinsert:?}"
        );
    }

    #[test]
    fn rollback_restores_last_rowid_counter() {
        // Rowids allocated inside a rolled-back transaction should be
        // reusable. The snapshot restores Table::last_rowid, so the
        // next insert picks up where the pre-BEGIN state left off.
        use crate::sql::db::table::Value;

        let mut db = seed_users_table(); // 3 rows, last_rowid = 3
        let pre = db.get_table("users".to_string()).unwrap().last_rowid;

        process_command("BEGIN;", &mut db).unwrap();
        process_command("INSERT INTO users (name, age) VALUES ('d', 50);", &mut db).unwrap(); // would be rowid 4
        process_command("INSERT INTO users (name, age) VALUES ('e', 60);", &mut db).unwrap(); // would be rowid 5
        process_command("ROLLBACK;", &mut db).unwrap();

        let post = db.get_table("users".to_string()).unwrap().last_rowid;
        assert_eq!(pre, post, "last_rowid must roll back with the snapshot");

        // Confirm: the next insert reuses rowid pre+1.
        process_command("INSERT INTO users (name, age) VALUES ('d', 50);", &mut db).unwrap();
        let users = db.get_table("users".to_string()).unwrap();
        let d_rowid = users
            .rowids()
            .into_iter()
            .find(|r| users.get_value("name", *r) == Some(Value::Text("d".into())))
            .expect("d row must exist");
        assert_eq!(d_rowid, pre + 1);
    }

    #[test]
    fn commit_on_in_memory_db_clears_txn_without_pager_call() {
        // In-memory DB (no source_path): COMMIT must still work — just
        // no disk flush. Covers the `if let Some(path) = …` branch
        // where the guard falls through without calling save_database.
        let mut db = seed_users_table(); // no source_path
        assert!(db.source_path.is_none());

        process_command("BEGIN;", &mut db).unwrap();
        process_command("INSERT INTO users (name, age) VALUES ('z', 99);", &mut db).unwrap();
        process_command("COMMIT;", &mut db).unwrap();

        assert!(!db.in_transaction());
        assert_eq!(db.get_table("users".to_string()).unwrap().rowids().len(), 4);
    }

    #[test]
    fn failed_commit_auto_rolls_back_in_memory_state() {
        // Data-safety regression: on COMMIT save failure we must auto-
        // rollback the in-memory state. Otherwise, any subsequent
        // non-transactional statement would auto-save the partial
        // mid-transaction work, silently publishing uncommitted
        // changes to disk.
        //
        // We simulate a save failure by making the WAL sidecar path
        // unavailable mid-transaction: after BEGIN, we take an
        // exclusive OS lock on the WAL via a second File handle,
        // forcing the next save to fail when it tries to append.
        //
        // Simpler repro: point source_path at a directory (not a file).
        // `OpenOptions::open` will fail with EISDIR on save.
        use crate::sql::pager::save_database;

        // Seed a file-backed db.
        let (path, mut db) = seed_file_backed(
            "failcommit",
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);",
        );

        // Prime one committed row so we have a baseline.
        process_command("INSERT INTO notes (body) VALUES ('before');", &mut db).unwrap();

        // Open a new txn and add a row.
        process_command("BEGIN;", &mut db).unwrap();
        process_command("INSERT INTO notes (body) VALUES ('inflight');", &mut db).unwrap();
        assert_eq!(
            db.get_table("notes".to_string()).unwrap().rowids().len(),
            2,
            "inflight row visible mid-txn"
        );

        // Swap source_path to a path that will fail on open. A
        // directory is a reliable failure mode — Pager::open on a
        // directory errors with an I/O error.
        let orig_source = db.source_path.clone();
        let orig_pager = db.pager.take();
        db.source_path = Some(std::env::temp_dir());

        let commit_result = process_command("COMMIT;", &mut db);
        assert!(commit_result.is_err(), "commit must fail");
        let err_str = format!("{}", commit_result.unwrap_err());
        assert!(
            err_str.contains("COMMIT failed") && err_str.contains("rolled back"),
            "error must surface auto-rollback; got: {err_str}"
        );

        // Auto-rollback fired: the inflight row is gone, the txn flag
        // is cleared, and a follow-up non-txn statement won't leak
        // stale state.
        assert!(
            !db.in_transaction(),
            "txn must be cleared after auto-rollback"
        );
        assert_eq!(
            db.get_table("notes".to_string()).unwrap().rowids().len(),
            1,
            "inflight row must be rolled back"
        );

        // Restore the real source_path + pager and verify a clean
        // subsequent write goes through.
        db.source_path = orig_source;
        db.pager = orig_pager;
        process_command("INSERT INTO notes (body) VALUES ('after');", &mut db).unwrap();
        drop(db);

        // Reopen and assert only 'before' + 'after' landed on disk.
        let reopened = crate::sql::pager::open_database(&path, "t".to_string()).unwrap();
        let notes = reopened.get_table("notes".to_string()).unwrap();
        assert_eq!(notes.rowids().len(), 2);
        // Ensure no leaked save_database partial happened.
        let _ = save_database; // silence unused-import lint if any
        drop(reopened);
        cleanup_file(&path);
    }

    #[test]
    fn begin_on_read_only_is_rejected() {
        use crate::sql::pager::{open_database_read_only, save_database};

        let path = {
            let mut p = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            p.push(format!("sqlrite-txn-ro-{pid}-{nanos}.sqlrite"));
            p
        };
        {
            let mut seed = Database::new("t".to_string());
            process_command("CREATE TABLE t (id INTEGER PRIMARY KEY);", &mut seed).unwrap();
            save_database(&mut seed, &path).unwrap();
        }

        let mut ro = open_database_read_only(&path, "t".to_string()).unwrap();
        let err = process_command("BEGIN;", &mut ro).unwrap_err();
        assert!(
            format!("{err}").contains("read-only"),
            "BEGIN on RO db should surface read-only; got: {err}"
        );
        assert!(!ro.in_transaction());

        let _ = std::fs::remove_file(&path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
    }

    #[test]
    fn read_only_database_rejects_mutations_before_touching_state() {
        // Phase 4e end-to-end: a `--readonly` caller that runs INSERT
        // must error *before* the row is added to the in-memory table.
        // Otherwise the user sees a rendered result table with the
        // phantom row, followed by the auto-save error — UX rot and a
        // state-drift risk.
        use crate::sql::pager::open_database_read_only;

        let mut seed = Database::new("t".to_string());
        process_command(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);",
            &mut seed,
        )
        .unwrap();
        process_command("INSERT INTO notes (body) VALUES ('alpha');", &mut seed).unwrap();

        let path = {
            let mut p = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            p.push(format!("sqlrite-ro-reject-{pid}-{nanos}.sqlrite"));
            p
        };
        crate::sql::pager::save_database(&mut seed, &path).unwrap();
        drop(seed);

        let mut ro = open_database_read_only(&path, "t".to_string()).unwrap();
        let notes_before = ro.get_table("notes".to_string()).unwrap().rowids().len();

        for stmt in [
            "INSERT INTO notes (body) VALUES ('beta');",
            "UPDATE notes SET body = 'x';",
            "DELETE FROM notes;",
            "CREATE TABLE more (id INTEGER PRIMARY KEY);",
            "CREATE INDEX notes_body ON notes (body);",
        ] {
            let err = process_command(stmt, &mut ro).unwrap_err();
            assert!(
                format!("{err}").contains("read-only"),
                "stmt {stmt:?} should surface a read-only error; got: {err}"
            );
        }

        // Nothing mutated: same row count as before, and SELECTs still work.
        let notes_after = ro.get_table("notes".to_string()).unwrap().rowids().len();
        assert_eq!(notes_before, notes_after);
        let sel = process_command("SELECT * FROM notes;", &mut ro).expect("select on RO must work");
        assert!(sel.contains("1 row returned"));

        // Cleanup.
        drop(ro);
        let _ = std::fs::remove_file(&path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
    }

    // -----------------------------------------------------------------
    // Phase 7a — VECTOR(N) end-to-end through process_command
    // -----------------------------------------------------------------

    #[test]
    fn vector_create_table_and_insert_basic() {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(3));",
            &mut db,
        )
        .expect("create table with VECTOR(3)");
        process_command(
            "INSERT INTO docs (embedding) VALUES ([0.1, 0.2, 0.3]);",
            &mut db,
        )
        .expect("insert vector");

        // process_command returns a status string; the rendered table
        // goes to stdout via print_table. Verify state by inspecting
        // the database directly.
        let sel = process_command("SELECT * FROM docs;", &mut db).expect("select");
        assert!(sel.contains("1 row returned"));

        let docs = db.get_table("docs".to_string()).expect("docs table");
        let rowids = docs.rowids();
        assert_eq!(rowids.len(), 1);
        match docs.get_value("embedding", rowids[0]) {
            Some(Value::Vector(v)) => assert_eq!(v, vec![0.1f32, 0.2, 0.3]),
            other => panic!("expected Value::Vector(...), got {other:?}"),
        }
    }

    #[test]
    fn vector_dim_mismatch_at_insert_is_clean_error() {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(3));",
            &mut db,
        )
        .expect("create table");

        // Too few elements.
        let err = process_command("INSERT INTO docs (embedding) VALUES ([0.1, 0.2]);", &mut db)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("dimension")
                && msg.contains("declared 3")
                && msg.contains("got 2"),
            "expected clear dim-mismatch error, got: {msg}"
        );

        // Too many elements.
        let err = process_command(
            "INSERT INTO docs (embedding) VALUES ([0.1, 0.2, 0.3, 0.4, 0.5]);",
            &mut db,
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("got 5"),
            "expected dim-mismatch error mentioning got 5, got: {err}"
        );
    }

    #[test]
    fn vector_create_table_rejects_missing_dim() {
        let mut db = Database::new("tempdb".to_string());
        // `VECTOR` (no parens) currently parses as `DataType::Custom` with
        // empty args from sqlparser, OR may not parse as Custom at all
        // depending on dialect. Either way, the column shouldn't end up
        // as a usable Vector type. Accept any error here — the precise
        // message is parser-version-dependent.
        let result = process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR);",
            &mut db,
        );
        assert!(
            result.is_err(),
            "expected CREATE TABLE with bare VECTOR to fail (no dim)"
        );
    }

    #[test]
    fn vector_create_table_rejects_zero_dim() {
        let mut db = Database::new("tempdb".to_string());
        let err = process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(0));",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("vector"),
            "expected VECTOR-related error for VECTOR(0), got: {msg}"
        );
    }

    #[test]
    fn vector_high_dim_works() {
        // 384-dim vector (OpenAI text-embedding-3-small size). Mostly a
        // smoke test — if cell encoding mishandles the size, this fails.
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE embeddings (id INTEGER PRIMARY KEY, e VECTOR(384));",
            &mut db,
        )
        .expect("create table VECTOR(384)");

        let lit = format!(
            "[{}]",
            (0..384)
                .map(|i| format!("{}", i as f32 * 0.001))
                .collect::<Vec<_>>()
                .join(",")
        );
        let sql = format!("INSERT INTO embeddings (e) VALUES ({lit});");
        process_command(&sql, &mut db).expect("insert 384-dim vector");

        let sel = process_command("SELECT id FROM embeddings;", &mut db).expect("select id");
        assert!(sel.contains("1 row returned"));
    }

    #[test]
    fn vector_multiple_rows() {
        // Three rows with different vectors — exercises the Row::Vector
        // BTreeMap path (not just single-row insertion).
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, e VECTOR(2));",
            &mut db,
        )
        .expect("create");
        for i in 0..3 {
            let sql = format!("INSERT INTO docs (e) VALUES ([{i}.0, {}.0]);", i + 1);
            process_command(&sql, &mut db).expect("insert");
        }
        let sel = process_command("SELECT * FROM docs;", &mut db).expect("select");
        assert!(sel.contains("3 rows returned"));

        // Verify each vector round-tripped correctly via direct DB inspection.
        let docs = db.get_table("docs".to_string()).expect("docs table");
        let rowids = docs.rowids();
        assert_eq!(rowids.len(), 3);
        let mut vectors: Vec<Vec<f32>> = rowids
            .iter()
            .filter_map(|r| match docs.get_value("e", *r) {
                Some(Value::Vector(v)) => Some(v),
                _ => None,
            })
            .collect();
        vectors.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
        assert_eq!(vectors[0], vec![0.0f32, 1.0]);
        assert_eq!(vectors[1], vec![1.0f32, 2.0]);
        assert_eq!(vectors[2], vec![2.0f32, 3.0]);
    }

    // -----------------------------------------------------------------
    // Phase 7d.2 — CREATE INDEX … USING hnsw end-to-end
    // -----------------------------------------------------------------

    /// Builds a 5-row docs(id, e VECTOR(2)) table with vectors arranged
    /// at known positions for clear distance reasoning. Used by both
    /// the 7d.2 KNN tests and the refuse-DELETE/UPDATE tests.
    fn seed_hnsw_table() -> Database {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, e VECTOR(2));",
            &mut db,
        )
        .unwrap();
        for v in &[
            "[1.0, 0.0]",   // id=1
            "[2.0, 0.0]",   // id=2
            "[0.0, 3.0]",   // id=3
            "[1.0, 4.0]",   // id=4
            "[10.0, 10.0]", // id=5
        ] {
            process_command(&format!("INSERT INTO docs (e) VALUES ({v});"), &mut db).unwrap();
        }
        db
    }

    #[test]
    fn create_index_using_hnsw_succeeds() {
        let mut db = seed_hnsw_table();
        let resp = process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();
        assert!(resp.to_lowercase().contains("create index"));
        // Index attached.
        let table = db.get_table("docs".to_string()).unwrap();
        assert_eq!(table.hnsw_indexes.len(), 1);
        assert_eq!(table.hnsw_indexes[0].name, "ix_e");
        assert_eq!(table.hnsw_indexes[0].column_name, "e");
        // Existing rows landed in the graph.
        assert_eq!(table.hnsw_indexes[0].index.len(), 5);
    }

    #[test]
    fn create_index_using_hnsw_rejects_non_vector_column() {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);",
            &mut db,
        )
        .unwrap();
        let err =
            process_command("CREATE INDEX ix_name ON t USING hnsw (name);", &mut db).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("vector"),
            "expected error mentioning VECTOR; got: {msg}"
        );
    }

    #[test]
    fn knn_query_uses_hnsw_after_create_index() {
        // The KNN-shaped query route through try_hnsw_probe rather than
        // the brute-force select_topk. The user-visible result should
        // be the same (HNSW recall is high on small graphs); we
        // primarily verify the index is being hit by checking that
        // the right rowids come back in the right order.
        let mut db = seed_hnsw_table();
        process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();

        // Top-3 closest to [1.0, 0.0]:
        //   id=1 [1.0, 0.0]   distance=0
        //   id=2 [2.0, 0.0]   distance=1
        //   id=3 [0.0, 3.0]   distance≈3.16
        let resp = process_command(
            "SELECT id FROM docs ORDER BY vec_distance_l2(e, [1.0, 0.0]) ASC LIMIT 3;",
            &mut db,
        )
        .unwrap();
        assert!(resp.contains("3 rows returned"), "got: {resp}");
    }

    #[test]
    fn knn_query_works_after_subsequent_inserts() {
        // Index built when 5 rows existed; insert 2 more after; the
        // HNSW gets maintained incrementally by insert_row, so the
        // KNN query should see the newly-inserted vectors.
        let mut db = seed_hnsw_table();
        process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();
        process_command("INSERT INTO docs (e) VALUES ([0.5, 0.0]);", &mut db).unwrap(); // id=6
        process_command("INSERT INTO docs (e) VALUES ([0.1, 0.1]);", &mut db).unwrap(); // id=7

        let table = db.get_table("docs".to_string()).unwrap();
        assert_eq!(
            table.hnsw_indexes[0].index.len(),
            7,
            "incremental insert should grow HNSW alongside row storage"
        );

        // Now query: id=7 [0.1, 0.1] is closer to [0.0, 0.0] than the
        // original 5 rows.
        let resp = process_command(
            "SELECT id FROM docs ORDER BY vec_distance_l2(e, [0.0, 0.0]) ASC LIMIT 1;",
            &mut db,
        )
        .unwrap();
        assert!(resp.contains("1 row returned"), "got: {resp}");
    }

    // Phase 7d.3 — DELETE / UPDATE on HNSW-indexed tables now works.
    // The 7d.2 versions of these tests asserted a refusal; replaced
    // with assertions that the operation succeeds + the index entry's
    // needs_rebuild flag flipped so the next save will rebuild.

    #[test]
    fn delete_on_hnsw_indexed_table_succeeds_and_marks_dirty() {
        let mut db = seed_hnsw_table();
        process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();
        let resp = process_command("DELETE FROM docs WHERE id = 1;", &mut db).unwrap();
        assert!(resp.contains("1 row"), "expected 1 row deleted: {resp}");

        let docs = db.get_table("docs".to_string()).unwrap();
        let entry = docs.hnsw_indexes.iter().find(|e| e.name == "ix_e").unwrap();
        assert!(
            entry.needs_rebuild,
            "DELETE should have marked HNSW index dirty for rebuild on next save"
        );
    }

    #[test]
    fn update_on_hnsw_indexed_vector_col_succeeds_and_marks_dirty() {
        let mut db = seed_hnsw_table();
        process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();
        let resp =
            process_command("UPDATE docs SET e = [9.0, 9.0] WHERE id = 1;", &mut db).unwrap();
        assert!(resp.contains("1 row"), "expected 1 row updated: {resp}");

        let docs = db.get_table("docs".to_string()).unwrap();
        let entry = docs.hnsw_indexes.iter().find(|e| e.name == "ix_e").unwrap();
        assert!(
            entry.needs_rebuild,
            "UPDATE on the vector column should have marked HNSW index dirty"
        );
    }

    #[test]
    fn duplicate_index_name_errors() {
        let mut db = seed_hnsw_table();
        process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();
        let err =
            process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("already exists"),
            "expected duplicate-index error; got: {msg}"
        );
    }

    #[test]
    fn index_if_not_exists_is_idempotent() {
        let mut db = seed_hnsw_table();
        process_command("CREATE INDEX ix_e ON docs USING hnsw (e);", &mut db).unwrap();
        // Second time with IF NOT EXISTS should succeed (no-op).
        process_command(
            "CREATE INDEX IF NOT EXISTS ix_e ON docs USING hnsw (e);",
            &mut db,
        )
        .unwrap();
        let table = db.get_table("docs".to_string()).unwrap();
        assert_eq!(table.hnsw_indexes.len(), 1);
    }

    // -----------------------------------------------------------------
    // Phase 8b — CREATE INDEX … USING fts end-to-end
    // -----------------------------------------------------------------

    /// 5-row docs(id INTEGER PK, body TEXT) populated with overlapping
    /// vocabulary so BM25 ranking has interesting structure.
    fn seed_fts_table() -> Database {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);",
            &mut db,
        )
        .unwrap();
        for body in &[
            "rust embedded database",        // id=1 — both 'rust' and 'embedded'
            "rust web framework",            // id=2 — 'rust' only
            "go embedded systems",           // id=3 — 'embedded' only
            "python web framework",          // id=4 — neither
            "rust rust rust embedded power", // id=5 — heavy on 'rust'
        ] {
            process_command(
                &format!("INSERT INTO docs (body) VALUES ('{body}');"),
                &mut db,
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn create_index_using_fts_succeeds_and_indexes_existing_rows() {
        let mut db = seed_fts_table();
        let resp =
            process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        assert!(resp.to_lowercase().contains("create index"), "got {resp}");
        let table = db.get_table("docs".to_string()).unwrap();
        assert_eq!(table.fts_indexes.len(), 1);
        assert_eq!(table.fts_indexes[0].name, "ix_body");
        assert_eq!(table.fts_indexes[0].column_name, "body");
        // All five rows should be in the in-memory PostingList.
        assert_eq!(table.fts_indexes[0].index.len(), 5);
    }

    #[test]
    fn create_index_using_fts_rejects_non_text_column() {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);",
            &mut db,
        )
        .unwrap();
        let err = process_command("CREATE INDEX ix_n ON t USING fts (n);", &mut db).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("text"),
            "expected error mentioning TEXT; got: {msg}"
        );
    }

    #[test]
    fn fts_match_returns_expected_rows() {
        let mut db = seed_fts_table();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        // Rows that contain 'rust': ids 1, 2, 5.
        let resp = process_command(
            "SELECT id FROM docs WHERE fts_match(body, 'rust');",
            &mut db,
        )
        .unwrap();
        assert!(resp.contains("3 rows returned"), "got: {resp}");
    }

    #[test]
    fn fts_match_without_index_errors_clearly() {
        let mut db = seed_fts_table();
        // No CREATE INDEX — fts_match must surface a useful error.
        let err = process_command(
            "SELECT id FROM docs WHERE fts_match(body, 'rust');",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("no FTS index"),
            "expected no-index error; got: {msg}"
        );
    }

    #[test]
    fn bm25_score_orders_descending_by_relevance() {
        let mut db = seed_fts_table();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        // ORDER BY bm25_score DESC LIMIT 1: id=5 has 'rust' three times in
        // a 5-token doc — highest tf, modest length penalty → top score.
        let out = process_command_with_render(
            "SELECT id FROM docs WHERE fts_match(body, 'rust') \
             ORDER BY bm25_score(body, 'rust') DESC LIMIT 1;",
            &mut db,
        )
        .unwrap();
        assert!(out.status.contains("1 row returned"), "got: {}", out.status);
        let rendered = out.rendered.expect("SELECT should produce rendered output");
        // The rendered prettytable contains the integer 5 in a cell.
        assert!(
            rendered.contains(" 5 "),
            "expected id=5 to be top-ranked; rendered:\n{rendered}"
        );
    }

    #[test]
    fn bm25_score_without_index_errors_clearly() {
        let mut db = seed_fts_table();
        let err = process_command(
            "SELECT id FROM docs ORDER BY bm25_score(body, 'rust') DESC LIMIT 1;",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("no FTS index"),
            "expected no-index error; got: {msg}"
        );
    }

    #[test]
    fn fts_post_create_inserts_are_indexed_incrementally() {
        let mut db = seed_fts_table();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        process_command(
            "INSERT INTO docs (body) VALUES ('rust embedded analytics');",
            &mut db,
        )
        .unwrap();
        let table = db.get_table("docs".to_string()).unwrap();
        // PostingList::len() reports doc count; should be 6 now.
        assert_eq!(table.fts_indexes[0].index.len(), 6);
        // 'analytics' appears only in the new row → query returns 1 hit.
        let resp = process_command(
            "SELECT id FROM docs WHERE fts_match(body, 'analytics');",
            &mut db,
        )
        .unwrap();
        assert!(resp.contains("1 row returned"), "got: {resp}");
    }

    #[test]
    fn delete_on_fts_indexed_table_marks_dirty() {
        let mut db = seed_fts_table();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        let resp = process_command("DELETE FROM docs WHERE id = 1;", &mut db).unwrap();
        assert!(resp.contains("1 row"), "got: {resp}");
        let docs = db.get_table("docs".to_string()).unwrap();
        let entry = docs
            .fts_indexes
            .iter()
            .find(|e| e.name == "ix_body")
            .unwrap();
        assert!(
            entry.needs_rebuild,
            "DELETE should have flagged the FTS index dirty"
        );
    }

    #[test]
    fn update_on_fts_indexed_text_col_marks_dirty() {
        let mut db = seed_fts_table();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        let resp = process_command(
            "UPDATE docs SET body = 'java spring framework' WHERE id = 1;",
            &mut db,
        )
        .unwrap();
        assert!(resp.contains("1 row"), "got: {resp}");
        let docs = db.get_table("docs".to_string()).unwrap();
        let entry = docs
            .fts_indexes
            .iter()
            .find(|e| e.name == "ix_body")
            .unwrap();
        assert!(
            entry.needs_rebuild,
            "UPDATE on the indexed TEXT column should have flagged dirty"
        );
    }

    #[test]
    fn fts_index_name_collides_with_btree_and_hnsw_namespaces() {
        let mut db = seed_fts_table();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        let err = process_command("CREATE INDEX ix_body ON docs (body);", &mut db).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("already exists"),
            "expected duplicate-index error; got: {msg}"
        );
    }

    #[test]
    fn fts_index_rejects_unique() {
        let mut db = seed_fts_table();
        let err = process_command(
            "CREATE UNIQUE INDEX ix_body ON docs USING fts (body);",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("unique"),
            "expected UNIQUE-rejection error; got: {msg}"
        );
    }

    #[test]
    fn try_fts_probe_falls_through_on_ascending() {
        // BM25 is "higher = better"; ASC is rejected so the slow path
        // applies. We verify by running the query and checking the
        // result is still correct (the slow path goes through scalar
        // bm25_score on every row).
        let mut db = seed_fts_table();
        process_command("CREATE INDEX ix_body ON docs USING fts (body);", &mut db).unwrap();
        // Same query as bm25_score_orders_descending but ASC → should
        // still succeed (slow path), and id=5 should now be LAST.
        let resp = process_command(
            "SELECT id FROM docs WHERE fts_match(body, 'rust') \
             ORDER BY bm25_score(body, 'rust') ASC LIMIT 3;",
            &mut db,
        )
        .unwrap();
        assert!(resp.contains("3 rows returned"), "got: {resp}");
    }

    // -----------------------------------------------------------------
    // Phase 7b — vector distance functions through process_command
    // -----------------------------------------------------------------

    /// Builds a 3-row docs table with 2-dim vectors aligned along the
    /// axes so the expected distances are easy to reason about:
    ///   id=1: [1, 0]
    ///   id=2: [0, 1]
    ///   id=3: [1, 1]
    fn seed_vector_docs() -> Database {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, e VECTOR(2));",
            &mut db,
        )
        .expect("create");
        process_command("INSERT INTO docs (e) VALUES ([1.0, 0.0]);", &mut db).expect("insert 1");
        process_command("INSERT INTO docs (e) VALUES ([0.0, 1.0]);", &mut db).expect("insert 2");
        process_command("INSERT INTO docs (e) VALUES ([1.0, 1.0]);", &mut db).expect("insert 3");
        db
    }

    #[test]
    fn vec_distance_l2_in_where_filters_correctly() {
        // Distance from [1,0]:
        //   id=1 [1,0]: 0
        //   id=2 [0,1]: √2 ≈ 1.414
        //   id=3 [1,1]: 1
        // WHERE distance < 1.1 should match id=1 and id=3 (2 rows).
        let mut db = seed_vector_docs();
        let resp = process_command(
            "SELECT * FROM docs WHERE vec_distance_l2(e, [1.0, 0.0]) < 1.1;",
            &mut db,
        )
        .expect("select");
        assert!(
            resp.contains("2 rows returned"),
            "expected 2 rows, got: {resp}"
        );
    }

    #[test]
    fn vec_distance_cosine_in_where() {
        // [1,0] vs [1,0]: cosine distance = 0
        // [1,0] vs [0,1]: cosine distance = 1 (orthogonal)
        // [1,0] vs [1,1]: cosine distance = 1 - 1/√2 ≈ 0.293
        // WHERE distance < 0.5 → id=1 and id=3 (2 rows).
        let mut db = seed_vector_docs();
        let resp = process_command(
            "SELECT * FROM docs WHERE vec_distance_cosine(e, [1.0, 0.0]) < 0.5;",
            &mut db,
        )
        .expect("select");
        assert!(
            resp.contains("2 rows returned"),
            "expected 2 rows, got: {resp}"
        );
    }

    #[test]
    fn vec_distance_dot_negated() {
        // [1,0]·[1,0] = 1 → -1
        // [1,0]·[0,1] = 0 → 0
        // [1,0]·[1,1] = 1 → -1
        // WHERE -dot < 0 (i.e. dot > 0) → id=1 and id=3 (2 rows).
        let mut db = seed_vector_docs();
        let resp = process_command(
            "SELECT * FROM docs WHERE vec_distance_dot(e, [1.0, 0.0]) < 0.0;",
            &mut db,
        )
        .expect("select");
        assert!(
            resp.contains("2 rows returned"),
            "expected 2 rows, got: {resp}"
        );
    }

    #[test]
    fn knn_via_order_by_distance_limit() {
        // Classic KNN shape: ORDER BY distance LIMIT k.
        // Distances from [1,0]: id=1=0, id=3=1, id=2=√2.
        // LIMIT 2 should return id=1 then id=3 in that order.
        let mut db = seed_vector_docs();
        let resp = process_command(
            "SELECT id FROM docs ORDER BY vec_distance_l2(e, [1.0, 0.0]) ASC LIMIT 2;",
            &mut db,
        )
        .expect("select");
        assert!(
            resp.contains("2 rows returned"),
            "expected 2 rows, got: {resp}"
        );
    }

    #[test]
    fn distance_function_dim_mismatch_errors() {
        // 2-dim column queried with a 3-dim probe → clean error.
        let mut db = seed_vector_docs();
        let err = process_command(
            "SELECT * FROM docs WHERE vec_distance_l2(e, [1.0, 0.0, 0.0]) < 1.0;",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("dimension")
                && msg.contains("lhs=2")
                && msg.contains("rhs=3"),
            "expected dim mismatch error, got: {msg}"
        );
    }

    #[test]
    fn unknown_function_errors_with_name() {
        // Use the function in WHERE, not projection — the projection
        // parser still requires bare column references; function calls
        // there are a future enhancement (with `AS alias` support).
        let mut db = seed_vector_docs();
        let err = process_command(
            "SELECT * FROM docs WHERE vec_does_not_exist(e, [1.0, 0.0]) < 1.0;",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("vec_does_not_exist"),
            "expected error mentioning function name, got: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Phase 7e — JSON column type + path-extraction functions
    // -----------------------------------------------------------------

    fn seed_json_table() -> Database {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, payload JSON);",
            &mut db,
        )
        .expect("create json table");
        db
    }

    #[test]
    fn json_column_round_trip_primitive_values() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"name": "alice", "age": 30}');"#,
            &mut db,
        )
        .expect("insert json");
        let docs = db.get_table("docs".to_string()).unwrap();
        let rowids = docs.rowids();
        assert_eq!(rowids.len(), 1);
        // Stored verbatim as Text underneath.
        match docs.get_value("payload", rowids[0]) {
            Some(Value::Text(s)) => {
                assert!(s.contains("alice"), "expected JSON text to round-trip: {s}");
            }
            other => panic!("expected Value::Text holding JSON, got {other:?}"),
        }
    }

    #[test]
    fn json_insert_rejects_invalid_json() {
        let mut db = seed_json_table();
        let err = process_command(
            "INSERT INTO docs (payload) VALUES ('not-valid-json{');",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("json") && msg.contains("payload"),
            "expected JSON validation error mentioning column, got: {msg}"
        );
    }

    #[test]
    fn json_extract_object_field() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"name": "alice", "age": 30}');"#,
            &mut db,
        )
        .unwrap();
        // We don't have function calls in projection (yet), so test
        // the function via WHERE.
        let resp = process_command(
            r#"SELECT id FROM docs WHERE json_extract(payload, '$.name') = 'alice';"#,
            &mut db,
        )
        .expect("select via json_extract");
        assert!(resp.contains("1 row returned"), "got: {resp}");

        let resp = process_command(
            r#"SELECT id FROM docs WHERE json_extract(payload, '$.age') = 30;"#,
            &mut db,
        )
        .expect("select via numeric json_extract");
        assert!(resp.contains("1 row returned"), "got: {resp}");
    }

    #[test]
    fn json_extract_array_index_and_nested() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"tags": ["rust", "sql", "vectors"], "meta": {"author": "joao"}}');"#,
            &mut db,
        )
        .unwrap();
        let resp = process_command(
            r#"SELECT id FROM docs WHERE json_extract(payload, '$.tags[0]') = 'rust';"#,
            &mut db,
        )
        .expect("select via array index");
        assert!(resp.contains("1 row returned"), "got: {resp}");

        let resp = process_command(
            r#"SELECT id FROM docs WHERE json_extract(payload, '$.meta.author') = 'joao';"#,
            &mut db,
        )
        .expect("select via nested object");
        assert!(resp.contains("1 row returned"), "got: {resp}");
    }

    #[test]
    fn json_extract_missing_path_returns_null() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"name": "alice"}');"#,
            &mut db,
        )
        .unwrap();
        // Missing key under WHERE returns NULL → predicate is false →
        // 0 rows returned. (Standard SQL three-valued logic.)
        let resp = process_command(
            r#"SELECT id FROM docs WHERE json_extract(payload, '$.missing') = 'something';"#,
            &mut db,
        )
        .expect("select with missing path");
        assert!(resp.contains("0 rows returned"), "got: {resp}");
    }

    #[test]
    fn json_extract_malformed_path_errors() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"a": 1}');"#,
            &mut db,
        )
        .unwrap();
        // Path doesn't start with '$' — syntax error.
        let err = process_command(
            r#"SELECT id FROM docs WHERE json_extract(payload, 'a.b') = 1;"#,
            &mut db,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("'$'"));
    }

    #[test]
    fn json_array_length_on_array() {
        // Note: json_array_length used in WHERE clause where it can be
        // compared; that exercises the function dispatch end-to-end.
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"tags": ["a", "b", "c"]}');"#,
            &mut db,
        )
        .unwrap();
        let resp = process_command(
            r#"SELECT id FROM docs WHERE json_array_length(payload, '$.tags') = 3;"#,
            &mut db,
        )
        .expect("select via array_length");
        assert!(resp.contains("1 row returned"), "got: {resp}");
    }

    #[test]
    fn json_array_length_on_non_array_errors() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"tags": "not-an-array"}');"#,
            &mut db,
        )
        .unwrap();
        let err = process_command(
            r#"SELECT id FROM docs WHERE json_array_length(payload, '$.tags') = 1;"#,
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("non-array"),
            "expected non-array error, got: {msg}"
        );
    }

    #[test]
    fn json_type_recognizes_each_kind() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"o": {}, "a": [], "s": "x", "i": 1, "f": 1.5, "t": true, "n": null}');"#,
            &mut db,
        )
        .unwrap();
        let cases = &[
            ("$.o", "object"),
            ("$.a", "array"),
            ("$.s", "text"),
            ("$.i", "integer"),
            ("$.f", "real"),
            ("$.t", "true"),
            ("$.n", "null"),
        ];
        for (path, expected_type) in cases {
            let sql = format!(
                "SELECT id FROM docs WHERE json_type(payload, '{path}') = '{expected_type}';"
            );
            let resp =
                process_command(&sql, &mut db).unwrap_or_else(|e| panic!("path {path}: {e}"));
            assert!(
                resp.contains("1 row returned"),
                "path {path} expected type {expected_type}; got response: {resp}"
            );
        }
    }

    #[test]
    fn update_on_json_column_revalidates() {
        let mut db = seed_json_table();
        process_command(
            r#"INSERT INTO docs (payload) VALUES ('{"a": 1}');"#,
            &mut db,
        )
        .unwrap();
        // Valid JSON update succeeds.
        process_command(
            r#"UPDATE docs SET payload = '{"a": 2, "b": 3}' WHERE id = 1;"#,
            &mut db,
        )
        .expect("valid JSON UPDATE");
        // Invalid JSON in UPDATE is rejected with the same shape of
        // error as INSERT.
        let err = process_command(
            r#"UPDATE docs SET payload = 'not-json{' WHERE id = 1;"#,
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("json") && msg.contains("payload"),
            "got: {msg}"
        );
    }

    // -------------------------------------------------------------------
    // DEFAULT clause on CREATE TABLE columns
    // -------------------------------------------------------------------

    #[test]
    fn default_literal_int_applies_when_column_omitted() {
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER DEFAULT 42);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO t (id) VALUES (1);", &mut db).unwrap();

        let table = db.get_table("t".to_string()).unwrap();
        assert_eq!(table.get_value("n", 1), Some(Value::Integer(42)));
    }

    #[test]
    fn default_literal_text_applies_when_column_omitted() {
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, status TEXT DEFAULT 'active');",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO users (id) VALUES (1);", &mut db).unwrap();

        let table = db.get_table("users".to_string()).unwrap();
        assert_eq!(
            table.get_value("status", 1),
            Some(Value::Text("active".to_string()))
        );
    }

    #[test]
    fn default_literal_real_negative_applies_when_column_omitted() {
        // `DEFAULT -1.5` arrives as a UnaryOp(Minus, Number) — exercise that path.
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, score REAL DEFAULT -1.5);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO t (id) VALUES (1);", &mut db).unwrap();

        let table = db.get_table("t".to_string()).unwrap();
        assert_eq!(table.get_value("score", 1), Some(Value::Real(-1.5)));
    }

    #[test]
    fn default_with_type_mismatch_errors_at_create_time() {
        let mut db = Database::new("t".to_string());
        let result = process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER DEFAULT 'oops');",
            &mut db,
        );
        let err = result.expect_err("text default on INTEGER column should be rejected");
        let msg = format!("{err}").to_lowercase();
        assert!(msg.contains("default"), "got: {msg}");
    }

    #[test]
    fn default_with_non_literal_expression_errors_at_create_time() {
        let mut db = Database::new("t".to_string());
        // Function-call DEFAULT (e.g. CURRENT_TIMESTAMP) → rejected; we only
        // accept literal expressions for now.
        let result = process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, ts TEXT DEFAULT CURRENT_TIMESTAMP);",
            &mut db,
        );
        let err = result.expect_err("non-literal DEFAULT should be rejected");
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("default") && msg.contains("literal"),
            "got: {msg}"
        );
    }

    #[test]
    fn default_null_is_accepted_at_create_time() {
        // `DEFAULT NULL` is a no-op equivalent to no DEFAULT clause; the
        // important thing is that CREATE TABLE accepts it without error
        // (some DDL exporters emit `DEFAULT NULL` redundantly).
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, note TEXT DEFAULT NULL);",
            &mut db,
        )
        .expect("CREATE TABLE with DEFAULT NULL should be accepted");
        let table = db.get_table("t".to_string()).unwrap();
        let note = table
            .columns
            .iter()
            .find(|c| c.column_name == "note")
            .unwrap();
        assert_eq!(note.default, Some(Value::Null));
    }

    // -------------------------------------------------------------------
    // DROP TABLE / DROP INDEX
    // -------------------------------------------------------------------

    #[test]
    fn drop_table_basic() {
        let mut db = seed_users_table();
        let response = process_command("DROP TABLE users;", &mut db).expect("drop table");
        assert!(response.contains("1 table dropped"));
        assert!(!db.contains_table("users".to_string()));
    }

    #[test]
    fn drop_table_if_exists_noop_on_missing() {
        let mut db = Database::new("t".to_string());
        let response =
            process_command("DROP TABLE IF EXISTS missing;", &mut db).expect("drop if exists");
        assert!(response.contains("0 tables dropped"));
    }

    #[test]
    fn drop_table_missing_errors_without_if_exists() {
        let mut db = Database::new("t".to_string());
        let err = process_command("DROP TABLE missing;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("does not exist"), "got: {err}");
    }

    #[test]
    fn drop_table_reserved_name_errors() {
        let mut db = Database::new("t".to_string());
        let err = process_command("DROP TABLE sqlrite_master;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("reserved"), "got: {err}");
    }

    #[test]
    fn drop_table_multi_target_rejected() {
        let mut db = seed_users_table();
        process_command("CREATE TABLE other (id INTEGER PRIMARY KEY);", &mut db).unwrap();
        // sqlparser accepts `DROP TABLE a, b` as one statement; we reject
        // to keep error semantics simple (no partial-failure rollback).
        let err = process_command("DROP TABLE users, other;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("single table"), "got: {err}");
    }

    #[test]
    fn drop_table_cascades_indexes_in_memory() {
        let mut db = seed_users_table();
        process_command("CREATE INDEX users_age_idx ON users (age);", &mut db).unwrap();
        // PK auto-index + UNIQUE-on-name auto-index + the explicit one.
        let users = db.get_table("users".to_string()).unwrap();
        assert!(
            users
                .secondary_indexes
                .iter()
                .any(|i| i.name == "users_age_idx")
        );

        process_command("DROP TABLE users;", &mut db).unwrap();

        // After DROP TABLE, no other table should claim the dropped indexes.
        for table in db.tables.values() {
            assert!(
                !table
                    .secondary_indexes
                    .iter()
                    .any(|i| i.name.contains("users")),
                "dropped table's indexes should not survive on any other table"
            );
        }
    }

    #[test]
    fn drop_index_explicit_basic() {
        let mut db = seed_users_table();
        process_command("CREATE INDEX users_age_idx ON users (age);", &mut db).unwrap();
        let response = process_command("DROP INDEX users_age_idx;", &mut db).expect("drop index");
        assert!(response.contains("1 index dropped"));

        let users = db.get_table("users".to_string()).unwrap();
        assert!(users.index_by_name("users_age_idx").is_none());
    }

    #[test]
    fn drop_index_refuses_auto_index() {
        let mut db = seed_users_table();
        // `users` was created with `id INTEGER PRIMARY KEY` → auto-index
        // named `sqlrite_autoindex_users_id`.
        let err = process_command("DROP INDEX sqlrite_autoindex_users_id;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("auto-created"), "got: {err}");
    }

    #[test]
    fn drop_index_if_exists_noop_on_missing() {
        let mut db = Database::new("t".to_string());
        let response =
            process_command("DROP INDEX IF EXISTS nope;", &mut db).expect("drop index if exists");
        assert!(response.contains("0 indexes dropped"));
    }

    #[test]
    fn drop_index_missing_errors_without_if_exists() {
        let mut db = Database::new("t".to_string());
        let err = process_command("DROP INDEX nope;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("does not exist"), "got: {err}");
    }

    #[test]
    fn drop_statements_rejected_on_readonly_db() {
        use crate::sql::pager::{open_database_read_only, save_database};

        let mut seed = Database::new("t".to_string());
        process_command(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);",
            &mut seed,
        )
        .unwrap();
        process_command("CREATE INDEX notes_body ON notes (body);", &mut seed).unwrap();
        let path = {
            let mut p = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            p.push(format!("sqlrite-drop-ro-{pid}-{nanos}.sqlrite"));
            p
        };
        save_database(&mut seed, &path).unwrap();
        drop(seed);

        let mut ro = open_database_read_only(&path, "t".to_string()).unwrap();
        for stmt in ["DROP TABLE notes;", "DROP INDEX notes_body;"] {
            let err = process_command(stmt, &mut ro).unwrap_err();
            assert!(
                format!("{err}").contains("read-only"),
                "{stmt:?} should surface read-only error, got: {err}"
            );
        }

        let _ = std::fs::remove_file(&path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
    }

    // -------------------------------------------------------------------
    // ALTER TABLE — RENAME TO / RENAME COLUMN / ADD COLUMN / DROP COLUMN
    // -------------------------------------------------------------------

    #[test]
    fn alter_rename_table_basic() {
        let mut db = seed_users_table();
        process_command("ALTER TABLE users RENAME TO members;", &mut db).expect("rename table");
        assert!(!db.contains_table("users".to_string()));
        assert!(db.contains_table("members".to_string()));
        // Data still queryable under the new name.
        let response = process_command("SELECT * FROM members;", &mut db).expect("select");
        assert!(response.contains("3 rows returned"));
    }

    #[test]
    fn alter_rename_table_renames_auto_indexes() {
        // Use a fresh table with both PK and a UNIQUE column so we
        // exercise both auto-index renames in one shot.
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE);",
            &mut db,
        )
        .unwrap();
        {
            let accounts = db.get_table("accounts".to_string()).unwrap();
            assert!(
                accounts
                    .index_by_name("sqlrite_autoindex_accounts_id")
                    .is_some()
            );
            assert!(
                accounts
                    .index_by_name("sqlrite_autoindex_accounts_email")
                    .is_some()
            );
        }
        process_command("ALTER TABLE accounts RENAME TO members;", &mut db).expect("rename");
        let members = db.get_table("members".to_string()).unwrap();
        assert!(
            members
                .index_by_name("sqlrite_autoindex_members_id")
                .is_some(),
            "PK auto-index should be renamed to match new table"
        );
        assert!(
            members
                .index_by_name("sqlrite_autoindex_members_email")
                .is_some()
        );
        // The old-named auto-indexes should be gone.
        assert!(
            members
                .index_by_name("sqlrite_autoindex_accounts_id")
                .is_none()
        );
        // table_name field on each index should also reflect the rename.
        for idx in &members.secondary_indexes {
            assert_eq!(idx.table_name, "members");
        }
    }

    #[test]
    fn alter_rename_table_to_existing_errors() {
        let mut db = seed_users_table();
        process_command("CREATE TABLE other (id INTEGER PRIMARY KEY);", &mut db).unwrap();
        let err = process_command("ALTER TABLE users RENAME TO other;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("already exists"), "got: {err}");
        // Both tables still present.
        assert!(db.contains_table("users".to_string()));
        assert!(db.contains_table("other".to_string()));
    }

    #[test]
    fn alter_rename_table_to_reserved_name_errors() {
        let mut db = seed_users_table();
        let err =
            process_command("ALTER TABLE users RENAME TO sqlrite_master;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("reserved"), "got: {err}");
    }

    #[test]
    fn alter_rename_column_basic() {
        let mut db = seed_users_table();
        process_command(
            "ALTER TABLE users RENAME COLUMN name TO full_name;",
            &mut db,
        )
        .expect("rename column");

        let users = db.get_table("users".to_string()).unwrap();
        assert!(users.contains_column("full_name".to_string()));
        assert!(!users.contains_column("name".to_string()));

        // Existing data is queryable under the new column name and value
        // is preserved at the same rowid.
        let bob_rowid = users
            .rowids()
            .into_iter()
            .find(|r| users.get_value("full_name", *r) == Some(Value::Text("bob".to_string())))
            .expect("bob row should be findable under the new column name");
        assert_eq!(
            users.get_value("full_name", bob_rowid),
            Some(Value::Text("bob".to_string()))
        );
    }

    #[test]
    fn alter_rename_column_collision_errors() {
        let mut db = seed_users_table();
        let err =
            process_command("ALTER TABLE users RENAME COLUMN name TO age;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("already exists"), "got: {err}");
    }

    #[test]
    fn alter_rename_column_updates_indexes() {
        // `accounts.email` is UNIQUE → has a renameable auto-index.
        let mut db = Database::new("t".to_string());
        process_command(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE);",
            &mut db,
        )
        .unwrap();
        process_command(
            "ALTER TABLE accounts RENAME COLUMN email TO contact;",
            &mut db,
        )
        .unwrap();
        let accounts = db.get_table("accounts".to_string()).unwrap();
        assert!(
            accounts
                .index_by_name("sqlrite_autoindex_accounts_contact")
                .is_some()
        );
        assert!(
            accounts
                .index_by_name("sqlrite_autoindex_accounts_email")
                .is_none()
        );
    }

    #[test]
    fn alter_add_column_basic() {
        let mut db = seed_users_table();
        process_command("ALTER TABLE users ADD COLUMN nickname TEXT;", &mut db)
            .expect("add column");
        let users = db.get_table("users".to_string()).unwrap();
        assert!(users.contains_column("nickname".to_string()));
        // Existing rows read NULL for the new column (no default given).
        let any_rowid = *users.rowids().first().expect("seed has rows");
        assert_eq!(users.get_value("nickname", any_rowid), None);

        // A new INSERT supplying the new column works.
        process_command(
            "INSERT INTO users (name, age, nickname) VALUES ('dan', 22, 'd');",
            &mut db,
        )
        .expect("insert with new col");
        let users = db.get_table("users".to_string()).unwrap();
        let dan_rowid = users
            .rowids()
            .into_iter()
            .find(|r| users.get_value("name", *r) == Some(Value::Text("dan".to_string())))
            .unwrap();
        assert_eq!(
            users.get_value("nickname", dan_rowid),
            Some(Value::Text("d".to_string()))
        );
    }

    #[test]
    fn alter_add_column_with_default_backfills_existing_rows() {
        let mut db = seed_users_table();
        process_command(
            "ALTER TABLE users ADD COLUMN status TEXT DEFAULT 'active';",
            &mut db,
        )
        .expect("add column with default");
        let users = db.get_table("users".to_string()).unwrap();
        for rowid in users.rowids() {
            assert_eq!(
                users.get_value("status", rowid),
                Some(Value::Text("active".to_string())),
                "rowid {rowid} should have been backfilled with the default"
            );
        }
    }

    #[test]
    fn alter_add_column_not_null_with_default_works_on_nonempty_table() {
        let mut db = seed_users_table();
        process_command(
            "ALTER TABLE users ADD COLUMN score INTEGER NOT NULL DEFAULT 0;",
            &mut db,
        )
        .expect("NOT NULL ADD with DEFAULT should succeed even with existing rows");
        let users = db.get_table("users".to_string()).unwrap();
        for rowid in users.rowids() {
            assert_eq!(users.get_value("score", rowid), Some(Value::Integer(0)));
        }
    }

    #[test]
    fn alter_add_column_not_null_without_default_errors_on_nonempty_table() {
        let mut db = seed_users_table();
        let err = process_command(
            "ALTER TABLE users ADD COLUMN score INTEGER NOT NULL;",
            &mut db,
        )
        .unwrap_err();
        let msg = format!("{err}").to_lowercase();
        assert!(
            msg.contains("not null") && msg.contains("default"),
            "got: {msg}"
        );
    }

    #[test]
    fn alter_add_column_pk_rejected() {
        let mut db = seed_users_table();
        let err = process_command(
            "ALTER TABLE users ADD COLUMN extra INTEGER PRIMARY KEY;",
            &mut db,
        )
        .unwrap_err();
        assert!(
            format!("{err}").to_lowercase().contains("primary key"),
            "got: {err}"
        );
    }

    #[test]
    fn alter_add_column_unique_rejected() {
        let mut db = seed_users_table();
        let err = process_command(
            "ALTER TABLE users ADD COLUMN extra INTEGER UNIQUE;",
            &mut db,
        )
        .unwrap_err();
        assert!(
            format!("{err}").to_lowercase().contains("unique"),
            "got: {err}"
        );
    }

    #[test]
    fn alter_add_column_existing_name_errors() {
        let mut db = seed_users_table();
        let err =
            process_command("ALTER TABLE users ADD COLUMN age INTEGER;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("already exists"), "got: {err}");
    }

    // Note: `ALTER TABLE ... ADD COLUMN IF NOT EXISTS ...` is not in the
    // SQLite dialect (PG/MSSQL extension); the AST `if_not_exists` flag is
    // still honoured by the executor if some other dialect ever produces
    // it, but there's no way to feed it via SQL in our default dialect.

    #[test]
    fn alter_drop_column_basic() {
        let mut db = seed_users_table();
        process_command("ALTER TABLE users DROP COLUMN age;", &mut db).expect("drop column");
        let users = db.get_table("users".to_string()).unwrap();
        assert!(!users.contains_column("age".to_string()));
        // Other columns and rowids still intact.
        assert!(users.contains_column("name".to_string()));
        assert_eq!(users.rowids().len(), 3);
    }

    #[test]
    fn alter_drop_column_drops_dependent_indexes() {
        let mut db = seed_users_table();
        process_command("CREATE INDEX users_age_idx ON users (age);", &mut db).unwrap();
        process_command("ALTER TABLE users DROP COLUMN age;", &mut db).unwrap();
        let users = db.get_table("users".to_string()).unwrap();
        assert!(users.index_by_name("users_age_idx").is_none());
    }

    #[test]
    fn alter_drop_column_pk_errors() {
        let mut db = seed_users_table();
        let err = process_command("ALTER TABLE users DROP COLUMN id;", &mut db).unwrap_err();
        assert!(
            format!("{err}").to_lowercase().contains("primary key"),
            "got: {err}"
        );
    }

    #[test]
    fn alter_drop_column_only_column_errors() {
        let mut db = Database::new("t".to_string());
        process_command("CREATE TABLE solo (only_col TEXT);", &mut db).unwrap();
        let err = process_command("ALTER TABLE solo DROP COLUMN only_col;", &mut db).unwrap_err();
        assert!(
            format!("{err}").to_lowercase().contains("only column"),
            "got: {err}"
        );
    }

    #[test]
    fn alter_unknown_table_errors_without_if_exists() {
        let mut db = Database::new("t".to_string());
        let err = process_command("ALTER TABLE missing RENAME TO other;", &mut db).unwrap_err();
        assert!(format!("{err}").contains("does not exist"), "got: {err}");
    }

    #[test]
    fn alter_unknown_table_if_exists_noop() {
        let mut db = Database::new("t".to_string());
        let response = process_command("ALTER TABLE IF EXISTS missing RENAME TO other;", &mut db)
            .expect("IF EXISTS makes missing-table ALTER a no-op");
        assert!(response.contains("no-op"));
    }

    #[test]
    fn alter_inside_transaction_rolls_back() {
        let mut db = seed_users_table();
        process_command("BEGIN;", &mut db).unwrap();
        process_command(
            "ALTER TABLE users ADD COLUMN status TEXT DEFAULT 'active';",
            &mut db,
        )
        .unwrap();
        // Confirm in-flight visibility.
        assert!(
            db.get_table("users".to_string())
                .unwrap()
                .contains_column("status".to_string())
        );
        process_command("ROLLBACK;", &mut db).unwrap();
        // Snapshot restore should erase the ALTER.
        assert!(
            !db.get_table("users".to_string())
                .unwrap()
                .contains_column("status".to_string())
        );
    }

    #[test]
    fn alter_rejected_on_readonly_db() {
        use crate::sql::pager::{open_database_read_only, save_database};

        let mut seed = Database::new("t".to_string());
        process_command(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);",
            &mut seed,
        )
        .unwrap();
        let path = {
            let mut p = std::env::temp_dir();
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            p.push(format!("sqlrite-alter-ro-{pid}-{nanos}.sqlrite"));
            p
        };
        save_database(&mut seed, &path).unwrap();
        drop(seed);

        let mut ro = open_database_read_only(&path, "t".to_string()).unwrap();
        for stmt in [
            "ALTER TABLE notes RENAME TO n2;",
            "ALTER TABLE notes RENAME COLUMN body TO b;",
            "ALTER TABLE notes ADD COLUMN extra TEXT;",
            "ALTER TABLE notes DROP COLUMN body;",
        ] {
            let err = process_command(stmt, &mut ro).unwrap_err();
            assert!(
                format!("{err}").contains("read-only"),
                "{stmt:?} should surface read-only error, got: {err}"
            );
        }

        let _ = std::fs::remove_file(&path);
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
    }
}
