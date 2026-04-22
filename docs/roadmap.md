# Roadmap

The project is staged in phases. Each phase is shippable on its own, ends with a working build + full test suite + a commit on `main`, and can be paused between. The README's roadmap section is a summary of this doc.

## ✅ Phase 0 — Modernization

*Done (commit `ce3ddd4`).*

The project sat dormant for four years. Phase 0 was the catch-up:

- Rust edition 2018 → 2024
- resolver 3, stable toolchain pinned via `rust-toolchain.toml`
- Every dep bumped to current majors: `rustyline` 9 → 18, `clap` 3 → 4, `sqlparser` 0.17 → 0.61, `thiserror` 1 → 2, `env_logger` 0.9 → 0.11, `prettytable-rs` 0.8 → 0.10, plus `serde` / `log` latest
- Ported every call site that broke: sqlparser struct-variant Statements, ColumnOption::PrimaryKey split, ValueWithSpan wrapper, DataType::Integer variant, rustyline `Editor<H, I>` generics, removed `OutputStreamType`, clap 4 Command API

The segfault in `cargo test` that came with the old `rustyline` / `nix` / `rustix` chain on modern macOS disappeared as a side effect.

## ✅ Phase 1 — SQL execution surface

*Done (commit `136e426`, with arithmetic follow-up `a19a831`).*

The engine could parse SQL but only execute CREATE and INSERT. Phase 1 finished the core surface:

- `SELECT` with projection, `WHERE`, single-column `ORDER BY`, `LIMIT`
- `UPDATE ... SET ... WHERE ...` with multi-column `SET`, type + UNIQUE enforcement at write time, arithmetic on the RHS
- `DELETE ... WHERE ...`
- Expression evaluator: `=`/`<>`/`<`/`<=`/`>`/`>=`, `AND`/`OR`/`NOT`, arithmetic `+`/`-`/`*`/`/`/`%`, string concat `||`, NULL-as-false in `WHERE`
- Every `.unwrap()` that used to panic on malformed input is now a typed error

## ✅ Phase 2 — On-disk persistence

*Done (commit `67f2ff8`).*

- Single-file database format — one `.sqlrite` per database
- 4 KiB pages; page 0 header (magic, version, page size, page count, schema-root pointer)
- Typed payload pages (`SchemaRoot` / `TableData` / `Overflow`) chained via `next`-page pointers
- Schema catalog + per-table state serialized via `bincode` 2.0
- `.open FILENAME`, `.save FILENAME`, `.tables` meta-commands
- Header written last on save, so a mid-save crash leaves the file recognizably unopenable

See [File format](file-format.md).

## Phase 3 — On-disk B-Tree + auto-save pager *(in progress)*

Split into sub-phases for manageable commits.

### ✅ Phase 3a — Auto-save

*Done (commit `2b6a4e4`).*

- Every committing SQL statement (`CREATE` / `INSERT` / `UPDATE` / `DELETE`) against a file-backed DB auto-flushes
- `.save FILE` becomes a rarely-needed manual flush
- `.open FILE` on a missing file materializes an empty DB immediately
- Clean error propagation if the save fails

### ✅ Phase 3b — Pager abstraction with diffing commits

*Done (commit `9116da3`).*

