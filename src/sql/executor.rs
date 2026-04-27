//! Query executors — evaluate parsed SQL statements against the in-memory
//! storage and produce formatted output.

use std::cmp::Ordering;

use prettytable::{Cell as PrintCell, Row as PrintRow, Table as PrintTable};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, CreateIndex, Delete, Expr, FromTable, Statement, TableFactor,
    TableWithJoins, UnaryOperator, Update,
};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::secondary_index::{IndexOrigin, SecondaryIndex};
use crate::sql::db::table::{DataType, Table, Value};
use crate::sql::parser::select::{OrderByClause, Projection, SelectQuery};

/// Executes a parsed `SelectQuery` against the database and returns a
/// human-readable rendering of the result set (prettytable). Also returns
/// the number of rows produced, for the top-level status message.
/// Structured result of a SELECT: column names in projection order,
/// and each matching row as a `Vec<Value>` aligned with the columns.
/// Phase 5a introduced this so the public `Connection` / `Statement`
/// API has typed rows to yield; the existing `execute_select` that
/// returns pre-rendered text is now a thin wrapper on top.
pub struct SelectResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Executes a SELECT and returns structured rows. The typed rows are
/// what the new public API streams to callers; the REPL / Tauri app
/// pre-render into a prettytable via `execute_select`.
pub fn execute_select_rows(query: SelectQuery, db: &Database) -> Result<SelectResult> {
    let table = db
        .get_table(query.table_name.clone())
        .map_err(|_| SQLRiteError::Internal(format!("Table '{}' not found", query.table_name)))?;

    // Resolve projection to a concrete ordered column list.
    let projected_cols: Vec<String> = match &query.projection {
        Projection::All => table.column_names(),
        Projection::Columns(cols) => {
            for c in cols {
                if !table.contains_column(c.to_string()) {
                    return Err(SQLRiteError::Internal(format!(
                        "Column '{c}' does not exist on table '{}'",
                        query.table_name
                    )));
                }
            }
            cols.clone()
        }
    };

    // Collect matching rowids. If the WHERE is the shape `col = literal`
    // and `col` has a secondary index, probe the index for an O(log N)
    // seek; otherwise fall back to the full table scan.
    let matching = match select_rowids(table, query.selection.as_ref())? {
        RowidSource::IndexProbe(rowids) => rowids,
        RowidSource::FullScan => {
            let mut out = Vec::new();
            for rowid in table.rowids() {
                if let Some(expr) = &query.selection {
                    if !eval_predicate(expr, table, rowid)? {
                        continue;
                    }
                }
                out.push(rowid);
            }
            out
        }
    };
    let mut matching = matching;

    // Sort before applying LIMIT, matching SQL semantics.
    if let Some(order) = &query.order_by {
        sort_rowids(&mut matching, table, order)?;
    }

    if let Some(n) = query.limit {
        matching.truncate(n);
    }

    // Build typed rows. Missing cells surface as `Value::Null` — that
    // maps a column-not-present-for-this-rowid case onto the public
    // `Row::get` → `Option<T>` surface cleanly.
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(matching.len());
    for rowid in &matching {
        let row: Vec<Value> = projected_cols
            .iter()
            .map(|col| table.get_value(col, *rowid).unwrap_or(Value::Null))
            .collect();
        rows.push(row);
    }

    Ok(SelectResult {
        columns: projected_cols,
        rows,
    })
}

/// Executes a SELECT and returns `(rendered_table, row_count)`. The
/// REPL and Tauri app use this to keep the table-printing behaviour
/// the engine has always shipped. Structured callers use
/// `execute_select_rows` instead.
pub fn execute_select(query: SelectQuery, db: &Database) -> Result<(String, usize)> {
    let result = execute_select_rows(query, db)?;
    let row_count = result.rows.len();

    let mut print_table = PrintTable::new();
    let header_cells: Vec<PrintCell> = result.columns.iter().map(|c| PrintCell::new(c)).collect();
    print_table.add_row(PrintRow::new(header_cells));

    for row in &result.rows {
        let cells: Vec<PrintCell> = row
            .iter()
            .map(|v| PrintCell::new(&v.to_display_string()))
            .collect();
        print_table.add_row(PrintRow::new(cells));
    }

    Ok((print_table.to_string(), row_count))
}

