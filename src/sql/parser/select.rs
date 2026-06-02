use sqlparser::ast::{
    DuplicateTreatment, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, JoinConstraint,
    JoinOperator, LimitClause, ObjectName, ObjectNamePart, OrderByKind, Query, Select, SelectItem,
    SetExpr, Statement, TableFactor, TableWithJoins, Value,
};

use crate::error::{Result, SQLRiteError};

/// Aggregate function name. v1 covers the SQLite-classic five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateFn {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl AggregateFn {
    pub fn as_str(self) -> &'static str {
        match self {
            AggregateFn::Count => "COUNT",
            AggregateFn::Sum => "SUM",
            AggregateFn::Avg => "AVG",
            AggregateFn::Min => "MIN",
            AggregateFn::Max => "MAX",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "count" => Some(AggregateFn::Count),
            "sum" => Some(AggregateFn::Sum),
            "avg" => Some(AggregateFn::Avg),
            "min" => Some(AggregateFn::Min),
            "max" => Some(AggregateFn::Max),
            _ => None,
        }
    }
}

/// What the aggregate is fed: `*` (only valid for COUNT) or a bare column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateArg {
    Star,
    Column(String),
}

/// A parsed aggregate call like `COUNT(*)`, `SUM(salary)`, `COUNT(DISTINCT dept)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateCall {
    pub func: AggregateFn,
    pub arg: AggregateArg,
    /// `DISTINCT` inside the parens. v1 only allows it on COUNT.
    pub distinct: bool,
}

impl AggregateCall {
    /// Canonical display form used to match ORDER BY expressions against
    /// aggregate output columns when the user didn't supply an alias.
    /// Mirrors the output-header convention.
    pub fn display_name(&self) -> String {
        let inner = match &self.arg {
            AggregateArg::Star => "*".to_string(),
            AggregateArg::Column(c) => {
                if self.distinct {
                    format!("DISTINCT {c}")
                } else {
                    c.clone()
                }
            }
        };
        format!("{}({inner})", self.func.as_str())
    }
}

/// One entry in the projection list.
#[derive(Debug, Clone)]
pub struct ProjectionItem {
    pub kind: ProjectionKind,
    /// `AS alias` if explicitly supplied.
    pub alias: Option<String>,
}

impl ProjectionItem {
    /// Resolve the user-visible column header for this projection item.
    /// Alias if supplied, else the bare column name or aggregate display.
    /// For qualified `t.col` shapes the header is just `col` — this
    /// matches SQLite, where qualifiers don't propagate to output
    /// column names.
    pub fn output_name(&self) -> String {
        if let Some(a) = &self.alias {
            return a.clone();
        }
        match &self.kind {
            ProjectionKind::Column { name, .. } => name.clone(),
            ProjectionKind::Aggregate(a) => a.display_name(),
        }
    }
}

/// What an individual projection item produces.
#[derive(Debug, Clone)]
pub enum ProjectionKind {
    /// Column reference. `qualifier` is `Some` for `t.col` shapes
    /// (SQLR-5 — needed so JOIN execution can disambiguate
    /// same-named columns across tables); `None` for bare `col`.
    /// The single-table path ignores the qualifier and looks up the
    /// name directly, preserving legacy behavior.
    Column {
        qualifier: Option<String>,
        name: String,
    },
    /// Aggregate function call: `COUNT(*)`, `SUM(col)`, etc.
    Aggregate(AggregateCall),
}

/// What columns to project from a SELECT.
#[derive(Debug, Clone)]
pub enum Projection {
    /// `SELECT *` — every column in the table, in declaration order.
    All,
    /// Explicit, ordered projection list — possibly mixing bare columns
    /// with aggregate calls (`SELECT dept, COUNT(*) FROM t`).
    Items(Vec<ProjectionItem>),
}

/// A parsed `ORDER BY` clause: a single sort key (expression), ascending
/// by default. Phase 7b widened this from "bare column name" to
/// "arbitrary expression" so KNN queries of the form
/// `ORDER BY vec_distance_l2(col, [...]) LIMIT k` work end-to-end. The
/// expression is evaluated per-row at execution time via `eval_expr`;
/// the simple `ORDER BY col` form still works because that's just an
/// `Expr::Identifier` taking the same path.
#[derive(Debug, Clone)]
pub struct OrderByClause {
    pub expr: Expr,
    pub ascending: bool,
}

