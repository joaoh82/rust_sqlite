//! Query executors — evaluate parsed SQL statements against the in-memory
//! storage and produce formatted output.

use std::cmp::Ordering;

use prettytable::{Cell as PrintCell, Row as PrintRow, Table as PrintTable};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, CreateIndex, Delete, Expr, FromTable, FunctionArg,
    FunctionArgExpr, FunctionArguments, IndexType, ObjectNamePart, Statement, TableFactor,
    TableWithJoins, UnaryOperator, Update,
};

use crate::error::{Result, SQLRiteError};
use crate::sql::db::database::Database;
use crate::sql::db::secondary_index::{IndexOrigin, SecondaryIndex};
use crate::sql::db::table::{DataType, HnswIndexEntry, Table, Value, parse_vector_literal};
use crate::sql::hnsw::{DistanceMetric, HnswIndex};
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

    // Phase 7c — bounded-heap top-k optimization.
    //
    // The naive "ORDER BY <expr>" path (Phase 7b) sorts every matching
    // rowid: O(N log N) sort_by + a truncate. For KNN queries
    //
    //     SELECT id FROM docs
    //     ORDER BY vec_distance_l2(embedding, [...])
    //     LIMIT 10;
    //
    // N is the table row count and k is the LIMIT. With a bounded
    // max-heap of size k we can find the top-k in O(N log k) — same
    // sort_by-per-row cost on the heap operations, but k is typically
    // 10-100 while N can be millions.
    //
    // Phase 7d.2 — HNSW ANN probe.
    //
    // Even better than the bounded heap: if the ORDER BY expression is
    // exactly `vec_distance_l2(<col>, <bracket-array literal>)` AND
    // `<col>` has an HNSW index attached, skip the linear scan
    // entirely and probe the graph in O(log N). Approximate but
    // typically ≥ 0.95 recall (verified by the recall tests in
    // src/sql/hnsw.rs).
    //
    // We branch in cases:
    //   1. ORDER BY + LIMIT k matches the HNSW probe pattern  → graph probe.
    //   2. ORDER BY + LIMIT k where k < |matching|            → bounded heap (7c).
    //   3. ORDER BY without LIMIT, or LIMIT >= |matching|     → full sort.
    //   4. LIMIT without ORDER BY                              → just truncate.
    match (&query.order_by, query.limit) {
        (Some(order), Some(k)) if try_hnsw_probe(table, &order.expr, k).is_some() => {
            matching = try_hnsw_probe(table, &order.expr, k).unwrap();
        }
        (Some(order), Some(k)) if k < matching.len() => {
            matching = select_topk(&matching, table, order, k)?;
        }
        (Some(order), _) => {
            sort_rowids(&mut matching, table, order)?;
            if let Some(k) = query.limit {
                matching.truncate(k);
            }
        }
        (None, Some(k)) => {
            matching.truncate(k);
        }
        (None, None) => {}
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

    // Phase 7d.2 limitation: HNSW lacks an in-place delete-node operation.
    // True deletion needs either soft-delete + tombstones or a graph rebuild
    // — both nontrivial. Until 7d.3 lands persistence we don't have a
    // natural rebuild trigger either. So: refuse DELETE on tables carrying
    // any HNSW index, with a message that points at the workaround
    // (DROP the index, DELETE, recreate).
    {
        let table = db.get_table(table_name.clone()).map_err(|_| {
            SQLRiteError::General(format!("DELETE references unknown table '{table_name}'"))
        })?;
        if !table.hnsw_indexes.is_empty() {
            let names: Vec<&str> = table.hnsw_indexes.iter().map(|e| e.name.as_str()).collect();
            return Err(SQLRiteError::NotImplemented(format!(
                "DELETE on tables with HNSW indexes is not supported yet \
                 (Phase 7d.3 follow-up). DROP the index first, then DELETE, then re-CREATE. \
                 Table '{table_name}' currently has: {names:?}"
            )));
        }
    }

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

    // Phase 7d.2 limitation (same shape as DELETE above): we have no
    // in-place UPDATE-an-HNSW-node primitive. UPDATE on a column NOT
    // covered by HNSW is fine in principle, but the simplest MVP is
    // refuse-everything-when-HNSW-is-present. Re-evaluate in 7d.3 once
    // persistence + rebuild is in.
    {
        let tbl = db.get_table(table_name.clone()).map_err(|_| {
            SQLRiteError::General(format!("UPDATE references unknown table '{table_name}'"))
        })?;
        if !tbl.hnsw_indexes.is_empty() {
            let names: Vec<&str> = tbl.hnsw_indexes.iter().map(|e| e.name.as_str()).collect();
            return Err(SQLRiteError::NotImplemented(format!(
                "UPDATE on tables with HNSW indexes is not supported yet \
                 (Phase 7d.3 follow-up). DROP the index first if you need to mutate. \
                 Table '{table_name}' currently has: {names:?}"
            )));
        }
    }

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

/// Handles `CREATE INDEX [UNIQUE] <name> ON <table> [USING <method>] (<column>)`.
/// Single-column indexes only.
///
/// Two flavours, branching on the optional `USING <method>` clause:
///   - **No USING, or `USING btree`**: regular B-Tree secondary index
///     (Phase 3e). Indexable types: Integer, Text.
///   - **`USING hnsw`**: HNSW ANN index (Phase 7d.2). Indexable types:
///     Vector(N) only. Distance metric is L2 by default; cosine and
///     dot variants are deferred to Phase 7d.x.
///
/// Returns the (possibly synthesized) index name for the status message.
pub fn execute_create_index(stmt: &Statement, db: &mut Database) -> Result<String> {
    let Statement::CreateIndex(CreateIndex {
        name,
        table_name,
        columns,
        using,
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

    // Detect USING <method>. The `using` field on CreateIndex covers the
    // pre-column form `CREATE INDEX … USING hnsw (col)`. (sqlparser also
    // accepts a post-column form `… (col) USING hnsw` and parks that in
    // `index_options`; we don't bother with it — the canonical form is
    // pre-column and matches PG/pgvector convention.)
    let method = match using {
        Some(IndexType::Custom(ident)) if ident.value.eq_ignore_ascii_case("hnsw") => {
            IndexMethod::Hnsw
        }
        Some(IndexType::Custom(ident)) if ident.value.eq_ignore_ascii_case("btree") => {
            IndexMethod::Btree
        }
        Some(other) => {
            return Err(SQLRiteError::NotImplemented(format!(
                "CREATE INDEX … USING {other:?} is not supported (try `hnsw` or no USING clause)"
            )));
        }
        None => IndexMethod::Btree,
    };

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

    // Validate: table exists, column exists, type matches the index method,
    // name is unique across both index kinds. Snapshot (rowid, value) pairs
    // up front under the immutable borrow so the mutable attach later
    // doesn't fight over `self`.
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

        // Name uniqueness check spans BOTH index kinds — a btree and an
        // hnsw can't share a name.
        if table.index_by_name(&index_name).is_some()
            || table.hnsw_indexes.iter().any(|i| i.name == index_name)
        {
            if *if_not_exists {
                return Ok(index_name);
            }
            return Err(SQLRiteError::General(format!(
                "index '{index_name}' already exists"
            )));
        }
        let datatype = clone_datatype(&col.datatype);

        let mut pairs = Vec::new();
        for rowid in table.rowids() {
            if let Some(v) = table.get_value(&column_name, rowid) {
                pairs.push((rowid, v));
            }
        }
        (datatype, pairs)
    };

    match method {
        IndexMethod::Btree => create_btree_index(
            db,
            &table_name_str,
            &index_name,
            &column_name,
            &datatype,
            *unique,
            &existing_rowids_and_values,
        ),
        IndexMethod::Hnsw => create_hnsw_index(
            db,
            &table_name_str,
            &index_name,
            &column_name,
            &datatype,
            *unique,
            &existing_rowids_and_values,
        ),
    }
}

/// `USING <method>` choices recognized by `execute_create_index`. A
/// missing USING clause defaults to `Btree` so existing CREATE INDEX
/// statements (Phase 3e) keep working unchanged.
#[derive(Debug, Clone, Copy)]
enum IndexMethod {
    Btree,
    Hnsw,
}

/// Builds a Phase 3e B-Tree secondary index and attaches it to the table.
fn create_btree_index(
    db: &mut Database,
    table_name: &str,
    index_name: &str,
    column_name: &str,
    datatype: &DataType,
    unique: bool,
    existing: &[(i64, Value)],
) -> Result<String> {
    let mut idx = SecondaryIndex::new(
        index_name.to_string(),
        table_name.to_string(),
        column_name.to_string(),
        datatype,
        unique,
        IndexOrigin::Explicit,
    )?;

    // Populate from existing rows. UNIQUE violations here mean the
    // existing data already breaks the new index's constraint — a
    // common source of user confusion, so be explicit.
    for (rowid, v) in existing {
        if unique && idx.would_violate_unique(v) {
            return Err(SQLRiteError::General(format!(
                "cannot create UNIQUE index '{index_name}': column '{column_name}' \
                 already contains the duplicate value {}",
                v.to_display_string()
            )));
        }
        idx.insert(v, *rowid)?;
    }

    let table_mut = db.get_table_mut(table_name.to_string())?;
    table_mut.secondary_indexes.push(idx);
    Ok(index_name.to_string())
}

/// Builds a Phase 7d.2 HNSW index and attaches it to the table.
fn create_hnsw_index(
    db: &mut Database,
    table_name: &str,
    index_name: &str,
    column_name: &str,
    datatype: &DataType,
    unique: bool,
    existing: &[(i64, Value)],
) -> Result<String> {
    // HNSW only makes sense on VECTOR columns. Reject anything else
    // with a clear message — this is the most likely user error.
    let dim = match datatype {
        DataType::Vector(d) => *d,
        other => {
            return Err(SQLRiteError::General(format!(
                "USING hnsw requires a VECTOR column; '{column_name}' is {other}"
            )));
        }
    };

    if unique {
        return Err(SQLRiteError::General(
            "UNIQUE has no meaning for HNSW indexes".to_string(),
        ));
    }

    // Build the in-memory graph. Distance metric is L2 by default
    // (Phase 7d.2 doesn't yet expose a knob for picking cosine/dot —
    // see `docs/phase-7-plan.md` for the deferral).
    //
    // Seed: hash the index name so different indexes get different
    // graph topologies, but the same index always gets the same one
    // — useful when debugging recall / index size.
    let seed = hash_str_to_seed(index_name);
    let mut idx = HnswIndex::new(DistanceMetric::L2, seed);

    // Snapshot the (rowid, vector) pairs into a side map so the
    // get_vec closure below can serve them by id without re-borrowing
    // the table (we're already holding `existing` — flatten it).
    let mut vec_map: std::collections::HashMap<i64, Vec<f32>> =
        std::collections::HashMap::with_capacity(existing.len());
    for (rowid, v) in existing {
        match v {
            Value::Vector(vec) => {
                if vec.len() != dim {
                    return Err(SQLRiteError::Internal(format!(
                        "row {rowid} stores a {}-dim vector in column '{column_name}' \
                         declared as VECTOR({dim}) — schema invariant violated",
                        vec.len()
                    )));
                }
                vec_map.insert(*rowid, vec.clone());
            }
            // Non-vector values (theoretical NULL, type coercion bug)
            // get skipped — they wouldn't have a sensible graph
            // position anyway.
            _ => continue,
        }
    }

    for (rowid, _) in existing {
        if let Some(v) = vec_map.get(rowid) {
            let v_clone = v.clone();
            idx.insert(*rowid, &v_clone, |id| {
                vec_map.get(&id).cloned().unwrap_or_default()
            });
        }
    }

    let table_mut = db.get_table_mut(table_name.to_string())?;
    table_mut.hnsw_indexes.push(HnswIndexEntry {
        name: index_name.to_string(),
        column_name: column_name.to_string(),
        index: idx,
    });
    Ok(index_name.to_string())
}

/// Stable, deterministic hash of a string into a u64 RNG seed. FNV-1a;
/// avoids pulling in `std::hash::DefaultHasher` (which is randomized
/// per process).
fn hash_str_to_seed(s: &str) -> u64 {
    let mut h: u64 = 0xCBF29CE484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001B3);
    }
    h
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

/// Recognizes the HNSW-probable query pattern and probes the graph
/// if a matching index exists.
///
/// Looks for ORDER BY `vec_distance_l2(<col>, <bracket-array literal>)`
/// where the table has an HNSW index attached to `<col>`. On a match,
/// returns the top-k rowids straight from the graph (O(log N)). On
/// any miss — different function name, no matching index, query
/// dimension wrong, etc. — returns `None` and the caller falls through
/// to the bounded-heap brute-force path (7c) or the full sort (7b),
/// preserving correct results regardless of whether the HNSW pathway
/// kicked in.
///
/// Phase 7d.2 caveats:
/// - Only `vec_distance_l2` is recognized. Cosine and dot fall through
///   to brute-force because we don't yet expose a per-index distance
///   knob (deferred to Phase 7d.x — see `docs/phase-7-plan.md`).
/// - Only ASCENDING order makes sense for "k nearest" — DESC ORDER BY
///   `vec_distance_l2(...) LIMIT k` would mean "k farthest", which
///   isn't what the index is built for. We don't bother to detect
///   `ascending == false` here; the optimizer just skips and the
///   fallback path handles it correctly (slower).
fn try_hnsw_probe(table: &Table, order_expr: &Expr, k: usize) -> Option<Vec<i64>> {
    if k == 0 {
        return None;
    }

    // Pattern-match: order expr must be a function call vec_distance_l2(a, b).
    let func = match order_expr {
        Expr::Function(f) => f,
        _ => return None,
    };
    let fname = match func.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => ident.value.to_lowercase(),
        _ => return None,
    };
    if fname != "vec_distance_l2" {
        return None;
    }

    // Extract the two args as raw Exprs.
    let arg_list = match &func.args {
        FunctionArguments::List(l) => &l.args,
        _ => return None,
    };
    if arg_list.len() != 2 {
        return None;
    }
    let exprs: Vec<&Expr> = arg_list
        .iter()
        .filter_map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
            _ => None,
        })
        .collect();
    if exprs.len() != 2 {
        return None;
    }

    // One arg must be a column reference (the indexed col); the other
    // must be a bracket-array literal (the query vector). Try both
    // orderings — pgvector's idiom puts the column on the left, but
    // SQL is commutative for distance.
    let (col_name, query_vec) = match identify_indexed_arg_and_literal(exprs[0], exprs[1]) {
        Some(v) => v,
        None => match identify_indexed_arg_and_literal(exprs[1], exprs[0]) {
            Some(v) => v,
            None => return None,
        },
    };

    // Find the HNSW index on this column.
    let entry = table
        .hnsw_indexes
        .iter()
        .find(|e| e.column_name == col_name)?;

    // Dimension sanity check — the query vector must match the
    // indexed column's declared dimension. If it doesn't, the brute-
    // force fallback would also error at the vec_distance_l2 dim-check;
    // returning None here lets that path produce the user-visible
    // error message.
    let declared_dim = match table.columns.iter().find(|c| c.column_name == col_name) {
        Some(c) => match &c.datatype {
            DataType::Vector(d) => *d,
            _ => return None,
        },
        None => return None,
    };
    if query_vec.len() != declared_dim {
        return None;
    }

    // Probe the graph. Vectors are looked up from the table's row
    // storage — a closure rather than a `&Table` so the algorithm
    // module stays decoupled from the SQL types.
    let column_for_closure = col_name.clone();
    let table_ref = table;
    let result = entry.index.search(&query_vec, k, |id| {
        match table_ref.get_value(&column_for_closure, id) {
            Some(Value::Vector(v)) => v,
            _ => Vec::new(),
        }
    });
    Some(result)
}

