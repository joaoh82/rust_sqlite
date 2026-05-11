//! Benchmark harness — SQLRite vs SQLite (and friends).
//!
//! See [`docs/benchmarks-plan.md`](../../docs/benchmarks-plan.md) for the
//! canonical design + decisions. Sub-phase 9.1 (this PR) lands the
//! scaffolding, the `Driver` trait, the SQLRite + SQLite drivers, and
//! one workload end-to-end (W1 — read-by-PK). Workloads W2–W12 land in
//! 9.2–9.4 by adding one file under `src/workloads/` and one register
//! call in `benches/suite.rs`.
//!
//! ## Driver model
//!
//! Workloads are generic over [`Driver`] — engine-agnostic Rust. Each
//! engine implements the trait once; adding a new comparator (libSQL,
//! DuckDB, …) is one file under `src/drivers/` and a register-call in
//! [`benches/suite.rs`](../../benchmarks/benches/suite.rs).
//!
//! [`Value`] is a small four-variant enum that's enough to express
//! every workload's data shape (the engine's full `Value` type has
//! booleans, vectors, JSON, etc., but bench inputs only need int /
//! real / text / null).
//!
//! ## JSON output schema (Q8 — workload versioning)
//!
//! Every bench iteration is reported in a per-row [`BenchSample`] that
//! carries an explicit `workload_version` (e.g. `"W1.v1"`). The
//! `aggregate` binary collects these into a [`ResultsEnvelope`] under
//! `benchmarks/results/`. Q8's commitment is enforced here: changing
//! a workload's shape requires bumping the version, so historical
//! comparisons can be filtered to same-version runs.

#![allow(clippy::missing_errors_doc)]

use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod data;
pub mod drivers;
pub mod envelope;
pub mod workloads;

/// Driver-side value type. Tight enough that any of the engines under
/// test can map it onto their native bind types — rusqlite has
/// [`rusqlite::ToSql`], DuckDB has its own. SQLRite gained parameter
/// binding in SQLR-23 (incl. `Value::Vector` for HNSW-eligible KNN
/// queries), so the SQLRite driver now binds through
/// `Statement::query_with_params` / `Statement::execute_with_params`
/// instead of formatting a SQL string per call.
///
/// `Vector` is SQLRite-only: SQLite-side drivers raise a clean error
/// rather than silently lying about the type, since the W10/W12
/// workloads that exercise it are explicitly SQLRite-only via
/// `driver_supports`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// Dense f32 query vector — bound directly into VECTOR columns
    /// or distance-function args. SQLRite-only; comparator drivers
    /// surface a typed error if a workload tries to bind a vector
    /// against them.
    Vector(Vec<f32>),
}

/// Engine-agnostic surface every workload binds to.
///
/// Implementations live in `src/drivers/`. The trait is intentionally
/// small: workloads use `execute` for setup (CREATE / INSERT inside a
/// transaction), `query_one` for hot SELECT-by-PK paths, and
/// `query_all` when they need every row (range scans, aggregates).
///
/// The `&self` receiver makes drivers cheap to clone into criterion
/// closures; per-connection mutable state lives in [`Driver::Conn`].
pub trait Driver: Send + Sync {
    /// Connection handle type. Owned per workload run; closed on drop.
    type Conn;

    /// Stable engine label that lands in the JSON envelope and the
    /// criterion bench id (e.g. `"sqlrite"`, `"sqlite"`).
    fn name(&self) -> &'static str;

    /// Open or create a database at `path`. The harness always passes
    /// a fresh path under a per-run [`tempfile::TempDir`].
    fn open(&self, path: &Path) -> anyhow::Result<Self::Conn>;

    /// Run a non-query statement (DDL, INSERT, BEGIN/COMMIT, PRAGMA,
    /// …). No-op return; errors propagate.
    fn execute(&self, conn: &mut Self::Conn, sql: &str) -> anyhow::Result<()>;

