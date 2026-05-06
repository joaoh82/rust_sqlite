//! Single criterion entry point — fans out (workload × driver) pairs.
//!
//! Adding a workload = one file under `src/workloads/` + one
//! `register_w*` call below. Adding a driver = one file under
//! `src/drivers/` + adding it to the per-workload register call.
//!
//! Each bench:
//! 1. Creates a fresh `TempDir` (per-run isolation — no page-cache /
//!    WAL-state bleed across iterations).
//! 2. Calls the workload's `setup` once **outside** `b.iter` —
//!    setup cost (CREATE TABLE, INSERT 100k rows) shouldn't pollute
//!    the per-iter measurement.
//! 3. Runs the workload's `correctness_check` once. If the engine
//!    returns the wrong shape, we fail fast rather than publish a
//!    "speedup" that's just a bug.
//! 4. Times `bench_iter` via `b.iter` over a pre-shuffled probe slice.
//!
//! W3 is the exception: each criterion sample needs to start from a
//! fresh DB (otherwise samples after the first measure inserts into
//! a table that's already been bulk-loaded), so it uses
//! `iter_batched` with `BatchSize::PerIteration`.

use criterion::measurement::WallTime;
use criterion::{BatchSize, BenchmarkGroup, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::path::{Path, PathBuf};

use sqlrite_benchmarks::Driver;
use sqlrite_benchmarks::drivers::sqlite::SQLiteDriver;
use sqlrite_benchmarks::drivers::sqlrite::SQLRiteDriver;
use sqlrite_benchmarks::workloads::{
    bulk_insert as w3, index_lookup as w6, kv as w1, mixed_oltp as w5, range_scan as w2,
    single_insert as w4,
};

/// Pick a DB filename inside `dir` based on the driver — keeps a
/// half-debugged file recognizable on disk.
fn db_path(dir: &Path, driver_name: &str, stem: &str) -> PathBuf {
    let ext = match driver_name {
        "sqlite" => "sqlite",
        "duckdb" => "duckdb",
        _ => "sqlrite",
    };
    dir.join(format!("{stem}.{ext}"))
}

/// Make a tempdir keyed to a workload + driver pair so a half-finished
/// run leaves a recognizable trail under `$TMPDIR`.
fn tempdir_for(workload: &str, driver_name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("sqlrite-bench-{workload}-{driver_name}-"))
        .tempdir()
        .expect("tempdir")
}

// ---------------------------------------------------------------------------
// W1 — read-by-PK
// ---------------------------------------------------------------------------

fn register_w1<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w1", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w1");
    let (mut conn, keys) = w1::setup(&driver, &path).expect("W1 setup");
    w1::correctness_check(&driver, &mut conn).expect("W1 correctness check");

    let mut group = c.benchmark_group(w1::W1.full());
    let bench_id = format!("{}/default", driver.name());
    let mut idx = 0usize;
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let key = keys[idx % keys.len()];
            idx = idx.wrapping_add(1);
            let row = w1::bench_iter(&driver, &mut conn, key).expect("W1 query");
            black_box(row)
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W2 — range scan (three width buckets: 100 / 1k / 10k)
// ---------------------------------------------------------------------------

fn register_w2<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w2", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w2");
    let (mut conn, dataset) = w2::setup(&driver, &path).expect("W2 setup");
    // Run correctness across all three widths so the gate proves the
    // engine returns the right count for each.
    for &(_, width) in &w2::RANGE_SIZES {
        w2::correctness_check(&driver, &mut conn, width).expect("W2 correctness check");
    }

    for &(label, _width) in &w2::RANGE_SIZES {
        // One criterion group per width — keeps the JSON envelope
        // tidy (each range size becomes its own (workload, driver)
        // sample row in `benchmarks/results/*.json`).
        let group_name = format!("{}/range-{}", w2::W2.full(), label);
        let mut group = c.benchmark_group(&group_name);
        bench_w2_width(&mut group, &driver, &mut conn, label, &dataset);
        group.finish();
    }
    drop(conn);
    drop(tmp);
}

fn bench_w2_width<D: Driver>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    driver: &D,
    conn: &mut D::Conn,
    label: &str,
    dataset: &sqlrite_benchmarks::data::GroupADataset,
) {
    let probes: &[(i64, i64)] = match label {
        "100" => &dataset.range_probes_100,
        "1k" => &dataset.range_probes_1k,
        "10k" => &dataset.range_probes_10k,
        _ => panic!("unknown W2 width label: {label}"),
    };
    let bench_id = format!("{}/default", driver.name());
    let mut idx = 0usize;
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let (lo, hi) = probes[idx % probes.len()];
            idx = idx.wrapping_add(1);
            let count = w2::bench_iter(driver, conn, lo, hi).expect("W2 query");
            black_box(count)
        });
    });
}

// ---------------------------------------------------------------------------
// W3 — bulk insert (100k rows in one transaction)
// ---------------------------------------------------------------------------

