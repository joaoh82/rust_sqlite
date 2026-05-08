//! Prepared-statement parameter binding (SQLR-23).
//!
//! Two responsibilities:
//!
//! 1. **Placeholder rewriting at prepare time.** The user writes `?` in
//!    the SQL; sqlparser parses each as `Expr::Value(Placeholder("?"))`.
//!    We walk the parsed AST left-to-right and rewrite each bare `?` to
//!    `?N` (1-indexed source order) so the later substitution pass knows
//!    which slot to bind. The rewritten AST is what `Statement` caches.
//!
//! 2. **Substitution at execute time.** Given the cached AST and a
//!    `&[Value]` slice, walk a clone of the AST and replace every
//!    `Expr::Value(Placeholder("?N"))` with the matching `params[N-1]`.
//!
//! Substitution lowers the bound value into a node shape the rest of the
//! pipeline already understands:
//!
//! - Scalars (`Integer`, `Real`, `Text`, `Bool`, `Null`) become
//!   `Expr::Value(...)` literals — same shape an inline literal would
//!   parse to. Existing executor / parser arms handle them unchanged.
//! - Vectors become `Expr::Identifier { quote_style: Some('['), value: "<csv>" }`,
//!   which is the in-band form sqlparser produces for inline bracket-array
//!   literals like `[0.1, 0.2, ...]`. The INSERT parser, the executor's
//!   `eval_expr_scope`, and the HNSW probe optimizer all already recognize
//!   that shape, so a bound `Value::Vector(...)` flows through every path
//!   that an inline `[...]` literal does — including the HNSW shortcut.
//!
//! Doing it as an AST-rewrite (rather than threading `&[Value]` through
//! the executor) keeps the diff focused: every existing executor arm
//! sees concrete literals, exactly as it does today on inline-params SQL.

use std::ops::ControlFlow;

use sqlparser::ast::{
    Expr, Ident, Statement, Value as AstValue, ValueWithSpan, visit_expressions_mut,
};
use sqlparser::tokenizer::Span;

use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::Value;

/// Walks every expression in `stmt` and rewrites bare `?` placeholders to
/// `?N` (1-indexed source order). Returns the total parameter count.
///
/// Idempotent for already-numbered placeholders: `?1`, `?2`, … pass
/// through unchanged. We deliberately don't try to *renumber* already-
/// numbered placeholders — that's a foot-gun (the user might use the
/// same index twice on purpose to bind once and reference twice), and
/// `Statement::new` runs this exactly once on a freshly-parsed AST.
pub fn rewrite_placeholders(stmt: &mut Statement) -> usize {
    let mut counter: usize = 0;
    let _ = visit_expressions_mut(stmt, |expr| {
        if let Expr::Value(v) = expr
            && let AstValue::Placeholder(s) = &mut v.value
            && s == "?"
        {
            counter += 1;
            *s = format!("?{counter}");
        }
        ControlFlow::<()>::Continue(())
    });
    counter
}

/// Substitutes every `?N` placeholder in `stmt` with the matching value
/// from `params`. Mutates the AST in place — callers should clone first
/// if they want the original back.
///
/// Errors if the AST references a placeholder index outside `params`,
/// or if a non-canonical placeholder form (`:name`, `$1`) is encountered.
pub fn substitute_params(stmt: &mut Statement, params: &[Value]) -> Result<()> {
    let mut bind_err: Option<SQLRiteError> = None;
    let _ = visit_expressions_mut(stmt, |expr| {
        let Expr::Value(v) = expr else {
            return ControlFlow::Continue(());
        };
        let placeholder_str = match &v.value {
            AstValue::Placeholder(s) => s.clone(),
            _ => return ControlFlow::Continue(()),
        };
        let idx = match placeholder_index(&placeholder_str) {
            Some(i) => i,
            None => {
                bind_err = Some(SQLRiteError::NotImplemented(format!(
                    "unsupported placeholder form `{placeholder_str}`; only `?` and `?N` are supported"
                )));
                return ControlFlow::Break(());
            }
        };
        let Some(value) = params.get(idx) else {
            bind_err = Some(SQLRiteError::General(format!(
                "missing bind value for `?{}` (got {} parameter{})",
                idx + 1,
                params.len(),
                if params.len() == 1 { "" } else { "s" }
            )));
            return ControlFlow::Break(());
        };
        *expr = value_to_expr(value);
        ControlFlow::<()>::Continue(())
    });
    if let Some(e) = bind_err {
        return Err(e);
    }
    Ok(())
}

/// Decode a `Placeholder("?N")` string into its 0-indexed slot. Returns
/// `None` for any non-canonical form (`:name`, `$1`, bare `?` after
/// rewriting — that last case shouldn't happen but is rejected
/// defensively).
fn placeholder_index(s: &str) -> Option<usize> {
    let n = s.strip_prefix('?')?.parse::<usize>().ok()?;
    if n == 0 {
        return None;
    }
    Some(n - 1)
}