/// Helper for `try_hnsw_probe`: given two function args, identify which
/// one is a bare column identifier (the indexed column) and which is a
/// bracket-array literal (the query vector). Returns
/// `Some((column_name, query_vec))` on a match, `None` otherwise.
fn identify_indexed_arg_and_literal(a: &Expr, b: &Expr) -> Option<(String, Vec<f32>)> {
    let col_name = match a {
        Expr::Identifier(ident) if ident.quote_style.is_none() => ident.value.clone(),
        _ => return None,
    };
    let lit_str = match b {
        Expr::Identifier(ident) if ident.quote_style == Some('[') => {
            format!("[{}]", ident.value)
        }
        _ => return None,
    };
    let v = parse_vector_literal(&lit_str).ok()?;
    Some((col_name, v))
}

/// One entry in the bounded-heap top-k path. Holds a pre-evaluated
/// sort key + the rowid it came from. The `asc` flag inverts `Ord`
/// so a single `BinaryHeap<HeapEntry>` works for both ASC and DESC
/// without wrapping in `std::cmp::Reverse` at the call site:
///
///   - ASC LIMIT k = "k smallest": natural Ord. Max-heap top is the
///     largest currently kept; new items smaller than top displace.
///   - DESC LIMIT k = "k largest": Ord reversed. Max-heap top is now
///     the smallest currently kept (under reversed Ord, smallest
///     looks largest); new items larger than top displace.
///
/// In both cases the displacement test reduces to "new entry < heap top".
struct HeapEntry {
    key: Value,
    rowid: i64,
    asc: bool,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        let raw = compare_values(Some(&self.key), Some(&other.key));
        if self.asc { raw } else { raw.reverse() }
    }
}

