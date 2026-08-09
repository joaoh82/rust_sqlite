# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **Source of truth.** This `CLAUDE.md` is the canonical working-guidelines
> file for **all** agent harnesses operating in this project — Claude Code,
> Codex, Hermes, Cursor, and anything else. `AGENTS.md` in this directory is a
> pointer to this file; if the two ever conflict, this file wins.

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
- `examples/desktop-journal/` — Tauri 2 + Svelte 5 local-first journaling app (package `sqlrite-journal`, SQLR-41). Showcase for BM25 + `ask` in a non-AI-native product; uses the modern `Connection` API. Built in its own CI job like `desktop/`.
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

Two concurrency models coexist: the default WAL mode (transactions snapshot in-memory state; ROLLBACK restores it) and the opt-in MVCC path under [src/mvcc/](src/mvcc/) (`PRAGMA journal_mode = mvcc` + `BEGIN CONCURRENT`). See **Concurrency** below and [docs/concurrent-writes.md](docs/concurrent-writes.md).

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
cargo run -p sqlrite-mcp -- /path/to.sqlrite     # MCP server (stdio)
cd desktop && npm install && npm run tauri dev            # desktop app dev mode
cd examples/desktop-journal && npm install && npm run tauri dev   # journal example app

# SDKs (each has its own toolchain; these mirror the CI jobs)
cd sdk/python  && maturin develop && python -m pytest tests/
cd sdk/nodejs  && npm ci && npm run build && npm test
cargo build --release -p sqlrite-ffi && cd sdk/go && go test ./...   # cgo needs the cdylib first
cd sdk/wasm    && wasm-pack build --target web --release

# Website (Next.js) — NOT port 3000, that's reserved (see Reserved local ports)
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

## General Guidelines

When a Context7 MCP server is available in the current harness, use it for code generation, setup or configuration steps, and library/API documentation. Resolve library IDs and fetch docs automatically without being asked.

## NEVER touch `main` — no exceptions, ever

This is an **absolute, inviolable rule**. There is no scenario — auto mode, "quick fix", "tiny change", emergency, "the world is ending", explicit user request that sounds like permission — in which it is acceptable to mutate files, install dependencies, run codegen, create files, or otherwise dirty the working tree while the current branch is `main` (or any equivalent integration branch: `master`, `trunk`, `develop`, the repo's configured default branch).

**Before doing ANY state-mutating work**, the very first action is to leave `main`:

1. **Preferred — worktree.** For any work that may run more than a few minutes, parallelize with other work, or change dependencies/lockfiles/generated files: `git worktree add <path> -b <branch>` (in Claude Code, subagents can be spawned with `isolation: "worktree"`). Cheap, isolated, can be thrown away without affecting the main checkout.
2. **Acceptable — feature branch.** For a tiny, single-purpose, short-lived edit you're going to commit and PR within the same session: `git switch -c <branch>` first, *then* edit.

The "I'll just edit and switch branches afterwards" path is **forbidden**. `npm install`, `cargo build`, codegen, `mkdir`, file writes and edits, `git add` — none of these run while HEAD points at `main`. Confirm the branch (`git branch --show-current`) before the first mutating action in any session that involves changes.

If the user asks for work and you realize you're on `main`, the response is to switch off `main` first and then start — not to apologize and ask, not to "just do this one thing." Switch first, every time.

**Why this exists:** this rule has already been violated in the past — an agent ran `npm install`, modified tracked files, and created new files on `main` during a session where Josh only asked it to file a ticket. The cleanup was painful and the trust hit was worse. This rule prevents repeat occurrences regardless of context, mode, or how innocuous the change feels.

### Always clean up the worktree when work is done

Once the PR is merged (or the work is otherwise abandoned), the matching worktree must be cleaned up — never leave stale worktrees on disk. **Do not wait for the user to ask.** After a merge confirmation, the standard cleanup is:

```sh
git fetch origin --prune
git worktree remove <worktree-path>      # or `git worktree remove --force` if it has tracked changes that are now upstream
git branch -d <feature-branch>           # `-D` only after confirming the changes really shipped upstream
git pull --ff-only origin main           # bring the main checkout up to the merge commit
```

Run `git worktree list` after cleanup to confirm nothing is left behind, and surface to the user any *other* worktrees that look stale — don't silently delete those, just point them out (they may be live work the agent isn't aware of).

**Why this exists:** stale worktrees accumulate fast (one per task), each one is a duplicate checkout, and a half-stale list makes "which branch am I supposed to be on?" harder than it should be. The cleanup is two commands; do it the same turn the merge is confirmed.

## Knowledge Base

Project knowledge lives in the **`projects-knowledge` Obsidian vault**, at `~/Documents/projects-knowledge/Projects/<project-name>/`. This is a plain folder synced by Obsidian — there is no git, nothing to clone, pull, commit, or push. Just read and write the files directly.

**Workflow:**

- **At session start:** read this project's folder (below). Read ONLY that folder unless instructed otherwise — do not read other projects' folders.
- **All updates** are ordinary file writes to the vault path. No staging, no commits; Obsidian handles syncing across machines.
- **The vault path is the same on every machine.** If `~/Documents/projects-knowledge/` does not exist (cloud sandbox, remote agent, CI), there is no knowledge base in that environment — skip it silently and carry on. Never try to reconstruct it from a git remote.
- **This is the only knowledge-base location.** Ignore any older references to a `joaoh82/projects-knowledge` git repo or to `~/projects/projects-knowledge` — that repo is retired.

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

