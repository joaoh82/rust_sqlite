# Design decisions

Short records of the major "why" choices in the codebase. If you read the source and something looks surprising, it's probably intentional for one of these reasons.

Decisions are grouped by the engine layer they concern: parser, storage, concurrency/durability, query execution, packaging. Within each group they're roughly chronological, so you can read this doc as a trail of the architectural moves that got us where we are.

---

## Parser

### 1. Delegate SQL parsing to `sqlparser`

**Decision.** SQL parsing is handled entirely by the [`sqlparser`](https://crates.io/crates/sqlparser) crate (SQLite dialect). The modules under `src/sql/parser/` only convert the crate's AST into trimmed-down structs we actually use.

**Why.** The project's goal is to learn database internals — storage, indexing, durability, concurrency. Writing a SQL tokenizer and recursive-descent parser is a different project and would consume time without advancing the stated goal. `sqlparser` is maintained, battle-tested, and covers enough of SQL that "the parser doesn't support this" hasn't been a blocker since Phase 1.

**Cost.** We're exposed to `sqlparser`'s API churn — the Phase 0 modernization absorbed ~10 breaking changes across 0.17 → 0.61. Coupling is isolated to `src/sql/parser/`, so upgrades are local.

**Artifact.** An empty `src/sql/tokenizer.rs` lingers as a historical placeholder. It's not used.

---

## Storage

### 2. 4 KiB page size, fixed at compile time

**Decision.** `PAGE_SIZE = 4096` in [`src/sql/pager/page.rs`](../src/sql/pager/page.rs) is a compile-time constant.

**Why.** Matches SQLite's default, typical OS page sizes, and common flash-storage block boundaries. Large enough that the ~20-byte per-page header is rounding error.

**Cost.** A variable page size would be more flexible but adds per-file configuration noise and complicates the Pager. The file header carries the page size and is validated `== 4096` on open, so a later implementation can make it configurable and still read old files.

---

### 3. Single-file database (+ WAL sidecar)

**Decision.** One `.sqlrite` file holds schema, data, and indexes. A sibling `.sqlrite-wal` file carries uncommitted + uncheckpointed frames; it's created on demand and truncated after checkpoint.

**Why.** Matches SQLite's model, which is the whole point of the project. A directory-per-database scheme would avoid some pager complexity but then moving/copying a DB becomes multi-file — usability regression.

**Cost.** The WAL sidecar complicates operations like "rename a DB" (you need to move both files together or checkpoint first). Accepted — that's the same tradeoff SQLite makes.

---

### 4. Cell-based row encoding, not `bincode` (Phase 3c, format v2)

**Decision.** Rows are stored as length-prefixed cells inside `TableLeaf` pages (see [`src/sql/pager/cell.rs`](../src/sql/pager/cell.rs) + [`table_page.rs`](../src/sql/pager/table_page.rs)). Each cell carries a varint length, a kind tag, a rowid, a column-count varint, and a tag-then-value payload with a leading null bitmap. Oversized cells spill into an overflow-page chain.

**Why.** The original Phase 2 format serialized whole `Table` structs via [`bincode` 2.0](https://crates.io/crates/bincode) — fast to build but had two hard limits: every save rewrote the whole table (no way to be selective inside a bincoded blob), and the format wasn't a real database format, just a serialized struct. Cell-based encoding makes a row a first-class unit the Pager can locate, update, and move between pages. It's what makes `INSERT ... LIMIT n` plans, B-Tree splits, and the eventual cursor API implementable.

**Cost.** ~500 lines of encoder/decoder/varint/overflow plumbing, plus a format-version bump that breaks compatibility with any pre-Phase-3c file. The format version lives in the file header so the Pager can reject old files with a clear error.

**Artifact.** `bincode` no longer appears anywhere in `Cargo.toml` — it was removed when Phase 3c shipped. Earlier drafts of this doc still mention it; treat those as historical.

---

### 5. Secondary indexes as separate B-Trees in the same file (Phase 3e, format v3)

**Decision.** Every `UNIQUE` and `PRIMARY KEY` column gets an auto-index at `CREATE TABLE` time, named `sqlrite_autoindex_<table>_<column>`. User-created indexes land via `CREATE [UNIQUE] INDEX name ON table (col)`. Each index lives as its own cell-based B-Tree in the same `.sqlrite` file, identified by a `type` column in the `sqlrite_master` schema catalog.

**Why.** Two requirements pulled in the same direction: UNIQUE enforcement needs fast equality probing (scanning the whole table on every INSERT is O(N)), and the `WHERE col = literal` optimizer needs an index to probe against. Making auto-indexes use the same machinery as explicit ones keeps the code paths unified; no special cases for "the PK" versus "a user index".

**Cost.** Index cells are `(value, rowid)` pairs — one write per indexed column per mutation. Multiplied across every UNIQUE + PK column this isn't free, but it's cheaper than a full table scan for any DB over a few thousand rows.

**Design detail.** Index cells share the same `cell_length | kind_tag | body` prefix as table cells and overflow cells. Binary search over slot directories works uniformly across all three cell kinds — see [`src/sql/pager/interior_page.rs`](../src/sql/pager/interior_page.rs).

---

### 6. B-Tree built bottom-up from sorted in-memory rows (Phase 3d)

**Decision.** `save_database` rebuilds the B-Tree from scratch on every commit: rowids are sorted, leaves are packed to fit, and interior nodes are added above as needed. `open` descends to the leftmost leaf and scans forward via the sibling `next_page` chain.

**Why.** In-place B-Tree updates (splits, merges, rebalances) are a substantial project in their own right — on the order of the whole rest of the engine. Bottom-up rebuild is O(N) in row count per commit but guaranteed correct with minimal code, and the diff-based Pager only actually writes pages whose bytes changed (see [Decision 9](#9-long-lived-pager-with-in-memory-page-snapshot--layered-reads)).

**Cost.** Saves are O(N); for a 10 M-row DB this starts mattering. Acceptable for a learning project. If it becomes a problem, in-place updates land as their own follow-up phase.

---

### 7. Runtime `Value` enum separate from storage `Row` enum

**Decision.** [`Row`](../src/sql/db/table.rs) (the in-memory per-column storage) uses `BTreeMap<i64, i32>` for Integer columns, `BTreeMap<i64, String>` for Text, `BTreeMap<i64, f32>` for Real, `BTreeMap<i64, bool>` for Bool — narrow types chosen for compactness. The [`Value`](../src/sql/db/table.rs) enum used at query-evaluation time is separate and carries `Integer(i64), Text(String), Real(f64), Bool(bool), Null` — wider types and a first-class NULL.

**Why.** The storage types pick compact representations suited to the columnar `BTreeMap`-per-column layout, while the runtime `Value` uses the widest sensible variants for arithmetic so `INTEGER + REAL` doesn't silently truncate. Keeping them separate also makes NULL a first-class runtime value without hacking around the storage's inability to hold NULL for numeric columns (NULL in the store is encoded by the cell-level null bitmap, not a `Value::Null`).

**Cost.** An extra conversion at the read/write boundary (`Row::get(rowid) → Value`, `set_value(col, rowid, Value)`). These boundaries are already the place where we're doing work, so the conversion is negligible.

**Known debt.** `Row` has been called "on-disk / in-memory" in older docs — since Phase 3c it's only in-memory. The on-disk representation is cell-based (see [Decision 4](#4-cell-based-row-encoding-not-bincode-phase-3c-format-v2)). The separation between `Row` and `Value` survived the refactor because the columnar-`BTreeMap` in-memory layout didn't change.

---

### 8. Header written last on legacy save path; WAL commit for the modern path

**Decision.** The legacy (non-WAL) `save_database` path stages every payload page first, commits them, and only after all pages are on disk does it write page 0 (the header). On open, `decode_header` rejects anything without the `SQLRiteFormat\0\0\0` magic bytes. The modern (Phase 4c+) path routes writes through the WAL — a commit frame sealing a batch of page copies is fsync'd before being considered durable.

**Why.** Best-effort crash safety with no journal (legacy path) or explicit journaling (WAL). Both paths guarantee that a crash leaves either the previous consistent state or a clearly-broken file the user can see, never a silently half-written database.

**Cost.** The legacy path does one extra header write per save (4 KiB, cheap). The WAL path adds the WAL sidecar file and the checkpointer (see [Decision 10](#10-wal-instead-of-undo-logging-for-durability-phase-4b4d)), which is considerably more machinery. The legacy path still exists because `.save FILENAME` for in-memory databases (no open pager) doesn't go through the WAL flow.

---

### 9. Long-lived `Pager` with in-memory page snapshot + layered reads

**Decision.** When a database is opened, the `Pager` reads every page into the `on_disk` map and keeps the file open. Writes during a session land in `staged`; WAL frames sit in `wal_cache` layered above `on_disk`. A read consults `staged → wal_cache → on_disk` in that order. Commit diffs `staged` against the effective committed state and appends a WAL frame with only the pages whose bytes actually changed.

**Why.** Auto-save runs after every mutating SQL statement. Without a cache the whole file would be rewritten every time — poor scaling as the DB grows. Keeping a byte snapshot in RAM lets commit skip unchanged pages, so a one-row UPDATE doesn't rewrite every table's leaves.

**Cost.** Memory usage is `O(page count)` — every page is resident even if the application isn't actively reading it. On a 10 MiB database (2500 pages) that's fine; past ~1 GiB it wouldn't be.

**Not yet done.** An LRU page cache with a bounded memory budget is a natural follow-up — would invert the "whole file in RAM" assumption. Earlier drafts of this doc claimed Phase 3d would do this; it didn't. Tracked in the roadmap as a future refactor.

---

### 10. WAL instead of undo logging for durability (Phase 4b–4d)

**Decision.** SQLRite persists durability through a SQLite-style WAL (`foo.sqlrite-wal`): each commit appends frames describing the page copies being made, sealed by a commit frame carrying the new page count + checksum. A checkpointer migrates frames back into the main file during idle windows.

**Why.** WAL is forward-friendly (appends are linear), easy to reason about (frames are contiguous byte ranges), and plays well with the Pager's "diff commit" model — the frame for a commit is exactly the diff we computed. Undo logging would need per-statement before-images and a redo semantics we don't need for single-writer workloads.

**Cost.** Checkpointing is an extra moving piece. The checkpointer holds a reader-blocking exclusive stretch while it migrates frames — acceptable under the current single-writer-or-many-readers concurrency (see [Decision 11](#11-posix-flock-for-multi-process-concurrency-phase-4a-4e)).

---

## Concurrency & durability

### 11. POSIX flock for multi-process concurrency (Phase 4a / 4e)

**Decision.** The Pager takes either an exclusive or a shared POSIX file lock via [`fs2`](https://crates.io/crates/fs2)'s `try_lock_exclusive` / `try_lock_shared` on both the main file and the WAL sidecar. Read-write openers hold `LOCK_EX`; read-only openers hold `LOCK_SH`. The non-blocking variant is used so a conflict surfaces as a clean typed error rather than a hang.

**Why.** Cross-process coordination is a required property — you can't have two REPLs mutating the same `.sqlrite` concurrently without corruption. `flock` is the simplest correct primitive that works on Linux, macOS, and (via `LockFileEx`) Windows. The alternative — a shared-memory coordination file plus a custom reader-writer protocol — is what SQLite's "WAL mode" does and it's a substantial amount of code. For a learning project with no "multiple writers must coexist" requirement, flock is the right cost/benefit.

**Cost.** POSIX flock is advisory on most filesystems and is bypassed by NFS / CIFS / some network shares. Accepted — single-machine use is the target. Reader and writer can't coexist: opening for read-write while a reader has `LOCK_SH` errors, and vice versa. That's the expected SQLite rollback-mode semantics.

---

### 12. Snapshot-based rollback via deep-clone (Phase 4f)

**Decision.** `BEGIN` deep-clones the `Database`'s in-memory tables (`Table::deep_clone` rebuilds the `Arc<Mutex<HashMap>>` so snapshot and live state don't share a map) and stashes the clone on `db.txn`. `ROLLBACK` replaces the live state with the snapshot. `COMMIT` flushes accumulated changes through the WAL as one commit frame and drops the snapshot.

**Why.** The alternative — WAL-level undo where every mutation writes an undo record, and ROLLBACK walks the undo log backwards — is powerful but requires the engine to generate reversible operations for every statement, which is substantially more code than we wanted to ship in Phase 4. Snapshot-based rollback is `O(N)` in data size at BEGIN time but trivially correct and localized to `begin/commit/rollback_transaction` methods on `Database`.

**Cost.** `BEGIN` on a big DB is slow (deep-clone is linear in total row count). Starting a transaction just to run a single read-only query is wasteful — use a plain `SELECT` instead. Savepoints (nested transactions) aren't supported because each nested level would need its own snapshot; doable, just not done yet.

**Design detail.** COMMIT-time disk failure auto-rolls-back (restores the pre-BEGIN snapshot) — leaving mid-transaction mutations in memory after a failed COMMIT would be unsafe because auto-save on the next non-transactional statement would silently publish partial work. See `src/sql/mod.rs`'s COMMIT handler for the exact flow.

---

## Query execution

### 13. `NULL`-as-false in `WHERE` clauses

**Decision.** In [`eval_predicate`](../src/sql/executor.rs), a `WHERE` expression evaluating to `NULL` is treated as `false` — the row does *not* match.

**Why.** Matches SQL's three-valued logic in spirit: `NULL` propagates through comparisons, and a `WHERE` requires a definitely-true predicate. Doing strict 3VL would mean threading an explicit `Option<bool>` / "unknown" state through the evaluator. For a query surface that doesn't have `HAVING` or aggregate post-filters, implicit coercion to `false` at the `WHERE` boundary is equivalent for every statement we execute.

**Cost.** Diverges subtly from strict SQL on edge cases involving `NULL` through `NOT` / `AND` / `OR`. If this matters later, the evaluator can be upgraded to 3VL without touching callers.

---

### 14. Deterministic page-number ordering when saving

**Decision.** [`save_database`](../src/sql/pager/mod.rs) sorts table names alphabetically before writing. Same DB contents → same bytes at same page numbers, every time.

**Why.** The Pager's diff-based commit needs *positionally* stable page contents to detect "no change". If the writer chose a random order, a table that hasn't changed might land at a different page number, marking it dirty and forcing a write. Sorting eliminates that source of spurious writes — and as a bonus, makes the format deterministic enough that two saves of the same in-memory state produce byte-identical files. Useful for testing.

**Cost.** One `Vec::sort()` per save. Negligible.

---

## Packaging

### 15. Engine split into library + binary (Phase 2.5 / 5a)

**Decision.** The root crate exposes both `[lib] name = "sqlrite"` (the engine) and `[[bin]] name = "sqlrite"` (the REPL). Downstream consumers — the Tauri desktop app, the C FFI shim, every language SDK — depend on the library. The binary uses the library like any other caller.

**Why.** The project started as "SQLite, but written in Rust" — a standalone REPL binary. But the binary model makes it impossible to ship a desktop app, a Python package, or a WASM build without re-writing the engine. Splitting the crate once and consuming it internally turned out to be cheap, and it's what unlocked Phases 2.5 (desktop), 5 (SDKs), and 6 (distribution) without touching the engine.

**Cost.** A small amount of Cargo.toml ceremony — `[lib]` and `[[bin]]` sections with `required-features = ["cli"]` on the bin so non-REPL consumers (WASM) don't pull in rustyline / clap / env_logger.

---

### 16. `Arc<Mutex<HashMap>>` for Table row storage (Phase 2.5)

**Decision.** Each `Table`'s row storage went from `Rc<RefCell<HashMap<String, Row>>>` (single-threaded) to `Arc<Mutex<HashMap<String, Row>>>` (thread-safe). The entire `Database` is `Send + Sync` as a result.

**Why.** Tauri's `State<T>` requires `Send + Sync` so the Svelte frontend's commands can touch the database from a command handler thread. `Rc<RefCell<_>>` doesn't cross threads; swapping to `Arc<Mutex<_>>` was the minimal change.

**Cost.** Every column access takes a mutex — negligible for single-writer workloads but would become real contention under concurrent writers. The engine is single-writer by design (see [Decision 11](#11-posix-flock-for-multi-process-concurrency-phase-4a-4e)), so this doesn't bite.

**Design detail.** The mutex isn't a concurrency optimization — it's a correctness requirement. Tauri commands serialize on the mutex, running one at a time against the engine.

---

### 17. Language SDKs layered over a single C FFI shim (Phase 5b–5g)

**Decision.** Every non-Rust SDK — Python (PyO3), Node.js (napi-rs), Go (cgo), C (direct) — compiles against the same `libsqlrite_c` produced by the `sqlrite-ffi` crate. The WASM SDK is the one exception; it builds against the engine directly because its target is wasm32 (no C FFI surface needed).

**Why.** Single source of truth. A fix in the engine propagates through one wrapper update per language instead of four independent reimplementations. This is the same pattern SQLite itself uses — the C library is the engine and every language binding is a shim.

**Cost.** The FFI layer has to stay C-idiomatic: opaque handles, error codes + `last_error` slots, manual memory ownership conventions. Language bindings each own the translation from idiomatic Rust-via-C into their host language's idioms (Python context managers, Node's Promises, Go's `database/sql/driver`).

---

### 18. WASM SDK as a standalone crate (not a workspace member)

**Decision.** `sdk/wasm/Cargo.toml` declares its own `[workspace]` root (an empty one). It's *not* listed in the root workspace's `members`.

**Why.** `cargo build --workspace` on a native host would try to compile every workspace member, including the wasm-only crate — which fails on targets that aren't `wasm32-unknown-unknown`. Keeping it outside the workspace lets the native build skip it cleanly; `wasm-pack build` drives the WASM-only build separately.

**Cost.** `sdk/wasm/` has its own `Cargo.lock` and doesn't share the workspace target directory. Minor operational overhead for a clear separation.

---

### 19. Lockstep versioning across all products (Phase 6)

**Decision.** Every release bumps every product's version in unison — engine, FFI, Python, Node.js, WASM, Go, desktop. A single `scripts/bump-version.sh X.Y.Z` invocation edits ten manifests plus `Cargo.lock`. The Release PR workflow automates this.

**Why.** SemVer-per-product sounds cleaner in theory but produces compatibility-matrix hell in practice: "Python 0.3.1 needs engine 0.4.0 but Node 0.2.7 only runs against engine ≤0.3.5". With lockstep versioning, "SQLRite 0.1.2" means one coherent wave across every package. Users installing two SDKs in the same project don't need to think about which versions pair.

**Cost.** A bugfix to (e.g.) the Node SDK still bumps every other product's version, even if the engine bits are unchanged. Acceptable — version numbers are cheap and the releases page makes it clear which product wave each bump corresponds to.

---

### 20. Crate name on crates.io is `sqlrite-engine`, not `sqlrite`

**Decision.** The Rust engine crate is published to crates.io as `sqlrite-engine`. The `[lib] name = "sqlrite"` and `[[bin]] name = "sqlrite"` stay unchanged, so users `cargo add sqlrite-engine` but continue to write `use sqlrite::…`. Workspace members that depend on the engine use the `package =` key:

```toml
sqlrite = { package = "sqlrite-engine", path = "…" }
```

**Why.** The short name `sqlrite` on crates.io is owned by an unrelated project (a RAG-oriented SQLite wrapper). Discovered during the v0.1.1 canary release when `cargo publish` returned 403. Renaming the *package* while preserving the *lib name* kept all source code unchanged and meant consumers writing `use sqlrite::Connection` didn't have to learn a new import.

**Cost.** `cargo add sqlrite-engine` is one extra character to type than `cargo add sqlrite` would have been. Also a permanent footnote in docs that "the crates.io name differs from the import name" — see the root `Cargo.toml`'s header comment. Trivial tradeoff for keeping the intuitive import form.

---

### 21. Two-workflow release flow (Phase 6d)

**Decision.** Releases split across two GitHub Actions workflows: `release-pr.yml` (manually dispatched, opens a Release PR with the version bumps) and `release.yml` (triggered by the Release PR merge, runs the actual publish jobs). A `release: vX.Y.Z` commit-message match on the PR merge commit is what triggers the latter.

**Why.** The Release PR is reviewable — a maintainer sees exactly what files are changing in the version bump before merging. Branch protection (required reviews) stays on for `main` without any special-casing for release commits. Publish jobs gate on the GitHub `release` environment's required-reviewer rule, giving one final approve step before anything hits crates.io / GitHub Releases.

**Cost.** More YAML. A `workflow_dispatch` fallback on `release.yml` lets a maintainer manually kick the publish side if the auto-trigger misfires (it did once — the squash-merge `(#N)` suffix initially broke the commit-message regex; fixed in PR #19).

---

## Process

### 22. Sub-phase granularity in Phase 3

**Decision.** Phase 3 is split into 3a (auto-save), 3b (Pager + diffing commits), 3c (cell-based pages), 3d (B-Tree), 3e (secondary indexes). Each is an independent commit wave.

**Why.** The full Phase 3 is ~2000 lines of code. Landing it as one patch makes review impossible and hides regressions. The sub-phases are each small enough to understand in isolation. They also provide natural decision points — 3c's cell encoding in particular is a design choice worth pausing on before committing to.

**Cost.** Every intermediate phase has to be consistent on its own, which means a bit of "throwaway" glue that later phases replace. Accepted — the educational value of shippable slices is higher than the engineering cost of rewrites.

**Later phases inherited the pattern.** Phase 4 (4a–4f), Phase 5 (5a–5g), and Phase 6 (6a–6i) all use the same sub-phase structure. It's the default way work lands in this codebase.

---

## Still to decide

These aren't decided yet — they come up when reading the code but the answer is "we haven't thought hard enough":

- **Case-sensitive identifiers.** `CREATE TABLE Users` + `SELECT * FROM users` errors today because `Database::contains_table` is a direct `HashMap` lookup. SQLite normalizes to lowercase; we should probably too, but the cursor refactor in Phase 5 is a better moment to make this change.
- **Projection expressions.** `SELECT age + 1 FROM t` isn't supported — the projection is bare column references only. Wired up at the parser level; execution path doesn't handle the `Expr` variant.
- **Composite / multi-column indexes.** Auto-indexes are single-column. `CREATE INDEX ON t (a, b)` is parsed but rejected. Needs a cell encoding that handles tuples as the indexed value.
- **Bounded page cache.** Currently the whole file sits in RAM (see [Decision 9](#9-long-lived-pager-with-in-memory-page-snapshot--layered-reads)). An LRU cache with a memory budget is the natural next step when someone tries to open a multi-GiB database.

When one of these turns into a real decision with a chosen answer, it graduates up into the numbered list above.