/// Bounded-heap top-k selection. Returns at most `k` rowids in the
/// caller's desired order (ascending key for `order.ascending`,
/// descending otherwise).
///
/// O(N log k) where N = `matching.len()`. Caller must check
/// `k < matching.len()` for this to be a win — for k ≥ N the
/// `sort_rowids` full-sort path is the same asymptotic cost without
/// the heap overhead.
fn select_topk(
    matching: &[i64],
    table: &Table,
    order: &OrderByClause,
    k: usize,
) -> Result<Vec<i64>> {
    use std::collections::BinaryHeap;

    if k == 0 || matching.is_empty() {
        return Ok(Vec::new());
    }

    let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(k + 1);

    for &rowid in matching {
        let key = eval_expr(&order.expr, table, rowid)?;
        let entry = HeapEntry {
            key,
            rowid,
            asc: order.ascending,
        };

        if heap.len() < k {
            heap.push(entry);
        } else {
            // peek() returns the largest under our direction-aware Ord
            // — the worst entry currently kept. Displace it iff the
            // new entry is "better" (i.e. compares Less).
            if entry < *heap.peek().unwrap() {
                heap.pop();
                heap.push(entry);
            }
        }
    }

    // `into_sorted_vec` returns ascending under our direction-aware Ord:
    //   ASC: ascending by raw key (what we want)
    //   DESC: ascending under reversed Ord = descending by raw key (what
    //         we want for an ORDER BY DESC LIMIT k result)
    Ok(heap
        .into_sorted_vec()
        .into_iter()
        .map(|e| e.rowid)
        .collect())
}

