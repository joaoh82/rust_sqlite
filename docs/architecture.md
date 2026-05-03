# Architecture

A bird's-eye view of the system, with pointers into the code.

## Layered diagram

```
         ┌─────────────────────────────────────────────────────────┐
         │  REPL  (src/main.rs, src/repl/)                         │
         │   rustyline editor, prompt, history, input validation   │
         └───────────────────────────┬─────────────────────────────┘
                                     │ raw string lines
                 ┌───────────────────┴─────────────┐
                 │                                 │
                 ▼                                 ▼
      ┌──────────────────┐              ┌────────────────────┐
      │ Meta dispatch    │              │  SQL dispatch      │
      │ src/meta_command │              │  src/sql/mod.rs    │
      │ .exit/.open/.save│              │  .tables at fn top │
      └────────┬─────────┘              └──────┬─────────────┘
               │ mutates Database               │
               │ via pager::{open,save}         │ parses with sqlparser
               │                                │ routes by Statement kind
               │                                ▼
               │                      ┌───────────────────────┐
               │                      │  Parser layer         │
               │                      │  src/sql/parser/      │
               │                      │   create / insert /   │
               │                      │   select              │
               │                      └──────┬────────────────┘
               │                             │ clean query structs
               │                             ▼
               │                      ┌───────────────────────┐
               │                      │  Executor             │
               │                      │  src/sql/executor.rs  │
               │                      │   eval_expr, exec_*   │
               │                      └──────┬────────────────┘
               │                             │ reads + mutates
               │                             ▼
               └─────────────────────┬─────────────────────────┐
                                     ▼                         │
                     ┌───────────────────────────┐             │
                     │  In-memory data model     │             │
                     │  src/sql/db/              │             │
                     │    database.rs            │             │
                     │    table.rs               │             │
                     └──────────┬────────────────┘             │
                                │  after write statements ────┘
                                │  auto-save triggers
                                ▼
                     ┌───────────────────────────┐
                     │  Pager + file format      │
                     │  src/sql/pager/           │
                     │    mod.rs  (high-level)   │
                     │    pager.rs (cache+diff)  │
                     │    file.rs  (raw I/O)     │
                     │    page.rs, header.rs     │
                     └──────────┬────────────────┘
                                │ one .sqlrite file
                                ▼
                     ┌───────────────────────────┐
                     │  Disk                     │
                     │  4 KiB pages              │
                     │  page 0 = header          │
                     │  page 1+ = typed payload  │
                     └───────────────────────────┘
```

## Workspace layout

The repo is a Cargo workspace. The engine is the root crate; everything else lives in a sibling directory.

| Crate / directory | Role |
|---|---|
| Root (`./`) — `sqlrite-engine` on crates.io, `use sqlrite::…` in code | The engine. Library + the REPL `[[bin]]`. Library surface: `Connection`, `Statement`, `Rows`, `Value`, `Database`. |
| [`sqlrite-ffi/`](../sqlrite-ffi/) | C FFI shim. Builds `libsqlrite_c.{so,dylib,dll}` + cbindgen-generated `sqlrite.h`. Used by the Go SDK + by anyone wanting to dlopen SQLRite from another language. Phase 5b. |
| [`sqlrite-ask/`](../sqlrite-ask/) | Pure-Rust LLM transport adapter (Anthropic-first; OpenAI / Ollama follow-ups pending). Takes a `&str` schema dump + `&str` question, returns generated SQL. No engine dep — the engine integration lives in `sqlrite-engine`'s `ask` feature. Phase 7g.1 + the 7g.2 dep-direction flip. |
| [`sqlrite-mcp/`](../sqlrite-mcp/) | Model Context Protocol server binary. Hand-rolled JSON-RPC 2.0 over stdio. Seven tools (`list_tables`, `describe_table`, `query`, `execute`, `schema_dump`, `vector_search`, `ask`). Phase 7h + 7g.8. See [`mcp.md`](mcp.md). |
| [`sdk/python/`](../sdk/python/) | PyO3 bindings — `sqlrite` on PyPI. Phase 5c. |
| [`sdk/nodejs/`](../sdk/nodejs/) | napi-rs bindings — `@joaoh82/sqlrite` on npm. Phase 5d. |
| [`sdk/go/`](../sdk/go/) | cgo wrapper over `sqlrite-ffi`. `database/sql` driver. Phase 5e. |
| [`sdk/wasm/`](../sdk/wasm/) | wasm-bindgen build — `@joaoh82/sqlrite-wasm` on npm. Phase 5g. *(Not a workspace member — wasm32 target only.)* |
| [`desktop/src-tauri/`](../desktop/src-tauri/) | Tauri 2.0 + Svelte 5 desktop app. Embeds the engine directly. Phase 2.5. |

