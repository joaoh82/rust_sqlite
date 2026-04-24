# SQLRite developer guide

A small, hand-written guide to the SQLRite codebase — how it's structured, how the pieces fit together, why the design choices were made, and how to work on the project.

## Start here

- [Getting started](getting-started.md) — install toolchain, build, run the REPL, your first `CREATE TABLE`
- [Using SQLRite](usage.md) — complete REPL / SQL / meta-command reference
- [Desktop app](desktop.md) — downloads, unsigned-installer bypass steps, and the Tauri architecture
- [Smoke test](smoke-test.md) — step-by-step walkthrough to sanity-check REPL + desktop app after any non-trivial change
- [Architecture](architecture.md) — high-level layer diagram and module map
- [Design decisions](design-decisions.md) — the "why" behind the major choices
- [Roadmap](roadmap.md) — what's done, what's next, and the long-term arc

## Using SQLRite as a library

- [Embedding](embedding.md) — the public `Connection` / `Statement` / `Rows` API (Phase 5a) and where the non-Rust SDKs plug in (Phase 5b – 5g)
- [`examples/`](../examples/) — runnable Rust quickstart (`cargo run --example quickstart`); language-specific subdirectories fill in as each 5x sub-phase lands

## Internals

These documents go into the implementation of each subsystem.

- [File format](file-format.md) — the `.sqlrite` on-disk layout, byte by byte
- [Pager](pager.md) — page cache, diffing commits, WAL-backed commits, and the checkpointer
- [Storage model](storage-model.md) — `Table`, `Column`, `Row`, `Index`, how rows are reassembled
- [SQL engine](sql-engine.md) — parser → executor pipeline, expression evaluator, NULL handling
- [Desktop app](desktop.md) — the Tauri 2.0 + Svelte shell under `desktop/`

## Project state

As of April 2026, SQLRite has:

- A working SQL engine (in-memory + on-disk with a real B-Tree per table + secondary indexes, Phases 0 – 3 complete)
- WAL-backed persistence with crash-safe checkpointing, shared/exclusive lock modes, and real `BEGIN` / `COMMIT` / `ROLLBACK` transactions (Phase 4 complete)
- A stable public Rust embedding API (Phase 5a complete — the current frontier is Phase 5b, the C FFI shim, and the language SDKs that build on it)
- A Tauri 2.0 + Svelte desktop app (Phase 2.5 complete)

See the [Roadmap](roadmap.md) for the full phase plan.

## Release engineering

- [Release plan](release-plan.md) — Phase 6 design doc: lockstep versioning, PR-based release flow, OIDC trusted publishing, the ten-file version-bump surface
- [Release secrets runbook](release-secrets.md) — one-time web-UI setup for crates.io, PyPI, npm, GitHub `release` environment, and `main` branch protection
- [`scripts/`](../scripts/) — runnable tooling used by release workflows + reproducible locally (start with `scripts/bump-version.sh`)

## Conventions

- Code lives under [`src/`](../src/); docs live here under [`docs/`](./).
- Commit messages carry a `[path]` prefix describing the areas touched (see `git log`).
- Every non-trivial change lands with tests. The suite is run on every commit via `cargo test`.
- The engine is both a library (`src/lib.rs` — the `sqlrite` crate) and a binary (`src/main.rs` — the REPL). External consumers should import it as a library.

If you're reading the code and a piece feels surprising, check this guide first — most non-obvious decisions are documented under [Design decisions](design-decisions.md).
