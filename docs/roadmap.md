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

### Phase 3c — Cell-based page layout *(next)*

Replace the per-table bincode blob with variable-length row cells packed into pages. Design questions (TBD):

- Cell encoding (tag-then-value? length-prefixed? bincode per cell?)
- Offset table at page end vs. fixed slot directory at page start
- Overflow when a single row exceeds one page
- NULL representation (null bitmap per cell?)
- How the schema catalog itself is stored (still bincode? a real table?)

The existing page format (7-byte header, chaining via `next`) will survive. What changes is the content *inside* the payload area.

### Phase 3d — Page-based B-Tree

Real B-Tree per table, keyed by ROWID. Leaf pages hold cells (from 3c); interior pages hold child pointers and divider keys. Split on full page, merge on underflow.

### Phase 3e — Secondary indexes

Separate B-Trees keyed by `(indexed_value, rowid)` for every declared `UNIQUE` column. Insert/update/delete on the base table propagate to each index's tree.

## Phase 2.5 — Tauri 2.0 desktop app *(after Phase 3)*

A cross-platform GUI wrapping the engine. Originally slated before Phase 3, deferred so the desktop app demos real-time saves (Phase 3's pager) rather than explicit-save-only.

- File picker → open `.sqlrite` files
- Table browser (schema + rows grid)
- Query editor with result grid
- Written in TypeScript (web frontend) + a thin Tauri command layer that wraps the Rust engine

## Phase 4 — Durability + concurrency

- Write-Ahead Log in `<db>.sqlrite-wal`
- Checkpointer that merges the WAL back into the main file
- OS file locks (`fs2` or `fd-lock`) so multiple processes can't corrupt each other
- SQLite-style **multiple readers + single writer** via WAL mode
- Transactional ACID properties, `BEGIN` / `COMMIT` / `ROLLBACK`

## Phase 5 — Library, embedding, WASM

- Split into `lib` + `bin` crates
- Public `Connection` / `Statement` / `Rows` API
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