/// Executes a DELETE statement. Returns the number of rows removed.
pub fn execute_delete(stmt: &Statement, db: &mut Database) -> Result<usize> {
    let Statement::Delete(Delete {
        from, selection, ..
    }) = stmt
    else {
        return Err(SQLRiteError::Internal(
            "execute_delete called on a non-DELETE statement".to_string(),
        ));
    };

    let tables = match from {
        FromTable::WithFromKeyword(t) | FromTable::WithoutKeyword(t) => t,
    };
    let table_name = extract_single_table_name(tables)?;

    // Compute matching rowids with an immutable borrow, then mutate.
    let matching: Vec<i64> = {
        let table = db
            .get_table(table_name.clone())
            .map_err(|_| SQLRiteError::Internal(format!("Table '{table_name}' not found")))?;
        match select_rowids(table, selection.as_ref())? {
            RowidSource::IndexProbe(rowids) => rowids,
            RowidSource::FullScan => {
                let mut out = Vec::new();
                for rowid in table.rowids() {
                    if let Some(expr) = selection {
                        if !eval_predicate(expr, table, rowid)? {
                            continue;
                        }
                    }
                    out.push(rowid);
                }
                out
            }
        }
    };

    let table = db.get_table_mut(table_name)?;
    for rowid in &matching {
        table.delete_row(*rowid);
    }
    Ok(matching.len())
}

/// Executes an UPDATE statement. Returns the number of rows updated.
pub fn execute_update(stmt: &Statement, db: &mut Database) -> Result<usize> {
    let Statement::Update(Update {
        table,
        assignments,
        from,
        selection,
        ..
    }) = stmt
    else {
        return Err(SQLRiteError::Internal(
            "execute_update called on a non-UPDATE statement".to_string(),
        ));
    };

    if from.is_some() {
        return Err(SQLRiteError::NotImplemented(
            "UPDATE ... FROM is not supported yet".to_string(),
        ));
    }

    let table_name = extract_table_name(table)?;

    // Resolve assignment targets to plain column names and verify they exist.
    let mut parsed_assignments: Vec<(String, Expr)> = Vec::with_capacity(assignments.len());
    {
        let tbl = db
            .get_table(table_name.clone())
            .map_err(|_| SQLRiteError::Internal(format!("Table '{table_name}' not found")))?;
        for a in assignments {
            let col = match &a.target {
                AssignmentTarget::ColumnName(name) => name
                    .0
                    .last()
                    .map(|p| p.to_string())
                    .ok_or_else(|| SQLRiteError::Internal("empty column name".to_string()))?,
                AssignmentTarget::Tuple(_) => {
                    return Err(SQLRiteError::NotImplemented(
                        "tuple assignment targets are not supported".to_string(),
                    ));
                }
            };
            if !tbl.contains_column(col.clone()) {
                return Err(SQLRiteError::Internal(format!(
                    "UPDATE references unknown column '{col}'"
                )));
            }
            parsed_assignments.push((col, a.value.clone()));
        }
    }

    // Gather matching rowids + the new values to write for each assignment, under
    // an immutable borrow. Uses the index-probe fast path when the WHERE is
    // `col = literal` on an indexed column.
    let work: Vec<(i64, Vec<(String, Value)>)> = {
        let tbl = db.get_table(table_name.clone())?;
        let matched_rowids: Vec<i64> = match select_rowids(tbl, selection.as_ref())? {
            RowidSource::IndexProbe(rowids) => rowids,
            RowidSource::FullScan => {
                let mut out = Vec::new();
                for rowid in tbl.rowids() {
                    if let Some(expr) = selection {
                        if !eval_predicate(expr, tbl, rowid)? {
                            continue;
                        }
                    }
                    out.push(rowid);
                }
                out
            }
        };
        let mut rows_to_update = Vec::new();
        for rowid in matched_rowids {
            let mut values = Vec::with_capacity(parsed_assignments.len());
            for (col, expr) in &parsed_assignments {
                // UPDATE's RHS is evaluated in the context of the row being updated,
                // so column references on the right resolve to the current row's values.
                let v = eval_expr(expr, tbl, rowid)?;
                values.push((col.clone(), v));
            }
            rows_to_update.push((rowid, values));
        }
        rows_to_update
    };

    let tbl = db.get_table_mut(table_name)?;
    for (rowid, values) in &work {
        for (col, v) in values {
            tbl.set_value(col, *rowid, v.clone())?;
        }
    }
    Ok(work.len())
}

