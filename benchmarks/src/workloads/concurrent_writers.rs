//! W13 — concurrent writers, mostly-disjoint rows.
//!
//! ```sql
//! CREATE TABLE counters (id INTEGER PRIMARY KEY, n INTEGER NOT NULL);
//! -- preload 1_000 rows: (1, 0), (2, 0), ..., (1_000, 0)
//! -- N worker threads each run M txs of:
//! --   BEGIN <CONCURRENT|IMMEDIATE>;
//! --   UPDATE counters SET n = n + 1 WHERE id = ?;  -- random id in 1..=1000
//! --   COMMIT;                                     -- retry on Busy/Locked
//! ```
//!
//! The headline differentiator workload Phase 11 was designed for.
//! Two writers on *disjoint* rows make progress in parallel under
//! SQLRite-MVCC; under SQLite they serialize on the WAL writer
//! lock. With `N = 4` workers and `K = 1_000` rows, the per-iter
//! collision probability is roughly `N/K = 0.4%` — "mostly disjoint"
//! is the workload's actual claim, not just its name.
//!
//! ## Per-engine begin / retry shape
//!
//! - **SQLRite-MVCC**: `BEGIN CONCURRENT` doesn't acquire a lock at
//!   BEGIN; the conflict is decided at COMMIT against `MvStore`. The
//!   loser sees `SQLRiteError::Busy` and retries with a fresh
//!   `begin_ts`. See [`docs/concurrent-writes.md`](../../docs/concurrent-writes.md).
//! - **SQLite**: `BEGIN IMMEDIATE` takes the WAL write lock at BEGIN
//!   so two writers can't race into `SQLITE_BUSY` at COMMIT. The
//!   driver's `busy_timeout = 5s` keeps BEGIN from failing
//!   immediately when another worker holds the lock — it blocks
//!   instead. End result: SQLite serializes the writers.
//!
//! Both engines run the same retry-on-busy loop, but only SQLRite
//! actually exercises the retry path under this workload's shape.
//! That contrast is the point.
//!
//! ## Why this isn't generic over `Driver` like W1..W12
//!
//! The other workloads share `bench_iter` across all drivers — they
//! all do the same SQL, the engine differences are entirely below
//! the trait. W13 is different: the *concurrency model itself* is
//! engine-specific (sibling handles vs separate-process handles;
//! BEGIN flavour; retry semantics). The trait absorbs that
//! variation via three new methods (`connect_sibling`,
//! `concurrent_begin_sql`, `is_retryable_busy`) and the workload
//! stays simple.

use std::path::Path;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::{Driver, Value, WorkloadId};

pub const W13: WorkloadId = WorkloadId {
    id: "W13",
    name: "concurrent-writers",
    version: "v1",
};

/// Number of preloaded rows. `4 workers × 0.4% collision per op` is
/// "mostly disjoint" — a higher number would be ~0 conflicts (and
/// stop measuring the retry path on SQLRite), a lower number would
/// flip the workload into "hot-row contention" which is a different
/// scenario (and a fair follow-up: `W13b — hot-row contention`).
pub const W13_PRELOAD_ROWS: i64 = 1_000;

/// Worker count per sample. Matches a typical M-series MBP's
/// performance-core count without pegging the scheduler. Higher
/// values (8, 16) are interesting future work but turn the
/// measurement into "OS scheduler noise" on many laptops; v1 keeps
/// it tight.
pub const W13_N_WORKERS: usize = 4;

/// Transactions per worker per criterion sample. With 4 workers ×
/// 50 txs each, a sample is 200 BEGIN/UPDATE/COMMIT cycles —
/// enough work to dwarf thread-spawn overhead, short enough that
/// a criterion sample finishes in a few hundred ms.
pub const W13_TXS_PER_WORKER: usize = 50;

const W13_SEED: u64 = 13;

/// Sets up the `counters` table and preloads [`W13_PRELOAD_ROWS`]
/// rows with `n = 0`. Returns the primary connection — workers
/// either share it via [`Driver::connect_sibling`] (SQLRite) or
/// each open their own (SQLite, by default).
pub fn setup<D: Driver>(driver: &D, path: &Path) -> Result<D::Conn> {
    let mut conn = driver.open(path)?;
    // Opt the engine into its concurrent path before CREATE so the
    // mode is in force for every later statement. SQLRite needs
    // `PRAGMA journal_mode = mvcc;` here; SQLite's no-op default
    // is fine because its concurrency story is set up at `open`
    // (WAL + busy_timeout).
    driver
        .enable_concurrent_mode(&mut conn)
        .context("W13 enable_concurrent_mode")?;
    driver.execute(
        &mut conn,
        "CREATE TABLE counters (id INTEGER PRIMARY KEY, n INTEGER NOT NULL)",
    )?;
    driver
        .execute(&mut conn, "BEGIN")
        .context("W13 preload BEGIN")?;
    for id in 1..=W13_PRELOAD_ROWS {
        driver
            .execute_with_params(
                &mut conn,
                "INSERT INTO counters (id, n) VALUES (?, ?)",
                &[Value::Integer(id), Value::Integer(0)],
            )
            .with_context(|| format!("W13 preload id={id}"))?;
    }
    driver
        .execute(&mut conn, "COMMIT")
        .context("W13 preload COMMIT")?;
    Ok(conn)
}

