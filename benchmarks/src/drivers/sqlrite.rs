//! SQLRite driver.
//!
//! Binds against the engine's public [`sqlrite::Connection`] surface —
//! the same API the language SDKs use. SQLRite has no parameter
//! binding yet (see `connection.rs:145` — "parameter binding and
//! prepared-plan caching are future work"), so the driver formats
//! `[Value]` into the SQL string at call time. That's an honest cost
//! to include in the comparison: a SQLRite user calling a hot SELECT
//! today pays the same per-call parse + format overhead.
//!
//! Once SQLRite gains parameter binding (post-9.6 follow-up), this
//! driver will switch to the bound path and a workload `v` bump will
//! capture the methodology change.

use std::path::Path;

use anyhow::{Context, Result};

use crate::{Driver, Value};

pub struct SQLRiteDriver;

impl Driver for SQLRiteDriver {
    type Conn = sqlrite::Connection;

    fn name(&self) -> &'static str {
        "sqlrite"
    }

    fn open(&self, path: &Path) -> Result<Self::Conn> {
        sqlrite::Connection::open(path)
            .map_err(|e| anyhow::anyhow!("sqlrite open({}): {e}", path.display()))
    }

    fn execute(&self, conn: &mut Self::Conn, sql: &str) -> Result<()> {
        conn.execute(sql)
            .map_err(|e| anyhow::anyhow!("sqlrite execute: {e}\n  sql: {sql}"))?;
        Ok(())
    }

    fn execute_with_params(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> Result<()> {
        let inlined = inline_params(sql, params)?;
        conn.execute(&inlined)
            .map_err(|e| anyhow::anyhow!("sqlrite execute_with_params: {e}\n  sql: {inlined}"))?;
        Ok(())
    }

    fn query_one(&self, conn: &mut Self::Conn, sql: &str, params: &[Value]) -> Result<Vec<Value>> {
        let inlined = inline_params(sql, params)?;
        let stmt = conn
            .prepare(&inlined)
            .map_err(|e| anyhow::anyhow!("sqlrite prepare: {e}\n  sql: {inlined}"))?;
        let mut rows = stmt
            .query()
            .map_err(|e| anyhow::anyhow!("sqlrite query: {e}\n  sql: {inlined}"))?;
        let row = rows
            .next()
            .map_err(|e| anyhow::anyhow!("sqlrite row read: {e}"))?
            .context("sqlrite query_one: zero rows returned")?;
        let cols = row.columns().len();
        let mut out = Vec::with_capacity(cols);
        for i in 0..cols {
            let v: sqlrite::Value = row.get(i)?;
            out.push(from_engine_value(v));
        }
        if rows
            .next()
            .map_err(|e| anyhow::anyhow!("sqlrite row read: {e}"))?
            .is_some()
        {
            anyhow::bail!("sqlrite query_one: >1 rows returned");
        }
        Ok(out)
    }

    fn query_all(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>> {
        let inlined = inline_params(sql, params)?;
        let stmt = conn
            .prepare(&inlined)
            .map_err(|e| anyhow::anyhow!("sqlrite prepare: {e}\n  sql: {inlined}"))?;
        let mut rows = stmt
            .query()
            .map_err(|e| anyhow::anyhow!("sqlrite query: {e}\n  sql: {inlined}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| anyhow::anyhow!("sqlrite row read: {e}"))?
        {
            let cols = row.columns().len();
            let mut buf = Vec::with_capacity(cols);
            for i in 0..cols {
                let v: sqlrite::Value = row.get(i)?;
                buf.push(from_engine_value(v));
            }
            out.push(buf);
        }
        Ok(out)
    }
}

/// Inline `?`-positional placeholders with literal values. Replaces the
/// first `?` with `params[0]`, the second with `params[1]`, etc. Errors
/// if the count doesn't match. Strings are SQL-escaped.
fn inline_params(sql: &str, params: &[Value]) -> Result<String> {
    let mut out = String::with_capacity(sql.len() + params.len() * 16);
    let mut iter = params.iter();
    let mut in_string = false;
    for ch in sql.chars() {
        if ch == '\'' {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if ch == '?' && !in_string {
            let p = iter
                .next()
                .context("inline_params: more `?` placeholders than params")?;
            push_literal(&mut out, p);
        } else {
            out.push(ch);
        }
    }
    if iter.next().is_some() {
        anyhow::bail!("inline_params: more params than `?` placeholders");
    }
    Ok(out)
}

fn push_literal(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("NULL"),
        Value::Integer(i) => out.push_str(&i.to_string()),
        Value::Real(f) => out.push_str(&format!("{f}")),
        Value::Text(s) => {
            out.push('\'');
            for ch in s.chars() {
                if ch == '\'' {
                    out.push('\'');
                }
                out.push(ch);
            }
            out.push('\'');
        }
    }
}

fn from_engine_value(v: sqlrite::Value) -> Value {
    match v {
        sqlrite::Value::Null => Value::Null,
        sqlrite::Value::Integer(i) => Value::Integer(i),
        sqlrite::Value::Real(f) => Value::Real(f),
        sqlrite::Value::Text(s) => Value::Text(s),
        // Bench inputs don't include booleans / vectors / JSON yet —
        // when a workload starts using them, this match grows.
        other => Value::Text(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_params_replaces_in_order() {
        let s = inline_params(
            "SELECT * FROM t WHERE a = ? AND b = ? AND c = ?",
            &[Value::Integer(1), Value::Text("x".into()), Value::Null],
        )
        .unwrap();
        assert_eq!(s, "SELECT * FROM t WHERE a = 1 AND b = 'x' AND c = NULL");
    }

    #[test]
    fn inline_params_preserves_question_marks_in_strings() {
        let s =
            inline_params("SELECT 'what?', * FROM t WHERE a = ?", &[Value::Integer(7)]).unwrap();
        assert_eq!(s, "SELECT 'what?', * FROM t WHERE a = 7");
    }

    #[test]
    fn inline_params_escapes_quotes() {
        let s = inline_params(
            "SELECT * FROM t WHERE name = ?",
            &[Value::Text("O'Hara".into())],
        )
        .unwrap();
        assert_eq!(s, "SELECT * FROM t WHERE name = 'O''Hara'");
    }

    #[test]
    fn inline_params_arity_mismatch_errors() {
        assert!(inline_params("SELECT ?", &[]).is_err());
        assert!(inline_params("SELECT 1", &[Value::Integer(1)]).is_err());
    }
}
