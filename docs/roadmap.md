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

Phase 6e will publish wheels to PyPI via `maturin-action` (manylinux x86_64/aarch64, macOS universal, Windows x86_64).

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

Phase 6e will publish prebuilt binaries to npm via the napi-rs GitHub Action (Linux x86_64/aarch64, macOS universal, Windows x86_64).

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

Prerequisites for building from source: `cargo build --release -p sqlrite-ffi` to materialize `libsqlrite_c`. Phase 6e will publish prebuilt binaries as GitHub Release assets so end users don't need the Rust toolchain.

Phase 6e also tags `sdk/go/v*.*.*` so `go get github.com/joaoh82/rust_sqlite/sdk/go@v0.1.0` resolves via Go's module proxy — no central registry push needed for Go.

### Phase 5f — Rust crate polish *(deferred — Phase 6c companion)*

The Rust library is already shippable — this sub-phase adds crate metadata, docs.rs config, a `Connection`-oriented quickstart, and prep for the `cargo publish` step. Deferred because it's mostly metadata work that makes more sense alongside the actual publish workflow in Phase 6c. Examples under `examples/rust/` already exist from Phase 5a.

### ✅ Phase 5g — WASM build

New `sdk/wasm/` crate (standalone, not in the Cargo workspace — wasm-only crates trip `cargo build --workspace` on native hosts). Compiles the Rust engine straight to `wasm32-unknown-unknown` via `wasm-bindgen`. Engine runs entirely in the browser tab.

```js
import init, { Database } from 'sqlrite-wasm';
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

Phase 6e will publish `sqlrite-wasm` to npm via `wasm-pack publish` on `v*` tag push.

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
3. **npm trusted publishers** for both `@joaoh82/sqlrite` (the Node binding — scoped because npm rejected the unscoped `sqlrite` name as too similar to `sqlite`/`sqlite3`) and `sqlrite-wasm` (the browser binding). Scoped packages under your own user scope auto-own the name, so the trusted-publisher flow works without a bootstrap `NPM_TOKEN`. See `docs/release-secrets.md` §3 for the full flow.
4. **GitHub `release` environment** — required reviewer (maintainer), `main`-only deployments, scoped secrets. Acts as a second human-in-the-loop gate after the Release PR merge but before any registry write.
5. **Branch protection on `main`** — require 14 CI status checks green + 1 review + conversation resolution. Admin bypass left available for emergencies.

The runbook is safe to execute right now — the PyPI + npm trusted-publisher entries reference `release.yml` (which lands in Phase 6d); they'll sit idle until that workflow exists.

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

### Phase 6h — WASM publish

Adds `publish-wasm` job. `wasm-pack publish` to npm as `sqlrite-wasm`.

### Phase 6i — Go SDK publish

Adds `publish-go` job. No registry — tags `sdk/go/vX.Y.Z`; attaches the FFI tarballs (from `publish-ffi`) to the Go GitHub Release for users who want prebuilt `libsqlrite_c`.

### Phase 6.1 — Code signing *(follow-up)*

Desktop installers from Phase 6e ship unsigned. Phase 6.1 adds code signing:
- macOS: Apple Developer ID cert → `codesign` + notarization via `xcrun notarytool` in `tauri-action`.
- Windows: code-signing cert → `signtool` via `tauri-action`.
- Involves procurement (Apple Developer $99/yr, Windows EV cert ~$300/yr) and secret management — both are separate ops tasks.

Separate phase because the code changes are tiny (just tauri-action flags) but the procurement story is long-lived.

## Phase 7 — AI-era extensions *(research)*

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
