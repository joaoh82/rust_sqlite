//! W6 — secondary-index lookup.
//!
//! ```sql
//! CREATE TABLE kv2 (id INTEGER PRIMARY KEY, secondary INTEGER, payload TEXT);
//! CREATE UNIQUE INDEX idx_kv2_secondary ON kv2(secondary);
//! -- Hot loop: SELECT id, payload FROM kv2 WHERE secondary = ?, 10k probes.
//! ```
//!
//! Tests the secondary-index ROWID indirection path. SQLRite's tiny
//! optimizer recognizes `<indexed_col> = <literal>` and probes the
//! index — see [`docs/supported-sql.md:210`](../../docs/supported-sql.md).
//! So unlike W2 (range scan), W6 should hit the index fast-path on
//! both engines and the comparison is "B-tree probe + secondary fetch
//! to row" vs "B-tree probe + row reassembly."
//!
//! Probes are unique-row lookups (`secondary` is a permutation of
//! `1..=100_000`) so every `secondary = ?` matches exactly one row.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{GROUP_A_ROW_COUNT, GroupADataset, group_a_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W6: WorkloadId = WorkloadId {
    id: "W6",
    name: "index-lookup",
    version: "v2",
};

pub const SELECT_SQL: &str = "SELECT id, payload FROM kv2 WHERE secondary = ?";

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

pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, secondary: i64) -> Result<Vec<Value>> {
    driver.query_one(conn, SELECT_SQL, &[Value::Integer(secondary)])
}

pub fn correctness_check<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &GroupADataset,
) -> Result<()> {
    // Pick the first three secondary values from the dataset; assert
    // each round-trips to the right (id, payload) pair.
    for row in dataset.rows.iter().take(3) {
        let result = driver.query_one(conn, SELECT_SQL, &[Value::Integer(row.secondary)])?;
        match (result.first(), result.get(1)) {
            (Some(Value::Integer(got_id)), Some(Value::Text(got_payload))) => {
                if *got_id != row.id {
                    anyhow::bail!(
                        "W6 correctness: secondary={} → id {got_id}, expected {}",
                        row.secondary,
                        row.id
                    );
                }
                if got_payload != &row.payload {
                    anyhow::bail!(
                        "W6 correctness: secondary={} → payload mismatch",
                        row.secondary
                    );
                }
            }
            other => anyhow::bail!("W6 correctness: unexpected row shape {other:?}"),
        }
    }
    Ok(())
}

fn insert_rows<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &GroupADataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W6 BEGIN")?;
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
            .with_context(|| format!("W6 INSERT id={}", row.id))?;
    }
    driver.execute(conn, "COMMIT").context("W6 COMMIT")?;
    debug_assert_eq!(dataset.rows.len(), GROUP_A_ROW_COUNT);
    Ok(())
}
