//! W7 — `SELECT SUM(v) FROM big` over 1M rows.
//!
//! Full-scan + accumulator throughput. The query is the simplest
//! possible aggregate — no GROUP BY, no WHERE, no projection-side
//! arithmetic. SQLRite's executor (after the SQLR-3 GROUP BY /
//! aggregates landing in v0.7.0) walks every row and accumulates a
//! running `i64` sum; SQLite walks every row through its VM. This
//! workload measures *the cost of touching every row* on a single
//! engine more than anything else.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{GROUP_B_ROW_COUNT, GroupBDataset, group_b_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W7: WorkloadId = WorkloadId {
    id: "W7",
    name: "aggregate-sum",
    version: "v2",
};

pub const SELECT_SQL: &str = "SELECT SUM(v) FROM big";

pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, GroupBDataset)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE big (id INTEGER PRIMARY KEY, v INTEGER, k_10 INTEGER, k_1k INTEGER, k_100k INTEGER)",
    )?;
    let dataset = group_b_dataset();
    insert_rows(driver, &mut conn, &dataset)?;
    Ok((conn, dataset))
}

pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn) -> Result<i64> {
    let row = driver.query_one(conn, SELECT_SQL, &[])?;
    match row.first() {
        Some(Value::Integer(n)) => Ok(*n),
        Some(Value::Real(f)) => Ok(*f as i64),
        other => anyhow::bail!("W7: SUM(v) returned {other:?}"),
    }
}

pub fn correctness_check<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &GroupBDataset,
) -> Result<()> {
    let got = bench_iter(driver, conn)?;
    if got != dataset.sum_v {
        anyhow::bail!("W7 correctness: SUM(v) = {got}, expected {}", dataset.sum_v);
    }
    Ok(())
}

pub(crate) fn insert_rows<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &GroupBDataset,
) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W7 BEGIN")?;
    for row in &dataset.rows {
        driver
            .execute_with_params(
                conn,
                "INSERT INTO big (id, v, k_10, k_1k, k_100k) VALUES (?, ?, ?, ?, ?)",
                &[
                    Value::Integer(row.id),
                    Value::Integer(row.v),
                    Value::Integer(row.k_10),
                    Value::Integer(row.k_1k),
                    Value::Integer(row.k_100k),
                ],
            )
            .with_context(|| format!("W7 INSERT id={}", row.id))?;
    }
    driver.execute(conn, "COMMIT").context("W7 COMMIT")?;
    debug_assert_eq!(dataset.rows.len(), GROUP_B_ROW_COUNT);
    Ok(())
}