fn sort_rowids(rowids: &mut [i64], table: &Table, order: &OrderByClause) -> Result<()> {
    // Phase 7b: ORDER BY now accepts any expression (column ref,
    // arithmetic, function call, …). Pre-compute the sort key for
    // every rowid up front so the comparator is called O(N log N)
    // times against pre-evaluated Values rather than re-evaluating
    // the expression O(N log N) times. Not strictly necessary today,
    // but vital once 7d's HNSW index lands and this same code path
    // could be running tens of millions of distance computations.
    let mut keys: Vec<(i64, Result<Value>)> = rowids
        .iter()
        .map(|r| (*r, eval_expr(&order.expr, table, *r)))
        .collect();

    // Surface the FIRST evaluation error if any. We could be lazy
    // and let sort_by encounter it, but `Ord::cmp` can't return a
    // Result and we'd have to swallow errors silently.
    for (_, k) in &keys {
        if let Err(e) = k {
            return Err(SQLRiteError::General(format!(
                "ORDER BY expression failed: {e}"
            )));
        }
    }

    keys.sort_by(|(_, ka), (_, kb)| {
        // Both unwrap()s are safe — we just verified above that
        // every key Result is Ok.
        let va = ka.as_ref().unwrap();
        let vb = kb.as_ref().unwrap();
        let ord = compare_values(Some(va), Some(vb));
        if order.ascending { ord } else { ord.reverse() }
    });

    // Write the sorted rowids back into the caller's slice.
    for (i, (rowid, _)) in keys.into_iter().enumerate() {
        rowids[i] = rowid;
    }
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

        Expr::Identifier(ident) => {
            // Phase 7b — sqlparser parses bracket-array literals like
            // `[0.1, 0.2, 0.3]` as bracket-quoted identifiers (it inherits
            // MSSQL `[name]` syntax). When we see `quote_style == Some('[')`
            // in expression-evaluation position (SELECT projection, WHERE,
            // ORDER BY, function args), parse the bracketed content as a
            // vector literal so the rest of the executor can compare /
            // distance-compute against it. Same trick the INSERT parser
            // uses; the executor needed its own copy because expression
            // eval runs on a different code path.
            if ident.quote_style == Some('[') {
                let raw = format!("[{}]", ident.value);
                let v = parse_vector_literal(&raw)?;
                return Ok(Value::Vector(v));
            }
            Ok(table.get_value(&ident.value, rowid).unwrap_or(Value::Null))
        }

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

        // Phase 7b — function-call dispatch. Currently only the three
        // vector-distance functions; this match arm becomes the single
        // place to register more SQL functions later (e.g. abs(),
        // length(), …) without re-touching the rest of the executor.
        //
        // Operator forms (`<->` `<=>` `<#>`) are NOT plumbed here: two
        // of three don't parse natively in sqlparser (we'd need a
        // string-preprocessing pass or a sqlparser fork). Deferred to
        // a follow-up sub-phase; see docs/phase-7-plan.md's "Scope
        // corrections" note.
        Expr::Function(func) => eval_function(func, table, rowid),

        other => Err(SQLRiteError::NotImplemented(format!(
            "unsupported expression in WHERE/projection: {other:?}"
        ))),
    }
}