- Long-lived `Pager` struct (owns the open file, keeps a `HashMap<u32, Box<[u8; PAGE_SIZE]>>` snapshot of what's currently on disk plus a staging map for the next commit)
- Commit diffs staged vs. snapshot and writes only pages whose bytes actually changed
- File truncates when page count shrinks
- Deterministic page-number ordering (alphabetical table sort) during save, so unchanged tables produce byte-identical pages and the diff actually catches them

See [Pager](pager.md).

### ✅ Phase 3c — Cell-based page layout *(done, file format v2)*

*Five commits: `af4d851`, `a87c05c`, `e10af65`, `c28f5c9`, `2c3171e`.*

Rows are now serialized as length-prefixed, kind-tagged cells and packed into `TableLeaf` pages with a SQLite-style slot directory. Cells that exceed ~1 KB spill into a chain of `Overflow` pages. The schema catalog itself is now an internal table named `sqlrite_master`.

- **3c.1** — varint (LEB128 + ZigZag) + cell codec (tag-then-value, null bitmap)
- **3c.2** — `TablePage` with slot directory + binary-search rowid lookup + insert/delete
- **3c.3** — overflow chains for oversized cells; kind-tagged cells to dispatch between local/overflow
- **3c.4** — wire cell storage into `save_database` / `open_database`
- **3c.5** — promote schema catalog to `sqlrite_master`, bump format version to 2

### ✅ Phase 3d — Page-based B-Tree *(done)*

*Commit `be642e3`.*

Real B-Tree per table, keyed by ROWID. Leaves stay in the Phase 3c cell format; interior pages (new `PageType::InteriorNode`, tag 4) hold child-page pointers and divider keys using the same `cell_length | kind_tag | body` prefix as local/overflow cells. Save rebuilds the tree bottom-up on every commit; open descends to the leftmost leaf and scans forward via the existing sibling `next_page` chain. No in-place splits or merges (vacuum is future work). Read path is still eager-load; the cursor / lazy-load refactor is deferred to Phase 5 alongside the library-API split.

### ✅ Phase 3e — Secondary indexes *(done, file format v3)*

*Four commits: `3bc42b6`, `d8366db`, `9b9b78e` (+ docs).*

- **3e.1** — Replaced per-`Column` `Index` with a dedicated `SecondaryIndex` type on `Table`. Every UNIQUE / PK column auto-creates one at CREATE TABLE time. `Column` shrinks to pure schema.
- **3e.2** — `CREATE [UNIQUE] INDEX [IF NOT EXISTS] <name> ON <table> (<col>)`. Single-column, Integer/Text only. Reflects into `Table::secondary_indexes` and is maintained through every write path automatically.
- **3e.3** — Executor optimizer: `WHERE col = literal` (and `literal = col`, with optional outer parens) probes the matching index for an O(log N) lookup. Other predicate shapes still fall back to full scan.
- **3e.4** — Persistence. File format v3 adds a `type` column to `sqlrite_master` (first position) distinguishing `'table'` rows from `'index'` rows. Each index persists as its own cell-based B-Tree; leaf cells use the new `KIND_INDEX` encoding `(rowid, value)`. Auto- and explicit-indexes travel the same on-disk path.

## ✅ Phase 2.5 — Tauri 2.0 desktop app *(done)*

*Two commits: `4f5f211`, `741effb`.*

- **2.5.1** — Engine split into lib + bin (pulled forward from Phase 5). `sqlrite` is now both a binary (the REPL) and a library consumable from external crates.
- **2.5.2 / 2.5.3** — Tauri 2.0 workspace member under `desktop/src-tauri/`, Svelte 5 UI under `desktop/src/`. Four backend commands (`open_database` / `list_tables` / `table_rows` / `execute_sql`). Three-pane dark-themed UI: header with file picker, table-list sidebar with per-table schema, query editor + result grid. File persistence uses the engine's auto-save, so every query that mutates state hits disk before returning.
- **Engine thread-safety** — Table's row storage migrated from `Rc<RefCell<_>>` to `Arc<Mutex<_>>` so `Database` is `Send + Sync` and can live in Tauri's shared state. Serde derives on engine storage types (dead since 3c.5) dropped at the same time; `serde` and `bincode` are no longer engine deps.

Build / run: `cd desktop && npm install && npm run tauri dev`. See [docs/desktop.md](../docs/desktop.md) for details.

## Phase 4 — Durability + concurrency *(in progress)*

### ✅ Phase 4a — Exclusive file lock

Every `Pager::open` / `Pager::create` takes a non-blocking OS exclusive advisory lock via `fs2::FileExt::try_lock_exclusive` — `flock(LOCK_EX \| LOCK_NB)` on Unix, `LockFileEx` on Windows. A second process attempting to open the same file gets a clean `database '…' is already opened by another process` error. The lock is tied to the `File` handle so it releases automatically when the `Pager` drops. No WAL yet — this is the single-writer-exclusive baseline that the rest of Phase 4 builds on.

### Phase 4b — WAL file format *(next)*

- `.sqlrite-wal` sidecar with a hand-rolled frame format: each frame carries `(page_num u32, commit_flag u8, salt u32, page_size bytes)`
- WAL header: magic + format version + page size + salt + checkpoint seq

### Phase 4c — WAL-aware Pager

- Reads consult WAL first, fall back to the main file
- Writes append frames to WAL instead of mutating the main file
- Commit writes a "commit" marker frame + fsync

### Phase 4d — Checkpointer

- Apply accumulated WAL frames back into the main file
- Triggered opportunistically on commit past a threshold, or on `.save`

### Phase 4e — Multi-reader / single-writer

- Graduate from exclusive-only to shared + exclusive lock modes
- Read marks so checkpointer doesn't pull frames that active readers still depend on

### Phase 4f — Transactions

- `BEGIN` / `COMMIT` / `ROLLBACK` on top of the WAL
- Uncommitted frames stay out of reader snapshots until commit

## Phase 5 — Library, embedding, WASM

- Split into `lib` + `bin` crates
- Public `Connection` / `Statement` / `Rows` API
- **Cursor abstraction** (deferred from Phase 3d): stream rows through the B-Tree via the pager on demand instead of eagerly loading every row into the in-memory `Table`. Touches `Table::rowids`, `Table::get_value`, and the executor's row iteration. Naturally pairs with the public `Statement::query_iter` API
- C FFI shim (`libsqlrite.so` / `libsqlrite.dylib`)
- WASM build via `wasm-pack` so the engine runs in a browser

## Phase 6 — AI-era extensions *(research)*

- Vector / embedding column type with an ANN index
- Natural-language → SQL front-end (emit SQL against this engine)
- Other agent-era ideas as they emerge

## "Possible extras" not pinned to a phase

- Joins (`INNER`, `LEFT OUTER`, `CROSS`)
- `GROUP BY`, aggregates (`COUNT`, `SUM`, `AVG`, ...), `DISTINCT`, `LIKE`, `IN`, `IS NULL`
- Composite and expression indexes
- Alternate storage engines (LSM/SSTable for write-heavy workloads)
- Benchmarks against SQLite

These will slot in where they make sense — many are natural side effects of Phase 3 storage work or Phase 5's library API.