/// Handles `CREATE INDEX [UNIQUE] <name> ON <table> (<column>)`. Single-
/// column indexes only; multi-column / composite indexes are future work.
/// Returns the (possibly synthesized) index name for the status message.
pub fn execute_create_index(stmt: &Statement, db: &mut Database) -> Result<String> {
    let Statement::CreateIndex(CreateIndex {
        name,
        table_name,
        columns,
        unique,
        if_not_exists,
        predicate,
        ..
    }) = stmt
    else {
        return Err(SQLRiteError::Internal(
            "execute_create_index called on a non-CREATE-INDEX statement".to_string(),
        ));
    };

    if predicate.is_some() {
        return Err(SQLRiteError::NotImplemented(
            "partial indexes (CREATE INDEX ... WHERE) are not supported yet".to_string(),
        ));
    }

    if columns.len() != 1 {
        return Err(SQLRiteError::NotImplemented(format!(
            "multi-column indexes are not supported yet ({} columns given)",
            columns.len()
        )));
    }

    let index_name = name.as_ref().map(|n| n.to_string()).ok_or_else(|| {
        SQLRiteError::NotImplemented(
            "anonymous CREATE INDEX (no name) is not supported — give it a name".to_string(),
        )
    })?;

    let table_name_str = table_name.to_string();
    let column_name = match &columns[0].column.expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => parts
            .last()
            .map(|p| p.value.clone())
            .ok_or_else(|| SQLRiteError::Internal("empty compound identifier".to_string()))?,
        other => {
            return Err(SQLRiteError::NotImplemented(format!(
                "CREATE INDEX only supports simple column references, got {other:?}"
            )));
        }
    };

    // Validate: table exists, column exists, type is indexable, name is unique.
    let (datatype, existing_rowids_and_values): (DataType, Vec<(i64, Value)>) = {
        let table = db.get_table(table_name_str.clone()).map_err(|_| {
            SQLRiteError::General(format!(
                "CREATE INDEX references unknown table '{table_name_str}'"
            ))
        })?;
        if !table.contains_column(column_name.clone()) {
            return Err(SQLRiteError::General(format!(
                "CREATE INDEX references unknown column '{column_name}' on table '{table_name_str}'"
            )));
        }
        let col = table
            .columns
            .iter()
            .find(|c| c.column_name == column_name)
            .expect("we just verified the column exists");
        if table.index_by_name(&index_name).is_some() {
            if *if_not_exists {
                return Ok(index_name);
            }
            return Err(SQLRiteError::General(format!(
                "index '{index_name}' already exists"
            )));
        }
        let datatype = clone_datatype(&col.datatype);

        // Snapshot (rowid, value) pairs so we can populate the index after
        // it's attached. Doing this under the immutable borrow of the table
        // means the mutable attach below can proceed without aliasing.
        let mut pairs = Vec::new();
        for rowid in table.rowids() {
            if let Some(v) = table.get_value(&column_name, rowid) {
                pairs.push((rowid, v));
            }
        }
        (datatype, pairs)
    };

    // Build the index.
    let mut idx = SecondaryIndex::new(
        index_name.clone(),
        table_name_str.clone(),
        column_name.clone(),
        &datatype,
        *unique,
        IndexOrigin::Explicit,
    )?;

    // Populate from the existing rows. UNIQUE violations here mean the
    // existing data already breaks the new index's constraint — a common
    // source of user confusion, so be explicit.
    for (rowid, v) in &existing_rowids_and_values {
        if *unique && idx.would_violate_unique(v) {
            return Err(SQLRiteError::General(format!(
                "cannot create UNIQUE index '{index_name}': column '{column_name}' \
                 already contains the duplicate value {}",
                v.to_display_string()
            )));
        }
        idx.insert(v, *rowid)?;
    }

    // Attach to the table.
    let table_mut = db.get_table_mut(table_name_str)?;
    table_mut.secondary_indexes.push(idx);
    Ok(index_name)
}

