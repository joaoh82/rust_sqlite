//! W4 — single-row insert (each in its own implicit transaction).
//!
//! ```sql
//! CREATE TABLE kv_writes (id INTEGER PRIMARY KEY, payload TEXT);
//! -- preload 1000 rows so the table has a representative size
//! -- before timing begins (see "Preload rationale" below)
//! INSERT INTO kv_writes (id, payload) VALUES (?, ?);  -- one per iter
//! ```
//!
//! W4 is the fsync / commit-cost hot path. Each iter is one INSERT
//! that auto-commits (no `BEGIN` wrapping) — for SQLRite that
//! triggers a full bottom-up B-tree rebuild + WAL append + checkpoint
//! per row; for SQLite-WAL+NORMAL that's "WAL frame append, fsync at
//! checkpoint boundary." The per-iter latency is dominated by these
//! commit-side costs, which is why the plan flags W4 as the workload
//! most likely to surface a >100× gap. (If it does, file an
//! investigation follow-up before moving on.)
//!
//! ## Preload rationale
//!
//! With no preload the first iters hit a near-empty table and later
//! iters hit a table grown to `iters_so_far` rows. SQLRite's
//! bottom-up commit is O(N) per insert, so without a preload the
//! per-iter cost climbs over the bench window and criterion's median
//! reflects "table size ≈ samples/2." Preloading [`W4_PRELOAD_ROWS`]
//! puts the table at a stable size before the timed loop starts, so
//! the median reflects "small-table single-row INSERT" — the actual
//! engineering question.
//!
//! Bumping the preload (e.g. to 10k or 100k) would measure
//! "medium / large-table commit cost" and is worth a separate
//! workload eventually (`W4b` or `W4.v2`). v1 keeps the smaller
//! preload so the headline number isn't dominated by O(N) rebuild
//! work that's a separately-tracked roadmap item.

use std::path::Path;

use anyhow::{Context, Result};

use crate::data::{W4_PAYLOAD, W4_PRELOAD_ROWS};
use crate::{Driver, Value, WorkloadId};

pub const W4: WorkloadId = WorkloadId {
    id: "W4",
    name: "single-insert",
    version: "v2",
};

/// Setup: open fresh DB, create `kv_writes`, preload [`W4_PRELOAD_ROWS`]
/// rows in one transaction (so preload doesn't pay per-row commit
/// cost — that's W4's measurement, not part of setup).
///
/// Returns the conn + the next id the bench loop should use (one
/// past the preload).
pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<(D::Conn, i64)> {
    let mut conn = driver.open(path)?;
    driver.execute(
        &mut conn,
        "CREATE TABLE kv_writes (id INTEGER PRIMARY KEY, payload TEXT)",
    )?;
    driver
        .execute(&mut conn, "BEGIN")
        .context("W4 preload BEGIN")?;
    for id in 1..=W4_PRELOAD_ROWS {
        driver
            .execute_with_params(
                &mut conn,
                "INSERT INTO kv_writes (id, payload) VALUES (?, ?)",
                &[Value::Integer(id), Value::Text(W4_PAYLOAD.to_string())],
            )
            .with_context(|| format!("W4 preload id={id}"))?;
    }
    driver
        .execute(&mut conn, "COMMIT")
        .context("W4 preload COMMIT")?;
    Ok((conn, W4_PRELOAD_ROWS + 1))
}

/// One iteration: a single auto-committed INSERT with the given id.
pub fn bench_iter<D: Driver>(driver: &D, conn: &mut D::Conn, id: i64) -> Result<()> {
    driver
        .execute_with_params(
            conn,
            "INSERT INTO kv_writes (id, payload) VALUES (?, ?)",
            &[Value::Integer(id), Value::Text(W4_PAYLOAD.to_string())],
        )
        .with_context(|| format!("W4 INSERT id={id}"))
}

/// Correctness gate. After setup the table must contain exactly
/// [`W4_PRELOAD_ROWS`]; insert one extra and verify it round-trips.
pub fn correctness_check<D: Driver>(driver: &D, conn: &mut D::Conn) -> Result<()> {
    let probe_id = W4_PRELOAD_ROWS + 1_000_000; // far past the preload
    bench_iter(driver, conn, probe_id)?;
    let row = driver.query_one(
        conn,
        "SELECT id, payload FROM kv_writes WHERE id = ?",
        &[Value::Integer(probe_id)],
    )?;
    match (row.first(), row.get(1)) {
        (Some(Value::Integer(got_id)), Some(Value::Text(got_payload))) => {
            if *got_id != probe_id {
                anyhow::bail!("W4 correctness: id round-trip {got_id} != {probe_id}");
            }
            if got_payload != W4_PAYLOAD {
                anyhow::bail!("W4 correctness: payload round-trip mismatch");
            }
        }
        other => anyhow::bail!("W4 correctness: unexpected row shape {other:?}"),
    }
    // Clean up the correctness-probe row so it doesn't collide with
    // the bench loop's id space (which starts at W4_PRELOAD_ROWS+1).
    driver.execute_with_params(
        conn,
        "DELETE FROM kv_writes WHERE id = ?",
        &[Value::Integer(probe_id)],
    )?;
    Ok(())
}
