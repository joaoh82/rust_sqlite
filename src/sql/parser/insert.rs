use sqlparser::ast::{Expr, Insert, SetExpr, Statement, Value, Values};

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
        let tname: Option<String>;
        let mut columns: Vec<String> = vec![];
        let mut all_values: Vec<Vec<String>> = vec![];

        match statement {
            Statement::Insert(Insert {
                table,
                columns: cols,
                source,
                ..
            }) => {
                tname = Some(table.to_string());
                for col in cols {
                    columns.push(col.to_string());
                }

                let source = source.as_ref().ok_or_else(|| {
                    SQLRiteError::Internal(
                        "INSERT statement is missing a source expression".to_string(),
                    )
                })?;

                if let SetExpr::Values(Values { rows, .. }) = source.body.as_ref() {
                    for row in rows {
                        let mut value_set: Vec<String> = vec![];
                        for e in row {
                            match e {
                                Expr::Value(v) => match &v.value {
                                    Value::Number(n, _) => {
                                        value_set.push(n.to_string());
                                    }
                                    Value::Boolean(b) => {
                                        if *b {
                                            value_set.push("true".to_string());
                                        } else {
                                            value_set.push("false".to_string());
                                        }
                                    }
                                    Value::SingleQuotedString(sqs) => {
                                        value_set.push(sqs.to_string());
                                    }
                                    Value::Null => {
                                        value_set.push("Null".to_string());
                                    }
                                    _ => {}
                                },
                                Expr::Identifier(i) => {
                                    // Phase 7a — sqlparser parses bracket-array
                                    // literals like `[0.1, 0.2, 0.3]` as
                                    // bracket-quoted identifiers (it inherits
                                    // MSSQL-style `[name]` quoting). Detect
                                    // that by `quote_style == Some('[')` and
                                    // re-wrap with brackets so the
                                    // `parse_vector_literal` helper at
                                    // insert_row time can recognize and parse
                                    // it. Regular unquoted identifiers (column
                                    // refs, which don't make sense in INSERT
                                    // VALUES anyway) keep the existing
                                    // pass-through-as-string behavior.
                                    if i.quote_style == Some('[') {
                                        value_set.push(format!("[{}]", i.value));
                                    } else {
                                        value_set.push(i.to_string());
                                    }
                                }
                                _ => {}
                            }
                        }
                        all_values.push(value_set);
                    }
                }
            }
            _ => {
                return Err(SQLRiteError::Internal(
                    "Error parsing insert query".to_string(),
                ));
            }
        }

        match tname {
            Some(t) => Ok(InsertQuery {
                table_name: t,
                columns,
                rows: all_values,
            }),
            None => Err(SQLRiteError::Internal(
                "Error parsing insert query".to_string(),
            )),
        }
    }
}
