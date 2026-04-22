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

Parsing is done by [`sqlparser`](https://crates.io/crates/sqlparser) using the SQLite dialect. Execution only implements the statements below; anything else is rejected with a `NotImplemented` error.

### `CREATE TABLE`

```sql
CREATE TABLE <name> (<col> <type> [constraint]*, ...);
```

- Supported types: `INTEGER` / `INT` / `BIGINT` / `SMALLINT`, `TEXT` / `VARCHAR`, `REAL` / `FLOAT` / `DOUBLE` / `DECIMAL`, `BOOLEAN`
- Supported column constraints: `PRIMARY KEY`, `UNIQUE`, `NOT NULL`
- Only one `PRIMARY KEY` per table; a duplicate column name is an error
- Table-level constraints are parsed but not enforced yet

### `INSERT INTO`

```sql
INSERT INTO <name> (col1, col2, ...) VALUES (v1, v2, ...);
```

- `INTEGER PRIMARY KEY` columns can be omitted — a ROWID is auto-assigned
- Omitted non-PK columns are stored as NULL (with type restrictions — see [Storage model](storage-model.md))
- Type-mismatched values return a typed error rather than panic
- `UNIQUE` / `PRIMARY KEY` violations are rejected

### `CREATE INDEX`

```sql
CREATE [UNIQUE] INDEX [IF NOT EXISTS] <name> ON <table> (<column>);
```

- Single-column only — multi-column / composite indexes are future work
- Integer and Text columns only (Real / Bool indexes aren't supported yet)
- Anonymous indexes (no name) are rejected — give every index a name
- `CREATE UNIQUE INDEX` fails if existing rows already carry duplicate values
- Auto-created indexes: every `UNIQUE` and `PRIMARY KEY` column gets one at `CREATE TABLE` time, named `sqlrite_autoindex_<table>_<col>`

### `SELECT`

```sql
SELECT {*|col1, col2, ...} FROM <name>
  [WHERE <expr>]
  [ORDER BY <col> [ASC|DESC]]
  [LIMIT <n>];
```

- Single-table only — no joins, subqueries, or CTEs yet
- Projection is `*` or a bare column list; expressions in the projection list aren't supported
- `ORDER BY` takes exactly one column
- `LIMIT` takes a non-negative integer literal; no `OFFSET` yet
- **Optimizer**: `WHERE col = literal` (or `literal = col`) on an indexed column probes the index instead of scanning the whole table. AND / OR / range predicates still fall back to full scan.

### `UPDATE`

```sql
UPDATE <name> SET col1 = <expr> [, col2 = <expr>] [WHERE <expr>];
```

- Assignments can reference other columns of the same row (`SET age = age + 1`)
- The declared column type is enforced at write time; mismatched types error cleanly
- UNIQUE constraints are re-checked against every other row's value

### `DELETE`

```sql
DELETE FROM <name> [WHERE <expr>];
```

- No `WHERE` deletes every row in the table

## Expressions

Expressions work in `WHERE` predicates and `UPDATE`'s `SET` right-hand side.

| Category | Operators |
|---|---|
| Comparison | `=`, `<>`, `<`, `<=`, `>`, `>=` |
| Logical | `AND`, `OR`, `NOT` |
| Arithmetic | `+`, `-`, `*`, `/`, `%` (integer ops stay integer; any `REAL` promotes to `f64`) |
| String | `\|\|` (concat) |
| Unary | `+`, `-` |
| Grouping | Parentheses |

Literals: integer numbers, real numbers, `'single-quoted strings'`, booleans (`TRUE`/`FALSE`), `NULL`.

NULL handling follows SQL convention: any comparison or arithmetic involving NULL is unknown, which is treated as `false` in a `WHERE` clause. `NOT NULL` stays NULL. Division or modulo by zero returns a clean runtime error rather than a panic.

## Not yet supported

- Joins (`INNER` / `LEFT OUTER` / `CROSS`)
- Subqueries, CTEs, views
- `GROUP BY`, aggregate functions (`COUNT`, `SUM`, `AVG`, ...)
- `DISTINCT`, `HAVING`
- `LIKE`, `IN`, `IS NULL`
- Expressions in the projection list
- `OFFSET`, multi-column `ORDER BY`
- Transactions (`BEGIN` / `COMMIT` / `ROLLBACK`)
- Multiple databases in one process, attach/detach

See [Roadmap](roadmap.md) for when these land.

## History

The REPL persists an interaction history file named `history` in the working directory. Delete it to reset.

## Programmatic use

There's no library crate yet — everything is built as a binary. A `lib.rs` split and a public `Connection` / `Statement` API are part of Phase 5. For now, see the tests under `src/sql/mod.rs` and `src/sql/pager/mod.rs` for how to drive the engine in Rust code.
