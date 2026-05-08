//! W8 — `SELECT k, COUNT(*) FROM big GROUP BY k` at three cardinalities.
//!
//! Three buckets:
//! - **k_10** — 10 distinct groups; lots of rows per group, the
//!   hash-aggregator is mostly in cache.
//! - **k_1k** — 1k distinct groups.
//! - **k_100k** — 100k distinct groups; one row per ~10 of input,
//!   the high-cardinality stress test.
//!
//! All three queries scan the same 1M-row `big` table. The shape is
//! "scan + group + count" — no WHERE, no ORDER BY, no LIMIT — so
//! the comparison is "engine's hash-aggregator vs SQLite's sort-then-
//! aggregate / hash-aggregate planner choice".
//!
//! Per-iter cost is dominated by:
//! - parse + plan (every iter for SQLRite; cached on SQLite)
//! - full table scan
//! - hash insertion / counter increment per row
//! - result materialization (10 / 1k / 100k rows depending on bucket)

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{GROUP_B_ROW_COUNT, GroupBDataset, group_b_dataset};
use crate::workloads::aggregate as w7;
use crate::{Driver, WorkloadId};

pub const W8: WorkloadId = WorkloadId {
    id: "W8",
    name: "group-by",
    version: "v2",
};

/// Cardinality buckets. `(label, group-key column, expected group count)`.
pub const BUCKETS: [(&str, &str, usize); 3] = [
    ("card-10", "k_10", 10),
    ("card-1k", "k_1k", 1_000),
    ("card-100k", "k_100k", 100_000),
];

/// Reuses W7's `big` table — same schema, same dataset. The criterion
/// register fn makes one connection and runs all three buckets
/// against it.
pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, GroupBDataset)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE big (id INTEGER PRIMARY KEY, v INTEGER, k_10 INTEGER, k_1k INTEGER, k_100k INTEGER)",
    )?;
    let dataset = group_b_dataset();
    w7::insert_rows(driver, &mut conn, &dataset)?;
    Ok((conn, dataset))
}

pub fn select_sql(bucket: &str) -> String {
    format!("SELECT {bucket}, COUNT(*) FROM big GROUP BY {bucket}")
}

/// One iteration: run the GROUP BY for one bucket and return the
/// number of groups that came back.
pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, bucket: &str) -> Result<usize> {
    let sql = select_sql(bucket);
    let rows = driver.query_all(conn, &sql, &[])?;
    Ok(rows.len())
}

/// Correctness gate. Run the GROUP BY at each cardinality and verify
/// the group count matches.
pub fn correctness_check<D: Driver>(driver: &D, conn: &mut D::Conn) -> Result<()> {
    for &(label, bucket, expected) in &BUCKETS {
        let got =
            bench_iter(driver, conn, bucket).with_context(|| format!("W8 correctness {label}"))?;
        if got != expected {
            anyhow::bail!(
                "W8 correctness ({label}): GROUP BY {bucket} returned {got} groups, expected {expected}"
            );
        }
    }
    const _: () = assert!(GROUP_B_ROW_COUNT >= 100_000);
    Ok(())
}