/// Cheap clone helper — `DataType` intentionally doesn't derive `Clone`
/// because the enum has no ergonomic reason to be cloneable elsewhere.
fn clone_datatype(dt: &DataType) -> DataType {
    match dt {
        DataType::Integer => DataType::Integer,
        DataType::Text => DataType::Text,
        DataType::Real => DataType::Real,
        DataType::Bool => DataType::Bool,
        DataType::Vector(dim) => DataType::Vector(*dim),
        DataType::None => DataType::None,
        DataType::Invalid => DataType::Invalid,
    }
}

fn extract_single_table_name(tables: &[TableWithJoins]) -> Result<String> {
    if tables.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "multi-table DELETE is not supported yet".to_string(),
        ));
    }
    extract_table_name(&tables[0])
}

fn extract_table_name(twj: &TableWithJoins) -> Result<String> {
    if !twj.joins.is_empty() {
        return Err(SQLRiteError::NotImplemented(
            "JOIN is not supported yet".to_string(),
        ));
    }
    match &twj.relation {
        TableFactor::Table { name, .. } => Ok(name.to_string()),
        _ => Err(SQLRiteError::NotImplemented(
            "only plain table references are supported".to_string(),
        )),
    }
}

/// Tells the executor how to produce its candidate rowid list.
enum RowidSource {
    /// The WHERE was simple enough to probe a secondary index directly.
    /// The `Vec` already contains exactly the rows the index matched;
    /// no further WHERE evaluation is needed (the probe is precise).
    IndexProbe(Vec<i64>),
    /// No applicable index; caller falls back to walking `table.rowids()`
    /// and evaluating the WHERE on each row.
    FullScan,
}

/// Try to satisfy `WHERE` with an index probe. Currently supports the
/// simplest shape: a single `col = literal` (or `literal = col`) where
/// `col` is on a secondary index. AND/OR/range predicates fall back to
/// full scan — those can be layered on later without changing the caller.
fn select_rowids(table: &Table, selection: Option<&Expr>) -> Result<RowidSource> {
    let Some(expr) = selection else {
        return Ok(RowidSource::FullScan);
    };
    let Some((col, literal)) = try_extract_equality(expr) else {
        return Ok(RowidSource::FullScan);
    };
    let Some(idx) = table.index_for_column(&col) else {
        return Ok(RowidSource::FullScan);
    };

    // Convert the literal into a runtime Value. If the literal type doesn't
    // match the column's index we still need correct semantics — evaluate
    // the WHERE against every row. Fall back to full scan.
    let literal_value = match convert_literal(&literal) {
        Ok(v) => v,
        Err(_) => return Ok(RowidSource::FullScan),
    };

    // Index lookup returns the full list of rowids matching this equality
    // predicate. For unique indexes that's at most one; for non-unique it
    // can be many.
    let mut rowids = idx.lookup(&literal_value);
    rowids.sort_unstable();
    Ok(RowidSource::IndexProbe(rowids))
}

/// Recognizes `expr` as a simple equality on a column reference against a
/// literal. Returns `(column_name, literal_value)` if the shape matches;
/// `None` otherwise. Accepts both `col = literal` and `literal = col`.
fn try_extract_equality(expr: &Expr) -> Option<(String, sqlparser::ast::Value)> {
    // Peel off Nested parens so `WHERE (x = 1)` is recognized too.
    let peeled = match expr {
        Expr::Nested(inner) => inner.as_ref(),
        other => other,
    };
    let Expr::BinaryOp { left, op, right } = peeled else {
        return None;
    };
    if !matches!(op, BinaryOperator::Eq) {
        return None;
    }
    let col_from = |e: &Expr| -> Option<String> {
        match e {
            Expr::Identifier(ident) => Some(ident.value.clone()),
            Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.clone()),
            _ => None,
        }
    };
    let literal_from = |e: &Expr| -> Option<sqlparser::ast::Value> {
        if let Expr::Value(v) = e {
            Some(v.value.clone())
        } else {
            None
        }
    };
    if let (Some(c), Some(l)) = (col_from(left), literal_from(right)) {
        return Some((c, l));
    }
    if let (Some(l), Some(c)) = (literal_from(left), col_from(right)) {
        return Some((c, l));
    }
    None
}

