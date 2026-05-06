//! JSON output envelope for `benchmarks/results/*.json`.
//!
//! The shape is locked in 9.1 (Q8 — workload version + host fingerprint
//! together let same-version, same-host runs be diffed mechanically).
//! The aggregator binary (`src/bin/aggregate.rs`) walks
//! `target/criterion/**/new/estimates.json` after `cargo bench` and
//! emits one [`ResultsEnvelope`] per `make bench` invocation.
//!
//! Schema rationale:
//! - `host` carries enough to know "is this run comparable to that
//!   one?" — CPU model, RAM, OS family, kernel rev. The plan's "macOS
//!   vs Linux skew" risk is addressed by including `os_kind` so the
//!   comparison script can refuse cross-OS diffs.
//! - `commit` (full SHA + dirty bit) lets us recover what was tested.
//! - `samples` is the per-(workload, driver) row, carrying every
//!   number criterion produced. `compare.py` (lands in 9.6) reads
//!   this directly.

use serde::{Deserialize, Serialize};

use crate::BenchSample;

/// Top-level results envelope. One file = one `make bench` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultsEnvelope {
    /// Schema version. Bump when the envelope shape changes; allows
    /// `compare.py` to refuse to diff incompatible files.
    pub schema_version: u32,
    /// Run timestamp in RFC 3339 (UTC). Captured at run start.
    pub run_started_at: String,
    /// Run duration, seconds wall-clock. Captured by the aggregator.
    pub run_duration_secs: f64,
    /// Host fingerprint (CPU / RAM / OS).
    pub host: HostInfo,
    /// Repo state at run time.
    pub commit: CommitInfo,
    /// One row per (workload-version, driver) pair.
    pub samples: Vec<BenchSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    /// CPU model string. On macOS read from `sysctl machdep.cpu.brand_string`;
    /// on Linux the first `model name` line in `/proc/cpuinfo`.
    pub cpu: String,
    /// Logical CPU count.
    pub cpus: u32,
    /// Total RAM in MiB.
    pub ram_mib: u64,
    /// `linux`, `macos`, `windows`, …
    pub os_kind: String,
    /// Kernel / OS version string (`uname -r`).
    pub os_release: String,
    /// CPU arch (`aarch64`, `x86_64`, …).
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Full git SHA at run time. `"unknown"` if the run was outside a
    /// git checkout.
    pub sha: String,
    /// Branch name at run time.
    pub branch: String,
    /// Working tree dirty? (uncommitted changes present)
    pub dirty: bool,
}

/// Schema version. Bump whenever any field in [`ResultsEnvelope`] or
/// [`BenchSample`] changes shape; `compare.py` reads this and refuses
/// cross-version diffs.
pub const SCHEMA_VERSION: u32 = 1;