/// SQLR-5 — flavor of join. SQLite ships INNER and LEFT OUTER; we
/// implement the full quartet on top of a single nested-loop driver
/// because the per-flavor differences are small (NULL-padding policy
/// for unmatched left/right rows). RIGHT OUTER and FULL OUTER aren't
/// in SQLite — see `docs/design-decisions.md` for the rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    LeftOuter,
    RightOuter,
    FullOuter,
}

impl JoinType {
    pub fn as_str(self) -> &'static str {
        match self {
            JoinType::Inner => "INNER",
            JoinType::LeftOuter => "LEFT OUTER",
            JoinType::RightOuter => "RIGHT OUTER",
            JoinType::FullOuter => "FULL OUTER",
        }
    }
}

/// How a JOIN matches rows. SQLR-5 originally shipped `ON` only; the
/// USING / NATURAL increment adds the two name-based constraints.
/// `ON` carries its predicate straight from the parser. `USING` and
/// `NATURAL` defer their equality synthesis to the executor because
/// they need table schemas (which column names exist, and — for
/// `NATURAL` — which are shared) that the parser doesn't have. The
/// executor turns both into the same `left.col = right.col [AND …]`
/// predicate the `ON` path already evaluates. `CROSS JOIN` is rewritten
/// to `ON true` at parse time (no schema needed) and so reuses the
/// `On` variant directly.
#[derive(Debug, Clone)]
pub enum JoinConstraintKind {
    /// `ON <expr>` (and the parse-time rewrite of `CROSS JOIN` to
    /// `ON true`). Evaluated per-row over the multi-table scope. Boxed
    /// to keep this enum small — `Expr` dwarfs the other variants.
    On(Box<Expr>),
    /// `USING (col[, col…])` — equality on each named column, plus the
    /// SQLite convention that each named column appears once in
    /// `SELECT *`. Columns are validated and the predicate is
    /// synthesized at execution time.
    Using(Vec<String>),
    /// `NATURAL` — the shared column names of the two sides are
    /// discovered at execution time, then treated exactly like
    /// `USING (<shared cols>)`. No shared columns ⇒ a cross product.
    Natural,
}

/// One JOIN clause from the FROM list. Multi-join queries
/// (`A JOIN B ... JOIN C ...`) become a `Vec<JoinClause>` evaluated
/// left-to-right against the accumulator. The match condition is one
/// of `ON` / `USING` / `NATURAL` (see [`JoinConstraintKind`]);
/// `CROSS JOIN` arrives here already rewritten to `ON true`.
#[derive(Debug, Clone)]
pub struct JoinClause {
    pub join_type: JoinType,
    pub right_table: String,
    /// `AS alias` if the right table introduced one. Stored separately
    /// from `right_table` so the executor can normalize on
    /// `alias.unwrap_or(right_table)` for qualifier matching.
    pub right_alias: Option<String>,
    /// What the join matches on. See [`JoinConstraintKind`].
    pub constraint: JoinConstraintKind,
}

/// A parsed, simplified SELECT query.
#[derive(Debug, Clone)]
pub struct SelectQuery {
    pub table_name: String,
    /// Optional `AS alias` on the leading FROM table. The executor's
    /// scope resolver treats `alias.unwrap_or(table_name)` as the
    /// qualifier name.
    pub table_alias: Option<String>,
    /// SQLR-5 — JOIN clauses in source order. Empty = single-table
    /// SELECT, the existing fast path.
    pub joins: Vec<JoinClause>,
    pub projection: Projection,
    /// Raw sqlparser WHERE expression, evaluated by the executor at run time.
    pub selection: Option<Expr>,
    pub order_by: Option<OrderByClause>,
    pub limit: Option<usize>,
    /// `SELECT DISTINCT`.
    pub distinct: bool,
    /// `GROUP BY a, b` — bare column names. Empty = no GROUP BY.
    pub group_by: Vec<String>,
}

