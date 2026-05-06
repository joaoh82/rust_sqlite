//! SQLite driver via [`rusqlite`] (bundled libsqlite3).
//!
//! Per Q3, the driver applies the **headline tuned profile** at open
//! time:
//!
//! ```sql
//! PRAGMA journal_mode = WAL;
//! PRAGMA synchronous  = NORMAL;
//! PRAGMA temp_store   = MEMORY;
//! PRAGMA cache_size   = -65536;  -- 64 MB
//! ```
//!
//! Rationale: SQLRite's WAL is mandatory + always-on, so SQLite-default
//! (`journal_mode=DELETE`, `synchronous=FULL`) is *not* apples-to-apples.
//! `synchronous=NORMAL` matches SQLRite's commit fsync semantics. A
//! "SQLite-default" comparator column is a future opt-in (post-9.6).
//!
//! Driver-bias mitigation (Q3 risk in the plan): every hot SELECT path
//! goes through `prepare_cached` so we measure the engine's execution
//! cost, not per-iter parse cost. That's how a perf-conscious rusqlite
//! user would write this.

use std::path::Path;

use anyhow::{Context, Result};

use crate::{Driver, Value};

pub struct SQLiteDriver;

impl Driver for SQLiteDriver {
    type Conn = rusqlite::Connection;

    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn open(&self, path: &Path) -> Result<Self::Conn> {
        let conn = rusqlite::Connection::open(path)
            .with_context(|| format!("rusqlite open({})", path.display()))?;
        // Tuned profile (Q3). Each PRAGMA returns one row of feedback
        // we discard; query_row is the right shape (`pragma_update`
        // doesn't accept the negative-value cache_size form).
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("PRAGMA journal_mode=WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("PRAGMA synchronous=NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")
            .context("PRAGMA temp_store=MEMORY")?;
        conn.pragma_update(None, "cache_size", -65536i64)
            .context("PRAGMA cache_size=-65536")?;
        Ok(conn)
    }

    fn execute(&self, conn: &mut Self::Conn, sql: &str) -> Result<()> {
        conn.execute_batch(sql)
            .with_context(|| format!("rusqlite execute_batch: {sql}"))?;
        Ok(())
    }

    fn execute_with_params(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> Result<()> {
        let bound: Vec<rusqlite::types::Value> = params.iter().map(to_rusqlite).collect();
        conn.execute(sql, rusqlite::params_from_iter(bound.iter()))
            .with_context(|| format!("rusqlite execute: {sql}"))?;
        Ok(())
    }

    fn query_one(&self, conn: &mut Self::Conn, sql: &str, params: &[Value]) -> Result<Vec<Value>> {
        // prepare_cached is the standard rusqlite hot-path idiom — it
        // hits the connection's per-statement LRU cache, so the same
        // SQL string only pays the parse cost once across all bench
        // iterations (cache size defaults to 16, plenty for our
        // workloads).
        let mut stmt = conn
            .prepare_cached(sql)
            .with_context(|| format!("rusqlite prepare_cached: {sql}"))?;
        let bound: Vec<rusqlite::types::Value> = params.iter().map(to_rusqlite).collect();
        let cols = stmt.column_count();
        let mut rows = stmt
            .query(rusqlite::params_from_iter(bound.iter()))
            .with_context(|| format!("rusqlite query: {sql}"))?;
        let row = rows
            .next()
            .with_context(|| format!("rusqlite row read: {sql}"))?
            .context("rusqlite query_one: zero rows returned")?;
        let mut out = Vec::with_capacity(cols);
        for i in 0..cols {
            out.push(extract_column(row, i)?);
        }
        if rows
            .next()
            .with_context(|| format!("rusqlite row read: {sql}"))?
            .is_some()
        {
            anyhow::bail!("rusqlite query_one: >1 rows returned");
        }
        Ok(out)
    }

    fn query_all(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>> {
        let mut stmt = conn
            .prepare_cached(sql)
            .with_context(|| format!("rusqlite prepare_cached: {sql}"))?;
        let bound: Vec<rusqlite::types::Value> = params.iter().map(to_rusqlite).collect();
        let cols = stmt.column_count();
        let mut rows = stmt
            .query(rusqlite::params_from_iter(bound.iter()))
            .with_context(|| format!("rusqlite query: {sql}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .with_context(|| format!("rusqlite row read: {sql}"))?
        {
            let mut buf = Vec::with_capacity(cols);
            for i in 0..cols {
                buf.push(extract_column(row, i)?);
            }
            out.push(buf);
        }
        Ok(out)
    }
}

fn to_rusqlite(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Null => rusqlite::types::Value::Null,
        Value::Integer(i) => rusqlite::types::Value::Integer(*i),
        Value::Real(f) => rusqlite::types::Value::Real(*f),
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
    }
}

fn extract_column(row: &rusqlite::Row<'_>, idx: usize) -> Result<Value> {
    let raw: rusqlite::types::Value = row.get(idx)?;
    Ok(match raw {
        rusqlite::types::Value::Null => Value::Null,
        rusqlite::types::Value::Integer(i) => Value::Integer(i),
        rusqlite::types::Value::Real(f) => Value::Real(f),
        rusqlite::types::Value::Text(s) => Value::Text(s),
        rusqlite::types::Value::Blob(_) => {
            anyhow::bail!("rusqlite extract_column: BLOB not yet supported by bench harness")
        }
    })
}
