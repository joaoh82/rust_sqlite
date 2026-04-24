# Using SQLRite

## Launching

```bash
cargo run                           # in-memory REPL, no persistence
cargo run -- mydb.sqlrite           # open (or create) mydb.sqlrite, auto-save enabled
cargo run --release -- mydb.sqlrite # same, optimized build
```

The positional `FILE` argument is equivalent to typing `.open FILE` right after the REPL starts — existing files are loaded, missing files are created. Without it, you land in a transient in-memory database.

`--help` prints the meta-command list and the supported SQL surface inline; worth a read if you're new to the tool.

## Meta commands

Meta commands start with a dot and don't need a trailing semicolon.

| Command | Behavior |
|---|---|
| `.help` | Print the meta-command list |
| `.exit` | Write history, quit cleanly |
| `.open FILENAME` | Open (or create) a `.sqlrite` file. From this point on, every committing SQL statement auto-saves. |
| `.save FILENAME` | Force-flush the current DB to `FILENAME`. Rarely needed — auto-save makes this redundant when it's the active file. Useful for "save as" to a different path. |
| `.tables` | List tables in the current database, sorted alphabetically |
| `.read` / `.ast` | Not yet implemented |

### `.open` semantics

- If `FILENAME` exists and is a valid SQLRite database: load it and enable auto-save.
- If `FILENAME` doesn't exist: create an empty database at that path (auto-save enabled immediately).
- If `FILENAME` exists but is not a valid SQLRite database: reject with a `bad magic bytes` error — the REPL stays in its previous state.

Only one database is active at a time. A subsequent `.open` replaces the in-memory state.

## Supported SQL

The full SQL surface — every statement, every operator, every edge case, every "not yet" — lives in the canonical reference: **[Supported SQL](supported-sql.md)**.

Quick hits worth knowing when you're working at the REPL:

- **One statement per call.** The REPL / `Connection::execute` expects a single statement; multi-statement strings return `Expected a single query statement, but there are N`. For batch execution use the SDKs' `executescript` / `execute_batch`.
- **Transactions are real.** `BEGIN` / `COMMIT` / `ROLLBACK` land as expected; auto-save is suppressed inside a transaction and everything flushes in one WAL commit frame on `COMMIT`. No nested transactions yet.
- **Arithmetic stays honest.** Integer-only operations stay integer; any `REAL` operand promotes to `f64`; divide-by-zero is a typed runtime error, never a panic.
- **NULL follows three-valued logic.** `NULL = NULL` is unknown (not true) — treated as false in `WHERE`. Use `IS NULL` / `IS NOT NULL` — oh wait, [those aren't supported yet](supported-sql.md#not-yet-supported). Track via Roadmap.
- **Identifiers are case-sensitive** (table / column names; no normalization), but keywords aren't. String literals preserve case.
- **Not yet supported**: joins, subqueries, `GROUP BY` / aggregates, `DISTINCT`, `LIKE` / `IN` / `IS NULL`, projection expressions, column aliases, `OFFSET`, multi-column `ORDER BY`, savepoints, `ALTER TABLE`, `DROP TABLE`, `DROP INDEX`. See the [full list in the reference](supported-sql.md#not-yet-supported).

## History

The REPL persists an interaction history file named `history` in the working directory. Delete it to reset.

## Programmatic use

The engine is both a binary (the REPL you've been using) and a library (the `sqlrite` crate). Phase 5a landed a stable public API:

```rust
use sqlrite::Connection;

let mut conn = Connection::open("foo.sqlrite")?;
conn.execute("INSERT INTO users (name) VALUES ('alice')")?;

let mut stmt = conn.prepare("SELECT id, name FROM users")?;
let mut rows = stmt.query()?;
while let Some(row) = rows.next()? {
    let id: i64 = row.get(0)?;
    let name: String = row.get_by_name("name")?;
    println!("{id}: {name}");
}
```

See [Embedding the SQLRite engine](embedding.md) for the full API reference, and [`examples/`](../examples/) for runnable samples (`cargo run --example quickstart` walks through the basics end-to-end). Language SDKs for Python, Node.js, Go, and WASM land in Phases 5b – 5g.
