# SQLRite developer guide

A small, hand-written guide to the SQLRite codebase — how it's structured, how the pieces fit together, why the design choices were made, and how to work on the project.

## Start here

- [Getting started](getting-started.md) — install toolchain, build, run the REPL, your first `CREATE TABLE`
- [Using SQLRite](usage.md) — complete REPL / SQL / meta-command reference
- [Architecture](architecture.md) — high-level layer diagram and module map
- [Design decisions](design-decisions.md) — the "why" behind the major choices
- [Roadmap](roadmap.md) — what's done, what's next, and the long-term arc

## Internals

These documents go into the implementation of each subsystem.

- [File format](file-format.md) — the `.sqlrite` on-disk layout, byte by byte
- [Pager](pager.md) — page cache, diffing commits, how `.open` / auto-save work under the hood
- [Storage model](storage-model.md) — `Table`, `Column`, `Row`, `Index`, how rows are reassembled
- [SQL engine](sql-engine.md) — parser → executor pipeline, expression evaluator, NULL handling
- [Desktop app](desktop.md) — the Tauri 2.0 + Svelte shell under `desktop/`

## Project state

As of this writing (April 2026), SQLRite has a working in-memory SQL engine plus on-disk persistence via 4 KiB paged files with a diff-based auto-save pager. The big missing piece is the real on-disk B-Tree (Phase 3c/3d) — today each table is still serialized as one opaque `bincode` blob that spans multiple pages. See the [Roadmap](roadmap.md) for the full phase plan.

## Conventions

- Code lives under [`src/`](../src/); docs live here under [`docs/`](./).
- Commit messages carry a `[path]` prefix describing the areas touched (see `git log`).
- Every non-trivial change lands with tests. The suite is run on every commit via `cargo test`.
- The project is a binary crate (`src/main.rs`); the engine is not yet split into a library (Phase 5).

If you're reading the code and a piece feels surprising, check this guide first — most non-obvious decisions are documented under [Design decisions](design-decisions.md).
