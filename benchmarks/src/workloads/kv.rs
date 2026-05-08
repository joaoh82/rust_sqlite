//! W1 — read-by-PK.
//!
//! Schema:
//!
//! ```sql
//! CREATE TABLE kv (
//!   id      INTEGER PRIMARY KEY,
//!   name    TEXT,
//!   payload TEXT
//! );
//! -- 100k rows inserted in one transaction.
//! -- Hot loop: SELECT name, payload FROM kv WHERE id = ?, 10k random keys.
//! ```
//!
//! The hot loop is the reference latency for the engine's hottest
//! path. Both engines have an auto-PK index, so this exercises:
//!
//! - parse + plan (every iter for SQLRite; cached for SQLite)
//! - PK index probe + leaf B-tree fetch
//! - row reassembly (3 cells: int + text + text)
//! - return + iterator setup/teardown

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{W1_KEY_COUNT, W1_ROW_COUNT, W1Dataset, w1_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W1: WorkloadId = WorkloadId {
    id: "W1",
    name: "read-by-pk",
    version: "v2",
};

/// SELECT used by the hot loop. Both engines support `?`-positional
/// binds; the SQLRite driver inlines, the SQLite driver binds via
/// `prepare_cached`.
pub const SELECT_SQL: &str = "SELECT name, payload FROM kv WHERE id = ?";

/// Build the W1 table + populate 100k rows. Returns the open connection
/// + the prebuilt random-key slice the hot loop will iterate.
///
/// Inserts go through one transaction so we don't pay the per-row
/// COMMIT cost — the bulk insert path lands as its own workload (W3)
/// in 9.2 and that's where COMMIT is the unit under measurement.
pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, Vec<i64>)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE kv (id INTEGER PRIMARY KEY, name TEXT, payload TEXT)",
    )?;

    let dataset = w1_dataset();
    insert_rows(driver, &mut conn, &dataset)?;
    Ok((conn, dataset.keys))
}

/// One iteration of the hot loop. Looks up `key`, asserts that exactly
/// one row came back. Returns the row so criterion's `black_box` can
/// see a real result and the optimizer doesn't shortcut the call.
pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, key: i64) -> Result<Vec<Value>> {
    driver.query_one(conn, SELECT_SQL, &[Value::Integer(key)])
}

/// Correctness gate (Q3 mitigation). Run once before the timed loop
/// to verify the engine returns the expected row shape. Comparing the
/// full payload across the whole dataset would dwarf the bench
/// runtime; we sample a handful of fixed keys.
pub fn correctness_check<D: Driver>(driver: &D, conn: &mut D::Conn) -> Result<()> {
    for &key in &[1i64, 50_001, 100_000] {
        let row = driver.query_one(conn, SELECT_SQL, &[Value::Integer(key)])?;
        if row.len() != 2 {
            anyhow::bail!(
                "W1 correctness: expected 2 columns (name, payload), got {}",
                row.len()
            );
        }
        match (&row[0], &row[1]) {
            (Value::Text(name), Value::Text(payload)) => {
                let expected_name = format!("user_{key}");
                if name != &expected_name {
                    anyhow::bail!(
                        "W1 correctness: key {key} → name = {name:?}, expected {expected_name:?}"
                    );
                }
                if payload.len() != 64 {
                    anyhow::bail!(
                        "W1 correctness: key {key} → payload len = {}, expected 64",
                        payload.len()
                    );
                }
            }
            other => anyhow::bail!("W1 correctness: key {key} → unexpected row shape {other:?}"),
        }
    }
    Ok(())
}

fn insert_rows<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &W1Dataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W1 BEGIN")?;
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
            .with_context(|| format!("W1 INSERT id={}", row.id))?;
    }
    driver.execute(conn, "COMMIT").context("W1 COMMIT")?;
    debug_assert_eq!(dataset.rows.len(), W1_ROW_COUNT);
    debug_assert_eq!(dataset.keys.len(), W1_KEY_COUNT);
    Ok(())
}