fn sort_rowids(rowids: &mut [i64], table: &Table, order: &OrderByClause) -> Result<()> {
    if !table.contains_column(order.column.clone()) {
        return Err(SQLRiteError::Internal(format!(
            "ORDER BY references unknown column '{}'",
            order.column
        )));
    }
    rowids.sort_by(|a, b| {
        let va = table.get_value(&order.column, *a);
        let vb = table.get_value(&order.column, *b);
        let ord = compare_values(va.as_ref(), vb.as_ref());
        if order.ascending { ord } else { ord.reverse() }
    });
    Ok(())
}

fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(a), Some(b)) => match (a, b) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
            (Value::Real(x), Value::Real(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (Value::Integer(x), Value::Real(y)) => {
                (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)
            }
            (Value::Real(x), Value::Integer(y)) => {
                x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)
            }
            (Value::Text(x), Value::Text(y)) => x.cmp(y),
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            // Cross-type fallback: stringify and compare; keeps ORDER BY total.
            (x, y) => x.to_display_string().cmp(&y.to_display_string()),
        },
    }
}

/// Returns `true` if the row at `rowid` matches the predicate expression.
pub fn eval_predicate(expr: &Expr, table: &Table, rowid: i64) -> Result<bool> {
    let v = eval_expr(expr, table, rowid)?;
    match v {
        Value::Bool(b) => Ok(b),
        Value::Null => Ok(false), // SQL NULL in a WHERE is treated as false
        Value::Integer(i) => Ok(i != 0),
        other => Err(SQLRiteError::Internal(format!(
            "WHERE clause must evaluate to boolean, got {}",
            other.to_display_string()
        ))),
    }
}

fn eval_expr(expr: &Expr, table: &Table, rowid: i64) -> Result<Value> {
    match expr {
        Expr::Nested(inner) => eval_expr(inner, table, rowid),

        Expr::Identifier(ident) => Ok(table.get_value(&ident.value, rowid).unwrap_or(Value::Null)),

        Expr::CompoundIdentifier(parts) => {
            // Accept `table.col` — we only have one table in scope, so ignore the qualifier.
            let col = parts
                .last()
                .map(|i| i.value.as_str())
                .ok_or_else(|| SQLRiteError::Internal("empty compound identifier".to_string()))?;
            Ok(table.get_value(col, rowid).unwrap_or(Value::Null))
        }

        Expr::Value(v) => convert_literal(&v.value),

        Expr::UnaryOp { op, expr } => {
            let inner = eval_expr(expr, table, rowid)?;
            match op {
                UnaryOperator::Not => match inner {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLRiteError::Internal(format!(
                        "NOT applied to non-boolean value: {}",
                        other.to_display_string()
                    ))),
                },
                UnaryOperator::Minus => match inner {
                    Value::Integer(i) => Ok(Value::Integer(-i)),
                    Value::Real(f) => Ok(Value::Real(-f)),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLRiteError::Internal(format!(
                        "unary minus on non-numeric value: {}",
                        other.to_display_string()
                    ))),
                },
                UnaryOperator::Plus => Ok(inner),
                other => Err(SQLRiteError::NotImplemented(format!(
                    "unary operator {other:?} is not supported"
                ))),
            }
        }

        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::And => {
                let l = eval_expr(left, table, rowid)?;
                let r = eval_expr(right, table, rowid)?;
                Ok(Value::Bool(as_bool(&l)? && as_bool(&r)?))
            }
            BinaryOperator::Or => {
                let l = eval_expr(left, table, rowid)?;
                let r = eval_expr(right, table, rowid)?;
                Ok(Value::Bool(as_bool(&l)? || as_bool(&r)?))
            }
            cmp @ (BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq) => {
                let l = eval_expr(left, table, rowid)?;
                let r = eval_expr(right, table, rowid)?;
                // Any comparison involving NULL is unknown → false in a WHERE.
                if matches!(l, Value::Null) || matches!(r, Value::Null) {
                    return Ok(Value::Bool(false));
                }
                let ord = compare_values(Some(&l), Some(&r));
                let result = match cmp {
                    BinaryOperator::Eq => ord == Ordering::Equal,
                    BinaryOperator::NotEq => ord != Ordering::Equal,
                    BinaryOperator::Lt => ord == Ordering::Less,
                    BinaryOperator::LtEq => ord != Ordering::Greater,
                    BinaryOperator::Gt => ord == Ordering::Greater,
                    BinaryOperator::GtEq => ord != Ordering::Less,
                    _ => unreachable!(),
                };
                Ok(Value::Bool(result))
            }
            arith @ (BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Modulo) => {
                let l = eval_expr(left, table, rowid)?;
                let r = eval_expr(right, table, rowid)?;
                eval_arith(arith, &l, &r)
            }
            BinaryOperator::StringConcat => {
                let l = eval_expr(left, table, rowid)?;
                let r = eval_expr(right, table, rowid)?;
                if matches!(l, Value::Null) || matches!(r, Value::Null) {
                    return Ok(Value::Null);
                }
                Ok(Value::Text(format!(
                    "{}{}",
                    l.to_display_string(),
                    r.to_display_string()
                )))
            }
            other => Err(SQLRiteError::NotImplemented(format!(
                "binary operator {other:?} is not supported yet"
            ))),
        },

        other => Err(SQLRiteError::NotImplemented(format!(
            "unsupported expression in WHERE/projection: {other:?}"
        ))),
    }
}

