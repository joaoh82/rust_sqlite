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
- [`examples/`](../examples/) — runnable Rust quickstart (`cargo run --example quickstart`); language-specific subdirectories fill in as each 5x sub-phase lands

## Phase 7 — AI-era extensions

- [Ask — natural-language → SQL](ask.md) — the canonical reference for the `ask()` feature across every product surface (REPL, desktop, Rust library, Python / Node / Go / WASM SDKs, MCP server); env vars, defaults, prompt caching, security
- [Ask backend proxy templates](ask-backend-examples.md) — copy-paste backend examples for the WASM SDK's split design: Cloudflare Workers, Vercel Edge, Deno Deploy, Firebase Functions, AWS Lambda, Express, pure Node
- [MCP server (`sqlrite-mcp`)](mcp.md) — Phase 7h: SQLRite as a Model Context Protocol stdio server. Wiring into Claude Code / Cursor / `mcp-inspector`; the seven tools (`list_tables`, `describe_table`, `query`, `execute`, `schema_dump`, `vector_search`, `ask`); read-only mode; the JSON-RPC wire format

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
- AI-era extensions across the product surface (Phase 7 complete except FTS): VECTOR columns + HNSW indexes (7a-7d), JSON columns (7e), the `ask()` natural-language → SQL family across the REPL / desktop / Rust / Python / Node / Go / WASM (7g.1-7g.7), and the [`sqlrite-mcp`](mcp.md) Model Context Protocol server with seven tools including `ask` (7h + 7g.8)
- A fully-automated release pipeline that ships every product to its registry on every release with one human action — Rust engine + `sqlrite-ask` + `sqlrite-mcp` to crates.io, Python wheels to PyPI (`sqlrite`), Node.js + WASM to npm (`@joaoh82/sqlrite` + `@joaoh82/sqlrite-wasm`), Go module via `sdk/go/v*` git tag, plus C FFI tarballs, MCP binary tarballs, and unsigned desktop installers as GitHub Release assets (Phase 6 complete)

The active frontier is **Phase 8 — Full-text search + hybrid retrieval** (the deferred 7f scope).

See the [Roadmap](roadmap.md) for the full phase plan.

## Release engineering

- [Release plan](release-plan.md) — Phase 6 design doc: lockstep versioning, PR-based release flow, OIDC trusted publishing, the version-bump surface
- [Release secrets runbook](release-secrets.md) — one-time web-UI setup for crates.io, PyPI, npm, GitHub `release` environment, and `main` branch protection
- [`scripts/`](../scripts/) — runnable tooling used by release workflows + reproducible locally (start with `scripts/bump-version.sh`)

## Future work

- [Phase 7 plan](phase-7-plan.md) — AI-era extensions (vector column type + HNSW, JSON, NL→SQL `ask()` API across REPL/library/SDKs/desktop/MCP, MCP server). **Implementation complete except 7f, which deferred to Phase 8.**
- Phase 8 — Full-text search (FTS5-style BM25) + hybrid retrieval, deferred from Phase 7 per the plan-doc's Q1. **Active frontier as of May 2026.**

## Conventions

- Code lives under [`src/`](../src/); docs live here under [`docs/`](./).
- Commit messages carry a `[path]` prefix describing the areas touched (see `git log`).
- Every non-trivial change lands with tests. The suite is run on every commit via `cargo test`.
- The engine is both a library (`src/lib.rs` — the `sqlrite` crate) and a binary (`src/main.rs` — the REPL). External consumers should import it as a library.

If you're reading the code and a piece feels surprising, check this guide first — most non-obvious decisions are documented under [Design decisions](design-decisions.md).
