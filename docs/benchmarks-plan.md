# Benchmarks plan — SQLRite vs SQLite (and friends)

**Status:** *approved 2026-05-06 — Q1–Q8 resolved; sub-phase 9.1 in flight.* All eight design questions were resolved by the project owner; see the **Decisions** section below for the canonical answers. Tracks task **SQLR-4** / **SQLR-16** (execution).

**TL;DR.** Stand up a small, focused benchmark suite that pits the engine against `SQLite` (mandatory) and `DuckDB` (optional, analytical-slice only) under a curated set of OLTP + analytical + AI-era workloads. Skip distributed and network-resident options (`Cloudflare D1`, `rqlite`) — they don't share SQLRite's deployment shape. Defer `libSQL` until we have a vector- or replication-flavored axis that justifies a third row-oriented embedded engine. Suite lives in a new top-level `benchmarks/` workspace member, not built by default, runs on demand on a pinned host, emits JSON for trend tracking.

The point isn't to "win" — SQLite has 25+ years of optimization behind it, expect a 10–100× gap on most workloads at first. The point is to (a) get a baseline so future engine work has a number to move, (b) prove the differentiator workloads (HNSW, BM25, hybrid retrieval) actually deliver, and (c) ground roadmap conversations about LSM / columnar engines and JIT'd executors with evidence rather than vibes.

---

## Why this exists