/// Drives `n_workers` threads against `path`, each running
/// `txs_per_worker` BEGIN/UPDATE/COMMIT cycles with retry on busy.
/// Returns the total number of committed UPDATEs (expected to equal
/// `n_workers * txs_per_worker` since we retry on conflict).
///
/// The bench loop calls this once per criterion sample. The primary
/// connection is opened up front; each worker either gets a sibling
/// (engines that override [`Driver::connect_sibling`] — SQLRite) or
/// opens its own separate connection (default — SQLite).
pub fn run_concurrent<D>(
    driver: Arc<D>,
    path: &Path,
    n_workers: usize,
    txs_per_worker: usize,
) -> Result<usize>
where
    D: Driver + Clone + 'static,
    D::Conn: Send + 'static,
{
    let mut primary = driver.open(path).context("W13 primary open")?;
    // `PRAGMA journal_mode` (SQLRite) is in-memory per-database
    // state, not persisted to disk. Every fresh `driver.open()`
    // resets it to the default (`Wal`), so we re-enable on every
    // primary acquisition before any worker issues a BEGIN
    // CONCURRENT. Siblings share the same `Arc<Mutex<Database>>`,
    // so toggling here propagates to all of them.
    driver
        .enable_concurrent_mode(&mut primary)
        .context("W13 primary enable_concurrent_mode")?;

    let mut conns: Vec<D::Conn> = Vec::with_capacity(n_workers);
    // Worker 0 gets the primary connection. Workers 1..N get
    // siblings — for SQLRite that's `Connection::connect()` off
    // the primary; for SQLite (default impl) that's a fresh
    // `rusqlite::Connection::open` on the same path.
    let mut primary_opt = Some(primary);
    for i in 0..n_workers {
        if i == 0 {
            conns.push(primary_opt.take().expect("primary present on i=0"));
        } else {
            let sibling = driver
                .connect_sibling(conns.first().expect("primary at conns[0]"), path)
                .with_context(|| format!("W13 connect_sibling worker={i}"))?;
            conns.push(sibling);
        }
    }

    let mut handles = Vec::with_capacity(n_workers);
    for (worker_id, conn) in conns.into_iter().enumerate() {
        let drv = Arc::clone(&driver);
        handles.push(thread::spawn(move || -> Result<usize> {
            run_worker(&*drv, conn, worker_id, txs_per_worker)
        }));
    }

    let mut total = 0usize;
    for (i, h) in handles.into_iter().enumerate() {
        let committed = h
            .join()
            .map_err(|e| anyhow::anyhow!("W13 worker {i} panicked: {e:?}"))?
            .with_context(|| format!("W13 worker {i} returned err"))?;
        total += committed;
    }
    Ok(total)
}

