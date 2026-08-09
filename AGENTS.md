# AGENTS.md

> **`CLAUDE.md` in this directory is the source of truth.** Read it in full
> before doing any work here — it contains the working guidelines (git/branch
> rules, knowledge base, task management, pull-request registration, reserved
> ports, engineering defaults). If anything in this file conflicts with
> `CLAUDE.md`, `CLAUDE.md` wins.

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Project

SQLRite is a from-scratch SQLite-style embedded database written in Rust. It's published on crates.io as `sqlrite-engine` (imported as `use sqlrite::…` — the lib target keeps the short name) and ships as: a REPL binary (`sqlrite`), a Tauri 2 + Svelte 5 desktop app, a Model Context Protocol stdio server (`sqlrite-mcp`), a C FFI shim (`sqlrite-ffi`), and language SDKs (Python via PyO3, Node via napi-rs, Go via cgo, WASM via wasm-bindgen).

Phases 0–11 are shipped (current release **v0.14.0**): SQL surface through JOINs / aggregates / prepared statements, WAL-backed durability, vector + JSON + full-text search, benchmarks vs SQLite and DuckDB, and MVCC `BEGIN CONCURRENT` writes. Work is now ticket-driven (`SQLR-*` in marvinapp) rather than phase-driven; [docs/roadmap.md](docs/roadmap.md) tracks the phase history and the open frontier.

## Workspace layout

`Cargo.toml` is a workspace whose members are: `.` (the engine, package `sqlrite-engine`, lib `sqlrite`), `desktop/src-tauri`, `examples/desktop-journal/src-tauri`, `sqlrite-ffi`, `sqlrite-ask`, `sqlrite-mcp`, `sdk/python`, `sdk/nodejs`, `benchmarks`. `sdk/wasm` and `sdk/go` are deliberately **not** workspace members (wasm32 target / cgo separation).

- `src/` — engine. Public API is `Connection`/`Statement`/`Rows`/`Row`/`Value` from [src/connection.rs](src/connection.rs), re-exported via [src/lib.rs](src/lib.rs). Any new SDK should bind only to this surface.
- `sqlrite-ask/` — pure-Rust LLM adapter (Anthropic/OpenAI/Ollama) for natural-language → SQL. The engine's `ask` feature provides the thin `ConnectionAskExt::ask` glue.
- `sqlrite-mcp/` — MCP stdio server. Eight tools, one per file under [sqlrite-mcp/src/tools/](sqlrite-mcp/src/tools/): `list_tables`, `describe_table`, `query`, `execute`, `schema_dump`, `vector_search`, `bm25_search`, `ask`. `--read-only` opens with a shared lock and hides `execute`.
- `sqlrite-ffi/` — C ABI cdylib + generated `sqlrite.h` header. Backs the Go SDK and any C consumer.
- `desktop/` — Tauri 2 + Svelte 5 generic SQL playground GUI. Embeds the engine directly (no FFI hop).
- `examples/desktop-journal/` — Tauri 2 + Svelte 5 local-first journaling app (package `sqlrite-journal`, SQLR-41). Showcase for BM25 + `ask` in a non-AI-native product. Mirrors `desktop/`'s engine-as-Cargo-dep pattern but uses the modern `Connection` API.
- `examples/` — also holds the runnable per-language samples (`rust/`, `python/`, `nodejs/`, `go/`, `c/`, `wasm/`) plus two CI-verified apps: `go-collector/` (edge/IoT, cgo against `libsqlrite_c`) and `nodejs-notes/`.
- `benchmarks/` — SQLR-4 / SQLR-16 bench harness. `Driver` trait + SQLRite + SQLite (rusqlite-bundled) drivers + criterion-driven workloads. Excluded from the default CI build/test/clippy/doc commands; run locally with `make bench` (or `make bench-duckdb`). See [docs/benchmarks.md](docs/benchmarks.md).
- `web/` — marketing + docs site (Next.js 15 + Tailwind v4). Independent of the Cargo workspace; lives in-repo for now but is structured to lift into its own repository later. See [web/README.md](web/README.md).

Architecture deep-dive: [docs/architecture.md](docs/architecture.md). The full doc index is [docs/_index.md](docs/_index.md).

## Engine data flow

SQL string → [src/sql/mod.rs](src/sql/mod.rs) `process_command` parses with the external `sqlparser` crate (SQLite dialect) → [src/sql/parser/](src/sql/parser/) trims the AST into internal structs (`CreateQuery`, `InsertQuery`, `SelectQuery`) → [src/sql/executor.rs](src/sql/executor.rs) runs the statement against the in-memory `Database` ([src/sql/db/database.rs](src/sql/db/database.rs)). On any write, auto-save serializes changed pages through [src/sql/pager/](src/sql/pager/) — 4 KiB pages, cell-encoded B-trees per table and index, WAL + crash-safe checkpoint, fs2 advisory locks, free-list + VACUUM.

