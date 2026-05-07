# SQLRite benchmarks

Benchmark suite for SQLRite vs SQLite (and friends). Tracks task **SQLR-4** / **SQLR-16**.

> **Status (2026-05-07):** all six sub-phases (9.1–9.6) shipped — harness + 12 workloads + DuckDB driver + canonical [`docs/benchmarks.md`](../docs/benchmarks.md) reference + first official pinned-host run committed under [`results/`](results/). See [`docs/benchmarks-plan.md`](../docs/benchmarks-plan.md) for the design rationale + the resolved Q1–Q8 decisions.

## Quick start

```sh
# From repo root.
make bench                  # SQLRite + SQLite (lean, ~4 min)
make bench-duckdb           # adds DuckDB driver (Group B only — W7/W8/W9)
```

Each invocation runs the criterion suite, then aggregates the per-bench JSON criterion writes under `target/criterion/` into a single envelope under [`results/`](results/), keyed by date + host fingerprint + commit short-SHA. The shape is locked in [`src/envelope.rs`](src/envelope.rs).

## Why this exists

Three drivers, in plan order:

1. **Decision support for the engine roadmap.** Concrete numbers for "is the bottom-up B-tree rebuild the bottleneck or is the executor's row reassembly?", informing whether LSM/SSTable engine work or executor refactors come first.
2. **Differentiator validation.** Phase 7d's HNSW, Phase 8's BM25, and Phase 8d's hybrid retrieval shipped without comparable numbers. W10–W12 publish absolute latencies for the "SQLRite for RAG" pitch.
3. **Regression detection.** JSON output is shaped so a future per-PR regression detector can mechanically diff `main` vs branch (parked as a post-9.6 follow-up — needs ~3 baseline runs first).

## Workloads

