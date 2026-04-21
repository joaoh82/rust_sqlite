use sqlparser::ast::{
    Expr, LimitClause, OrderByKind, Query, Select, SelectItem, SetExpr, Statement, TableFactor,
    TableWithJoins,
};

use crate::error::{Result, SQLRiteError};

/// What columns to project from a SELECT.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    /// `SELECT *` — every column in the table, in declaration order.
    All,
    /// `SELECT a, b, c` — explicit list.
    Columns(Vec<String>),
}

/// An ORDER BY clause restricted to a single column, ascending by default.
#[derive(Debug, Clone)]
pub struct OrderByClause {
    pub column: String,
    pub ascending: bool,
}

/// A parsed, simplified SELECT query.
#[derive(Debug)]
pub struct SelectQuery {
    pub table_name: String,
    pub projection: Projection,
    /// Raw sqlparser WHERE expression, evaluated by the executor at run time.
    pub selection: Option<Expr>,
    pub order_by: Option<OrderByClause>,
    pub limit: Option<usize>,
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

        if distinct.is_some() {
            return Err(SQLRiteError::NotImplemented(
                "SELECT DISTINCT is not supported yet".to_string(),
            ));
        }
        if having.is_some() {
            return Err(SQLRiteError::NotImplemented(
                "HAVING is not supported yet".to_string(),
            ));
        }
        // GroupByExpr::Expressions(v, _) with an empty v is the "no GROUP BY" shape.
        if let sqlparser::ast::GroupByExpr::Expressions(exprs, _) = group_by {
            if !exprs.is_empty() {
                return Err(SQLRiteError::NotImplemented(
                    "GROUP BY is not supported yet".to_string(),
                ));
            }
        } else {
            return Err(SQLRiteError::NotImplemented(
                "GROUP BY ALL is not supported".to_string(),
            ));
        }

        let table_name = extract_single_table_name(from)?;
        let projection = parse_projection(projection)?;
        let order_by = parse_order_by(order_by.as_ref())?;
        let limit = parse_limit(limit_clause.as_ref())?;

        Ok(SelectQuery {
            table_name,
            projection,
            selection: selection.clone(),
            order_by,
            limit,
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
    if items.len() == 1 {
        if let SelectItem::Wildcard(_) = &items[0] {
            return Ok(Projection::All);
        }
    }
    let mut cols = Vec::with_capacity(items.len());
    for item in items {
        match item {
            SelectItem::UnnamedExpr(Expr::Identifier(ident)) => cols.push(ident.value.clone()),
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                if let Some(last) = parts.last() {
                    cols.push(last.value.clone());
                } else {
                    return Err(SQLRiteError::Internal(
                        "empty qualified column reference".to_string(),
                    ));
                }
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                return Err(SQLRiteError::NotImplemented(
                    "Wildcard mixed with other columns is not supported".to_string(),
                ));
            }
            SelectItem::ExprWithAlias { .. } | SelectItem::UnnamedExpr(_) => {
                return Err(SQLRiteError::NotImplemented(
                    "Only bare column references are supported in the projection list"
                        .to_string(),
                ));
            }
        }
    }
    Ok(Projection::Columns(cols))
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
    let column = match &obe.expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => parts
            .last()
            .map(|i| i.value.clone())
            .unwrap_or_default(),
        _ => {
            return Err(SQLRiteError::NotImplemented(
                "ORDER BY only supports a bare column name for now".to_string(),
            ));
        }
    };
    // `asc == None` is the dialect default (ASC).
    let ascending = obe.options.asc.unwrap_or(true);
    Ok(Some(OrderByClause { column, ascending }))
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