The executor is the big file (~6k lines) and owns most of the surface area:

- **Joins / aggregates** — four JOIN flavors, `GROUP BY` + aggregate functions ([src/sql/agg.rs](src/sql/agg.rs)), `DISTINCT`, `LIKE`, `IN`.
- **Prepared statements** — `?` placeholders are rewritten and bound in [src/sql/params.rs](src/sql/params.rs) (needs `sqlparser`'s `visitor` feature); each `Connection` keeps an LRU plan cache.
- **Vector search** — [src/sql/hnsw.rs](src/sql/hnsw.rs) for the ANN index; KNN uses a bounded-heap top-k in the executor. Metric selected via `WITH (metric = …)`.
- **Full-text search** — [src/sql/fts/](src/sql/fts/) (tokenizer, posting lists, BM25). `CREATE INDEX … USING fts`, the `fts_match` / `bm25_score` scalar functions, and the `try_fts_probe` optimizer hook. See [docs/fts.md](docs/fts.md).
- **PRAGMA** — [src/sql/pragma.rs](src/sql/pragma.rs) dispatches `auto_vacuum`, `journal_mode`, etc.

There is no general query optimizer — only the index-equality probe, the KNN/HNSW shortcut, and `try_fts_probe`.

Two concurrency models coexist: the default WAL mode (transactions snapshot in-memory state; ROLLBACK restores it) and the opt-in MVCC path under [src/mvcc/](src/mvcc/) (`PRAGMA journal_mode = mvcc` + `BEGIN CONCURRENT`). See [docs/concurrent-writes.md](docs/concurrent-writes.md).

## Commands

CI is the source of truth — the workspace excludes that follow are required because the two Tauri crates need a frontend build first, the PyO3/napi-rs cdylibs can't link standalone test binaries, and the `benchmarks/` harness deliberately stays out of CI (criterion is noisy on shared runners; the rusqlite-bundled build is heavy).

```sh
# Build / test the Rust workspace (matches CI)
cargo build --workspace --exclude sqlrite-desktop --exclude sqlrite-journal --exclude sqlrite-python --exclude sqlrite-nodejs --exclude sqlrite-benchmarks --all-targets
cargo test  --workspace --exclude sqlrite-desktop --exclude sqlrite-journal --exclude sqlrite-python --exclude sqlrite-nodejs --exclude sqlrite-benchmarks

# Single test (exact name; --nocapture to see println!)
cargo test <test_name> -- --nocapture

# Lint (CI runs all three)
cargo fmt --all -- --check
cargo clippy --workspace --exclude sqlrite-desktop --exclude sqlrite-journal --exclude sqlrite-python --exclude sqlrite-nodejs --exclude sqlrite-benchmarks --all-targets
cargo doc    --workspace --exclude sqlrite-desktop --exclude sqlrite-journal --exclude sqlrite-python --exclude sqlrite-nodejs --exclude sqlrite-benchmarks --no-deps

# Run the REPL (default features include cli + ask + file-locks)
cargo run                                  # in-memory
cargo run -- path/to/db.sqlrite            # open/create file
cargo run -- --readonly path/to/db.sqlrite # shared-lock open

# Crate-specific
cargo build --release -p sqlrite-ffi       # C cdylib + sqlrite.h
cd desktop && npm install && npm run tauri dev   # desktop app dev mode
cargo run -p sqlrite-mcp -- /path/to.sqlrite     # MCP server (stdio)
cd examples/desktop-journal && npm install && npm run tauri dev  # journal example app

# SDKs (each has its own toolchain; these mirror the CI jobs)
cd sdk/python  && maturin develop && python -m pytest tests/
cd sdk/nodejs  && npm ci && npm run build && npm test
cargo build --release -p sqlrite-ffi && cd sdk/go && go test ./...   # cgo needs the cdylib first
cd sdk/wasm    && wasm-pack build --target web --release

# Website (Next.js) — NOT port 3000, that's reserved for the local Marvin API
cd web && npm install && npm run dev -- -p 3001

# Benchmarks (SQLR-4 / SQLR-16) — local-only, never CI
make bench                                 # SQLRite + SQLite (lean)
make bench-duckdb                          # adds DuckDB driver (Group B only)

# Release plumbing
scripts/bump-version.sh 0.15.0             # lockstep bump across every manifest
```

`SQLRITE_LLM_API_KEY` is required for the `.ask` REPL command, the engine's `ask` feature, and the MCP `ask` tool. Clippy is **not** `-D warnings` yet (intentional — see top of [.github/workflows/ci.yml](.github/workflows/ci.yml)); deny-by-default lints still fail CI.

## Project-specific conventions

- **Errors.** Single `SQLRiteError` enum (thiserror) with a project-wide `Result<T>` alias. All public APIs return typed errors; no panics. The enum hand-rolls `PartialEq` because `std::io::Error` doesn't derive it. `Busy` / `BusySnapshot` are the retryable MVCC variants and every SDK maps them to a typed retryable error — don't collapse them into `General`.
- **Storage isn't bincode.** Tables and indexes share a cell-encoded B-tree format with a 4 KiB page size. The header's format version is promoted **on demand**: v4 is the baseline, the first save carrying an FTS index writes v5, the first save producing a non-empty free-list writes v6. Decoders accept all three. The diff-based pager only writes changed pages. See [docs/file-format.md](docs/file-format.md) and [docs/pager.md](docs/pager.md).
- **B-tree commit strategy.** Bottom-up rebuild on every commit (O(N), correct-by-construction). No in-place splits — deferred design decision.
- **Feature gates matter.** `default = ["cli", "ask", "file-locks"]`. The REPL `[[bin]]` `required-features = ["cli", "ask"]`. WASM and lean library embeddings build with `default-features = false` to avoid rustyline / clap / fs2 / sqlrite-ask. Don't pull these into the always-on dependency set.
- **Don't reinvent the SQL parser.** `sqlparser` is the tokenizer and AST source; project code only narrows that AST. New SQL features start by mapping the existing `sqlparser` AST node, not by extending a custom grammar.
- **Concurrency.** Engine mutates state through `Arc<Mutex<_>>` (Tauri-friendly); `Connection` is `Send + Sync` and `Connection::connect()` mints sibling handles that share backing state. On-disk concurrency uses fs2 advisory locks: shared for readers, exclusive for the single writer. Under `PRAGMA journal_mode = mvcc`, `BEGIN CONCURRENT` writes validate at commit against `MvStore` and abort with `Busy`; callers are expected to retry (see [examples/rust/concurrent_writers.rs](examples/rust/concurrent_writers.rs)). Indexes under MVCC are explicitly not implemented yet.
- **Version bumps are lockstep.** Every crate, SDK manifest, and `package.json` carries the same version. Always run `scripts/bump-version.sh <version>` — never hand-edit a manifest; the script also rewrites the intra-workspace dependency pins that `cargo publish` requires.
- **The `cargo publish` include-list is load-bearing.** The package root *is* the workspace root, so the root `Cargo.toml`'s `include = [...]` allowlist is what keeps the crates.io tarball under the 10 MiB cap. Adding a new `[[example]]` means adding its `.rs` path there too.
- **Playground WASM is vendored.** `/playground` on the site loads a pinned copy of `sdk/wasm/pkg/` committed at `web/public/playground/pkg/`. Engine changes don't reach it until that bundle is rebuilt and re-committed — Vercel won't build it.
- **Docs are canonical, plan docs are historical.** Each subsystem has a user-facing reference in [docs/](docs/) (`fts.md`, `concurrent-writes.md`, `benchmarks.md`, `ask.md`, `mcp.md`, `supported-sql.md`); the matching `docs/*-plan.md` files are design rationale kept for archaeology. Update the canonical doc when behavior changes.

## Knowledge Base

Project knowledge lives in the **`projects-knowledge` Obsidian vault**, under `~/Documents/projects-knowledge/Projects/`. It is a plain synced folder — no git, nothing to clone, pull, or commit; just read and write the files, and read only this project's folder. If the vault path doesn't exist (cloud sandbox, remote agent, CI), skip the knowledge base. Full workflow: see the matching section in [CLAUDE.md](CLAUDE.md).

### Project-specific — `~/Documents/projects-knowledge/Projects/rust_sqlite/`

- **Code:** `/Users/joaoh82/projects/rust_sqlite`
- **Context (read first):** `~/Documents/projects-knowledge/Projects/rust_sqlite/context.md`
- **Notes (running journal):** `~/Documents/projects-knowledge/Projects/rust_sqlite/notes.md`
- **Project wiki:** `~/Documents/projects-knowledge/Projects/rust_sqlite/wiki/`

**How to use each:**

- `context.md` — stable background (product goals, stakeholders, domain). Read before starting non-trivial work. Update only when underlying facts change.
- `notes.md` — append-only dated journal. Add entries under `## YYYY-MM-DD` headings for decisions, blockers, TODOs, and incidents — anything worth preserving but not stable enough for `context.md`.
- `wiki/` — reference sub-docs (e.g. `Architecture.md`, `Local Dev Setup.md`, `Tech Services.md`). Create new files as topics emerge.

**When to save:**

- New stable fact about the product/domain → update `context.md`.
- A decision, incident, or working note → append a dated entry to `notes.md`.
- Reusable reference material (setup steps, credential locations, architecture) → new/updated file in `wiki/`.

## Harness-specific notes

Additions specific to a particular harness (Codex, Hermes, etc.) may live below this line. Everything general belongs in `CLAUDE.md`.
