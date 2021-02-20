mod parser;
pub mod tokenizer;

use sqlparser::ast::{Statement};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::{Parser, ParserError};

use crate::error::{SQLRiteError, Result};

/// Performs initial parsing of SQL Statement using sqlparser-rs
pub fn prepare_statement(query: &str) -> Result<String> {
    let dialect = SQLiteDialect{};
    let message: String;
    let mut ast = Parser::parse_sql(&dialect, &query).map_err(SQLRiteError::from)?;

    if ast.len() > 1 {
        return Err(SQLRiteError::SqlError(ParserError::ParserError(format!(
            "Expected a single query statement, but there are {}",
            ast.len()
        ))));
    }

    // Initialy only implementing some basic SQL Statements
    let _query = match ast.pop().unwrap() {
        Statement::Query(_query) => message = String::from("SELECT Statement executed."),
        Statement::Insert {..} => message = String::from("INSERT Statement executed."),
        Statement::Delete{..} => message = String::from("DELETE Statement executed."),
        _ => {
            return Err(SQLRiteError::NotImplemented(
                "SQL Statement not supported yet.".to_string()))
        }
    };

    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_statement_select_test() {
        let inputed_query = String::from("SELECT * from users;"); 

        let _ = match prepare_statement(&inputed_query) {
            Ok(response) => assert_eq!(response, "SELECT Statement executed."),
            Err(_) => assert!(false),
        };
    }

    #[test]
    fn prepare_statement_insert_test() {
        let inputed_query = String::from("INSERT INTO users (name) Values ('josh');"); 

        let _ = match prepare_statement(&inputed_query) {
            Ok(response) => assert_eq!(response, "INSERT Statement executed."),
            Err(_) => assert!(false),
        };
    }

    #[test]
    fn prepare_statement_delete_test() {
        let inputed_query = String::from("DELETE FROM users WHERE id=1;"); 

        let _ = match prepare_statement(&inputed_query) {
            Ok(response) => assert_eq!(response, "DELETE Statement executed."),
            Err(_) => assert!(false),
        };
    }

    #[test]
    fn prepare_statement_not_implemented_test() {
        let inputed_query = String::from("UPDATE users SET name='josh' where id=1;"); 
        let expected = Err(SQLRiteError::NotImplemented("SQL Statement not supported yet.".to_string()));

        let result = prepare_statement(&inputed_query).map_err(|e| e);
        assert_eq!(result, expected);
    }

}