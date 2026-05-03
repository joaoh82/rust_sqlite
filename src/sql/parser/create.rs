use sqlparser::ast::{ColumnOption, CreateTable, DataType, ObjectName, ObjectNamePart, Statement};

use crate::error::{Result, SQLRiteError};

/// True when an `ObjectName` resolves to a single identifier `VECTOR`
/// (case-insensitive). Phase 7a adds the `VECTOR(N)` column type as a
/// sqlparser `DataType::Custom` — the engine recognizes it via this
/// helper so the regular DataType match arm above stays uncluttered.
fn is_vector_type(name: &ObjectName) -> bool {
    name.0.len() == 1
        && match &name.0[0] {
            ObjectNamePart::Identifier(ident) => ident.value.eq_ignore_ascii_case("VECTOR"),
            // Function-form ObjectNamePart shouldn't appear in a CREATE TABLE
            // column type position. If it ever does, treat it as not-a-vector
            // and the outer match falls through to the "Invalid" arm.
            _ => false,
        }
}

/// Parses the dimension out of the `Custom` args for `VECTOR(N)`.
/// `args` is the `Vec<String>` sqlparser hands back for parenthesized
/// type arguments — for `VECTOR(384)` that's `["384"]`. Validates that
/// exactly one positive-integer argument was supplied.
fn parse_vector_dim(args: &[String]) -> std::result::Result<usize, String> {
    match args {
        [] => Err("VECTOR requires a dimension, e.g. `VECTOR(384)`".to_string()),
        [single] => {
            let trimmed = single.trim();
            match trimmed.parse::<usize>() {
                Ok(d) if d > 0 => Ok(d),
                Ok(_) => Err(format!("VECTOR dimension must be ≥ 1 (got `{trimmed}`)")),
                Err(_) => Err(format!(
                    "VECTOR dimension must be a positive integer (got `{trimmed}`)"
                )),
            }
        }
        many => Err(format!(
            "VECTOR takes exactly one dimension argument (got {})",
            many.len()
        )),
    }
}

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
///
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
            Statement::CreateTable(CreateTable {
                name,
                columns,
                constraints,
                ..
            }) => {
                let table_name = name;
                let mut parsed_columns: Vec<ParsedColumn> = vec![];

                // Iterating over the columns returned form the Parser::parse:sql
                // in the mod sql
                for col in columns {
                    let name = col.name.to_string();

                    // Checks if columm already added to parsed_columns, if so, returns an error
                    if parsed_columns.iter().any(|col| col.name == name) {
                        return Err(SQLRiteError::Internal(format!(
                            "Duplicate column name: {}",
                            &name
                        )));
                    }

                    // Parsing each column for it data type
                    // For now only accepting basic data types
                    let datatype: String = match &col.data_type {
                        DataType::TinyInt(_)
                        | DataType::SmallInt(_)
                        | DataType::Int2(_)
                        | DataType::Int(_)
                        | DataType::Int4(_)
                        | DataType::Int8(_)
                        | DataType::Integer(_)
                        | DataType::BigInt(_) => "Integer".to_string(),
                        DataType::Boolean => "Bool".to_string(),
                        DataType::Text => "Text".to_string(),
                        DataType::Varchar(_bytes) => "Text".to_string(),
                        DataType::Real => "Real".to_string(),
                        DataType::Float(_precision) => "Real".to_string(),
                        DataType::Double(_) => "Real".to_string(),
                        DataType::Decimal(_) => "Real".to_string(),
                        // Phase 7e — `JSON` parses as a unit variant in
                        // sqlparser's DataType enum. JSONB is treated as
                        // an alias (matches PostgreSQL's permissive
                        // behaviour); both store as text under the hood.
                        DataType::JSON | DataType::JSONB => "Json".to_string(),
                        // Phase 7a — `VECTOR(N)` parses as Custom("VECTOR", ["N"]).
                        // sqlparser's SQLite dialect doesn't have a built-in
                        // Vector variant; Custom is what unrecognized type
                        // names + their parenthesized args fall through to.
                        DataType::Custom(name, args) if is_vector_type(name) => {
                            match parse_vector_dim(args) {
                                Ok(dim) => format!("vector({dim})"),
                                Err(e) => {
                                    return Err(SQLRiteError::General(format!(
                                        "Invalid VECTOR column '{}': {e}",
                                        col.name
                                    )));
                                }
                            }
                        }
                        other => {
                            eprintln!("not matched on custom type: {other:?}");
                            "Invalid".to_string()
                        }
                    };

                    // checking if column is PRIMARY KEY
                    let mut is_pk: bool = false;
                    // chekcing if column is UNIQUE
                    let mut is_unique: bool = false;
                    // chekcing if column is NULLABLE
                    let mut not_null: bool = false;
                    for column_option in &col.options {
                        match &column_option.option {
                            ColumnOption::PrimaryKey(_) => {
                                // For now, only Integer and Text types can be PRIMARY KEY and Unique
                                // Therefore Indexed.
                                if datatype != "Real" && datatype != "Bool" {
                                    // Checks if table being created already has a PRIMARY KEY, if so, returns an error
                                    if parsed_columns.iter().any(|col| col.is_pk) {
                                        return Err(SQLRiteError::Internal(format!(
                                            "Table '{}' has more than one primary key",
                                            &table_name
                                        )));
                                    }
                                    is_pk = true;
                                    is_unique = true;
                                    not_null = true;
                                }
                            }
                            ColumnOption::Unique(_) => {
                                // For now, only Integer and Text types can be UNIQUE
                                // Therefore Indexed.
                                if datatype != "Real" && datatype != "Bool" {
                                    is_unique = true;
                                }
                            }
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
                // TODO: handle constraints + default values + check
                // constraints + ON DELETE / ON UPDATE referential actions
                // properly. They're currently parsed by `sqlparser` and
                // dropped on the floor here. (Previously we `println!`-ed
                // them to stdout as a debug aid — removed in the
                // engine-stdout-pollution cleanup; flip to a `tracing`
                // span if we ever want them visible in dev builds.)
                let _ = constraints;
                Ok(CreateQuery {
                    table_name: table_name.to_string(),
                    columns: parsed_columns,
                })
            }

            _ => Err(SQLRiteError::Internal("Error parsing query".to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::*;

    #[test]
    fn create_table_validate_tablename_test() {
        let sql_input = String::from(
            "CREATE TABLE contacts (
            id INTEGER PRIMARY KEY,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULl,
            email TEXT NOT NULL UNIQUE
        );",
        );
        let expected_table_name = String::from("contacts");

        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, &sql_input).unwrap();

        assert!(ast.len() == 1, "ast has more then one Statement");

        let query = ast.pop().unwrap();

        // Initialy only implementing some basic SQL Statements
        if let Statement::CreateTable(_) = query {
            let result = CreateQuery::new(&query);
            match result {
                Ok(payload) => {
                    assert_eq!(payload.table_name, expected_table_name);
                }
                Err(_) => panic!("an error occured during parsing CREATE TABLE Statement"),
            }
        }
    }
}
