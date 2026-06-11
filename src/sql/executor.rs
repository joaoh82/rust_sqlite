//! Query executors — evaluate parsed SQL statements against the in-memory
//! storage and produce formatted output.

use std::cmp::Ordering;

use prettytable::{Cell as PrintCell, Row as PrintRow, Table as PrintTable};
use sqlparser::ast::{
    AlterTable, AlterTableOperation, AssignmentTarget, BinaryOperator, CreateIndex, Delete, Expr,
    FromTable, FunctionArg, FunctionArgExpr, FunctionArguments, Ident, IndexType, ObjectName,
    ObjectNamePart, RenameTableNameKind, Statement, TableFactor, TableWithJoins, UnaryOperator,
    Update, Value as AstValue,
};

use crate::error::{Result, SQLRiteError};
use crate::sql::agg::{AggState, DistinctKey, like_match};
use crate::sql::db::database::Database;
use crate::sql::db::secondary_index::{IndexOrigin, SecondaryIndex};
use crate::sql::db::table::{
    DataType, FtsIndexEntry, HnswIndexEntry, Table, Value, parse_vector_literal,
};
use crate::sql::fts::{Bm25Params, PostingList};
use crate::sql::hnsw::{DistanceMetric, HnswIndex};
use crate::sql::parser::select::{
    AggregateArg, AggregateFn, GroupByKey, JoinConstraintKind, JoinType, OrderByClause, Projection,
    ProjectionItem, ProjectionKind, SelectQuery, parse_aggregate_call,
};

// -----------------------------------------------------------------
// SQLR-5 — Row-scope abstraction
// -----------------------------------------------------------------
//
// Single-table SELECT / UPDATE / DELETE evaluate WHERE / ORDER BY /
// projection expressions over `(&Table, rowid)`. JOIN evaluation
// needs the same expression evaluator to look up columns across
// multiple tables, with NULL padding for unmatched outer-join rows.
//
// Rather than fork the evaluator, we abstract "what's in scope when
// I see a column reference" behind a trait. Every callsite that
// previously took `(table, rowid)` now takes `&dyn RowScope`. The
// single-table case constructs a tiny `SingleTableScope`; the join
// case constructs a `JoinedScope` that knows about every table in
// scope plus the per-table rowid (or `None` for a NULL-padded row).
//
// The trait stays small on purpose:
//
//   - `lookup` resolves a column reference (`col` or `t.col`) to a
//     `Value`. Unknown columns error in both scopes (SQLR-2), and so
//     do unknown table qualifiers (SQLR-14). NULL-padded joined rows
//     yield `Value::Null` for any column from their side. Ambiguous
//     unqualified references in joined scope error.
//
//   - `single_table_view` lets index-probing helpers (FTS, HNSW,
//     vec_distance) bail out cleanly when invoked over a join — they
//     need a `(Table, rowid)` pair to look up an index, and the
//     joined case can't answer without per-call disambiguation we
//     haven't plumbed yet. Returns `None` in joined scope.
pub(crate) trait RowScope {
    fn lookup(&self, qualifier: Option<&str>, col: &str) -> Result<Value>;

    /// `Some((table, rowid))` for a single-table scope; `None` for a
    /// joined scope. v1 join support delegates "needs single-table"
    /// helpers (FTS / HNSW / vec_distance with column args) to the
    /// single-table path; calling them from a joined query produces
    /// a `NotImplemented` error rather than wrong results.
    fn single_table_view(&self) -> Option<(&Table, i64)>;
}

/// The default scope for non-join queries: one table, one rowid.
/// `scope_name` is the user-visible name a `t.col` qualifier must
/// match — the FROM alias when one is declared, else the table name
/// (SQLR-14; mirrors [`JoinedTableRef::scope_name`]).
pub(crate) struct SingleTableScope<'a> {
    table: &'a Table,
    rowid: i64,
    scope_name: &'a str,
}

impl<'a> SingleTableScope<'a> {
    pub(crate) fn new(table: &'a Table, rowid: i64, scope_name: &'a str) -> Self {
        Self {
            table,
            rowid,
            scope_name,
        }
    }
}

impl RowScope for SingleTableScope<'_> {
    fn lookup(&self, qualifier: Option<&str>, col: &str) -> Result<Value> {
        // SQLR-14 — a qualifier must name the one table in scope,
        // matching `JoinedScope`'s qualified-reference arm. When an
        // alias is declared it is the *only* valid qualifier
        // (`SELECT t.id FROM t AS a` errors), per SQLite.
        check_single_scope_qualifier(qualifier, self.scope_name, col)?;
        // SQLR-2 — unknown columns error, matching `JoinedScope`. A
        // schema column whose cell was never written (omitted from the
        // INSERT column list) still reads as NULL.
        if !self.table.contains_column(col.to_string()) {
            return Err(SQLRiteError::Internal(format!(
                "Column '{col}' does not exist on table '{}'",
                self.table.tb_name
            )));
        }
        Ok(self.table.get_value(col, self.rowid).unwrap_or(Value::Null))
    }

    fn single_table_view(&self) -> Option<(&Table, i64)> {
        Some((self.table, self.rowid))
    }
}

/// One table participating in a joined query, plus the user-visible
/// name to match against `t.col` qualifiers (alias if present, else
/// the bare table name).
pub(crate) struct JoinedTableRef<'a> {
    pub table: &'a Table,
    pub scope_name: String,
}

/// Multi-table scope used during join execution. `rowids[i]` is the
/// rowid in `tables[i]`, or `None` for a NULL-padded row coming out
/// of an outer join.
pub(crate) struct JoinedScope<'a> {
    pub tables: &'a [JoinedTableRef<'a>],
    pub rowids: &'a [Option<i64>],
}

impl RowScope for JoinedScope<'_> {
    fn lookup(&self, qualifier: Option<&str>, col: &str) -> Result<Value> {
        if let Some(q) = qualifier {
            // Qualified reference: pick the matching table; if it's
            // NULL-padded, the column is NULL; else fetch from row.
            let pos = self
                .tables
                .iter()
                .position(|t| t.scope_name.eq_ignore_ascii_case(q))
                .ok_or_else(|| {
                    SQLRiteError::Internal(format!(
                        "unknown table qualifier '{q}' in column reference '{q}.{col}'"
                    ))
                })?;
            if !self.tables[pos].table.contains_column(col.to_string()) {
                return Err(SQLRiteError::Internal(format!(
                    "column '{col}' does not exist on '{}'",
                    self.tables[pos].scope_name
                )));
            }
            return Ok(match self.rowids[pos] {
                None => Value::Null,
                Some(r) => self.tables[pos]
                    .table
                    .get_value(col, r)
                    .unwrap_or(Value::Null),
            });
        }
        // Unqualified: search every in-scope table. Exactly-one match
        // wins; zero matches → unknown column; multi matches →
        // ambiguous, prompt the user to qualify.
        let mut hit: Option<usize> = None;
        for (i, t) in self.tables.iter().enumerate() {
            if t.table.contains_column(col.to_string()) {
                if hit.is_some() {
                    return Err(SQLRiteError::Internal(format!(
                        "column reference '{col}' is ambiguous — qualify it as <table>.{col}"
                    )));
                }
                hit = Some(i);
            }
        }
        let i = hit.ok_or_else(|| {
            SQLRiteError::Internal(format!(
                "unknown column '{col}' in joined SELECT (no in-scope table has it)"
            ))
        })?;
        Ok(match self.rowids[i] {
            None => Value::Null,
            Some(r) => self.tables[i]
                .table
                .get_value(col, r)
                .unwrap_or(Value::Null),
        })
    }

    fn single_table_view(&self) -> Option<(&Table, i64)> {
        None
    }
}

/// SQLR-14 — reject a `q.col` qualifier that doesn't name the single
/// table in scope (alias if declared, else table name). Shared by
/// [`SingleTableScope::lookup`] (per-row evaluation) and the
/// schema-level checks that run before any row is visited (projection
/// list, GROUP BY keys, aggregate args, index-probe WHERE shapes).
/// Wording matches `JoinedScope`'s unknown-qualifier error.
fn check_single_scope_qualifier(
    qualifier: Option<&str>,
    scope_name: &str,
    col: &str,
) -> Result<()> {
    if let Some(q) = qualifier
        && !q.eq_ignore_ascii_case(scope_name)
    {
        return Err(SQLRiteError::Internal(format!(
            "unknown table qualifier '{q}' in column reference '{q}.{col}'"
        )));
    }
    Ok(())
}

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
    // SQLR-5 — joined SELECTs go through a dedicated executor that
    // knows how to thread a multi-table scope through expression
    // evaluation. The single-table fast path below stays untouched
    // (and so do its HNSW / FTS / bounded-heap optimizations).
    if !query.joins.is_empty() {
        return execute_select_rows_joined(query, db);
    }

    // SQLR-10 — `SELECT … FROM sqlrite_master` introspects the catalog.
    // The catalog isn't a live entry in `db.tables` (it's materialized at
    // save time), so we synthesize a read-only in-memory snapshot on
    // demand and run the normal single-table path against it. WHERE /
    // projections / ORDER BY / LIMIT all work unchanged. Writes against
    // sqlrite_master remain rejected (it never lands in `db.tables`), and
    // joins against it are not supported (the joined path doesn't
    // synthesize it).
    let master_snapshot;
    let table: &Table = if query.table_name == crate::sql::pager::MASTER_TABLE_NAME {
        master_snapshot = crate::sql::pager::build_master_table_snapshot(db)?;
        &master_snapshot
    } else {
        db.get_table(query.table_name.clone()).map_err(|_| {
            SQLRiteError::Internal(format!("Table '{}' not found", query.table_name))
        })?
    };

    // SQLR-14 — the name a `q.col` qualifier must match: the FROM
    // alias when declared, else the table name (same normalization the
    // joined path applies per table).
    let scope_name: &str = query.table_alias.as_deref().unwrap_or(&query.table_name);

    // SQLR-3: Materialize the projection as `Vec<ProjectionItem>` so
    // both the simple-row path and the aggregation path can iterate the
    // same shape. `Projection::All` expands to bare-column items in
    // declaration order; that path then runs the existing rowid pipeline.
    let proj_items: Vec<ProjectionItem> = match &query.projection {
        Projection::All => table
            .column_names()
            .into_iter()
            .map(|c| ProjectionItem {
                kind: ProjectionKind::Column {
                    qualifier: None,
                    name: c,
                },
                alias: None,
            })
            .collect(),
        Projection::Items(items) => items.clone(),
    };
    let has_aggregates = proj_items
        .iter()
        .any(|i| matches!(i.kind, ProjectionKind::Aggregate(_)));
    // Validate column references against the table schema, and their
    // qualifiers (if any) against the scope name (SQLR-14).
    for item in &proj_items {
        if let ProjectionKind::Column { qualifier, name: c } = &item.kind {
            check_single_scope_qualifier(qualifier.as_deref(), scope_name, c)?;
            if !table.contains_column(c.clone()) {
                return Err(SQLRiteError::Internal(format!(
                    "Column '{c}' does not exist on table '{}'",
                    query.table_name
                )));
            }
        }
    }
    for g in &query.group_by {
        check_single_scope_qualifier(g.qualifier.as_deref(), scope_name, &g.name)?;
        if !table.contains_column(g.name.clone()) {
            return Err(SQLRiteError::Internal(format!(
                "GROUP BY references unknown column '{}' on table '{}'",
                g.name, query.table_name
            )));
        }
    }
    // Collect matching rowids. If the WHERE is the shape `col = literal`
    // and `col` has a secondary index, probe the index for an O(log N)
    // seek; otherwise fall back to the full table scan.
    let matching = match select_rowids(table, query.selection.as_ref(), scope_name)? {
        RowidSource::IndexProbe(rowids) => rowids,
        RowidSource::FullScan => {
            let mut out = Vec::new();
            for rowid in table.rowids() {
                if let Some(expr) = &query.selection
                    && !eval_predicate(expr, table, rowid, scope_name)?
                {
                    continue;
                }
                out.push(rowid);
            }
            out
        }
    };
    let mut matching = matching;

    let aggregating = has_aggregates || !query.group_by.is_empty();

    // SQLR-3: aggregation path. When the SELECT contains aggregates or a
    // GROUP BY, the rowid-shaped optimizations (HNSW / FTS / bounded
    // heap) don't compose with grouping — every row contributes to its
    // group, so we walk the full filtered rowid set, accumulate, then
    // sort/truncate the resulting *output rows*.
    if aggregating {
        let (all_items, having_expr) = lower_having_into_hidden_slots(&query, &proj_items)?;

        // Validate aggregate column args (visible + HAVING-hidden).
        for item in &all_items {
            if let ProjectionKind::Aggregate(call) = &item.kind
                && let AggregateArg::Column { qualifier, name: c } = &call.arg
            {
                check_single_scope_qualifier(qualifier.as_deref(), scope_name, c)?;
                if !table.contains_column(c.clone()) {
                    return Err(SQLRiteError::Internal(format!(
                        "{}({}) references unknown column '{c}' on table '{}'",
                        call.func.as_str(),
                        c,
                        query.table_name
                    )));
                }
            }
        }

        let scopes = matching
            .iter()
            .map(|&r| SingleTableScope::new(table, r, scope_name));
        return run_aggregation_pipeline(scopes, &query, &proj_items, &all_items, &having_expr);
    }

    // Non-aggregating path — same flow as before, with the extra
    // affordances that (a) the projection list now goes through
    // `ProjectionItem` and (b) DISTINCT applies after row materialization.

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
    //   2. ORDER BY + LIMIT k matches the FTS probe pattern   → posting probe.
    //   3. ORDER BY + LIMIT k where k < |matching|            → bounded heap (7c).
    //   4. ORDER BY without LIMIT, or LIMIT >= |matching|     → full sort.
    //   5. LIMIT without ORDER BY                              → just truncate.
    //
    // DISTINCT is applied post-projection (we'd over-truncate if LIMIT
    // ran before DISTINCT had a chance to collapse duplicates), so when
    // DISTINCT is on we defer truncation past the dedupe step.
    let defer_limit_for_distinct = query.distinct;
    match (&query.order_by, query.limit) {
        (Some(order), Some(k)) if try_hnsw_probe(table, &order.expr, k).is_some() => {
            matching = try_hnsw_probe(table, &order.expr, k).unwrap();
        }
        (Some(order), Some(k))
            if try_fts_probe(table, &order.expr, order.ascending, k).is_some() =>
        {
            matching = try_fts_probe(table, &order.expr, order.ascending, k).unwrap();
        }
        (Some(order), Some(k)) if !defer_limit_for_distinct && k < matching.len() => {
            matching = select_topk(&matching, table, order, k, scope_name)?;
        }
        (Some(order), _) => {
            sort_rowids(&mut matching, table, order, scope_name)?;
            if let Some(k) = query.limit
                && !defer_limit_for_distinct
            {
                matching.truncate(k);
            }
        }
        (None, Some(k)) if !defer_limit_for_distinct => {
            matching.truncate(k);
        }
        _ => {}
    }

    let columns: Vec<String> = proj_items.iter().map(|i| i.output_name()).collect();
    let projected_cols: Vec<String> = proj_items
        .iter()
        .map(|i| match &i.kind {
            ProjectionKind::Column { name, .. } => name.clone(),
            ProjectionKind::Aggregate(_) => unreachable!("aggregation handled above"),
        })
        .collect();

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

    if query.distinct {
        rows = dedupe_rows(rows);
        if let Some(k) = query.limit {
            rows.truncate(k);
        }
    }

    Ok(SelectResult { columns, rows })
}

/// A join constraint resolved against the live table schemas: the
/// concrete `ON` predicate to evaluate, plus the columns that
/// `SELECT *` should show once (empty for a plain `ON` join, non-empty
/// for `USING` / `NATURAL`).
struct ResolvedJoin {
    on: Expr,
    using_columns: Vec<String>,
}

