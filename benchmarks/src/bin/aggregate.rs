//! Aggregator binary — walks `target/criterion/` after `cargo bench`
//! and emits a single results JSON file under `benchmarks/results/`.
//!
//! Run via `scripts/run.sh` (which `make bench` invokes), not directly.
//! Usage:
//!
//! ```text
//! aggregate --criterion-dir <path> --output <path> --run-started-at <RFC3339>
//! ```
//!
//! `criterion-dir` defaults to `target/criterion`. `output` defaults
//! to a date-host-commit file under `benchmarks/results/`.
//!
//! Criterion writes per-bench `estimates.json` files at
//! `<group>/<bench_id>/new/estimates.json`. We harvest each one,
//! reconstruct the (workload, driver) pair from the directory name,
//! and emit one [`BenchSample`] per pair into a [`ResultsEnvelope`].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::Deserialize;
use sqlrite_benchmarks::BenchSample;
use sqlrite_benchmarks::envelope::{CommitInfo, HostInfo, ResultsEnvelope, SCHEMA_VERSION};
use walkdir::WalkDir;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut criterion_dir = PathBuf::from("target/criterion");
    let mut output: Option<PathBuf> = None;
    let mut run_started_at: Option<String> = None;
    let mut run_duration_secs: Option<f64> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--criterion-dir" => {
                criterion_dir = PathBuf::from(args.next().context("--criterion-dir <path>")?);
            }
            "--output" => {
                output = Some(PathBuf::from(args.next().context("--output <path>")?));
            }
            "--run-started-at" => {
                run_started_at = Some(args.next().context("--run-started-at <rfc3339>")?);
            }
            "--run-duration-secs" => {
                run_duration_secs = Some(
                    args.next()
                        .context("--run-duration-secs <secs>")?
                        .parse()
                        .context("--run-duration-secs parse")?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "usage: aggregate [--criterion-dir <path>] [--output <path>] \
                     [--run-started-at <rfc3339>] [--run-duration-secs <secs>]"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }

    if !criterion_dir.exists() {
        anyhow::bail!(
            "criterion output dir {} does not exist — did `cargo bench` run?",
            criterion_dir.display()
        );
    }

    let host = host_info();
    let commit = commit_info();
    let samples = collect_samples(&criterion_dir)?;
    let envelope = ResultsEnvelope {
        schema_version: SCHEMA_VERSION,
        run_started_at: run_started_at.unwrap_or_else(now_rfc3339),
        run_duration_secs: run_duration_secs.unwrap_or(0.0),
        host: host.clone(),
        commit: commit.clone(),
        samples,
    };

    let output = output.unwrap_or_else(|| default_output_path(&host, &commit));
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&envelope)?;
    fs::write(&output, json).with_context(|| format!("write {}", output.display()))?;
    println!(
        "wrote {} ({} samples)",
        output.display(),
        envelope.samples.len()
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct CriterionEstimates {
    mean: PointEstimate,
    median: PointEstimate,
    std_dev: PointEstimate,
}

#[derive(Debug, Deserialize)]
struct PointEstimate {
    confidence_interval: ConfidenceInterval,
    point_estimate: f64,
    #[allow(dead_code)]
    standard_error: f64,
}

#[derive(Debug, Deserialize)]
struct ConfidenceInterval {
    #[allow(dead_code)]
    confidence_level: f64,
    lower_bound: f64,
    upper_bound: f64,
}

#[derive(Debug, Deserialize)]
struct BenchmarkInfo {
    /// Bench id within the group. Format: `{driver}/{suffix}` (see
    /// `benches/suite.rs::register_w1`).
    function_id: Option<String>,
    /// Group name. Format: `W{n}.v{v}` (see `WorkloadId::full`).
    group_id: String,
}

/// Just the array lengths from `sample.json` — enough to recover
/// criterion's per-bench sample count without parsing every f64.
#[derive(Debug, Deserialize)]
struct SampleInfo {
    times: Vec<f64>,
}

fn collect_samples(criterion_dir: &Path) -> Result<Vec<BenchSample>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(criterion_dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "estimates.json" {
            continue;
        }
        // We only want the `new/estimates.json` files — criterion also
        // keeps `base/` snapshots from prior runs, and we don't want
        // to double-count those.
        let parent = match entry.path().parent() {
            Some(p) => p,
            None => continue,
        };
        if parent.file_name().and_then(|s| s.to_str()) != Some("new") {
            continue;
        }
        // benchmark.json sits next to estimates.json (same `new/`
        // directory). It carries the human-readable group + function
        // ids — much friendlier than parsing the path.
        let info_path = parent.join("benchmark.json");
        if !info_path.exists() {
            continue;
        }
        let info: BenchmarkInfo = serde_json::from_str(
            &fs::read_to_string(&info_path)
                .with_context(|| format!("read {}", info_path.display()))?,
        )
        .with_context(|| format!("parse {}", info_path.display()))?;
        let estimates: CriterionEstimates = serde_json::from_str(
            &fs::read_to_string(entry.path())
                .with_context(|| format!("read {}", entry.path().display()))?,
        )
        .with_context(|| format!("parse {}", entry.path().display()))?;
        let sample_path = parent.join("sample.json");
        let sample_count: u64 = if sample_path.exists() {
            let s: SampleInfo = serde_json::from_str(
                &fs::read_to_string(&sample_path)
                    .with_context(|| format!("read {}", sample_path.display()))?,
            )
            .with_context(|| format!("parse {}", sample_path.display()))?;
            s.times.len() as u64
        } else {
            0
        };

        // Bench id format: `{driver}/{suffix}` — split on the first
        // `/`. If it doesn't have one, treat the whole thing as the
        // driver and leave suffix empty.
        let function_id = info.function_id.clone().unwrap_or_default();
        let driver = function_id
            .split_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or(function_id);

        let median_ns = estimates.median.point_estimate;
        let ops_per_s = if median_ns > 0.0 {
            1e9 / median_ns
        } else {
            0.0
        };
        out.push(BenchSample {
            workload: info.group_id,
            driver,
            median_ns,
            median_ci_lower_ns: estimates.median.confidence_interval.lower_bound,
            median_ci_upper_ns: estimates.median.confidence_interval.upper_bound,
            mean_ns: estimates.mean.point_estimate,
            std_dev_ns: estimates.std_dev.point_estimate,
            samples: sample_count,
            ops_per_s,
        });
    }
    out.sort_by(|a, b| a.workload.cmp(&b.workload).then(a.driver.cmp(&b.driver)));
    Ok(out)
}