impl SelectQuery {
    pub fn new(statement: &Statement) -> Result<Self> {
        let Statement::Query(query) = statement else {
            return Err(SQLRiteError::Internal(
                "Error parsing SELECT: expected a Query statement".to_string(),
            ));
        };

        let Query {
            body,
            order_by,
            limit_clause,
            ..
        } = query.as_ref();

        let SetExpr::Select(select) = body.as_ref() else {
            return Err(SQLRiteError::NotImplemented(
                "Only simple SELECT queries are supported (no UNION / VALUES / CTEs yet)"
                    .to_string(),
            ));
        };
        let Select {
            projection,
            from,
            selection,
            distinct,
            group_by,
            having,
            ..
        } = select.as_ref();

        // SQLR-3: read DISTINCT instead of rejecting it. Postgres's
        // `DISTINCT ON (...)` stays unsupported — it's a per-group
        // tie-breaker that isn't part of the SQLite surface we mirror.
        let distinct_flag = match distinct {
            None => false,
            Some(sqlparser::ast::Distinct::Distinct) => true,
            Some(sqlparser::ast::Distinct::All) => false,
            Some(sqlparser::ast::Distinct::On(_)) => {
                return Err(SQLRiteError::NotImplemented(
                    "SELECT DISTINCT ON (...) is not supported".to_string(),
                ));
            }
        };
        if having.is_some() {
            return Err(SQLRiteError::NotImplemented(
                "HAVING is not supported yet".to_string(),
            ));
        }
        // SQLR-3: parse GROUP BY into a list of bare column names.
        // GroupByExpr::Expressions(v, _) with an empty v is the "no
        // GROUP BY" shape; non-empty means we've got grouping. Reject
        // GROUP BY ALL and GROUP BY on non-bare expressions for v1.
        let group_by_cols: Vec<String> = match group_by {
            sqlparser::ast::GroupByExpr::Expressions(exprs, _) => {
                let mut out = Vec::with_capacity(exprs.len());
                for e in exprs {
                    let col = match e {
                        Expr::Identifier(ident) => ident.value.clone(),
                        Expr::CompoundIdentifier(parts) => {
                            parts.last().map(|p| p.value.clone()).ok_or_else(|| {
                                SQLRiteError::Internal("empty compound identifier".to_string())
                            })?
                        }
                        other => {
                            return Err(SQLRiteError::NotImplemented(format!(
                                "GROUP BY only supports bare column references for now, got {other:?}"
                            )));
                        }
                    };
                    out.push(col);
                }
                out
            }
            _ => {
                return Err(SQLRiteError::NotImplemented(
                    "GROUP BY ALL is not supported".to_string(),
                ));
            }
        };

        let (table_name, table_alias, joins) = extract_from_clause(from)?;
        let projection = parse_projection(projection)?;
        let order_by = parse_order_by(order_by.as_ref())?;
        let limit = parse_limit(limit_clause.as_ref())?;

        // SQLR-3 validation: when GROUP BY is present, every bare-column
        // entry in the projection must appear in the GROUP BY list. Bare
        // columns in the SELECT are otherwise undefined per group.
        if !group_by_cols.is_empty()
            && let Projection::Items(items) = &projection
        {
            for item in items {
                if let ProjectionKind::Column { name: c, .. } = &item.kind
                    && !group_by_cols.contains(c)
                {
                    return Err(SQLRiteError::Internal(format!(
                        "column '{c}' must appear in GROUP BY or be used in an aggregate function"
                    )));
                }
            }
        }

        // SQLR-5 — aggregations across joined results aren't covered
        // by the current single-table grouping pipeline. Reject GROUP
        // BY / aggregates over a join up front so the user gets a clear
        // message rather than wrong results.
        if !joins.is_empty() {
            let has_agg = matches!(
                &projection,
                Projection::Items(items)
                    if items.iter().any(|i| matches!(i.kind, ProjectionKind::Aggregate(_)))
            );
            if has_agg || !group_by_cols.is_empty() {
                return Err(SQLRiteError::NotImplemented(
                    "GROUP BY / aggregate functions over JOIN results are not supported yet"
                        .to_string(),
                ));
            }
            if distinct_flag {
                return Err(SQLRiteError::NotImplemented(
                    "SELECT DISTINCT over JOIN results is not supported yet".to_string(),
                ));
            }
        }

        Ok(SelectQuery {
            table_name,
            table_alias,
            joins,
            projection,
            selection: selection.clone(),
            order_by,
            limit,
            distinct: distinct_flag,
            group_by: group_by_cols,
        })
    }
}

