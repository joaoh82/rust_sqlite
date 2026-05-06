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
/// [`rusqlite::ToSql`], DuckDB has its own; SQLRite has no parameter
/// binding yet so the SQLRite driver inlines via SQL formatting.
///
/// Deliberately doesn't carry every type the engines support
/// (booleans, vectors, JSON, blobs); workload inputs only need these
/// four. New variants land alongside the workload that needs them.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
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