fn host_info() -> HostInfo {
    let cpu = read_cpu_brand().unwrap_or_else(|| "unknown".to_string());
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(0);
    let ram_mib = read_ram_mib().unwrap_or(0);
    HostInfo {
        cpu,
        cpus,
        ram_mib,
        os_kind: std::env::consts::OS.to_string(),
        os_release: read_os_release().unwrap_or_else(|| "unknown".to_string()),
        arch: std::env::consts::ARCH.to_string(),
    }
}

fn read_cpu_brand() -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else if cfg!(target_os = "linux") {
        let s = fs::read_to_string("/proc/cpuinfo").ok()?;
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("model name") {
                if let Some(after_colon) = rest.split_once(':') {
                    return Some(after_colon.1.trim().to_string());
                }
            }
        }
        None
    } else {
        None
    }
}

fn read_ram_mib() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        let bytes: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(bytes / (1024 * 1024))
    } else if cfg!(target_os = "linux") {
        let s = fs::read_to_string("/proc/meminfo").ok()?;
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kib / 1024);
            }
        }
        None
    } else {
        None
    }
}

fn read_os_release() -> Option<String> {
    let out = Command::new("uname").arg("-r").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn commit_info() -> CommitInfo {
    let sha = git("rev-parse", &["HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let branch =
        git("rev-parse", &["--abbrev-ref", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = !git("status", &["--porcelain"])
        .unwrap_or_default()
        .trim()
        .is_empty();
    CommitInfo { sha, branch, dirty }
}

fn git(cmd: &str, args: &[&str]) -> Option<String> {
    let mut full = vec![cmd];
    full.extend_from_slice(args);
    let out = Command::new("git").args(&full).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn now_rfc3339() -> String {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    // Manual RFC 3339 (UTC) — avoids a chrono dep for one timestamp.
    let secs = dur.as_secs() as i64;
    let days_since_epoch = secs / 86_400;
    let secs_today = secs % 86_400;
    let (y, mo, d) = ymd_from_days(days_since_epoch);
    let h = secs_today / 3600;
    let mi = (secs_today / 60) % 60;
    let s = secs_today % 60;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Convert "days since 1970-01-01" to (year, month, day). Standard
/// civil-from-days algorithm (Howard Hinnant's date library).
fn ymd_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn default_output_path(host: &HostInfo, commit: &CommitInfo) -> PathBuf {
    // benchmarks/results/YYYY-MM-DD-<host_token>-<short_sha>.json
    let now = now_rfc3339();
    let date = &now[..10];
    let host_token = host
        .cpu
        .split_whitespace()
        .next()
        .unwrap_or("host")
        .to_lowercase();
    let host_token = host_token.replace(|c: char| !c.is_ascii_alphanumeric(), "");
    let short_sha = if commit.sha == "unknown" {
        "unknown".to_string()
    } else {
        commit.sha.chars().take(8).collect()
    };
    PathBuf::from(format!(
        "benchmarks/results/{date}-{host_token}-{short_sha}.json"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ymd_from_days_known_dates() {
        // 1970-01-01
        assert_eq!(ymd_from_days(0), (1970, 1, 1));
        // 2026-05-06 — today's date per the project memory.
        let target = days_from_ymd(2026, 5, 6);
        assert_eq!(ymd_from_days(target), (2026, 5, 6));
    }

    /// Inverse of [`ymd_from_days`] — only used for round-trip in
    /// tests; the production code only goes one way (days → ymd).
    fn days_from_ymd(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }
}
