# Roadmap

The project is staged in phases. Each phase is shippable on its own, ends with a working build + full test suite + a commit on `main`, and can be paused between. The README's roadmap section is a summary of this doc.

> **Active frontier (May 2026):** Phases 0–10 shipped end-to-end. After Phase 8 closed the v0.1.x cycle, the v0.2.0 → v0.9.1 wave (Phase 9, sub-phases 9a–9i) landed the SQL surface that had been parked under "possible extras": DDL completeness (DEFAULT, DROP TABLE/INDEX, ALTER TABLE), free-list + auto-VACUUM, IS NULL, GROUP BY + aggregates + DISTINCT + LIKE + IN, four flavors of JOIN, prepared statements with parameter binding, HNSW metric extension, and the PRAGMA dispatcher. Phase 10 published the SQLR-4 / SQLR-16 benchmarks against SQLite + DuckDB. **Current head: v0.9.1.** Phase 11 (concurrent writes via MVCC + `BEGIN CONCURRENT`, SQLR-22) is now in flight — the multi-connection foundation (11.1) is the first slice; see [`concurrent-writes-plan.md`](concurrent-writes-plan.md) for the full design.

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

## ✅ Phase 3 — On-disk B-Tree + auto-save pager

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

## ✅ Phase 4 — Durability + concurrency

### ✅ Phase 4a — Exclusive file lock

Every `Pager::open` / `Pager::create` takes a non-blocking OS exclusive advisory lock via `fs2::FileExt::try_lock_exclusive` — `flock(LOCK_EX \| LOCK_NB)` on Unix, `LockFileEx` on Windows. A second process attempting to open the same file gets a clean `database '…' is already opened by another process` error. The lock is tied to the `File` handle so it releases automatically when the `Pager` drops. No WAL yet — this is the single-writer-exclusive baseline that the rest of Phase 4 builds on.

### ✅ Phase 4b — WAL file format

Standalone `src/sql/pager/wal.rs` module with a 32-byte WAL header (magic `"SQLRWAL\0"`, format version, page size, salt, checkpoint seq) and fixed-size frames of `FRAME_HEADER_SIZE + PAGE_SIZE = 4112` bytes: `(page_num u32, commit_page_count u32, salt u32, checksum u32, body PAGE_SIZE)`. A commit frame is one whose `commit_page_count > 0`; dirty frames carry `0` there.

Checksum is a rolling `rotate_left(1) + byte` sum over the first 12 header bytes plus the body — order-sensitive, no external dep. On open the reader walks every frame from the start, validates checksum and salt, and builds a `(page_num → latest-committed-frame-offset)` map. Torn writes / partial trailing frames are silently truncated at the boundary; earlier valid frames survive.

Eight standalone tests cover: empty-WAL round trip, single commit frame, multi-frame latest-wins, uncommitted-frame invisibility, truncate-and-reopen, bad magic rejection, corrupt-body end-of-log detection, partial-trailing-frame handling. Not wired into the Pager yet — 4c's job.

### ✅ Phase 4c — WAL-aware Pager

The `Pager` now owns both the main `.sqlrite` file and its `-wal` sidecar. Reads consult `staged → wal_cache → on_disk` (with a page-count bounds check that hides logically-truncated pages); `commit` appends one WAL frame per dirty page and a final **commit frame** for page 0 whose body is the new encoded header and whose `commit_page_count` carries the post-commit page count. That commit frame is the only write that fsyncs. The main file is left completely untouched between checkpoints — a close / reopen round-trips the WAL via `Wal::load_committed_into`, and the decoded page-0 frame overrides the (stale) main-file header.

Five new Pager-level tests cover sidecar creation, main-file frozen-ness, shrink-via-bounds-check, WAL replay on reopen, and the diff staying effective (two identical commits produce zero dirty data frames).

### ✅ Phase 4d — Checkpointer

`Pager::checkpoint()` folds every WAL-resident page back into the main file at its proper offset, then rewrites the header, `set_len`-truncates the tail, and calls `Wal::truncate` (which rolls the salt + bumps the checkpoint seq). **Two fsync barriers** flank the header write so no reordered writeback can expose a header over stale data pages — matching SQLite's checkpoint ordering. `wal.truncate()` runs before the in-memory cache swap so a truncate failure leaves the Pager in a well-defined state. Auto-fires from `commit` once the WAL passes `AUTO_CHECKPOINT_THRESHOLD_FRAMES` (currently 100) and is also callable explicitly.

Six Pager-level tests pin the behaviour: explicit flush + WAL truncate, idempotency on repeat, shrink-then-checkpoint physically shrinks the main file, auto-threshold actually fires, the exact-threshold-crossing commit is the one that triggers, and a real mid-checkpoint crash (data pages on disk but header still stale) recovers via WAL replay.

### ✅ Phase 4e — Multi-reader / single-writer

New `AccessMode { ReadWrite, ReadOnly }` enum drives the lock mode. `Pager::open_read_only` takes a shared advisory lock (`flock(LOCK_SH | LOCK_NB)`) on both the main file and the WAL sidecar; `Pager::open` / `Pager::create` stay exclusive. Multiple read-only openers coexist; any writer excludes all readers and vice versa — POSIX flock semantics.

Library surface: `sqlrite::open_database_read_only(path, name)` mirrors `open_database`. Mutating operations on a read-only `Pager` (`stage_page`'s auto-save commit, explicit `commit`, `checkpoint`) return `General error: cannot commit: database is opened read-only` rather than panicking. Reads fall back cleanly to the main file when the WAL sidecar is absent — a read-only caller can't materialize one on its own.

REPL gained a `--readonly` / `-r` flag: `sqlrite --readonly foo.sqlrite` opens with a shared lock; attempted writes surface the read-only error.

**Read marks are not needed under this scoping.** With POSIX flock, a writer can't coexist with live readers, so the checkpointer is never asked to drop frames an active reader depends on. True concurrent reader + writer access requires a shared-memory coordination file; that's deferred as out-of-scope for Phase 4.

Four Pager-level tests: two read-only openers coexist, RW-blocks-RO and RO-blocks-RW, RO pager rejects mutations with typed errors, RO open without a WAL sidecar succeeds.

### ✅ Phase 4f — Transactions

`BEGIN` / `COMMIT` / `ROLLBACK` are now real statements, not the implicit per-statement transactions that every mutating SQL call used to run under.

- **BEGIN** deep-clones the `Database`'s in-memory tables (`Table::deep_clone` rebuilds the `Arc<Mutex<HashMap>>` so snapshot and live state don't share a map) and stashes the clone on `db.txn`. Rejects nested begins and read-only databases.
- **Auto-save suppressed** while `db.txn.is_some()` — statements mutate in memory but don't append WAL frames.
- **COMMIT** calls `save_database` once, which appends all accumulated changes as a single WAL commit frame, then clears `db.txn`. A failed save **auto-rolls-back** the in-memory state — leaving it in place would let a subsequent non-transactional statement's auto-save silently publish partial mid-transaction work.
- **ROLLBACK** restores `db.tables` from the snapshot and clears `db.txn`. Runtime errors inside a transaction (bad INSERT, UNIQUE violation) are not implicit rollbacks — the caller stays in the transaction until they explicitly `ROLLBACK` or `COMMIT`.

Reader-side semantics fall out of this for free: we're still single-writer under Phase 4e's flock, so uncommitted in-memory changes aren't visible to other processes to begin with. The "uncommitted frames stay out of reader snapshots" clause from the original roadmap is a non-concern under POSIX flock — by design, no concurrent reader exists during an open transaction.

Fourteen new tests under `src/sql/mod.rs` covering the happy paths, every rejection edge, and the trickier secondary-effects: rollback of `CREATE TABLE`, rollback of a secondary-index insert (followed by successful re-insert to prove the index was restored, not just the rows), `last_rowid` counter restoration, in-memory COMMIT without a pager, and the auto-rollback on a failed COMMIT save.

## Phase 5 — Embedding surface: public API + language SDKs

The engine is already available as a Rust library (split in Phase 2.5.1). Phase 5 turns that library into a proper cross-language embedding surface: a public Rust API that external code can rely on, a C FFI shim for non-Rust consumers, and SDKs for the four languages people actually use to embed an SQLite-like engine (Python, Node.js, Go, plus polishing the Rust crate). Capped off by a WASM build so the engine runs in a browser. Each sub-phase is shippable on its own.

### ✅ Phase 5a — Public `Connection` / `Statement` / `Rows` API *(partial)*

Foundation every language binding builds on — shape after `rusqlite` / Python's `sqlite3`:

```rust
let mut conn = Connection::open("foo.sqlrite")?;
conn.execute("INSERT INTO users (name) VALUES ('alice')")?;
let mut stmt = conn.prepare("SELECT id, name FROM users")?;
let mut rows = stmt.query()?;
while let Some(row) = rows.next()? {
    let (id, name): (i64, String) = (row.get(0)?, row.get_by_name("name")?);
    println!("{id}: {name}");
}
```

**Landed (5a.1):**
- New `src/connection.rs` with `Connection`, `Statement`, `Rows`, `Row`, `OwnedRow`, and `FromValue`. All re-exported at the crate root (`sqlrite::Connection` etc.).
- `executor::execute_select` split: `execute_select_rows` returns `SelectResult { columns, rows: Vec<Vec<Value>> }`; the existing string-rendering path is now a thin wrapper on top, so REPL/Tauri behaviour is unchanged.
- `FromValue` impls for `i64`, `f64`, `String`, `bool`, `Option<T>`, `Value`. Trait is public so downstream crates can extend it.
- `Connection::open` / `open_read_only` / `open_in_memory`; transactions flow through `execute("BEGIN")` / `execute("COMMIT")` / `execute("ROLLBACK")` with `Connection::in_transaction()` for introspection.
- `examples/rust/quickstart.rs` — runnable end-to-end walkthrough via `cargo run --example quickstart`.
- 9 new Connection tests: in-memory round-trip, file-backed persistence across connections, RO rejection, transactions, `get_by_name`, NULL → `Option<None>`, `prepare` multi-statement rejection, `query` on non-SELECT rejection, out-of-bounds index error.

**Deferred to 5a.2 (separate slice):**
- **Parameter binding** — `stmt.query(&[&30])` style. Requires touching the executor and the parser path; material enough to deserve its own commit.
- **Cursor abstraction** (deferred from Phase 3d). Today `Rows` wraps an eagerly-materialized `Vec<Vec<Value>>`. Phase 5a.2 swaps this for a lazy B-Tree walker so long SELECTs stream in O(1) memory. Touches `Table::rowids`, `Table::get_value`, and the executor's row iteration; the `Rows::next() -> Result<Option<Row>>` signature was designed up-front to accept the streaming version without an API break.