/// Build the AST `Expr` equivalent of a runtime `Value`. The shapes
/// match what `sqlparser` produces for inline literals so downstream
/// executor code paths don't need to change.
fn value_to_expr(v: &Value) -> Expr {
    match v {
        Value::Null => Expr::Value(ValueWithSpan {
            value: AstValue::Null,
            span: Span::empty(),
        }),
        Value::Integer(i) => Expr::Value(ValueWithSpan {
            value: AstValue::Number(i.to_string(), false),
            span: Span::empty(),
        }),
        Value::Real(f) => Expr::Value(ValueWithSpan {
            // f64::Display picks the shortest round-tripping form;
            // re-parsing it back via str::parse::<f64> is exact.
            value: AstValue::Number(f.to_string(), false),
            span: Span::empty(),
        }),
        Value::Text(s) => Expr::Value(ValueWithSpan {
            value: AstValue::SingleQuotedString(s.clone()),
            span: Span::empty(),
        }),
        Value::Bool(b) => Expr::Value(ValueWithSpan {
            value: AstValue::Boolean(*b),
            span: Span::empty(),
        }),
        Value::Vector(v) => {
            // Inline bracket-array form. `i.value` carries the inner
            // CSV without brackets — `format!("[{}]", i.value)` at the
            // consumer side reconstructs the literal that
            // `parse_vector_literal` accepts.
            let inner = format_vector_inner(v);
            Expr::Identifier(Ident {
                value: inner,
                quote_style: Some('['),
                span: Span::empty(),
            })
        }
    }
}

fn format_vector_inner(v: &[f32]) -> String {
    // Preallocate generously: each f32 averages ~8 chars + ", ".
    let mut s = String::with_capacity(v.len() * 10);
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&x.to_string());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::dialect::SqlriteDialect;
    use sqlparser::parser::Parser;

    fn parse_one(sql: &str) -> Statement {
        let mut ast = Parser::parse_sql(&SqlriteDialect::new(), sql).unwrap();
        ast.pop().unwrap()
    }

    #[test]
    fn rewrite_assigns_indices_in_source_order() {
        let mut stmt = parse_one("SELECT * FROM t WHERE a = ? AND b = ? AND c = ?");
        let n = rewrite_placeholders(&mut stmt);
        assert_eq!(n, 3);
        let sql = stmt.to_string();
        assert!(sql.contains("?1"));
        assert!(sql.contains("?2"));
        assert!(sql.contains("?3"));
    }

    #[test]
    fn rewrite_zero_for_no_placeholders() {
        let mut stmt = parse_one("SELECT * FROM t WHERE a = 1");
        assert_eq!(rewrite_placeholders(&mut stmt), 0);
    }

    #[test]
    fn rewrite_idempotent_on_numbered_placeholders() {
        // `?1` parses with placeholder string `?1`. Walking again must
        // not double-number.
        let mut stmt = parse_one("SELECT * FROM t WHERE a = ?1 AND b = ?2");
        let n = rewrite_placeholders(&mut stmt);
        // Bare `?` count is zero — the existing `?1`/`?2` are left
        // alone. The total parameter count is therefore reported as 0
        // here; callers using `?N` form should already know their
        // arity from the source SQL.
        assert_eq!(n, 0);
    }

    #[test]
    fn substitute_replaces_scalar_params() {
        let mut stmt = parse_one("SELECT * FROM t WHERE a = ? AND b = ? AND c = ?");
        rewrite_placeholders(&mut stmt);
        substitute_params(
            &mut stmt,
            &[
                Value::Integer(1),
                Value::Text("x".into()),
                Value::Bool(true),
            ],
        )
        .unwrap();
        let sql = stmt.to_string();
        assert!(sql.contains("a = 1"), "got: {sql}");
        assert!(sql.contains("b = 'x'"), "got: {sql}");
        // sqlparser renders Boolean::true as `true`.
        assert!(sql.contains("c = true"), "got: {sql}");
    }

    #[test]
    fn substitute_replaces_vector_param_as_bracket_array() {
        let mut stmt = parse_one("SELECT id FROM t ORDER BY vec_distance_l2(v, ?) LIMIT 5");
        rewrite_placeholders(&mut stmt);
        substitute_params(&mut stmt, &[Value::Vector(vec![0.1, 0.2, 0.3])]).unwrap();
        let sql = stmt.to_string();
        // sqlparser renders bracket-quoted Identifier as `[<inner>]`.
        assert!(sql.contains("[0.1, 0.2, 0.3]"), "got: {sql}");
    }

    #[test]
    fn substitute_errors_on_too_few_params() {
        let mut stmt = parse_one("SELECT * FROM t WHERE a = ? AND b = ?");
        rewrite_placeholders(&mut stmt);
        let err = substitute_params(&mut stmt, &[Value::Integer(1)]).unwrap_err();
        assert!(format!("{err}").contains("missing bind value"));
    }

    #[test]
    fn substitute_replaces_null_param() {
        let mut stmt = parse_one("SELECT * FROM t WHERE a = ?");
        rewrite_placeholders(&mut stmt);
        substitute_params(&mut stmt, &[Value::Null]).unwrap();
        let sql = stmt.to_string();
        assert!(sql.to_uppercase().contains("NULL"), "got: {sql}");
    }

    #[test]
    fn placeholder_index_decodes_canonical_form() {
        assert_eq!(placeholder_index("?1"), Some(0));
        assert_eq!(placeholder_index("?42"), Some(41));
        assert_eq!(placeholder_index("?"), None);
        assert_eq!(placeholder_index("?0"), None);
        assert_eq!(placeholder_index(":name"), None);
        assert_eq!(placeholder_index("$1"), None);
    }
}