## Task management

**marvinapp is the canonical task management system.** Tasks and issues for this project belong there — not in `notes.md` TODO bullets, not in scratch files, not invented locally.

Interact with it exclusively through the `mcp__marvinapp__*` MCP tools (never shell out, never assume a web URL):

- **Project lookup** — `list_projects`, `get_project`. Resolve the target project before creating work in it; don't guess project IDs. If no matching project exists, surface that to the user rather than silently inventing one.
- **Task lifecycle** — `create_task`, `update_task`, `get_task`, `list_tasks`, `delete_task`, `add_comment_to_task`, `list_task_comments`.
- **Issue lifecycle** — `create_issue`, `update_issue`, `get_issue`, `list_issues`, `delete_issue`, `add_comment_to_issue`, `list_issue_comments`.
- **Metadata** — `list_labels`, `list_users`, `whoami`.

**When to file.** Whenever work produces a follow-up, bug, or deferred TODO that belongs to a known project, create it in marvinapp. `notes.md` remains the journal for decisions, blockers, and incident logs — it is not the action tracker.

**Status values (critical).** When updating marvinapp tasks or issues through MCP tools, use the API enum values exactly, not display labels. In particular, the review status is `in_review` (underscore), **not** `in review` (space). This applies across all projects and harnesses (Claude Code, Codex, Cursor, etc.).

**Prompt vs description field (critical).** When creating tasks or issues in marvinapp:

- **Do NOT set the `prompt` field.** Leave it empty/unset. Josh has a prompt template configured in marvinapp that injects the description into a fully-formed prompt when he clicks "generate prompt." Pre-filling the `prompt` field bypasses that template and strips the surrounding guidelines/sections he relies on.
- **Put everything in the `description` field.** All context, instructions, acceptance criteria, file paths, repro steps, links, and anything else you would otherwise write into a prompt go into `description`. Write it as if it were the body of the prompt — Markdown is fine — because that is exactly how it will be used once the template wraps it.
- This applies to both `create_task`/`update_task` and `create_issue`/`update_issue`.

## Pull requests — register every PR in marvinapp

**Every PR you open must be registered in marvinapp.** The registered open PRs are a *review queue*: other agents poll it to find work to review, so a PR that isn't registered is a PR nobody reviews. This is part of "finishing a feature," not an optional extra — the feature isn't done at `gh pr create`, it's done once the PR is registered and the linked task reflects it.

Use the `mcp__marvinapp__*` PR tools (same MCP server as tasks/issues — never shell out):

- **`register_pull_request`** — call it immediately after creating the PR. `url` is the only required field (`https://github.com/<owner>/<repo>/pull/<n>`); also pass `title`, `status` (`open` or `draft`), and link it with `projectId` plus whichever of `taskId` / `issueId` the work came from. It is **idempotent on owner/repo/number**, so re-running it is safe and is also how you correct or enrich an earlier registration.
- **`list_pull_requests`** — the review queue. Defaults to open + draft across all projects; filter with `projectId`, `repository` (`owner/repo`), `taskId`, `issueId`; pass `history=true` or `status=merged|closed` to see PRs that already left the queue.
- **`get_pull_request`** — fetch one by its marvinapp id.
- **`update_pull_request`** — keep the lifecycle honest: set `status` to `open` when a draft is marked ready, and to `merged` or `closed` the moment the GitHub PR leaves the open queue. Also use it to fix the title or re-link `projectId` / `taskId` / `issueId` (pass an empty string to clear a link).

**Always link the PR to its task or issue.** Tasks and issues carry a GitHub integration link, and it's the `taskId` / `issueId` on the PR record that populates it. An unlinked PR still sits in the queue but a reviewer has no context for it, so resolve the project and ticket first (`list_projects`, `get_task` / `get_issue`) rather than registering a bare URL.

**Keep the task in sync.** When you open a PR for a task, move that task to `in_review` (enum value, underscore — see the status note above) in the same turn you register the PR.

**Stale entries are worse than missing ones.** If you merge, close, or abandon a PR yourself, update its status right then. If you notice a registered PR whose GitHub state no longer matches (merged upstream, closed without merging), reconcile it instead of leaving the queue to rot.

## Reserved local ports

- **Port `3000` is reserved for the local Marvin API.** No other app, dev server, or script may bind to `3000`. When scaffolding a new project, choosing a dev port, or restarting something that defaults to `3000`, pick a different free port (e.g. `3001`, `5173`, `8080`, etc.). If a tool stubbornly defaults to `3000` and can't be reconfigured easily, surface it to the user rather than letting it collide with Marvin. **In this repo that means `web/`:** `next dev` defaults to 3000 — always start it with `npm run dev -- -p 3001` (or another free port).

## Engineering defaults

When starting fresh work without an established stack or conventions, consult `~/.claude/docs/engineering-defaults.md` (if available on this machine) for preferred backend / frontend / docs choices. **Always prefer the project's existing patterns over these defaults.**
