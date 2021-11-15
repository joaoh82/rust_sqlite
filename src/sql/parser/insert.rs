use sqlparser::ast::{Expr, Query, SetExpr, Statement, Value, Values};

use crate::error::{Result, SQLRiteError};

/// The following structure represents a INSERT query already parsed
/// and broken down into `table_name` a `Vec<String>` representing the `Columns`
/// and `Vec<Vec<String>>` representing the list of `Rows` to be inserted
#[derive(Debug)]
pub struct InsertQuery {
    pub table_name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl InsertQuery {
    pub fn new(statement: &Statement) -> Result<InsertQuery> {
        #[allow(unused_assignments)]
        let mut tname: Option<String> = None;
        let mut columns: Vec<String> = vec![];
        let mut all_values: Vec<Vec<String>> = vec![];

        match statement {
            Statement::Insert {
                table_name,
                columns: cols,
                source,
                ..
            } => {
                tname = Some(table_name.to_string());
                for col in cols {
                    columns.push(col.to_string());
                }

                match &**source {
                    Query {
                        body,
                        order_by: _order_by,
                        limit: _limit,
                        offset: _offset,
                        fetch: _fetch,
                        ..
                    } => {
                        if let SetExpr::Values(values) = body {
                            #[allow(irrefutable_let_patterns)]
                            if let Values(expressions) = values {
                                for i in expressions {
                                    let mut value_set: Vec<String> = vec![];
                                    for e in i {
                                        match e {
                                            Expr::Value(v) => match v {
                                                Value::Number(n,_) => {
                                                    value_set.push(n.to_string());
                                                }
                                                Value::Boolean(b) => match *b {
                                                    true => value_set.push("true".to_string()),
                                                    false => value_set.push("false".to_string()),
                                                },
                                                Value::SingleQuotedString(sqs) => {
                                                    value_set.push(sqs.to_string());
                                                }
                                                Value::Null => {
                                                    value_set.push("Null".to_string());
                                                }
                                                _ => {}
                                            },
                                            Expr::Identifier(i) => {
                                                value_set.push(i.to_string());
                                            }
                                            _ => {}
                                        }
                                    }
                                    all_values.push(value_set);
                                }
                            }
                        }
                    }
                }
            }
            _ => return Err(SQLRiteError::Internal("Error parsing insert query".to_string())),
        }

        match tname {
            Some(t) => Ok(InsertQuery {
                table_name: t,
                columns,
                rows: all_values,
            }),
            None => Err(SQLRiteError::Internal("Error parsing insert query".to_string())),
        }
    }
}