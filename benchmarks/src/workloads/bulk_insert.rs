//! W3 — bulk insert (100k rows in one transaction).
//!
//! ```sql
//! CREATE TABLE kv (id INTEGER PRIMARY KEY, name TEXT, payload TEXT);
//! BEGIN;
//! INSERT INTO kv (id, name, payload) VALUES (?, ?, ?);  -- 100k times
//! COMMIT;
//! ```
//!
//! This is the macro-bench: each criterion sample is one full
//! 100k-row transaction. Per-iter cost is dominated by:
//! - parsing 100k INSERTs (SQLRite has no statement cache yet),
//! - cell-encoding 100k rows (~6.4 MB of payload + ints),
//! - the COMMIT — for SQLRite that's a single bottom-up B-tree
//!   rebuild + WAL append + checkpoint; for SQLite-WAL+NORMAL
//!   that's an fsync-on-checkpoint at boundary.
//!
//! The bench uses [`criterion::Criterion::iter_batched`] with
//! [`criterion::BatchSize::PerIteration`] so the table is *recreated
//! fresh per sample* — without that, samples would write into a
//! growing table and the bench would measure an N² rebuild ramp on
//! SQLRite's commit path. Setup (open conn + CREATE TABLE) runs in
//! the batched-setup closure and is excluded from timing.
//!
//! Throughput-wise, the relevant unit is "rows/s" rather than the
//! per-iter latency criterion reports. The aggregator records
//! `median_ns / row_count` in a downstream view; for now the README
//! table just shows median latency for one full bulk insert.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{W1_ROW_COUNT, W1Dataset, w1_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W3: WorkloadId = WorkloadId {
    id: "W3",
    name: "bulk-insert",
    version: "v1",
};

/// One per-iter dataset is reused across samples — the row contents
/// are deterministic so reuse doesn't hide a correctness bug, and
/// regenerating ~6 MB of payload per sample would dominate the
/// "warm-cache" portion of criterion's runtime.
pub fn dataset() -> W1Dataset {
    w1_dataset()
}

/// Per-iter setup: open a fresh DB at `path`, run CREATE TABLE.
/// Returned conn is moved into the timed closure.
pub fn setup_iter<D: Driver>(driver: &D, path: &Path) -> Result<D::Conn> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE kv (id INTEGER PRIMARY KEY, name TEXT, payload TEXT)",
    )?;
    Ok(conn)
}

/// One iteration: BEGIN + 100k INSERTs + COMMIT against the freshly-
/// set-up connection. Caller is responsible for cleaning up the DB
/// file (handled by the per-iter `TempDir` in `benches/suite.rs`).
pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &W1Dataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W3 BEGIN")?;
    for row in &dataset.rows {
        driver
            .execute_with_params(
                conn,
                "INSERT INTO kv (id, name, payload) VALUES (?, ?, ?)",
                &[
                    Value::Integer(row.id),
                    Value::Text(row.name.clone()),
                    Value::Text(row.payload.clone()),
                ],
            )
            .with_context(|| format!("W3 INSERT id={}", row.id))?;
    }
    driver.execute(conn, "COMMIT").context("W3 COMMIT")?;
    debug_assert_eq!(dataset.rows.len(), W1_ROW_COUNT);
    Ok(())
}

/// Correctness gate. Run after one full insert against a probe DB:
/// the row count must equal the dataset's row count.
pub fn correctness_check<D: Driver>(driver: &D, conn: &mut D::Conn, expected: usize) -> Result<()> {
    let rows = driver.query_one(conn, "SELECT COUNT(*) FROM kv", &[])?;
    let got = match rows.first() {
        Some(Value::Integer(n)) => *n,
        other => anyhow::bail!("W3 correctness: COUNT(*) returned unexpected shape: {other:?}"),
    };
    if got as usize != expected {
        anyhow::bail!("W3 correctness: COUNT(*) = {got}, expected {expected}");
    }
    Ok(())
}