/// Pull the leading FROM table (with optional alias) and any JOIN
/// clauses out of the parsed FROM list. Supports a single base table
/// plus zero or more INNER / LEFT / RIGHT / FULL OUTER joins with an
/// `ON`, `USING (...)`, or `NATURAL` constraint, and `CROSS JOIN`
/// (rewritten to `INNER ... ON true`). Comma-separated FROM lists and
/// SEMI / ANTI / ASOF / APPLY joins surface as `NotImplemented`.
fn extract_from_clause(
    from: &[TableWithJoins],
) -> Result<(String, Option<String>, Vec<JoinClause>)> {
    if from.is_empty() {
        return Err(SQLRiteError::Internal(
            "SELECT requires a FROM clause".to_string(),
        ));
    }
    if from.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "comma-separated FROM lists are not supported — use explicit JOIN syntax".to_string(),
        ));
    }
    let twj = &from[0];
    let (table_name, table_alias) = extract_table_factor(&twj.relation)?;

    let mut joins = Vec::with_capacity(twj.joins.len());
    for j in &twj.joins {
        let (right_table, right_alias) = extract_table_factor(&j.relation)?;
        let (join_type, constraint) = match &j.join_operator {
            // Bare `JOIN` defaults to INNER per SQL standard.
            JoinOperator::Join(c) | JoinOperator::Inner(c) => {
                (JoinType::Inner, convert_constraint(c)?)
            }
            JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => {
                (JoinType::LeftOuter, convert_constraint(c)?)
            }
            JoinOperator::Right(c) | JoinOperator::RightOuter(c) => {
                (JoinType::RightOuter, convert_constraint(c)?)
            }
            JoinOperator::FullOuter(c) => (JoinType::FullOuter, convert_constraint(c)?),
            // `CROSS JOIN` is the cross product: INNER with an always-true
            // ON. A constraint on a CROSS JOIN is non-standard, but if the
            // parser handed us `USING` / `NATURAL` / `ON` we honor it
            // rather than silently dropping it.
            JoinOperator::CrossJoin(c) => (JoinType::Inner, convert_cross_constraint(c)?),
            other => {
                return Err(SQLRiteError::NotImplemented(format!(
                    "join flavor {other:?} is not supported \
                     (only INNER / LEFT OUTER / RIGHT OUTER / FULL OUTER / CROSS, \
                     with ON / USING / NATURAL)"
                )));
            }
        };
        joins.push(JoinClause {
            join_type,
            right_table,
            right_alias,
            constraint,
        });
    }

    Ok((table_name, table_alias, joins))
}

fn extract_table_factor(tf: &TableFactor) -> Result<(String, Option<String>)> {
    match tf {
        TableFactor::Table { name, alias, .. } => {
            let table_name = name.to_string();
            let alias_name = alias.as_ref().map(|a| a.name.value.clone());
            // We don't yet support alias column lists like `(c1, c2)` —
            // they only matter for table-valued functions / derived
            // tables, which we don't have either.
            if let Some(a) = alias.as_ref()
                && !a.columns.is_empty()
            {
                return Err(SQLRiteError::NotImplemented(
                    "table alias column lists are not supported".to_string(),
                ));
            }
            Ok((table_name, alias_name))
        }
        _ => Err(SQLRiteError::NotImplemented(
            "only plain table references are supported in FROM / JOIN".to_string(),
        )),
    }
}

/// Lower a `sqlparser` join constraint into our [`JoinConstraintKind`].
/// `ON` passes through; `USING` is narrowed to a list of bare column
/// names; `NATURAL` defers to the executor. A constraint-less join
/// (`A JOIN B` with no `ON` / `USING`) is rejected — `CROSS JOIN` is
/// the supported way to ask for a cross product and is handled by
/// [`convert_cross_constraint`].
fn convert_constraint(constraint: &JoinConstraint) -> Result<JoinConstraintKind> {
    match constraint {
        JoinConstraint::On(expr) => Ok(JoinConstraintKind::On(Box::new(expr.clone()))),
        JoinConstraint::Using(cols) => {
            let names = cols
                .iter()
                .map(extract_using_column)
                .collect::<Result<Vec<String>>>()?;
            Ok(JoinConstraintKind::Using(names))
        }
        JoinConstraint::Natural => Ok(JoinConstraintKind::Natural),
        JoinConstraint::None => Err(SQLRiteError::NotImplemented(
            "JOIN without an ON / USING / NATURAL condition is not supported \
             (use `... ON ...`, `... USING (...)`, `NATURAL JOIN`, or `CROSS JOIN`)"
                .to_string(),
        )),
    }
}

/// Constraint handling for `CROSS JOIN`. The standard form carries no
/// constraint and means "cross product", which we express as `ON true`
/// so it flows through the same executor path as any other join.
fn convert_cross_constraint(constraint: &JoinConstraint) -> Result<JoinConstraintKind> {
    match constraint {
        JoinConstraint::None => Ok(JoinConstraintKind::On(Box::new(true_literal()))),
        // Non-standard, but if a constraint was attached to a CROSS JOIN,
        // honor it instead of dropping it on the floor.
        other => convert_constraint(other),
    }
}