Three groups (full descriptions in [`docs/benchmarks-plan.md`](../docs/benchmarks-plan.md#workloads)):

- **Group A — OLTP baseline.** W1 read-by-PK, W2 range scan, W3 bulk insert, W4 single-row insert, W5 mixed OLTP, W6 index lookup. ✅ shipped (9.1 + 9.2).
- **Group B — SQL-feature scaling.** W7 aggregate, W8 GROUP BY, W9 INNER JOIN. ✅ shipped (9.3).
- **Group C — Differentiators.** W10 vector top-10, W11 BM25 top-10, W12 hybrid retrieval. ✅ shipped (9.4).

For headline numbers + reading-the-numbers methodology + the engineering debts the suite surfaced, see the canonical [**`docs/benchmarks.md`**](../docs/benchmarks.md).

Workloads are versioned (`W1.v1`, `W1.v2`, …) per Q8 — bumping the version is the explicit "I changed the benchmark" gesture.

## Drivers

| Engine | Crate | Profile | Status |
|---|---|---|---|
| SQLRite | `sqlrite-engine` (path dep, `default-features = false` + `file-locks`) | Default | ✅ 9.1 |
| SQLite | `rusqlite` v0.36 (bundled libsqlite3) | `WAL` + `synchronous=NORMAL` + 64 MB cache + `temp_store=MEMORY` (Q3 — tuned headline) | ✅ 9.1 |
| DuckDB | `duckdb-rs` v1.4 (bundled libduckdb) | Defaults — DuckDB's MVCC + commit semantics are uniform; no equivalent to SQLite's WAL+NORMAL opt-in | ✅ 9.5, opt-in via `--features duckdb` (Group B only) |
| libSQL | — | — | 🟡 deferred (post-9.6) |

The SQLite driver runs the **tuned profile** as the headline (Q3). A SQLite-default column is opt-in, post-9.6.

## Results

Each `make bench` invocation drops a JSON file under `results/`. The `.gitignore` keeps casual local runs out of git; the first "official" pinned-host run gets committed in sub-phase 9.6.

The table below is updated as new workloads land. Numbers are placeholders until 9.6 commits an official pinned-host run; ratios shown here are illustrative output from the sub-phase 9.1 development run on the project owner's M-series MBP.

> All numbers below come from a **smoke run** (criterion `--warm-up-time 1 --measurement-time 1-2 --sample-size 10`) on the project owner's M1 Pro / 32 GiB / macOS 23.5.0 host on `bench-9.2-group-a`. They're directionally honest but the official pinned-host publication is sub-phase 9.6's job. Open `target/criterion/report/index.html` after a local `make bench` for the full criterion HTML.

### W1 — read-by-PK (`SELECT name, payload FROM kv WHERE id = ?`)

100k-row table, 10k random keys, in-process, prepared statement on the SQLite side / per-call parse on the SQLRite side (no parameter binding yet — see [`src/drivers/sqlrite.rs`](src/drivers/sqlrite.rs)).

| Engine | Median latency | Throughput (ops/s) | Notes |
|---|---|---|---|
| SQLRite | ~19 µs | ~53k | Per-iter parse + plan; no prepared params yet |
| SQLite (rusqlite, WAL+NORMAL) | ~2.4 µs | ~423k | `prepare_cached` + bound `?` params |

**Read this as:** ~8× slower on the hottest-path SELECT-by-PK. Most of the gap is per-iteration parsing — SQLRite has no prepared-plan cache, so every iteration walks the `sqlparser` AST again. A future "prepared statement support" follow-up will tighten this.

### W2 — range scan (`WHERE secondary >= ? AND secondary <= ?`)

100k-row table with a unique-index on `secondary` (a permutation of `1..=100_000`). Three width buckets, all measuring how the scan time scales with the number of matching rows.

> SQLRite's tiny optimizer only probes the index on `<col> = <literal>` shape ([`docs/supported-sql.md:210`](../docs/supported-sql.md)) — range predicates fall back to full table scan, which is why all three width buckets land at ~the same SQLRite latency. SQLite uses the index for range scans.

| Width | SQLRite | SQLite | Ratio |
|---|---|---|---|
| 100 rows | ~36.6 ms | ~60 µs | ~610× |
| 1,000 rows | ~32.2 ms | ~594 µs | ~54× |
| 10,000 rows | ~33.8 ms | ~6.5 ms | ~5× |

**Read this as:** the ratio collapses as the matching set grows — SQLite's index range scan does its work, SQLRite's full-table scan does its (constant) work, and at 10k matching rows of a 100k table they converge. A future range-scan optimizer is the roadmap unlock here; the 100-row bucket gap is its yardstick.

### W3 — bulk insert (100k rows in one transaction)

`BEGIN; INSERT…×100k; COMMIT`. Per criterion sample is one full transaction; each sample starts from a fresh DB (`iter_batched(BatchSize::PerIteration)`). Only 10 samples requested per driver because each sample is expensive — re-run with `cargo bench -- --sample-size 30 --measurement-time 30 W3` to sharpen the estimate.

| Engine | Median per 100k-row txn | Rows/s |
|---|---|---|
| SQLRite | ~1.35 s | ~74k |
| SQLite (rusqlite, WAL+NORMAL) | ~206 ms | ~485k |
| **Ratio** | **~6.6×** | |

**Read this as:** in-transaction bulk paths are tractably close. The gap is mostly per-row parse + execute (no prepared cache on SQLRite), with one COMMIT amortized across all 100k rows. Same root cause as W1's per-iter parse, scaled up.

### W4 — single-row insert (each in its own implicit transaction)

`INSERT INTO kv_writes …` — auto-committed per row. Preloaded with 1,000 rows so the table size is stable across iterations (otherwise the bottom-up rebuild's O(N) commit cost would ramp through the bench window). See [`src/workloads/single_insert.rs`](src/workloads/single_insert.rs) for the preload rationale.

| Engine | Median latency | Throughput (ops/s) |
|---|---|---|
| SQLRite | ~7.34 ms | ~136 |
| SQLite (rusqlite, WAL+NORMAL) | ~10.9 µs | ~91k |
| **Ratio** | **~673×** ⚠️ | |

**Read this as:** every COMMIT triggers a full bottom-up B-tree rebuild on SQLRite. Even at a tiny 1k-row preload, the rebuild + WAL append + checkpoint per row dominates. **Investigation follow-up filed (SQLR-18)** per the plan's "if W4 shows >100× gap" heuristic — informational, not a release gate. The fix is a phase-level change (in-place B-tree splits, or LSM/SSTable swap), not a bench-suite tweak.

### W5 — mixed OLTP (50/50 SELECT-by-PK + UPDATE-by-PK)

YCSB-A flavor over the Group-A 100k-row table. Even iterations are SELECTs, odd iterations are UPDATEs. SELECTs are fast on both engines; UPDATEs hit the same per-row commit path as W4 — but on a 100k-row preload, so the rebuild cost is much larger.

| Engine | Median per mixed op | Throughput (ops/s) |
|---|---|---|
| SQLRite | ~71.1 ms | ~14 |
| SQLite (rusqlite, WAL+NORMAL) | ~12.3 µs | ~81k |
| **Ratio** | **~5,800×** | |

**Read this as:** same root cause as W4, amplified by 100× the preload (1k → 100k). The median sits in the bimodal "every other op is an O(100k) rebuild" zone. Tracked together with W4 under SQLR-18.

### W6 — secondary-index lookup (`WHERE secondary = ?`)

Unique-index probes — every probe matches exactly one row. SQLRite's optimizer fast-paths `<indexed_col> = <literal>`, so this exercises the same code path on both engines: B-tree probe + ROWID indirection + row reassembly.

| Engine | Median latency | Throughput (ops/s) |
|---|---|---|
| SQLRite | ~14.6 µs | ~68k |
| SQLite (rusqlite, WAL+NORMAL) | ~3.1 µs | ~323k |
| **Ratio** | **~5×** | |

**Read this as:** secondary-index path is healthy — same ~5–8× per-iter-parse-cost band as W1. The index probe itself is fine; what we're measuring is mostly `sqlparser` per call.

### W7 — `SELECT SUM(v) FROM big` (1M-row full-scan aggregate)

Single-statement aggregate. No WHERE, no GROUP BY. All three engines walk every row through their executor; the question is how cheap the per-row "touch" is — and how much an OLAP-shaped engine wins on a workload built for it.

| Engine | Median per SUM | Throughput (rows/s) |
|---|---|---|
| SQLRite | ~111 ms | ~9.0M |
| SQLite (rusqlite, WAL+NORMAL) | ~31 ms | ~32M |
| DuckDB (default) | ~378 µs | **~2.6B** |

**Read this as:** vectorized columnar wins big on a workload built for it. DuckDB is **~80× faster than SQLite** on the same SUM-over-1M, and ~290× faster than SQLRite. SQLite-vs-SQLRite is the closest gap (~3.5×) — solid for our row-store. The DuckDB column is the "what does a different storage model look like?" sister number from the plan.

### W8 — `SELECT k, COUNT(*) FROM big GROUP BY k` (three cardinalities)

1M-row table, GROUP BY at 10 / 1k / 100k distinct keys.

| Cardinality | SQLRite | SQLite (WAL+NORMAL) | DuckDB | Notes |
|---|---|---|---|---|
| 10 groups | ~204 ms | ~460 ms | ~634 µs | DuckDB **~720×** faster than SQLite |
| 1,000 groups | ~1.50 s | ~242 ms | ~701 µs | DuckDB **~345×** faster than SQLite |
| 100,000 groups | **skipped** | ~241 ms | ~20.4 ms | SQLR-19 (SQLRite); DuckDB still ~12× faster than SQLite |

**SQLRite × 100k-cardinality is skipped by default** in `make bench`. A first measurement clocked ~245 s/iter (~41 min for 10 samples), strongly suggesting an O(n × cardinality) path inside the GROUP BY executor (a hash aggregator should be O(n + groups)). **SQLR-19** tracks the investigation. Set `SQLRITE_BENCH_W8_CARD_100K_SQLRITE=1` once that lands to re-include the bucket.

**Read this as:** GROUP BY at low cardinality is fine on SQLRite; high-cardinality work is broken. DuckDB's vectorized hash aggregator dominates SQLite at every cardinality — that's the analytical-engine value proposition in one workload.

### W9 — INNER JOIN, customer ↔ order, probe by customer PK

Plan target was two **100k-row tables**. v1 ships at **10k rows** because SQLRite's join executor doesn't push the ON predicate down to an index probe on the inner side — at the 100k scale, per-iter cost was >5 minutes (88 minutes of measured runtime didn't produce a single sample before the smoke run was killed). **SQLR-20** tracks the join-planner / inner-side index-probe fix. Until then, v1 = 10k rows; bumping to 100k follows the fix + a `W9.v2` tag.

Even at 10k scale, the gap is large:

| Engine | Median per probe | Throughput (probes/s) |
|---|---|---|
| SQLRite | ~32 s | ~0.03 |
| SQLite (rusqlite, WAL+NORMAL) | ~2.3 µs | ~459k |
| **Ratio** | **~14,000,000×** ⚠️ | |

**Read this as:** SQLRite's join executor scans the entire inner table per outer row (no index probe pushdown), and the per-pair overhead is much higher than a single-table full scan. At 32 s for ~10k inner-row checks, the per-row cost is ~3 ms — about **3,000× slower** than W2's full-scan rate (1 µs/row). The inner-side index probe is the obvious next unlock; the per-pair overhead is the second.

**DuckDB on W9 (10k×10k):** ~500 µs / probe — about **220× slower than SQLite**. Per-PK probe + single-row JOIN is the workload SQLite was built for; DuckDB pays a much higher per-query overhead because it's optimized for analytical scans, not sub-millisecond OLTP probes. The plan's viability section flags exactly this — DuckDB is "apples-to-oranges" on PK-probe-by-rowid workloads and we include it here for the directional comparison only.

### W10 — vector top-10 (cosine), brute-force vs HNSW

10k 384-dim vectors. Two variants per the plan: brute-force (no index) and HNSW (`CREATE INDEX … USING hnsw`). SQLRite-only — `sqlite-vec` extension wiring is a follow-up (`rusqlite[bundled]` doesn't ship it; loading a pre-compiled `.dylib` at runtime is non-trivial and was out of scope for v1).

| Variant | SQLRite median | Throughput |
|---|---|---|
| brute-force | ~122 ms | ~8 ops/s |
| hnsw | ~132 ms | ~7 ops/s |

**Read this as:** at 10k vectors × 384 dim, **HNSW barely beats brute-force**. That's not the index's fault — both numbers are dominated by the **per-iter SQL parse cost** (the 384-element bracket-array literal in the `ORDER BY` clause is ~4 KB of SQL the parser walks every iteration; the actual cosine work is ~3.8M FP ops ≈ a few ms). At a much larger corpus (millions of vectors) HNSW would dominate; at 10k the parser cost masks the algorithmic win. A future "prepare-vector-query-once" path or VECTOR-bind binding would surface the real HNSW vs brute-force gap.

### W11 — BM25 top-10

**1000 synthetic docs.** Plan target was 10k docs; SQLRite's FTS doc-lengths sidecar must fit in one 4 KiB page, capping the corpus at ~1,360 docs until Phase 8.1 ships overflow chaining. **SQLR-21** tracks that. Engine-asymmetric setup:

- **SQLRite**: `CREATE TABLE docs (id, body) + CREATE INDEX … USING fts (body)` + `SELECT id FROM docs WHERE fts_match(body, ?) ORDER BY bm25_score(body, ?) DESC LIMIT 10`.
- **SQLite**: `CREATE VIRTUAL TABLE docs USING fts5(body)` + `SELECT rowid FROM docs WHERE docs MATCH ? ORDER BY rank LIMIT 10`. Query terms are joined with explicit `OR` to match SQLRite's any-of semantics (FTS5's default operator is `AND`).

Both engines use an inverted index + BM25 ranker; the SQL shapes differ because FTS5 is a virtual table while SQLRite attaches the index to a regular table. See [`benchmarks/src/workloads/fts.rs`](src/workloads/fts.rs) for the per-driver branch.

| Engine | Median per query | Throughput (ops/s) |
|---|---|---|
| SQLRite | ~533 µs | ~1,876 |
| SQLite (FTS5) | ~24 µs | ~42,000 |
| **Ratio** | **~22×** | |

**Read this as:** healthy. ~22× is a much smaller gap than W4 / W5 / W9 — SQLRite's BM25 path is in the same band as the W1 / W6 per-iter-parse-dominated workloads. The dominant cost is again the per-call SQL parsing of the BM25 query; once SQLRite gains prepared-statement support, this should close further.

### W12 — hybrid retrieval (BM25 + cosine fusion)

**1000 docs with both a text body and a 384-dim embedding.** Hot loop: `WHERE fts_match(body, ?) ORDER BY 0.5 * (1 − bm25_score/10) + 0.5 * vec_distance_cosine(embedding, [...]) ASC LIMIT 10`. Mirrors [`examples/hybrid-retrieval/`](../examples/hybrid-retrieval/).

**SQLRite-only.** No off-the-shelf comparator exists in a single embedded engine that can compose BM25 + vector cosine in one query — that's the plan's stated stance. The number stands on its own as the baseline for the "SQLRite for RAG" pitch. (Same 1000-doc cap as W11 per SQLR-21.)

| Engine | Median per query | Throughput (ops/s) |
|---|---|---|
| SQLRite | ~654 µs | ~1,529 |

**Read this as:** absolute number for the headline RAG pitch. ~650 µs/query for "filter by FTS, rank by 50/50 BM25 + cosine over 384-dim embeddings" on a 1000-doc corpus is solid — competitive with what a Python+sklearn user would pay round-tripping to a separate vector DB + BM25 engine. The number scales with corpus size; bumping the cap (post Phase 8.1) is what unlocks larger-scale headlines.

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
│   │   └── duckdb.rs      — feature-gated; Group B only
│   ├── workloads/
│   │   ├── mod.rs
│   │   ├── kv.rs            — W1 read-by-PK
│   │   ├── range_scan.rs    — W2 range scan (100/1k/10k)
│   │   ├── bulk_insert.rs   — W3 bulk insert (100k/txn)
│   │   ├── single_insert.rs — W4 single-row insert
│   │   ├── mixed_oltp.rs    — W5 50/50 SELECT+UPDATE
│   │   ├── index_lookup.rs  — W6 secondary-index lookup
│   │   ├── aggregate.rs     — W7 SUM(v) over 1M rows
│   │   ├── group_by.rs      — W8 GROUP BY (10/1k/100k cardinalities)
│   │   ├── join.rs          — W9 INNER JOIN, customer ↔ order
│   │   ├── vector.rs        — W10 vector top-10 (brute-force + HNSW)
│   │   ├── fts.rs           — W11 BM25 top-10 (vs SQLite FTS5)
│   │   └── hybrid.rs        — W12 hybrid BM25 + cosine fusion
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
