# SQLRite developer guide

A small, hand-written guide to the SQLRite codebase — how it's structured, how the pieces fit together, why the design choices were made, and how to work on the project.

## Start here

- [Getting started](getting-started.md) — install toolchain, build, run the REPL, your first `CREATE TABLE`
- [Using SQLRite](usage.md) — REPL flow, meta-commands, history, launch modes
- [Supported SQL](supported-sql.md) — canonical reference for every statement, operator, and edge case the engine executes today (plus what's not supported yet)
- [Desktop app](desktop.md) — downloads, unsigned-installer bypass steps, and the Tauri architecture
- [Smoke test](smoke-test.md) — step-by-step walkthrough to sanity-check REPL + desktop app after any non-trivial change
- [Architecture](architecture.md) — high-level layer diagram and module map
- [Design decisions](design-decisions.md) — the "why" behind the major choices
- [Roadmap](roadmap.md) — what's done, what's next, and the long-term arc

## Using SQLRite as a library

- [Embedding](embedding.md) — the public `Connection` / `Statement` / `Rows` API (Phase 5a) and where the non-Rust SDKs plug in (Phase 5b – 5g)
- [Concurrent writes — MVCC + `BEGIN CONCURRENT`](concurrent-writes.md) — Phase 11 canonical reference: SQL surface, embedding API, SDK error mapping, REPL meta-commands, durability story, limitations. Design rationale lives in the [historical plan-doc](concurrent-writes-plan.md).
- [`examples/`](../examples/) — runnable Rust quickstart (`cargo run --example quickstart`) + concurrent-writers retry-loop demo (`cargo run --example concurrent_writers`); language-specific subdirectories fill in as each 5x sub-phase lands

## Phase 7 — AI-era extensions

- [Ask — natural-language → SQL](ask.md) — the canonical reference for the `ask()` feature across every product surface (REPL, desktop, Rust library, Python / Node / Go / WASM SDKs, MCP server); env vars, defaults, prompt caching, security
- [Ask backend proxy templates](ask-backend-examples.md) — copy-paste backend examples for the WASM SDK's split design: Cloudflare Workers, Vercel Edge, Deno Deploy, Firebase Functions, AWS Lambda, Express, pure Node
- [MCP server (`sqlrite-mcp`)](mcp.md) — Phase 7h + 8e: SQLRite as a Model Context Protocol stdio server. Wiring into Claude Code / Cursor / `mcp-inspector`; the eight tools (`list_tables`, `describe_table`, `query`, `execute`, `schema_dump`, `vector_search`, `bm25_search`, `ask`); read-only mode; the JSON-RPC wire format

## Phase 8 — Full-text search + hybrid retrieval

- [FTS — full-text search + hybrid retrieval](fts.md) — the canonical reference for `CREATE INDEX … USING fts`, the `fts_match` / `bm25_score` scalar functions, the `try_fts_probe` optimizer hook, hybrid retrieval via raw arithmetic with `vec_distance_cosine`, persistence + the on-demand v4 → v5 file-format bump, and the `bm25_search` MCP tool

## Benchmarks

- [Benchmarks](benchmarks.md) — the canonical reference for the SQLRite-vs-SQLite-and-friends bench suite: how to run it (`make bench` / `make bench-duckdb`), what the twelve workloads measure, headline numbers, the engineering debts the suite surfaced (SQLR-18 / 19 / 20 / 21), reproducing a run, and the `compare.py` diff tool

## Internals

These documents go into the implementation of each subsystem.

- [File format](file-format.md) — the `.sqlrite` on-disk layout, byte by byte
- [Pager](pager.md) — page cache, diffing commits, WAL-backed commits, and the checkpointer
- [Storage model](storage-model.md) — `Table`, `Column`, `Row`, `Index`, how rows are reassembled
- [SQL engine](sql-engine.md) — parser → executor pipeline, expression evaluator, NULL handling
- [Desktop app](desktop.md) — the Tauri 2.0 + Svelte shell under `desktop/`

## Project state

As of May 2026, SQLRite has:

- A working SQL engine (in-memory + on-disk with a real B-Tree per table + secondary indexes, Phases 0 – 3 complete)
- WAL-backed persistence with crash-safe checkpointing, shared/exclusive lock modes, and real `BEGIN` / `COMMIT` / `ROLLBACK` transactions (Phase 4 complete)
- A stable public Rust embedding API plus C FFI shim and SDKs for Python, Node.js, Go, and WASM (Phase 5 complete except the optional 5f crate-polish task)
- A Tauri 2.0 + Svelte desktop app (Phase 2.5 complete)
- AI-era extensions across the product surface (Phase 7 complete): VECTOR columns + HNSW indexes (7a-7d), JSON columns (7e), the `ask()` natural-language → SQL family across the REPL / desktop / Rust / Python / Node / Go / WASM (7g.1-7g.7), and the [`sqlrite-mcp`](mcp.md) Model Context Protocol server (7h + 7g.8)
- Full-text search + hybrid retrieval (Phase 8 complete): FTS5-style inverted index with BM25 ranking + `fts_match` / `bm25_score` scalar functions + `try_fts_probe` optimizer hook + on-disk persistence with on-demand v4 → v5 file-format bump (8a-8c), a worked hybrid-retrieval example combining BM25 with vector cosine via raw arithmetic (8d), and a `bm25_search` MCP tool symmetric with `vector_search` (8e). See [`docs/fts.md`](fts.md).
- SQL surface + DX follow-ups (Phase 9 complete, v0.2.0 → v0.9.1): DDL completeness — `DEFAULT`, `DROP TABLE` / `DROP INDEX`, `ALTER TABLE` (9a); free-list + manual `VACUUM` (9b) + auto-VACUUM (9c); `IS NULL` / `IS NOT NULL` (9d); `GROUP BY` + aggregates + `DISTINCT` + `LIKE` + `IN` (9e); four flavors of `JOIN` — INNER, LEFT, RIGHT, FULL OUTER (9f); prepared statements + `?` parameter binding with a per-connection LRU plan cache (9g); HNSW probe widened to cosine + dot via `WITH (metric = …)` (9h); `PRAGMA` dispatcher with the `auto_vacuum` knob (9i)
- Benchmarks against SQLite + DuckDB (Phase 10 complete, SQLR-4 / SQLR-16): twelve-workload bench harness with a pluggable `Driver` trait, criterion-driven, pinned-host runs published. See [`docs/benchmarks.md`](benchmarks.md).
- Phase 11 (concurrent writes via MVCC + `BEGIN CONCURRENT`, SQLR-22) is **shipped end-to-end** — `Connection` is `Send + Sync`; `Connection::connect()` mints sibling handles. `sqlrite::mvcc` exposes `MvccClock`, `ActiveTxRegistry`, `MvStore`, `ConcurrentTx`, and the `MvccCommitBatch` / `MvccLogRecord` WAL codec. WAL header v1 → v2 persisted the clock high-water mark; v2 → v3 added typed MVCC log-record frames. `PRAGMA journal_mode = mvcc;` opts a database into MVCC. `BEGIN CONCURRENT` writes commit-validate against `MvStore`, abort with `SQLRiteError::Busy`, and append a typed MVCC log-record frame to the WAL — covered by the same fsync as the legacy page commit. Reopen replays those frames into `MvStore` and seeds `MvccClock` past the highest committed `commit_ts`. Reads via `Statement::query` see the BEGIN-time snapshot. Per-commit GC + `vacuum_mvcc()` bound version-chain growth. C FFI / Python / Node / Go all propagate `Busy` / `BusySnapshot` as typed retryable errors *and* mint sibling handles that share backing state — Go's process-level path registry (Phase 11.11c) handles cross-`*sql.DB` sharing too. The `sqlrite` REPL ships `.spawn` / `.use` / `.conns` for interactive demos; the SQLR-16 benchmark suite adds `W13` (concurrent writers, mostly disjoint rows) as the Phase-11 differentiator workload. The only remaining items are deferred-by-design or foundation work: indexes under MVCC (11.10) and the checkpoint-drain follow-up (parked half of 11.9). **User-facing reference:** [`docs/concurrent-writes.md`](concurrent-writes.md); runnable example at [`examples/rust/concurrent_writers.rs`](../examples/rust/concurrent_writers.rs). Original design proposal: [`docs/concurrent-writes-plan.md`](concurrent-writes-plan.md).
- A fully-automated release pipeline that ships every product to its registry on every release with one human action — Rust engine + `sqlrite-ask` + `sqlrite-mcp` to crates.io, Python wheels to PyPI (`sqlrite`), Node.js + WASM to npm (`@joaoh82/sqlrite` + `@joaoh82/sqlrite-wasm`), Go module via `sdk/go/v*` git tag, plus C FFI tarballs, MCP binary tarballs, and unsigned desktop installers as GitHub Release assets (Phase 6 complete)

See the [Roadmap](roadmap.md) for the full phase plan.

## Release engineering

- [Release plan](release-plan.md) — Phase 6 design doc: lockstep versioning, PR-based release flow, OIDC trusted publishing, the version-bump surface
- [Release secrets runbook](release-secrets.md) — one-time web-UI setup for crates.io, PyPI, npm, GitHub `release` environment, and `main` branch protection
- [`scripts/`](../scripts/) — runnable tooling used by release workflows + reproducible locally (start with `scripts/bump-version.sh`)

## Future work

- [Phase 7 plan](phase-7-plan.md) — AI-era extensions (vector column type + HNSW, JSON, NL→SQL `ask()` API across REPL/library/SDKs/desktop/MCP, MCP server). **Implementation complete except 7f, which deferred to Phase 8.**
- [Phase 8 plan](phase-8-plan.md) — Full-text search (FTS5-style BM25) + hybrid retrieval. The deferred 7f scope. **All six sub-phases (8a–8f) shipped.** Canonical reference: [`docs/fts.md`](fts.md).
- [Benchmarks plan](benchmarks-plan.md) — design rationale + the resolved Q1–Q8 decisions for the bench suite. Historical reference; the user-facing canonical doc is [`docs/benchmarks.md`](benchmarks.md) (above). All six sub-phases (9.1–9.6) shipped.

## Conventions

- Code lives under [`src/`](../src/); docs live here under [`docs/`](./).
- Commit messages carry a `[path]` prefix describing the areas touched (see `git log`).
- Every non-trivial change lands with tests. The suite is run on every commit via `cargo test`.
- The engine is both a library (`src/lib.rs` — the `sqlrite` crate) and a binary (`src/main.rs` — the REPL). External consumers should import it as a library.

If you're reading the code and a piece feels surprising, check this guide first — most non-obvious decisions are documented under [Design decisions](design-decisions.md).