/// Pull a bare column name out of a `USING (...)` entry. `USING`
/// columns are always simple identifiers; anything qualified or
/// multi-part is rejected.
fn extract_using_column(name: &ObjectName) -> Result<String> {
    match name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => Ok(ident.value.clone()),
        _ => Err(SQLRiteError::NotImplemented(format!(
            "USING column must be a simple column name, got {name}"
        ))),
    }
}

/// An always-true boolean literal expression, used to rewrite
/// `CROSS JOIN` into `INNER JOIN ... ON true`.
fn true_literal() -> Expr {
    Expr::Value(Value::Boolean(true).with_empty_span())
}

fn parse_projection(items: &[SelectItem]) -> Result<Projection> {
    // Special-case `SELECT *`.
    if items.len() == 1
        && let SelectItem::Wildcard(_) = &items[0]
    {
        return Ok(Projection::All);
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(parse_select_item(item)?);
    }
    Ok(Projection::Items(out))
}

fn parse_select_item(item: &SelectItem) -> Result<ProjectionItem> {
    match item {
        SelectItem::UnnamedExpr(expr) => parse_projection_expr(expr, None),
        SelectItem::ExprWithAlias { expr, alias } => {
            parse_projection_expr(expr, Some(alias.value.clone()))
        }
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
            Err(SQLRiteError::NotImplemented(
                "Wildcard mixed with other columns is not supported".to_string(),
            ))
        }
    }
}

fn parse_projection_expr(expr: &Expr, alias: Option<String>) -> Result<ProjectionItem> {
    match expr {
        Expr::Identifier(ident) => Ok(ProjectionItem {
            kind: ProjectionKind::Column {
                qualifier: None,
                name: ident.value.clone(),
            },
            alias,
        }),
        Expr::CompoundIdentifier(parts) => match parts.as_slice() {
            [only] => Ok(ProjectionItem {
                kind: ProjectionKind::Column {
                    qualifier: None,
                    name: only.value.clone(),
                },
                alias,
            }),
            [q, c] => Ok(ProjectionItem {
                kind: ProjectionKind::Column {
                    qualifier: Some(q.value.clone()),
                    name: c.value.clone(),
                },
                alias,
            }),
            _ => Err(SQLRiteError::NotImplemented(format!(
                "compound identifier with {} parts is not supported in projection",
                parts.len()
            ))),
        },
        Expr::Function(func) => {
            let call = parse_aggregate_call(func)?;
            Ok(ProjectionItem {
                kind: ProjectionKind::Aggregate(call),
                alias,
            })
        }
        other => Err(SQLRiteError::NotImplemented(format!(
            "Only bare column references and aggregate functions are supported in the projection list (got {other:?})"
        ))),
    }
}

