# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

SQLRite is a from-scratch SQLite-style embedded database written in Rust. It's published on crates.io as `sqlrite-engine` (imported as `use sqlrite::…` — the lib target keeps the short name) and ships as: a REPL binary (`sqlrite`), a Tauri 2 + Svelte 5 desktop app, a Model Context Protocol stdio server (`sqlrite-mcp`), a C FFI shim (`sqlrite-ffi`), and language SDKs (Python via PyO3, Node via napi-rs, Go via cgo, WASM via wasm-bindgen). Phases 1–7 are shipped; the current branch `phase-8-plan` drafts inverted-index + BM25 full-text search and hybrid retrieval.

## Workspace layout

`Cargo.toml` is a workspace whose members are: `.` (the engine, package `sqlrite-engine`, lib `sqlrite`), `desktop/src-tauri`, `sqlrite-ffi`, `sqlrite-ask`, `sqlrite-mcp`, `sdk/python`, `sdk/nodejs`. `sdk/wasm` and `sdk/go` are deliberately **not** workspace members (wasm32 target / cgo separation).

- `src/` — engine. Public API is `Connection`/`Statement`/`Rows`/`Row`/`Value` from [src/connection.rs](src/connection.rs), re-exported via [src/lib.rs](src/lib.rs). Any new SDK should bind only to this surface.
- `sqlrite-ask/` — pure-Rust LLM adapter (Anthropic/OpenAI/Ollama) for natural-language → SQL. The engine's `ask` feature provides the thin `ConnectionAskExt::ask` glue.
- `sqlrite-mcp/` — MCP stdio server. Seven tools: `list_tables`, `describe_table`, `query`, `execute`, `schema_dump`, `vector_search`, `ask`. `--read-only` opens with a shared lock and hides `execute`.
- `sqlrite-ffi/` — C ABI cdylib + generated `sqlrite.h` header. Backs the Go SDK and any C consumer.
- `desktop/` — Tauri 2 + Svelte 5 GUI. Embeds the engine directly (no FFI hop).

Architecture deep-dive: [docs/architecture.md](docs/architecture.md). The full doc index is [docs/_index.md](docs/_index.md).

## Engine data flow

SQL string → [src/sql/mod.rs](src/sql/mod.rs) `process_command` parses with the external `sqlparser` crate (SQLite dialect) → [src/sql/parser/](src/sql/parser/) trims the AST into internal structs (`CreateQuery`, `InsertQuery`, `SelectQuery`) → [src/sql/executor.rs](src/sql/executor.rs) runs the statement against the in-memory `Database` ([src/sql/db/database.rs](src/sql/db/database.rs)). On any write, auto-save serializes changed pages through [src/sql/pager/](src/sql/pager/) — 4 KiB pages, cell-encoded B-trees per table and index, WAL + crash-safe checkpoint, fs2 advisory locks. Vector search (Phase 7d) goes through [src/sql/hnsw.rs](src/sql/hnsw.rs); KNN uses a bounded-heap top-k in the executor. Transactions snapshot the in-memory state; ROLLBACK restores it. There is no query optimizer beyond the KNN/HNSW shortcut, no joins, no aggregates yet.

## Commands

CI is the source of truth — the workspace excludes that follow are required because the desktop crate needs a Svelte build first and the PyO3/napi-rs cdylibs can't link standalone test binaries.

```sh
# Build / test the Rust workspace (matches CI)
cargo build --workspace --exclude sqlrite-desktop --exclude sqlrite-python --exclude sqlrite-nodejs --all-targets
cargo test  --workspace --exclude sqlrite-desktop --exclude sqlrite-python --exclude sqlrite-nodejs

# Single test (exact name; --nocapture to see println!)
cargo test <test_name> -- --nocapture

# Lint (CI runs all three)
cargo fmt --all -- --check
cargo clippy --workspace --exclude sqlrite-desktop --exclude sqlrite-python --exclude sqlrite-nodejs --all-targets
cargo doc    --workspace --exclude sqlrite-desktop --exclude sqlrite-python --exclude sqlrite-nodejs --no-deps

# Run the REPL (default features include cli + ask + file-locks)
cargo run                                  # in-memory
cargo run -- path/to/db.sqlrite            # open/create file
cargo run -- --readonly path/to/db.sqlrite # shared-lock open

# Crate-specific
cargo build --release -p sqlrite-ffi       # C cdylib + sqlrite.h
cd desktop && npm install && npm run tauri dev   # desktop app dev mode
cargo run -p sqlrite-mcp -- /path/to.sqlrite     # MCP server (stdio)

# Release plumbing
scripts/bump-version.sh 0.2.0              # bumps version across 11 manifests
```

