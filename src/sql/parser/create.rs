use sqlparser::ast::{ColumnOption, DataType, Statement};

use crate::error::{Result, SQLRiteError};

/// The schema for each SQL column in every table is represented by
/// the following structure after parsed and tokenized
#[derive(PartialEq, Debug)]
pub struct ParsedColumn {
    /// Name of the column
    pub name: String,
    /// Datatype of the column in String format
    pub datatype: String,
    /// Value representing if column is PRIMARY KEY
    pub is_pk: bool,
    /// Value representing if column was declared with the NOT NULL Constraint
    pub not_null: bool,
    /// Value representing if column was declared with the UNIQUE Constraint
    pub is_unique: bool,
}

/// The following structure represents a CREATE TABLE query already parsed
/// and broken down into name and a Vector of `ParsedColumn` metadata
#[derive(Debug)]
pub struct CreateQuery {
    /// name of table after parking and tokenizing of query
    pub table_name: String,      
    /// Vector of `ParsedColumn` type with column metadata information
    pub columns: Vec<ParsedColumn>,
}

impl CreateQuery {
    pub fn new(statement: &Statement) -> Result<CreateQuery> {
        match statement {
            // Confirming the Statement is sqlparser::ast:Statement::CreateTable
            Statement::CreateTable {
                name,
                columns,
                constraints: _constraints,
                with_options: _with_options,
                external: _external,
                file_format: _file_format,
                location: _location,
                ..
            } => {
                let table_name = name;
                let mut parsed_columns: Vec<ParsedColumn> = vec![];

                // Iterating over the columns returned form the Parser::parse:sql
                // in the mod sql
                for col in columns {
                    let name = col.name.to_string();

                    // Checks if columm already added to parsed_columns, if so, returns an error
                    if parsed_columns.iter().any(|col| col.name == name){
                        return Err(SQLRiteError::Internal(format!("Duplicate column name: {}", &name)))
                    }
                    
                    // Parsing each column for it data type
                    // For now only accepting basic data types
                    let datatype = match &col.data_type {
                        DataType::SmallInt => "Integer",
                        DataType::Int => "Integer",
                        DataType::BigInt => "Integer",
                        DataType::Boolean => "Bool",
                        DataType::Text => "Text",
                        DataType::Varchar(_bytes) => "Text",
                        DataType::Real => "Real",
                        DataType::Float(_precision) => "Real",
                        DataType::Double => "Real",
                        DataType::Decimal(_precision1, _precision2) => "Real",
                        _ => {
                            eprintln!("not matched on custom type");
                            "Invalid"
                        }
                    };

                    // checking if column is PRIMARY KEY
                    let mut is_pk: bool = false;
                    // chekcing if column is UNIQUE
                    let mut is_unique: bool = false;
                    // chekcing if column is NULLABLE
                    let mut not_null: bool = false;
                    for column_option in &col.options {
                        match column_option.option {
                            ColumnOption::Unique { is_primary } => {
                                // For now, only Integer and Text types can be PRIMERY KEY and Unique
                                // Therefore Indexed.
                                if datatype != "Real" && datatype != "Bool" {
                                    is_pk = is_primary;
                                    if is_primary {
                                        // Checks if table being created already has a PRIMARY KEY, if so, returns an error
                                        if parsed_columns.iter().any(|col| col.is_pk == true){
                                            return Err(SQLRiteError::Internal(format!("Table '{}' has more than one primary key", &table_name)))
                                        }
                                        not_null = true;
                                    }
                                    is_unique = true;
                                }
                            },
                            ColumnOption::NotNull => {
                                not_null = true;
                            }
                            _ => (),
                        };
                    }

                    parsed_columns.push(ParsedColumn {
                        name,
                        datatype: datatype.to_string(),
                        is_pk,
                        not_null,
                        is_unique,
                    });
                    
                }
                // TODO: Handle constraints,
                // Default value and others.
                for constraint in _constraints {
                    println!("{:?}", constraint);
                }
                return Ok(CreateQuery {
                    table_name: table_name.to_string(),
                    columns: parsed_columns,
                });
            }

            _ => return Err(SQLRiteError::Internal("Error parsing query".to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::*;

    #[test]
    fn create_table_validate_tablename_test() {
        let sql_input = String::from("CREATE TABLE contacts (
            id INTEGER PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULl,
            email TEXT NOT NULL UNIQUE
        );");
        let expected_table_name = String::from("contacts");

        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, &sql_input).unwrap();

        assert!(ast.len() == 1, "ast has more then one Statement");

        let query = ast.pop().unwrap();

        // Initialy only implementing some basic SQL Statements
        match query {
            Statement::CreateTable { .. } => {
                let result = CreateQuery::new(&query);
                match result {
                    Ok(payload) => {
                        assert_eq!(payload.table_name, expected_table_name);
                    }
                    Err(_) => assert!(false, "an error occured during parsing CREATE TABLE Statement"),
                }
            },
            _ => ()
        };          
    }
}
