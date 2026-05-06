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
//! 4. Times `bench_iter` via `b.iter` over a pre-shuffled key slice.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use sqlrite_benchmarks::Driver;
use sqlrite_benchmarks::drivers::sqlite::SQLiteDriver;
use sqlrite_benchmarks::drivers::sqlrite::SQLRiteDriver;
use sqlrite_benchmarks::workloads::kv as w1;

fn register_w1<D: Driver>(c: &mut Criterion, driver: D, suffix: &str) {
    let tmp = tempfile::Builder::new()
        .prefix(&format!("sqlrite-bench-w1-{suffix}-"))
        .tempdir()
        .expect("tempdir");
    let db_path = tmp.path().join(match driver.name() {
        // Use the engine's native extension so a half-debugged DB is
        // recognizable on disk.
        "sqlite" => "w1.sqlite",
        "duckdb" => "w1.duckdb",
        _ => "w1.sqlrite",
    });
    let (mut conn, keys) = w1::setup(&driver, &db_path).expect("W1 setup");
    w1::correctness_check(&driver, &mut conn).expect("W1 correctness check");

    let mut group = c.benchmark_group(w1::W1.full());
    let bench_id = format!("{}/{}", driver.name(), suffix);
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

    // Keep tempdir alive until all benches finished — drop here.
    drop(conn);
    drop(tmp);
}

fn benches(c: &mut Criterion) {
    register_w1(c, SQLRiteDriver, "default");
    register_w1(c, SQLiteDriver, "default");
}

criterion_group!(suite, benches);
criterion_main!(suite);