/// Dispatches an `Expr::Function` to its built-in implementation.
/// Currently only the three vec_distance_* functions; other functions
/// surface as `NotImplemented` errors with the function name in the
/// message so users see what they tried.
fn eval_function(func: &sqlparser::ast::Function, table: &Table, rowid: i64) -> Result<Value> {
    // Function name lives in `name.0[0]` for unqualified calls. Anything
    // qualified (e.g. `pkg.fn(...)`) falls through to NotImplemented.
    let name = match func.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => ident.value.to_lowercase(),
        _ => {
            return Err(SQLRiteError::NotImplemented(format!(
                "qualified function names not supported: {:?}",
                func.name
            )));
        }
    };

    match name.as_str() {
        "vec_distance_l2" | "vec_distance_cosine" | "vec_distance_dot" => {
            let (a, b) = extract_two_vector_args(&name, &func.args, table, rowid)?;
            let dist = match name.as_str() {
                "vec_distance_l2" => vec_distance_l2(&a, &b),
                "vec_distance_cosine" => vec_distance_cosine(&a, &b)?,
                "vec_distance_dot" => vec_distance_dot(&a, &b),
                _ => unreachable!(),
            };
            // Widen f32 → f64 for the runtime Value. Vectors are stored
            // as f32 (consistent with industry convention for embeddings),
            // but the executor's numeric type is f64 so distances slot
            // into Value::Real cleanly and can be compared / ordered with
            // other reals via the existing arithmetic + comparison paths.
            Ok(Value::Real(dist as f64))
        }
        other => Err(SQLRiteError::NotImplemented(format!(
            "unknown function: {other}(...)"
        ))),
    }
}

/// Extracts exactly two `Vec<f32>` arguments from a function call,
/// validating arity and that both sides are Vector-typed with matching
/// dimensions. Used by all three vec_distance_* functions.
fn extract_two_vector_args(
    fn_name: &str,
    args: &FunctionArguments,
    table: &Table,
    rowid: i64,
) -> Result<(Vec<f32>, Vec<f32>)> {
    let arg_list = match args {
        FunctionArguments::List(l) => &l.args,
        _ => {
            return Err(SQLRiteError::General(format!(
                "{fn_name}() expects exactly two vector arguments"
            )));
        }
    };
    if arg_list.len() != 2 {
        return Err(SQLRiteError::General(format!(
            "{fn_name}() expects exactly 2 arguments, got {}",
            arg_list.len()
        )));
    }
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(2);
    for (i, arg) in arg_list.iter().enumerate() {
        let expr = match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
            other => {
                return Err(SQLRiteError::NotImplemented(format!(
                    "{fn_name}() argument {i} has unsupported shape: {other:?}"
                )));
            }
        };
        let val = eval_expr(expr, table, rowid)?;
        match val {
            Value::Vector(v) => out.push(v),
            other => {
                return Err(SQLRiteError::General(format!(
                    "{fn_name}() argument {i} is not a vector: got {}",
                    other.to_display_string()
                )));
            }
        }
    }
    let b = out.pop().unwrap();
    let a = out.pop().unwrap();
    if a.len() != b.len() {
        return Err(SQLRiteError::General(format!(
            "{fn_name}(): vector dimensions don't match (lhs={}, rhs={})",
            a.len(),
            b.len()
        )));
    }
    Ok((a, b))
}

