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

#[derive(Clone, Copy)]
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
        // Preserve the typed `SQLRiteError` as the anyhow source so
        // [`is_retryable_busy`] can downcast — the W13 retry loop
        // needs to distinguish `Busy` / `BusySnapshot` from other
        // failures. Adding context via `.with_context` keeps the
        // human-readable wrapper while threading the typed source
        // underneath.
        conn.execute(sql)
            .map_err(anyhow::Error::new)
            .with_context(|| format!("sqlrite execute: {sql}"))?;
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
            .map_err(anyhow::Error::new)
            .with_context(|| format!("sqlrite prepare_cached: {sql}"))?;
        stmt.execute_with_params(&bound)
            .map_err(anyhow::Error::new)
            .with_context(|| format!("sqlrite execute_with_params: {sql}"))?;
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

    /// Mint a sibling Connection that shares the primary's
    /// `Arc<Mutex<Database>>`. A fresh `Connection::open(path)`
    /// would fail here because the primary already holds an
    /// exclusive `flock(LOCK_EX)` on the WAL sidecar.
    fn connect_sibling(&self, primary: &Self::Conn, _path: &Path) -> Result<Self::Conn> {
        Ok(primary.connect())
    }

    fn enable_concurrent_mode(&self, conn: &mut Self::Conn) -> Result<()> {
        // `BEGIN CONCURRENT` requires `journal_mode = mvcc;`
        // otherwise the engine surfaces a typed error. The PRAGMA
        // is per-database (not per-connection), so toggling once
        // on the primary suffices for every sibling.
        conn.execute("PRAGMA journal_mode = mvcc")
            .map_err(anyhow::Error::new)
            .context("PRAGMA journal_mode = mvcc")?;
        Ok(())
    }

    fn concurrent_begin_sql(&self) -> &'static str {
        "BEGIN CONCURRENT"
    }

    /// SQLRite signals both `Busy` (write-write conflict at commit)
    /// and `BusySnapshot` (snapshot GC'd under a long-lived reader)
    /// via `SQLRiteError::is_retryable()`. The bench harness wraps
    /// engine errors in `anyhow::Error`, so we peel back to the
    /// typed source and consult the predicate.
    fn is_retryable_busy(&self, err: &anyhow::Error) -> bool {
        err.downcast_ref::<sqlrite::SQLRiteError>()
            .map(|e| e.is_retryable())
            .unwrap_or(false)
            || err
                .chain()
                .filter_map(|e| e.downcast_ref::<sqlrite::SQLRiteError>())
                .any(|e| e.is_retryable())
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
