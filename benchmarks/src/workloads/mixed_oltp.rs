//! W5 — mixed OLTP (50/50 SELECT-by-PK + UPDATE-by-PK).
//!
//! ```sql
//! CREATE TABLE kv2 (id INTEGER PRIMARY KEY, secondary INTEGER, payload TEXT);
//! CREATE UNIQUE INDEX idx_kv2_secondary ON kv2(secondary);
//! -- 100k rows from group_a_dataset.
//! -- Hot loop alternates two op shapes:
//! --   SELECT id, secondary, payload FROM kv2 WHERE id = ?
//! --   UPDATE kv2 SET payload = ? WHERE id = ?
//! ```
//!
//! YCSB-A flavor: half reads, half writes, all by primary key. The
//! plan calls for "100k-row keyed table, 10k ops" — criterion's
//! per-iter loop drives that count organically (one mixed op per
//! iter, criterion picks the iter count to fit its measurement
//! window).
//!
//! The 50/50 mix is deterministic — even iterations are SELECT, odd
//! iterations are UPDATE. That gives stable mix ratios across runs
//! and keeps both operation paths warm in cache. The keys for both
//! op kinds rotate through a pre-shuffled `pk_probes` slice so PK
//! traversal is unpredictable.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{GROUP_A_ROW_COUNT, GroupADataset, group_a_dataset};
use crate::{Driver, Value, WorkloadId};

pub const W5: WorkloadId = WorkloadId {
    id: "W5",
    name: "mixed-oltp",
    version: "v2",
};

pub const SELECT_SQL: &str = "SELECT id, secondary, payload FROM kv2 WHERE id = ?";
pub const UPDATE_SQL: &str = "UPDATE kv2 SET payload = ? WHERE id = ?";

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

/// One iteration. Even `iter_idx` → SELECT; odd → UPDATE.
///
/// For UPDATEs, the new payload is a function of the iter index so
/// successive UPDATEs on the same key actually change the column
/// (some engines short-circuit no-op UPDATEs). The new payload keeps
/// the 64-char width of the originals.
pub fn bench_iter<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    iter_idx: usize,
    keys: &[i64],
) -> Result<()> {
    let key = keys[iter_idx % keys.len()];
    if iter_idx % 2 == 0 {
        let row = driver.query_one(conn, SELECT_SQL, &[Value::Integer(key)])?;
        if row.len() != 3 {
            anyhow::bail!("W5 SELECT: unexpected row width {}", row.len());
        }
    } else {
        let new_payload = update_payload(iter_idx);
        driver.execute_with_params(
            conn,
            UPDATE_SQL,
            &[Value::Text(new_payload), Value::Integer(key)],
        )?;
    }
    Ok(())
}

pub fn correctness_check<D: Driver>(
    driver: &D,
    conn: &mut D::Conn,
    dataset: &GroupADataset,
) -> Result<()> {
    // SELECT round-trip on a known key.
    let key = dataset.rows[0].id;
    let row = driver.query_one(conn, SELECT_SQL, &[Value::Integer(key)])?;
    if row.len() != 3 {
        anyhow::bail!("W5 correctness: SELECT row width {}", row.len());
    }
    // UPDATE / SELECT round-trip.
    let new_payload = update_payload(99_999);
    driver.execute_with_params(
        conn,
        UPDATE_SQL,
        &[Value::Text(new_payload.clone()), Value::Integer(key)],
    )?;
    let after = driver.query_one(conn, SELECT_SQL, &[Value::Integer(key)])?;
    match after.get(2) {
        Some(Value::Text(s)) if s == &new_payload => Ok(()),
        other => anyhow::bail!("W5 correctness: UPDATE round-trip got {other:?}"),
    }
}

fn update_payload(iter_idx: usize) -> String {
    // 64 chars, deterministic per iter index. Same width as the
    // original payload so the row's cell layout stays stable.
    let mut s = String::with_capacity(64);
    let mut x = (iter_idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for _ in 0..8 {
        s.push_str(&format!("{x:016x}"));
        x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        if s.len() >= 64 {
            s.truncate(64);
            break;
        }
    }
    s
}

fn insert_rows<D: Driver>(driver: &D, conn: &mut D::Conn, dataset: &GroupADataset) -> Result<()> {
    driver.execute(conn, "BEGIN").context("W5 BEGIN")?;
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
            .with_context(|| format!("W5 INSERT id={}", row.id))?;
    }
    driver.execute(conn, "COMMIT").context("W5 COMMIT")?;
    debug_assert_eq!(dataset.rows.len(), GROUP_A_ROW_COUNT);
    Ok(())
}
