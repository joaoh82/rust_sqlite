use sqlparser::ast::{Expr, Insert, SetExpr, Statement, Value as AstValue, Values};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::{Value, parse_vector_literal};

/// Parsed INSERT statement: target table, declared column list, and one
/// or more rows of typed values.
///
/// `rows` is `Vec<Vec<Option<Value>>>` rather than `Vec<Vec<String>>` so
/// SQL `NULL` can be represented faithfully as `None` instead of leaking
/// out as the string sentinel `"Null"` (which used to break INTEGER /
/// REAL / BOOL inserts and silently round-trip as the literal text
/// `"Null"` in TEXT columns).
#[derive(Debug)]
pub struct InsertQuery {
    pub table_name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<Value>>>,
}

impl InsertQuery {
    pub fn new(statement: &Statement) -> Result<InsertQuery> {
        let tname: Option<String>;
        let mut columns: Vec<String> = vec![];
        let mut all_values: Vec<Vec<Option<Value>>> = vec![];

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
                        let mut value_set: Vec<Option<Value>> = vec![];
                        for e in row {
                            match e {
                                Expr::Value(v) => match &v.value {
                                    AstValue::Number(n, _) => {
                                        // Try integer first; if the literal has
                                        // a decimal point or exponent, i64 fails
                                        // and we fall through to f64.
                                        if let Ok(i) = n.parse::<i64>() {
                                            value_set.push(Some(Value::Integer(i)));
                                        } else if let Ok(f) = n.parse::<f64>() {
                                            value_set.push(Some(Value::Real(f)));
                                        } else {
                                            return Err(SQLRiteError::General(format!(
                                                "Could not parse numeric literal '{n}'"
                                            )));
                                        }
                                    }
                                    AstValue::Boolean(b) => {
                                        value_set.push(Some(Value::Bool(*b)));
                                    }
                                    AstValue::SingleQuotedString(sqs) => {
                                        value_set.push(Some(Value::Text(sqs.to_string())));
                                    }
                                    AstValue::Null => {
                                        value_set.push(None);
                                    }
                                    _ => {}
                                },
                                Expr::Identifier(i) => {
                                    // Phase 7a — sqlparser parses bracket-array
                                    // literals like `[0.1, 0.2, 0.3]` as
                                    // bracket-quoted identifiers (it inherits
                                    // MSSQL-style `[name]` quoting). Detect
                                    // that by `quote_style == Some('[')` and
                                    // parse it eagerly into a typed
                                    // `Value::Vector` so the rest of the
                                    // pipeline sees a real vector. Dimension
                                    // checking against the column declaration
                                    // happens at insert_row time.
                                    if i.quote_style == Some('[') {
                                        let raw = format!("[{}]", i.value);
                                        let parsed = parse_vector_literal(&raw).map_err(|e| {
                                            SQLRiteError::General(format!(
                                                "Could not parse vector literal '{raw}': {e}"
                                            ))
                                        })?;
                                        value_set.push(Some(Value::Vector(parsed)));
                                    } else {
                                        value_set.push(Some(Value::Text(i.to_string())));
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