The engine never depends on the SDK crates; the SDK crates each depend on the engine via path-dep. `sqlrite-mcp` depends on the engine (default-features = false) plus its own optional `ask` feature that re-enables the engine's `ask` feature, which pulls `sqlrite-ask` transitively. The whole graph is acyclic — see the 7g.2 dep-direction flip retrospective in [roadmap.md](roadmap.md) for the work that made it so.

## Module map (engine)

| Module | What it owns |
|---|---|
| [`src/main.rs`](../src/main.rs) | Binary entry: init env_logger, build rustyline editor, run the REPL loop, route input to either the meta or SQL dispatcher |
| [`src/lib.rs`](../src/lib.rs) | Library entry: re-exports `Connection`, `Statement`, `Rows`, `Value`, `Database`, `process_command`, the `ask` module (when feature on), etc. — the stable public surface every SDK binds against |
| [`src/connection.rs`](../src/connection.rs) | `Connection` / `Statement` / `Rows` / `Row` / `OwnedRow` / `FromValue` — the Phase 5a public API |
| [`src/ask/`](../src/ask/) | Engine integration with `sqlrite-ask`: `ConnectionAskExt`, `ask_with_database`, the `schema::dump_schema_for_database` helper. The `schema` submodule is always available; the rest is gated behind the `ask` feature. Phase 7g.2. |
| [`src/repl/`](../src/repl/) | `REPLHelper` (implements rustyline's `Helper` trait: completer, hinter, highlighter, validator). Also `get_config` and `get_command_type` |
| [`src/meta_command/`](../src/meta_command/) | `MetaCommand` enum, parsing (`.open FOO.db` → `Open(PathBuf)`, `.ask <Q>` → `Ask(String)`), and dispatch to persistence + ask helpers |
| [`src/error.rs`](../src/error.rs) | `SQLRiteError` (thiserror-derived), `Result<T>` alias, hand-rolled `PartialEq` that handles `io::Error` |
| [`src/sql/mod.rs`](../src/sql/mod.rs) | `SQLCommand` classifier, `process_command` — the top-level entry that parses a SQL string and routes to the right executor. Also triggers auto-save. |
| [`src/sql/parser/`](../src/sql/parser/) | Takes a `sqlparser::ast::Statement` and produces internal query structs (`CreateQuery`, `InsertQuery`, `SelectQuery`) with only the fields we actually use |
| [`src/sql/executor.rs`](../src/sql/executor.rs) | `execute_select`, `execute_delete`, `execute_update`, plus the shared expression evaluator `eval_expr` / `eval_predicate`. Also the bounded-heap top-k optimization (Phase 7c) and the HNSW probe shortcut (Phase 7d.2). |
| [`src/sql/db/database.rs`](../src/sql/db/database.rs) | `Database`: table map + optional `source_path` + optional long-lived `Pager` + transaction-snapshot state |
| [`src/sql/db/table.rs`](../src/sql/db/table.rs) | `Table`, `Column`, `Row`, `Value` (in-memory storage incl. VECTOR + JSON columns); helpers for row iteration (`rowids`, `get_value`, `set_value`, `delete_row`, `insert_row`) |
| [`src/sql/hnsw.rs`](../src/sql/hnsw.rs) | Standalone HNSW algorithm — insert / search / layer assignment / beam search. Phase 7d.1. |
| [`src/sql/json.rs`](../src/sql/json.rs) | JSON column type + path-extraction functions (`json_extract`, `json_type`, `json_array_length`, `json_object_keys`). Phase 7e. |
| [`src/sql/pager/`](../src/sql/pager/) | On-disk file format and I/O — see [file-format.md](file-format.md) and [pager.md](pager.md) for details. WAL + checkpointer + shared/exclusive lock modes (Phase 4a-4e) live here. |

## Flow of a SQL statement

Take `UPDATE users SET age = age + 1 WHERE name = 'bob';`:

1. **REPL** reads a line, [`repl::get_command_type`](../src/repl/mod.rs) sees it doesn't start with `.`, so it's a `SQLCommand`.
2. **`process_command`** ([`src/sql/mod.rs`](../src/sql/mod.rs)) asks `sqlparser` to parse the string into a `Statement`. It sees `Statement::Update(_)`.
3. Before dispatching, it records `is_write_statement = true` so auto-save runs later.
4. It calls **`executor::execute_update`** ([`src/sql/executor.rs`](../src/sql/executor.rs)).
5. The executor destructures `Update { table, assignments, selection, .. }`, validates that the assignment targets exist on the table, then enters two passes:
   - **Read pass**: walk every rowid in the table, evaluate `selection` (the `WHERE`), evaluate the RHS of each assignment expression under the matched row's context, collect `(rowid, [(col, new_value)])` tuples.
   - **Write pass**: take `&mut` on the table and call `set_value(col, rowid, new_value)` for each planned write.
6. `set_value` enforces the declared column type and the `UNIQUE` constraint before touching storage, updates the `BTreeMap` row storage, and refreshes any index.
7. Control returns to `process_command`. Since `is_write_statement` is true and `db.source_path` is `Some`, it calls `pager::save_database(db, path)`.
8. **`save_database`** takes the long-lived `Pager` off the Database, re-serializes every table with `bincode`, stages the resulting pages into the pager, and commits. Commit diffs staged bytes against the pager's `on_disk` snapshot and only writes pages whose bytes actually changed.

Steps 1–7 are purely in-memory; step 8 is the only disk contact, and after the first write it's sub-full-file.

## What lives where — by concern

- **Parsing**: `src/sql/parser/` + upstream `sqlparser` crate. Converts SQL strings → ASTs → simplified internal structs.
- **Planning**: intentionally not a thing yet. Execution is direct — a query plan is implicit in the executor code path.
- **Execution**: `src/sql/executor.rs` walks the internal structs, drives reads against `Table`, and writes via `Table::set_value` / `insert_row` / `delete_row`.
- **Storage (in memory)**: `src/sql/db/table.rs` — column-oriented `BTreeMap<rowid, value>` per column; indexes as separate `BTreeMap`s on UNIQUE/PK columns.
- **Storage (on disk)**: `src/sql/pager/` — 4 KiB pages, real B-Tree per table (Phase 3d), secondary indexes (3e), HNSW indexes as their own page tree (7d.3), WAL + crash-safe checkpointer (4c-4d), shared/exclusive lock modes (4e).
- **Persistence policy**: `src/sql/mod.rs::process_command` for when to auto-save; `src/sql/pager/mod.rs::save_database` for how. Inside a `BEGIN`/`COMMIT` block, auto-save is suppressed and changes accumulate against an in-memory snapshot — `COMMIT` flushes the whole batch in one WAL frame; `ROLLBACK` restores the snapshot.
- **Error handling**: `src/error.rs` defines a single `SQLRiteError` enum used throughout, with `#[from]` conversions from `ParserError` and `io::Error`.

## What's deliberately missing

The roadmap has shipped far enough that the original "deliberately missing" list mostly turned into shipped features. What's still left:

- **No query optimizer** beyond the bounded-heap top-k pass for KNN (Phase 7c) and the HNSW probe shortcut (7d.2). Equality-on-PK probes are direct; everything else is a table scan.
- **No joins.** `INNER` / `LEFT OUTER` / `CROSS` are parsed but rejected by the executor. On the "possible extras" list in [roadmap.md](roadmap.md).
- **No aggregates.** `COUNT(*)` / `SUM` / `AVG` / `GROUP BY` aren't implemented yet — the parser accepts them but the executor errors. Phase 8 candidate alongside FTS.
- **No network layer.** SQLRite is embedded-only. The closest thing is the [`sqlrite-mcp`](mcp.md) server, which is stdio (not network). A real wire protocol isn't on the roadmap.
- **No streaming row cursor.** `Rows` is currently backed by an eager `Vec` (Phase 5a). The `Rows::next` API is shaped to support a real cursor — the swap is deferred to **5a.2**.

Everything else from the original "deliberately missing" list (transactions, file locking, concurrency, embedding API, FFI, language SDKs, WASM, AI extensions) has shipped. See [roadmap.md](roadmap.md) for the full ledger.