    /// Run a non-query statement with positional parameters.
    fn execute_with_params(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> anyhow::Result<()>;

    /// Run a query expected to return a single row. Returns the row's
    /// values in projection order. Errors if zero or >1 rows come
    /// back — the caller is asserting one row by shape.
    fn query_one(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> anyhow::Result<Vec<Value>>;

    /// Run a query and materialize every row. Used for range scans /
    /// aggregates / GROUP BY where the result set is the answer.
    #[allow(dead_code)]
    fn query_all(
        &self,
        conn: &mut Self::Conn,
        sql: &str,
        params: &[Value],
    ) -> anyhow::Result<Vec<Vec<Value>>>;

    /// Phase 11.11b — mint a sibling connection sharing the same
    /// backing state as `primary`. Used by W13 (concurrent writers)
    /// to drive `N` worker threads against the same database from
    /// a single process.
    ///
    /// The default implementation opens a fresh connection at `path`
    /// — appropriate for engines (SQLite, DuckDB) whose per-process
    /// concurrency is mediated by file-level locking + `busy_timeout`.
    /// Engines whose primary opener takes an exclusive lock (SQLRite,
    /// where `Connection::open` calls `flock(LOCK_EX)`) override this
    /// to mint an in-process sibling that shares the lock + the
    /// backing `Arc<Mutex<Database>>`.
    #[allow(dead_code)]
    fn connect_sibling(&self, primary: &Self::Conn, path: &Path) -> anyhow::Result<Self::Conn> {
        let _ = primary;
        self.open(path)
    }

    /// Phase 11.11b — opt the connection into the engine's
    /// concurrent-write mode before W13 issues its first `BEGIN
    /// CONCURRENT` (or equivalent). For SQLRite this runs
    /// `PRAGMA journal_mode = mvcc;`; for SQLite the default is a
    /// no-op (its WAL + busy_timeout setup happens at `open` time).
    ///
    /// Called once per primary connection at workload setup; the
    /// per-database setting then propagates to every sibling
    /// minted via [`Driver::connect_sibling`].
    #[allow(dead_code)]
    fn enable_concurrent_mode(&self, conn: &mut Self::Conn) -> anyhow::Result<()> {
        let _ = conn;
        Ok(())
    }

    /// Phase 11.11b — engine-idiomatic `BEGIN` flavour for the
    /// concurrent-writers workload (W13).
    ///
    /// - SQLRite returns `"BEGIN CONCURRENT"` (MVCC + commit-time validation).
    /// - SQLite returns `"BEGIN IMMEDIATE"` (acquire the write lock at BEGIN
    ///   so two writers don't race into `SQLITE_BUSY` at COMMIT — same shape
    ///   the SQLite docs recommend for multi-writer apps).
    /// - DuckDB / future engines return their idiomatic form.
    ///
    /// Default is plain `"BEGIN"` for engines that don't yet have a
    /// W13 story.
    #[allow(dead_code)]
    fn concurrent_begin_sql(&self) -> &'static str {
        "BEGIN"
    }

    /// Phase 11.11b — does `err` indicate a retryable busy / conflict
    /// from this engine's concurrent path? W13's per-worker loop
    /// retries on `true` and bubbles on `false`.
    ///
    /// Default: no error is retryable. Drivers that override
    /// [`Driver::concurrent_begin_sql`] should override this too so
    /// the workload's retry loop knows when to spin.
    #[allow(dead_code)]
    fn is_retryable_busy(&self, err: &anyhow::Error) -> bool {
        let _ = err;
        false
    }
}

/// Workload-version tag. Mirrors Q8 in `benchmarks-plan.md`: every
/// workload carries an explicit version (`W1.v1`, `W1.v2`, …). The
/// comparison script (lands in 9.6) only diffs same-version pairs and
/// warns on cross-version compares.
///
/// Bumping a workload's version is the explicit "we changed the
/// benchmark" gesture. Old JSON files keep their old version key and
/// stay readable forever.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadId {
    /// Stable id like `"W1"`. Used as the criterion group prefix.
    pub id: &'static str,
    /// Human-readable name, e.g. `"read-by-pk"`.
    pub name: &'static str,
    /// Version tag — increment when the workload's shape changes.
    pub version: &'static str,
}

impl WorkloadId {
    /// `id.vversion`, e.g. `"W1.v1"`. Used as the criterion bench-group
    /// id and the JSON envelope's `workload` key.
    pub fn full(&self) -> String {
        format!("{}.{}", self.id, self.version)
    }
}

/// One sample row in the JSON envelope. Matches criterion's
/// `estimates.json` shape closely so `aggregate` is mostly a copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSample {
    /// Full workload id with version, e.g. `"W1.v1"`.
    pub workload: String,
    /// Engine label from [`Driver::name`].
    pub driver: String,
    /// criterion's median estimate, in nanoseconds per iteration.
    pub median_ns: f64,
    /// 95% confidence interval lower bound on the median, in ns.
    pub median_ci_lower_ns: f64,
    /// 95% confidence interval upper bound on the median, in ns.
    pub median_ci_upper_ns: f64,
    /// criterion's mean estimate, in ns/iter.
    pub mean_ns: f64,
    /// Standard deviation, in ns/iter.
    pub std_dev_ns: f64,
    /// Number of samples criterion took (default 100).
    pub samples: u64,
    /// Throughput, ops/s — derived from `1e9 / median_ns`.
    pub ops_per_s: f64,
}