/// Evaluates an integer/real arithmetic op. NULL on either side propagates.
/// Mixed Integer/Real promotes to Real. Divide/Modulo by zero → error.
fn eval_arith(op: &BinaryOperator, l: &Value, r: &Value) -> Result<Value> {
    if matches!(l, Value::Null) || matches!(r, Value::Null) {
        return Ok(Value::Null);
    }
    match (l, r) {
        (Value::Integer(a), Value::Integer(b)) => match op {
            BinaryOperator::Plus => Ok(Value::Integer(a.wrapping_add(*b))),
            BinaryOperator::Minus => Ok(Value::Integer(a.wrapping_sub(*b))),
            BinaryOperator::Multiply => Ok(Value::Integer(a.wrapping_mul(*b))),
            BinaryOperator::Divide => {
                if *b == 0 {
                    Err(SQLRiteError::General("division by zero".to_string()))
                } else {
                    Ok(Value::Integer(a / b))
                }
            }
            BinaryOperator::Modulo => {
                if *b == 0 {
                    Err(SQLRiteError::General("modulo by zero".to_string()))
                } else {
                    Ok(Value::Integer(a % b))
                }
            }
            _ => unreachable!(),
        },
        // Anything involving a Real promotes both sides to f64.
        (a, b) => {
            let af = as_number(a)?;
            let bf = as_number(b)?;
            match op {
                BinaryOperator::Plus => Ok(Value::Real(af + bf)),
                BinaryOperator::Minus => Ok(Value::Real(af - bf)),
                BinaryOperator::Multiply => Ok(Value::Real(af * bf)),
                BinaryOperator::Divide => {
                    if bf == 0.0 {
                        Err(SQLRiteError::General("division by zero".to_string()))
                    } else {
                        Ok(Value::Real(af / bf))
                    }
                }
                BinaryOperator::Modulo => {
                    if bf == 0.0 {
                        Err(SQLRiteError::General("modulo by zero".to_string()))
                    } else {
                        Ok(Value::Real(af % bf))
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

fn as_number(v: &Value) -> Result<f64> {
    match v {
        Value::Integer(i) => Ok(*i as f64),
        Value::Real(f) => Ok(*f),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(SQLRiteError::General(format!(
            "arithmetic on non-numeric value '{}'",
            other.to_display_string()
        ))),
    }
}

fn as_bool(v: &Value) -> Result<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Null => Ok(false),
        Value::Integer(i) => Ok(*i != 0),
        other => Err(SQLRiteError::Internal(format!(
            "expected boolean, got {}",
            other.to_display_string()
        ))),
    }
}

fn convert_literal(v: &sqlparser::ast::Value) -> Result<Value> {
    use sqlparser::ast::Value as AstValue;
    match v {
        AstValue::Number(n, _) => {
            if let Ok(i) = n.parse::<i64>() {
                Ok(Value::Integer(i))
            } else if let Ok(f) = n.parse::<f64>() {
                Ok(Value::Real(f))
            } else {
                Err(SQLRiteError::Internal(format!(
                    "could not parse numeric literal '{n}'"
                )))
            }
        }
        AstValue::SingleQuotedString(s) => Ok(Value::Text(s.clone())),
        AstValue::Boolean(b) => Ok(Value::Bool(*b)),
        AstValue::Null => Ok(Value::Null),
        other => Err(SQLRiteError::NotImplemented(format!(
            "unsupported literal value: {other:?}"
        ))),
    }
}
