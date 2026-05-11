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
use std::sync::Arc;

use sqlrite_benchmarks::Driver;
#[cfg(feature = "duckdb")]
use sqlrite_benchmarks::drivers::duckdb::DuckDBDriver;
use sqlrite_benchmarks::drivers::sqlite::SQLiteDriver;
use sqlrite_benchmarks::drivers::sqlrite::SQLRiteDriver;
use sqlrite_benchmarks::workloads::{
    aggregate as w7, bulk_insert as w3, concurrent_writers as w13, fts as w11, group_by as w8,
    hybrid as w12, index_lookup as w6, join as w9, kv as w1, mixed_oltp as w5, range_scan as w2,
    single_insert as w4, vector as w10,
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
// W7 — SUM aggregate over 1M rows
// ---------------------------------------------------------------------------

fn register_w7<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w7", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w7");
    let (mut conn, dataset) = w7::setup(&driver, &path).expect("W7 setup");
    w7::correctness_check(&driver, &mut conn, &dataset).expect("W7 correctness check");

    let mut group = c.benchmark_group(w7::W7.full());
    // SUM over 1M rows is single-shot heavy work — drop sample size
    // so a smoke run finishes in a couple of minutes per driver.
    group.sample_size(10);
    let bench_id = format!("{}/default", driver.name());
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let n = w7::bench_iter(&driver, &mut conn).expect("W7 SUM");
            black_box(n)
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W8 — GROUP BY at three cardinalities (10 / 1k / 100k groups)
// ---------------------------------------------------------------------------

fn register_w8<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w8", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w8");
    let (mut conn, _dataset) = w8::setup(&driver, &path).expect("W8 setup");
    w8::correctness_check(&driver, &mut conn).expect("W8 correctness check");

    for &(label, bucket, _expected) in &w8::BUCKETS {
        // SQLRite's GROUP BY at 100k-cardinality over 1M rows is
        // pathologically slow today (~245 s per iter on an M-series
        // MBP — see SQLR-19 for the investigation follow-up). That
        // would block a smoke run for ~40 min, so we skip the cell
        // by default. Set `SQLRITE_BENCH_W8_CARD_100K_SQLRITE=1` to
        // force-include it once the blowup is resolved.
        if label == "card-100k"
            && driver.name() == "sqlrite"
            && std::env::var("SQLRITE_BENCH_W8_CARD_100K_SQLRITE")
                .ok()
                .as_deref()
                != Some("1")
        {
            continue;
        }
        let group_name = format!("{}/{}", w8::W8.full(), label);
        let mut group = c.benchmark_group(&group_name);
        group.sample_size(10);
        let bench_id = format!("{}/default", driver.name());
        group.bench_function(&bench_id, |b| {
            b.iter(|| {
                let n = w8::bench_iter(&driver, &mut conn, bucket).expect("W8 GROUP BY");
                black_box(n)
            });
        });
        group.finish();
    }
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W9 — INNER JOIN, customer ↔ order, probe by customer PK
// ---------------------------------------------------------------------------