fn parse_aggregate_call(func: &sqlparser::ast::Function) -> Result<AggregateCall> {
    // Function name: only unqualified names like COUNT(...). Qualified
    // names like `pkg.fn(...)` are out of scope.
    let name = match func.name.0.as_slice() {
        [sqlparser::ast::ObjectNamePart::Identifier(ident)] => ident.value.clone(),
        _ => {
            return Err(SQLRiteError::NotImplemented(format!(
                "qualified function names not supported: {:?}",
                func.name
            )));
        }
    };
    let agg_fn = AggregateFn::from_name(&name).ok_or_else(|| {
        SQLRiteError::NotImplemented(format!(
            "function '{name}' is not supported in the projection list (only aggregate functions are: COUNT, SUM, AVG, MIN, MAX)"
        ))
    })?;

    // Aggregates only accept the basic List form. None / Subquery forms
    // (CURRENT_TIMESTAMP, scalar subqueries) don't apply here.
    let arg_list = match &func.args {
        FunctionArguments::List(l) => l,
        _ => {
            return Err(SQLRiteError::NotImplemented(format!(
                "{name}(...) — unsupported argument shape"
            )));
        }
    };

    let distinct = matches!(
        arg_list.duplicate_treatment,
        Some(DuplicateTreatment::Distinct)
    );

    if !arg_list.clauses.is_empty() {
        return Err(SQLRiteError::NotImplemented(format!(
            "{name}(...) — extra argument clauses (ORDER BY / LIMIT inside the call) are not supported"
        )));
    }
    if func.over.is_some() {
        return Err(SQLRiteError::NotImplemented(
            "window functions (OVER (...)) are not supported".to_string(),
        ));
    }
    if func.filter.is_some() {
        return Err(SQLRiteError::NotImplemented(
            "FILTER (WHERE ...) on aggregates is not supported".to_string(),
        ));
    }
    if !func.within_group.is_empty() {
        return Err(SQLRiteError::NotImplemented(
            "WITHIN GROUP on aggregates is not supported".to_string(),
        ));
    }

    if arg_list.args.len() != 1 {
        return Err(SQLRiteError::NotImplemented(format!(
            "{name}(...) expects exactly one argument, got {}",
            arg_list.args.len()
        )));
    }

    let arg = match &arg_list.args[0] {
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => AggregateArg::Star,
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(ident))) => {
            AggregateArg::Column(ident.value.clone())
        }
        FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::CompoundIdentifier(parts))) => {
            let c = parts
                .last()
                .map(|p| p.value.clone())
                .ok_or_else(|| SQLRiteError::Internal("empty compound identifier".to_string()))?;
            AggregateArg::Column(c)
        }
        other => {
            return Err(SQLRiteError::NotImplemented(format!(
                "{name}(...) — argument must be `*` or a bare column reference (got {other:?})"
            )));
        }
    };

    // v1: only COUNT(DISTINCT col) is supported. SUM/AVG/MIN/MAX with
    // DISTINCT are valid SQL but uncommon and add accumulator complexity
    // we don't yet need.
    if distinct && agg_fn != AggregateFn::Count {
        return Err(SQLRiteError::NotImplemented(format!(
            "DISTINCT is only supported on COUNT(...) for now, not {}",
            agg_fn.as_str()
        )));
    }
    if matches!(arg, AggregateArg::Star) && agg_fn != AggregateFn::Count {
        return Err(SQLRiteError::NotImplemented(format!(
            "{}(*) is not supported; use {}(<column>)",
            agg_fn.as_str(),
            agg_fn.as_str()
        )));
    }

    Ok(AggregateCall {
        func: agg_fn,
        arg,
        distinct,
    })
}

fn parse_order_by(order_by: Option<&sqlparser::ast::OrderBy>) -> Result<Option<OrderByClause>> {
    let Some(ob) = order_by else {
        return Ok(None);
    };
    let exprs = match &ob.kind {
        OrderByKind::Expressions(v) => v,
        OrderByKind::All(_) => {
            return Err(SQLRiteError::NotImplemented(
                "ORDER BY ALL is not supported".to_string(),
            ));
        }
    };
    if exprs.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "ORDER BY must have exactly one column for now".to_string(),
        ));
    }
    let obe = &exprs[0];
    // Phase 7b: accept arbitrary expressions, not just bare column refs.
    // The executor's `sort_rowids` evaluates this expression per row via
    // `eval_expr`, which handles Identifier (column lookup), Function
    // (vec_distance_*), arithmetic, etc. uniformly. The previous
    // column-name-only restriction has been lifted.
    let expr = obe.expr.clone();
    // `asc == None` is the dialect default (ASC).
    let ascending = obe.options.asc.unwrap_or(true);
    Ok(Some(OrderByClause { expr, ascending }))
}

fn parse_limit(limit: Option<&LimitClause>) -> Result<Option<usize>> {
    let Some(lc) = limit else {
        return Ok(None);
    };
    let limit_expr = match lc {
        LimitClause::LimitOffset { limit, offset, .. } => {
            if offset.is_some() {
                return Err(SQLRiteError::NotImplemented(
                    "OFFSET is not supported yet".to_string(),
                ));
            }
            limit.as_ref()
        }
        LimitClause::OffsetCommaLimit { .. } => {
            return Err(SQLRiteError::NotImplemented(
                "`LIMIT <offset>, <limit>` syntax is not supported yet".to_string(),
            ));
        }
    };
    let Some(expr) = limit_expr else {
        return Ok(None);
    };
    let n = eval_const_usize(expr)?;
    Ok(Some(n))
}

fn eval_const_usize(expr: &Expr) -> Result<usize> {
    match expr {
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::Number(n, _) => n.parse::<usize>().map_err(|e| {
                SQLRiteError::Internal(format!("LIMIT must be a non-negative integer: {e}"))
            }),
            _ => Err(SQLRiteError::Internal(
                "LIMIT must be an integer literal".to_string(),
            )),
        },
        _ => Err(SQLRiteError::NotImplemented(
            "LIMIT expression must be a literal number".to_string(),
        )),
    }
}