The "Possible extras" list in [`roadmap.md`](roadmap.md#possible-extras-not-pinned-to-a-phase) has carried "Benchmarks against SQLite" since the project was reopened. With Phase 8 done, JOINs (SQLR-5) and GROUP BY / aggregates (SQLR-3) shipped, the engine now has enough SQL surface that benchmark workloads beyond "single-row by PK" actually exercise something interesting.

Three concrete drivers:

1. **Decision support for the `Possible extras` list.** Roadmap items like "Alternate storage engines (LSM/SSTable for write-heavy workloads)" and the deferred Phase 5a.2 cursor refactor are speculative without measurement. A single-row-INSERT throughput number against SQLite tells us whether the bottom-up B-tree rebuild is actually the problem worth fixing, or whether the executor's row reassembly dominates.
2. **Differentiator validation.** Phase 7d (HNSW), Phase 8 (BM25), and Phase 8d (hybrid retrieval) shipped without comparable numbers. We have correctness tests; we don't have "this query against this corpus runs in N ms." That gap matters for users evaluating SQLRite for RAG.
3. **Regression detection.** Once the harness exists and emits JSON, it becomes mechanical to spot a 30% slowdown in a future PR. No regression detector is wired in v1, but the JSON shape is designed to support one.

---

## Scope philosophy

Same posture as the phase plans: stay proportional, ship in narrow sub-phases, each one independently useful.

- **Curated, not exhaustive.** ~10 workloads, hand-picked for the questions we want answered. Not a YCSB clone, not TPC-C — those measure things we don't ship (concurrency, real transactions across nodes).
- **One driver trait, multiple engines behind it.** A workload is engine-agnostic Rust code; the engine choice is a generic parameter. Adding libSQL or DuckDB is one file, not a fork.
- **Read-only on shared infra.** No CI runs at first — too noisy. The harness emits JSON to `benchmarks/results/` keyed by host + commit, and a pinned local machine produces the publishable numbers.
- **No SLOs in v1.** Publish numbers, don't gate merges. Once we have ~3 dated runs we can look at adding a "fail PR if SQLRite regresses >20% on workload X" job.

---

## Comparison-target viability

The task brief lists five candidates. Honest assessment of each:

### ✅ SQLite — primary target

The reference implementation. SQLRite is explicitly modeled on SQLite (see [`docs/architecture.md`](architecture.md), [`docs/file-format.md`](file-format.md)), so apples-to-apples comparison is the whole point.

- **Driver.** [`rusqlite`](https://crates.io/crates/rusqlite) — Rust bindings to libsqlite3, links the C library. Mature, well-optimized, the right "is this our fault or libsqlite's fault?" baseline.
- **Configuration.** Run with `journal_mode=WAL`, `synchronous=NORMAL`, `temp_store=MEMORY`. SQLRite's WAL is mandatory + always-on, so SQLite-default (`journal_mode=DELETE`, full fsync per commit) is *not* apples-to-apples. The [Q3 discussion](#q3-sqlite-tuning) below proposes locking in the WAL+NORMAL profile.
- **Coverage.** Every workload (1–9 below). The differentiator workloads (10–12) bring in SQLite's `FTS5` virtual table for BM25 and `sqlite-vec` for vectors as well-defined opponents.

### ✅ DuckDB — secondary, analytical-slice only

Embedded, in-process, single-file. Same deployment shape as SQLRite, *different* storage model: columnar OLAP, vectorized executor, MVCC. That divergence is what makes DuckDB interesting as a comparison.

- **Driver.** [`duckdb-rs`](https://crates.io/crates/duckdb) — official Rust bindings.
- **Where it's apples-to-apples.** Read-only SELECTs, COUNT/SUM/AVG aggregates, GROUP BY at scale, indexed range scans on read-only data. These are workloads where SQLRite's row-store is structurally disadvantaged and the question is *how* disadvantaged.
- **Where it's apples-to-oranges (skip).** Single-row INSERTs (DuckDB's bulk-load path is heavy; per-row insert is pathological by design), UPDATE/DELETE workloads, transactional mixed OLTP, secondary-index lookups by PK on small reads. Including DuckDB on these would be misleading.
- **Why include at all.** Two reasons: (1) the roadmap "Possible extras" mentions LSM/SSTable as a future write-heavy storage engine — DuckDB-on-OLAP gives us a sister number for "what does a different storage model look like?"; (2) it grounds the "we are not DuckDB, we are a SQLite alternative" positioning with a measurement, not just a sentence.
- **Gating.** Optional `--features duckdb` on the bench crate. Default `make bench` doesn't pull DuckDB; `make bench-duckdb` does. Avoids a heavy dep for users who only care about the SQLite comparison.

### 🟡 libSQL (Turso embedded) — defer to v2 of the suite

libSQL is a fork of SQLite (still C, with extensions: native vector type, server-side filter pushdown, Hrana wire protocol for remote sync). For pure embedded use, the SQL execution path mostly tracks SQLite — same parser tree, same VM, same B-tree. Differences worth measuring only show up on:

- **Native vector** — libSQL has a built-in vector index. Comparing SQLRite's HNSW (Phase 7d) against libSQL's would be more interesting than against `sqlite-vec`, since both are "first-party" implementations rather than extensions.
- **Replication-aware writes** — irrelevant for embedded benchmarks.

Verdict: skip in v1. The embedded SQL surface tracks SQLite closely enough that adding libSQL as a third row-oriented OLTP driver would mostly produce noise within a few percent of the SQLite numbers. Revisit when we want to publish a vector-only benchmark page (post-9.4) and want a non-extension competitor.

### ❌ Cloudflare D1 — out of scope

D1 is a remote, managed SQLite-compatible service that runs at Cloudflare's edge. Every query goes over HTTP. Even single-digit-millisecond network round-trips will dominate every workload — we'd be measuring the network, not the engine.

Document as out-of-scope. Revisit only if SQLRite ever ships a remote-server mode (no such phase is on the roadmap).

### ❌ rqlite — out of scope

rqlite is a distributed SQLite (Raft consensus over a cluster of nodes, accessed via HTTP). Not embedded. Reads from a follower still cross HTTP; writes pay Raft consensus latency. Same network-dominates-everything problem as D1.

Document as out-of-scope. Worth revisiting only if SQLRite explores a distributed mode — interesting then as a "what does the consistency cost look like?" reference.

---

## Workloads

Ten workloads, three groups. Each one has a fixed input dataset (deterministic seed), a fixed expected result that's checked once before timing starts, and a fixed criterion configuration. Group A = OLTP baseline; Group B = SQL-feature scaling; Group C = SQLRite differentiators.

### Group A — OLTP baseline (vs SQLite)

| ID | Name | Shape | Why it matters |
|----|------|-------|----------------|
| W1 | Read-by-PK | 100k-row table, prepared `SELECT … WHERE id = ?`, 10k random keys | Reference latency for the hottest path |
| W2 | Range scan | `WHERE indexed_col BETWEEN x AND y`, ranges sized 100 / 1k / 10k rows | Tests B-tree leaf walk + secondary-index path |
| W3 | Bulk insert | 100k rows in one transaction | Throughput; isolates COMMIT cost |
| W4 | Single-row insert | 1k INSERTs, each in its own implicit transaction | The fsync / WAL-commit hot path — expected gap vs SQLite |
| W5 | Mixed OLTP | YCSB-A flavor: 50/50 SELECT-by-PK / UPDATE-by-PK, 100k-row keyed table, 10k ops | Realistic-ish read+write mix |
| W6 | Index lookup | `SELECT * FROM t WHERE secondary = ?`, 10k probes on 100k rows | Tests secondary-index ROWID indirection |

### Group B — SQL-feature scaling (vs SQLite, optionally DuckDB)

| ID | Name | Shape | Why it matters |
|----|------|-------|----------------|
| W7 | Aggregate | `SELECT SUM(x) FROM t`, 1M rows | Full-scan + accumulator throughput |
| W8 | GROUP BY | `SELECT k, COUNT(*) FROM t GROUP BY k`, group counts of 10 / 1k / 100k | Hash aggregator behavior under cardinality pressure |
| W9 | JOIN | INNER JOIN on indexed PK/FK between two 100k-row tables | New territory after SQLR-5 — exercises the join planner |

### Group C — Differentiators (SQLRite-flavored, opportunistic comparators)

| ID | Name | Shape | Comparator |
|----|------|-------|------------|
| W10 | Vector top-10 | 10k 384-dim vectors, cosine top-10 query, with HNSW + brute-force variants | `sqlite-vec` extension if installable; else SQLRite-only baseline |
| W11 | BM25 top-10 | 10k-doc corpus, top-10 BM25 query | SQLite `FTS5` virtual table |
| W12 | Hybrid retrieval | 50/50 BM25 + cosine fusion (mirrors `examples/hybrid-retrieval/`) | SQLRite-only baseline |

For W10/W11, the goal isn't to beat the comparators — they're battle-hardened — it's to publish absolute numbers ("HNSW top-10 over 10k vectors: N ms") so users evaluating SQLRite for RAG have something concrete.

For W12, no off-the-shelf comparator exists in a single embedded engine; the number stands on its own.

---

## Metrics

Keep tight. The task brief lists many candidates; the suite measures these:

- **Latency: p50 / p95 / p99 / max.** Per workload, captured via `criterion`'s HDR-style sampling. Tail latency matters more than mean for OLTP.
- **Throughput: ops/s and rows/s.** Reported on bulk paths (W3, W7, W8).
- **Wall-clock per workload.** Sanity figure on the README table.
- **Disk usage at rest.** `.sqlrite` / `.sqlite` / `.duckdb` file size after each insert workload. One-line per-workload metric, useful for spotting page-fragmentation regressions.
- **Peak RSS.** Captured by wrapping the harness in `/usr/bin/time -v` (Linux) / `/usr/bin/time -l` (macOS), one number per run. Don't over-rotate on this — RSS is noisy and the engine's whole-DB-in-RAM model means it's mostly a function of dataset size.
- **fsync count (W4 only).** Linux: `/proc/self/io`. macOS: skip (no equivalent without dtrace). The point is to confirm that the gap between SQLRite and SQLite-WAL-NORMAL on W4 is fsync-shaped, not parser-shaped.

Explicitly **not** measured in v1:

- **CPU%.** Noisy on a shared machine, redundant with wall-clock for single-threaded workloads.
- **Concurrency curves.** Engine is single-writer by design (Phase 4e). No concurrent-writer workload is meaningful until that changes.
- **Network I/O.** All targets are in-process.

---

## Methodology

### Tooling

- **Harness:** [`criterion = "0.5"`](https://crates.io/crates/criterion). Defaults: 3 s warm-up, 5 s measurement, 30 samples, statistical confidence intervals. Outputs HTML reports + JSON.
- **Drivers:** Rust crates only — `rusqlite`, `duckdb-rs`, `sqlrite::Connection`. Keeps the harness language-uniform; no cross-language harness drift.
- **Process isolation:** each criterion `bench_function` opens a fresh DB file in a per-run `TempDir`. Cleanup on drop. Prevents page-cache / WAL-state bleed across runs.
- **Data generation:** deterministic seed (`StdRng::seed_from_u64(42)`). Datasets generated once into `benchmarks/data/` and reused across runs to keep generation cost out of timing.
- **Correctness gate:** every workload returns a result hash; the hash is verified against a fixed expected value before any timing runs. Wrong answers fast = not a win, and we want to catch bugs that change query results from showing up as a "speedup."

### SQLite tuning

Locked-in profile (see [Q3](#q3-sqlite-tuning)):

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA temp_store  = MEMORY;
PRAGMA cache_size  = -65536;  -- 64 MB
```

Rationale: SQLRite's WAL is mandatory + checkpointer is always on; SQLRite's commit fsync semantics map most closely to SQLite's `synchronous=NORMAL` (fsync on checkpoint, not on every commit). Comparing against SQLite-default (`DELETE` + full sync) would flatter SQLRite by measuring SQLite's most paranoid mode against SQLRite's only mode. We can publish a SQLite-default column too once the harness exists, but the headline number uses NORMAL.

### Hardware + reproducibility

- **v1 host:** owner's M-series MBP. Specs captured into the JSON envelope (CPU model, RAM, OS, kernel). Background processes quiesced via [criterion's `nocapture` notes].
- **CI:** off. Adds noise without value at this stage. The JSON output format is designed so that a future "regression detector" job can compare two runs from the same host.
- **Run command:** `make bench` from repo root → invokes `cargo bench -p sqlrite-benchmarks`. `make bench-duckdb` adds the `--features duckdb` axis.

---

## Repository layout

New top-level `benchmarks/` workspace member. CI skips it via an explicit `--exclude sqlrite-benchmarks` on every `cargo build` / `cargo test` / `cargo clippy` / `cargo doc` invocation (the same pattern that hides `sqlrite-desktop`, `sqlrite-python`, and `sqlrite-nodejs`). Run locally with `make bench`.

```
benchmarks/
├── Cargo.toml             — depends on rusqlite (default), duckdb (optional), criterion
├── README.md              — how to run, results table, host pinning notes
├── src/
│   ├── lib.rs             — Driver trait, common helpers
│   ├── data.rs            — deterministic dataset generators (seeded)
│   ├── drivers/
│   │   ├── sqlrite.rs
│   │   ├── sqlite.rs
│   │   └── duckdb.rs      — feature-gated
│   └── workloads/
│       ├── kv.rs          — W1
│       ├── range_scan.rs  — W2
│       ├── bulk_insert.rs — W3
│       ├── single_insert.rs — W4
│       ├── mixed_oltp.rs  — W5
│       ├── index_lookup.rs — W6
│       ├── aggregate.rs   — W7
│       ├── group_by.rs    — W8
│       ├── join.rs        — W9
│       ├── vector.rs      — W10
│       ├── fts.rs         — W11
│       └── hybrid.rs      — W12
├── benches/
│   └── suite.rs           — single criterion entry point that fans out
├── scripts/
│   ├── run.sh             — pipeline + JSON capture into results/
│   └── compare.py         — render JSON → markdown table
└── results/
    └── 2026-MM-DD-<host>-<commit>.json
```

The `Driver` trait carries just enough surface to express every workload:

```rust
pub trait Driver {
    type Conn;
    fn name(&self) -> &'static str;
    fn open(&self, path: &Path) -> anyhow::Result<Self::Conn>;
    fn execute(&self, conn: &mut Self::Conn, sql: &str) -> anyhow::Result<()>;
    fn execute_with_params(&self, conn: &mut Self::Conn, sql: &str, params: &[Value]) -> anyhow::Result<()>;
    fn query_one(&self, conn: &mut Self::Conn, sql: &str, params: &[Value]) -> anyhow::Result<Vec<Value>>;
    fn query_all(&self, conn: &mut Self::Conn, sql: &str, params: &[Value]) -> anyhow::Result<Vec<Vec<Value>>>;
}
```

Workloads are generic over `D: Driver`; the criterion entry point fans the same workload across `(SQLRiteDriver, SQLiteDriver, [DuckDBDriver])` and emits one bench per (workload, driver) pair.

---

## Sub-phases

Each ships as its own PR, runs the full existing test suite green, and adds one row to `benchmarks/README.md`'s results table.

### 9.1 — Harness scaffolding (~400 LOC + tests)

- `benchmarks/` crate skeleton, `Driver` trait, `data.rs` seeded generators.
- Two drivers: SQLRite (via `sqlrite::Connection`) + SQLite (via `rusqlite`).
- One workload end-to-end: **W1 (read-by-PK)**. Proves the harness shape.
- Lock in JSON output schema (workload, driver, p50/p95/p99/max, ops/s, dataset size, commit, host fingerprint).
- `make bench` target.

**Exit criterion:** `make bench` produces a JSON file under `benchmarks/results/`, `benchmarks/README.md` shows a 2-row table for W1.

### 9.2 — Group A workloads (~400 LOC + tests)

W2–W6. Each workload is one file in `src/workloads/`, plus one entry in `benches/suite.rs`. No engine changes — workloads compose only existing public API.

**Exit criterion:** all 6 Group A rows in the results table; if W4 (single-row insert) shows >100× gap, file a follow-up to investigate the commit path before moving on. (Investigation, not a gate — the gap is informational.)

### 9.3 — Group B workloads (~200 LOC + tests)

W7–W9. Aggregates / GROUP BY / JOIN — exercises SQLR-3 and SQLR-5 surface.

**Exit criterion:** all 3 Group B rows. JOIN performance vs SQLite is the most informative number here — SQLite has 25 years of join-planner tuning that SQLRite skipped; the magnitude of the gap is itself a roadmap input.

### 9.4 — Group C differentiators (~300 LOC + tests)

W10–W12. Vector top-10 (with `sqlite-vec` if installable), BM25 (vs SQLite FTS5), hybrid (SQLRite-only).

**Exit criterion:** absolute latency numbers published in the README. These are the headline numbers for the "SQLRite for RAG" pitch.

### 9.5 — DuckDB driver *(optional)* (~150 LOC)

Add the `duckdb-rs` driver under a `--features duckdb` flag. Wire only into Group B workloads (W7–W9) per the viability section. `make bench-duckdb` runs the extended suite.

**Exit criterion:** Group B table grows a third column. If a workload is misleading on DuckDB (e.g. DuckDB needs CHECKPOINT semantics that don't translate), document and skip rather than publish bad numbers.

### 9.6 — Reporting + first published run (~50 LOC + docs)

- `scripts/compare.py` renders any two JSONs into a Markdown diff table.
- First "official" pinned-host run committed under `benchmarks/results/`.
- `docs/benchmarks.md` becomes the canonical reference (mirrors how `docs/fts.md` is the canonical FTS reference for Phase 8). Cross-links from `README.md` "Roadmap" section and `docs/_index.md`.

**Exit criterion:** `docs/benchmarks.md` exists, the README has a "Benchmarks" section pointing at it, the first dated results JSON is committed.

### Post-9.6 ideas (parked)

- **libSQL driver** if/when we want a non-extension vector competitor for W10.
- **Per-PR regression detector.** A GitHub Action that runs the bench on a self-hosted runner and posts a comment if any workload regresses >20% from the last `main` baseline.
- **Concurrency workloads** if/when SQLRite gains true multi-writer support.
- **Larger datasets (10M, 100M).** v1 is sized for fast iteration on a laptop. A "release-blocker run" config could 100× the row counts.

### Total scope estimate

~1.5 kLOC of new Rust + ~150 lines of Python + a `Makefile` target + the canonical `docs/benchmarks.md` reference. Parallel scope with Phase 8 (~1.2 kLOC across 6 sub-phases). No engine changes — pure additive testing infrastructure.

---

## Decisions (was: open questions)

Q1–Q8 were resolved by the project owner on 2026-05-06. Each question keeps its original options + recommendation as a record of the rationale; the **Decided:** line at the top is the canonical answer the implementation should follow.

### Q1. Bench harness host

> **Decided: pinned local M-series MBP for v1.** Bare-metal hosts (Hetzner / Equinix) revisited post-9.6 once we're publishing numbers externally and want stability across runs. The JSON envelope already captures CPU model / RAM / OS / kernel so a future host swap is documentable, not a silent break.

Pinned local M-series MBP for v1, or rent a bare-metal box (Hetzner / Equinix) for stable numbers from day one?

**Recommendation:** local laptop. Cheaper, faster iteration, "developer wall-clock" is the right unit at this stage. Switch to bare-metal when we want to publish numbers externally (post-9.6).

### Q2. SLO thresholds

> **Decided: no PR-gating in v1.** The harness publishes numbers; it does not fail PRs. Tracked as a post-9.6 follow-up — needs ~3+ same-host baseline runs before any threshold (e.g. "regress >20% on workload X") is anything but noise-fitting.

Should the bench harness gate PRs ("fail if SQLRite regresses >X% on workload Y")?

**Recommendation:** no for v1. Need ~3+ baseline runs to know what "noise" looks like before setting a threshold. Document as a Post-9.6 idea.

### Q3. SQLite tuning

> **Decided: tuned (WAL + `synchronous=NORMAL`) is the headline.** SQLite-default (`journal_mode=DELETE` + `synchronous=FULL`) is the *secondary* column — opt-in via the harness, not the default `make bench` axis. Rationale stays in `docs/benchmarks.md` so anyone reading "SQLite Y ms vs SQLRite Z ms" sees up front that both engines are durability-comparable, not "SQLRite vs SQLite's most paranoid mode."

Compare against SQLite default settings (`journal_mode=DELETE`, `synchronous=FULL`) or tuned (`WAL` + `synchronous=NORMAL`)?

**Recommendation:** tuned (WAL+NORMAL) as the headline number, with a note in `benchmarks.md` explaining why. Optionally publish a "SQLite-default" column too, since some users compare against the default. Apples-to-apples is the goal — SQLRite has no `synchronous=FULL` mode to opt into.

### Q4. DuckDB inclusion

> **Decided: opt-in via `--features duckdb`** on the bench crate. `make bench` stays lean (rusqlite + sqlrite only); `make bench-duckdb` pulls the heavy dep and runs only Group B (W7–W9), per the viability section.

Hard-out, opt-in feature, or default-on?

**Recommendation:** opt-in via `--features duckdb`. Heavy dep, only useful on Group B. `make bench` stays lean.

### Q5. libSQL

> **Decided: punted to post-9.6.** Embedded libSQL tracks SQLite within a few percent on the OLTP path; not enough signal to justify a third row-oriented driver in v1. Revisit alongside any "vector-only" benchmark page (post-9.4) where a non-extension vector competitor would be informative.

Add as a +1 driver in 9.5 alongside DuckDB, or punt to post-9.6?

**Recommendation:** punt. The OLTP numbers will track SQLite within a few percent and add noise without insight. Worth adding when we want a non-extension vector competitor on W10.

### Q6. D1 / rqlite

> **Decided: out of scope.** Both are network-resident; round-trip latency dominates every workload. Out-of-scope rationale already lives in the [viability section](#comparison-target-viability); no driver work, no follow-up. Revisit only if SQLRite ever ships a remote-server / distributed mode.

Already proposed out-of-scope in the [viability section](#comparison-target-viability). Confirming by Q.

**Recommendation:** out. Document the rationale, don't burn cycles.

### Q7. Where to publish

> **Decided: in-repo.** Canonical reference at `docs/benchmarks.md` (lands in 9.6); raw JSON committed under `benchmarks/results/` keyed by date + host + commit; cross-link from `README.md` and `docs/_index.md`. A standalone docs site can grow out of this if/when demand appears, but versioned-with-the-code is the right v1 default.

In-repo Markdown (`docs/benchmarks.md` + raw JSON in `benchmarks/results/`), or a separate docs site?

**Recommendation:** in-repo. Versioned with the code, no extra infra, clickable from the README. A separate site can grow out of this if there's demand.

### Q8. Workload shape changes mid-suite

> **Decided: workloads carry an explicit version (`W1.v1`, `W1.v2`, …).** The JSON output schema includes `workload_version` per row; the comparison script only diffs same-version pairs and warns on cross-version compares. Bumping the version is the explicit "we changed the benchmark" gesture; old JSON files remain readable forever.

If we add a column or change a query in a workload between releases, how do we keep historical comparison meaningful?

**Recommendation:** workloads are versioned (`W1.v1`, `W1.v2`). Old JSON keeps the old workload-version key; results page only compares same-version runs. Cheap, opt-in, avoids "we silently changed the benchmark" mistakes.

---

## Risks + things to watch

- **Driver bias.** A poorly-written SQLite driver call (e.g. forgetting `prepare_cached`) makes SQLRite look 5× better than it is. Mitigation: code review every driver impl with the question "is this how a perf-conscious user of <engine> would write it?", and the correctness gate (hash-matching) catches divergent semantics.
- **Criterion overhead in micro-workloads.** For W1 (sub-microsecond per op territory after warmup), criterion's per-iter accounting can dominate. Mitigation: batch iterations inside the bench closure (criterion's `iter_batched` + a 1k-iteration inner loop), report ops/s computed against the inner-loop count.
- **`sqlite-vec` availability.** The extension isn't shipped with stock SQLite. W10 should treat the SQLite vector comparator as opportunistic — if not installed, run SQLRite-only and note it in the table. Don't make Group C hard-depend on it.
- **macOS vs Linux skew.** fsync semantics differ; W4 numbers won't be portable across OSes. Mitigation: JSON envelope captures `os.kind`, results page only compares within the same OS family.
- **Future format-version bumps.** Workloads write `.sqlrite` files. A future on-disk format change (e.g. v5→v6) means historical results files reference databases the engine can't reopen. Mitigation: results JSON only stores numbers + dataset spec, never the DB file. Datasets are always regenerated from seed.

---

## See also

- [`roadmap.md`](roadmap.md) — Phase 8 closeout + "Possible extras" entry that this plan replaces.
- [`docs/architecture.md`](architecture.md) — engine layer map; benchmarks bind to the public `Connection` surface only.
- [`docs/fts.md`](fts.md), [`examples/hybrid-retrieval/`](../examples/hybrid-retrieval/) — input shapes for W11, W12.
- [`docs/phase-7-plan.md`](phase-7-plan.md), [`docs/phase-8-plan.md`](phase-8-plan.md) — plan-doc shape this document mirrors.
