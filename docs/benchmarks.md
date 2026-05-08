# Benchmarks

SQLRite ships with a curated benchmark suite that pits the engine against `SQLite` (the reference) and optionally `DuckDB` (analytical-slice comparator) on a fixed set of OLTP, SQL-feature-scaling, and AI-era workloads. Numbers are produced on demand with `make bench`; raw JSON envelopes get committed under [`benchmarks/results/`](../benchmarks/results/) so trends can be diffed mechanically.

The point isn't to "win" — SQLite has 25+ years of optimization behind it, and the ratios shipped here range from healthy single-digit gaps to known-broken multi-million-x outliers that surfaced honest engine TODOs (SQLR-18 / SQLR-19 / SQLR-20 / SQLR-21). The point is to (a) baseline so future engine work has a number to move, (b) prove the differentiator workloads (HNSW, BM25, hybrid retrieval) actually deliver, and (c) ground roadmap conversations about LSM / columnar / JIT directions with evidence rather than vibes.

For the design rationale (resolved Q1–Q8) see [`docs/benchmarks-plan.md`](benchmarks-plan.md). For the workload-by-workload narrative numbers see [`benchmarks/README.md`](../benchmarks/README.md).

---

## Table of contents

- [Quick start](#quick-start)
- [What the suite measures](#what-the-suite-measures)
- [Reading the numbers](#reading-the-numbers)
- [Headline numbers](#headline-numbers)
- [Engineering debts surfaced](#engineering-debts-surfaced)
- [Reproducing a run](#reproducing-a-run)
- [Comparing two runs](#comparing-two-runs)
- [What's NOT measured (and why)](#whats-not-measured-and-why)
- [See also](#see-also)

---

## Quick start

```sh
# Lean profile — SQLRite + SQLite (rusqlite-bundled, WAL+NORMAL).
# ~5 min on an M-series MBP.
make bench

# Full profile — adds the optional DuckDB driver on Group B (W7/W8/W9).
# ~30 min on the same host (DuckDB's per-row INSERT path is heavy on
# the 1M-row Group B setup).
make bench-duckdb
```

Both commands run `cargo bench -p sqlrite-benchmarks` and then aggregate criterion's per-bench JSON into a single envelope under [`benchmarks/results/`](../benchmarks/results/). Open `target/criterion/report/index.html` afterwards for the full criterion HTML report (per-bench p50/p95/p99 distributions, regression plots, etc.).

---

## What the suite measures

Twelve workloads, three groups. Each one has a fixed input dataset (deterministic seed), a correctness gate that runs once before timing starts, and a fixed criterion configuration. Workloads are versioned (`W1.v1`, `W1.v2`, …) so a future shape change is an explicit, traceable bump (Q8).

### Group A — OLTP baseline

| ID | Name | Shape |
|----|------|-------|
| W1 | Read-by-PK | 100k-row table, 10k random `WHERE id = ?` probes |
| W2 | Range scan | `WHERE indexed_col >= ? AND <= ?`, three width buckets (100 / 1k / 10k rows) |
| W3 | Bulk insert | 100k rows in one transaction |
| W4 | Single-row insert | 1k auto-committed INSERTs against a 1k-row preloaded table |
| W5 | Mixed OLTP | 50/50 SELECT-by-PK + UPDATE-by-PK on a 100k-row table |
| W6 | Index lookup | 10k probes by `secondary = ?` on a unique non-PK index |

### Group B — SQL-feature scaling

| ID | Name | Shape |
|----|------|-------|
| W7 | Aggregate | `SELECT SUM(v) FROM big`, 1M rows |
| W8 | GROUP BY | `SELECT k, COUNT(*) FROM big GROUP BY k`, three cardinalities (10 / 1k / 100k) |
| W9 | INNER JOIN | per-PK probe on `customers ⨝ orders`, 10k×10k tables (plan target was 100k×100k — see [SQLR-20](#engineering-debts-surfaced)) |

### Group C — Differentiators

| ID | Name | Shape |
|----|------|-------|
| W10 | Vector top-10 | 10k 384-dim vectors, cosine top-10. Brute-force + HNSW variants. |
| W11 | BM25 top-10 | 1k-doc corpus, top-10 BM25. SQLite FTS5 as comparator. |
| W12 | Hybrid retrieval | 50/50 BM25 + cosine fusion on a 1k-doc corpus. SQLRite-only. |

W11 / W12 cap at 1k docs because of an engine constraint ([SQLR-21](#engineering-debts-surfaced)); plan target was 10k.

---

## Reading the numbers

A few methodology notes that change how you read the table.

**Q3 — SQLite is run with the tuned profile.** `journal_mode=WAL`, `synchronous=NORMAL`, `temp_store=MEMORY`, `cache_size=-65536` (64 MB). Rationale: SQLRite's WAL is mandatory + always-on, so SQLite-default (`journal_mode=DELETE`, `synchronous=FULL`, on-commit fsync) is *not* apples-to-apples. WAL+NORMAL matches SQLRite's commit fsync semantics. A SQLite-default secondary column is a future opt-in.

**Q3 — DuckDB runs at defaults.** No equivalent to SQLite's WAL+NORMAL knob; DuckDB's MVCC + commit semantics are uniform.

**Parser tax — historical, addressed in SQLR-23.** Pre-SQLR-23, the engine parsed SQL on every `Connection::execute` / `Connection::prepare` call. The bench driver's SQLRite path inlined `?` placeholders into the SQL string, so every iteration also walked the full `sqlparser` AST. Several workloads' headline numbers were dominated by this overhead — W1 and W6 (sub-µs paths where parser cost was most of the per-iter time), W10 (the 384-dim bracket-array literal in `ORDER BY` was ~4 KB of SQL the parser walked every iteration; brute-force vs HNSW looked indistinguishable as a result).

[SQLR-23](https://github.com/joaoh82/rust_sqlite/pulls?q=SQLR-23) shipped:

- `Connection::prepare_cached` — small per-connection LRU of parsed plans (default cap 16, matches rusqlite).
- `Statement::query_with_params(&[Value])` / `Statement::execute_with_params(&[Value])` — bind `?` placeholders at execute time without re-running sqlparser.
- `Value::Vector(Vec<f32>)` as a first-class bind type — the 4 KB query vector for W10 is now bound directly instead of being re-lexed every iteration. The HNSW probe optimizer still recognizes the bound shape, so the algorithmic shortcut keeps firing.

The bench harness `Driver::query_one` / `query_all` paths route through `prepare_cached` + the bound API. Every workload's `WorkloadId.version` was bumped `v1 → v2` in lockstep — old JSON envelopes keep the v1 tag and stay readable, but cross-version comparisons require an explicit acknowledgment in the comparison script. The next official pinned-host run will land the post-binding numbers; treat the v1 row above as "before" and watch this section for the "after" once republished.

**Where DuckDB is misleading.** Per-PK-probe single-row OLTP queries (W9) are SQLite's home turf, not DuckDB's. The plan flags this as "apples-to-oranges"; we still publish the number because the directional comparison is informative.

**Workload-shape capacity caps.** Two workloads ship at smaller-than-plan dataset sizes because of engine constraints, both tracked as separate follow-ups (SQLR-20, SQLR-21). The deviations are documented inline in [`benchmarks/src/workloads/`](../benchmarks/src/workloads/).

---

## Headline numbers

Median latency from the first official pinned-host run — [`benchmarks/results/2026-05-07-apple-9ffd55a5.json`](../benchmarks/results/2026-05-07-apple-9ffd55a5.json), Apple M1 Pro / macOS 23.5.0, criterion defaults (3 s warm-up, 5 s measurement, 100 samples on light workloads / 10 samples on heavy ones — see the JSON envelope's per-sample `samples` field). Only medians here; the JSON carries 95 % CIs, mean, std-dev, ops/s.

| Workload | SQLRite | SQLite (WAL+NORMAL) | DuckDB | Notes |
|---|---|---|---|---|
| **W1** read-by-PK | 9.87 µs | 2.05 µs | — | ~5× — parser tax |
| **W2** range-100 | 23.99 ms | 60.50 µs | — | ~400× — full-scan vs index range probe |
| **W2** range-1k | 24.92 ms | 585.21 µs | — | ~43× |
| **W2** range-10k | 30.15 ms | 6.24 ms | — | ~5× — converges as scan dominates |
| **W3** bulk insert (100k/txn) | 1.029 s | 166.43 ms | — | ~6.2× |
| **W4** single-row insert | 6.76 ms | 9.78 µs | — | **~691× ⚠️** SQLR-18 |
| **W5** mixed OLTP | 55.63 ms | 9.96 µs | — | **~5,580× ⚠️** SQLR-18 |
| **W6** index lookup | 10.45 µs | 2.50 µs | — | ~4× — parser tax |
| **W7** SUM (1M rows) | 109.47 ms | 31.14 ms | 468.74 µs | DuckDB ~66× faster than SQLite |
| **W8** GROUP BY card-10 | 201.80 ms | 438.09 ms | 761.40 µs | DuckDB ~575× faster than SQLite |
| **W8** GROUP BY card-1k | 1.372 s | 251.13 ms | 871.80 µs | DuckDB ~288× faster than SQLite |
| **W8** GROUP BY card-100k | _skipped_ | 238.96 ms | 19.58 ms | **SQLRite skipped ⚠️** SQLR-19; DuckDB ~12× faster than SQLite |
| **W9** INNER JOIN (10k×10k) | 34.25 s | 2.23 µs | 699.23 µs | **~15M× ⚠️** SQLR-20; DuckDB ~313× slower than SQLite (analytical-engine OLTP weakness) |
| **W10** vector top-10 (brute-force, 10k×384) | 138.66 ms | — | — | parser cost dominates |
| **W10** vector top-10 (HNSW) | 126.81 ms | — | — | masked by parser cost |
| **W11** BM25 top-10 (1k docs) | 1.079 ms | 25.03 µs | — | ~43× |
| **W12** hybrid (1k docs) | 713.53 µs | — | — | RAG headline |

> The **canonical run** is [`benchmarks/results/2026-05-07-apple-9ffd55a5.json`](../benchmarks/results/2026-05-07-apple-9ffd55a5.json). The `dirty=true` flag in the commit metadata reflects the working-tree state when 9.6 PR was being authored (this doc + README updates uncommitted at run time); the **measurements themselves only depend on the bench binary**, which was built from the committed bench-9.5-duckdb tip. Subsequent official runs land alongside this file with their own date / host / commit.

---

## Engineering debts surfaced

Every bench-suite-found gap that exceeds the plan's "informational, not a gate" heuristic gets its own task. As of v1 of the suite:

| Task | Workload | Symptom | Likely root cause |
|---|---|---|---|
| **SQLR-17** | (CI infra) | `desktop-build` apt-get hung 39 min on the 9.1 PR | Azure-side runner / mirror flake — only act if recurs |
| **SQLR-18** | W4 single-row INSERT | ~673× vs SQLite | Bottom-up B-tree rebuild on every COMMIT (CLAUDE.md "B-tree commit strategy") |
| **SQLR-19** | W8 GROUP BY 100k-cardinality | ~245 s/iter (skipped by default) | Suspected `Vec`-backed group store — should be `HashMap` |
| **SQLR-20** | W9 INNER JOIN | ~14M× at 10k×10k; intractable at the 100k×100k plan target | Nested-loop driver doesn't push ON predicate to inner-side index probe |
| **SQLR-21** | W11 / W12 corpus cap | FTS doc-lengths sidecar must fit in one 4 KiB page (~1,360-doc cap) | Phase 8.1 — overflow chaining for posting + sidecar cells |

All five are "investigation, not a release gate" — the suite ships with the gap measured + the workaround documented inline + the task linked. Each task carries a reproducer and a sketch of the fix.

---

## Reproducing a run

```sh
# Default — lean (SQLRite + SQLite), criterion defaults
# (3s warmup, 5s measurement, 100 samples), heavy workloads
# capped at 10 samples.
make bench

# Full — adds DuckDB on Group B
make bench-duckdb

# Sharpen estimates on heavy workloads (override the per-group
# sample_size cap):
cargo bench -p sqlrite-benchmarks --bench suite -- \
    --measurement-time 30 --sample-size 30 'W3'

# Force-include SQLRite × W8/card-100k (default-skipped — SQLR-19):
SQLRITE_BENCH_W8_CARD_100K_SQLRITE=1 make bench
```

The aggregator picks an output filename based on host + commit short-SHA: `benchmarks/results/<YYYY-MM-DD>-<host_token>-<short_sha>.json`. Override with `OUTPUT=path/to.json scripts/run.sh`.

Local results are gitignored (`benchmarks/results/.gitignore`); only the pinned-host "official" runs get committed.

---

## Comparing two runs

```sh
benchmarks/scripts/compare.py \
    benchmarks/results/2026-05-07-applem1pro-aaaaaaaa.json \
    benchmarks/results/2026-06-01-applem1pro-bbbbbbbb.json \
    --md /tmp/diff.md
```

Reads two JSON envelopes, matches samples by `(workload, driver)`, computes per-workload percent change + ratio, and prints a Markdown table to stdout (or `--md OUT.md`). Same-version-only by Q8 — cross-version pairs land in their own "ignored" section. Cross-host pairs get a header warning; the script still runs but the numbers shouldn't be trusted as a true delta.

Pure stdlib Python — no third-party deps.

---

## What's NOT measured (and why)

- **CPU%.** Noisy on a shared machine; redundant with wall-clock for single-threaded workloads.
- **Concurrency curves.** Engine is single-writer by design (Phase 4e). No concurrent-writer workload is meaningful until that changes.
- **Network I/O.** All targets are in-process. Cloudflare D1 / rqlite are explicitly out of scope per the plan's viability section — they're network-dominated and would measure latency, not engine throughput.
- **libSQL.** Deferred — its embedded SQL surface tracks SQLite within a few percent, so a third row-oriented OLTP comparator would mostly add noise. Worth revisiting alongside a vector-only benchmark page when sqlite-vec / libSQL native vector indexes become a useful comparison axis.
- **fsync count on macOS.** `/proc/self/io` exists on Linux only. On macOS the equivalent would need dtrace; out of scope for v1.

---

## See also

- [`docs/benchmarks-plan.md`](benchmarks-plan.md) — design rationale + the resolved Q1–Q8 decisions.
- [`benchmarks/README.md`](../benchmarks/README.md) — workload-by-workload narrative.
- [`benchmarks/src/workloads/`](../benchmarks/src/workloads/) — one file per workload; methodology notes inline.
- [`benchmarks/results/`](../benchmarks/results/) — committed pinned-host runs.
- [`docs/architecture.md`](architecture.md) — engine layer map; benchmarks bind to the public `Connection` surface only.
- [`docs/roadmap.md`](roadmap.md) — broader project roadmap.