fn register_w3<D: Driver + 'static>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w3", driver.name());
    let dataset = w3::dataset();

    // Sanity-check correctness on a one-shot insert against a probe
    // file before the timed loop — same DB lifecycle a real iter
    // sees.
    {
        let probe_path = db_path(tmp.path(), driver.name(), "w3-probe");
        let mut probe_conn = w3::setup_iter(&driver, &probe_path).expect("W3 probe setup");
        w3::bench_iter(&driver, &mut probe_conn, &dataset).expect("W3 probe insert");
        w3::correctness_check(&driver, &mut probe_conn, dataset.rows.len())
            .expect("W3 correctness check");
        drop(probe_conn);
        std::fs::remove_file(&probe_path).ok();
        // Also clean up the WAL sidecar SQLRite leaves behind so
        // criterion doesn't reopen a stale half-checkpointed file
        // when it picks the same path on a subsequent run.
        std::fs::remove_file(probe_path.with_extension(format!(
            "{}-wal",
            probe_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
        )))
        .ok();
    }

    let mut group = c.benchmark_group(w3::W3.full());
    // 100k inserts per sample = expensive; cap the sample count and
    // measurement-time so a single-driver run finishes in a couple
    // of minutes instead of a quarter-hour. Override at the CLI with
    // `cargo bench -- --measurement-time 30 --sample-size 30` to
    // sharpen the estimate.
    group.sample_size(10);
    let bench_id = format!("{}/default", driver.name());
    // Each sample needs its own DB. `iter_batched` runs the setup
    // closure outside timing; the routine closure is what criterion
    // measures.
    let tmp_path = tmp.path().to_path_buf();
    let driver_name = driver.name().to_string();
    group.bench_function(&bench_id, |b| {
        b.iter_batched(
            || {
                // Per-sample fresh DB. Stamp the path with a counter
                // so SQLRite's WAL sidecar from one sample doesn't
                // collide with the next.
                static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let stem = format!("w3-{n}");
                let path = db_path(&tmp_path, &driver_name, &stem);
                let conn = w3::setup_iter(&driver, &path).expect("W3 setup");
                (conn, path)
            },
            |(mut conn, _path)| {
                w3::bench_iter(&driver, &mut conn, &dataset).expect("W3 bulk insert");
                drop(conn);
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W4 — single-row insert
// ---------------------------------------------------------------------------

fn register_w4<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w4", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w4");
    let (mut conn, mut next_id) = w4::setup(&driver, &path).expect("W4 setup");
    w4::correctness_check(&driver, &mut conn).expect("W4 correctness check");

    let mut group = c.benchmark_group(w4::W4.full());
    let bench_id = format!("{}/default", driver.name());
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let id = next_id;
            next_id += 1;
            w4::bench_iter(&driver, &mut conn, id).expect("W4 INSERT");
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W5 — mixed OLTP
// ---------------------------------------------------------------------------

fn register_w5<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w5", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w5");
    let (mut conn, dataset) = w5::setup(&driver, &path).expect("W5 setup");
    w5::correctness_check(&driver, &mut conn, &dataset).expect("W5 correctness check");

    let mut group = c.benchmark_group(w5::W5.full());
    let bench_id = format!("{}/default", driver.name());
    let keys = dataset.pk_probes.clone();
    let mut iter_idx = 0usize;
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            w5::bench_iter(&driver, &mut conn, iter_idx, &keys).expect("W5 op");
            iter_idx = iter_idx.wrapping_add(1);
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W6 — secondary-index lookup
// ---------------------------------------------------------------------------

fn register_w6<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w6", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w6");
    let (mut conn, dataset) = w6::setup(&driver, &path).expect("W6 setup");
    w6::correctness_check(&driver, &mut conn, &dataset).expect("W6 correctness check");

    let mut group = c.benchmark_group(w6::W6.full());
    let bench_id = format!("{}/default", driver.name());
    let probes = dataset.secondary_probes.clone();
    let mut idx = 0usize;
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let secondary = probes[idx % probes.len()];
            idx = idx.wrapping_add(1);
            let row = w6::bench_iter(&driver, &mut conn, secondary).expect("W6 query");
            black_box(row)
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn benches(c: &mut Criterion) {
    register_w1(c, SQLRiteDriver);
    register_w1(c, SQLiteDriver);
    register_w2(c, SQLRiteDriver);
    register_w2(c, SQLiteDriver);
    register_w3(c, SQLRiteDriver);
    register_w3(c, SQLiteDriver);
    register_w4(c, SQLRiteDriver);
    register_w4(c, SQLiteDriver);
    register_w5(c, SQLRiteDriver);
    register_w5(c, SQLiteDriver);
    register_w6(c, SQLRiteDriver);
    register_w6(c, SQLiteDriver);
}

criterion_group!(suite, benches);
criterion_main!(suite);