fn register_w9<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w9", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w9");
    let (mut conn, dataset) = w9::setup(&driver, &path).expect("W9 setup");
    w9::correctness_check(&driver, &mut conn, &dataset).expect("W9 correctness check");

    let mut group = c.benchmark_group(w9::W9.full());
    // SQLRite's nested-loop join + un-indexed inner scan is heavy
    // (~100k rows scanned per probe). Cap the sample size so a
    // smoke run finishes; override at the CLI for a sharper estimate.
    group.sample_size(10);
    let bench_id = format!("{}/default", driver.name());
    let probes = dataset.probes.clone();
    let mut idx = 0usize;
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let cid = probes[idx % probes.len()];
            idx = idx.wrapping_add(1);
            let row = w9::bench_iter(&driver, &mut conn, cid).expect("W9 JOIN");
            black_box(row)
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W10 — vector top-10, brute-force vs HNSW (SQLRite-only — see
//        `vector::driver_supports`)
// ---------------------------------------------------------------------------

fn register_w10<D: Driver>(c: &mut Criterion, driver: D) {
    if !w10::driver_supports(driver.name()) {
        return;
    }
    for &(label, with_hnsw) in &w10::VARIANTS {
        let tmp = tempdir_for(&format!("w10-{label}"), driver.name());
        let path = db_path(tmp.path(), driver.name(), "w10");
        let (mut conn, dataset) = w10::setup(&driver, &path, with_hnsw).expect("W10 setup");
        w10::correctness_check(&driver, &mut conn, &dataset).expect("W10 correctness check");

        let group_name = format!("{}/{}", w10::W10.full(), label);
        let mut group = c.benchmark_group(&group_name);
        // 10k-vector top-10 cosine probes — brute-force per probe is
        // hundreds of ms on SQLRite. Cap samples so the smoke run
        // finishes quickly; override at the CLI for a sharper estimate.
        group.sample_size(10);
        let bench_id = format!("{}/default", driver.name());
        let queries = dataset.queries.clone();
        let mut idx = 0usize;
        group.bench_function(&bench_id, |b| {
            b.iter(|| {
                let q = &queries[idx % queries.len()];
                idx = idx.wrapping_add(1);
                let n = w10::bench_iter(&driver, &mut conn, q).expect("W10 query");
                black_box(n)
            });
        });
        group.finish();
        drop(conn);
        drop(tmp);
    }
}

// ---------------------------------------------------------------------------
// W11 — BM25 top-10 (SQLRite USING fts vs SQLite FTS5 virtual table)
// ---------------------------------------------------------------------------

fn register_w11<D: Driver>(c: &mut Criterion, driver: D) {
    let tmp = tempdir_for("w11", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w11");
    let (mut conn, dataset) = w11::setup(&driver, &path).expect("W11 setup");
    w11::correctness_check(&driver, &mut conn, &dataset).expect("W11 correctness check");

    let mut group = c.benchmark_group(w11::W11.full());
    group.sample_size(10);
    let bench_id = format!("{}/default", driver.name());
    let queries = dataset.queries.clone();
    let mut idx = 0usize;
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let q = &queries[idx % queries.len()];
            idx = idx.wrapping_add(1);
            let n = w11::bench_iter(&driver, &mut conn, q).expect("W11 query");
            black_box(n)
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W12 — hybrid retrieval (SQLRite-only)
// ---------------------------------------------------------------------------

fn register_w12<D: Driver>(c: &mut Criterion, driver: D) {
    if !w12::driver_supports(driver.name()) {
        return;
    }
    let tmp = tempdir_for("w12", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w12");
    let (mut conn, dataset) = w12::setup(&driver, &path).expect("W12 setup");
    w12::correctness_check(&driver, &mut conn, &dataset).expect("W12 correctness check");

    let mut group = c.benchmark_group(w12::W12.full());
    group.sample_size(10);
    let bench_id = format!("{}/default", driver.name());
    let text_queries = dataset.fts.queries.clone();
    let vec_queries = dataset.vec.queries.clone();
    let mut idx = 0usize;
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let tq = &text_queries[idx % text_queries.len()];
            let vq = &vec_queries[idx % vec_queries.len()];
            idx = idx.wrapping_add(1);
            let n = w12::bench_iter(&driver, &mut conn, tq, vq).expect("W12 query");
            black_box(n)
        });
    });
    group.finish();
    drop(conn);
    drop(tmp);
}

// ---------------------------------------------------------------------------
// W13 — concurrent writers, mostly-disjoint rows (Phase 11.11b)
// ---------------------------------------------------------------------------

fn register_w13<D>(c: &mut Criterion, driver: D)
where
    D: Driver + Clone + 'static,
    D::Conn: Send + 'static,
{
    let driver = Arc::new(driver);
    let tmp = tempdir_for("w13", driver.name());
    let path = db_path(tmp.path(), driver.name(), "w13");

    // Correctness gate FIRST — runs a single 4×10 burst against a
    // fresh DB and verifies the sum matches commits.
    w13::correctness_check(Arc::clone(&driver), &path).expect("W13 correctness check");

    // Re-setup so the bench starts from a known preload state.
    // `correctness_check` left the DB with 40 increments in it;
    // that doesn't matter for throughput but we want a clean
    // baseline.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file({
        let mut p = path.as_os_str().to_owned();
        p.push("-wal");
        PathBuf::from(p)
    });
    {
        let _ = w13::setup(&*driver, &path).expect("W13 setup");
    }

    let mut group = c.benchmark_group(w13::W13.full());
    // 4 workers × 50 txs × N samples can run long; criterion's
    // default sample size (100) would push a single driver past
    // 30s of wall clock. Cap at 20 — still plenty of statistical
    // confidence for a throughput comparison; override at the CLI
    // for a sharper estimate.
    group.sample_size(20);

    let bench_id = format!(
        "{}/n={}/m={}",
        driver.name(),
        w13::W13_N_WORKERS,
        w13::W13_TXS_PER_WORKER
    );
    group.bench_function(&bench_id, |b| {
        b.iter(|| {
            let committed = w13::run_concurrent(
                Arc::clone(&driver),
                &path,
                w13::W13_N_WORKERS,
                w13::W13_TXS_PER_WORKER,
            )
            .expect("W13 run");
            black_box(committed)
        });
    });
    group.finish();
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
    register_w7(c, SQLRiteDriver);
    register_w7(c, SQLiteDriver);
    #[cfg(feature = "duckdb")]
    register_w7(c, DuckDBDriver);
    register_w8(c, SQLRiteDriver);
    register_w8(c, SQLiteDriver);
    #[cfg(feature = "duckdb")]
    register_w8(c, DuckDBDriver);
    register_w9(c, SQLRiteDriver);
    register_w9(c, SQLiteDriver);
    #[cfg(feature = "duckdb")]
    register_w9(c, DuckDBDriver);
    register_w10(c, SQLRiteDriver);
    register_w10(c, SQLiteDriver); // skipped via driver_supports
    register_w11(c, SQLRiteDriver);
    register_w11(c, SQLiteDriver);
    register_w12(c, SQLRiteDriver);
    register_w12(c, SQLiteDriver); // skipped via driver_supports
    register_w13(c, SQLRiteDriver);
    register_w13(c, SQLiteDriver);
}

criterion_group!(suite, benches);
criterion_main!(suite);
