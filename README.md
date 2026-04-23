Rust-SQLite (SQLRite)
===
[![Build Status](https://github.com/joaoh82/rust_sqlite/workflows/Rust/badge.svg)](https://github.com/joaoh82/rust_sqlite/actions)
[![dependency status](https://deps.rs/repo/github/joaoh82/rust_sqlite/status.svg)](https://deps.rs/repo/github/joaoh82/rust_sqlite)
[![Coverage Status](https://coveralls.io/repos/github/joaoh82/rust_sqlite/badge.svg?branch=main)](https://coveralls.io/github/joaoh82/rust_sqlite?branch=main)
[![Maintenance](https://img.shields.io/badge/maintenance-actively%20maintained-brightgreen.svg)](https://deps.rs/repo/github/joaoh82/rust_sqlite)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

`Rust-SQLite`, aka `SQLRite` , is a simple embedded database modeled off `SQLite`, but developed with `Rust`. The goal is get a better understanding of database internals by building one.

> What I cannot create, I do not understand. 
> — Richard Feynman


<table style="width:100%">
<tr>
  <td>
    <table style="width:100%">
      <tr>
        <td> key </td>
        <td> value </td>
      </tr>
      <tr>
        <td><a href="https://github.com/sqlrite/design">Design and discussions about direction<br>of the project going on over here.</a></td>
        <td></td>
      </tr>
      <tr>
        <td><a href="https://github.com/sponsors/joaoh82">Show us your support by buying us a coffee, <br>so we can keep building cool stuff. (coming soon)</a></td>
        <td><a href="https://github.com/sponsors/joaoh82"><img src="https://img.shields.io/opencollective/backers/sqlrite"></a></td>
      </tr>
      <tr>
        <td><a href="https://docs.rs/sqlrite">Documentation (coming soon)</a></td>
        <td><a href="https://docs.rs/sqlrite"><img src="https://docs.rs/sqlrite/badge.svg"></a></td>
      </tr>
      <tr>
        <td><a href="https://discord.gg/dHPmw89zAE">Come and Chat about databases with us</a></td>
        <td><a href="https://discord.gg/dHPmw89zAE">
        <img src="https://discordapp.com/api/guilds/853931853219758091/widget.png?style=shield" alt="sqlritedb discord server"/></a></td>
      </tr>
     </table>
  </td>
  <td>
<p align="center">
  <img src="images/SQLRite_logo.png" width="50%" height="auto" /> 
  </p>
  </td>
 </tr>
</table>

### Read the series of posts about it:
##### What would SQLite look like if written in Rust?
* [Part 0 - Overview](https://medium.com/the-polyglot-programmer/what-would-sqlite-would-look-like-if-written-in-rust-part-0-4fc192368984)
* [Part 1 - Understanding SQLite and Setting up CLI Application and REPL](https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-1-4a84196c217d)
* [Part 2 - SQL Statement and Meta Commands Parser + Error Handling](https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-2-55b30824de0c)
* [Part 3 - Understanding the B-Tree and its role on database design](https://medium.com/the-polyglot-programmer/what-would-sqlite-look-like-if-written-in-rust-part-3-edd2eefda473)

![The SQLite Architecture](images/architecture.png "The SQLite Architecture")

### CREATE TABLE and INSERT Statements
[![asciicast](https://asciinema.org/a/406447.svg)](https://asciinema.org/a/406447)

### Desktop app

A cross-platform Tauri 2.0 + Svelte 5 desktop GUI ships alongside the REPL (see [`desktop/`](desktop/) and [docs/desktop.md](docs/desktop.md) for details).

![SQLRite Desktop](<images/SQLRite - Desktop.png> "The SQLRite desktop app")

Launch it with `cd desktop && npm install && npm run tauri dev`. The header's New… / Open… / Save As… buttons cover the file lifecycle; the query editor has a live line-number gutter, `⌘/` (Ctrl+/) SQL comment toggle, and selection-aware Run (highlight a statement to run just that one).

### Developer guide

In-depth documentation lives under [`docs/`](docs/). Start at [`docs/_index.md`](docs/_index.md) — it navigates to:

- [Getting started](docs/getting-started.md), [Using SQLRite](docs/usage.md), [Architecture](docs/architecture.md)
- [Design decisions](docs/design-decisions.md), [Roadmap](docs/roadmap.md)
- Internals: [File format](docs/file-format.md), [Pager](docs/pager.md), [Storage model](docs/storage-model.md), [SQL engine](docs/sql-engine.md)

### Requirements
Before you begin, ensure you have met the following requirements:
* Rust (latest stable) – [How to install Rust](https://www.rust-lang.org/en-US/install.html)

### Usage

Build and launch the REPL:

```shell
cargo run
```

You'll drop into a REPL connected to a transient in-memory database. On-disk persistence (`.open`, `.save`) is coming in Phase 2.

```
SQLRite - 0.1.0
Enter .exit to quit.
Enter .help for usage hints.
Connected to a transient in-memory database.
Use '.open FILENAME' to reopen on a persistent database.
sqlrite> CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);
sqlrite> INSERT INTO users (name, age) VALUES ('alice', 30);
sqlrite> INSERT INTO users (name, age) VALUES ('bob', 25);
sqlrite> SELECT name FROM users WHERE age > 25 ORDER BY age DESC LIMIT 5;
+-------+
| name  |
+-------+
| alice |
+-------+
SELECT Statement executed. 1 row returned.
sqlrite> UPDATE users SET age = age + 1 WHERE name = 'bob';
sqlrite> DELETE FROM users WHERE age < 30;
```

#### Supported SQL

| Statement | Features |
|---|---|
| `CREATE TABLE` | `PRIMARY KEY`, `UNIQUE`, `NOT NULL`; duplicate-column detection; types `INTEGER`/`INT`/`BIGINT`/`SMALLINT`, `TEXT`/`VARCHAR`, `REAL`/`FLOAT`/`DOUBLE`/`DECIMAL`, `BOOLEAN` |
| `CREATE [UNIQUE] INDEX` | single-column, named indexes; `IF NOT EXISTS` supported; persists as a dedicated cell-based B-Tree |
| `INSERT INTO` | auto-ROWID for INTEGER PRIMARY KEY; UNIQUE enforcement via indexes; clean type errors (no panics) |
| `SELECT` | `*` or column list, `WHERE`, `ORDER BY col [ASC\|DESC]`, `LIMIT n`. `WHERE col = literal` probes an index when one exists |
| `UPDATE` | multi-column `SET`, `WHERE`; UNIQUE + type enforcement; arithmetic in assignments (`SET age = age + 1`) |
| `DELETE` | `WHERE` predicate or full-table delete |

Expressions in `WHERE` and `SET`:

- Comparisons — `=`, `<>`, `<`, `<=`, `>`, `>=`
- Logical — `AND`, `OR`, `NOT`
- Arithmetic — `+`, `-`, `*`, `/`, `%` (integer ops stay integer; any `REAL` promotes to `f64`; divide/modulo by zero is a clean error)
- String concat — `||`
- Literals — numbers, single-quoted strings, booleans, `NULL`; parentheses

Not yet implemented: joins, subqueries, `GROUP BY` / aggregates, `DISTINCT`, `LIKE` / `IN` / `IS NULL`, expressions in the projection list, `OFFSET`. See the [Roadmap](#roadmap).

#### Meta commands

| Command | Status |
|---|---|
| `.help` | working |
| `.exit` | working |
| `.open FILENAME` | working — opens an existing `.sqlrite` file or creates a fresh one; **auto-save is enabled from this point on** |
| `.save FILENAME` | working — explicit flush (rarely needed once `.open` is in play) |
| `.tables` | working |
| `.read FILENAME` | later |
| `.ast QUERY` | later |

### Roadmap

The project is staged in phases, each independently shippable. A finished phase is committed to `main` before the next one starts.

**Phase 0 — Modernization** *(done)*
- [x] Rust edition 2024, resolver 3, stable toolchain pinned via `rust-toolchain.toml`
- [x] Upgrade every dependency to current majors: `rustyline` 18, `clap` 4, `sqlparser` 0.61, `thiserror` 2, `env_logger` 0.11, `prettytable-rs` 0.10, `serde` / `log` latest

**Phase 1 — SQL execution surface** *(done)*
- [x] CLI + rustyline REPL with history, syntax highlighting, bracket matching, line validation
- [x] Parsing via `sqlparser` (SQLite dialect); typed `SQLRiteError` via `thiserror`
- [x] `CREATE TABLE` with `PRIMARY KEY`, `UNIQUE`, `NOT NULL`; duplicate-column detection; in-memory `BTreeMap` indexes on PK/UNIQUE columns
- [x] `INSERT` with auto-ROWID for `INTEGER PRIMARY KEY`, UNIQUE enforcement, NULL padding for missing columns
- [x] `SELECT` — projection, `WHERE`, `ORDER BY`, `LIMIT` (single-table, no joins yet)
- [x] `UPDATE ... SET ... WHERE ...` with type + UNIQUE enforcement at write time
- [x] `DELETE ... WHERE ...`
- [x] Expression evaluator: `=`/`<>`/`<`/`<=`/`>`/`>=`, `AND`/`OR`/`NOT`, arithmetic `+`/`-`/`*`/`/`/`%`, string concat `||`, NULL-as-false in `WHERE`
- [x] Replaced every `.unwrap()` panic on malformed input with typed errors

**Phase 2 — On-disk persistence** *(done)*
- [x] Single-file database format — one `.sqlrite` file per database
- [x] Fixed 4 KiB pages; page 0 carries a header (magic `SQLRiteFormat\0\0\0`, format version, page size, page count, schema-root page)
- [x] Typed payload pages (schema-root / table-data / overflow) chained via `next`-page pointers; payloads up to 4089 bytes before spilling into overflow
- [x] Schema catalog + per-table state serialized via `bincode` 2.0
- [x] `.open FILENAME` — create-or-load a database file
- [x] `.save FILENAME` — explicit flush of the in-memory DB to disk (auto-save arrives with Phase 3's pager)
- [x] `.tables` — list tables in the current database
- [x] Header written last during save, so a mid-save crash leaves the file recognizably unopenable

**Phase 3 — On-disk B-Tree + auto-save pager** *(done)*
- [x] **3a — Auto-save**: every committing SQL statement (`CREATE` / `INSERT` / `UPDATE` / `DELETE`) against a file-backed DB auto-flushes; `.save` is now a rare manual flush
- [x] **3b — Pager abstraction**: long-lived `Pager` holding a byte snapshot of every page on disk plus a staging area for the next commit; `commit` diffs staged vs. snapshot and writes only pages whose bytes actually changed; file truncates when the page count shrinks
- [x] **3c — Cell-based pages** *(format v2)*: rows stored as length-prefixed cells (tag-then-value encoding with null bitmap) in `TableLeaf` pages carrying a SQLite-style slot directory; oversized cells spill into an overflow page chain; the schema catalog itself is now a real table named `sqlrite_master` stored in the same cell format
- [x] **3d — B-Tree**: `InteriorNode` pages above the existing leaves; save rebuilds the tree bottom-up from the in-memory sorted rows; open descends to the leftmost leaf and scans forward via the sibling `next_page` chain. Interior cells share the `cell_length | kind_tag | body` prefix with local/overflow cells so binary search over slot directories works uniformly. Cursor / lazy-load reads deferred to Phase 5.
- [x] **3e — Secondary indexes** *(format v3)*: UNIQUE/PRIMARY KEY columns get an auto-index named `sqlrite_autoindex_<table>_<col>` at CREATE TABLE time; `CREATE [UNIQUE] INDEX name ON table (col)` adds explicit single-column indexes. `sqlrite_master` gains a `type` column distinguishing `'table'` rows from `'index'` rows. Each index persists as its own cell-based B-Tree using `KIND_INDEX` cells `(rowid, value)`. Executor optimizer probes indexes for `WHERE col = literal` (and `literal = col`) instead of full-scanning.

**Phase 2.5 — Tauri 2.0 desktop app** *(done)*
- [x] **Engine split into lib + bin** (pulled forward from Phase 5): `sqlrite` is now both a library and a binary. The Tauri app and the eventual WASM / FFI targets all import the engine as a regular Rust dependency.
- [x] **Thread-safe engine**: `Table`'s row storage switched from `Rc<RefCell<_>>` to `Arc<Mutex<_>>` so `Database` is `Send + Sync` and can live inside Tauri's shared state. The serde derives on storage types (dead since 3c.5) dropped at the same time.
- [x] **Workspace**: root `Cargo.toml` is now a Cargo workspace; `desktop/src-tauri/` is the second member.
- [x] **Tauri 2.0 backend**: four commands (`open_database`, `list_tables`, `table_rows`, `execute_sql`) wrap the engine; results are tagged enums shipped to the UI via the JSON IPC bridge.
- [x] **Svelte 5 frontend**: dark-themed three-pane layout — header with "Open…" file picker, sidebar with table list + schema, query editor with Cmd/Ctrl+Enter to run, result grid with sticky header.

**Phase 4 — Durability and concurrency** *(in progress)*
- [x] **4a — Exclusive file lock**: `Pager::open` / `::create` takes an OS advisory lock (`fs2::try_lock_exclusive`); a second process on the same file gets a clean "already in use" error. Lock releases automatically when the Pager drops.
- [x] **4b — Write-Ahead Log (`<db>.sqlrite-wal`) file format + frame codec**: 32-byte WAL header (magic / version / page size / salt / checkpoint seq), 4112-byte frames carrying `(page_num, commit_page_count, salt, checksum, body)`. Rolling-sum checksum. Torn-write recovery: corrupt or partial trailing frames are silently truncated at the boundary. Standalone module; not wired yet.
- [x] **4c — WAL-aware Pager**: `Pager::open` / `::create` now own both the main file and its `-wal` sidecar. Reads resolve `staged → wal_cache → on_disk` with a page-count bounds check; commits append a WAL frame per dirty page plus a final commit frame carrying the new page 0 (encoded header). The main file stays frozen between checkpoints — reopening replays the WAL and the decoded page-0 frame overrides the (stale) main-file header.
- [x] **4d — Checkpointer**: `Pager::checkpoint()` folds WAL-resident pages into the main file, rewrites the header, truncates the tail, fsyncs, then `Wal::truncate`s the sidecar (rolling the salt). Auto-fires from `commit` past a 100-frame threshold; also callable explicitly. Crash-safe and idempotent — a crash mid-checkpoint leaves the WAL as the source of truth, so reads stay correct and a retry rewrites the same bytes.
- [x] **4e — Multi-reader / single-writer**: new `AccessMode { ReadWrite, ReadOnly }` drives lock mode. `Pager::open_read_only` takes a shared lock (`flock(LOCK_SH)`) on both the main file and the WAL; `open` / `create` stay exclusive. Multiple RO openers coexist; any writer excludes all readers (POSIX flock semantics — "multiple readers OR one writer", not both). Read-only Pagers reject writes with a typed error. REPL gained a `--readonly` flag; library exposes `sqlrite::open_database_read_only`. Read marks aren't needed under flock — a writer can't coexist with readers, so the checkpointer never pulls frames out from under them.
- [x] **4f — Transactions (`BEGIN` / `COMMIT` / `ROLLBACK`)**: `BEGIN` snapshots the in-memory tables (`Table::deep_clone`) and suppresses auto-save; every subsequent mutation stays in memory. `COMMIT` flushes accumulated changes in one `save_database` call (one WAL commit frame for the whole transaction). `ROLLBACK` restores the pre-BEGIN snapshot. Nested begins, orphan commits/rollbacks, and BEGIN on read-only DBs all return typed errors. Errors mid-transaction keep the transaction open so the caller can explicitly recover.

**Phase 5 — Embedding surface: public API + language SDKs**
- [x] **5a — Public Rust API** *(partial)*: `Connection` / `Statement` / `Rows` / `Row` / `OwnedRow` / `FromValue` / `Value` at the crate root; structured row return from the executor; `examples/rust/quickstart.rs` runnable via `cargo run --example quickstart`. Parameter binding + cursor abstraction deferred to 5a.2.
- [x] **5b — C FFI shim**: new `sqlrite-ffi/` workspace crate ships `libsqlrite_c.{so,dylib,dll}` + a cbindgen-generated `sqlrite.h`. Opaque-pointer types, thread-local last-error, split `sqlrite_execute` (DDL/DML/transactions) vs `sqlrite_query`/`sqlrite_step` (SELECT iteration). Runnable `examples/c/hello.c` + `Makefile` (`cd examples/c && make run`).
- [x] **5c — Python SDK**: new `sdk/python/` workspace crate via PyO3 (`abi3-py38`) + maturin. DB-API 2.0-inspired — `sqlrite.connect(path)` → `Cursor.execute` / `fetchall` / iteration, context-manager support (commit-on-clean-exit / rollback-on-exception), read-only connections, 16-test pytest suite. `examples/python/hello.py` runs after `maturin develop`. PyPI publish lands in Phase 6e.
- [x] **5d — Node.js SDK**: new `sdk/nodejs/` workspace crate via napi-rs (N-API v9, Node 18+). Prebuilt `.node` binaries — no `node-gyp` install step. `better-sqlite3`-style sync API (`new Database(path)`, `stmt.all() / get() / iterate()` returning row objects), auto-generated TypeScript defs, 11 `node:test` integration tests. `examples/nodejs/hello.mjs` runs after `npm install && npm run build`. npm publish lands in Phase 6e.
- [ ] **5e — Go SDK** via cgo against the C FFI; implements `database/sql` driver so users get the standard-library experience (`sql.Open("sqlrite", path)`)
- [ ] **5f — Rust crate polish**: crate metadata, docs.rs config, `Connection`-oriented quickstart, prep for `cargo publish`
- [ ] **5g — WASM** build via `wasm-pack` (`web` + `bundler` targets); in-memory-only MVP (OPFS-backed persistence deferred). Browser demo under `examples/wasm/`
- [ ] Code examples for every language under `examples/{rust,python,nodejs,go,wasm}/`

**Phase 6 — Release engineering + CI/CD**
- [ ] **6a — CI**: GitHub Actions matrix on Linux / macOS / Windows; `cargo build` / `test` / `clippy` / `fmt` on every PR + push
- [ ] **6b — Desktop releases**: Tauri build matrix triggered on `v*` tag → signed `.AppImage` / `.deb` / `.dmg` / `.msi` uploaded to GitHub Release
- [ ] **6c — Rust crate publish**: `cargo publish` to crates.io on tag push
- [ ] **6d — C FFI prebuilt binaries**: `libsqlrite.{so,dylib,dll}` for Linux x86_64/aarch64 + macOS universal + Windows x86_64 as GitHub Release assets
- [ ] **6e — Language SDK publishes**: wheels → PyPI, `.node` binaries → npm, Go `sdk/go/v*.*.*` git tag, `sqlrite-wasm` package → npm
- [ ] **6f — Release orchestration**: `.github/workflows/release.yml` fans a single `v*` tag push out to every publish workflow and finalizes the GitHub Release

**Phase 7 — AI-era extensions** *(research)*
- [ ] Vector / embedding column type with an ANN index
- [ ] Natural-language → SQL front-end that emits queries against this engine
- [ ] Other agent-era ideas as they emerge

**Possible extras** *(no committed phase)*
- Joins (`INNER`, `LEFT OUTER`, `CROSS` — SQLite does not support `RIGHT`/`FULL OUTER`)
- `GROUP BY`, aggregates (`COUNT`, `SUM`, `AVG`, ...), `DISTINCT`, `LIKE`, `IN`, `IS NULL`
- Composite and expression indexes (with cost analysis)
- Alternate storage engines — LSM/SSTable for write-heavy workloads alongside the B-Tree
- Benchmarks against SQLite

### Contributing
**Pull requests are warmly welcome!!!**

For major changes, please [open an issue](https://github.com/joaoh82/rust_sqlite/issues/new) first and let's talk about it. We are all ears!

If you'd like to contribute, please fork the repository and make changes as you'd like and shoot a Pull Request our way!

**Please make sure to update tests as appropriate.**

If you feel like you need it go check the GitHub documentation on [creating a pull request](https://help.github.com/en/github/collaborating-with-issues-and-pull-requests/creating-a-pull-request).

### Code of Conduct

Contribution to the project is organized under the terms of the
Contributor Covenant, the maintainer of SQLRite, [@joaoh82](https://github.com/joaoh82), promises to
intervene to uphold that code of conduct.

### Contact

If you want to contact me you can reach me at <joaoh82@gmail.com>.

##### Inspiration
* https://cstack.github.io/db_tutorial/
