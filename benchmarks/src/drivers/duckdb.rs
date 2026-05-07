//! DuckDB driver via [`duckdb-rs`] (bundled libduckdb).
//!
//! Wired into Group B only — W7 / W8 / W9. The plan-doc viability
//! section explicitly excludes Group A (single-row INSERTs are
//! pathological by design on a columnar engine) and Group C
//! (DuckDB doesn't ship vector or BM25 native primitives the way
//! SQLRite / SQLite + extensions do).
//!
//! Feature-gated under `duckdb`. `make bench-duckdb` enables it;
//! plain `make bench` keeps the heavier dep out of the build.
//!
//! ## Configuration
//!
//! Defaults. DuckDB's out-of-the-box settings are already sensible
//! for analytical workloads — there's no equivalent to SQLite's
//! `WAL+NORMAL` opt-in (DuckDB's MVCC + commit semantics are uniform).
//! That asymmetry is documented in the README; the SQLite tuned
//! profile (Q3) and the DuckDB defaults are both "the headline"
//! configuration on their respective engines.

use std::path::Path;

use anyhow::{Context, Result};

use crate::{Driver, Value};

pub struct DuckDBDriver;

impl Driver for DuckDBDriver {
    type Conn = duckdb::Connection;

    fn name(&self) -> &'static str {
        "duckdb"
    }

    fn open(&self, path: &Path) -> Result<Self::Conn> {
        duckdb::Connection::open(path).with_context(|| format!("duckdb open({})", path.display()))
    }

    fn execute(&self, conn: &mut Self::Conn, sql: &str) -> Result<()> {
        conn.execute_batch(sql)
            .with_context(|| format!("duckdb execute_batch: {sql}"))?;
        Ok(())
    }

    fn execute_with_params(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> Result<()> {
        let bound: Vec<duckdb::types::Value> = params.iter().map(to_duckdb).collect();
        conn.execute(sql, duckdb::params_from_iter(bound.iter()))
            .with_context(|| format!("duckdb execute: {sql}"))?;
        Ok(())
    }

    fn query_one(&self, conn: &mut Self::Conn, sql: &str, params: &[Value]) -> Result<Vec<Value>> {
        // duckdb-rs doesn't expose a `prepare_cached` (the C API
        // doesn't have an LRU under the hood the way libsqlite3 does);
        // every query reprepares, which is honest for the comparison —
        // the SQL parser is part of what we're measuring. Per-iter
        // parse cost on DuckDB's optimizer is heavier than libsqlite3
        // on simple queries, but DuckDB amortizes that with a much
        // faster vectorized executor on full-scan paths (W7 / W8).
        //
        // DuckDB's column_count is only valid AFTER query execution
        // (it reads from the result struct, not the parsed plan), so
        // we pull it from the first Row via `Row::as_ref()`.
        let mut stmt = conn
            .prepare(sql)
            .with_context(|| format!("duckdb prepare: {sql}"))?;
        let bound: Vec<duckdb::types::Value> = params.iter().map(to_duckdb).collect();
        let mut rows = stmt
            .query(duckdb::params_from_iter(bound.iter()))
            .with_context(|| format!("duckdb query: {sql}"))?;
        let row = rows
            .next()
            .with_context(|| format!("duckdb row read: {sql}"))?
            .context("duckdb query_one: zero rows returned")?;
        let cols = row.as_ref().column_count();
        let mut out = Vec::with_capacity(cols);
        for i in 0..cols {
            out.push(extract_column(row, i)?);
        }
        if rows
            .next()
            .with_context(|| format!("duckdb row read: {sql}"))?
            .is_some()
        {
            anyhow::bail!("duckdb query_one: >1 rows returned");
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
            .prepare(sql)
            .with_context(|| format!("duckdb prepare: {sql}"))?;
        let bound: Vec<duckdb::types::Value> = params.iter().map(to_duckdb).collect();
        let mut rows = stmt
            .query(duckdb::params_from_iter(bound.iter()))
            .with_context(|| format!("duckdb query: {sql}"))?;
        let mut out = Vec::new();
        let mut cols: Option<usize> = None;
        while let Some(row) = rows
            .next()
            .with_context(|| format!("duckdb row read: {sql}"))?
        {
            // Column count is the same for every row in a result set;
            // grab it once from the first row.
            let n = *cols.get_or_insert_with(|| row.as_ref().column_count());
            let mut buf = Vec::with_capacity(n);
            for i in 0..n {
                buf.push(extract_column(row, i)?);
            }
            out.push(buf);
        }
        Ok(out)
    }
}

fn to_duckdb(v: &Value) -> duckdb::types::Value {
    match v {
        Value::Null => duckdb::types::Value::Null,
        Value::Integer(i) => duckdb::types::Value::BigInt(*i),
        Value::Real(f) => duckdb::types::Value::Double(*f),
        Value::Text(s) => duckdb::types::Value::Text(s.clone()),
    }
}

fn extract_column(row: &duckdb::Row<'_>, idx: usize) -> Result<Value> {
    // DuckDB returns rich types — coerce the integer family down to
    // i64 (the harness's Value::Integer). Booleans, decimals, dates,
    // etc. aren't produced by any Group B workload's projections, so
    // the catch-all bails loudly if one shows up.
    let raw: duckdb::types::Value = row.get(idx)?;
    Ok(match raw {
        duckdb::types::Value::Null => Value::Null,
        duckdb::types::Value::TinyInt(n) => Value::Integer(n as i64),
        duckdb::types::Value::SmallInt(n) => Value::Integer(n as i64),
        duckdb::types::Value::Int(n) => Value::Integer(n as i64),
        duckdb::types::Value::BigInt(n) => Value::Integer(n),
        // DuckDB widens SUM(BIGINT) to HugeInt(i128) defensively, even
        // when the result fits in i64. W7's `SUM(v)` over 1M small ints
        // produces ~5e8 — comfortably in range. Downcast when it fits;
        // bail loudly if a future workload genuinely overflows.
        duckdb::types::Value::HugeInt(n) => {
            if n >= i64::MIN as i128 && n <= i64::MAX as i128 {
                Value::Integer(n as i64)
            } else {
                anyhow::bail!("duckdb extract_column: HugeInt {n} doesn't fit i64");
            }
        }
        duckdb::types::Value::UTinyInt(n) => Value::Integer(n as i64),
        duckdb::types::Value::USmallInt(n) => Value::Integer(n as i64),
        duckdb::types::Value::UInt(n) => Value::Integer(n as i64),
        duckdb::types::Value::UBigInt(n) => Value::Integer(n as i64),
        duckdb::types::Value::Boolean(b) => Value::Integer(b as i64),
        duckdb::types::Value::Float(f) => Value::Real(f as f64),
        duckdb::types::Value::Double(f) => Value::Real(f),
        duckdb::types::Value::Text(s) => Value::Text(s),
        other => anyhow::bail!("duckdb extract_column: unsupported type {other:?}"),
    })
}
