# SQLRite benchmarks

Benchmark suite for SQLRite vs SQLite (and friends). Tracks task **SQLR-4** / **SQLR-16**.

> **Status (2026-05-06):** sub-phase 9.1 landed — harness + W1. W2–W12 land in 9.2–9.4. The first official pinned-host JSON gets committed in 9.6. See [`docs/benchmarks-plan.md`](../docs/benchmarks-plan.md) for the canonical design + the resolved Q1–Q8 decisions.

## Quick start

```sh
# From repo root.
make bench                  # SQLRite + SQLite (lean, ~4 min)
# make bench-duckdb         # adds DuckDB driver (Group B only) — lands in 9.5
```

Each invocation runs the criterion suite, then aggregates the per-bench JSON criterion writes under `target/criterion/` into a single envelope under [`results/`](results/), keyed by date + host fingerprint + commit short-SHA. The shape is locked in [`src/envelope.rs`](src/envelope.rs).

## Why this exists

Three drivers, in plan order:

1. **Decision support for the engine roadmap.** Concrete numbers for "is the bottom-up B-tree rebuild the bottleneck or is the executor's row reassembly?", informing whether LSM/SSTable engine work or executor refactors come first.
2. **Differentiator validation.** Phase 7d's HNSW, Phase 8's BM25, and Phase 8d's hybrid retrieval shipped without comparable numbers. W10–W12 publish absolute latencies for the "SQLRite for RAG" pitch.
3. **Regression detection.** JSON output is shaped so a future per-PR regression detector can mechanically diff `main` vs branch (parked as a post-9.6 follow-up — needs ~3 baseline runs first).

## Workloads

Three groups (full descriptions in [`docs/benchmarks-plan.md`](../docs/benchmarks-plan.md#workloads)):

- **Group A — OLTP baseline.** W1 read-by-PK, W2 range scan, W3 bulk insert, W4 single-row insert, W5 mixed OLTP, W6 index lookup. Lands in 9.1 (W1) + 9.2.
- **Group B — SQL-feature scaling.** W7 aggregate, W8 GROUP BY, W9 INNER JOIN. Lands in 9.3.
- **Group C — Differentiators.** W10 vector top-10, W11 BM25 top-10, W12 hybrid retrieval. Lands in 9.4.

Workloads are versioned (`W1.v1`, `W1.v2`, …) per Q8 — bumping the version is the explicit "I changed the benchmark" gesture.

## Drivers

| Engine | Crate | Profile | Status |
|---|---|---|---|
| SQLRite | `sqlrite-engine` (path dep, `default-features = false` + `file-locks`) | Default | ✅ 9.1 |
| SQLite | `rusqlite` v0.36 (bundled libsqlite3) | `WAL` + `synchronous=NORMAL` + 64 MB cache + `temp_store=MEMORY` (Q3 — tuned headline) | ✅ 9.1 |
| DuckDB | `duckdb-rs` | Defaults | 🟡 9.5, opt-in via `--features duckdb` |
| libSQL | — | — | 🟡 deferred (post-9.6) |

The SQLite driver runs the **tuned profile** as the headline (Q3). A SQLite-default column is opt-in, post-9.6.

## Results

Each `make bench` invocation drops a JSON file under `results/`. The `.gitignore` keeps casual local runs out of git; the first "official" pinned-host run gets committed in sub-phase 9.6.

The table below is updated as new workloads land. Numbers are placeholders until 9.6 commits an official pinned-host run; ratios shown here are illustrative output from the sub-phase 9.1 development run on the project owner's M-series MBP.

### W1 — read-by-PK (`SELECT name, payload FROM kv WHERE id = ?`)

100k-row table, 10k random keys, in-process, prepared statement on the SQLite side / per-call parse on the SQLRite side (no parameter binding yet — see [`src/drivers/sqlrite.rs`](src/drivers/sqlrite.rs)).

| Engine | Median latency | Throughput (ops/s) | Notes |
|---|---|---|---|
| SQLRite (this branch) | ~12 µs | ~84k | Per-iter parse + plan; no prepared params yet |
| SQLite (rusqlite, WAL+NORMAL) | ~2.5 µs | ~395k | `prepare_cached` + bound `?` params |

**Read this as:** SQLRite is ~5× slower than SQLite on the hottest-path SELECT-by-PK on a sub-µs-per-op workload. Most of that gap is per-iteration parsing — SQLRite has no prepared-plan cache, so every iteration walks the `sqlparser` AST again. A future "prepared statement support" follow-up will tighten this; the W1 number is the baseline that work needs to move.

> Replace these numbers with the official pinned-host run that lands in 9.6. The sample command above also writes raw criterion HTML reports to `target/criterion/` for the curious — open `target/criterion/report/index.html`.

## Adding a workload

Per the [`docs/benchmarks-plan.md`](../docs/benchmarks-plan.md#sub-phases) sequencing:

1. New file `src/workloads/<name>.rs` — `pub const W*: WorkloadId`, `setup`, `bench_iter`, `correctness_check`.
2. Register in `src/workloads/mod.rs` (`pub mod <name>;`).
3. One `register_*` function in `benches/suite.rs` that builds the (workload × driver) bench groups.
4. Add a row to the table above with the new measured numbers.

If the workload's shape changes between releases (added column, different query plan target), bump the `version` in the `WorkloadId`. Old JSON results stay readable; the comparison script (`scripts/compare.py`, lands in 9.6) only diffs same-version pairs.

## Adding a driver

1. New file `src/drivers/<name>.rs` implementing the `Driver` trait.
2. Add to `src/drivers/mod.rs`.
3. Register on each workload in `benches/suite.rs`.

Driver-bias is a known risk (see `docs/benchmarks-plan.md` "Risks + things to watch") — review every implementation against the question "is this how a perf-conscious user of `<engine>` would write it?"

## Layout

```
benchmarks/
├── Cargo.toml             — workspace member, not published, excluded from CI
├── README.md              — this file
├── src/
│   ├── lib.rs             — Driver trait, Value type, BenchSample
│   ├── envelope.rs        — JSON output schema (locked in 9.1)
│   ├── data.rs            — deterministic dataset generators (seeded ChaCha8)
│   ├── drivers/
│   │   ├── mod.rs
│   │   ├── sqlrite.rs     — engine-side driver (inlines params)
│   │   ├── sqlite.rs      — rusqlite + Q3 tuned profile
│   │   └── duckdb.rs      — feature-gated; lands in 9.5
│   ├── workloads/
│   │   ├── mod.rs
│   │   └── kv.rs          — W1 read-by-PK (this PR)
│   └── bin/
│       └── aggregate.rs   — walks target/criterion/ → results/*.json
├── benches/
│   └── suite.rs           — single criterion entry point
├── scripts/
│   └── run.sh             — make bench → cargo bench + aggregator
└── results/
    ├── .gitkeep
    └── .gitignore         — keeps local runs out of git until 9.6
```

## See also

- [`docs/benchmarks-plan.md`](../docs/benchmarks-plan.md) — canonical design + decisions.
- [`docs/architecture.md`](../docs/architecture.md) — engine layer map; benchmarks bind to the public `Connection` surface only.
- [`docs/roadmap.md`](../docs/roadmap.md) — broader project roadmap; SQLR-4 lives under "Possible extras."
