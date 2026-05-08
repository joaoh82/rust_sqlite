//! SQLRite driver.
//!
//! Binds against the engine's public [`sqlrite::Connection`] surface —
//! the same API the language SDKs use.
//!
//! ## SQLR-23 — bound + cached path
//!
//! SQLRite gained a prepared-statement plan cache + parameter binding
//! in SQLR-23. This driver uses both:
//!
//! - `query_one` / `query_all` route through [`sqlrite::Connection::prepare_cached`]
//!   so a hot SELECT pays the sqlparser walk exactly once across the
//!   whole bench loop (cache cap defaults to 16, plenty for any single
//!   workload).
//! - `execute_with_params` does the same for INSERT-loop hot paths.
//! - `Value::Vector` binds directly through `Statement::query_with_params`
//!   without round-tripping through a 4 KB bracket-array SQL literal —
//!   this is the W10/W12 unlock. The HNSW probe optimizer recognizes
//!   the bound vector via the same in-band shape an inline `[…]` would
//!   produce, so the optimizer hook still kicks in on bound queries.
//!
//! That's how a perf-conscious SQLRite user would write hot-path code
//! today.

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
        let bound = to_engine_values(params);
        let mut stmt = conn
            .prepare_cached(sql)
            .map_err(|e| anyhow::anyhow!("sqlrite prepare_cached: {e}\n  sql: {sql}"))?;
        stmt.execute_with_params(&bound)
            .map_err(|e| anyhow::anyhow!("sqlrite execute_with_params: {e}\n  sql: {sql}"))?;
        Ok(())
    }

    fn query_one(&self, conn: &mut Self::Conn, sql: &str, params: &[Value]) -> Result<Vec<Value>> {
        let bound = to_engine_values(params);
        let stmt = conn
            .prepare_cached(sql)
            .map_err(|e| anyhow::anyhow!("sqlrite prepare_cached: {e}\n  sql: {sql}"))?;
        let mut rows = stmt
            .query_with_params(&bound)
            .map_err(|e| anyhow::anyhow!("sqlrite query_with_params: {e}\n  sql: {sql}"))?;
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
        let bound = to_engine_values(params);
        let stmt = conn
            .prepare_cached(sql)
            .map_err(|e| anyhow::anyhow!("sqlrite prepare_cached: {e}\n  sql: {sql}"))?;
        let mut rows = stmt
            .query_with_params(&bound)
            .map_err(|e| anyhow::anyhow!("sqlrite query_with_params: {e}\n  sql: {sql}"))?;
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

/// Map the bench harness's `Value` to SQLRite's engine `Value`. Both
/// enums carry the same logical shapes; this is just a name-mapping.
fn to_engine_values(params: &[Value]) -> Vec<sqlrite::Value> {
    params.iter().map(to_engine_value).collect()
}

fn to_engine_value(v: &Value) -> sqlrite::Value {
    match v {
        Value::Null => sqlrite::Value::Null,
        Value::Integer(i) => sqlrite::Value::Integer(*i),
        Value::Real(f) => sqlrite::Value::Real(*f),
        Value::Text(s) => sqlrite::Value::Text(s.clone()),
        Value::Vector(v) => sqlrite::Value::Vector(v.clone()),
    }
}

fn from_engine_value(v: sqlrite::Value) -> Value {
    match v {
        sqlrite::Value::Null => Value::Null,
        sqlrite::Value::Integer(i) => Value::Integer(i),
        sqlrite::Value::Real(f) => Value::Real(f),
        sqlrite::Value::Text(s) => Value::Text(s),
        sqlrite::Value::Vector(v) => Value::Vector(v),
        // Bool / JSON aren't yet a bench `Value` variant — workloads
        // don't surface them. If a future workload reads one back,
        // grow this match alongside the harness `Value` enum.
        other => Value::Text(format!("{other:?}")),
    }
}