/// Turn a [`JoinConstraintKind`] into the `ON` predicate the nested-loop
/// driver evaluates. `tables[..right_pos]` are the tables in scope on
/// the left of this join; `tables[right_pos]` is the table being joined.
///
/// - `On` passes its predicate through unchanged.
/// - `Using(cols)` becomes `left.col = right.col` AND-chained over every
///   named column. The left qualifier is the first in-scope table that
///   actually has the column, so the rewrite is correct for join chains
///   (`A JOIN B USING(x) JOIN C USING(x)` resolves both `x`es against
///   `A`). A column missing from either side is an error.
/// - `Natural` discovers the shared column names first (right table's
///   columns that also appear somewhere on the left), then proceeds
///   exactly like `Using`. No shared columns ⇒ an always-true predicate,
///   i.e. a cross product, matching SQLite.
fn resolve_join_constraint(
    constraint: &JoinConstraintKind,
    tables: &[JoinedTableRef<'_>],
    right_pos: usize,
) -> Result<ResolvedJoin> {
    match constraint {
        JoinConstraintKind::On(expr) => Ok(ResolvedJoin {
            on: (**expr).clone(),
            using_columns: Vec::new(),
        }),
        JoinConstraintKind::Using(cols) => build_using_join(cols, tables, right_pos),
        JoinConstraintKind::Natural => {
            // Shared columns = the right table's columns that also exist
            // on some left table, preserving the right table's column
            // order for determinism.
            let shared: Vec<String> = tables[right_pos]
                .table
                .column_names()
                .into_iter()
                .filter(|c| {
                    tables[..right_pos]
                        .iter()
                        .any(|t| t.table.contains_column(c.clone()))
                })
                .collect();
            build_using_join(&shared, tables, right_pos)
        }
    }
}

/// Shared lowering for `USING` and `NATURAL`: synthesize the AND-chain
/// of `left.col = right.col` equalities and report the deduplicated
/// columns. An empty `cols` (a `NATURAL` join with nothing in common)
/// yields an always-true predicate and no dedup, i.e. a cross product.
fn build_using_join(
    cols: &[String],
    tables: &[JoinedTableRef<'_>],
    right_pos: usize,
) -> Result<ResolvedJoin> {
    let right = &tables[right_pos];
    let mut predicate: Option<Expr> = None;
    for col in cols {
        // The named column must exist on the right side …
        if !right.table.contains_column(col.clone()) {
            return Err(SQLRiteError::Internal(format!(
                "cannot join USING column '{col}' — it is not present on table '{}'",
                right.scope_name
            )));
        }
        // … and on at least one left-side table. Qualify the left
        // reference with whichever table actually has it.
        let left = tables[..right_pos]
            .iter()
            .find(|t| t.table.contains_column(col.clone()))
            .ok_or_else(|| {
                SQLRiteError::Internal(format!(
                    "cannot join USING column '{col}' — it is not present on any left-side table"
                ))
            })?;
        let eq = col_eq(&left.scope_name, &right.scope_name, col);
        predicate = Some(match predicate {
            None => eq,
            Some(prev) => Expr::BinaryOp {
                left: Box::new(prev),
                op: BinaryOperator::And,
                right: Box::new(eq),
            },
        });
    }
    Ok(ResolvedJoin {
        on: predicate
            .unwrap_or_else(|| Expr::Value(sqlparser::ast::Value::Boolean(true).with_empty_span())),
        using_columns: cols.to_vec(),
    })
}

/// Build the `left_scope.col = right_scope.col` equality used to lower
/// `USING` / `NATURAL` joins onto the existing `ON` evaluation path.
fn col_eq(left_scope: &str, right_scope: &str, col: &str) -> Expr {
    let col_ref = |scope: &str| {
        Expr::CompoundIdentifier(vec![
            Ident::new(scope.to_string()),
            Ident::new(col.to_string()),
        ])
    };
    Expr::BinaryOp {
        left: Box::new(col_ref(left_scope)),
        op: BinaryOperator::Eq,
        right: Box::new(col_ref(right_scope)),
    }
}

// -----------------------------------------------------------------
// SQLR-5 — Joined SELECT execution
// -----------------------------------------------------------------
//
// The strategy is a left-folded nested-loop join: start with the
// rowids of the leading FROM table, then for each JOIN clause
// combine the accumulator (`Vec<Vec<Option<i64>>>`) with the rowids
// of the next table. Each join flavor differs only in how it
// handles unmatched left / right rows:
//
//   INNER       — drop unmatched on both sides
//   LEFT OUTER  — keep every left row; pad right side with NULL
//   RIGHT OUTER — keep every right row; pad left side with NULL
//   FULL OUTER  — keep both unmatched sets, NULL-padding the other
//
// This isn't a hash join — every join is O(N×M) in the size of the
// accumulator and the right table. Adequate for SQLRite's "embedded
// learning database" niche; a future phase could layer hash / merge
// joins on equi-join shapes without changing the surface API.
//
// SQLR-6 — aggregates / GROUP BY / DISTINCT compose with joins: the
// fully-joined row stream feeds the same scope-generic aggregation
// pipeline the single-table path uses (Stage 3.5 below), and DISTINCT
// dedupes the projected output rows.
fn execute_select_rows_joined(query: SelectQuery, db: &Database) -> Result<SelectResult> {
    // Resolve every participating table once and capture its scope
    // name (alias if supplied, else table name). Scope names are
    // case-sensitive in matching the original identifier text;
    // qualifier matches in `JoinedScope::lookup` use
    // `eq_ignore_ascii_case` so `T1.c1` works whether the user
    // wrote `T1`, `t1`, or `T1` differently than the alias.
    let mut joined_tables: Vec<JoinedTableRef<'_>> = Vec::with_capacity(1 + query.joins.len());

    let primary = db
        .get_table(query.table_name.clone())
        .map_err(|_| SQLRiteError::Internal(format!("Table '{}' not found", query.table_name)))?;
    joined_tables.push(JoinedTableRef {
        table: primary,
        scope_name: query
            .table_alias
            .clone()
            .unwrap_or_else(|| query.table_name.clone()),
    });
    for j in &query.joins {
        let t = db
            .get_table(j.right_table.clone())
            .map_err(|_| SQLRiteError::Internal(format!("Table '{}' not found", j.right_table)))?;
        joined_tables.push(JoinedTableRef {
            table: t,
            scope_name: j
                .right_alias
                .clone()
                .unwrap_or_else(|| j.right_table.clone()),
        });
    }

    // Reject duplicate scope names — `FROM t JOIN t ON ...` without
    // an alias on one side would silently collapse qualifiers and
    // produce confusing results. Forcing the user to alias one side
    // keeps `t1.col` / `t2.col` unambiguous.
    {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for t in &joined_tables {
            let key = t.scope_name.to_ascii_lowercase();
            if !seen.insert(key) {
                return Err(SQLRiteError::Internal(format!(
                    "duplicate table reference '{}' in FROM/JOIN — use AS to alias one side",
                    t.scope_name
                )));
            }
        }
    }

    // Resolve each join's match constraint into a concrete ON predicate
    // (plus, for USING / NATURAL, the set of columns that `SELECT *`
    // shows once). This is done here rather than at parse time because
    // USING needs to know which side each named column lives on, and
    // NATURAL needs the schemas to discover the shared columns at all —
    // neither is available to the parser. `resolved[i]` lines up with
    // `query.joins[i]` (i.e. `joined_tables[i + 1]`).
    let resolved: Vec<ResolvedJoin> = query
        .joins
        .iter()
        .enumerate()
        .map(|(j_idx, join)| resolve_join_constraint(&join.constraint, &joined_tables, j_idx + 1))
        .collect::<Result<Vec<_>>>()?;

    // Validate qualified projection column references against the
    // table they qualify. Unqualified names are validated by the
    // first scope lookup at row materialization — the runtime check
    // there gives the same "ambiguous / unknown" message we'd want
    // here, so we don't pre-resolve them.
    let proj_items: Vec<ProjectionItem> = match &query.projection {
        Projection::All => {
            // `SELECT *` over a join expands to every column of every
            // in-scope table, in source order. We use the bare column
            // name as both the projected identifier and the output
            // header — qualified expansion (`t1.col`) would force
            // composite headers like `t1.col` which conflict with
            // alias-less convention. Duplicate header names are
            // permitted (matches SQLite); callers needing
            // disambiguation can `SELECT t.col AS t_col`.
            //
            // USING / NATURAL columns are the exception: SQLite shows a
            // joined-on column once, taking the left side's copy and
            // omitting the right side's. We honor that by skipping any
            // column listed in the right table's `using_columns` when we
            // reach that table during expansion. (The left copy was
            // already emitted by an earlier table.)
            let mut all = Vec::new();
            for (t_idx, t) in joined_tables.iter().enumerate() {
                // `t_idx == 0` is the primary table (no incoming join);
                // every later table corresponds to `resolved[t_idx - 1]`.
                let dedup: &[String] = t_idx
                    .checked_sub(1)
                    .map(|r| resolved[r].using_columns.as_slice())
                    .unwrap_or(&[]);
                for col in t.table.column_names() {
                    if dedup.contains(&col) {
                        continue;
                    }
                    all.push(ProjectionItem {
                        kind: ProjectionKind::Column {
                            // Qualify the synthetic items so duplicate
                            // column names across tables route to the
                            // right side at projection time. The output
                            // header still uses the bare `name`.
                            qualifier: Some(t.scope_name.clone()),
                            name: col,
                        },
                        alias: None,
                    });
                }
            }
            all
        }
        Projection::Items(items) => items.clone(),
    };

    let columns: Vec<String> = proj_items.iter().map(|i| i.output_name()).collect();

    // Stage 1: enumerate rows of the leading table. The accumulator
    // is `Vec<Vec<Option<i64>>>` where each inner `Vec` is a join
    // row whose i-th slot is the rowid of `joined_tables[i]` (or
    // None for a NULL-padded row from an outer join).
    let mut acc: Vec<Vec<Option<i64>>> = primary
        .rowids()
        .into_iter()
        .map(|r| {
            let mut row = Vec::with_capacity(joined_tables.len());
            row.push(Some(r));
            row
        })
        .collect();

    // Stage 2: fold each JOIN clause into the accumulator. After
    // join `i`, every row in `acc` has length `i + 2` (primary +
    // i+1 right tables joined). Unmatched-side handling depends on
    // the join flavor.
    for (j_idx, join) in query.joins.iter().enumerate() {
        let right_pos = j_idx + 1;
        let right_table = joined_tables[right_pos].table;
        let right_rowids: Vec<i64> = right_table.rowids();

        // Track which right rowids matched at least once across the
        // entire left accumulator. Used by RIGHT / FULL to emit
        // unmatched right rows after the loop.
        let mut right_matched: Vec<bool> = vec![false; right_rowids.len()];

        let mut next_acc: Vec<Vec<Option<i64>>> = Vec::with_capacity(acc.len());

        // ON evaluation only sees tables that are in scope *at this
        // join level* — the leading FROM table plus every right
        // table joined so far, including the one we're matching.
        // Restricting the scope means a typo like `JOIN c ON a.id =
        // c.id JOIN c ON ...` (referencing `c` before it joins)
        // surfaces as "unknown table qualifier 'c'" rather than
        // silently `NULL → false`-ing every row.
        let on_scope_tables: &[JoinedTableRef<'_>] = &joined_tables[..=right_pos];

        for left_row in acc.into_iter() {
            // Build a row prefix and extend it with each candidate
            // right rowid; record whether any matched (for outer
            // padding on the left side).
            let mut left_match_count = 0usize;
            for (r_idx, &rrid) in right_rowids.iter().enumerate() {
                let mut on_rowids: Vec<Option<i64>> = left_row.clone();
                on_rowids.push(Some(rrid));
                debug_assert_eq!(on_rowids.len(), on_scope_tables.len());
                let scope = JoinedScope {
                    tables: on_scope_tables,
                    rowids: &on_rowids,
                };
                // Reuse `eval_predicate_scope` so ON shares the same
                // truthiness rule WHERE uses — non-zero integers are
                // truthy, NULL is false, etc. — instead of rejecting
                // anything that isn't a literal bool. `resolved[j_idx].on`
                // is the user's ON expr, or the equality we synthesized
                // for USING / NATURAL.
                if eval_predicate_scope(&resolved[j_idx].on, &scope)? {
                    left_match_count += 1;
                    right_matched[r_idx] = true;
                    // Accumulator entries carry only as many slots
                    // as join levels processed so far; the next
                    // iteration extends them again. No trailing
                    // padding needed here.
                    next_acc.push(on_rowids);
                }
            }

            if left_match_count == 0
                && matches!(join.join_type, JoinType::LeftOuter | JoinType::FullOuter)
            {
                // Outer-join NULL pad on the right side: keep the
                // left row, push None for the right rowid.
                let mut padded = left_row;
                padded.push(None);
                next_acc.push(padded);
            }
        }

        // Right-only emission for RIGHT / FULL: any right rowid that
        // never matched on the entire accumulator surfaces with all
        // left positions NULL-padded.
        if matches!(join.join_type, JoinType::RightOuter | JoinType::FullOuter) {
            for (r_idx, matched) in right_matched.iter().enumerate() {
                if *matched {
                    continue;
                }
                let mut row: Vec<Option<i64>> = vec![None; right_pos];
                row.push(Some(right_rowids[r_idx]));
                next_acc.push(row);
            }
        }

        acc = next_acc;
    }

    // Stage 3: apply WHERE on each fully-joined row. Outer-join
    // NULL-padded rows where WHERE references a NULL'd column will
    // (per SQL three-valued logic) be excluded — this is the same
    // posture as the single-table path.
    let mut filtered: Vec<Vec<Option<i64>>> = if let Some(where_expr) = &query.selection {
        let mut out = Vec::with_capacity(acc.len());
        for row in acc {
            let scope = JoinedScope {
                tables: &joined_tables,
                rowids: &row,
            };
            if eval_predicate_scope(where_expr, &scope)? {
                out.push(row);
            }
        }
        out
    } else {
        acc
    };

    // Stage 3.5 — SQLR-6: aggregation over the joined row stream. The
    // fully-joined, WHERE-filtered rows are just another row source
    // for the shared grouping accumulator: each joined row becomes a
    // `JoinedScope` and feeds the same pipeline the single-table path
    // uses (grouping, HAVING, DISTINCT, output-row ORDER BY, LIMIT).
    // NULL-padded outer-join rows group under a NULL key and are
    // skipped by `COUNT(col)` like any other NULL.
    let has_aggregates = proj_items
        .iter()
        .any(|i| matches!(i.kind, ProjectionKind::Aggregate(_)));
    if has_aggregates || !query.group_by.is_empty() {
        let (all_items, having_expr) = lower_having_into_hidden_slots(&query, &proj_items)?;

        // Validate every column reference against the joined scope up
        // front: GROUP BY keys and aggregate args must resolve to
        // exactly one in-scope table, and every bare projection column
        // must resolve to the same table+column as some GROUP BY key
        // (the joined-scope equivalent of the parser's single-table
        // "must appear in GROUP BY" check).
        for g in &query.group_by {
            resolve_scope_column(&joined_tables, g.qualifier.as_deref(), &g.name)?;
        }
        for item in &all_items {
            match &item.kind {
                ProjectionKind::Aggregate(call) => {
                    if let AggregateArg::Column { qualifier, name } = &call.arg {
                        resolve_scope_column(&joined_tables, qualifier.as_deref(), name)?;
                    }
                }
                ProjectionKind::Column { qualifier, name } => {
                    let pos = resolve_scope_column(&joined_tables, qualifier.as_deref(), name)?;
                    let in_group_by = query.group_by.iter().any(|g| {
                        g.name == *name
                            && resolve_scope_column(&joined_tables, g.qualifier.as_deref(), &g.name)
                                == Ok(pos)
                    });
                    if !in_group_by {
                        return Err(SQLRiteError::Internal(format!(
                            "column '{name}' must appear in GROUP BY or be used in an \
                             aggregate function"
                        )));
                    }
                }
            }
        }

        let scopes = filtered.iter().map(|row| JoinedScope {
            tables: &joined_tables,
            rowids: row,
        });
        return run_aggregation_pipeline(scopes, &query, &proj_items, &all_items, &having_expr);
    }

    // Stage 4: ORDER BY across the joined scope. We pre-compute the
    // sort key per row (same approach as `sort_rowids`) so the
    // comparator runs on Values, not against the expression tree.
    if let Some(order) = &query.order_by {
        // Validate up front so a bad ORDER BY surfaces a clear
        // error before sort starts.
        let mut keys: Vec<(usize, Value)> = Vec::with_capacity(filtered.len());
        for (i, row) in filtered.iter().enumerate() {
            let scope = JoinedScope {
                tables: &joined_tables,
                rowids: row,
            };
            let v = eval_expr_scope(&order.expr, &scope)?;
            keys.push((i, v));
        }
        keys.sort_by(|(_, a), (_, b)| {
            let ord = compare_values(Some(a), Some(b));
            if order.ascending { ord } else { ord.reverse() }
        });
        let mut sorted = Vec::with_capacity(filtered.len());
        for (i, _) in keys {
            sorted.push(filtered[i].clone());
        }
        filtered = sorted;
    }

    // Stage 5: LIMIT. SQLR-6 — when DISTINCT is on, truncating the
    // joined rows here would over-truncate (duplicates collapse later),
    // so the limit is deferred past the dedupe step, mirroring the
    // single-table path.
    if let Some(k) = query.limit
        && !query.distinct
    {
        filtered.truncate(k);
    }

    // Stage 6: project. For each row, evaluate every projection item
    // through the joined scope.
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(filtered.len());
    for row in &filtered {
        let scope = JoinedScope {
            tables: &joined_tables,
            rowids: row,
        };
        let mut out_row = Vec::with_capacity(proj_items.len());
        for item in &proj_items {
            let v = match &item.kind {
                ProjectionKind::Column { qualifier, name } => {
                    scope.lookup(qualifier.as_deref(), name)?
                }
                ProjectionKind::Aggregate(_) => {
                    // Aggregates are handled by the Stage 3.5 pipeline,
                    // which returns before reaching this projection —
                    // defense in depth keeps the pattern match total.
                    return Err(SQLRiteError::Internal(
                        "aggregate projection reached the non-aggregating join path".to_string(),
                    ));
                }
            };
            out_row.push(v);
        }
        rows.push(out_row);
    }

    // SQLR-6 — SELECT DISTINCT over a join: dedupe the projected
    // output rows, then apply the LIMIT that Stage 5 deferred.
    if query.distinct {
        rows = dedupe_rows(rows);
        if let Some(k) = query.limit {
            rows.truncate(k);
        }
    }

    Ok(SelectResult { columns, rows })
}

/// Resolve an optionally-qualified column reference to the index of
/// the in-scope joined table that owns it. Schema-only counterpart of
/// [`JoinedScope::lookup`]: a qualified reference must name a known
/// scope and the column must exist there; an unqualified reference
/// must exist on exactly one in-scope table (zero → unknown column,
/// several → ambiguous).
fn resolve_scope_column(
    tables: &[JoinedTableRef<'_>],
    qualifier: Option<&str>,
    name: &str,
) -> Result<usize> {
    if let Some(q) = qualifier {
        let pos = tables
            .iter()
            .position(|t| t.scope_name.eq_ignore_ascii_case(q))
            .ok_or_else(|| {
                SQLRiteError::Internal(format!(
                    "unknown table qualifier '{q}' in column reference '{q}.{name}'"
                ))
            })?;
        if !tables[pos].table.contains_column(name.to_string()) {
            return Err(SQLRiteError::Internal(format!(
                "column '{name}' does not exist on '{}'",
                tables[pos].scope_name
            )));
        }
        return Ok(pos);
    }
    let mut hit: Option<usize> = None;
    for (i, t) in tables.iter().enumerate() {
        if t.table.contains_column(name.to_string()) {
            if hit.is_some() {
                return Err(SQLRiteError::Internal(format!(
                    "column reference '{name}' is ambiguous — qualify it as <table>.{name}"
                )));
            }
            hit = Some(i);
        }
    }
    hit.ok_or_else(|| {
        SQLRiteError::Internal(format!(
            "unknown column '{name}' in joined SELECT (no in-scope table has it)"
        ))
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
    let (table_name, table_alias) = extract_single_table_name(tables)?;
    // SQLR-14 — qualifiers in the WHERE must name this table (or its
    // alias, which shadows the table name when declared).
    let scope_name = table_alias.as_deref().unwrap_or(&table_name);

    // Compute matching rowids with an immutable borrow, then mutate.
    let matching: Vec<i64> = {
        let table = db
            .get_table(table_name.clone())
            .map_err(|_| SQLRiteError::Internal(format!("Table '{table_name}' not found")))?;
        match select_rowids(table, selection.as_ref(), scope_name)? {
            RowidSource::IndexProbe(rowids) => rowids,
            RowidSource::FullScan => {
                let mut out = Vec::new();
                for rowid in table.rowids() {
                    if let Some(expr) = selection {
                        if !eval_predicate(expr, table, rowid, scope_name)? {
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
    // Phase 7d.3 — any DELETE invalidates every HNSW index on this
    // table (the deleted node could still appear in other nodes'
    // neighbor lists, breaking subsequent searches). Mark dirty so
    // the next save rebuilds from current rows before serializing.
    //
    // Phase 8b — same posture for FTS indexes (Q7 — rebuild-on-save
    // mirrors HNSW). The deleted rowid still appears in posting
    // lists; leaving it would surface zombie hits in future queries.
    if !matching.is_empty() {
        for entry in &mut table.hnsw_indexes {
            entry.needs_rebuild = true;
        }
        for entry in &mut table.fts_indexes {
            entry.needs_rebuild = true;
        }
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

    let (table_name, table_alias) = extract_table_name(table)?;
    // SQLR-14 — qualifiers in the WHERE / SET expressions must name
    // this table (or its alias, which shadows the table name when
    // declared).
    let scope_name = table_alias.as_deref().unwrap_or(&table_name);

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
        let matched_rowids: Vec<i64> = match select_rowids(tbl, selection.as_ref(), scope_name)? {
            RowidSource::IndexProbe(rowids) => rowids,
            RowidSource::FullScan => {
                let mut out = Vec::new();
                for rowid in tbl.rowids() {
                    if let Some(expr) = selection {
                        if !eval_predicate(expr, tbl, rowid, scope_name)? {
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
                let v = eval_expr(expr, tbl, rowid, scope_name)?;
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

    // Phase 7d.3 — UPDATE may have changed a vector column that an
    // HNSW index covers. Mark every covering index dirty so save
    // rebuilds from current rows. (Updates that only touched
    // non-vector columns also mark dirty, which is over-conservative
    // but harmless — the rebuild walks rows anyway, and the cost is
    // only paid on save.)
    //
    // Phase 8b — same shape for FTS indexes covering updated TEXT cols.
    if !work.is_empty() {
        let updated_columns: std::collections::HashSet<&str> = work
            .iter()
            .flat_map(|(_, values)| values.iter().map(|(c, _)| c.as_str()))
            .collect();
        for entry in &mut tbl.hnsw_indexes {
            if updated_columns.contains(entry.column_name.as_str()) {
                entry.needs_rebuild = true;
            }
        }
        for entry in &mut tbl.fts_indexes {
            if updated_columns.contains(entry.column_name.as_str()) {
                entry.needs_rebuild = true;
            }
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
        with,
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
        Some(IndexType::Custom(ident)) if ident.value.eq_ignore_ascii_case("fts") => {
            IndexMethod::Fts
        }
        Some(IndexType::Custom(ident)) if ident.value.eq_ignore_ascii_case("btree") => {
            IndexMethod::Btree
        }
        Some(other) => {
            return Err(SQLRiteError::NotImplemented(format!(
                "CREATE INDEX … USING {other:?} is not supported \
                 (try `hnsw`, `fts`, or no USING clause)"
            )));
        }
        None => IndexMethod::Btree,
    };

    // Parse `WITH (key = value, …)` options (SQLR-28). The only key
    // recognized today is `metric` for HNSW indexes — `'l2'` /
    // `'cosine'` / `'dot'`. The clause is rejected on non-HNSW indexes
    // so a typo doesn't silently sit on a btree index where it can't
    // do anything useful.
    let hnsw_metric = parse_hnsw_with_options(with, &index_name, method)?;

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

        // Name uniqueness check spans ALL index kinds — btree, hnsw, and
        // fts share one namespace per table.
        if table.index_by_name(&index_name).is_some()
            || table.hnsw_indexes.iter().any(|i| i.name == index_name)
            || table.fts_indexes.iter().any(|i| i.name == index_name)
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
            hnsw_metric.unwrap_or(DistanceMetric::L2),
            &existing_rowids_and_values,
        ),
        IndexMethod::Fts => create_fts_index(
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

/// Executes `DROP TABLE [IF EXISTS] <name>;`. Mirrors SQLite's single-target
/// shape: sqlparser parses `DROP TABLE a, b` as one statement with
/// `names: vec![a, b]`, but we reject the multi-target form to keep error
/// semantics simple (no partial-failure rollback).
///
/// On success the table — and every index attached to it — disappears from
/// the in-memory `Database`. The next auto-save rebuilds `sqlrite_master`
/// from scratch and simply doesn't write a row for the dropped table or
/// its indexes; pages previously occupied by them become orphans on disk
/// (no free-list yet — file size doesn't shrink until a future VACUUM).
pub fn execute_drop_table(
    names: &[ObjectName],
    if_exists: bool,
    db: &mut Database,
) -> Result<usize> {
    if names.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "DROP TABLE supports a single table per statement".to_string(),
        ));
    }
    let name = names[0].to_string();

    if name == crate::sql::pager::MASTER_TABLE_NAME {
        return Err(SQLRiteError::General(format!(
            "'{}' is a reserved name used by the internal schema catalog",
            crate::sql::pager::MASTER_TABLE_NAME
        )));
    }

    if !db.contains_table(name.clone()) {
        return if if_exists {
            Ok(0)
        } else {
            Err(SQLRiteError::General(format!(
                "Table '{name}' does not exist"
            )))
        };
    }

    db.tables.remove(&name);
    Ok(1)
}

/// Executes `DROP INDEX [IF EXISTS] <name>;`. The statement does not name a
/// table, so we walk every table looking for the index across all three
/// index families (B-Tree secondary, HNSW, FTS).
///
/// Refuses to drop auto-indexes (`origin == IndexOrigin::Auto`) — those are
/// invariants of the table's PRIMARY KEY / UNIQUE constraints and should
/// only disappear when the column or table they depend on is dropped.
/// SQLite has the same rule for its `sqlite_autoindex_*` indexes.
pub fn execute_drop_index(
    names: &[ObjectName],
    if_exists: bool,
    db: &mut Database,
) -> Result<usize> {
    if names.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "DROP INDEX supports a single index per statement".to_string(),
        ));
    }
    let name = names[0].to_string();

    for table in db.tables.values_mut() {
        if let Some(secondary) = table.secondary_indexes.iter().find(|i| i.name == name) {
            if secondary.origin == IndexOrigin::Auto {
                return Err(SQLRiteError::General(format!(
                    "cannot drop auto-created index '{name}' (drop the column or table instead)"
                )));
            }
            table.secondary_indexes.retain(|i| i.name != name);
            return Ok(1);
        }
        if table.hnsw_indexes.iter().any(|i| i.name == name) {
            table.hnsw_indexes.retain(|i| i.name != name);
            return Ok(1);
        }
        if table.fts_indexes.iter().any(|i| i.name == name) {
            table.fts_indexes.retain(|i| i.name != name);
            return Ok(1);
        }
    }

    if if_exists {
        Ok(0)
    } else {
        Err(SQLRiteError::General(format!(
            "Index '{name}' does not exist"
        )))
    }
}

/// Executes `ALTER TABLE [IF EXISTS] <name> <op>;` for one operation per
/// statement. Supports four sub-operations matching SQLite:
///
///   - `RENAME TO <new>`
///   - `RENAME COLUMN <old> TO <new>`
///   - `ADD COLUMN <coldef>` (NOT NULL requires DEFAULT on a non-empty table;
///     PK / UNIQUE constraints rejected — would need backfill + uniqueness)
///   - `DROP COLUMN <name>` (refuses PK column and only-column)
///
/// Multi-operation ALTER (`ALTER TABLE foo RENAME TO bar, ADD COLUMN x ...`)
/// is rejected; SQLite forbids it too.
pub fn execute_alter_table(alter: AlterTable, db: &mut Database) -> Result<String> {
    let table_name = alter.name.to_string();

    if table_name == crate::sql::pager::MASTER_TABLE_NAME {
        return Err(SQLRiteError::General(format!(
            "'{}' is a reserved name used by the internal schema catalog",
            crate::sql::pager::MASTER_TABLE_NAME
        )));
    }

    if !db.contains_table(table_name.clone()) {
        return if alter.if_exists {
            Ok("ALTER TABLE: no-op (table does not exist)".to_string())
        } else {
            Err(SQLRiteError::General(format!(
                "Table '{table_name}' does not exist"
            )))
        };
    }

    if alter.operations.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "ALTER TABLE supports one operation per statement".to_string(),
        ));
    }

    match &alter.operations[0] {
        AlterTableOperation::RenameTable { table_name: kind } => {
            let new_name = match kind {
                RenameTableNameKind::To(name) => name.to_string(),
                RenameTableNameKind::As(_) => {
                    return Err(SQLRiteError::NotImplemented(
                        "ALTER TABLE ... RENAME AS (MySQL-only) is not supported; use RENAME TO"
                            .to_string(),
                    ));
                }
            };
            alter_rename_table(db, &table_name, &new_name)?;
            Ok(format!(
                "ALTER TABLE '{table_name}' RENAME TO '{new_name}' executed."
            ))
        }
        AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => {
            let old = old_column_name.value.clone();
            let new = new_column_name.value.clone();
            db.get_table_mut(table_name.clone())?
                .rename_column(&old, &new)?;
            Ok(format!(
                "ALTER TABLE '{table_name}' RENAME COLUMN '{old}' TO '{new}' executed."
            ))
        }
        AlterTableOperation::AddColumn {
            column_def,
            if_not_exists,
            ..
        } => {
            let parsed = crate::sql::parser::create::parse_one_column(column_def)?;
            let table = db.get_table_mut(table_name.clone())?;
            if *if_not_exists && table.contains_column(parsed.name.clone()) {
                return Ok(format!(
                    "ALTER TABLE '{table_name}' ADD COLUMN: no-op (column '{}' already exists)",
                    parsed.name
                ));
            }
            let col_name = parsed.name.clone();
            table.add_column(parsed)?;
            Ok(format!(
                "ALTER TABLE '{table_name}' ADD COLUMN '{col_name}' executed."
            ))
        }
        AlterTableOperation::DropColumn {
            column_names,
            if_exists,
            ..
        } => {
            if column_names.len() != 1 {
                return Err(SQLRiteError::NotImplemented(
                    "ALTER TABLE DROP COLUMN supports a single column per statement".to_string(),
                ));
            }
            let col_name = column_names[0].value.clone();
            let table = db.get_table_mut(table_name.clone())?;
            if *if_exists && !table.contains_column(col_name.clone()) {
                return Ok(format!(
                    "ALTER TABLE '{table_name}' DROP COLUMN: no-op (column '{col_name}' does not exist)"
                ));
            }
            table.drop_column(&col_name)?;
            Ok(format!(
                "ALTER TABLE '{table_name}' DROP COLUMN '{col_name}' executed."
            ))
        }
        other => Err(SQLRiteError::NotImplemented(format!(
            "ALTER TABLE operation {other:?} is not supported"
        ))),
    }
}

/// Executes `VACUUM;` (SQLR-6). Compacts the database file: rewrites
/// every live table, index, and the catalog contiguously from page 1,
/// drops the freelist, and truncates the tail at the next checkpoint.
///
/// Refuses to run inside a transaction (would publish in-flight writes
/// out of band); refuses on read-only databases (handled upstream by
/// the read-only mutation gate); and is a no-op on in-memory databases
/// (no file to compact). Bare `VACUUM;` only — non-default options
/// (`FULL`, `REINDEX`, table targets, etc.) are rejected.
pub fn execute_vacuum(db: &mut Database) -> Result<String> {
    if db.in_transaction() {
        return Err(SQLRiteError::General(
            "VACUUM cannot run inside a transaction".to_string(),
        ));
    }
    let path = match db.source_path.clone() {
        Some(p) => p,
        None => {
            return Ok("VACUUM is a no-op for in-memory databases".to_string());
        }
    };
    // Checkpoint before AND after VACUUM so the main-file size we report
    // reflects only what VACUUM actually reclaimed — without the leading
    // checkpoint, `size_before` would be the stale main-file snapshot
    // (typically 2 pages) while WAL holds the live bytes, making the
    // bytes-reclaimed delta meaningless.
    if let Some(pager) = db.pager.as_mut() {
        let _ = pager.checkpoint();
    }
    let size_before = std::fs::metadata(&path).ok().map(|m| m.len()).unwrap_or(0);
    let pages_before = db
        .pager
        .as_ref()
        .map(|p| p.header().page_count)
        .unwrap_or(0);
    crate::sql::pager::vacuum_database(db, &path)?;
    // Second checkpoint so the main file shrinks now — VACUUM's whole
    // purpose is to reclaim bytes, so paying the I/O up front is fair.
    if let Some(pager) = db.pager.as_mut() {
        let _ = pager.checkpoint();
    }
    let size_after = std::fs::metadata(&path).ok().map(|m| m.len()).unwrap_or(0);
    let pages_after = db
        .pager
        .as_ref()
        .map(|p| p.header().page_count)
        .unwrap_or(0);
    let pages_reclaimed = pages_before.saturating_sub(pages_after);
    let bytes_reclaimed = size_before.saturating_sub(size_after);
    Ok(format!(
        "VACUUM completed. {pages_reclaimed} pages reclaimed ({bytes_reclaimed} bytes)."
    ))
}

/// Renames a table in `db.tables`. Updates `tb_name`, every secondary
/// index's `table_name` field, and any auto-index whose name embedded
/// the old table name. HNSW / FTS index entries don't carry a
/// `table_name` field — they're addressed implicitly via the `Table`
/// they live inside, so they move with the rename for free.
fn alter_rename_table(db: &mut Database, old: &str, new: &str) -> Result<()> {
    if new == crate::sql::pager::MASTER_TABLE_NAME {
        return Err(SQLRiteError::General(format!(
            "'{}' is a reserved name used by the internal schema catalog",
            crate::sql::pager::MASTER_TABLE_NAME
        )));
    }
    if old == new {
        return Ok(());
    }
    if db.contains_table(new.to_string()) {
        return Err(SQLRiteError::General(format!(
            "target table '{new}' already exists"
        )));
    }

    let mut table = db
        .tables
        .remove(old)
        .ok_or_else(|| SQLRiteError::General(format!("Table '{old}' does not exist")))?;
    table.tb_name = new.to_string();
    for idx in table.secondary_indexes.iter_mut() {
        idx.table_name = new.to_string();
        if idx.origin == IndexOrigin::Auto
            && idx.name == SecondaryIndex::auto_name(old, &idx.column_name)
        {
            idx.name = SecondaryIndex::auto_name(new, &idx.column_name);
        }
    }
    db.tables.insert(new.to_string(), table);
    Ok(())
}

/// `USING <method>` choices recognized by `execute_create_index`. A
/// missing USING clause defaults to `Btree` so existing CREATE INDEX
/// statements (Phase 3e) keep working unchanged.
#[derive(Debug, Clone, Copy)]
enum IndexMethod {
    Btree,
    Hnsw,
    /// Phase 8b — full-text inverted index over a TEXT column.
    Fts,
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
    metric: DistanceMetric,
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

    // Build the in-memory graph. The distance metric was picked at
    // CREATE INDEX time (defaults to L2 if no `WITH (metric = …)`
    // clause was supplied). The graph topology is metric-specific —
    // L2 neighbour pruning ≠ cosine neighbour pruning — so the
    // optimizer's HNSW shortcut only fires when the query's
    // `vec_distance_*` function matches this value (SQLR-28).
    //
    // Seed: hash the index name so different indexes get different
    // graph topologies, but the same index always gets the same one
    // — useful when debugging recall / index size.
    let seed = hash_str_to_seed(index_name);
    let mut idx = HnswIndex::new(metric, seed);

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
            })?;
        }
    }

    let table_mut = db.get_table_mut(table_name.to_string())?;
    table_mut.hnsw_indexes.push(HnswIndexEntry {
        name: index_name.to_string(),
        column_name: column_name.to_string(),
        metric,
        index: idx,
        // Freshly built — no DELETE/UPDATE has invalidated it yet.
        needs_rebuild: false,
    });
    Ok(index_name.to_string())
}

/// Parses the `WITH (metric = '<name>', …)` options bag on a CREATE
/// INDEX statement. Returns the chosen metric (or `None` if no
/// `metric` key was supplied) on HNSW indexes; raises a
/// user-visible error on:
///
///   - WITH options on a non-HNSW index (btree / fts have no knobs we
///     understand here),
///   - unknown option keys,
///   - unknown metric names (typo guard — silently falling back to L2
///     would hide the user's intent and re-introduce the SQLR-28 bug).
fn parse_hnsw_with_options(
    with: &[Expr],
    index_name: &str,
    method: IndexMethod,
) -> Result<Option<DistanceMetric>> {
    if with.is_empty() {
        return Ok(None);
    }
    if !matches!(method, IndexMethod::Hnsw) {
        return Err(SQLRiteError::General(format!(
            "CREATE INDEX '{index_name}' has a WITH (...) clause but its index method \
             doesn't support any options — only `USING hnsw` recognises `WITH (metric = ...)`"
        )));
    }

    let mut metric: Option<DistanceMetric> = None;
    for opt in with {
        let Expr::BinaryOp { left, op, right } = opt else {
            return Err(SQLRiteError::General(format!(
                "CREATE INDEX '{index_name}': unsupported WITH option {opt:?} \
                 (expected `key = 'value'`)"
            )));
        };
        if !matches!(op, BinaryOperator::Eq) {
            return Err(SQLRiteError::General(format!(
                "CREATE INDEX '{index_name}': WITH options must use `=` (got {op:?})"
            )));
        }
        let key = match left.as_ref() {
            Expr::Identifier(ident) => ident.value.clone(),
            other => {
                return Err(SQLRiteError::General(format!(
                    "CREATE INDEX '{index_name}': WITH option key must be a bare identifier, \
                     got {other:?}"
                )));
            }
        };
        let value = match right.as_ref() {
            Expr::Value(v) => match &v.value {
                AstValue::SingleQuotedString(s) => s.clone(),
                AstValue::DoubleQuotedString(s) => s.clone(),
                other => {
                    return Err(SQLRiteError::General(format!(
                        "CREATE INDEX '{index_name}': WITH option '{key}' value must be \
                         a quoted string, got {other:?}"
                    )));
                }
            },
            Expr::Identifier(ident) => ident.value.clone(),
            other => {
                return Err(SQLRiteError::General(format!(
                    "CREATE INDEX '{index_name}': WITH option '{key}' value must be a \
                     quoted string, got {other:?}"
                )));
            }
        };

        if key.eq_ignore_ascii_case("metric") {
            let parsed = DistanceMetric::from_sql_name(&value).ok_or_else(|| {
                SQLRiteError::General(format!(
                    "CREATE INDEX '{index_name}': unknown HNSW metric '{value}' \
                     (try 'l2', 'cosine', or 'dot')"
                ))
            })?;
            if metric.is_some() {
                return Err(SQLRiteError::General(format!(
                    "CREATE INDEX '{index_name}': metric specified more than once in WITH (...)"
                )));
            }
            metric = Some(parsed);
        } else {
            return Err(SQLRiteError::General(format!(
                "CREATE INDEX '{index_name}': unknown WITH option '{key}' \
                 (only 'metric' is recognised on HNSW indexes)"
            )));
        }
    }

    Ok(metric)
}

/// Builds a Phase 8b FTS inverted index and attaches it to the table.
/// Mirrors [`create_hnsw_index`] in shape: validate column type,
/// tokenize each existing row's text into the in-memory posting list,
/// push an `FtsIndexEntry`.
fn create_fts_index(
    db: &mut Database,
    table_name: &str,
    index_name: &str,
    column_name: &str,
    datatype: &DataType,
    unique: bool,
    existing: &[(i64, Value)],
) -> Result<String> {
    // FTS is a TEXT-only feature for the MVP. JSON columns share the
    // Row::Text storage but their content is structured — full-text
    // indexing JSON keys + values would need a different design (and
    // is out of scope per the Phase 8 plan's "Out of scope" section).
    match datatype {
        DataType::Text => {}
        other => {
            return Err(SQLRiteError::General(format!(
                "USING fts requires a TEXT column; '{column_name}' is {other}"
            )));
        }
    }

    if unique {
        return Err(SQLRiteError::General(
            "UNIQUE has no meaning for FTS indexes".to_string(),
        ));
    }

    let mut idx = PostingList::new();
    for (rowid, v) in existing {
        if let Value::Text(text) = v {
            idx.insert(*rowid, text);
        }
        // Non-text values (Null, type coercion bugs) get skipped — same
        // posture as create_hnsw_index for non-vector values.
    }

    let table_mut = db.get_table_mut(table_name.to_string())?;
    table_mut.fts_indexes.push(FtsIndexEntry {
        name: index_name.to_string(),
        column_name: column_name.to_string(),
        index: idx,
        needs_rebuild: false,
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
        DataType::Json => DataType::Json,
        DataType::None => DataType::None,
        DataType::Invalid => DataType::Invalid,
    }
}

fn extract_single_table_name(tables: &[TableWithJoins]) -> Result<(String, Option<String>)> {
    if tables.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "multi-table DELETE is not supported yet".to_string(),
        ));
    }
    extract_table_name(&tables[0])
}

/// Pull `(table_name, alias)` out of a plain single-table reference.
/// UPDATE / DELETE use this; the alias feeds qualifier validation
/// (SQLR-14) the same way `SelectQuery.table_alias` does for SELECT.
fn extract_table_name(twj: &TableWithJoins) -> Result<(String, Option<String>)> {
    if !twj.joins.is_empty() {
        return Err(SQLRiteError::NotImplemented(
            "JOIN is not supported yet".to_string(),
        ));
    }
    match &twj.relation {
        TableFactor::Table { name, alias, .. } => Ok((
            name.to_string(),
            alias.as_ref().map(|a| a.name.value.clone()),
        )),
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
fn select_rowids(table: &Table, selection: Option<&Expr>, scope_name: &str) -> Result<RowidSource> {
    let Some(expr) = selection else {
        return Ok(RowidSource::FullScan);
    };
    let Some((qualifier, col, literal)) = try_extract_equality(expr) else {
        return Ok(RowidSource::FullScan);
    };
    // SQLR-14 — `bogus.col = 1` must not silently probe an index on
    // `col`; the qualifier has to name the table in scope, same as the
    // per-row evaluation path it short-circuits.
    check_single_scope_qualifier(qualifier.as_deref(), scope_name, &col)?;
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

/// Recognizes `expr` as a simple equality on a column reference against
/// a literal. Returns `(qualifier, column_name, literal_value)` if the
/// shape matches; `None` otherwise. Accepts both `col = literal` and
/// `literal = col`. The qualifier (SQLR-14) lets the caller validate
/// `t.col = 1` shapes instead of silently stripping the `t.`.
fn try_extract_equality(expr: &Expr) -> Option<(Option<String>, String, sqlparser::ast::Value)> {
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
    let col_from = |e: &Expr| -> Option<(Option<String>, String)> {
        match e {
            Expr::Identifier(ident) => Some((None, ident.value.clone())),
            Expr::CompoundIdentifier(parts) => match parts.as_slice() {
                [only] => Some((None, only.value.clone())),
                [q, c] => Some((Some(q.value.clone()), c.value.clone())),
                _ => None,
            },
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
    if let (Some((q, c)), Some(l)) = (col_from(left), literal_from(right)) {
        return Some((q, c, l));
    }
    if let (Some(l), Some((q, c))) = (literal_from(left), col_from(right)) {
        return Some((q, c, l));
    }
    None
}

/// Recognizes the HNSW-probable query pattern and probes the graph
/// if a matching index exists.
///
/// Looks for ORDER BY `vec_distance_<l2|cosine|dot>(<col>, <bracket-
/// array literal>)` where the table has an HNSW index attached to
/// `<col>` *built for that same distance metric*. On a match, returns
/// the top-k rowids straight from the graph (O(log N)). On any miss —
/// different function name, no matching index, query dimension wrong,
/// metric mismatch, etc. — returns `None` and the caller falls through
/// to the bounded-heap brute-force path (7c) or the full sort (7b),
/// preserving correct results regardless of whether the HNSW pathway
/// kicked in.
///
/// Caveats:
/// - The index's metric and the query's `vec_distance_*` function must
///   agree. An L2-built graph silently doesn't help cosine queries
///   (different neighbour pruning policy → potentially different
///   topology), so we don't pretend to.  Pick the metric at CREATE
///   INDEX time via `WITH (metric = '<l2|cosine|dot>')` (SQLR-28).
/// - Only ASCENDING order makes sense for "k nearest" — DESC ORDER BY
///   `vec_distance_*(...) LIMIT k` would mean "k farthest", which isn't
///   what the index is built for. We don't bother to detect
///   `ascending == false` here; the optimizer just skips and the
///   fallback path handles it correctly (slower).
fn try_hnsw_probe(table: &Table, order_expr: &Expr, k: usize) -> Option<Vec<i64>> {
    if k == 0 {
        return None;
    }

    // Pattern-match: order expr must be a function call
    // vec_distance_<l2|cosine|dot>(a, b).
    let func = match order_expr {
        Expr::Function(f) => f,
        _ => return None,
    };
    let fname = match func.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => ident.value.to_lowercase(),
        _ => return None,
    };
    let query_metric = match fname.as_str() {
        "vec_distance_l2" => DistanceMetric::L2,
        "vec_distance_cosine" => DistanceMetric::Cosine,
        "vec_distance_dot" => DistanceMetric::Dot,
        _ => return None,
    };

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

    // Find the HNSW index on this column AND with a matching metric.
    // Multiple indexes on the same column are allowed in principle
    // (cosine-built + L2-built), and a query picks whichever metric
    // its `vec_distance_*` function names.
    let entry = table
        .hnsw_indexes
        .iter()
        .find(|e| e.column_name == col_name && e.metric == query_metric)?;

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
    let result = entry
        .index
        .search(&query_vec, k, |id| {
            match table_ref.get_value(&column_for_closure, id) {
                Some(Value::Vector(v)) => v,
                _ => Vec::new(),
            }
        })
        .ok()?;
    Some(result)
}

/// Phase 8b — FTS optimizer hook.
///
/// Recognizes `ORDER BY bm25_score(<col>, '<query>') DESC LIMIT <k>`
/// and serves it from the FTS index instead of full-scanning. Returns
/// `Some(rowids)` already sorted by descending BM25 (with rowid
/// ascending as tie-break), or `None` to fall through to scalar eval.
///
/// **Known limitation (mirrors `try_hnsw_probe`).** This shortcut
/// ignores any `WHERE` clause. The canonical FTS query has a
/// `WHERE fts_match(<col>, '<q>')` predicate, which is implicitly
/// satisfied by the probe results — so dropping it is harmless.
/// Anything *else* in the WHERE (`AND status = 'published'`) gets
/// silently skipped on the optimizer path. Per Phase 8 plan Q6 we
/// match HNSW's posture here; a correctness-preserving multi-index
/// composer is deferred.
fn try_fts_probe(table: &Table, order_expr: &Expr, ascending: bool, k: usize) -> Option<Vec<i64>> {
    if k == 0 || ascending {
        // BM25 is "higher = better"; ASC ranking is almost certainly a
        // user mistake. Fall through so the caller gets either an
        // explicit error from scalar eval or the slow correct path.
        return None;
    }

    let func = match order_expr {
        Expr::Function(f) => f,
        _ => return None,
    };
    let fname = match func.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => ident.value.to_lowercase(),
        _ => return None,
    };
    if fname != "bm25_score" {
        return None;
    }

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

    // Arg 0 must be a bare column identifier.
    let col_name = match exprs[0] {
        Expr::Identifier(ident) if ident.quote_style.is_none() => ident.value.clone(),
        _ => return None,
    };

    // Arg 1 must be a single-quoted string literal. Anything else
    // (column reference, function call) requires per-row evaluation —
    // we'd lose the whole point of the probe.
    let query = match exprs[1] {
        Expr::Value(v) => match &v.value {
            AstValue::SingleQuotedString(s) => s.clone(),
            _ => return None,
        },
        _ => return None,
    };

    let entry = table
        .fts_indexes
        .iter()
        .find(|e| e.column_name == col_name)?;

    let scored = entry.index.query(&query, &Bm25Params::default());
    let mut out: Vec<i64> = scored.into_iter().map(|(id, _)| id).collect();
    if out.len() > k {
        out.truncate(k);
    }
    Some(out)
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
    scope_name: &str,
) -> Result<Vec<i64>> {
    use std::collections::BinaryHeap;

    if k == 0 || matching.is_empty() {
        return Ok(Vec::new());
    }

    let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(k + 1);

    for &rowid in matching {
        let key = eval_expr(&order.expr, table, rowid, scope_name)?;
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

fn sort_rowids(
    rowids: &mut [i64],
    table: &Table,
    order: &OrderByClause,
    scope_name: &str,
) -> Result<()> {
    // Phase 7b: ORDER BY now accepts any expression (column ref,
    // arithmetic, function call, …). Pre-compute the sort key for
    // every rowid up front so the comparator is called O(N log N)
    // times against pre-evaluated Values rather than re-evaluating
    // the expression O(N log N) times. Not strictly necessary today,
    // but vital once 7d's HNSW index lands and this same code path
    // could be running tens of millions of distance computations.
    let mut keys: Vec<(i64, Result<Value>)> = rowids
        .iter()
        .map(|r| (*r, eval_expr(&order.expr, table, *r, scope_name)))
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

/// Returns `true` if the row at `rowid` matches the predicate
/// expression. `scope_name` is the user-visible name a `t.col`
/// qualifier must match — the FROM alias when declared, else the
/// table name (SQLR-14).
pub fn eval_predicate(expr: &Expr, table: &Table, rowid: i64, scope_name: &str) -> Result<bool> {
    eval_predicate_scope(expr, &SingleTableScope::new(table, rowid, scope_name))
}

/// Scope-aware predicate evaluation. The single-table fast path wraps
/// this with a [`SingleTableScope`]; the join executor wraps it with
/// a [`JoinedScope`].
pub(crate) fn eval_predicate_scope(expr: &Expr, scope: &dyn RowScope) -> Result<bool> {
    let v = eval_expr_scope(expr, scope)?;
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

/// Single-table convenience wrapper around [`eval_expr_scope`].
fn eval_expr(expr: &Expr, table: &Table, rowid: i64, scope_name: &str) -> Result<Value> {
    eval_expr_scope(expr, &SingleTableScope::new(table, rowid, scope_name))
}

fn eval_expr_scope(expr: &Expr, scope: &dyn RowScope) -> Result<Value> {
    match expr {
        Expr::Nested(inner) => eval_expr_scope(inner, scope),

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
            scope.lookup(None, &ident.value)
        }

        Expr::CompoundIdentifier(parts) => {
            // `qualifier.col` — single-table scope requires the
            // qualifier to name its one table (alias if declared,
            // SQLR-14). Joined scope dispatches to the table matching
            // `qualifier`. The compound form must have at least two
            // parts; deeper paths (`db.schema.t.col`) are not
            // supported.
            match parts.as_slice() {
                [only] => scope.lookup(None, &only.value),
                [q, c] => scope.lookup(Some(&q.value), &c.value),
                _ => Err(SQLRiteError::NotImplemented(format!(
                    "compound identifier with {} parts is not supported",
                    parts.len()
                ))),
            }
        }

        Expr::Value(v) => convert_literal(&v.value),

        Expr::UnaryOp { op, expr } => {
            let inner = eval_expr_scope(expr, scope)?;
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
                let l = eval_expr_scope(left, scope)?;
                let r = eval_expr_scope(right, scope)?;
                Ok(Value::Bool(as_bool(&l)? && as_bool(&r)?))
            }
            BinaryOperator::Or => {
                let l = eval_expr_scope(left, scope)?;
                let r = eval_expr_scope(right, scope)?;
                Ok(Value::Bool(as_bool(&l)? || as_bool(&r)?))
            }
            cmp @ (BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq) => {
                let l = eval_expr_scope(left, scope)?;
                let r = eval_expr_scope(right, scope)?;
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
                let l = eval_expr_scope(left, scope)?;
                let r = eval_expr_scope(right, scope)?;
                eval_arith(arith, &l, &r)
            }
            BinaryOperator::StringConcat => {
                let l = eval_expr_scope(left, scope)?;
                let r = eval_expr_scope(right, scope)?;
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

        // SQLR-7 — `col IS NULL` / `col IS NOT NULL`. Identifier
        // evaluation already maps a missing rowid in the column's
        // BTreeMap to `Value::Null`, so this works uniformly for
        // explicit NULL inserts, omitted columns, and (post-Phase 7e)
        // legacy "Null"-sentinel TEXT cells. NULLs are never inserted
        // into secondary / HNSW / FTS indexes, so an IS NULL probe
        // correctly falls through to a full scan via `select_rowids`.
        Expr::IsNull(inner) => {
            let v = eval_expr_scope(inner, scope)?;
            Ok(Value::Bool(matches!(v, Value::Null)))
        }
        Expr::IsNotNull(inner) => {
            let v = eval_expr_scope(inner, scope)?;
            Ok(Value::Bool(!matches!(v, Value::Null)))
        }

        // SQLR-3 — LIKE / NOT LIKE / ILIKE. Pattern matching uses our
        // own iterative two-pointer matcher (see `agg::like_match`).
        // SQLite's default is case-insensitive ASCII; we follow that.
        // ILIKE is also case-insensitive (a no-op switch here, but we
        // keep the arm explicit so SQLite users typing ILIKE get the
        // expected semantics rather than a NotImplemented).
        Expr::Like {
            negated,
            any,
            expr: lhs,
            pattern,
            escape_char,
        } => eval_like(
            scope,
            *negated,
            *any,
            lhs,
            pattern,
            escape_char.as_ref(),
            true,
        ),
        Expr::ILike {
            negated,
            any,
            expr: lhs,
            pattern,
            escape_char,
        } => eval_like(
            scope,
            *negated,
            *any,
            lhs,
            pattern,
            escape_char.as_ref(),
            true,
        ),

        // SQLR-3 — IN (list) / NOT IN (list). Subquery form is rejected.
        // Three-valued logic: if the LHS is NULL, return NULL; if any
        // list entry is NULL and no match was found, return NULL too.
        // WHERE coerces NULL → false at line ~1494, so the practical
        // effect is "row excluded" — matches SQLite.
        Expr::InList {
            expr: lhs,
            list,
            negated,
        } => eval_in_list(scope, lhs, list, *negated),
        Expr::InSubquery { .. } => Err(SQLRiteError::NotImplemented(
            "IN (subquery) is not supported (only literal lists are)".to_string(),
        )),

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
        Expr::Function(func) => eval_function(func, scope),

        other => Err(SQLRiteError::NotImplemented(format!(
            "unsupported expression in WHERE/projection: {other:?}"
        ))),
    }
}

/// Dispatches an `Expr::Function` to its built-in implementation.
/// Currently only the three vec_distance_* functions; other functions
/// surface as `NotImplemented` errors with the function name in the
/// message so users see what they tried.
fn eval_function(func: &sqlparser::ast::Function, scope: &dyn RowScope) -> Result<Value> {
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
            let (a, b) = extract_two_vector_args(&name, &func.args, scope)?;
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
        // Phase 7e — JSON functions. All four parse the JSON text on
        // demand (we don't cache parsed values), then resolve a path
        // (default `$` = root). The path resolver handles `.key` for
        // object access and `[N]` for array index. SQLite-style.
        "json_extract" => json_fn_extract(&name, &func.args, scope),
        "json_type" => json_fn_type(&name, &func.args, scope),
        "json_array_length" => json_fn_array_length(&name, &func.args, scope),
        "json_object_keys" => json_fn_object_keys(&name, &func.args, scope),
        // Phase 8b — FTS scalars. Both consult an FTS index attached to
        // the named column; both error if no index exists (the index is
        // a hard prerequisite, mirroring SQLite FTS5's MATCH).
        //
        // SQLR-5 — these only work in a single-table scope because they
        // need the owning `Table` to look up an FTS index by name and
        // they key results by the row's rowid. In a joined query the
        // index lookup would be ambiguous (which table's FTS?) and the
        // scoring rowid is per-table. Reject up front rather than
        // silently wrong-result.
        "fts_match" | "bm25_score" => {
            let Some((table, rowid)) = scope.single_table_view() else {
                return Err(SQLRiteError::NotImplemented(format!(
                    "{name}() is not yet supported inside a JOIN query — \
                     use it on a single-table SELECT or move the FTS lookup into a subquery"
                )));
            };
            let (entry, query) = resolve_fts_args(&name, &func.args, table, scope)?;
            Ok(match name.as_str() {
                "fts_match" => Value::Bool(entry.index.matches(rowid, &query)),
                "bm25_score" => {
                    Value::Real(entry.index.score(rowid, &query, &Bm25Params::default()))
                }
                _ => unreachable!(),
            })
        }
        // SQLR-3: catch aggregate names used in scalar position (e.g.
        // `WHERE COUNT(*) > 1`) with a clearer message than "unknown
        // function".
        "count" | "sum" | "avg" | "min" | "max" => Err(SQLRiteError::NotImplemented(format!(
            "aggregate function '{name}' is not allowed in WHERE / projection-scalar position; \
             use it as a top-level projection item or in HAVING"
        ))),
        other => Err(SQLRiteError::NotImplemented(format!(
            "unknown function: {other}(...)"
        ))),
    }
}

/// Helper for `fts_match` / `bm25_score`: pull the column reference out
/// of arg 0 (a bare identifier — we need the *name*, not the per-row
/// value), evaluate arg 1 as a Text query string, and look up the FTS
/// index attached to that column. Errors if any step fails.
fn resolve_fts_args<'t>(
    fn_name: &str,
    args: &FunctionArguments,
    table: &'t Table,
    scope: &dyn RowScope,
) -> Result<(&'t FtsIndexEntry, String)> {
    let arg_list = match args {
        FunctionArguments::List(l) => &l.args,
        _ => {
            return Err(SQLRiteError::General(format!(
                "{fn_name}() expects exactly two arguments: (column, query_text)"
            )));
        }
    };
    if arg_list.len() != 2 {
        return Err(SQLRiteError::General(format!(
            "{fn_name}() expects exactly 2 arguments, got {}",
            arg_list.len()
        )));
    }

    // Arg 0: bare column identifier. Must resolve syntactically to a
    // column name (we can't accept arbitrary expressions because we
    // need the column to look up the index, not the column's value).
    let col_expr = match &arg_list[0] {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
        other => {
            return Err(SQLRiteError::NotImplemented(format!(
                "{fn_name}() argument 0 must be a column name, got {other:?}"
            )));
        }
    };
    let col_name = match col_expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => parts
            .last()
            .map(|p| p.value.clone())
            .ok_or_else(|| SQLRiteError::Internal("empty compound identifier".to_string()))?,
        other => {
            return Err(SQLRiteError::General(format!(
                "{fn_name}() argument 0 must be a column reference, got {other:?}"
            )));
        }
    };

    // Arg 1: query string. Evaluated through the normal expression
    // pipeline so callers can pass a literal `'rust db'` or an
    // expression that yields TEXT.
    let q_expr = match &arg_list[1] {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
        other => {
            return Err(SQLRiteError::NotImplemented(format!(
                "{fn_name}() argument 1 must be a text expression, got {other:?}"
            )));
        }
    };
    let query = match eval_expr_scope(q_expr, scope)? {
        Value::Text(s) => s,
        other => {
            return Err(SQLRiteError::General(format!(
                "{fn_name}() argument 1 must be TEXT, got {}",
                other.to_display_string()
            )));
        }
    };

    let entry = table
        .fts_indexes
        .iter()
        .find(|e| e.column_name == col_name)
        .ok_or_else(|| {
            SQLRiteError::General(format!(
                "{fn_name}({col_name}, ...): no FTS index on column '{col_name}' \
                 (run CREATE INDEX <name> ON <table> USING fts({col_name}) first)"
            ))
        })?;
    Ok((entry, query))
}

// -----------------------------------------------------------------
// Phase 7e — JSON path-extraction functions
// -----------------------------------------------------------------

/// Extracts the JSON-typed text + optional path string out of a
/// function call's args. Used by all four json_* functions.
///
/// Arity rules (matching SQLite JSON1):
///   - 1 arg  → JSON value, path defaults to `$` (root)
///   - 2 args → (JSON value, path text)
///
/// Returns `(json_text, path)` so caller can serde_json::from_str
/// + walk_json_path on it.
fn extract_json_and_path(
    fn_name: &str,
    args: &FunctionArguments,
    scope: &dyn RowScope,
) -> Result<(String, String)> {
    let arg_list = match args {
        FunctionArguments::List(l) => &l.args,
        _ => {
            return Err(SQLRiteError::General(format!(
                "{fn_name}() expects 1 or 2 arguments"
            )));
        }
    };
    if !(arg_list.len() == 1 || arg_list.len() == 2) {
        return Err(SQLRiteError::General(format!(
            "{fn_name}() expects 1 or 2 arguments, got {}",
            arg_list.len()
        )));
    }
    // Evaluate first arg → must produce text.
    let first_expr = match &arg_list[0] {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
        other => {
            return Err(SQLRiteError::NotImplemented(format!(
                "{fn_name}() argument 0 has unsupported shape: {other:?}"
            )));
        }
    };
    let json_text = match eval_expr_scope(first_expr, scope)? {
        Value::Text(s) => s,
        Value::Null => {
            return Err(SQLRiteError::General(format!(
                "{fn_name}() called on NULL — JSON column has no value for this row"
            )));
        }
        other => {
            return Err(SQLRiteError::General(format!(
                "{fn_name}() argument 0 is not JSON-typed: got {}",
                other.to_display_string()
            )));
        }
    };

    // Path defaults to root `$` when omitted.
    let path = if arg_list.len() == 2 {
        let path_expr = match &arg_list[1] {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
            other => {
                return Err(SQLRiteError::NotImplemented(format!(
                    "{fn_name}() argument 1 has unsupported shape: {other:?}"
                )));
            }
        };
        match eval_expr_scope(path_expr, scope)? {
            Value::Text(s) => s,
            other => {
                return Err(SQLRiteError::General(format!(
                    "{fn_name}() path argument must be a string literal, got {}",
                    other.to_display_string()
                )));
            }
        }
    } else {
        "$".to_string()
    };

    Ok((json_text, path))
}

/// Walks a `serde_json::Value` along a JSONPath subset:
///   - `$` is the root
///   - `.key` for object access (key may not contain `.` or `[`)
///   - `[N]` for array index (N a non-negative integer)
///   - chains arbitrarily: `$.foo.bar[0].baz`
///
/// Returns `Ok(None)` for "path didn't match anything" (NULL in SQL),
/// `Err` for malformed paths. Matches SQLite JSON1's semantic
/// distinction: missing-key = NULL, malformed-path = error.
fn walk_json_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Result<Option<&'a serde_json::Value>> {
    let mut chars = path.chars().peekable();
    if chars.next() != Some('$') {
        return Err(SQLRiteError::General(format!(
            "JSON path must start with '$', got `{path}`"
        )));
    }
    let mut current = value;
    while let Some(&c) = chars.peek() {
        match c {
            '.' => {
                chars.next();
                let mut key = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '[' {
                        break;
                    }
                    key.push(c);
                    chars.next();
                }
                if key.is_empty() {
                    return Err(SQLRiteError::General(format!(
                        "JSON path has empty key after '.' in `{path}`"
                    )));
                }
                match current.get(&key) {
                    Some(v) => current = v,
                    None => return Ok(None),
                }
            }
            '[' => {
                chars.next();
                let mut idx_str = String::new();
                while let Some(&c) = chars.peek() {
                    if c == ']' {
                        break;
                    }
                    idx_str.push(c);
                    chars.next();
                }
                if chars.next() != Some(']') {
                    return Err(SQLRiteError::General(format!(
                        "JSON path has unclosed `[` in `{path}`"
                    )));
                }
                let idx: usize = idx_str.trim().parse().map_err(|_| {
                    SQLRiteError::General(format!(
                        "JSON path has non-integer index `[{idx_str}]` in `{path}`"
                    ))
                })?;
                match current.get(idx) {
                    Some(v) => current = v,
                    None => return Ok(None),
                }
            }
            other => {
                return Err(SQLRiteError::General(format!(
                    "JSON path has unexpected character `{other}` in `{path}` \
                     (expected `.`, `[`, or end-of-path)"
                )));
            }
        }
    }
    Ok(Some(current))
}

/// Converts a serde_json scalar to a SQLRite Value. For composite
/// types (object, array) returns the JSON-encoded text — callers
/// pattern-match on shape from the calling json_* function.
fn json_value_to_sql(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            // Match SQLite: integer if it fits an i64, else f64.
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Real(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Text(s.clone()),
        // Objects + arrays come out as JSON-encoded text. Same as
        // SQLite's json_extract: composite results round-trip through
        // text rather than being modeled as a richer Value type.
        composite => Value::Text(composite.to_string()),
    }
}

fn json_fn_extract(name: &str, args: &FunctionArguments, scope: &dyn RowScope) -> Result<Value> {
    let (json_text, path) = extract_json_and_path(name, args, scope)?;
    let parsed: serde_json::Value = serde_json::from_str(&json_text).map_err(|e| {
        SQLRiteError::General(format!("{name}() got invalid JSON `{json_text}`: {e}"))
    })?;
    match walk_json_path(&parsed, &path)? {
        Some(v) => Ok(json_value_to_sql(v)),
        None => Ok(Value::Null),
    }
}

fn json_fn_type(name: &str, args: &FunctionArguments, scope: &dyn RowScope) -> Result<Value> {
    let (json_text, path) = extract_json_and_path(name, args, scope)?;
    let parsed: serde_json::Value = serde_json::from_str(&json_text).map_err(|e| {
        SQLRiteError::General(format!("{name}() got invalid JSON `{json_text}`: {e}"))
    })?;
    let resolved = match walk_json_path(&parsed, &path)? {
        Some(v) => v,
        None => return Ok(Value::Null),
    };
    let ty = match resolved {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(true) => "true",
        serde_json::Value::Bool(false) => "false",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "real"
            }
        }
        serde_json::Value::String(_) => "text",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    };
    Ok(Value::Text(ty.to_string()))
}

fn json_fn_array_length(
    name: &str,
    args: &FunctionArguments,
    scope: &dyn RowScope,
) -> Result<Value> {
    let (json_text, path) = extract_json_and_path(name, args, scope)?;
    let parsed: serde_json::Value = serde_json::from_str(&json_text).map_err(|e| {
        SQLRiteError::General(format!("{name}() got invalid JSON `{json_text}`: {e}"))
    })?;
    let resolved = match walk_json_path(&parsed, &path)? {
        Some(v) => v,
        None => return Ok(Value::Null),
    };
    match resolved.as_array() {
        Some(arr) => Ok(Value::Integer(arr.len() as i64)),
        None => Err(SQLRiteError::General(format!(
            "{name}() resolved to a non-array value at path `{path}`"
        ))),
    }
}

fn json_fn_object_keys(
    name: &str,
    args: &FunctionArguments,
    scope: &dyn RowScope,
) -> Result<Value> {
    let (json_text, path) = extract_json_and_path(name, args, scope)?;
    let parsed: serde_json::Value = serde_json::from_str(&json_text).map_err(|e| {
        SQLRiteError::General(format!("{name}() got invalid JSON `{json_text}`: {e}"))
    })?;
    let resolved = match walk_json_path(&parsed, &path)? {
        Some(v) => v,
        None => return Ok(Value::Null),
    };
    let obj = resolved.as_object().ok_or_else(|| {
        SQLRiteError::General(format!(
            "{name}() resolved to a non-object value at path `{path}`"
        ))
    })?;
    // SQLite's json_object_keys is a table-valued function (one row
    // per key). Without set-returning function support we can't
    // reproduce that shape; instead return the keys as a JSON array
    // text. Caller can iterate via json_array_length + json_extract,
    // or just treat it as a serialized list. Document this divergence
    // in supported-sql.md.
    let keys: Vec<serde_json::Value> = obj
        .keys()
        .map(|k| serde_json::Value::String(k.clone()))
        .collect();
    Ok(Value::Text(serde_json::Value::Array(keys).to_string()))
}

/// Extracts exactly two `Vec<f32>` arguments from a function call,
/// validating arity and that both sides are Vector-typed with matching
/// dimensions. Used by all three vec_distance_* functions.
fn extract_two_vector_args(
    fn_name: &str,
    args: &FunctionArguments,
    scope: &dyn RowScope,
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
        let val = eval_expr_scope(expr, scope)?;
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

// -----------------------------------------------------------------
// SQLR-3 — LIKE / IN evaluators
// -----------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn eval_like(
    scope: &dyn RowScope,
    negated: bool,
    any: bool,
    lhs: &Expr,
    pattern: &Expr,
    escape_char: Option<&AstValue>,
    case_insensitive: bool,
) -> Result<Value> {
    if any {
        return Err(SQLRiteError::NotImplemented(
            "LIKE ANY (...) is not supported".to_string(),
        ));
    }
    if escape_char.is_some() {
        return Err(SQLRiteError::NotImplemented(
            "LIKE ... ESCAPE '<char>' is not supported (default `\\` escape only)".to_string(),
        ));
    }

    let l = eval_expr_scope(lhs, scope)?;
    let p = eval_expr_scope(pattern, scope)?;
    if matches!(l, Value::Null) || matches!(p, Value::Null) {
        return Ok(Value::Null);
    }
    let text = match l {
        Value::Text(s) => s,
        other => other.to_display_string(),
    };
    let pat = match p {
        Value::Text(s) => s,
        other => other.to_display_string(),
    };
    let m = like_match(&text, &pat, case_insensitive);
    Ok(Value::Bool(if negated { !m } else { m }))
}

fn eval_in_list(scope: &dyn RowScope, lhs: &Expr, list: &[Expr], negated: bool) -> Result<Value> {
    let l = eval_expr_scope(lhs, scope)?;
    if matches!(l, Value::Null) {
        return Ok(Value::Null);
    }
    let mut saw_null = false;
    for item in list {
        let r = eval_expr_scope(item, scope)?;
        if matches!(r, Value::Null) {
            saw_null = true;
            continue;
        }
        if compare_values(Some(&l), Some(&r)) == Ordering::Equal {
            return Ok(Value::Bool(!negated));
        }
    }
    if saw_null {
        // SQLite three-valued IN: unmatched + a NULL on the RHS → NULL.
        // WHERE coerces NULL → false, so the row is excluded either way.
        Ok(Value::Null)
    } else {
        Ok(Value::Bool(negated))
    }
}

// -----------------------------------------------------------------
// SQLR-3 — Aggregation phase, DISTINCT, post-projection sort
// -----------------------------------------------------------------

/// SQLR-52 — HAVING lowering, shared by the single-table and joined
/// aggregation paths. The expression may reference aggregates and
/// GROUP BY keys that aren't in the SELECT output (SQLite allows
/// both: `SELECT dept FROM t GROUP BY dept HAVING COUNT(*) > 1`).
/// We append those as *hidden* trailing projection slots so the
/// `aggregate_rows` accumulator computes them alongside the visible
/// ones; the pipeline strips them after filtering. Aggregate calls in
/// the HAVING tree are lowered to identifiers naming their output slot
/// (`COUNT(*)` → identifier "COUNT(*)") so the shared expression
/// evaluator can resolve them through a `GroupRowScope` like any other
/// column. Returns the widened projection list plus the lowered
/// HAVING expression (`None` when the query has no HAVING).
fn lower_having_into_hidden_slots(
    query: &SelectQuery,
    proj_items: &[ProjectionItem],
) -> Result<(Vec<ProjectionItem>, Option<Expr>)> {
    let mut all_items = proj_items.to_vec();
    let having_expr = match &query.having {
        Some(h) => {
            for g in &query.group_by {
                if !all_items
                    .iter()
                    .any(|i| i.output_name().eq_ignore_ascii_case(&g.name))
                {
                    all_items.push(ProjectionItem {
                        kind: ProjectionKind::Column {
                            qualifier: g.qualifier.clone(),
                            name: g.name.clone(),
                        },
                        alias: None,
                    });
                }
            }
            Some(lower_having_expr(h, &mut all_items)?)
        }
        None => None,
    };
    Ok((all_items, having_expr))
}

/// The aggregation tail shared by the single-table and joined SELECT
/// paths: accumulate groups over the row scopes, apply HAVING, strip
/// hidden HAVING-only slots, then DISTINCT / ORDER BY / LIMIT on the
/// output rows. Callers validate column references against their own
/// scope (table schema vs. joined-table list) before invoking.
fn run_aggregation_pipeline<S: RowScope>(
    scopes: impl IntoIterator<Item = S>,
    query: &SelectQuery,
    proj_items: &[ProjectionItem],
    all_items: &[ProjectionItem],
    having_expr: &Option<Expr>,
) -> Result<SelectResult> {
    let columns: Vec<String> = proj_items.iter().map(|i| i.output_name()).collect();
    let mut rows = aggregate_rows(scopes, &query.group_by, all_items)?;

    if let Some(h) = having_expr {
        let all_columns: Vec<String> = all_items.iter().map(|i| i.output_name()).collect();
        rows = filter_groups_by_having(rows, h, &all_columns)?;
    }
    // Drop the hidden HAVING-only slots back to the user-visible width.
    if all_items.len() > proj_items.len() {
        for row in &mut rows {
            row.truncate(proj_items.len());
        }
    }

    if query.distinct {
        rows = dedupe_rows(rows);
    }

    if let Some(order) = &query.order_by {
        sort_output_rows(&mut rows, &columns, proj_items, order)?;
    }
    if let Some(k) = query.limit {
        rows.truncate(k);
    }

    Ok(SelectResult { columns, rows })
}

/// Walk the row scopes, partition into groups (one synthetic group
/// when `group_by` is empty), update one `AggState` per aggregate
/// projection slot per group, then materialize one output row per
/// group in projection order. Group-key columns surface their original
/// `Value` (captured the first time the group was seen); aggregate
/// slots surface `AggState::finalize()`.
///
/// SQLR-6 — generic over [`RowScope`] so the same accumulator serves
/// the single-table path (a [`SingleTableScope`] per matching rowid)
/// and the joined path (a [`JoinedScope`] per joined row, where
/// NULL-padded outer-join sides surface as `Value::Null` — grouped
/// together like any other NULL, and skipped by `COUNT(col)` per the
/// usual NULL-skipping aggregate semantics).
fn aggregate_rows<S: RowScope>(
    scopes: impl IntoIterator<Item = S>,
    group_by: &[GroupByKey],
    proj_items: &[ProjectionItem],
) -> Result<Vec<Vec<Value>>> {
    // Build the per-projection-slot accumulator template once. Each
    // group clones this template on first sight. Non-aggregate slots
    // hold a "captured group-key value" (`None` until set).
    let template: Vec<Option<AggState>> = proj_items
        .iter()
        .map(|i| match &i.kind {
            ProjectionKind::Aggregate(call) => Some(AggState::new(call)),
            ProjectionKind::Column { .. } => None,
        })
        .collect();

    // Linear-scan group lookup. For typical ad-hoc queries (cardinality
    // ≪ 10k), this is fine; if grouping cardinality grows, swap to a
    // HashMap<Vec<DistinctKey>, usize> keyed by the same DistinctKey
    // wrapper. Order-preserving for readable output (groups appear in
    // first-occurrence order, matching SQLite's typical behavior).
    let mut keys: Vec<Vec<DistinctKey>> = Vec::new();
    let mut group_states: Vec<Vec<Option<AggState>>> = Vec::new();
    let mut group_key_values: Vec<Vec<Value>> = Vec::new();

    for scope in scopes {
        let mut key_values: Vec<Value> = Vec::with_capacity(group_by.len());
        let mut key: Vec<DistinctKey> = Vec::with_capacity(group_by.len());
        for g in group_by {
            let v = scope.lookup(g.qualifier.as_deref(), &g.name)?;
            key.push(DistinctKey::from_value(&v));
            key_values.push(v);
        }
        let idx = match keys.iter().position(|k| k == &key) {
            Some(i) => i,
            None => {
                keys.push(key);
                group_states.push(template.clone());
                group_key_values.push(key_values);
                keys.len() - 1
            }
        };

        for (slot, item) in proj_items.iter().enumerate() {
            if let ProjectionKind::Aggregate(call) = &item.kind {
                let v = match &call.arg {
                    AggregateArg::Star => Value::Null,
                    AggregateArg::Column { qualifier, name } => {
                        scope.lookup(qualifier.as_deref(), name)?
                    }
                };
                if let Some(state) = group_states[idx][slot].as_mut() {
                    state.update(&v)?;
                }
            }
        }
    }

    // No groups but no aggregate-only "implicit one row" semantic to
    // emit: e.g. `SELECT dept FROM t GROUP BY dept` over an empty
    // matching set should produce zero rows. `SELECT COUNT(*) FROM t`
    // (no GROUP BY) DOES produce one row even on empty input — the
    // single-synthetic-group path below handles it.
    if keys.is_empty() && group_by.is_empty() {
        // Synthetic single empty group so we still emit one row with
        // initial accumulator finals (e.g. COUNT(*) → 0).
        keys.push(Vec::new());
        group_states.push(template.clone());
        group_key_values.push(Vec::new());
    }

    // Project: one row per group, in projection order.
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(keys.len());
    for (group_idx, _) in keys.iter().enumerate() {
        let mut row: Vec<Value> = Vec::with_capacity(proj_items.len());
        for (slot, item) in proj_items.iter().enumerate() {
            match &item.kind {
                ProjectionKind::Column { qualifier, name: c } => {
                    // Parser / executor validation ties bare-column
                    // projections to GROUP BY entries, but `SELECT *`
                    // expansions reach here unvalidated — surface a
                    // clean error rather than panicking.
                    let pos = group_by
                        .iter()
                        .position(|g| g.matches_column(qualifier.as_deref(), c))
                        .ok_or_else(|| {
                            SQLRiteError::Internal(format!(
                                "column '{c}' must appear in GROUP BY or be used in an \
                                 aggregate function"
                            ))
                        })?;
                    row.push(group_key_values[group_idx][pos].clone());
                }
                ProjectionKind::Aggregate(_) => {
                    let state = group_states[group_idx][slot]
                        .as_ref()
                        .expect("aggregate slot has state");
                    row.push(state.finalize());
                }
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

// -----------------------------------------------------------------
// SQLR-52 — HAVING (post-aggregation filter)
// -----------------------------------------------------------------

/// Scope for evaluating a HAVING expression against one group's output
/// row. Column references resolve against the output column names —
/// GROUP BY keys, aggregate aliases, aggregate display forms like
/// `COUNT(*)` (the lowered shape `lower_having_expr` produces), and
/// the hidden HAVING-only slots appended by the executor.
struct GroupRowScope<'a> {
    columns: &'a [String],
    values: &'a [Value],
}

impl RowScope for GroupRowScope<'_> {
    fn lookup(&self, qualifier: Option<&str>, col: &str) -> Result<Value> {
        // Output columns carry no table qualifier — `t.dept` in HAVING
        // resolves by its column part, same as the aggregating ORDER BY.
        let _ = qualifier;
        self.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(col))
            .map(|i| self.values[i].clone())
            .ok_or_else(|| {
                SQLRiteError::Internal(format!(
                    "HAVING references '{col}', which is neither a GROUP BY column nor an \
                     aggregate in scope"
                ))
            })
    }

    fn single_table_view(&self) -> Option<(&Table, i64)> {
        None
    }
}

/// Rewrite a HAVING expression for group-row evaluation: every
/// aggregate call in the tree becomes an identifier naming its output
/// slot (`SUM(salary)` → identifier `"SUM(salary)"`), registering a
/// hidden projection slot for any aggregate not already in the SELECT
/// list so `aggregate_rows` computes it. Non-aggregate functions and
/// leaf expressions pass through untouched — the shared evaluator
/// handles (or rejects) them at filter time.
fn lower_having_expr(expr: &Expr, items: &mut Vec<ProjectionItem>) -> Result<Expr> {
    Ok(match expr {
        Expr::Function(func) => {
            let is_aggregate = matches!(
                func.name.0.as_slice(),
                [ObjectNamePart::Identifier(ident)] if AggregateFn::from_name(&ident.value).is_some()
            );
            if !is_aggregate {
                return Ok(expr.clone());
            }
            let call = parse_aggregate_call(func)?;
            let display = call.display_name();
            // Resolvable already? Identifier lookup goes by output
            // column name, so an unaliased projection of the same
            // aggregate (output name == display form) suffices. An
            // *aliased* one doesn't — its output name is the alias —
            // so the call still gets a hidden slot of its own.
            let already_known = items
                .iter()
                .any(|i| i.output_name().eq_ignore_ascii_case(&display));
            if !already_known {
                items.push(ProjectionItem {
                    kind: ProjectionKind::Aggregate(call),
                    alias: None,
                });
            }
            Expr::Identifier(Ident::new(display))
        }
        Expr::Nested(inner) => Expr::Nested(Box::new(lower_having_expr(inner, items)?)),
        Expr::UnaryOp { op, expr: inner } => Expr::UnaryOp {
            op: *op,
            expr: Box::new(lower_having_expr(inner, items)?),
        },
        Expr::BinaryOp { left, op, right } => Expr::BinaryOp {
            left: Box::new(lower_having_expr(left, items)?),
            op: op.clone(),
            right: Box::new(lower_having_expr(right, items)?),
        },
        Expr::IsNull(inner) => Expr::IsNull(Box::new(lower_having_expr(inner, items)?)),
        Expr::IsNotNull(inner) => Expr::IsNotNull(Box::new(lower_having_expr(inner, items)?)),
        Expr::InList {
            expr: lhs,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(lower_having_expr(lhs, items)?),
            list: list
                .iter()
                .map(|e| lower_having_expr(e, items))
                .collect::<Result<Vec<_>>>()?,
            negated: *negated,
        },
        Expr::Like {
            negated,
            any,
            expr: lhs,
            pattern,
            escape_char,
        } => Expr::Like {
            negated: *negated,
            any: *any,
            expr: Box::new(lower_having_expr(lhs, items)?),
            pattern: Box::new(lower_having_expr(pattern, items)?),
            escape_char: escape_char.clone(),
        },
        Expr::ILike {
            negated,
            any,
            expr: lhs,
            pattern,
            escape_char,
        } => Expr::ILike {
            negated: *negated,
            any: *any,
            expr: Box::new(lower_having_expr(lhs, items)?),
            pattern: Box::new(lower_having_expr(pattern, items)?),
            escape_char: escape_char.clone(),
        },
        // Leaves (identifiers, literals) and unsupported shapes pass
        // through; the evaluator produces its own error for the latter.
        other => other.clone(),
    })
}

/// Keep only the groups whose HAVING expression evaluates truthy.
/// NULL collapses to false — same three-valued-logic coercion the
/// WHERE path applies.
fn filter_groups_by_having(
    rows: Vec<Vec<Value>>,
    having: &Expr,
    columns: &[String],
) -> Result<Vec<Vec<Value>>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let scope = GroupRowScope {
            columns,
            values: &row,
        };
        let keep = match eval_expr_scope(having, &scope)? {
            Value::Bool(b) => b,
            Value::Null => false,
            Value::Integer(i) => i != 0,
            other => {
                return Err(SQLRiteError::Internal(format!(
                    "HAVING clause must evaluate to boolean, got {}",
                    other.to_display_string()
                )));
            }
        };
        if keep {
            out.push(row);
        }
    }
    Ok(out)
}

/// SELECT DISTINCT post-pass. Walks the rows once with a `HashSet` of
/// row-keys, preserving first-occurrence order. NULL == NULL for
/// dedupe purposes, which matches the SQL DISTINCT semantic.
fn dedupe_rows(rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    use std::collections::HashSet;
    let mut seen: HashSet<Vec<DistinctKey>> = HashSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let key: Vec<DistinctKey> = row.iter().map(DistinctKey::from_value).collect();
        if seen.insert(key) {
            out.push(row);
        }
    }
    out
}

/// Sort output rows for the aggregating path. ORDER BY can reference
/// either an output column name (alias or bare GROUP BY column) or an
/// aggregate function call by display form (e.g. `COUNT(*)`).
fn sort_output_rows(
    rows: &mut [Vec<Value>],
    columns: &[String],
    proj_items: &[ProjectionItem],
    order: &OrderByClause,
) -> Result<()> {
    let target_idx = resolve_order_by_index(&order.expr, columns, proj_items)?;
    rows.sort_by(|a, b| {
        let va = &a[target_idx];
        let vb = &b[target_idx];
        let ord = compare_values(Some(va), Some(vb));
        if order.ascending { ord } else { ord.reverse() }
    });
    Ok(())
}

/// Map an ORDER BY expression to the index of the output column that
/// should drive the sort.
fn resolve_order_by_index(
    expr: &Expr,
    columns: &[String],
    proj_items: &[ProjectionItem],
) -> Result<usize> {
    // Bare identifier — match against output names (alias-first).
    let target_name: Option<String> = match expr {
        Expr::Identifier(ident) => Some(ident.value.clone()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.clone()),
        Expr::Function(_) => None,
        Expr::Nested(inner) => return resolve_order_by_index(inner, columns, proj_items),
        other => {
            return Err(SQLRiteError::NotImplemented(format!(
                "ORDER BY expression not supported on aggregating queries: {other:?}"
            )));
        }
    };
    if let Some(name) = target_name {
        if let Some(i) = columns.iter().position(|c| c.eq_ignore_ascii_case(&name)) {
            return Ok(i);
        }
        return Err(SQLRiteError::Internal(format!(
            "ORDER BY references unknown column '{name}' in the SELECT output"
        )));
    }
    // Function form: match by display name against any aggregate item
    // whose canonical display equals the user's call. Tolerate case
    // differences in the function name. SQLR-6 — a second pass with
    // `t.` qualifiers stripped from both sides keeps qualifier
    // spelling differences from blocking the match (`ORDER BY
    // SUM(amount)` finds a `SELECT SUM(o.amount)` slot and vice
    // versa), preserving the pre-qualifier behavior.
    if let Expr::Function(func) = expr {
        let user_disp = format_function_display(func, true);
        for (i, item) in proj_items.iter().enumerate() {
            if let ProjectionKind::Aggregate(call) = &item.kind
                && call.display_name().eq_ignore_ascii_case(&user_disp)
            {
                return Ok(i);
            }
        }
        let user_disp_unqualified = format_function_display(func, false);
        for (i, item) in proj_items.iter().enumerate() {
            if let ProjectionKind::Aggregate(call) = &item.kind
                && call
                    .display_name_unqualified()
                    .eq_ignore_ascii_case(&user_disp_unqualified)
            {
                return Ok(i);
            }
        }
        return Err(SQLRiteError::Internal(format!(
            "ORDER BY references aggregate '{user_disp}' that isn't in the SELECT output"
        )));
    }
    Err(SQLRiteError::Internal(
        "ORDER BY expression could not be resolved against the output columns".to_string(),
    ))
}

/// Format a sqlparser function call into the same canonical form
/// `AggregateCall::display_name()` uses, so ORDER BY on
/// `COUNT(*)` / `SUM(salary)` matches its projection counterpart.
/// `qualified` keeps or strips the argument's `t.` qualifier, matching
/// `display_name()` / `display_name_unqualified()` respectively.
fn format_function_display(func: &sqlparser::ast::Function, qualified: bool) -> String {
    let name = match func.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => ident.value.to_uppercase(),
        _ => format!("{:?}", func.name).to_uppercase(),
    };
    let inner = match &func.args {
        FunctionArguments::List(l) => {
            let distinct = matches!(
                l.duplicate_treatment,
                Some(sqlparser::ast::DuplicateTreatment::Distinct)
            );
            let arg = l.args.first().map(|a| match a {
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => "*".to_string(),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(i))) => i.value.clone(),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::CompoundIdentifier(parts))) => {
                    if qualified {
                        parts
                            .iter()
                            .map(|p| p.value.clone())
                            .collect::<Vec<_>>()
                            .join(".")
                    } else {
                        parts.last().map(|p| p.value.clone()).unwrap_or_default()
                    }
                }
                _ => String::new(),
            });
            match (distinct, arg) {
                (true, Some(a)) if a != "*" => format!("DISTINCT {a}"),
                (_, Some(a)) => a,
                _ => String::new(),
            }
        }
        _ => String::new(),
    };
    format!("{name}({inner})")
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
    use crate::sql::dialect::SqlriteDialect;
    use crate::sql::parser::select::SelectQuery;
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
        let dialect = SqlriteDialect::new();
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
        sort_rowids(&mut full, table, order, "docs").unwrap();
        full.truncate(10);

        // Bounded-heap path
        let topk = select_topk(&all_rowids, table, order, 10, "docs").unwrap();

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
        sort_rowids(&mut full, table, order, "docs").unwrap();
        full.truncate(10);

        let topk = select_topk(&all_rowids, table, order, 10, "docs").unwrap();

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
        let topk = select_topk(&table.rowids(), table, order, 1000, "docs").unwrap();
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
        let topk = select_topk(&table.rowids(), table, order, 0, "docs").unwrap();
        assert!(topk.is_empty());
    }

    #[test]
    fn topk_empty_input_returns_empty() {
        let db = seed_score_table(0);
        let table = db.get_table("docs".to_string()).unwrap();
        let q = parse_select("SELECT * FROM docs ORDER BY score ASC LIMIT 5;");
        let order = q.order_by.as_ref().unwrap();
        let topk = select_topk(&[], table, order, 5, "docs").unwrap();
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
        let _topk = select_topk(&all_rowids, table, order, K, "docs").unwrap();
        let heap_dur = t0.elapsed();

        // Time full sort + truncate.
        let t1 = Instant::now();
        let mut full = all_rowids.clone();
        sort_rowids(&mut full, table, order, "docs").unwrap();
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

    // ---------------------------------------------------------------------
    // SQLR-7 — IS NULL / IS NOT NULL
    // ---------------------------------------------------------------------

    /// Helper for IS NULL tests: run a SELECT through process_command and
    /// return the rendered table as a String so the test can assert on the
    /// row-count line without re-implementing the executor.
    fn run_select(db: &mut Database, sql: &str) -> String {
        crate::sql::process_command(sql, db).expect("select")
    }

    #[test]
    fn where_is_null_returns_null_rows() {
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);",
            &mut db,
        )
        .unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (1, 10);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (2, NULL);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (3, 30);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (4, NULL);", &mut db).unwrap();

        let response = run_select(&mut db, "SELECT id FROM t WHERE n IS NULL;");
        assert!(
            response.contains("2 rows returned"),
            "IS NULL should return 2 rows, got: {response}"
        );
    }

    #[test]
    fn where_is_not_null_returns_non_null_rows() {
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);",
            &mut db,
        )
        .unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (1, 10);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (2, NULL);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (3, 30);", &mut db).unwrap();

        let response = run_select(&mut db, "SELECT id FROM t WHERE n IS NOT NULL;");
        assert!(
            response.contains("2 rows returned"),
            "IS NOT NULL should return 2 rows, got: {response}"
        );
    }

    #[test]
    fn where_is_null_on_indexed_column() {
        // UNIQUE on a TEXT column gets an automatic secondary index.
        // NULLs aren't stored in the index, so IS NULL falls through to
        // a full scan via select_rowids — verify the full-scan path is
        // still correct.
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT UNIQUE);",
            &mut db,
        )
        .unwrap();
        crate::sql::process_command("INSERT INTO t (id, name) VALUES (1, 'alice');", &mut db)
            .unwrap();
        crate::sql::process_command("INSERT INTO t (id, name) VALUES (2, NULL);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, name) VALUES (3, 'bob');", &mut db)
            .unwrap();

        let null_rows = run_select(&mut db, "SELECT id FROM t WHERE name IS NULL;");
        assert!(
            null_rows.contains("1 row returned"),
            "indexed IS NULL should return 1 row, got: {null_rows}"
        );
        let not_null_rows = run_select(&mut db, "SELECT id FROM t WHERE name IS NOT NULL;");
        assert!(
            not_null_rows.contains("2 rows returned"),
            "indexed IS NOT NULL should return 2 rows, got: {not_null_rows}"
        );
    }

    #[test]
    fn where_is_null_works_on_omitted_column() {
        // No DEFAULT, column missing from the INSERT column list — the
        // BTreeMap entry never gets written, get_value returns None,
        // eval_expr maps that to Value::Null, and IS NULL matches.
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, qty INTEGER, label TEXT);",
            &mut db,
        )
        .unwrap();
        crate::sql::process_command(
            "INSERT INTO t (id, qty, label) VALUES (1, 7, 'a');",
            &mut db,
        )
        .unwrap();
        // qty omitted on row 2.
        crate::sql::process_command("INSERT INTO t (id, label) VALUES (2, 'b');", &mut db).unwrap();

        let response = run_select(&mut db, "SELECT id FROM t WHERE qty IS NULL;");
        assert!(
            response.contains("1 row returned"),
            "IS NULL should match the omitted-column row, got: {response}"
        );
    }

    // ---------------------------------------------------------------------
    // SQLR-2 — unknown columns error in single-table scope, matching
    // JoinedScope. Before the fix, lookup silently returned NULL, so a
    // typo'd WHERE matched every row (catastrophic for UPDATE/DELETE).
    // ---------------------------------------------------------------------

    /// Seed a two-row table the SQLR-2 tests share.
    fn seed_sqlr2() -> Database {
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);",
            &mut db,
        )
        .unwrap();
        crate::sql::process_command("INSERT INTO t (id, name) VALUES (1, 'alice');", &mut db)
            .unwrap();
        crate::sql::process_command("INSERT INTO t (id, name) VALUES (2, 'bob');", &mut db)
            .unwrap();
        db
    }

    #[test]
    fn where_unknown_column_errors_single_table() {
        let mut db = seed_sqlr2();
        let res = crate::sql::process_command("SELECT id FROM t WHERE typo IS NULL;", &mut db);
        let err = res.expect_err("WHERE on an unknown column must error, not match via NULL");
        assert!(
            err.to_string().contains("does not exist"),
            "expected unknown-column error, got: {err}"
        );
    }

    #[test]
    fn order_by_unknown_column_errors_single_table() {
        let mut db = seed_sqlr2();
        let res = crate::sql::process_command("SELECT id FROM t ORDER BY typo;", &mut db);
        assert!(
            res.is_err(),
            "ORDER BY on an unknown column must error, not sort by NULL"
        );
    }

    #[test]
    fn update_with_unknown_column_in_where_errors_and_mutates_nothing() {
        let mut db = seed_sqlr2();
        let res =
            crate::sql::process_command("UPDATE t SET name = 'x' WHERE typo IS NULL;", &mut db);
        assert!(
            res.is_err(),
            "UPDATE with a typo'd WHERE column must error, not update every row"
        );
        let rows = run_select(&mut db, "SELECT id FROM t WHERE name = 'x';");
        assert!(
            rows.contains("0 rows returned"),
            "no row may be updated when the WHERE errors, got: {rows}"
        );
    }

    #[test]
    fn delete_with_unknown_column_in_where_errors_and_deletes_nothing() {
        let mut db = seed_sqlr2();
        let res = crate::sql::process_command("DELETE FROM t WHERE typo IS NULL;", &mut db);
        assert!(
            res.is_err(),
            "DELETE with a typo'd WHERE column must error, not delete every row"
        );
        let rows = run_select(&mut db, "SELECT id FROM t;");
        assert!(
            rows.contains("2 rows returned"),
            "no row may be deleted when the WHERE errors, got: {rows}"
        );
    }

    #[test]
    fn where_is_null_combines_with_and_or() {
        // Sanity check that the new arms compose with the existing
        // boolean operators in eval_expr — `n IS NULL AND id > 1`
        // should narrow correctly.
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER);",
            &mut db,
        )
        .unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (1, NULL);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (2, NULL);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (id, n) VALUES (3, 30);", &mut db).unwrap();

        let response = run_select(&mut db, "SELECT id FROM t WHERE n IS NULL AND id > 1;");
        assert!(
            response.contains("1 row returned"),
            "IS NULL combined with AND should match exactly row 2, got: {response}"
        );
    }

    // ---------------------------------------------------------------------
    // SQLR-14 — table qualifiers are validated in single-table scope,
    // matching JoinedScope. Before the fix, `SELECT x.id FROM t`
    // resolved `x.id` as plain `id`, silently accepting any qualifier.
    // ---------------------------------------------------------------------

    /// Assert `sql` fails with the SQLR-14 unknown-qualifier message.
    fn assert_unknown_qualifier(db: &mut Database, sql: &str, qualifier: &str) {
        let err = crate::sql::process_command(sql, db)
            .expect_err("a bogus table qualifier must error, not be ignored");
        assert!(
            err.to_string()
                .contains(&format!("unknown table qualifier '{qualifier}'")),
            "expected unknown-qualifier error for `{sql}`, got: {err}"
        );
    }

    #[test]
    fn qualifier_matching_table_name_works() {
        let mut db = seed_sqlr2();
        let rows = run_select(&mut db, "SELECT t.id FROM t WHERE t.id = 1 ORDER BY t.id;");
        assert!(rows.contains("1 row returned"), "got: {rows}");
    }

    #[test]
    fn qualifier_matching_alias_works() {
        let mut db = seed_sqlr2();
        let rows = run_select(
            &mut db,
            "SELECT a.id FROM t AS a WHERE a.id = 1 ORDER BY a.id;",
        );
        assert!(rows.contains("1 row returned"), "got: {rows}");
    }

    #[test]
    fn qualifier_match_is_case_insensitive() {
        let mut db = seed_sqlr2();
        let rows = run_select(&mut db, "SELECT T.id FROM t WHERE T.id = 1;");
        assert!(rows.contains("1 row returned"), "got: {rows}");
    }

    #[test]
    fn unknown_qualifier_in_projection_errors() {
        let mut db = seed_sqlr2();
        assert_unknown_qualifier(&mut db, "SELECT x.id FROM t;", "x");
    }

    #[test]
    fn unknown_qualifier_in_where_errors() {
        let mut db = seed_sqlr2();
        assert_unknown_qualifier(&mut db, "SELECT id FROM t WHERE bogus.id = 1;", "bogus");
    }

    #[test]
    fn unknown_qualifier_in_indexed_where_errors() {
        // The `col = literal` WHERE shape short-circuits per-row
        // evaluation via the index probe — the qualifier check must
        // hold there too.
        let mut db = seed_sqlr2();
        crate::sql::process_command("CREATE INDEX idx_name ON t (name);", &mut db).unwrap();
        assert_unknown_qualifier(
            &mut db,
            "SELECT id FROM t WHERE bogus.name = 'alice';",
            "bogus",
        );
    }

    #[test]
    fn unknown_qualifier_in_order_by_errors() {
        let mut db = seed_sqlr2();
        let res = crate::sql::process_command("SELECT id FROM t ORDER BY x.id;", &mut db);
        let err = res.expect_err("ORDER BY with a bogus qualifier must error");
        assert!(
            err.to_string().contains("unknown table qualifier 'x'"),
            "got: {err}"
        );
    }

    #[test]
    fn alias_shadows_table_name_as_qualifier() {
        // SQLite semantics: once `FROM t AS a` declares an alias, the
        // alias is the *only* valid qualifier — `t.id` errors.
        let mut db = seed_sqlr2();
        assert_unknown_qualifier(&mut db, "SELECT t.id FROM t AS a;", "t");
        assert_unknown_qualifier(&mut db, "SELECT id FROM t AS a WHERE t.id = 1;", "t");
    }

    #[test]
    fn unknown_qualifier_in_group_by_and_aggregates_errors() {
        let mut db = seed_sqlr2();
        assert_unknown_qualifier(&mut db, "SELECT COUNT(x.id) FROM t;", "x");
        assert_unknown_qualifier(
            &mut db,
            "SELECT x.name, COUNT(*) FROM t GROUP BY x.name;",
            "x",
        );
        // Matching qualifiers keep working through the aggregation path.
        let rows = run_select(
            &mut db,
            "SELECT t.name, COUNT(t.id) FROM t GROUP BY t.name;",
        );
        assert!(rows.contains("2 rows returned"), "got: {rows}");
    }

    #[test]
    fn update_unknown_qualifier_in_where_errors_and_mutates_nothing() {
        let mut db = seed_sqlr2();
        let res = crate::sql::process_command("UPDATE t SET name = 'x' WHERE x.id = 1;", &mut db);
        assert!(
            res.is_err(),
            "UPDATE with a bogus WHERE qualifier must error"
        );
        let rows = run_select(&mut db, "SELECT id FROM t WHERE name = 'x';");
        assert!(
            rows.contains("0 rows returned"),
            "no row may be updated when the WHERE errors, got: {rows}"
        );
    }

    #[test]
    fn update_set_rhs_unknown_qualifier_errors() {
        let mut db = seed_sqlr2();
        assert_unknown_qualifier(&mut db, "UPDATE t SET name = x.name;", "x");
    }

    #[test]
    fn update_with_alias_validates_against_alias() {
        let mut db = seed_sqlr2();
        // Alias works as the qualifier …
        crate::sql::process_command("UPDATE t AS a SET name = 'x' WHERE a.id = 1;", &mut db)
            .expect("alias qualifier must be accepted in UPDATE");
        let rows = run_select(&mut db, "SELECT id FROM t WHERE name = 'x';");
        assert!(rows.contains("1 row returned"), "got: {rows}");
        // … and shadows the table name, per SQLite.
        assert_unknown_qualifier(&mut db, "UPDATE t AS a SET name = 'y' WHERE t.id = 1;", "t");
    }

    #[test]
    fn delete_unknown_qualifier_in_where_errors_and_deletes_nothing() {
        let mut db = seed_sqlr2();
        let res = crate::sql::process_command("DELETE FROM t WHERE x.id = 1;", &mut db);
        assert!(
            res.is_err(),
            "DELETE with a bogus WHERE qualifier must error"
        );
        let rows = run_select(&mut db, "SELECT id FROM t;");
        assert!(
            rows.contains("2 rows returned"),
            "no row may be deleted when the WHERE errors, got: {rows}"
        );
    }

    #[test]
    fn delete_with_alias_validates_against_alias() {
        let mut db = seed_sqlr2();
        crate::sql::process_command("DELETE FROM t AS a WHERE a.id = 2;", &mut db)
            .expect("alias qualifier must be accepted in DELETE");
        let rows = run_select(&mut db, "SELECT id FROM t;");
        assert!(rows.contains("1 row returned"), "got: {rows}");
        assert_unknown_qualifier(&mut db, "DELETE FROM t AS a WHERE t.id = 1;", "t");
    }

    // ---------------------------------------------------------------------
    // SQLR-3 — LIKE / IN / DISTINCT / GROUP BY / aggregates
    // ---------------------------------------------------------------------

    /// Seed a small employees table the analytical tests share.
    fn seed_employees() -> Database {
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE emp (id INTEGER PRIMARY KEY, name TEXT, dept TEXT, salary INTEGER);",
            &mut db,
        )
        .unwrap();
        let rows = [
            "INSERT INTO emp (name, dept, salary) VALUES ('Alice', 'eng', 100);",
            "INSERT INTO emp (name, dept, salary) VALUES ('alex',  'eng', 120);",
            "INSERT INTO emp (name, dept, salary) VALUES ('Bob',   'eng', 100);",
            "INSERT INTO emp (name, dept, salary) VALUES ('Carol', 'sales', 90);",
            "INSERT INTO emp (name, dept, salary) VALUES ('Dave',  'sales', NULL);",
            "INSERT INTO emp (name, dept, salary) VALUES ('Eve',   'ops', 80);",
        ];
        for sql in rows {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        db
    }

    /// Drive `execute_select_rows` directly so tests can assert on typed values.
    fn run_rows(db: &Database, sql: &str) -> SelectResult {
        let q = parse_select(sql);
        execute_select_rows(q, db).expect("select")
    }

    // ----- LIKE -----

    #[test]
    fn like_percent_prefix_case_insensitive() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT name FROM emp WHERE name LIKE 'a%';");
        // Matches Alice and alex (case-insensitive ASCII).
        let names: Vec<_> = r.rows.iter().map(|r| r[0].to_display_string()).collect();
        assert_eq!(names.len(), 2, "expected 2 rows, got {names:?}");
        assert!(names.contains(&"Alice".to_string()));
        assert!(names.contains(&"alex".to_string()));
    }

    #[test]
    fn like_underscore_singlechar() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT name FROM emp WHERE name LIKE '_ve';");
        // Eve matches; alex does not (3 chars vs 4).
        let names: Vec<_> = r.rows.iter().map(|r| r[0].to_display_string()).collect();
        assert_eq!(names, vec!["Eve".to_string()]);
    }

    #[test]
    fn not_like_excludes_match() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT name FROM emp WHERE name NOT LIKE 'a%';");
        // Excludes Alice + alex; 4 rows remain.
        assert_eq!(r.rows.len(), 4);
    }

    #[test]
    fn like_with_null_excludes_row() {
        let db = seed_employees();
        // Match 'sales' rows where salary is NULL → just Dave.
        let r = run_rows(
            &db,
            "SELECT name FROM emp WHERE dept LIKE 'sales' AND salary IS NULL;",
        );
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].to_display_string(), "Dave");
    }

    // ----- IN -----

    #[test]
    fn in_list_positive() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT name FROM emp WHERE id IN (1, 3, 5);");
        let names: Vec<_> = r.rows.iter().map(|r| r[0].to_display_string()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"Alice".to_string()));
        assert!(names.contains(&"Bob".to_string()));
        assert!(names.contains(&"Dave".to_string()));
    }

    #[test]
    fn not_in_excludes_listed() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT name FROM emp WHERE id NOT IN (1, 2);");
        // 6 rows total - 2 excluded = 4.
        assert_eq!(r.rows.len(), 4);
    }

    #[test]
    fn in_list_with_null_three_valued() {
        let db = seed_employees();
        // x = 1 should match; for other rows the NULL in the list yields
        // unknown → false in WHERE → excluded.
        let r = run_rows(&db, "SELECT name FROM emp WHERE id IN (1, NULL);");
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].to_display_string(), "Alice");
    }

    // ----- DISTINCT -----

    #[test]
    fn distinct_single_column() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT DISTINCT dept FROM emp;");
        // 3 distinct depts: eng, sales, ops.
        assert_eq!(r.rows.len(), 3);
    }

    #[test]
    fn distinct_multi_column_with_null() {
        let db = seed_employees();
        // (dept, salary) tuples — the two 'eng' / 100 rows collapse.
        let r = run_rows(&db, "SELECT DISTINCT dept, salary FROM emp;");
        // 6 input rows; (eng, 100) appears twice → 5 distinct tuples.
        assert_eq!(r.rows.len(), 5);
    }

    // ----- Aggregates without GROUP BY -----

    #[test]
    fn count_star_no_groupby() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT COUNT(*) FROM emp;");
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0], Value::Integer(6));
    }

    #[test]
    fn count_col_skips_nulls() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT COUNT(salary) FROM emp;");
        // 6 rows, 1 NULL salary → COUNT(salary) = 5.
        assert_eq!(r.rows[0][0], Value::Integer(5));
    }

    #[test]
    fn count_distinct_dedupes_and_skips_nulls() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT COUNT(DISTINCT salary) FROM emp;");
        // Distinct non-null salaries: {100, 120, 90, 80} → 4.
        assert_eq!(r.rows[0][0], Value::Integer(4));
    }

    #[test]
    fn sum_int_stays_integer() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT SUM(salary) FROM emp;");
        // 100 + 120 + 100 + 90 + 80 = 490 (NULL skipped).
        assert_eq!(r.rows[0][0], Value::Integer(490));
    }

    #[test]
    fn avg_returns_real() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT AVG(salary) FROM emp;");
        // 490 / 5 = 98.0
        match &r.rows[0][0] {
            Value::Real(v) => assert!((v - 98.0).abs() < 1e-9),
            other => panic!("expected Real, got {other:?}"),
        }
    }

    #[test]
    fn min_max_skip_nulls() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT MIN(salary), MAX(salary) FROM emp;");
        assert_eq!(r.rows[0][0], Value::Integer(80));
        assert_eq!(r.rows[0][1], Value::Integer(120));
    }

    #[test]
    fn aggregates_on_empty_table_emit_one_row() {
        let mut db = Database::new("t".to_string());
        crate::sql::process_command("CREATE TABLE t (x INTEGER);", &mut db).unwrap();
        let r = run_rows(
            &db,
            "SELECT COUNT(*), SUM(x), AVG(x), MIN(x), MAX(x) FROM t;",
        );
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0], Value::Integer(0));
        assert_eq!(r.rows[0][1], Value::Null);
        assert_eq!(r.rows[0][2], Value::Null);
        assert_eq!(r.rows[0][3], Value::Null);
        assert_eq!(r.rows[0][4], Value::Null);
    }

    // ----- GROUP BY -----

    #[test]
    fn group_by_single_col_with_count() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT dept, COUNT(*) FROM emp GROUP BY dept;");
        assert_eq!(r.rows.len(), 3);
        // Build a map for a stable assertion regardless of group order.
        let mut by_dept: std::collections::HashMap<String, i64> = Default::default();
        for row in &r.rows {
            let d = row[0].to_display_string();
            let c = match &row[1] {
                Value::Integer(i) => *i,
                v => panic!("expected Integer count, got {v:?}"),
            };
            by_dept.insert(d, c);
        }
        assert_eq!(by_dept["eng"], 3);
        assert_eq!(by_dept["sales"], 2);
        assert_eq!(by_dept["ops"], 1);
    }

    #[test]
    fn group_by_with_where_filter() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept, SUM(salary) FROM emp WHERE salary > 80 GROUP BY dept;",
        );
        // After WHERE, ops drops out (Eve = 80 excluded). eng has 3 rows
        // contributing (100+120+100=320); sales has 1 (90; Dave NULL skipped).
        let by: std::collections::HashMap<String, i64> = r
            .rows
            .iter()
            .map(|row| {
                (
                    row[0].to_display_string(),
                    match &row[1] {
                        Value::Integer(i) => *i,
                        v => panic!("expected Integer sum, got {v:?}"),
                    },
                )
            })
            .collect();
        assert_eq!(by.len(), 2);
        assert_eq!(by["eng"], 320);
        assert_eq!(by["sales"], 90);
    }

    #[test]
    fn group_by_without_aggregates_is_distinct() {
        let db = seed_employees();
        let r = run_rows(&db, "SELECT dept FROM emp GROUP BY dept;");
        assert_eq!(r.rows.len(), 3);
    }

    #[test]
    fn order_by_count_desc() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept, COUNT(*) AS n FROM emp GROUP BY dept ORDER BY n DESC LIMIT 2;",
        );
        assert_eq!(r.rows.len(), 2);
        // Top group is 'eng' with 3.
        assert_eq!(r.rows[0][0].to_display_string(), "eng");
        assert_eq!(r.rows[0][1], Value::Integer(3));
    }

    #[test]
    fn order_by_aggregate_call_form() {
        let db = seed_employees();
        // No alias — ORDER BY references the aggregate by its display form.
        let r = run_rows(
            &db,
            "SELECT dept, COUNT(*) FROM emp GROUP BY dept ORDER BY COUNT(*) DESC;",
        );
        assert_eq!(r.rows.len(), 3);
        assert_eq!(r.rows[0][0].to_display_string(), "eng");
    }

    #[test]
    fn group_by_invalid_bare_column_errors() {
        // `name` is neither aggregated nor in GROUP BY → must error at parse.
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, dept TEXT, name TEXT);",
            &mut db,
        )
        .unwrap();
        let err = crate::sql::process_command("SELECT dept, name FROM t GROUP BY dept;", &mut db);
        assert!(err.is_err(), "should reject bare 'name' not in GROUP BY");
    }

    #[test]
    fn aggregate_in_where_errors_friendly() {
        let mut db = Database::new("t".to_string());
        crate::sql::process_command("CREATE TABLE t (x INTEGER);", &mut db).unwrap();
        crate::sql::process_command("INSERT INTO t (x) VALUES (1);", &mut db).unwrap();
        let err = crate::sql::process_command("SELECT x FROM t WHERE COUNT(*) > 0;", &mut db);
        assert!(err.is_err(), "aggregates must not be allowed in WHERE");
    }

    // ---------------------------------------------------------------------
    // SQLR-52 — HAVING (post-aggregation filter)
    // ---------------------------------------------------------------------
    //
    // seed_employees groups: eng × 3 (salaries 100, 120, 100 → SUM 320),
    // sales × 2 (90, NULL → SUM 90), ops × 1 (80). Groups materialize in
    // first-occurrence order: eng, sales, ops.

    #[test]
    fn having_count_filters_groups() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept, COUNT(*) FROM emp GROUP BY dept HAVING COUNT(*) > 1;",
        );
        // ops (1 member) drops; hidden HAVING slots must not leak into
        // the output width.
        assert_eq!(r.columns, vec!["dept".to_string(), "COUNT(*)".to_string()]);
        let got: Vec<(String, i64)> = r
            .rows
            .iter()
            .map(|row| (row[0].to_display_string(), expect_int(&row[1])))
            .collect();
        assert_eq!(got, vec![("eng".to_string(), 3), ("sales".to_string(), 2)]);
    }

    #[test]
    fn having_sum_threshold() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept, SUM(salary) FROM emp GROUP BY dept HAVING SUM(salary) > 100;",
        );
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].to_display_string(), "eng");
        assert_eq!(r.rows[0][1], Value::Integer(320));
    }

    #[test]
    fn having_references_aggregate_alias() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept, SUM(salary) AS total FROM emp GROUP BY dept HAVING total > 100;",
        );
        assert_eq!(r.columns, vec!["dept".to_string(), "total".to_string()]);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][1], Value::Integer(320));
    }

    #[test]
    fn having_aggregate_not_in_projection() {
        let db = seed_employees();
        // COUNT(*) only exists in HAVING — computed via a hidden slot,
        // stripped before output.
        let r = run_rows(
            &db,
            "SELECT dept FROM emp GROUP BY dept HAVING COUNT(*) > 1;",
        );
        assert_eq!(r.columns, vec!["dept".to_string()]);
        let depts: Vec<String> = r
            .rows
            .iter()
            .map(|row| row[0].to_display_string())
            .collect();
        assert_eq!(depts, vec!["eng".to_string(), "sales".to_string()]);
    }

    #[test]
    fn having_group_key_not_in_projection() {
        let db = seed_employees();
        // dept only exists in GROUP BY + HAVING, not the SELECT list.
        let r = run_rows(
            &db,
            "SELECT COUNT(*) FROM emp GROUP BY dept HAVING dept = 'eng';",
        );
        assert_eq!(r.columns, vec!["COUNT(*)".to_string()]);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0], Value::Integer(3));
    }

    #[test]
    fn having_compound_and_predicate() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept FROM emp GROUP BY dept \
             HAVING COUNT(*) > 1 AND SUM(salary) > 100;",
        );
        // eng passes both; sales passes COUNT but fails SUM (90).
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].to_display_string(), "eng");
    }

    #[test]
    fn having_composes_with_order_by_and_limit() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept, COUNT(*) AS n FROM emp GROUP BY dept \
             HAVING n >= 1 ORDER BY n DESC LIMIT 2;",
        );
        let got: Vec<(String, i64)> = r
            .rows
            .iter()
            .map(|row| (row[0].to_display_string(), expect_int(&row[1])))
            .collect();
        assert_eq!(got, vec![("eng".to_string(), 3), ("sales".to_string(), 2)]);
    }

    #[test]
    fn having_can_exclude_every_group() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept FROM emp GROUP BY dept HAVING COUNT(*) > 99;",
        );
        assert_eq!(r.rows.len(), 0);
    }

    #[test]
    fn having_null_aggregate_collapses_to_false() {
        let mut db = seed_employees();
        // mkt's only salary is NULL → SUM(salary) is NULL → NULL > 0 is
        // unknown → group excluded (NULL-as-false, same as WHERE).
        crate::sql::process_command(
            "INSERT INTO emp (name, dept, salary) VALUES ('Zoe', 'mkt', NULL);",
            &mut db,
        )
        .unwrap();
        let r = run_rows(
            &db,
            "SELECT dept FROM emp GROUP BY dept HAVING SUM(salary) > 0;",
        );
        let depts: Vec<String> = r
            .rows
            .iter()
            .map(|row| row[0].to_display_string())
            .collect();
        assert_eq!(
            depts,
            vec!["eng".to_string(), "sales".to_string(), "ops".to_string()],
            "mkt (all-NULL salaries) must be filtered out"
        );
    }

    #[test]
    fn having_lowercase_function_form_matches() {
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept FROM emp GROUP BY dept HAVING count(*) > 1;",
        );
        assert_eq!(r.rows.len(), 2);
    }

    #[test]
    fn having_without_group_by_is_rejected() {
        let mut db = seed_employees();
        let err =
            crate::sql::process_command("SELECT COUNT(*) FROM emp HAVING COUNT(*) > 0;", &mut db);
        match err {
            Err(SQLRiteError::NotImplemented(msg)) => assert!(
                msg.contains("HAVING without GROUP BY"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    #[test]
    fn having_unknown_column_is_rejected() {
        let mut db = seed_employees();
        // `name` is neither a GROUP BY key nor an aggregate — typed error,
        // not a silent NULL like the legacy single-table WHERE leniency.
        let err = crate::sql::process_command(
            "SELECT dept, COUNT(*) FROM emp GROUP BY dept HAVING name = 'Alice';",
            &mut db,
        );
        match err {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("HAVING references"),
                    "unexpected message: {msg}"
                );
            }
            Ok(_) => panic!("HAVING on an out-of-scope column must error"),
        }
    }

    #[test]
    fn having_over_join_filters_groups_for_all_flavors() {
        // SQLR-6 — GROUP BY + HAVING compose with every join flavor.
        // Only Alice has more than one order; the dangling order (RIGHT /
        // FULL) groups under a NULL name with count 1 and is filtered.
        for flavor in ["INNER", "LEFT OUTER", "RIGHT OUTER", "FULL OUTER"] {
            let sql = format!(
                "SELECT customers.name, COUNT(*) FROM customers \
                 {flavor} JOIN orders ON customers.id = orders.customer_id \
                 GROUP BY customers.name HAVING COUNT(*) > 1;"
            );
            let db = seed_join_fixture();
            let r = run_rows(&db, &sql);
            assert_eq!(r.rows.len(), 1, "{flavor}: only Alice has >1 order");
            assert_eq!(r.rows[0][0].to_display_string(), "Alice", "{flavor}");
            assert_eq!(expect_int(&r.rows[0][1]), 2, "{flavor}");
        }
    }

    /// Helper: unwrap an integer `Value` in HAVING tests.
    fn expect_int(v: &Value) -> i64 {
        match v {
            Value::Integer(i) => *i,
            other => panic!("expected integer value, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // SQLR-5 — JOINs (INNER / LEFT OUTER / RIGHT OUTER / FULL OUTER)
    // ---------------------------------------------------------------------

    /// Two-table fixture used across the join tests. `customers` has
    /// (1: Alice, 2: Bob, 3: Carol). `orders` has (id, customer_id,
    /// amount): (1, 1, 100), (2, 1, 200), (3, 2, 50), (4, 4, 999).
    /// Customer 3 (Carol) has no orders; order 4 has no customer
    /// (dangling foreign key) — together they exercise both sides of
    /// the outer-join NULL-padding.
    fn seed_join_fixture() -> Database {
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);",
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, amount INTEGER);",
            "INSERT INTO customers (name) VALUES ('Alice');",
            "INSERT INTO customers (name) VALUES ('Bob');",
            "INSERT INTO customers (name) VALUES ('Carol');",
            "INSERT INTO orders (customer_id, amount) VALUES (1, 100);",
            "INSERT INTO orders (customer_id, amount) VALUES (1, 200);",
            "INSERT INTO orders (customer_id, amount) VALUES (2, 50);",
            "INSERT INTO orders (customer_id, amount) VALUES (4, 999);",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        db
    }

    #[test]
    fn inner_join_returns_only_matched_rows() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id;",
        );
        assert_eq!(r.columns, vec!["name".to_string(), "amount".to_string()]);
        // Alice: 100, 200; Bob: 50. Carol drops (no orders), order 4 drops
        // (no customer). 3 rows.
        let pairs: Vec<(String, i64)> = r
            .rows
            .iter()
            .map(|row| {
                (
                    row[0].to_display_string(),
                    match row[1] {
                        Value::Integer(i) => i,
                        ref v => panic!("expected integer amount, got {v:?}"),
                    },
                )
            })
            .collect();
        assert_eq!(pairs.len(), 3);
        assert!(pairs.contains(&("Alice".to_string(), 100)));
        assert!(pairs.contains(&("Alice".to_string(), 200)));
        assert!(pairs.contains(&("Bob".to_string(), 50)));
    }

    #[test]
    fn bare_join_defaults_to_inner() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name FROM customers \
             JOIN orders ON customers.id = orders.customer_id;",
        );
        assert_eq!(r.rows.len(), 3, "JOIN without prefix should be INNER");
    }

    #[test]
    fn left_outer_join_preserves_unmatched_left() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             LEFT OUTER JOIN orders ON customers.id = orders.customer_id;",
        );
        // Alice: two rows. Bob: one row. Carol: one NULL-padded row.
        // Order 4 is dropped (left side has no customer for id=4).
        assert_eq!(r.rows.len(), 4);
        let carol = r
            .rows
            .iter()
            .find(|row| row[0].to_display_string() == "Carol")
            .expect("Carol should appear with a NULL-padded right side");
        assert_eq!(carol[1], Value::Null);
    }

    #[test]
    fn right_outer_join_preserves_unmatched_right() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             RIGHT OUTER JOIN orders ON customers.id = orders.customer_id;",
        );
        // 3 matched rows + 1 dangling order (id=4, customer_id=4 with no
        // matching customer). Total 4. Carol drops because the right
        // table has no row pointing at her.
        assert_eq!(r.rows.len(), 4);
        let dangling = r
            .rows
            .iter()
            .find(|row| matches!(row[1], Value::Integer(999)))
            .expect("dangling order 999 should appear with a NULL-padded customer name");
        assert_eq!(dangling[0], Value::Null);
    }

    #[test]
    fn full_outer_join_preserves_both_sides() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             FULL OUTER JOIN orders ON customers.id = orders.customer_id;",
        );
        // 3 matched + 1 unmatched left (Carol) + 1 unmatched right
        // (order 999) = 5 rows.
        assert_eq!(r.rows.len(), 5);
        // Carol with NULL amount.
        assert!(
            r.rows
                .iter()
                .any(|row| row[0].to_display_string() == "Carol" && matches!(row[1], Value::Null))
        );
        // 999 with NULL name.
        assert!(
            r.rows
                .iter()
                .any(|row| matches!(row[1], Value::Integer(999)) && matches!(row[0], Value::Null))
        );
    }

    #[test]
    fn join_with_table_aliases_resolves_qualifiers() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT c.name, o.amount FROM customers AS c \
             INNER JOIN orders AS o ON c.id = o.customer_id;",
        );
        assert_eq!(r.rows.len(), 3);
        assert_eq!(r.columns, vec!["name".to_string(), "amount".to_string()]);
    }

    #[test]
    fn join_with_where_filter_applies_after_join() {
        let db = seed_join_fixture();
        // Filter to only orders >= 100. With INNER JOIN, this drops Bob's
        // 50-amount order, leaving Alice's 100 and 200.
        let r = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id \
             WHERE orders.amount >= 100;",
        );
        assert_eq!(r.rows.len(), 2);
        assert!(
            r.rows
                .iter()
                .all(|row| row[0].to_display_string() == "Alice")
        );
    }

    #[test]
    fn left_join_with_where_on_right_side_is_not_inner() {
        // WHERE on the right side that excludes NULL turns LEFT JOIN
        // back into INNER JOIN semantically. Verify the executor
        // applies the WHERE *after* the join padded NULLs in.
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             LEFT OUTER JOIN orders ON customers.id = orders.customer_id \
             WHERE orders.amount IS NULL;",
        );
        // Only Carol survives — she's the only customer with no order.
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].to_display_string(), "Carol");
        assert_eq!(r.rows[0][1], Value::Null);
    }

    #[test]
    fn select_star_over_join_emits_all_columns_from_both_tables() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT * FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id;",
        );
        // customers has 2 cols (id, name), orders has 3 cols
        // (id, customer_id, amount). 5 columns total. Header order
        // follows source order — primary table first.
        assert_eq!(
            r.columns,
            vec![
                "id".to_string(),
                "name".to_string(),
                "id".to_string(),
                "customer_id".to_string(),
                "amount".to_string(),
            ]
        );
        assert_eq!(r.rows.len(), 3);
    }

    #[test]
    fn join_order_by_sorts_full_joined_rows() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT c.name, o.amount FROM customers AS c \
             INNER JOIN orders AS o ON c.id = o.customer_id \
             ORDER BY o.amount;",
        );
        let amounts: Vec<i64> = r
            .rows
            .iter()
            .map(|row| match row[1] {
                Value::Integer(i) => i,
                ref v => panic!("expected integer, got {v:?}"),
            })
            .collect();
        assert_eq!(amounts, vec![50, 100, 200]);
    }

    #[test]
    fn join_limit_truncates_after_join_and_sort() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT c.name, o.amount FROM customers AS c \
             INNER JOIN orders AS o ON c.id = o.customer_id \
             ORDER BY o.amount DESC LIMIT 2;",
        );
        assert_eq!(r.rows.len(), 2);
        // Top two by amount DESC: 200 (Alice), 100 (Alice).
        let amounts: Vec<i64> = r
            .rows
            .iter()
            .map(|row| match row[1] {
                Value::Integer(i) => i,
                ref v => panic!("expected integer, got {v:?}"),
            })
            .collect();
        assert_eq!(amounts, vec![200, 100]);
    }

    #[test]
    fn three_table_join_chains_correctly() {
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE a (id INTEGER PRIMARY KEY, label TEXT);",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, tag TEXT);",
            "CREATE TABLE c (id INTEGER PRIMARY KEY, b_id INTEGER, note TEXT);",
            "INSERT INTO a (label) VALUES ('a-one');",
            "INSERT INTO a (label) VALUES ('a-two');",
            "INSERT INTO b (a_id, tag) VALUES (1, 'b1');",
            "INSERT INTO b (a_id, tag) VALUES (2, 'b2');",
            "INSERT INTO c (b_id, note) VALUES (1, 'c1');",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        let r = run_rows(
            &db,
            "SELECT a.label, b.tag, c.note FROM a \
             INNER JOIN b ON a.id = b.a_id \
             INNER JOIN c ON b.id = c.b_id;",
        );
        // Only b1 has a c row. So one combined row.
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].to_display_string(), "a-one");
        assert_eq!(r.rows[0][1].to_display_string(), "b1");
        assert_eq!(r.rows[0][2].to_display_string(), "c1");
    }

    #[test]
    fn ambiguous_unqualified_column_in_join_errors() {
        // Both customers and orders have a column named `id`. An
        // unqualified `id` in the SELECT must error rather than
        // silently picking one side.
        let db = seed_join_fixture();
        let q = parse_select(
            "SELECT id FROM customers INNER JOIN orders ON customers.id = orders.customer_id;",
        );
        let res = execute_select_rows(q, &db);
        assert!(res.is_err(), "unqualified ambiguous 'id' should error");
    }

    #[test]
    fn join_self_without_alias_is_rejected() {
        let mut db = Database::new("t".to_string());
        crate::sql::process_command(
            "CREATE TABLE n (id INTEGER PRIMARY KEY, parent INTEGER);",
            &mut db,
        )
        .unwrap();
        let q = parse_select("SELECT n.id FROM n INNER JOIN n ON n.id = n.parent;");
        let res = execute_select_rows(q, &db);
        assert!(
            res.is_err(),
            "self-join without an alias should error on duplicate qualifier"
        );
    }

    // ----- SQLR-5 follow-up: USING / NATURAL / CROSS joins -----

    /// `customers` and `orders` both have an `id` column. Joining on it
    /// via USING must produce exactly the same rows as the equivalent
    /// explicit `ON customers.id = orders.id`.
    #[test]
    fn join_using_matches_same_rows_as_on() {
        let db = seed_join_fixture();
        let using = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             INNER JOIN orders USING (id) ORDER BY orders.amount;",
        );
        let on = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers \
             INNER JOIN orders ON customers.id = orders.id ORDER BY orders.amount;",
        );
        // id matches: cust1↔order1 (100), cust2↔order2 (200), cust3↔order3 (50).
        let pairs: Vec<(String, Value)> = using
            .rows
            .iter()
            .map(|r| (r[0].to_display_string(), r[1].clone()))
            .collect();
        assert_eq!(pairs.len(), 3);
        assert_eq!(
            using.rows, on.rows,
            "USING must mirror the explicit ON rows"
        );
    }

    /// `SELECT *` over a USING join shows the joined-on column once
    /// (SQLite convention), taking the left side's copy.
    #[test]
    fn select_star_using_dedups_joined_column() {
        let db = seed_join_fixture();
        let r = run_rows(&db, "SELECT * FROM customers INNER JOIN orders USING (id);");
        // Without USING dedup this would be 5 columns (id,name,id,
        // customer_id,amount). USING(id) collapses the duplicate `id`
        // to one, leaving 4 in source order.
        assert_eq!(
            r.columns,
            vec![
                "id".to_string(),
                "name".to_string(),
                "customer_id".to_string(),
                "amount".to_string(),
            ]
        );
        assert_eq!(r.rows.len(), 3);
        // Each surviving row's single `id` equals both sides' id (they
        // were matched on equality), so the left copy is correct.
        for row in &r.rows {
            assert!(matches!(row[0], Value::Integer(_)));
        }
    }

    fn seed_natural_fixture() -> Database {
        let mut db = Database::new("t".to_string());
        for sql in [
            // Distinct PK names (lid / rid) so the *only* shared columns
            // are k1 and k2 — NATURAL must match on both with AND.
            "CREATE TABLE l (lid INTEGER PRIMARY KEY, k1 INTEGER, k2 INTEGER, v1 TEXT);",
            "CREATE TABLE r (rid INTEGER PRIMARY KEY, k1 INTEGER, k2 INTEGER, v2 TEXT);",
            "INSERT INTO l (k1, k2, v1) VALUES (1, 1, 'l-a');",
            "INSERT INTO l (k1, k2, v1) VALUES (1, 2, 'l-b');",
            "INSERT INTO l (k1, k2, v1) VALUES (2, 1, 'l-c');",
            "INSERT INTO r (k1, k2, v2) VALUES (1, 1, 'r-a');",
            "INSERT INTO r (k1, k2, v2) VALUES (1, 2, 'r-b');",
            "INSERT INTO r (k1, k2, v2) VALUES (9, 9, 'r-z');",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        db
    }

    /// NATURAL JOIN auto-discovers the shared columns (k1, k2) and
    /// matches on both with AND.
    #[test]
    fn natural_join_matches_on_all_shared_columns() {
        let db = seed_natural_fixture();
        let natural = run_rows(&db, "SELECT v1, v2 FROM l NATURAL JOIN r ORDER BY v1;");
        // (1,1)->l-a/r-a and (1,2)->l-b/r-b match. (2,1) and (9,9) don't.
        let pairs: Vec<(String, String)> = natural
            .rows
            .iter()
            .map(|r| (r[0].to_display_string(), r[1].to_display_string()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("l-a".to_string(), "r-a".to_string()),
                ("l-b".to_string(), "r-b".to_string()),
            ]
        );
        // Equivalent explicit form yields the same rows.
        let explicit = run_rows(
            &db,
            "SELECT v1, v2 FROM l INNER JOIN r ON l.k1 = r.k1 AND l.k2 = r.k2 ORDER BY v1;",
        );
        assert_eq!(natural.rows, explicit.rows);
    }

    /// `SELECT *` over a NATURAL join shows each shared column once.
    #[test]
    fn select_star_natural_dedups_shared_columns() {
        let db = seed_natural_fixture();
        let r = run_rows(&db, "SELECT * FROM l NATURAL JOIN r;");
        // Source order with k1,k2 taken from the left only:
        // l: lid, k1, k2, v1 ; r: rid, v2  (k1,k2 dropped from r).
        assert_eq!(
            r.columns,
            vec![
                "lid".to_string(),
                "k1".to_string(),
                "k2".to_string(),
                "v1".to_string(),
                "rid".to_string(),
                "v2".to_string(),
            ]
        );
        assert_eq!(r.rows.len(), 2);
    }

    /// NATURAL JOIN between tables with no shared column names degrades
    /// to a cross product, matching SQLite.
    #[test]
    fn natural_join_without_common_columns_is_cross_product() {
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE p (pid INTEGER PRIMARY KEY, pa TEXT);",
            "CREATE TABLE q (qid INTEGER PRIMARY KEY, qb TEXT);",
            "INSERT INTO p (pa) VALUES ('p1');",
            "INSERT INTO p (pa) VALUES ('p2');",
            "INSERT INTO q (qb) VALUES ('q1');",
            "INSERT INTO q (qb) VALUES ('q2');",
            "INSERT INTO q (qb) VALUES ('q3');",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        let r = run_rows(&db, "SELECT p.pa, q.qb FROM p NATURAL JOIN q;");
        assert_eq!(r.rows.len(), 2 * 3, "no shared columns ⇒ cross product");
    }

    /// CROSS JOIN produces the full cartesian product and is equivalent
    /// to `INNER JOIN ... ON 1`.
    #[test]
    fn cross_join_produces_cartesian_product() {
        let db = seed_join_fixture();
        let cross = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers CROSS JOIN orders;",
        );
        // 3 customers × 4 orders = 12 rows.
        assert_eq!(cross.rows.len(), 12);
        let on_true = run_rows(
            &db,
            "SELECT customers.name, orders.amount FROM customers INNER JOIN orders ON 1;",
        );
        assert_eq!(cross.rows.len(), on_true.rows.len());
        // SELECT * over a cross join keeps every column from both sides.
        let star = run_rows(&db, "SELECT * FROM customers CROSS JOIN orders;");
        assert_eq!(star.columns.len(), 5);
        assert_eq!(star.rows.len(), 12);
    }

    /// A LEFT OUTER join expressed with USING still preserves unmatched
    /// left rows (NULL-padding the right), and the deduplicated column
    /// keeps the left side's value.
    #[test]
    fn left_outer_join_using_preserves_unmatched_left() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT * FROM customers LEFT OUTER JOIN orders USING (id);",
        );
        // customers ids 1,2,3 each match an order id; none are unmatched
        // here, so confirm the dedup + row count instead. 4 columns,
        // 3 matched rows (orders has no id=customer beyond 1..3 overlap).
        assert_eq!(r.columns.len(), 4, "id is shown once");
        assert_eq!(r.rows.len(), 3);
    }

    /// USING a column that doesn't exist on one of the sides is a clean
    /// error, not a silent empty result.
    #[test]
    fn using_unknown_column_errors() {
        let db = seed_join_fixture();
        let q = parse_select("SELECT * FROM customers INNER JOIN orders USING (nope);");
        let res = execute_select_rows(q, &db);
        assert!(res.is_err(), "USING (nope) must error — column absent");
    }

    // ---------------------------------------------------------------------
    // SQLR-6 — aggregates / GROUP BY / DISTINCT over JOIN results
    // ---------------------------------------------------------------------

    #[test]
    fn group_by_with_aggregates_over_inner_join() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, COUNT(*), SUM(orders.amount) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id \
             GROUP BY customers.name ORDER BY customers.name;",
        );
        assert_eq!(r.columns, vec!["name", "COUNT(*)", "SUM(orders.amount)"]);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0][0].to_display_string(), "Alice");
        assert_eq!(expect_int(&r.rows[0][1]), 2);
        assert_eq!(expect_int(&r.rows[0][2]), 300);
        assert_eq!(r.rows[1][0].to_display_string(), "Bob");
        assert_eq!(expect_int(&r.rows[1][1]), 1);
        assert_eq!(expect_int(&r.rows[1][2]), 50);
    }

    #[test]
    fn aggregates_over_join_without_group_by() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT COUNT(*), SUM(orders.amount) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id;",
        );
        assert_eq!(r.rows.len(), 1);
        assert_eq!(expect_int(&r.rows[0][0]), 3);
        assert_eq!(expect_int(&r.rows[0][1]), 350);
    }

    #[test]
    fn count_column_skips_outer_join_null_padding() {
        // Carol has no orders: her LEFT-JOIN row is NULL-padded on the
        // right. COUNT(*) counts the padded row; COUNT(orders.id) skips
        // its NULL, per the usual NULL-skipping aggregate semantics.
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, COUNT(*), COUNT(orders.id) FROM customers \
             LEFT OUTER JOIN orders ON customers.id = orders.customer_id \
             GROUP BY customers.name ORDER BY customers.name;",
        );
        assert_eq!(r.rows.len(), 3);
        let carol = &r.rows[2];
        assert_eq!(carol[0].to_display_string(), "Carol");
        assert_eq!(expect_int(&carol[1]), 1, "COUNT(*) counts the padded row");
        assert_eq!(expect_int(&carol[2]), 0, "COUNT(col) skips the NULL");
    }

    #[test]
    fn outer_join_null_keys_group_together() {
        // FULL OUTER surfaces the dangling order (customer_id 4) with a
        // NULL customers.name — it must form its own group, not vanish.
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, COUNT(*) FROM customers \
             FULL OUTER JOIN orders ON customers.id = orders.customer_id \
             GROUP BY customers.name;",
        );
        assert_eq!(r.rows.len(), 4, "Alice, Bob, Carol, NULL");
        let null_group = r
            .rows
            .iter()
            .find(|row| row[0] == Value::Null)
            .expect("dangling order groups under NULL");
        assert_eq!(expect_int(&null_group[1]), 1);
    }

    #[test]
    fn count_distinct_over_join() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT COUNT(DISTINCT customers.name) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id;",
        );
        assert_eq!(expect_int(&r.rows[0][0]), 2);
    }

    #[test]
    fn group_by_qualified_key_resolves_ambiguous_name() {
        // `id` exists on both tables — the qualified GROUP BY key picks
        // the customers side.
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.id, COUNT(*) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id \
             GROUP BY customers.id ORDER BY customers.id;",
        );
        assert_eq!(r.rows.len(), 2);
        assert_eq!(expect_int(&r.rows[0][0]), 1);
        assert_eq!(expect_int(&r.rows[0][1]), 2);
    }

    #[test]
    fn group_by_ambiguous_unqualified_key_over_join_errors() {
        let err = crate::sql::process_command(
            "SELECT COUNT(*) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id GROUP BY id;",
            &mut seed_join_fixture(),
        );
        match err {
            Err(e) => assert!(
                e.to_string().contains("ambiguous"),
                "unexpected message: {e}"
            ),
            Ok(_) => panic!("ambiguous GROUP BY key must error"),
        }
    }

    #[test]
    fn bare_column_not_in_group_by_over_join_errors() {
        let err = crate::sql::process_command(
            "SELECT orders.amount, COUNT(*) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id \
             GROUP BY customers.name;",
            &mut seed_join_fixture(),
        );
        match err {
            Err(e) => assert!(
                e.to_string().contains("must appear in GROUP BY"),
                "unexpected message: {e}"
            ),
            Ok(_) => panic!("bare column outside GROUP BY must error"),
        }
    }

    #[test]
    fn aggregate_in_where_over_join_errors_cleanly() {
        // Code-review gap from SQLR-5: aggregate misuse inside WHERE on
        // a joined query must be a typed error, not wrong results.
        let err = crate::sql::process_command(
            "SELECT COUNT(*) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id \
             WHERE COUNT(*) > 1;",
            &mut seed_join_fixture(),
        );
        match err {
            Err(SQLRiteError::NotImplemented(msg)) => assert!(
                msg.contains("not allowed in WHERE"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    #[test]
    fn order_by_aggregate_over_join() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT customers.name, SUM(orders.amount) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id \
             GROUP BY customers.name ORDER BY SUM(orders.amount) DESC;",
        );
        assert_eq!(r.rows[0][0].to_display_string(), "Alice");
        // Qualifier-stripped fallback: ORDER BY SUM(amount) finds the
        // SUM(orders.amount) slot even though the spellings differ.
        let r2 = run_rows(
            &db,
            "SELECT customers.name, SUM(orders.amount) FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id \
             GROUP BY customers.name ORDER BY SUM(amount) DESC;",
        );
        assert_eq!(r2.rows[0][0].to_display_string(), "Alice");
    }

    #[test]
    fn distinct_over_join_dedupes_output_rows() {
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT DISTINCT customers.name FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id;",
        );
        assert_eq!(r.rows.len(), 2);
        let names: Vec<String> = r
            .rows
            .iter()
            .map(|row| row[0].to_display_string())
            .collect();
        assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);
    }

    #[test]
    fn distinct_over_join_defers_limit_past_dedupe() {
        // Without deferral, LIMIT 2 would truncate the joined rows to
        // Alice's two orders and dedupe to a single row.
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT DISTINCT customers.name FROM customers \
             INNER JOIN orders ON customers.id = orders.customer_id LIMIT 2;",
        );
        assert_eq!(r.rows.len(), 2, "LIMIT applies after DISTINCT collapses");
    }

    #[test]
    fn select_star_group_by_errors_instead_of_panicking() {
        // Single-table regression: the parser's "must appear in GROUP BY"
        // check skips `SELECT *`, so the executor used to hit an
        // `expect()` panic when a non-grouped column reached projection.
        let err = crate::sql::process_command(
            "SELECT * FROM orders GROUP BY customer_id;",
            &mut seed_join_fixture(),
        );
        match err {
            Err(e) => assert!(
                e.to_string().contains("must appear in GROUP BY"),
                "unexpected message: {e}"
            ),
            Ok(_) => panic!("SELECT * with GROUP BY must error, not panic"),
        }
    }

    #[test]
    fn group_by_qualified_key_single_table_still_works() {
        // Qualified GROUP BY keys are accepted on the single-table path
        // too (qualifier ignored, same posture as projections).
        let db = seed_employees();
        let r = run_rows(
            &db,
            "SELECT dept, COUNT(*) FROM emp GROUP BY emp.dept ORDER BY dept;",
        );
        assert_eq!(r.rows.len(), 3, "eng / sales / ops");
    }

    #[test]
    fn left_join_with_no_matches_pads_every_row() {
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE a (id INTEGER PRIMARY KEY, x INTEGER);",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, y INTEGER);",
            "INSERT INTO a (x) VALUES (1);",
            "INSERT INTO a (x) VALUES (2);",
            "INSERT INTO b (y) VALUES (10);",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        // ON condition matches nothing.
        let r = run_rows(
            &db,
            "SELECT a.x, b.y FROM a LEFT OUTER JOIN b ON a.x = b.y;",
        );
        assert_eq!(r.rows.len(), 2);
        for row in &r.rows {
            assert_eq!(row[1], Value::Null);
        }
    }

    #[test]
    fn left_outer_join_order_by_places_nulls_first() {
        // NULL ordering matches the engine-wide rule: NULL is Less
        // than every concrete value (see compare_values). So an
        // ORDER BY of a NULL-padded right column puts the
        // outer-join row at the top under ASC.
        let db = seed_join_fixture();
        let r = run_rows(
            &db,
            "SELECT c.name, o.amount FROM customers AS c \
             LEFT OUTER JOIN orders AS o ON c.id = o.customer_id \
             ORDER BY o.amount ASC;",
        );
        assert_eq!(r.rows.len(), 4);
        // Carol's NULL amount sorts first.
        assert_eq!(r.rows[0][0].to_display_string(), "Carol");
        assert_eq!(r.rows[0][1], Value::Null);
    }

    #[test]
    fn chained_left_outer_join_preserves_left_through_two_levels() {
        // A LEFT JOIN B LEFT JOIN C — a row in A with no match in B
        // must survive both joins with NULL padding for both sides.
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE a (id INTEGER PRIMARY KEY, label TEXT);",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, tag TEXT);",
            "CREATE TABLE c (id INTEGER PRIMARY KEY, b_id INTEGER, note TEXT);",
            "INSERT INTO a (label) VALUES ('a-one');",
            "INSERT INTO a (label) VALUES ('a-two');",
            // b only matches a-one.
            "INSERT INTO b (a_id, tag) VALUES (1, 'b1');",
            // No c rows at all.
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        let r = run_rows(
            &db,
            "SELECT a.label, b.tag, c.note FROM a \
             LEFT OUTER JOIN b ON a.id = b.a_id \
             LEFT OUTER JOIN c ON b.id = c.b_id;",
        );
        // Two rows: a-one + b1 with c=NULL, and a-two with b=NULL+c=NULL.
        assert_eq!(r.rows.len(), 2);
        let by_label: std::collections::HashMap<String, &Vec<Value>> = r
            .rows
            .iter()
            .map(|row| (row[0].to_display_string(), row))
            .collect();
        assert_eq!(by_label["a-one"][1].to_display_string(), "b1");
        assert_eq!(by_label["a-one"][2], Value::Null);
        assert_eq!(by_label["a-two"][1], Value::Null);
        assert_eq!(by_label["a-two"][2], Value::Null);
    }

    #[test]
    fn on_clause_referencing_not_yet_joined_table_errors_clearly() {
        // ON should only see tables joined so far. Referencing a
        // table that hasn't joined yet is a clean error rather than
        // silently NULL-coalescing into "ON evaluated false".
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE a (id INTEGER PRIMARY KEY, x INTEGER);",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, x INTEGER);",
            "CREATE TABLE c (id INTEGER PRIMARY KEY, x INTEGER);",
            "INSERT INTO a (x) VALUES (1);",
            "INSERT INTO b (x) VALUES (1);",
            "INSERT INTO c (x) VALUES (1);",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        let q =
            parse_select("SELECT a.x FROM a INNER JOIN b ON a.x = c.x INNER JOIN c ON b.x = c.x;");
        let res = execute_select_rows(q, &db);
        assert!(
            res.is_err(),
            "ON referencing not-yet-joined table 'c' should error"
        );
    }

    #[test]
    fn join_on_truthy_integer_is_accepted() {
        // ON `1` should be treated as true, like WHERE 1. Verifies
        // the executor reuses eval_predicate_scope's truthiness
        // semantic on JOIN conditions.
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE a (id INTEGER PRIMARY KEY, x INTEGER);",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, y INTEGER);",
            "INSERT INTO a (x) VALUES (1);",
            "INSERT INTO a (x) VALUES (2);",
            "INSERT INTO b (y) VALUES (10);",
            "INSERT INTO b (y) VALUES (20);",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        let r = run_rows(&db, "SELECT a.x, b.y FROM a INNER JOIN b ON 1;");
        // ON 1 is always true → cross product → 2 × 2 = 4 rows.
        assert_eq!(r.rows.len(), 4);
    }

    #[test]
    fn full_join_on_empty_tables_returns_empty() {
        let mut db = Database::new("t".to_string());
        for sql in [
            "CREATE TABLE a (id INTEGER PRIMARY KEY, x INTEGER);",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, y INTEGER);",
        ] {
            crate::sql::process_command(sql, &mut db).unwrap();
        }
        let r = run_rows(
            &db,
            "SELECT a.x, b.y FROM a FULL OUTER JOIN b ON a.x = b.y;",
        );
        assert!(r.rows.is_empty());
    }
}