/// One worker's run. Each iteration picks a random row id and runs
/// `BEGIN <flavour>; UPDATE counters SET n = n + 1 WHERE id = ?;
/// COMMIT;` — retrying the whole BEGIN/UPDATE/COMMIT triple on
/// retryable busy errors. The retry strategy is "spin immediately"
/// — picking a backoff policy is the caller's job in real apps;
/// for the bench we want the maximum throughput the engine can
/// deliver with no artificial delay.
fn run_worker<D: Driver>(
    driver: &D,
    mut conn: D::Conn,
    worker_id: usize,
    txs_per_worker: usize,
) -> Result<usize> {
    // Per-worker deterministic RNG so a bench run reproduces the
    // same id-collision pattern across hosts (modulo the worker
    // scheduling itself).
    let mut rng = ChaCha8Rng::seed_from_u64(W13_SEED.wrapping_add(worker_id as u64));
    let begin_sql = driver.concurrent_begin_sql();

    let mut committed = 0usize;
    for _ in 0..txs_per_worker {
        let id: i64 = rng.gen_range(1..=W13_PRELOAD_ROWS);
        loop {
            // Open the tx. On retry the previous attempt has already
            // been rolled back (Busy drops the tx server-side); we
            // need a fresh BEGIN.
            driver
                .execute(&mut conn, begin_sql)
                .with_context(|| format!("W13 worker={worker_id} BEGIN"))?;
            // The UPDATE itself can also error retryably (rare on
            // SQLRite, possible on SQLite if busy_timeout elapses
            // during the statement). Treat it the same as a COMMIT
            // failure — roll back and spin.
            let update_res = driver.execute_with_params(
                &mut conn,
                "UPDATE counters SET n = n + 1 WHERE id = ?",
                &[Value::Integer(id)],
            );
            if let Err(e) = update_res {
                if driver.is_retryable_busy(&e) {
                    let _ = driver.execute(&mut conn, "ROLLBACK");
                    continue;
                }
                return Err(e).with_context(|| format!("W13 worker={worker_id} UPDATE id={id}"));
            }
            match driver.execute(&mut conn, "COMMIT") {
                Ok(()) => {
                    committed += 1;
                    break;
                }
                Err(e) if driver.is_retryable_busy(&e) => {
                    // SQLRite drops the tx on a failed COMMIT; no
                    // explicit ROLLBACK needed (and indeed it
                    // would error "no transaction is open"). For
                    // SQLite the COMMIT failure also clears the
                    // tx state. Either way, spin to retry.
                    continue;
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("W13 worker={worker_id} COMMIT id={id}"));
                }
            }
        }
    }
    Ok(committed)
}

/// Correctness gate. Runs a single 4×10 concurrent burst against a
/// freshly-set-up DB and verifies the post-state matches the
/// expected total. Caught divergences here would point at the
/// retry loop double-counting, the workers dropping commits, or
/// the engine mis-handling a Busy boundary.
pub fn correctness_check<D>(driver: Arc<D>, path: &Path) -> Result<()>
where
    D: Driver + Clone + 'static,
    D::Conn: Send + 'static,
{
    // Re-build a fresh DB at the same path so we don't reuse the
    // setup-touched preload. Drop the file (and SQLRite's WAL
    // sidecar) so `setup` lands a clean schema.
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file({
        let mut p = path.as_os_str().to_owned();
        p.push("-wal");
        std::path::PathBuf::from(p)
    });
    let _ = setup(&*driver, path).context("W13 correctness setup")?;

    let n = 4usize;
    let m = 10usize;
    let committed = run_concurrent(Arc::clone(&driver), path, n, m)?;
    let expected = n * m;
    if committed != expected {
        anyhow::bail!("W13 correctness: workers reported {committed} commits, expected {expected}");
    }

    // The table's total counter sum should equal `committed` —
    // every commit increments exactly one row by exactly one. A
    // sum mismatch would indicate either a lost commit or a
    // double-counted retry, both of which W13 is here to catch.
    let mut probe = driver.open(path)?;
    let row = driver.query_one(&mut probe, "SELECT SUM(n) FROM counters", &[])?;
    match row.first() {
        Some(Value::Integer(sum)) if *sum == committed as i64 => Ok(()),
        Some(Value::Integer(sum)) => {
            anyhow::bail!("W13 correctness: SUM(n) = {sum}, expected {committed}",)
        }
        other => anyhow::bail!("W13 correctness: SUM returned unexpected shape {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::sqlite::SQLiteDriver;
    use crate::drivers::sqlrite::SQLRiteDriver;

    /// Per-driver fast smoke test for W13. Mirrors the criterion
    /// correctness_check shape but without the criterion harness
    /// overhead — run with `cargo test -p sqlrite-benchmarks w13`
    /// to verify the workload works end-to-end in a few seconds.
    fn run_one<D>(driver: D)
    where
        D: Driver + Clone + 'static,
        D::Conn: Send + 'static,
    {
        let tmp = tempfile::Builder::new()
            .prefix(&format!("w13-test-{}-", driver.name()))
            .tempdir()
            .unwrap();
        let path = tmp.path().join(format!(
            "w13.{}",
            match driver.name() {
                "sqlite" => "sqlite",
                "duckdb" => "duckdb",
                _ => "sqlrite",
            }
        ));
        let driver = Arc::new(driver);
        correctness_check(Arc::clone(&driver), &path).unwrap_or_else(|e| {
            let name = driver.name();
            let chain: Vec<String> = e.chain().map(|c| format!("{c}")).collect();
            panic!(
                "W13 correctness on {name}:\n  {}",
                chain.join("\n  caused by: ")
            );
        });
    }

    #[test]
    fn w13_sqlrite_correctness() {
        run_one(SQLRiteDriver);
    }

    #[test]
    fn w13_sqlite_correctness() {
        run_one(SQLiteDriver);
    }
}