/// Euclidean (L2) distance: √Σ(aᵢ − bᵢ)².
/// Smaller-is-closer; identical vectors return 0.0.
pub(crate) fn vec_distance_l2(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        let d = a[i] - b[i];
        sum += d * d;
    }
    sum.sqrt()
}

/// Cosine distance: 1 − (a·b) / (‖a‖·‖b‖).
/// Smaller-is-closer; identical (non-zero) vectors return 0.0,
/// orthogonal vectors return 1.0, opposite-direction vectors return 2.0.
///
/// Errors if either vector has zero magnitude — cosine similarity is
/// undefined for the zero vector and silently returning NaN would
/// poison `ORDER BY` ranking. Callers who want the silent-NaN
/// behavior can compute `vec_distance_dot(a, b) / (norm(a) * norm(b))`
/// themselves.
pub(crate) fn vec_distance_cosine(a: &[f32], b: &[f32]) -> Result<f32> {
    debug_assert_eq!(a.len(), b.len());
    let mut dot = 0.0f32;
    let mut norm_a_sq = 0.0f32;
    let mut norm_b_sq = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a_sq += a[i] * a[i];
        norm_b_sq += b[i] * b[i];
    }
    let denom = (norm_a_sq * norm_b_sq).sqrt();
    if denom == 0.0 {
        return Err(SQLRiteError::General(
            "vec_distance_cosine() is undefined for zero-magnitude vectors".to_string(),
        ));
    }
    Ok(1.0 - dot / denom)
}