### ✅ Phase 5b — C FFI shim

New `sqlrite-ffi/` workspace crate ships `libsqlrite_c.{so,dylib,dll}` + `libsqlrite_c.a` alongside a cbindgen-generated `sqlrite-ffi/include/sqlrite.h`. Opaque-pointer types (`SqlriteConnection*`, `SqlriteStatement*`), C-style status codes (`Ok` / `Error` / `InvalidArgument` / `Done` / `Row`), thread-local last-error via `sqlrite_last_error()`. UTF-8 strings in both directions; heap-allocated C strings returned by `sqlrite_column_text` / `sqlrite_column_name` must be freed via `sqlrite_free_string`.

Split API rather than SQLite's prepare/step-for-everything: `sqlrite_execute` is fire-and-forget for DDL/DML/transactions, `sqlrite_query` returns a statement handle that yields rows via `sqlrite_step` + `sqlrite_column_int64` / `_double` / `_text` / `_is_null`. `sqlrite_in_transaction` / `sqlrite_is_read_only` expose the flags.

Crate named `sqlrite_c` (so the rlib doesn't collide with the root `sqlrite` crate; the shipped artifact is `libsqlrite_c.{so,dylib,dll}` — SDKs link against `-lsqlrite_c`). `build.rs` regenerates the header from the `extern "C"` surface on each `cargo build`.

Deliverables:
- 8 FFI-level tests covering every code path (open/execute/query/step/column_*/transactions/NULL/null-pointer/close-null-noop).
- `examples/c/hello.c` + `Makefile` — runnable end-to-end sample that opens an in-memory DB, runs CREATE/INSERT/SELECT, iterates rows, runs a BEGIN/ROLLBACK block. `make run` does the whole build-and-execute.
- `sqlrite-ffi/include/sqlrite.h` committed to the repo so downstream C consumers can grab the header without running cargo.

### ✅ Phase 5c — Python SDK

`sqlrite` module shipped via new `sdk/python/` workspace crate (PyO3 `abi3-py38` + maturin). One wheel works on every CPython 3.8+ release — no per-version rebuild. Shape follows PEP 249 / the stdlib `sqlite3` module:

```python
import sqlrite

with sqlrite.connect("foo.sqlrite") as conn:
    cur = conn.cursor()
    cur.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
    cur.execute("INSERT INTO users (name) VALUES ('alice')")
    for row in cur.execute("SELECT id, name FROM users"):
        print(row)  # tuples, not Row objects (DB-API style)
```

Landed:

- `Connection` (`connect(path)` / `connect_read_only(path)` / `":memory:"`), `Cursor` (`execute`, `executemany`, `executescript`, `fetchone`/`fetchmany`/`fetchall`, iteration, `description`, `rowcount`), context-manager support (commits on clean exit, rolls back on exception), `in_transaction` / `read_only` properties.
- `sqlrite.SQLRiteError` exception — every Rust error surfaces as this.
- Parameter binding accepts the DB-API signature but raises `TypeError` on non-empty params (deferred to Phase 5a.2, which adds real binding across the whole stack).
- Wraps the Rust `Connection` directly rather than the C FFI — PyO3 marshals types without the extra C round-trip.
- 16 pytest integration tests in `sdk/python/tests/` covering CRUD, transactions, context manager commit/rollback, file-backed persistence, read-only rejection, error paths, DB-API shortcuts, `executescript`.
- `examples/python/hello.py` runnable walkthrough after `maturin develop`.
- `sdk/python/README.md` — install, quickstart, API table, status.

Phase 6f publishes abi3-py38 wheels to PyPI via `maturin-action` (manylinux x86_64/aarch64, macOS aarch64, Windows x86_64) plus an sdist, on every release. OIDC trusted publishing — no long-lived PyPI token.

### ✅ Phase 5d — Node.js SDK

`sqlrite` module shipped via new `sdk/nodejs/` workspace crate (napi-rs 2.x, N-API v9 / Node 18+). Prebuilt `.node` binaries per platform — no `node-gyp` install dance. Shape follows `better-sqlite3`:

```js
import { Database } from 'sqlrite';

const db = new Database('foo.sqlrite');
db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
db.prepare("INSERT INTO users (name) VALUES ('alice')").run();
const rows = db.prepare("SELECT id, name FROM users").all();
// → [{ id: 1, name: 'alice' }]
```

Landed:

- `Database` class with `new Database(path)` / `Database.openReadOnly(path)` / `":memory:"`, `exec()`, `prepare()`, `close()`, `inTransaction` / `readonly` getters.
- `Statement` class with `run(params?)`, `get(params?)`, `all(params?)`, `iterate(params?)`, `columns()`. Rows come back as plain JS objects keyed by column name.
- `RunResult` object (`{ changes, lastInsertRowid }`) — both 0 for now since the engine doesn't track those at the public API layer; shape reserved so upgrading doesn't break callers.
- Auto-generated `index.d.ts` TypeScript definitions from the Rust source via napi-rs.
- Sync API, not async — engine is in-process and most ops finish in microseconds.
- Wraps the Rust `Connection` directly (not via the C FFI).
- Parameter binding accepts `undefined` / `null` / `[]` for forward compat; non-empty arrays throw until Phase 5a.2.
- 11 Node.js integration tests using Node 18+'s built-in `node:test` runner covering CRUD, transactions, file-backed persistence, read-only rejection, error paths, closed-DB rejection, `columns()`, `get`/`all`/`iterate`.
- `examples/nodejs/hello.mjs` runnable walkthrough.
- `sdk/nodejs/README.md` — install, quickstart, API table, status.

Phase 6g publishes prebuilt `.node` binaries to npm under the `@joaoh82/sqlrite` scope via the napi-rs GitHub Action (Linux x86_64/aarch64, macOS aarch64, Windows x86_64). OIDC trusted publishing with sigstore provenance attestations — no `NPM_TOKEN` in the repo.

### ✅ Phase 5e — Go SDK

New `sdk/go/` directory ships a Go module at `github.com/joaoh82/rust_sqlite/sdk/go`. Unlike Python and Node (which bind Rust directly), Go goes through the C ABI from Phase 5b via cgo — Go's FFI story is cgo-shaped, so leveraging the existing `libsqlrite_c.{so,dylib,dll}` is both natural and free.

```go
import (
    "database/sql"
    _ "github.com/joaoh82/rust_sqlite/sdk/go"
)

db, _ := sql.Open("sqlrite", "foo.sqlrite")
db.Exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
rows, _ := db.Query("SELECT id, name FROM users")
for rows.Next() {
    var id int64; var name string
    rows.Scan(&id, &name)
}
```

Landed:

- Implements the full `database/sql/driver` surface: `Driver`, `Conn`, `Stmt`, `Rows`, `Tx`, plus context-aware variants (`ConnBeginTx`, `ExecerContext`, `QueryerContext`, `StmtExecContext`, `StmtQueryContext`, `Pinger`).
- `sqlrite.DriverName = "sqlrite"` registered at package init; `_ "github.com/joaoh82/rust_sqlite/sdk/go"` is all users need.
- `sqlrite.OpenReadOnly(path)` side door since `database/sql.Open` doesn't carry a read-only flag. Returns a regular `*sql.DB` backed by a custom `driver.Connector`.
- cgo wiring: `#cgo CFLAGS: -I${SRCDIR}/../../sqlrite-ffi/include` + `LDFLAGS: -L…/target/release -lsqlrite_c` with an embedded rpath so `go run` / `go test` work without `DYLD_LIBRARY_PATH` dance.
- Column type detection in `Rows.Next` tries `int64 → double → text` accessors in order, picking the first non-erroring one. Engine returns Bool/Int/Real via their Display through `sqlrite_column_text` as a catch-all.
- 9 `go test` integration tests covering CRUD + `QueryRow` + `Columns()` + transactions commit/rollback + file-backed persistence across reopens + `OpenReadOnly` + bad-SQL + parameter-binding rejection.
- Runnable `examples/go/hello.go` with its own `go.mod` + `replace` directive at `examples/go/`.

Prerequisites for building from source: `cargo build --release -p sqlrite-ffi` to materialize `libsqlrite_c`. Phase 6i ships prebuilt `libsqlrite_c` tarballs as GitHub Release assets on every release at `sdk/go/v<V>`, so end users consuming the Go module don't need the Rust toolchain.

Phase 6i tags `sdk/go/v<V>` (slash-bearing submodule tag — Go's convention for module paths with subpaths) on every release, so `go get github.com/joaoh82/rust_sqlite/sdk/go@vX.Y.Z` resolves via proxy.golang.org as soon as the tag is pushed — no central registry push needed for Go.

### Phase 5f — Rust crate polish *(deferred — Phase 6c companion)*

The Rust library is already shippable — this sub-phase adds crate metadata, docs.rs config, a `Connection`-oriented quickstart, and prep for the `cargo publish` step. Deferred because it's mostly metadata work that makes more sense alongside the actual publish workflow in Phase 6c. Examples under `examples/rust/` already exist from Phase 5a.

### ✅ Phase 5g — WASM build

New `sdk/wasm/` crate (standalone, not in the Cargo workspace — wasm-only crates trip `cargo build --workspace` on native hosts). Compiles the Rust engine straight to `wasm32-unknown-unknown` via `wasm-bindgen`. Engine runs entirely in the browser tab.

```js
import init, { Database } from '@joaoh82/sqlrite-wasm';
await init();

const db = new Database();
db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
db.exec("INSERT INTO users (name) VALUES ('alice')");
const rows = db.query("SELECT id, name FROM users");
// → [{ id: 1, name: 'alice' }]
```

Landed:

- **Feature-gated engine**: root crate's `rustyline` / `rustyline-derive` / `clap` / `env_logger` moved behind a `cli` feature (default-on), `fs2` behind a `file-locks` feature (default-on). WASM depends with `default-features = false` so neither pulls in. `[[bin]]` has `required-features = ["cli"]` so a minimal build skips the REPL entirely. Pager's `acquire_lock` stubs out to a no-op under `#[cfg(not(feature = "file-locks"))]`.
- **`Database` class** exposed via wasm-bindgen: `new Database()` (in-memory only), `exec(sql)`, `query(sql) → Array<Object>`, `columns(sql) → Array<string>`, `inTransaction` / `readonly` getters, `free()` for explicit GC.
- **Rows as plain JS objects** in projection order — `serde_wasm_bindgen::Serializer::serialize_maps_as_objects(true)` + `serde_json`'s `preserve_order` feature. Matches the Node.js SDK shape so callers don't have to learn a different row format.
- **Panic hook** (default-on feature) routes Rust panics to `console.error` with a real stack trace; costs ~4 KiB.
- **Three build targets** via `wasm-pack build --target {web,bundler,nodejs}`. Release profile tuned for size (`opt-level = "z"`, LTO, single codegen unit, stripped debuginfo). `.wasm` ~1.8 MB uncompressed / ~500 KB gzipped.
- **Browser demo** at `examples/wasm/` with a self-contained HTML SQL console. `make build && make serve` spins it up on `localhost:8080`.

**Scope of MVP:**
- In-memory only. OPFS-backed persistence is a natural follow-up — browser file locks + WAL don't map to a tab sandbox.
- No prepared-statement object at the JS boundary; `db.query(sql)` is one-shot. The engine still does prepare/execute internally.
- Parameter binding deferred to 5a.2 (same as every other SDK).

Phase 6h publishes `@joaoh82/sqlrite-wasm` to npm via `wasm-pack build` + `npm publish` (OIDC trusted publisher) on every release.

## Phase 6 — Release engineering + CI/CD

Once Phase 5 landed artifacts in five distribution channels (crates.io, PyPI, npm, Go modules, GitHub Releases for WASM + desktop), Phase 6 automates the release pipeline end-to-end via GitHub Actions.

**Approach**: lockstep versioning (one bump, one PR, all products) with a two-workflow design that respects branch protection. Full plan + rationale in [release-plan.md](release-plan.md).

### ✅ Phase 6a — `scripts/bump-version.sh`

One script that rewrites the version string across every product's manifest in a single pass — seven TOML files (root `Cargo.toml`, sub-crate `Cargo.toml`s, `sdk/python/pyproject.toml`) and three JSON files (two `package.json`s + `tauri.conf.json`) = ten manifests edited per release. `Cargo.lock` refreshes via `cargo build` after the script runs, making eleven files total in the release diff.

Uses line-anchored `sed` (both BSD + GNU flavors) — no `jq` dependency, no Python, portable to every CI runner and dev machine. Validates the input against the semver regex (`X.Y.Z[-prerelease][+build]`); rejects `foo`, `0.2`, `0.2.0.5` cleanly. Idempotent: running twice with the same version is a no-op; running with a different version lands on the second. A verify pass at the end confirms every file actually updated, catching future refactors (e.g., someone reformats a JSON file to 4-space indent) that would otherwise silently no-op.

Used by:
- Humans, locally: `./scripts/bump-version.sh 0.2.0 && cargo build && git diff` rehearses the bump without GitHub.
- The Phase 6d release workflow, on `workflow_dispatch` — the commit that the Release PR contains.

After the Phase 6a commit lands, full test suite still passes at bumped version `0.1.1` with zero code changes beyond the manifests themselves (verified end-to-end before back-out).

### ✅ Phase 6b — `ci.yml`

Runs on every PR + push to main. Seven parallel jobs with caching via `Swatinem/rust-cache` + `actions/setup-*` built-in caches for fast PR turnaround:

- **rust-build-and-test** — Linux / macOS / Windows matrix; `cargo build --workspace --exclude sqlrite-desktop --all-targets` + `cargo test`
- **rust-lint** — ubuntu only; `cargo fmt --check`, `cargo clippy`, `cargo doc --no-deps`
- **python-sdk** — Linux / macOS / Windows matrix; creates a venv + sets `VIRTUAL_ENV` so `maturin develop` works cross-platform, then `pytest`
- **nodejs-sdk** — Linux / macOS / Windows matrix; `npm ci && npm run build && npm test`
- **go-sdk** — Linux / macOS matrix (Windows skipped — Go cgo on Windows needs mingw, deferred); `cargo build --release -p sqlrite-ffi` + `go test -v ./...`
- **wasm-build** — ubuntu only; `wasm-pack build --target web --release` + `.wasm` size reported as a GitHub notice
- **desktop-build** — ubuntu only; installs Tauri Linux deps (webkit2gtk, appindicator, rsvg, patchelf), `npm ci && npm run build` for the frontend, then `cargo build -p sqlrite-desktop`. Other platforms covered in the Phase 6e desktop-release matrix.

Pre-existing clippy warnings (~24, mostly cosmetic — overindented docstrings, `Vec::new() + push` patterns, `&Vec<T>` vs `&[T]`, `assert!(false)` in tests) stay as warnings rather than errors. Hard clippy errors (deny-by-default lints like `approx_constant`) still block. A follow-up task will clean up the warnings and then flip on `-D warnings` at the workflow level.

One pre-existing warning fixed inline during Phase 6b: a `3.14` test constant in `src/sql/pager/cell.rs` that clippy's `approx_constant` lint (deny-by-default) flags as a PI lookalike. Swapped for `2.5`.

### ✅ Phase 6c — Trusted-publisher + branch-protection runbook

One-time non-code setup — the state lives in registry web UIs + GitHub settings, not in this repo. Documented top-to-bottom in [`docs/release-secrets.md`](release-secrets.md) so future-you isn't re-discovering it at 2am:

1. **crates.io API token** → `CRATES_IO_TOKEN` in the `release` environment's secrets (crates.io doesn't support OIDC yet, so this is the only long-lived token in the pipeline).
2. **PyPI trusted publisher** pointed at `release.yml` / environment `release` — short-lived OIDC tokens, no secret to leak.
3. **npm trusted publishers** for both `@joaoh82/sqlrite` (the Node binding) and `@joaoh82/sqlrite-wasm` (the browser binding). Both scoped because npm rejected the unscoped `sqlrite` and the WASM stem also risks the same similarity check against `sqlite-wasm`. Scoped packages under your own user scope auto-own the name; npm-side trusted-publisher config still requires the package to exist first (publish a `0.0.0` placeholder via `npm login` + `npm publish --access public` in a temp dir, then add the trusted publisher on the package's settings page). See `docs/release-secrets.md` §3 for the full flow + the gotchas we hit.
4. **GitHub `release` environment** — required reviewer (maintainer), `main`-only deployments, scoped secrets. Acts as a second human-in-the-loop gate after the Release PR merge but before any registry write.
5. **Branch protection on `main`** — require 14 CI status checks green + 1 review + conversation resolution. Admin bypass left available for emergencies.

The runbook (now historical — Phase 6d–6i all landed) was safe to execute as soon as Phase 6c shipped; the PyPI + npm trusted-publisher entries point at `release.yml` and sat idle until Phase 6d wired up the first workflow run.

### ✅ Phase 6d — `release-pr.yml` + skeleton `release.yml`

Two new workflows under `.github/workflows/`:

**`release-pr.yml`** (dispatch → PR):
- `workflow_dispatch` with a `version` input (required, semver-validated).
- Validates: rejects downgrades, rejects reuse of an existing `v*` tag.
- Creates branch `release/vX.Y.Z`, runs `scripts/bump-version.sh`, refreshes `Cargo.lock` via `cargo build --workspace --exclude sqlrite-desktop`.
- Commits with the exact message `release: vX.Y.Z` (load-bearing — the publish workflow matches on it).
- Pushes the branch, opens a PR titled `Release vX.Y.Z` with a body documenting what the merge will trigger.
- Uses the `github-actions[bot]` identity for the commit; default `GITHUB_TOKEN` for push + PR-open (no extra secrets).

**`release.yml`** (merge → tag + publish):
- Triggers on `push: branches: [main]` with a first-step check of the HEAD commit message: if it matches `^release: v<semver>$`, proceed; else exit silently (so every non-release push to main no-ops cleanly).
- Also reachable via `workflow_dispatch` for manual re-runs after partial failures (e.g., transient wheel-upload flake; re-dispatch at the same version).
- Concurrency group `release` — one publish at a time, no parallel clobbering.

Jobs wired up in Phase 6d:

1. **detect** — parse version from commit message or dispatch input. Outputs `version` + `should_release`.
2. **tag-all** — idempotent: creates `sqlrite-vX.Y.Z`, `sqlrite-ffi-vX.Y.Z`, and umbrella `vX.Y.Z`; skips any tag that already exists so "Re-run failed jobs" works cleanly after a partial-failure scenario.
3. **publish-crate** — `cargo publish -p sqlrite-engine --no-verify` using `CRATES_IO_TOKEN` from the `release` environment (required-reviewer gate applies). Creates the per-product GitHub Release `sqlrite-vX.Y.Z`. The crates.io name is `sqlrite-engine` because the short `sqlrite` name was taken by an unrelated project; the `[lib] name = "sqlrite"` preserves `use sqlrite::…` at the import site.
4. **publish-ffi** — matrix build of `libsqlrite_c` on Linux x86_64 (`ubuntu-latest`), Linux aarch64 (`ubuntu-24.04-arm`), macOS aarch64 (`macos-latest`), Windows x86_64 (`windows-latest`). Packages the cdylib + staticlib + `sqlrite.h` + README stub into a tarball, uploads to the `sqlrite-ffi-vX.Y.Z` GitHub Release. macOS universal (x86_64 + aarch64 lipo'd together) is a follow-up — MVP ships aarch64-only for Mac; add `macos-13` to the matrix if x86 demand materializes.
5. **finalize** — creates the umbrella `vX.Y.Z` GitHub Release with GitHub's native auto-generated notes (`generate_release_notes: true`). Body links to every per-product release from this wave.

Products whose publish jobs land in later phases (desktop, Python, Node.js, WASM, Go) aren't tagged yet — `tag-all` only creates tags for products that have an active publish job. Cleaner than creating empty releases for products we can't actually ship.

**Verification path**: push this branch → merge → dispatch `release-pr.yml` with version `0.1.1` → review the auto-opened PR → merge → approve the `release` environment prompt → watch crates.io show `sqlrite-engine 0.1.1` + Release page show two per-product releases + umbrella release. Once that works end-to-end, 6e lands the desktop publish, and we bump to `v0.1.2` for the next canary.

> **v0.1.1 canary retrospective** *(2026-04-22)* — first publish attempt failed on `cargo publish` with a 403 because the `sqlrite` crate name on crates.io is owned by an unrelated RAG-SQLite project. Renamed the package to `sqlrite-engine` (lib / bin names unchanged, so `use sqlrite::…` still works for consumers). Tags `sqlrite-v0.1.1` / `sqlrite-ffi-v0.1.1` / `v0.1.1` stay on main per the never-reuse-a-tag policy; the next canary cuts `v0.1.2` under the new crate name.

> **v0.1.2 canary success** *(2026-04-23)* — end-to-end pipeline validated. `sqlrite-engine 0.1.2` landed on crates.io; `sqlrite-v0.1.2` / `sqlrite-ffi-v0.1.2` / `v0.1.2` GitHub Releases all live. One hiccup: GitHub's squash-merge default title (`release: v0.1.2 (#18)`) didn't match `detect`'s anchored regex, so the auto-trigger skipped and we kicked `release.yml` via `workflow_dispatch` as a manual fallback. [PR #19](https://github.com/joaoh82/rust_sqlite/pull/19) fixes that by stripping `(#N)` before the regex test — future canaries auto-publish without the manual kick.

### ✅ Phase 6e — Desktop publish

Adds `publish-desktop` job to `release.yml`. [`tauri-apps/tauri-action@v0`](https://github.com/tauri-apps/tauri-action) builds for Linux (AppImage + deb, x86_64 on ubuntu-22.04 for broad glibc compat), macOS (dmg, aarch64 — matching the publish-ffi matrix), Windows (msi, x86_64). Unsigned — signing is Phase 6.1.

Icons are pre-generated via `npx tauri icon desktop/src-tauri/icons/icon.png` and committed to `desktop/src-tauri/icons/` (one source PNG → .icns + .ico + size-specific PNGs + mobile assets). That keeps CI deterministic and saves ~10s per matrix cell; the tradeoff is that changing `icon.png` requires re-running `tauri icon` locally and committing the regenerated assets.

Release assets land on the `sqlrite-desktop-vX.Y.Z` GitHub Release with a body that explains the unsigned-installer warnings (macOS Gatekeeper / Windows SmartScreen) and how to bypass them until Phase 6.1 lands.

Follow-ups: macOS universal (x86_64 + aarch64 lipo'd — adds one Rust target build + `lipo` step), Linux aarch64 AppImage (adds one matrix cell on `ubuntu-24.04-arm`).

### ✅ Phase 6f — Python SDK publish

Adds three jobs to `release.yml` — `build-python-wheels` (matrix), `build-python-sdist` (single), `publish-python` (aggregator + PyPI upload + GitHub Release).

**Two-job shape (build then publish), not one matrix job with inline upload**, because PyPI expects wheels as a single batch — racing uploads from per-platform matrix cells would leave PyPI with a partial wave if any one cell failed. Artifacts from every matrix cell land in a single aggregated `dist/` directory, which is then atomically uploaded by `pypa/gh-action-pypi-publish`.

Wheel matrix mirrors publish-ffi + publish-desktop: Linux x86_64 (manylinux2014 via the `auto` preset), Linux aarch64 (same preset on `ubuntu-24.04-arm`), macOS aarch64, Windows x86_64. abi3-py38 means one wheel per platform works on every CPython ≥ 3.8 — no per-Python-version axis. An sdist is built alongside for platforms not covered by the wheel matrix.

Authentication via PyPI trusted publishing (OIDC) — zero long-lived tokens. `permissions: id-token: write` on the publish job plus the `release` GitHub environment (one-time trusted-publisher config on PyPI's web UI, documented in `docs/release-secrets.md`).

### ✅ Phase 6g — Node.js SDK publish

Adds two jobs to `release.yml` — `build-nodejs-binaries` (matrix of 4 platforms) + `publish-nodejs` (aggregator + npm upload + GitHub Release).

**Bundled-binaries architecture**: the main `sqlrite` npm package ships every platform's `.node` binary inside one tarball (~15 MiB), not the per-platform optional-dep packages `@napi-rs/*` projects use. Simpler for an MVP (one npm publish, one package to manage); the tradeoff is a bigger install, acceptable for a database driver people install once. The `index.js` dispatcher napi generates picks the right binary at require time via `process.platform` + `process.arch`.

Same build/publish split as publish-python — matrix cells upload `.node` artifacts, a single aggregator job downloads everything into `sdk/nodejs/`, runs `npm publish --provenance` once. `--provenance` attaches a sigstore-signed attestation linking the published package to this exact workflow run (npm's equivalent of PyPI's PEP 740).

Authentication via npm OIDC trusted publishing — zero long-lived `NPM_TOKEN`. One-time trusted-publisher registration on npmjs.com, documented in `docs/release-secrets.md`.

### ✅ Phase 6h — WASM publish

Adds a single `publish-wasm` job to `release.yml` (no per-platform matrix — WebAssembly is one universal artifact). `wasm-pack build --target bundler --scope joaoh82 --release` produces `sdk/wasm/pkg/` containing the `.wasm` binary, JS glue, TypeScript types, and an auto-generated `package.json` with `name: "@joaoh82/sqlrite-wasm"`. `npm publish --access public --provenance` then uploads via the same OIDC trusted-publisher flow as `publish-nodejs`.

**Scoped (`@joaoh82/sqlrite-wasm`) preemptively** — the unscoped `sqlrite-wasm` is currently free on npm but the similarity check that rejected `sqlrite` (vs `sqlite`) might also reject `sqlrite-wasm` (vs `sqlite-wasm`, distance 1). Going scoped from day one matches the Node SDK and avoids the rename dance we did for it in PR #30.

**Build target = `bundler`** ships JS modules + `.wasm` that webpack/vite/rollup/parcel users can consume directly. `web` / `nodejs` / `deno` targets can be added as siblings later if there's demand; one target per package is the simpler MVP shape.

The `.wasm` binary is also attached to the `sqlrite-wasm-vX.Y.Z` GitHub Release for users who want a download link rather than going through npm.

`docs/release-secrets.md` §3 now covers both scoped npm packages with the bootstrap-then-add-trusted-publisher flow we settled on after the v0.1.5–v0.1.7 publish-nodejs debugging cycle.

### ✅ Phase 6i — Go SDK publish

Adds `publish-go` job. **No registry to publish to** — Go modules pull straight from VCS via tag (`go get …@vX.Y.Z` resolves the moment the tag is on GitHub, modulo proxy.golang.org cache lag). The job's actual work:

1. Verifies `tag-all` pushed `sdk/go/v<V>` (the slash-bearing submodule tag Go modules require for the path `github.com/joaoh82/rust_sqlite/sdk/go`).
2. Downloads the per-platform `libsqlrite_c-*.tar.gz` tarballs that `publish-ffi` already uploaded to its release.
3. Re-attaches them to a fresh Go GitHub Release at the `sdk/go/v<V>` tag, so Go users have one page with both the `go get` instructions AND the cgo dependency tarballs.

The release body documents the cgo wiring (`CGO_CFLAGS` / `CGO_LDFLAGS` / `LD_LIBRARY_PATH` per platform).

**Why this can't fail in interesting ways:** no registry auth, no OIDC, no cross-platform build matrix, no npm-similarity-check theater. Just a tag check + a file download + a release create. The big hidden cost was getting the *upstream* (publish-ffi) right months earlier; this job is mostly orchestration on top.

With 6i done, **Phase 6 is complete** — every product distribution channel ships on every release with one human action (Release PR review + merge).

### Phase 6.1 — Code signing *(follow-up)*

Desktop installers from Phase 6e ship unsigned. Phase 6.1 adds code signing:
- macOS: Apple Developer ID cert → `codesign` + notarization via `xcrun notarytool` in `tauri-action`.
- Windows: code-signing cert → `signtool` via `tauri-action`.
- Involves procurement (Apple Developer $99/yr, Windows EV cert ~$300/yr) and secret management — both are separate ops tasks.

Separate phase because the code changes are tiny (just tauri-action flags) but the procurement story is long-lived.

## Phase 7 — AI-era extensions *(approved 2026-04-26 — see [phase-7-plan.md](phase-7-plan.md))*

The full plan + recorded design decisions live in [`docs/phase-7-plan.md`](phase-7-plan.md). Short version: turn SQLRite from "small SQLite clone" into "small SQLite clone that's pleasant to use from an LLM agent" by adding the storage + query primitives that modern AI workloads need (vectors, JSON), the surface that LLMs naturally drive (an MCP server), and `ask()` as a first-class natural-language → SQL API across every product (REPL, library, SDKs, desktop, MCP).

Approved sub-phases (Q1–Q10 resolved):

- **✅ 7a — `VECTOR(N)` column type** *(v0.1.10)* — dense fixed-dimension f32 storage via the existing cell encoding; format bumped to v4. Bracket-array literal syntax `[0.1, 0.2, …]` (Q7).
- **✅ 7b — Distance functions** *(v0.1.11)* — `vec_distance_l2/cosine/dot`, plus the ORDER BY-expressions parser change so KNN queries work end-to-end. Operators (`<->` `<=>` `<#>`) deferred to **7b.1** — sqlparser doesn't parse them natively, contradicting Q6's "tiny parser change" assumption.
- **✅ 7c — Brute-force KNN executor optimization** — bounded `BinaryHeap` of size k for `ORDER BY <expr> LIMIT k`. ~1.8× faster than full-sort at N=10k for cheap keys; bigger gains on expensive keys like `vec_distance_l2`.
- **✅ 7d — HNSW ANN index** — three PRs: 7d.1 (algorithm w/ recall@10 ≥ 0.95), 7d.2 (SQL integration + query optimizer), 7d.3 (persistence + DELETE/UPDATE rebuild). `CREATE INDEX … USING hnsw (col)`; fixed defaults `M=16, ef_construction=200, ef_search=50` (Q2). New `KIND_HNSW` cell tag.
- **✅ 7e — JSON column type + path queries** — `JSON` data type stored as canonical text (validated via `serde_json::from_str` at INSERT/UPDATE time; SQLite-JSON1-style — Q3 scope correction since bincode was removed in Phase 3c). Functions: `json_extract` / `json_type` / `json_array_length` / `json_object_keys`. Path subset supports `$`, `.key`, `[N]`, chained. `json_object_keys` returns a JSON-array text rather than a table-valued result (no set-returning functions in the executor yet).
- **7f — ~~Full-text search with BM25~~** — **deferred to Phase 8** (Q1).
- **7g — `ask()` API across the product surface** — natural-language → SQL via Anthropic API (Q4), Anthropic-first then OpenAI + Ollama follow-ups. Foundational **✅ 7g.1** introduces a new `sqlrite-ask` crate (Q10 — separate crate, not a feature flag) — `ask_with_schema()` over `&str` inputs (Phase 7g.2 made it pure — see retrospective below), sync `ureq` POST to `/v1/messages`, schema-aware prompt with prompt-caching on the schema dump (Sonnet 4.6 default; configurable). **✅ 7g.2** wires the REPL's `.ask` meta-command (`MetaCommand::Ask(String)` + confirm-and-run UX) and adds the `sqlrite::ask` module on the engine side (gated under a new `ask` feature) carrying `ConnectionAskExt` + the schema introspection helper. **✅ 7g.3** adds the desktop "Ask…" composer (slide-in panel above the editor; Tauri command runs the LLM call in the Rust backend so the API key stays out of the webview). **✅ 7g.4** ships the Python SDK surface — `conn.ask(question, config=None)` returns an `AskResponse(.sql, .explanation, .usage)`; `conn.ask_run()` adds the one-shot generate-and-execute convenience; `AskConfig` carries the three-layer precedence (per-call > per-connection > env > defaults). **✅ 7g.5** ships the Node.js SDK surface — `db.ask(question, config?)`, `db.askRun(question, config?)`, `db.setAskConfig(cfg)`, `new AskConfig({apiKey, model, maxTokens, cacheTtl, baseUrl})` + `AskConfig.fromEnv()`. Same three-layer precedence; idiomatic JS camelCase option-object. **✅ 7g.6** ships the Go SDK surface via cgo — `sqlrite.Ask(db, q, *AskConfig)` / `AskRun(...)` plus `AskContext`/`AskRunContext` for context-aware variants. The FFI grew one new C function (`sqlrite_ask`) that takes the config as a JSON string and returns the response as JSON — smaller, more extensible ABI than plumbing 6+ struct fields across cgo. **✅ 7g.7** ships the WASM SDK with the JS-callback shape per Q9 — `db.askPrompt(q, opts?)` returns the LLM-API request body, JS caller routes through their own backend, `db.askParse(rawResponse)` returns `{sql, explanation, usage}`. Required structurally: `sqlrite-ask` got an `http` feature flag (default-on, off for wasm); engine's `sqlrite::ask::schema` un-gated so wasm-safe consumers can introspect schemas without the HTTP transport; `sqlrite_ask::parse_response` made public. The remaining 7g.8 covers the MCP `ask` tool — folded in alongside the SDK README catch-up for VECTOR / JSON / HNSW capabilities.
- **✅ 7h — MCP server adapter (`sqlrite-mcp`)** *(this wave)* — new workspace-member crate + `[[bin]]`, hand-rolled JSON-RPC 2.0 over line-delimited JSON on stdio (no tokio, no third-party MCP framework — same dep-frugal theme as `sqlrite-ask`'s hand-rolled JSON over `ureq`). Seven tools: `list_tables`, `describe_table`, `query`, `execute`, `schema_dump`, `vector_search`, plus `ask` as Phase **✅ 7g.8** behind a default-on `ask` cargo feature (folded into the same wave). `--read-only` mode hides `execute` from `tools/list`. The whole binary is ~1100 LOC + 16 integration tests. **Critical implementation detail:** the engine's `process_command` calls `print!`/`println!` for REPL-convenience output (CREATE-table schema dump, INSERT row dump, SELECT result table) — those writes would corrupt the JSON-RPC protocol channel. Solved with a `dup2(2, 1)` dance at process startup that redirects fd 1 to fd 2; JSON-RPC responses go through a saved-off duplicate of the original fd 1 (`sqlrite-mcp/src/stdio_redirect.rs`). The same pollution affects the existing SDKs but isn't visible there because their stdout doesn't matter — fixing it in the engine is a future cleanup. Per-platform tarballs land on the GH Release page alongside the existing FFI artifacts; crate publishes to crates.io as `sqlrite-mcp`. See [`docs/mcp.md`](mcp.md) for wiring into Claude Code / Cursor / `mcp-inspector`.

Total scope budget: ~3-4 kLOC of new Rust across the wave. Each sub-phase ships as its own PR + release wave through the Phase 6 pipeline. The Phase 7 wave will likely close out **v0.2.0** (first minor bump after the 0.1.x Phase 6 cycle). Two new product lines added to lockstep versioning: `sqlrite-ask` and `sqlrite-mcp`.

> **v0.1.17 partial-publish retrospective** *(2026-04-29)* — first wave to ship `sqlrite-ask` as a brand-new product line. 23/25 jobs succeeded — `sqlrite-engine 0.1.17` landed on crates.io alongside Python / Node / Go / WASM / FFI / Desktop, and the umbrella `v0.1.17` tag exists. Two jobs failed: `publish-ask` and the `finalize` step that depends on it. Root cause: `cargo publish` rejects path-deps that don't carry a `version` requirement, with `error: dependency 'sqlrite-engine' does not specify a version`. We hit it because `sqlrite-ask` is the **first crate-besides-the-engine to actually publish to crates.io** — `sqlrite-ffi` only ships GitHub Release tarballs, so it never tripped the same check. Fixed in PR #58 by adding `version = "0.1"` (caret-compatible across 0.1.x — no per-release update) to the path-dep declaration. Verified locally with `cargo publish -p sqlrite-ask --dry-run --allow-dirty`. **`sqlrite-ask 0.1.17` will not exist on crates.io** per the never-reuse-a-version policy; the next canary cuts `v0.1.18` and ships `sqlrite-ask` for the first time there. Tags `sqlrite-ask-v0.1.17` and `v0.1.17` stay on `main` per the never-reuse-a-tag policy.

> **v0.1.19 dep-direction flip retrospective** *(2026-04-30)* — Phase 7g.2 wired the REPL's `.ask` meta-command, which required the engine binary to call into `sqlrite-ask`. That created a cargo cycle: `sqlrite-engine[bin] → sqlrite-ask → sqlrite-engine[lib]` (because `sqlrite-ask` 0.1.18 imported `sqlrite::Connection` for `ConnectionAskExt`). Cargo's static cycle detection counts every edge in the graph regardless of features, so `optional = true` didn't help — the cycle is rejected even when nobody actually exercises both directions at once. The fix flipped the dep direction structurally: `sqlrite-ask` 0.1.19 dropped `sqlrite-engine` entirely and became pure over `&str` schemas (canonical API: `ask_with_schema(schema_dump, question, &cfg)`). The engine integration (`schema::dump_schema_for_database`, `ConnectionAskExt`, `ask`, `ask_with_database`) moved into a new `sqlrite::ask` module gated by a fresh `ask` feature on `sqlrite-engine`. Default-on for the CLI binary; off for the WASM SDK and any `default-features = false` lib embedding. **Breaking change for `sqlrite-ask` 0.1.18 callers:** `use sqlrite_ask::ConnectionAskExt` becomes `use sqlrite::ConnectionAskExt` (after enabling the engine's `ask` feature). API method signature unchanged. The 0.1.18 crate had been live ~30 minutes with no known adopters at the time of the flip. Lesson: when a "thin per-product wrapper" sub-phase introduces a new edge in the dep graph, sketch out the full graph BEFORE writing code — would have caught the cycle in design rather than mid-implementation.

## ✅ Phase 8 — Full-text search + hybrid retrieval *(complete — see [`phase-8-plan.md`](phase-8-plan.md), [`fts.md`](fts.md))*

Adds the FTS5-style inverted-index machinery that Phase 7 deliberately skipped, plus hybrid retrieval (BM25 + vector score fusion via raw arithmetic — no new typed function needed). Hybrid search (lexical + semantic) is the modern standard for RAG retrieval — vector-only retrieval misses keyword-grounded queries.

Mirrored the integration shape Phase 7d (HNSW) laid down: new `IndexMethod::Fts` arm, `try_fts_probe` optimizer hook, dedicated `KIND_FTS_POSTING` cell tag, on-demand v4→v5 file-format bump. Closes out the 0.1.x cycle and lines up the **v0.2.0** release (the 0.1.x → 0.2.x bump marks the file-format change + new SQL surface).

### ✅ Phase 8a — Standalone algorithms

`src/sql/fts/` ships three standalone modules: `tokenizer.rs` (ASCII split + lowercase), `bm25.rs` (BM25+ scoring with `k1=1.5`, `b=0.75` fixed at SQLite FTS5 defaults), and `posting_list.rs` (in-memory inverted index keyed on `i64` rowid, with `insert` / `remove` / `query` / `matches` / `score`). Pure algorithm — no SQL coupling, infallible API, only `std` deps. Inline `#[cfg(test)] mod tests` per file (22 tests covering empty cases, TF monotonicity, length normalization, IDF behavior, hand-computed BM25 reference, deterministic 1k-doc corpus). PR #78.

### ✅ Phase 8b — SQL surface

Wires the standalone algorithms into the executor end-to-end. `IndexMethod::Fts` arm + `create_fts_index` (TEXT-only validation + seed from existing rows + push `FtsIndexEntry`). `fts_match(col, 'q')` / `bm25_score(col, 'q')` scalar functions with pre-flight FTS-index check. `try_fts_probe` optimizer hook recognizes `WHERE fts_match(col, 'q') ORDER BY bm25_score(col, 'q') DESC LIMIT k`. INSERT incremental update via `maintain_fts_on_insert`; DELETE / UPDATE flag `needs_rebuild = true`; `rebuild_dirty_fts_indexes` runs at save start. 14 new tests (12 integration + 2 persistence round-trip via the rootpage=0 replay path). PR #79.

### ✅ Phase 8c — Persistence

Cell-encoded storage so the in-memory `PostingList` survives save/reopen byte-equivalently. `KIND_FTS_POSTING = 0x06` cell tag; new `src/sql/pager/fts_cell.rs` with `FtsPostingCell` (per-term cells + an empty-term sidecar carrying the doc-lengths map for round-trip honesty on zero-token rows). `stage_fts_btree` / `load_fts_postings` mirror the HNSW save/load shape; `rebuild_fts_index` gains the cell-load fast path. **On-demand v4→v5 file-format bump** ([Q10](phase-8-plan.md#q10-file-format-version-bump-strategy)): existing v4 databases without FTS keep writing v4; the first FTS-bearing save promotes to v5. Decoders accept both. 16 new tests (10 cell-codec, 1 PostingList round-trip, 5 pager-level: persistence path, v4 preservation, v5 bump, empty / zero-token edge cases, 500-doc multi-leaf). PR #80.

### ✅ Phase 8d — Hybrid retrieval worked example

`examples/hybrid-retrieval/` ships a self-contained Rust example showing how to compose `bm25_score` (8b) with `vec_distance_cosine` (Phase 7d) via raw arithmetic ([Q8](phase-8-plan.md#q8-hybrid-retrieval)) — no new engine code. 6-doc tech-blurb corpus with hand-baked 4-dim embeddings (no embedding-model dependency); runs three rankings on the same query: pure BM25, pure vector cosine, and 50/50 hybrid via `0.5 * bm25_score + 0.5 * (1.0 - vec_distance_cosine)`. README walks through when each shape wins, the cosine-distance-vs-similarity inversion gotcha, weight-tuning sketches, and a production checklist. PR #81.

### ✅ Phase 8e — MCP `bm25_search` tool

Adds the `bm25_search` MCP tool, symmetric with `vector_search` (Phase 7h). Wraps the canonical `WHERE fts_match(col, 'q') ORDER BY bm25_score(col, 'q') DESC LIMIT k` SQL so the LLM doesn't have to remember the WHERE pre-filter, the DESC direction, or string quoting. Pre-flight checks (table exists, column is TEXT, FTS index attached) surface clean errors before any SQL runs. SQL string-literal escaper handles embedded apostrophes per SQL standard. 3 new protocol tests. The MCP server now exposes 8 tools (was 7). PR #82.

### ✅ Phase 8f — Docs sweep

Final docs pass — canonical [`fts.md`](fts.md) reference (mirrors `ask.md`'s shape); FTS sections added to [`supported-sql.md`](supported-sql.md), [`architecture.md`](architecture.md) (module map + storage section), [`file-format.md`](file-format.md) (`KIND_FTS_POSTING` layout, v4→v5 bump in version history), [`sql-engine.md`](sql-engine.md) (`try_fts_probe` optimizer hook), [`mcp.md`](mcp.md) (`bm25_search` tool entry + count bump 7→8); FTS step added to [`smoke-test.md`](smoke-test.md); [`_index.md`](_index.md) re-organized to give Phase 8 its own top-level section.

## ✅ Phase 9 — SQL surface + DX follow-ups *(0.2.0 → 0.9.1)*

After Phase 8 closed out the v0.1.x cycle and the v0.2.0 file-format bump shipped, the next wave landed the SQL features that had been parked under "possible extras," plus the storage hygiene + DX work that had accumulated alongside them. Each sub-phase shipped as its own minor release, so consumers got each capability the moment it was stable on `main`.

### ✅ Phase 9a — DDL completeness *(v0.3.0)*

`feat(ddl): DEFAULT clause, DROP TABLE/INDEX, ALTER TABLE` (PR #86).

- **`DEFAULT <literal>`** column constraint — accepted on CREATE TABLE and ADD COLUMN; literal-only (function defaults like `CURRENT_TIMESTAMP` rejected at parse time so we don't silently accept misleading SQL).
- **`DROP TABLE [IF EXISTS]`** + **`DROP INDEX [IF EXISTS]`** — single-target; refuses to drop `sqlrite_autoindex_*` (constraint-bound). All attached indexes (auto, explicit, HNSW, FTS) ride along when a table goes away.
- **`ALTER TABLE`** — `RENAME TO` / `RENAME COLUMN` / `ADD COLUMN` / `DROP COLUMN`. One operation per statement (SQLite parity). Auto-index names follow renames; index deps cascade through column drops; `ADD COLUMN` with `DEFAULT` backfills existing rows.

### ✅ Phase 9b — Free-list + manual VACUUM *(v0.4.0, SQLR-6)*

Pages released by `DROP TABLE` / `DROP INDEX` / `ALTER TABLE DROP COLUMN` go onto a persisted free-page list rather than being silently leaked. `CREATE TABLE` and INSERT consult the freelist before extending the file. Bare `VACUUM;` rewrites every live B-Tree contiguously from page 1 and clears the freelist; modifiers (`VACUUM FULL`, table targets, etc.) are parsed but rejected at execution. No-op on in-memory databases. Refused inside an open transaction.

### ✅ Phase 9c — Auto-VACUUM *(v0.5.0, SQLR-10)*

Every page-releasing DDL checks the freelist after committing and runs `vacuum_database` automatically when the freelist exceeds **25%** of `page_count` (SQLite parity). Skips databases under 16 pages, skips inside transactions, skips on in-memory and read-only DBs. Threshold tunable per-`Connection` via `set_auto_vacuum_threshold(Option<f64>)`.

### ✅ Phase 9d — `IS NULL` / `IS NOT NULL` + typed `Option<Value>` INSERT pipeline *(v0.5.1, SQLR-7)*

Explicit null tests across `WHERE` / `UPDATE SET` / `DELETE WHERE`. The INSERT pipeline started carrying `Option<Value>` end-to-end so `NULL` and a missing-column DEFAULT can be distinguished without a sentinel.

### ✅ Phase 9e — `GROUP BY`, aggregates, `DISTINCT`, `LIKE`, `IN` *(v0.6.0, SQLR-3)*

The biggest single SQL-surface jump in the project's history.

- **`GROUP BY <col>[, <col>, …]`** — bare column names only. Every non-aggregate projection item must appear in the `GROUP BY` list (parser-checked).
- **Aggregates** — `COUNT(*)`, `COUNT(col)`, `COUNT(DISTINCT col)`, `SUM`, `AVG`, `MIN`, `MAX`. Integer `SUM` stays integer until a `REAL` arrives or `i64` overflows (one-time promotion). `AVG` returns `REAL` (or `NULL` on empty groups). `MIN` / `MAX` skip NULLs and use the same total order as `ORDER BY`. Empty-group results are `0` for counts, `NULL` for the rest.
- **`DISTINCT`** — applies after projection (and after aggregation when both are present); `LIMIT` counts unique rows; `NULL = NULL` for dedupe.
- **`LIKE` / `NOT LIKE` / `ILIKE`** — `%`, `_`, `\`-escape. ASCII case folding on by default (SQLite parity). `NULL LIKE 'pattern'` evaluates to `NULL` (excluded by `WHERE`).
- **`IN (literal-list)`** + **`NOT IN (literal-list)`** — three-valued logic per SQL standard.

### ✅ Phase 9f — JOINs *(v0.7.0, SQLR-5)*

`INNER`, `LEFT OUTER`, `RIGHT OUTER`, `FULL OUTER JOIN ... ON …` with explicit `ON`. Why all four when SQLite ships only INNER + LEFT: the per-flavor differences are NULL-padding policies on top of one nested-loop driver — `RIGHT` / `FULL` were free once the executor had a multi-table scope. See [`docs/design-decisions.md`](design-decisions.md) for the rationale.

- Aliases (`FROM customers AS c JOIN orders AS o ON c.id = o.customer_id`); when an alias is supplied the original name leaves scope (SQL standard).
- Qualified column references (`<table>.<col>` / `<alias>.<col>`); ambiguous bare references error with a "qualify it" hint.
- Multi-join chains left-fold: `A ⨝ B ⨝ C` evaluates as `(A ⨝ B) ⨝ C`.
- Self-joins require an alias on at least one side.
- `WHERE` runs after joins (the standard `LEFT JOIN ... WHERE right.col IS NULL` anti-join idiom works).

Not yet supported: `CROSS JOIN`, comma-separated FROMs, `NATURAL JOIN`, `JOIN ... USING (col)`, aggregates / `GROUP BY` / `DISTINCT` *over* a join, `fts_match` / `bm25_score` inside a join expression. Algorithm: plain nested-loop, O(N×M) per level — hash / merge joins are a future optimization.

### ✅ Phase 9g — Prepared statements + parameter binding *(v0.9.0, SQLR-23)*

Every executable statement accepts `?` placeholders anywhere a value literal is allowed. Public Rust API: `Connection::prepare` / `prepare_cached`, `Statement::execute_with_params(&[Value])` / `query_with_params(&[Value])`. Strict positional binding, strict arity. `Value::Vector(Vec<f32>)` binds where a bracket-array literal would normally appear — including the second arg of `vec_distance_*` inside an HNSW-eligible `ORDER BY`, so the graph shortcut still fires for prepared KNN queries.

`prepare_cached` keeps a per-connection LRU plan cache (default cap 16, tunable via `set_prepared_cache_capacity`) — a hot SQL string parses exactly once across the connection's lifetime. Named placeholders (`:foo`, `$1`) deferred.

### ✅ Phase 9h — HNSW probe widened to cosine + dot *(v0.9.0, SQLR-28)*

`CREATE INDEX … USING hnsw (col) WITH (metric = '<l2|cosine|dot>')` — the metric travels with the index and the optimizer only takes the graph shortcut when the query's `vec_distance_*` function matches the index's metric. Mismatches fall through to brute force rather than returning a wrong answer. Pre-SQLR-28 catalogs round-trip unchanged (no `WITH` is equivalent to `metric = 'l2'`).

### ✅ Phase 9i — `PRAGMA` dispatcher + `auto_vacuum` knob *(v0.9.1, SQLR-13)*

`PRAGMA <name>;` (read) / `PRAGMA <name> = <value>;` (write) is now a real executor arm. The first wired pragma is `auto_vacuum`, which exposes the SQLR-10 threshold to SDK / FFI / MCP consumers that can't call the Rust setter. Out-of-range values, NaN, ±∞, and unknown identifiers are rejected with typed errors — the trigger never silently saturates. Adding a new pragma is a single arm in `execute_pragma`; future ones (`journal_mode`, `synchronous`, `cache_size`, `page_size`, …) will land as they earn their keep.

## ✅ Phase 10 — Benchmarks vs SQLite *(SQLR-4 / SQLR-16)*

End-to-end SQLR-4 / SQLR-16 bench harness with twelve workloads across three groups (read-by-PK, transactional CRUD, analytical slices, vector / FTS retrieval). Pluggable `Driver` trait + bundled SQLite + DuckDB drivers; criterion-based; pinned-host runs published at [`docs/benchmarks.md`](benchmarks.md). Excluded from CI (criterion is too noisy on shared runners; `rusqlite-bundled` is heavy). See [`docs/benchmarks-plan.md`](benchmarks-plan.md) for the design and PRs #102–#114 for the staged rollout.

## Phase 11 — Concurrent writes via MVCC + `BEGIN CONCURRENT` *(SQLR-22; in flight — see [`concurrent-writes-plan.md`](concurrent-writes-plan.md))*

Lift SQLRite past SQLite's single-writer ceiling with multi-version concurrency control and a `BEGIN CONCURRENT` transaction mode, modelled on Turso's experimental MVCC. The plan doc internally numbers sub-phases as "Phase 10.x" (its working title before the roadmap renumbering); they're listed under Phase 11 here because Phase 10 already shipped.

### ✅ Phase 11.1 — Multi-connection foundation *(plan-doc "Phase 10.1")*

`Connection` is a thin handle backed by `Arc<Mutex<Database>>`. Call [`Connection::connect`] to mint a sibling that shares the same engine state — typically one per worker thread. The headline contract: `Connection` is `Send + Sync`, and the engine no longer requires the caller to wrap the public API in their own `Mutex`. Today every operation still serializes through the per-database mutex (and the pager's existing process-level flock), so the behaviour change is *capability*, not throughput; concurrent throughput arrives with `BEGIN CONCURRENT` in 11.4.

### ✅ Phase 11.2 — Logical clock + active-tx registry *(plan-doc "Phase 10.2")*

[`sqlrite::mvcc`](../src/mvcc/) module:

- `MvccClock` — process-wide monotonic `u64` over `AtomicU64`. `tick()` hands out begin- / commit-timestamps; `now()` reads the high-water without advancing it; `observe(value)` advances the clock to `value` if greater (used at WAL replay).
- `ActiveTxRegistry` — `Mutex<BTreeMap>` over in-flight transactions. `register(&clock)` allocates a `TxId`, snapshots `begin_ts`, and returns a RAII `TxHandle`; `min_active_begin_ts()` is the GC watermark Phase 11.6 reads on every commit + on `Connection::vacuum_mvcc`.
- `TxId` newtype + `TxTimestampOrId` tagged union — defined now so 11.4 can plug in without re-litigating the type shape.

WAL format bumps **v1 → v2**: bytes 24..32 of the WAL header (previously reserved-zero) now carry the persisted `clock_high_water` `u64`. v1 WALs open cleanly — those zero bytes read as "clock never advanced" — and the next checkpoint rewrites the header at v2. No offline upgrade step. `Wal::set_clock_high_water` / `Wal::clock_high_water` accessors expose the field; the setter rejects regressions with a typed error.

### ✅ Phase 11.3 — `MvStore` skeleton + `PRAGMA journal_mode` opt-in *(plan-doc "Phase 10.3")*

Standalone version-index data structure + the per-database journal-mode toggle.

- New [`MvStore`](../src/mvcc/store.rs): `Mutex<HashMap<RowID, Arc<RwLock<Vec<RowVersion>>>>>`. `RowID = (table, rowid)`; each `RowVersion` carries `begin: TxTimestampOrId`, `end: Option<TxTimestampOrId>`, `payload: VersionPayload` (`Present(cols)` or `Tombstone`). `MvStore::read(row, begin_ts)` implements the textbook snapshot-isolation visibility rule (`begin <= T < end`). `push_committed` validates monotonicity + caps the previous latest version's `end`; `push_in_flight` adds a placeholder version that's invisible to other readers until commit rewrites its `begin`.
- New [`JournalMode`](../src/mvcc/mod.rs) enum (`Wal` default, `Mvcc`); per-database setting on `Database`. `PRAGMA journal_mode = wal | mvcc;` toggles; `PRAGMA journal_mode;` returns the current value as a single-row, single-column result. `Connection::journal_mode()` reads the value through the public API. Switching `Mvcc → Wal` is rejected if the store carries committed versions (would silently strand them); v0 is intentionally strict.
- `Database` grows `mvcc_clock: Arc<MvccClock>` and `mv_store: MvStore` fields, allocated on every `Database::new` so the toggle to MVCC mode doesn't require a re-init step. Both are shared across every `Connection::connect` sibling.

### ✅ Phase 11.4 — `BEGIN CONCURRENT` writes + commit-time validation *(plan-doc "Phase 10.4" — the meat)*

The headline slice. Multiple sibling `Connection`s can each hold their own open `BEGIN CONCURRENT` transaction; commits validate against `MvStore` and abort with [`SQLRiteError::Busy`](../src/error.rs) on row-level write-write conflict. The four plan-required tests pass: disjoint inserts both commit, same-row updates collide and one wins, aborted writes never become visible, retry-after-`Busy` succeeds.

- New [`ConcurrentTx`](../src/mvcc/transaction.rs) — per-`Connection` state holding the [`TxHandle`](../src/mvcc/registry.rs) (RAII registry entry, drops at COMMIT/ROLLBACK), a private deep-clone of `Database::tables` (working state — what each statement's executor mutates), and an immutable second clone (`tables_at_begin` — used at COMMIT to derive the write-set without seeing other transactions' commits). The doubled per-tx memory is the v0 trade for correctness; column-level COW ↔ shared-`Arc` table cloning is the obvious follow-up.
- `Connection` grows a `concurrent_tx: Option<ConcurrentTx>` field, plus three new methods: `begin_concurrent()`, `commit_concurrent()`, `rollback_concurrent()`. `Connection::execute` intercepts `BEGIN CONCURRENT` / `COMMIT` / `ROLLBACK` before sqlparser runs (sqlparser 0.61 doesn't have a `Concurrent` modifier — same intercept pattern as PRAGMA).
- Inside an open concurrent transaction, every other statement runs against the transaction's private cloned tables: `Connection::execute_in_concurrent_tx` swaps `db.tables` ↔ `tx.tables` for the duration of the executor call, parks a dummy `TxnSnapshot` on `db.txn` to suppress the auto-save, runs `process_command`, then unwinds in reverse. The executor itself doesn't change.
- COMMIT shape (Hekaton-style optimistic validation): diff `tx.tables_at_begin` vs `tx.tables` → write-set; for each row, walk `MvStore` for the latest committed version's `begin_ts` (new `MvStore::latest_committed_begin` accessor); abort with `Busy` if any latest exceeds `tx.begin_ts`. On success: tick the clock for `commit_ts`, push every write into `MvStore` as a committed version (auto-caps the previous latest's `end`), apply per-row to `db.tables` (`delete_row` then `restore_row` — preserves secondary B-tree indexes), and run the legacy `save_database` so changes persist via the existing WAL.
- ROLLBACK is just `self.concurrent_tx.take()` — the cloned tables drop, the `TxHandle` unregisters, the live database was never touched.
- New `SQLRiteError::Busy(String)` and `SQLRiteError::BusySnapshot(String)` variants. `SQLRiteError::is_retryable()` covers both — the contract SDK retry helpers will rely on.
- DDL inside `BEGIN CONCURRENT` (CREATE TABLE / CREATE INDEX / DROP TABLE / DROP INDEX / ALTER TABLE / VACUUM) is rejected before the swap with a typed error (plan §8 non-goal).

**Known limitations carried forward (most resolved in 11.5):**

- ~~Reads via `Statement::query` / `Statement::query_with_params` bypass the swap.~~ ✅ Fixed in 11.5 — `Connection.concurrent_tx` is now `Mutex<Option<…>>` and a new `with_snapshot_read` helper threads the swap through `&self`.
- The `MvStore` write-set isn't yet persisted to the WAL — Phase 11.9 introduces an MVCC log-record frame kind so commits become durable through `MvStore` itself rather than via the legacy `Database::tables` mirror. (Durability already works through the legacy mirror in v0; the WAL log-record format is foundation work for cross-process MVCC.)
- `AUTOINCREMENT` inside `BEGIN CONCURRENT` isn't explicitly rejected; the v0 deep-clone-snapshot model handles concurrent INSERTs by isolating each tx's `last_rowid` bumps to its private snapshot, so two concurrent INSERTs on an `AUTOINCREMENT` column may collide at COMMIT and surface as `Busy`. Adopting the plan's "reject AUTOINCREMENT under MVCC" gate is a clean follow-up.
- Tables touched by `BEGIN CONCURRENT` writes can't carry FTS or HNSW indexes today — `restore_row` only maintains B-tree secondary indexes. Concurrent-tx tests don't exercise FTS / HNSW, but a runtime guard would surface this with a clear error rather than producing inconsistent indexes.

### ✅ Phase 11.5 — Snapshot-isolated reads via `Statement::query` *(slotted ahead of plan-doc 11.5 checkpoint work because the prepare/query gap was the most user-visible 11.4 limitation)*

`Connection.concurrent_tx` is now `Mutex<Option<ConcurrentTx>>` (was plain `Option`). A new `with_snapshot_read` helper takes `&self`, locks `concurrent_tx`, then locks the database, and — when a tx is open — swaps the tx's private cloned `tables` in for the duration of the read closure (with a scope-guarded unswap so a panic inside the closure can't strand the database). [`Statement::query`] and [`Statement::query_with_params`] route through this helper so the prepared-statement path now sees the same BEGIN-time snapshot the `execute("SELECT…")` path already saw in 11.4.

Lock order is consistently `concurrent_tx → inner` across every code path; deadlock-free by construction. `Connection` is still `Send + Sync`.

This was renumbered out of plan-doc order: the plan-doc had 11.5 as checkpoint integration, but that's a much larger slice and the prepare/query-bypass-the-swap gap was a real correctness hole for users hitting `BEGIN CONCURRENT`. Plan-doc 11.5 (checkpoint) → roadmap 11.7; plan-doc 11.6 (GC) → roadmap 11.6 (this one).

### ✅ Phase 11.6 — Garbage collection *(plan-doc "Phase 10.6"; promoted ahead of plan-doc 11.5 because unbounded `MvStore` growth was the next concrete user-impact concern after 11.5 closed the snapshot-read gap)*

Bounds in-memory growth of the [`MvStore`](../src/mvcc/store.rs) version chains. Without this, every committed version stays forever in the in-memory chain — a memory leak that grows linearly with commits.

- `MvStore::active_watermark()` returns the GC watermark — the smallest `begin_ts` across the active-tx registry, or `u64::MAX` when nothing is in flight. Versions whose committed `end` timestamp is `<= watermark` are reclaimable: no reader's `begin_ts` can fall in the half-open `[begin, end)` interval that snapshot-isolation visibility requires.
- `MvStore::gc_chain(row_id, watermark)` reclaims one row's superseded versions (kept: latest version with `end == None`, in-flight versions, and any committed version still possibly visible to a reader). Returns the number of versions dropped. Drops the row from the outer map entirely if its chain becomes empty so long-running sessions don't leak per-row entries.
- `MvStore::gc_all(watermark)` sweeps every row in one pass; returns total versions reclaimed. Snapshots the row keys upfront so the outer map lock isn't held across per-chain locks.
- [`Connection::commit_concurrent`](../src/connection.rs) gains a per-commit GC sweep on the write-set's chains. Drops the `tx` `TxHandle` *first* so its `begin_ts` exits the registry — otherwise the watermark is still pinned to our own `begin_ts` and we'd preserve versions we're free to reclaim. Cheap (sweeps only the rows this transaction wrote), and runs on every successful commit.
- New `Connection::vacuum_mvcc()` method runs a full-store sweep at the current watermark. Returns the version count reclaimed. The "vacuum the whole store" escape hatch for memory-pressure workloads or tests that want a deterministic baseline. Safe to call regardless of `journal_mode` (a no-op `Wal`-mode database returns 0).

**What 11.6 doesn't yet do:**

- No background GC thread or `PRAGMA mvcc_gc_interval_ms`. Per-commit sweep + explicit `vacuum_mvcc()` cover the v0 model; the periodic-sweep variant lands as a follow-up if profiles show it's needed.
- GC sweeps don't trigger `Mvcc → Wal` journal-mode downgrades. The `set_journal_mode` setter still rejects the transition while the store carries committed versions; promoting that path requires the checkpoint-integration story from 11.9.

### ✅ Phase 11.7 — SDK propagation of `Busy` / `BusySnapshot` *(plan-doc "Phase 10.8"'s first half; promoted ahead of plan-doc 11.5 checkpoint work because surfacing retryable errors to SDK callers is what unblocks Python / Node / Go users from writing `BEGIN CONCURRENT` retry loops)*

- **C FFI** ([`sqlrite-ffi/src/lib.rs`](../sqlrite-ffi/src/lib.rs)): new `SqlriteStatus::Busy = 5` and `SqlriteStatus::BusySnapshot = 6` codes; `SqlriteStatus::is_retryable()` covers both. A new internal `status_of_sqlrite` mapper inspects the engine's `SQLRiteError` variant and routes `Busy` / `BusySnapshot` to the dedicated codes.
- **Python SDK**: two new exception classes `sqlrite.BusyError` and `sqlrite.BusySnapshotError`, both inheriting from `sqlrite.SQLRiteError`. `map_engine_err` helper raises the matching subclass.
- **Node.js SDK**: exported `ErrorKind` string enum (`'Busy'`, `'BusySnapshot'`, `'Other'`) and `errorKind(message: string)` classifier function. The engine's `thiserror` Display prefixes retryable errors with `'Busy: '` / `'BusySnapshot: '` so the classifier just regex-tests the prefix.
- **Go SDK**: two new sentinel error values `sqlrite.ErrBusy` / `sqlrite.ErrBusySnapshot`, plus an `IsRetryable(err error) bool` helper. `wrapErr` recognises the new FFI status codes and wraps the engine message with `fmt.Errorf("…: %w", ErrBusy)`.
- **WASM SDK** — deliberately untouched (browser is single-threaded; multi-handle shape not yet exposed).

### ✅ Phase 11.8 — Multi-handle SDK shape *(in progress, was plan-doc 11.8's other half; promoted ahead of plan-doc 11.5 again because the 11.7 retry-error machinery can't be exercised end-to-end through any SDK until siblings are reachable)*

Each pre-11.8 SDK `connect()` / `new Database()` built an *isolated* backing DB; the 11.7 `BusyError` / `errorKind` / `ErrBusy` plumbing was reachable but not actually triggerable from user code. This slice exposes the engine's `Connection::connect()` through every reachable language so apps can mint sibling handles that share state, and finally exercise the 11.7 retry idioms with real cross-handle conflicts.

- **C FFI** ([`sqlrite-ffi/src/lib.rs`](../sqlrite-ffi/src/lib.rs)): new `sqlrite_connect_sibling(existing, out)` function. Wraps the engine's `Connection::connect`. Callers get a sibling handle with its own `SqlriteConnection` pointer but shared backing database; the sibling must be closed via `sqlrite_close` (its lifecycle is independent — closing one handle doesn't tear down the others while a sibling is still alive).
- **Python SDK** ([`sdk/python/src/lib.rs`](../sdk/python/src/lib.rs)): new `Connection.connect()` instance method that mints a sibling pyclass. Wraps the engine's `Connection::connect` inside the existing `Mutex<RustConnection>`. The new handle inherits the parent's `ask_config`.
- **Node.js SDK** ([`sdk/nodejs/src/lib.rs`](../sdk/nodejs/src/lib.rs)): new `db.connect()` method on the `Database` class. Same shape — sibling shares state, can hold its own `BEGIN CONCURRENT`.
- **Go SDK** — deliberately not changed. Go's `database/sql` already gives callers a connection pool over a single `sql.Open`; each pool connection acquired through `db.Conn(ctx)` is *already* a sibling of the rest at the driver layer. But each `sql.Open("sqlrite", path)` still builds an independent backing DB because the pool is per-`sql.DB`. Exposing a cross-pool sibling shape through the `database/sql` driver model is genuinely non-obvious (it'd require a process-level registry keyed by path); deferred to the multi-handle Go follow-up.
- **WASM SDK** — still untouched. The browser is single-threaded and `wasm-bindgen` lifetimes complicate sibling pyclass-style sharing. Same deferral as 11.7.

Each SDK gets end-to-end tests that exercise `BEGIN CONCURRENT` cross-handle conflicts: two sibling handles, two concurrent transactions on the same row, the second commit hits the SDK's typed retryable error, retry succeeds.

### ✅ Phase 11.9 — WAL log-record durability + crash recovery *(plan-doc "Phase 10.5"; renumbered to follow SDK propagation because durability via the legacy `save_database` mirror already worked in v0)*

MVCC commits now leave a typed log-record frame in the WAL on top of the existing page-level commit. The MVCC frame is appended before the legacy save's commit-frame fsync, so a single fsync covers both: a crash either keeps both or loses both. On reopen, the WAL replay decodes every MVCC frame and re-pushes the row versions into `MvStore`; the in-memory MVCC clock is seeded past the highest replayed `commit_ts` so post-restart transactions can never hand out a regressed `begin_ts`.

- **WAL format version bumped to v3.** v1 / v2 are still readable (replay just sees zero MVCC frames); v3 adds the MVCC frame marker (`page_num = u32::MAX`) and the body codec.
- **Frame body codec** ([`src/mvcc/log.rs`](../src/mvcc/log.rs)): `MvccCommitBatch { commit_ts, records }` encoded with magic `MVCC0001`, then `commit_ts` (u64 LE), record count (u16 LE), then per-record `(op tag, table name, rowid, optional column-value pairs)`. Everything fits in the 4 KiB frame body; the encoder surfaces a typed error if a single commit overflows (multi-frame batches are a deferred slice).
- **Append path** ([`src/connection.rs`](../src/connection.rs) `commit_concurrent`): after validation passes, the resolved write-set is encoded into a batch, appended to the WAL (no fsync), and then `save_database` runs and seals the transaction with its own fsync. The clock high-water in the WAL header is also bumped so a future checkpoint persists it.
- **Replay path** ([`src/sql/pager/mod.rs`](../src/sql/pager/mod.rs) `replay_mvcc_into_db`): drains `Pager::recovered_mvcc_commits` into `MvStore` and observes the clock past `max(header.clock_high_water, max(commit_ts))`. Replay is unconditional — `JournalMode::Wal`-mode databases simply see zero frames.
- **Tests** ([`src/connection.rs`](../src/connection.rs)): six new cases cover round-trip persistence, multi-row batches, ROLLBACK-leaves-no-frame, legacy-commit-leaves-no-frame, multi-commit replay after an unclean close, and clock-seeding past the last `commit_ts`.

**Out of scope for 11.9** (parked for a follow-up): checkpoint draining the `MvStore` versions back into the pager (which would let `set_journal_mode(Mvcc → Wal)` succeed); a real OS-level kill-mid-commit test (the existing test uses a clean drop, which exercises the same crash-recovery codepath because the WAL is the durable record).

### Phase 11.10 — Indexes under MVCC *(deferred-by-design, plan-doc "Phase 10.7")*

Each secondary-index entry becomes its own `RowVersion`. Turso explicitly punted on this; SQLRite's v0 will reject `CREATE INDEX` while `journal_mode = mvcc`.

### Phase 11.11 — REPL `.spawn` + bench workload *(planned)*

REPL `.spawn` meta-command for interactive `BEGIN CONCURRENT` demos. New "N concurrent writers" benchmark workload pitting SQLRite-MVCC against SQLite + DuckDB on disjoint-row write throughput. Plus Go SDK multi-handle work (cross-pool sibling shape).

### Phase 11.12 — Docs *(planned, plan-doc "Phase 10.9")*

Promote the plan to `docs/concurrent-writes.md` and update the cross-references.

## "Possible extras" not pinned to a phase

The remaining items — actually open, not retroactively rewritten:

- Subqueries (scalar, `IN (SELECT ...)`, correlated) and CTEs (`WITH`, recursive)
- `HAVING` (post-aggregation filter)
- `CASE WHEN … THEN … END`, `BETWEEN`, `GLOB`, `REGEXP`, `LIKE … ESCAPE '<char>'`
- Aggregates / `GROUP BY` / `DISTINCT` *over* joins (needs a single executor pass that knows about multiple input streams)
- Multi-column / expression `ORDER BY`, `OFFSET`, `NULLS FIRST/LAST`
- `UNION` / `INTERSECT` / `EXCEPT`, `INSERT ... SELECT`
- Composite + expression indexes
- `CREATE VIEW`, `CREATE TRIGGER`, `FOREIGN KEY`, `CHECK`, table-level / composite constraints
- Savepoints + isolation-level control (`BEGIN IMMEDIATE` / `BEGIN EXCLUSIVE`)
- Built-in scalar functions (`LENGTH`, `UPPER`, `LOWER`, `COALESCE`, `IFNULL`, date/time, `printf`, …)
- More pragmas (`journal_mode`, `synchronous`, `cache_size`, `page_size`, …)
- Alternate storage engines (LSM/SSTable for write-heavy workloads)
- Code signing for desktop installers (Phase 6.1)

These slot in where they make sense — many are natural side effects of the existing executor / pager / parser surfaces.
