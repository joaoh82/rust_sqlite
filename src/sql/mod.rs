pub mod db;
pub mod executor;
pub mod parser;
// pub mod tokenizer;

use parser::create::CreateQuery;
use parser::insert::InsertQuery;
use parser::select::SelectQuery;

use sqlparser::ast::Statement;
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

/// Performs initial parsing of SQL Statement using sqlparser-rs
pub fn process_command(query: &str, db: &mut Database) -> Result<String> {
    let dialect = SQLiteDialect {};
    let message: String;
    let mut ast = Parser::parse_sql(&dialect, &query).map_err(SQLRiteError::from)?;

    if ast.len() > 1 {
        return Err(SQLRiteError::SqlError(ParserError::ParserError(format!(
            "Expected a single query statement, but there are {}",
            ast.len()
        ))));
    }

    let query = ast.pop().unwrap();

    // Initialy only implementing some basic SQL Statements
    match query {
        Statement::CreateTable(_) => {
            let create_query = CreateQuery::new(&query);
            match create_query {
                Ok(payload) => {
                    let table_name = payload.table_name.clone();
                    // Checking if table already exists, after parsing CREATE TABLE query
                    match db.contains_table(table_name.to_string()) {
                        true => {
                            return Err(SQLRiteError::Internal(
                                "Cannot create, table already exists.".to_string(),
                            ));
                        }
                        false => {
                            let table = Table::new(payload);
                            let _ = table.print_table_schema();
                            db.tables.insert(table_name.to_string(), table);
                            // Iterate over everything.
                            // for (table_name, _) in &db.tables {
                            //     println!("{}" , table_name);
                            // }
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
                            db_table.print_table_data();
                        }
                        false => {
                            return Err(SQLRiteError::Internal("Table doesn't exist".to_string()))
                        }
                    }
                }
                Err(err) => return Err(err),
            }

            message = String::from("INSERT Statement executed.")
        }
        Statement::Query(_) => {
            let select_query = SelectQuery::new(&query)?;
            let (rendered, rows) = executor::execute_select(select_query, db)?;
            // Print the result table above the status message so the REPL shows both.
            print!("{rendered}");
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
        _ => {
            return Err(SQLRiteError::NotImplemented(
                "SQL Statement not supported yet.".to_string(),
            ));
        }
    };

    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `users(id INTEGER PK, name TEXT, age INTEGER)` table populated
    /// with three rows, for use in executor-level tests.
    fn seed_users_table() -> Database {
        let mut db = Database::new("tempdb".to_string());
        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);",
            &mut db,
        )
        .expect("create table");
        process_command("INSERT INTO users (name, age) VALUES ('alice', 30);", &mut db)
            .expect("insert alice");
        process_command("INSERT INTO users (name, age) VALUES ('bob', 25);", &mut db)
            .expect("insert bob");
        process_command("INSERT INTO users (name, age) VALUES ('carol', 40);", &mut db)
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
        let response = process_command("SELECT name FROM users WHERE age > 25;", &mut db)
            .expect("select");
        assert!(response.contains("2 rows returned"));
    }

    #[test]
    fn process_command_select_eq_string_test() {
        let mut db = seed_users_table();
        let response = process_command("SELECT name FROM users WHERE name = 'bob';", &mut db)
            .expect("select");
        assert!(response.contains("1 row returned"));
    }

    #[test]
    fn process_command_select_limit_test() {
        let mut db = seed_users_table();
        let response =
            process_command("SELECT * FROM users ORDER BY age ASC LIMIT 2;", &mut db)
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
        let mut ast = Parser::parse_sql(&dialect, &query_statement).unwrap();
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
        let _ = match process_command(&insert_query, &mut db) {
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
        let mut ast = Parser::parse_sql(&dialect, &query_statement).unwrap();
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
        let _ = match process_command(&insert_query, &mut db) {
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
        let response = process_command("DELETE FROM users WHERE name = 'bob';", &mut db)
            .expect("delete");
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
    fn process_command_unsupported_statement_test() {
        let mut db = Database::new("tempdb".to_string());
        // Nothing in Phase 1 handles DROP.
        let result = process_command("DROP TABLE users;", &mut db);
        assert!(result.is_err());
    }
}