`SQLRITE_LLM_API_KEY` is required for the `.ask` REPL command, the engine's `ask` feature, and the MCP `ask` tool. Clippy is **not** `-D warnings` yet (intentional — see top of [.github/workflows/ci.yml](.github/workflows/ci.yml)); deny-by-default lints still fail CI.

## Project-specific conventions

- **Errors.** Single `SQLRiteError` enum (thiserror, six variants) with a project-wide `Result<T>` alias. All public APIs return typed errors; no panics. The enum hand-rolls `PartialEq` because `std::io::Error` doesn't derive it.
- **Storage isn't bincode.** Tables and indexes share a cell-encoded B-tree format with a 4 KiB page size; the file header carries a format version (currently v4 after the Phase 7a vector column work). The diff-based pager only writes changed pages. See [docs/file-format.md](docs/file-format.md) and [docs/pager.md](docs/pager.md).
- **B-tree commit strategy.** Bottom-up rebuild on every commit (O(N), correct-by-construction). No in-place splits — deferred design decision.
- **Feature gates matter.** `default = ["cli", "ask", "file-locks"]`. The REPL `[[bin]]` `required-features = ["cli", "ask"]`. WASM and lean library embeddings build with `default-features = false` to avoid rustyline / clap / fs2 / sqlrite-ask. Don't pull these into the always-on dependency set.
- **Don't reinvent the SQL parser.** `sqlparser` is the tokenizer and AST source; project code only narrows that AST. New SQL features start by mapping the existing `sqlparser` AST node, not by extending a custom grammar.
- **Phase numbering is real.** The roadmap is sequenced in [docs/roadmap.md](docs/roadmap.md); design discussions live at github.com/sqlrite/design and feature work generally tracks an open phase plan in `docs/phase-*-plan.md`. Treat in-flight phase plans as load-bearing context.
- **Concurrency.** Engine mutates state through `Arc<Mutex<_>>` (Tauri-friendly). On-disk concurrency uses fs2 advisory locks: shared for readers, exclusive for the single writer.

## Knowledge Base

### Project-specific — `~/Documents/josh-obsidian-synced/Projects/rust_sqlite/`

- **Code:** `/Users/joaoh82/projects/rust_sqlite`
- **Context (read first):** `~/Documents/josh-obsidian-synced/Projects/rust_sqlite/context.md`
- **Notes (running journal):** `~/Documents/josh-obsidian-synced/Projects/rust_sqlite/notes.md`
- **Project wiki:** `~/Documents/josh-obsidian-synced/Projects/rust_sqlite/wiki/`

**How to use each:**

- `context.md` — stable background (product goals, stakeholders, domain). Read before starting non-trivial work. Update only when underlying facts change.
- `notes.md` — append-only dated journal. Add entries under `## YYYY-MM-DD` headings for decisions, blockers, TODOs, and incidents — anything worth preserving but not stable enough for `context.md`.
- `wiki/` — reference sub-docs (e.g. `Architecture.md`, `Local Dev Setup.md`, `Tech Services.md`). Create new files as topics emerge.

**When to save:**

- New stable fact about the product/domain → update `context.md`.
- A decision, incident, or working note → append a dated entry to `notes.md`.
- Reusable reference material (setup steps, credential locations, architecture) → new/updated file in `wiki/`.

### Cross-project knowledge — `~/Documents/josh-obsidian-synced/vault/`

- **General wiki:** `~/Documents/josh-obsidian-synced/vault/wiki/` — start at `_master-index.md`, then drill into the relevant topic's `_index.md`.
- **Raw dumps:** `~/Documents/josh-obsidian-synced/vault/raw/` — drop unprocessed research here as `YYYY-MM-DD-{slug}.md`.

Read the general wiki when the question isn't specific to this project. Drop raw research or imported notes into `vault/raw/` so it's captured even before it's distilled.
