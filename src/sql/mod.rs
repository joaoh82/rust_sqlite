pub mod parser;
// pub mod tokenizer;
pub mod db;

use parser::create::CreateQuery;

use sqlparser::ast::Statement;
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::{Parser, ParserError};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::Table;
use crate::sql::db::database::Database;

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
        Statement::CreateTable { .. } => {
            let create_query = CreateQuery::new(&query);
            match create_query {
                Ok(payload) => {
                    let table_name = &payload.table_name;
                    db.tables.insert(table_name.to_string(), Table::new(payload));
                    // Iterate over everything.
                    for (table_name, _) in &db.tables {
                        println!("{}" , table_name);
                    }
                    message = String::from("CREATE TABLE Statement executed.");
                    // TODO: Push table to DB
                }
                Err(err) => return Err(err),
            }
        }
        Statement::Insert { .. } => { 
            // println!("{:?}", &query);
            message = String::from("INSERT Statement executed.") 
        }
        Statement::Query(_query) => message = String::from("SELECT Statement executed."),
        // Statement::Insert { .. } => message = String::from("INSERT Statement executed."),
        Statement::Delete { .. } => message = String::from("DELETE Statement executed."),
        _ => {
            return Err(SQLRiteError::NotImplemented(
                "SQL Statement not supported yet.".to_string(),
            ))
        }
    };

    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_command_select_test() {
        let inputed_query = String::from("SELECT * from users;");
        let mut db = Database::new("tempdb".to_string());

        let _ = match process_command(&inputed_query, &mut db) {
            Ok(response) => assert_eq!(response, "SELECT Statement executed."),
            Err(_) => assert!(false),
        };
    }

    #[test]
    fn process_command_insert_test() {
        let inputed_query = String::from("INSERT INTO users (name) Values ('josh');");
        let mut db = Database::new("tempdb".to_string());

        let _ = match process_command(&inputed_query, &mut db) {
            Ok(response) => assert_eq!(response, "INSERT Statement executed."),
            Err(_) => assert!(false),
        };
    }

    #[test]
    fn process_command_delete_test() {
        let inputed_query = String::from("DELETE FROM users WHERE id=1;");
        let mut db = Database::new("tempdb".to_string());

        let _ = match process_command(&inputed_query, &mut db) {
            Ok(response) => assert_eq!(response, "DELETE Statement executed."),
            Err(_) => assert!(false),
        };
    }

    #[test]
    fn process_command_not_implemented_test() {
        let inputed_query = String::from("UPDATE users SET name='josh' where id=1;");
        let mut db = Database::new("tempdb".to_string());
        let expected = Err(SQLRiteError::NotImplemented(
            "SQL Statement not supported yet.".to_string(),
        ));

        let result = process_command(&inputed_query, &mut db).map_err(|e| e);
        assert_eq!(result, expected);
    }
}
