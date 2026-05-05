use sqlparser::ast::{
    DuplicateTreatment, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, LimitClause,
    OrderByKind, Query, Select, SelectItem, SetExpr, Statement, TableFactor, TableWithJoins,
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
    pub fn output_name(&self) -> String {
        if let Some(a) = &self.alias {
            return a.clone();
        }
        match &self.kind {
            ProjectionKind::Column(c) => c.clone(),
            ProjectionKind::Aggregate(a) => a.display_name(),
        }
    }
}

/// What an individual projection item produces.
#[derive(Debug, Clone)]
pub enum ProjectionKind {
    /// Bare column reference: `SELECT a, b, c`.
    Column(String),
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

/// A parsed, simplified SELECT query.
#[derive(Debug, Clone)]
pub struct SelectQuery {
    pub table_name: String,
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

        let table_name = extract_single_table_name(from)?;
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
                if let ProjectionKind::Column(c) = &item.kind
                    && !group_by_cols.contains(c)
                {
                    return Err(SQLRiteError::Internal(format!(
                        "column '{c}' must appear in GROUP BY or be used in an aggregate function"
                    )));
                }
            }
        }

        Ok(SelectQuery {
            table_name,
            projection,
            selection: selection.clone(),
            order_by,
            limit,
            distinct: distinct_flag,
            group_by: group_by_cols,
        })
    }
}

fn extract_single_table_name(from: &[TableWithJoins]) -> Result<String> {
    if from.len() != 1 {
        return Err(SQLRiteError::NotImplemented(
            "SELECT from multiple tables (joins / comma-joins) is not supported yet".to_string(),
        ));
    }
    let twj = &from[0];
    if !twj.joins.is_empty() {
        return Err(SQLRiteError::NotImplemented(
            "JOIN is not supported yet".to_string(),
        ));
    }
    match &twj.relation {
        TableFactor::Table { name, .. } => Ok(name.to_string()),
        _ => Err(SQLRiteError::NotImplemented(
            "Only SELECT from a plain table is supported".to_string(),
        )),
    }
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
            kind: ProjectionKind::Column(ident.value.clone()),
            alias,
        }),
        Expr::CompoundIdentifier(parts) => {
            let name = parts.last().map(|p| p.value.clone()).ok_or_else(|| {
                SQLRiteError::Internal("empty qualified column reference".to_string())
            })?;
            Ok(ProjectionItem {
                kind: ProjectionKind::Column(name),
                alias,
            })
        }
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