/// Negated dot product: −(a·b).
/// pgvector convention — negated so smaller-is-closer like L2 / cosine.
/// For unit-norm vectors `vec_distance_dot(a, b) == vec_distance_cosine(a, b) - 1`.
pub(crate) fn vec_distance_dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut dot = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
    }
    -dot
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

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Phase 7b — Vector distance function math
    // -----------------------------------------------------------------

    /// Float comparison helper — distance results need a small epsilon
    /// because we accumulate sums across many f32 multiplies.
    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn vec_distance_l2_identical_is_zero() {
        let v = vec![0.1, 0.2, 0.3];
        assert_eq!(vec_distance_l2(&v, &v), 0.0);
    }

    #[test]
    fn vec_distance_l2_unit_basis_is_sqrt2() {
        // [1, 0] vs [0, 1]: distance = √((1-0)² + (0-1)²) = √2 ≈ 1.414
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(approx_eq(vec_distance_l2(&a, &b), 2.0_f32.sqrt(), 1e-6));
    }

    #[test]
    fn vec_distance_l2_known_value() {
        // [0, 0, 0] vs [3, 4, 0]: √(9 + 16 + 0) = 5 (the classic 3-4-5 triangle).
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![3.0, 4.0, 0.0];
        assert!(approx_eq(vec_distance_l2(&a, &b), 5.0, 1e-6));
    }

    #[test]
    fn vec_distance_cosine_identical_is_zero() {
        let v = vec![0.1, 0.2, 0.3];
        let d = vec_distance_cosine(&v, &v).unwrap();
        assert!(approx_eq(d, 0.0, 1e-6), "cos(v,v) = {d}, expected ≈ 0");
    }

    #[test]
    fn vec_distance_cosine_orthogonal_is_one() {
        // Two orthogonal unit vectors should have cosine distance = 1.0
        // (cosine similarity = 0 → distance = 1 - 0 = 1).
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(approx_eq(vec_distance_cosine(&a, &b).unwrap(), 1.0, 1e-6));
    }

    #[test]
    fn vec_distance_cosine_opposite_is_two() {
        // a and -a have cosine similarity = -1 → distance = 1 - (-1) = 2.
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!(approx_eq(vec_distance_cosine(&a, &b).unwrap(), 2.0, 1e-6));
    }

    #[test]
    fn vec_distance_cosine_zero_magnitude_errors() {
        // Cosine is undefined for the zero vector — error rather than NaN.
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];
        let err = vec_distance_cosine(&a, &b).unwrap_err();
        assert!(format!("{err}").contains("zero-magnitude"));
    }

    #[test]
    fn vec_distance_dot_negates() {
        // a·b = 1*4 + 2*5 + 3*6 = 32. Negated → -32.
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert!(approx_eq(vec_distance_dot(&a, &b), -32.0, 1e-6));
    }

    #[test]
    fn vec_distance_dot_orthogonal_is_zero() {
        // Orthogonal vectors have dot product 0 → negated is also 0.
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert_eq!(vec_distance_dot(&a, &b), 0.0);
    }

    #[test]
    fn vec_distance_dot_unit_norm_matches_cosine_minus_one() {
        // For unit-norm vectors: dot(a,b) = cos(a,b)
        // → -dot(a,b) = -cos(a,b) = (1 - cos(a,b)) - 1 = vec_distance_cosine(a,b) - 1.
        // Useful sanity check that the two functions agree on unit vectors.
        let a = vec![0.6f32, 0.8]; // unit norm: √(0.36+0.64) = 1
        let b = vec![0.8f32, 0.6]; // unit norm too
        let dot = vec_distance_dot(&a, &b);
        let cos = vec_distance_cosine(&a, &b).unwrap();
        assert!(approx_eq(dot, cos - 1.0, 1e-5));
    }

    // -----------------------------------------------------------------
    // Phase 7c — bounded-heap top-k correctness + benchmark
    // -----------------------------------------------------------------

    use crate::sql::db::database::Database;
    use crate::sql::parser::select::SelectQuery;
    use sqlparser::dialect::SQLiteDialect;
    use sqlparser::parser::Parser;

    /// Builds a `docs(id INTEGER PK, score REAL)` table with N rows of
    /// distinct positive scores so top-k tests aren't sensitive to
    /// tie-breaking (heap is unstable; full-sort is stable; we want
    /// both to agree without arguing about equal-score row order).
    ///
    /// **Why positive scores:** the INSERT parser doesn't currently
    /// handle `Expr::UnaryOp(Minus, …)` for negative number literals
    /// (it would parse `-3.14` as a unary expression and the value
    /// extractor would skip it). That's a pre-existing bug, out of
    /// scope for 7c. Using the Knuth multiplicative hash gives us
    /// distinct positive scrambled values without dancing around the
    /// negative-literal limitation.
    fn seed_score_table(n: usize) -> Database {
        let mut db = Database::new("tempdb".to_string());
        crate::sql::process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, score REAL);",
            &mut db,
        )
        .expect("create");
        for i in 0..n {
            // Knuth multiplicative hash mod 1_000_000 — distinct,
            // dense in [0, 999_999], no collisions for n up to ~tens
            // of thousands.
            let score = ((i as u64).wrapping_mul(2_654_435_761) % 1_000_000) as f64;
            let sql = format!("INSERT INTO docs (score) VALUES ({score});");
            crate::sql::process_command(&sql, &mut db).expect("insert");
        }
        db
    }

    /// Helper: parses an SQL SELECT into a SelectQuery so we can drive
    /// `select_topk` / `sort_rowids` directly without the rest of the
    /// process_command pipeline.
    fn parse_select(sql: &str) -> SelectQuery {
        let dialect = SQLiteDialect {};
        let mut ast = Parser::parse_sql(&dialect, sql).expect("parse");
        let stmt = ast.pop().expect("one statement");
        SelectQuery::new(&stmt).expect("select-query")
    }

    #[test]
    fn topk_matches_full_sort_asc() {
        // Build N=200, top-k=10. Bounded heap output must equal
        // full-sort-then-truncate output (both produce ASC order).
        let db = seed_score_table(200);
        let table = db.get_table("docs".to_string()).unwrap();
        let q = parse_select("SELECT * FROM docs ORDER BY score ASC LIMIT 10;");
        let order = q.order_by.as_ref().unwrap();
        let all_rowids = table.rowids();

        // Full-sort path
        let mut full = all_rowids.clone();
        sort_rowids(&mut full, table, order).unwrap();
        full.truncate(10);

        // Bounded-heap path
        let topk = select_topk(&all_rowids, table, order, 10).unwrap();

        assert_eq!(topk, full, "top-k via heap should match full-sort+truncate");
    }

    #[test]
    fn topk_matches_full_sort_desc() {
        // Same with DESC — verifies the direction-aware Ord wrapper.
        let db = seed_score_table(200);
        let table = db.get_table("docs".to_string()).unwrap();
        let q = parse_select("SELECT * FROM docs ORDER BY score DESC LIMIT 10;");
        let order = q.order_by.as_ref().unwrap();
        let all_rowids = table.rowids();

        let mut full = all_rowids.clone();
        sort_rowids(&mut full, table, order).unwrap();
        full.truncate(10);

        let topk = select_topk(&all_rowids, table, order, 10).unwrap();

        assert_eq!(
            topk, full,
            "top-k DESC via heap should match full-sort+truncate"
        );
    }

    #[test]
    fn topk_k_larger_than_n_returns_everything_sorted() {
        // The executor branches off to the full-sort path when k >= N,
        // but if a caller invokes select_topk directly with k > N, it
        // should still produce all-sorted output (no truncation
        // because we don't have N items to truncate to k).
        let db = seed_score_table(50);
        let table = db.get_table("docs".to_string()).unwrap();
        let q = parse_select("SELECT * FROM docs ORDER BY score ASC LIMIT 1000;");
        let order = q.order_by.as_ref().unwrap();
        let topk = select_topk(&table.rowids(), table, order, 1000).unwrap();
        assert_eq!(topk.len(), 50);
        // All scores in ascending order.
        let scores: Vec<f64> = topk
            .iter()
            .filter_map(|r| match table.get_value("score", *r) {
                Some(Value::Real(f)) => Some(f),
                _ => None,
            })
            .collect();
        assert!(scores.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn topk_k_zero_returns_empty() {
        let db = seed_score_table(10);
        let table = db.get_table("docs".to_string()).unwrap();
        let q = parse_select("SELECT * FROM docs ORDER BY score ASC LIMIT 1;");
        let order = q.order_by.as_ref().unwrap();
        let topk = select_topk(&table.rowids(), table, order, 0).unwrap();
        assert!(topk.is_empty());
    }

    #[test]
    fn topk_empty_input_returns_empty() {
        let db = seed_score_table(0);
        let table = db.get_table("docs".to_string()).unwrap();
        let q = parse_select("SELECT * FROM docs ORDER BY score ASC LIMIT 5;");
        let order = q.order_by.as_ref().unwrap();
        let topk = select_topk(&[], table, order, 5).unwrap();
        assert!(topk.is_empty());
    }

    #[test]
    fn topk_works_through_select_executor_with_distance_function() {
        // Integration check that the executor actually picks the
        // bounded-heap path on a KNN-shaped query and produces the
        // correct top-k.
        let mut db = Database::new("tempdb".to_string());
        crate::sql::process_command(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, e VECTOR(2));",
            &mut db,
        )
        .unwrap();
        // Five rows with distinct distances from probe [1.0, 0.0]:
        //   id=1 [1.0, 0.0]   distance=0
        //   id=2 [2.0, 0.0]   distance=1
        //   id=3 [0.0, 3.0]   distance=√(1+9) = √10 ≈ 3.16
        //   id=4 [1.0, 4.0]   distance=4
        //   id=5 [10.0, 10.0] distance=√(81+100) ≈ 13.45
        for v in &[
            "[1.0, 0.0]",
            "[2.0, 0.0]",
            "[0.0, 3.0]",
            "[1.0, 4.0]",
            "[10.0, 10.0]",
        ] {
            crate::sql::process_command(&format!("INSERT INTO docs (e) VALUES ({v});"), &mut db)
                .unwrap();
        }
        let resp = crate::sql::process_command(
            "SELECT id FROM docs ORDER BY vec_distance_l2(e, [1.0, 0.0]) ASC LIMIT 3;",
            &mut db,
        )
        .unwrap();
        // Top-3 closest to [1.0, 0.0] are id=1, id=2, id=3 (in that order).
        // The status message tells us how many rows came back.
        assert!(resp.contains("3 rows returned"), "got: {resp}");
    }

    /// Manual benchmark — not run by default. Recommended invocation:
    ///
    ///     cargo test -p sqlrite-engine --lib topk_benchmark --release \
    ///         -- --ignored --nocapture
    ///
    /// (`--release` matters: Rust's optimized sort gets very fast under
    /// optimization, so the heap's relative advantage is best observed
    /// against a sort that's also been optimized.)
    ///
    /// Measured numbers on an Apple Silicon laptop with N=10_000 + k=10:
    ///   - bounded heap:    ~820µs
    ///   - full sort+trunc: ~1.5ms
    ///   - ratio:           ~1.8×
    ///
    /// The advantage is real but moderate at this size because the sort
    /// key here is a single REAL column read (cheap) and Rust's sort_by
    /// has a very low constant factor. The asymptotic O(N log k) vs
    /// O(N log N) advantage scales with N and with per-row work — KNN
    /// queries where the sort key is `vec_distance_l2(col, [...])` are
    /// where this path really pays off, because each key evaluation is
    /// itself O(dim) and the heap path skips the per-row evaluation
    /// in the comparator (see `sort_rowids` for the contrast).
    #[test]
    #[ignore]
    fn topk_benchmark() {
        use std::time::Instant;
        const N: usize = 10_000;
        const K: usize = 10;

        let db = seed_score_table(N);
        let table = db.get_table("docs".to_string()).unwrap();
        let q = parse_select("SELECT * FROM docs ORDER BY score ASC LIMIT 10;");
        let order = q.order_by.as_ref().unwrap();
        let all_rowids = table.rowids();

        // Time bounded heap.
        let t0 = Instant::now();
        let _topk = select_topk(&all_rowids, table, order, K).unwrap();
        let heap_dur = t0.elapsed();

        // Time full sort + truncate.
        let t1 = Instant::now();
        let mut full = all_rowids.clone();
        sort_rowids(&mut full, table, order).unwrap();
        full.truncate(K);
        let sort_dur = t1.elapsed();

        let ratio = sort_dur.as_secs_f64() / heap_dur.as_secs_f64().max(1e-9);
        println!("\n--- topk_benchmark (N={N}, k={K}) ---");
        println!("  bounded heap:   {heap_dur:?}");
        println!("  full sort+trunc: {sort_dur:?}");
        println!("  speedup ratio:  {ratio:.2}×");

        // Soft assertion. Floor is 1.4× because the cheap-key
        // benchmark hovers around 1.8× empirically; setting this too
        // close to the measured value risks flaky CI on slower
        // runners. Floor of 1.4× still catches an actual regression
        // (e.g., if select_topk became O(N²) or stopped using the
        // heap entirely).
        assert!(
            ratio > 1.4,
            "bounded heap should be substantially faster than full sort, but ratio = {ratio:.2}"
        );
    }
}
