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

## Module map

| Module | What it owns |
|---|---|
| [`src/main.rs`](../src/main.rs) | Binary entry: init env_logger, build rustyline editor, run the REPL loop, route input to either the meta or SQL dispatcher |
| [`src/repl/`](../src/repl/) | `REPLHelper` (implements rustyline's `Helper` trait: completer, hinter, highlighter, validator). Also `get_config` and `get_command_type` |
| [`src/meta_command/`](../src/meta_command/) | `MetaCommand` enum, parsing (`.open FOO.db` → `Open(PathBuf)`), and dispatch to persistence helpers |
| [`src/error.rs`](../src/error.rs) | `SQLRiteError` (thiserror-derived), `Result<T>` alias, hand-rolled `PartialEq` that handles `io::Error` |
| [`src/sql/mod.rs`](../src/sql/mod.rs) | `SQLCommand` classifier, `process_command` — the top-level entry that parses a SQL string and routes to the right executor. Also triggers auto-save. |
| [`src/sql/parser/`](../src/sql/parser/) | Takes a `sqlparser::ast::Statement` and produces internal query structs (`CreateQuery`, `InsertQuery`, `SelectQuery`) with only the fields we actually use |
| [`src/sql/executor.rs`](../src/sql/executor.rs) | `execute_select`, `execute_delete`, `execute_update`, plus the shared expression evaluator `eval_expr` / `eval_predicate` |
| [`src/sql/db/database.rs`](../src/sql/db/database.rs) | `Database`: table map + optional `source_path` + optional long-lived `Pager` |
| [`src/sql/db/table.rs`](../src/sql/db/table.rs) | `Table`, `Column`, `Row`, `Index` (in-memory storage); helpers for row iteration (`rowids`, `get_value`, `set_value`, `delete_row`, `insert_row`) |
| [`src/sql/pager/`](../src/sql/pager/) | On-disk file format and I/O — see [file-format.md](file-format.md) and [pager.md](pager.md) for details |

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
- **Storage (on disk)**: `src/sql/pager/` — 4 KiB pages. Currently every table serializes to a `bincode` blob laid across chained pages; a real B-Tree replaces this in Phase 3d.
- **Persistence policy**: `src/sql/mod.rs::process_command` for when to auto-save; `src/sql/pager/mod.rs::save_database` for how.
- **Error handling**: `src/error.rs` defines a single `SQLRiteError` enum used throughout, with `#[from]` conversions from `ParserError` and `io::Error`.

## What's deliberately missing

- No network layer — SQLRite is embedded only. Phase 5 will split into a `lib` crate with a Connection API.
- No transactions — every statement implicitly commits. `BEGIN`/`COMMIT` are parsed by `sqlparser` but rejected by the executor.
- No query optimizer — simple table scans.
- No server process — no daemon, no wire protocol.
- No concurrent access — single process, single thread. WAL + file locking is Phase 4.

Most of these are on the [Roadmap](roadmap.md).
