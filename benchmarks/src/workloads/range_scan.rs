//! W2 — range scan over an indexed column.
//!
//! ```sql
//! CREATE TABLE kv2 (id INTEGER PRIMARY KEY, secondary INTEGER, payload TEXT);
//! CREATE UNIQUE INDEX idx_kv2_secondary ON kv2(secondary);
//! -- 100k rows; secondary is a permutation of 1..=100_000.
//! -- Hot loop: SELECT id, secondary, payload FROM kv2
//! --          WHERE secondary >= ? AND secondary <= ?, three width buckets.
//! ```
//!
//! The plan calls for `BETWEEN x AND y`. SQLRite [doesn't implement
//! `BETWEEN` yet](../../docs/supported-sql.md#not-yet-supported), so the
//! workload uses the equivalent `>= ? AND <= ?` form — semantically
//! identical, both engines parse / execute it cleanly. Bumping to
//! `BETWEEN` once the engine supports it is a workload-version bump
//! (`W2.v2`).
//!
//! ## Methodology note
//!
//! Per [`docs/supported-sql.md:210`](../../docs/supported-sql.md):
//! SQLRite's tiny optimizer probes the index only on `<col> = <literal>`
//! shape — range predicates *fall back to a full table scan*. SQLite's
//! optimizer uses the index for range scans. So W2 numbers are
//! expected to show a much wider gap than W1: the scan is comparing
//! "SQLRite full-scan + filter" against "SQLite index range probe".
//! That's the honest comparison for the engine's current state and is
//! itself a roadmap input — a future range-scan optimizer follow-up
//! has W2 as its yardstick.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{GROUP_A_ROW_COUNT, GroupADataset, group_a_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W2: WorkloadId = WorkloadId {
    id: "W2",
    name: "range-scan",
    version: "v2",
};

pub const SELECT_SQL: &str =
    "SELECT id, secondary, payload FROM kv2 WHERE secondary >= ? AND secondary <= ?";

/// Range size buckets. Plan calls for "100 / 1k / 10k rows" — these
/// are the only three sizes shipped in v1. Adding a bucket = `v2`.
pub const RANGE_SIZES: [(&str, i64); 3] = [("100", 100), ("1k", 1_000), ("10k", 10_000)];

pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, GroupADataset)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE kv2 (id INTEGER PRIMARY KEY, secondary INTEGER, payload TEXT)",
    )?;
    driver.execute(
        &mut conn,
        "CREATE UNIQUE INDEX idx_kv2_secondary ON kv2(secondary)",
    )?;
    let dataset = group_a_dataset();
    insert_rows(driver, &mut conn, &dataset)?;
    Ok((conn, dataset))
}

/// One iteration: count the rows that come back from the range scan.
/// Returning the count (rather than the rows themselves) keeps the
/// black_box fingerprint constant across iterations and lets us assert
/// the row count matches the requested range width.
pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, lo: i64, hi: i64) -> Result<usize> {
    let rows = driver.query_all(conn, SELECT_SQL, &[Value::Integer(lo), Value::Integer(hi)])?;
    Ok(rows.len())
}

/// Correctness gate. Verifies the engine returns exactly `width` rows
/// for a known-safe range, and that each row has the 3-column shape.
/// Run once per (workload, driver) pair before the timed loop.
pub fn correctness_check<D: Driver>(driver: &D, conn: &mut D::Conn, width: i64) -> Result<()> {
    let lo = 1_000;
    let hi = lo + width - 1;
    let rows = driver.query_all(conn, SELECT_SQL, &[Value::Integer(lo), Value::Integer(hi)])?;
    let count = rows.len() as i64;
    if count != width {
        anyhow::bail!("W2 correctness: range [{lo}, {hi}] returned {count} rows, expected {width}");
    }
    for r in rows.iter().take(3) {
        if r.len() != 3 {
            anyhow::bail!(
                "W2 correctness: expected 3 columns (id, secondary, payload), got {}",
                r.len()
            );
        }
    }
    Ok(())
}

fn insert_rows<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &GroupADataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W2 BEGIN")?;
    for row in &dataset.rows {
        driver
            .execute_with_params(
                conn,
                "INSERT INTO kv2 (id, secondary, payload) VALUES (?, ?, ?)",
                &[
                    Value::Integer(row.id),
                    Value::Integer(row.secondary),
                    Value::Text(row.payload.clone()),
                ],
            )
            .with_context(|| format!("W2 INSERT id={}", row.id))?;
    }
    driver.execute(conn, "COMMIT").context("W2 COMMIT")?;
    debug_assert_eq!(dataset.rows.len(), GROUP_A_ROW_COUNT);
    Ok(())
}
