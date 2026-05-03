# SQL engine

How a SQL string turns into data changes. If you want to see how storage works, read [storage-model.md](storage-model.md) — this doc is the parse → plan → execute pipeline on top of it.

## Entry point

Every SQL statement goes through [`process_command`](../src/sql/mod.rs):

```rust
pub fn process_command(query: &str, db: &mut Database) -> Result<String>

// Richer variant — returns both the status line and (for SELECT) the
// pre-rendered prettytable. The REPL uses this so it can print the
// table above the status; SDK / FFI / MCP callers ignore the rendered
// field and use the typed-row API for actual row access.
pub fn process_command_with_render(query: &str, db: &mut Database)
    -> Result<CommandOutput>;

pub struct CommandOutput {
    pub status: String,
    pub rendered: Option<String>,   // Some(_) only for SELECT
}
```

Given a raw line of SQL (e.g. `"SELECT name FROM users WHERE age > 25;"`) and the in-memory database, both functions return either a human-readable status (and, for SELECT, a rendered prettytable) or a typed error. **Neither writes to stdout** — the REPL is the only consumer that prints anything; everyone else (Tauri desktop, SDKs, the MCP server) just reads the returned struct.

The function's shape:

1. Parse with `sqlparser` (SQLite dialect).
2. Reject multi-statement input.
3. Classify as read-only vs. writing.
4. Match on `Statement::*` and dispatch to the appropriate executor.
5. If the statement writes and the DB is file-backed, auto-save.
6. Return `CommandOutput { status, rendered }`. (`process_command` is a backwards-compat wrapper that just returns `.status`.)

## Parsing

The heavy lifting is `sqlparser::Parser::parse_sql`. It produces a `Vec<sqlparser::ast::Statement>`; we error out if there's more than one.

### Simplifying the AST

The `sqlparser` AST is designed to cover every SQL dialect, so its types are huge. We unwrap them into trimmed internal structs living under [`src/sql/parser/`](../src/sql/parser/):

| sqlparser variant | internal struct | file |
|---|---|---|
| `Statement::CreateTable(CreateTable)` | `CreateQuery { table_name, columns: Vec<ParsedColumn> }` | [`create.rs`](../src/sql/parser/create.rs) |
| `Statement::Insert(Insert)` | `InsertQuery { table_name, columns, rows }` | [`insert.rs`](../src/sql/parser/insert.rs) |
| `Statement::Query(_)` | `SelectQuery { table_name, projection, selection, order_by, limit }` | [`select.rs`](../src/sql/parser/select.rs) |

`UPDATE` and `DELETE` don't have a dedicated internal struct — the executor pattern-matches the sqlparser types directly because there's less transformation needed.

Each parser module also rejects features we don't implement with `SQLRiteError::NotImplemented` — `JOIN`, `GROUP BY`, `HAVING`, `DISTINCT`, `OFFSET`, multi-table DELETE, tuple assignment targets, etc. These errors carry the feature name in the message so the user knows what isn't there.

## Statement dispatch

The core `match` inside `process_command`:

```rust
match query {
    Statement::CreateTable(_) => { /* CreateQuery::new + db.tables.insert */ }
    Statement::Insert(_)      => { /* InsertQuery::new + table.insert_row */ }
    Statement::Query(_)       => { /* SelectQuery::new + executor::execute_select */ }
    Statement::Delete(_)      => { /* executor::execute_delete */ }
    Statement::Update(_)      => { /* executor::execute_update */ }
    _ => NotImplemented,
}
```

`CREATE` and `INSERT` are inlined in the dispatcher because they're short. `SELECT`, `DELETE`, and `UPDATE` each have a dedicated `execute_*` function in the executor module because they share predicate evaluation.

## The executor

[`src/sql/executor.rs`](../src/sql/executor.rs) is the home of all expression evaluation and all row iteration.

### The expression evaluator

`eval_expr(expr: &Expr, table: &Table, rowid: i64) -> Result<Value>` walks a `sqlparser::Expr` and produces a runtime [`Value`](storage-model.md#runtime-value-vs-storage-row). It's a straightforward recursive match:

- `Expr::Nested(inner)` → recurse
- `Expr::Identifier(ident)` → look up `ident.value` on the table at the given rowid
- `Expr::CompoundIdentifier(parts)` → same, using the last component (we ignore qualifiers because there's only one table in scope)
- `Expr::Value(v)` → convert a sqlparser literal to a runtime `Value`
- `Expr::UnaryOp { op, expr }` → recurse on inner, apply `+` / `-` / `NOT`
- `Expr::BinaryOp { left, op, right }` → recurse on both sides, apply the operator
- Anything else → `NotImplemented`

### Operators supported

| Category | Operators |
|---|---|
| Logical | `AND`, `OR`, `NOT` |
| Comparison | `=`, `<>`, `<`, `<=`, `>`, `>=` |
| Arithmetic | `+`, `-`, `*`, `/`, `%` |
| String | `\|\|` |
| Unary | `+`, `-`, `NOT` |

### Type promotion

Arithmetic follows a simple rule:

- Integer + Integer → Integer (with wrapping semantics on overflow)
- Any other combination of numeric types → widen both sides to `f64`, result is `Value::Real`
- Booleans coerce to `1.0` / `0.0` when mixed into arithmetic (via `as_number`)
- `Value::Text` in arithmetic is an error

`||` (string concat) always returns a `Value::Text` produced from both sides' `to_display_string()`.

### NULL semantics

- Any arithmetic or comparison with NULL → NULL (propagation).
- `eval_predicate` (`WHERE` evaluator) treats NULL as `false` — see [Design decisions §9](design-decisions.md#9-null-as-false-in-where-clauses).
- Unary `NOT NULL` → `NULL`. Unary `-NULL` → `NULL`.
- `'abc' || NULL` → `NULL`.

### Division / modulo by zero

Both return `SQLRiteError::General("division by zero")` rather than panicking or returning NaN/Infinity. The check happens *after* NULL propagation, so `5 / NULL` is still `NULL` (not an error).

## Two-pass pattern for UPDATE and DELETE

Both `execute_update` and `execute_delete` use the same pattern to satisfy Rust's aliasing rules:

1. **Read pass (`&db`)**: walk every rowid, evaluate the predicate, for UPDATE also evaluate the RHS expressions under each matched row's context. Collect a `Vec<(rowid, payload)>` of planned mutations.
2. **Write pass (`&mut db`)**: take a mutable borrow of the table, apply the collected mutations.

This is necessary because `eval_predicate` / `eval_expr` reads many column values simultaneously (the whole row might be needed) while the write operation mutates, and we can't hold both an immutable and mutable borrow of the same `Table`.

The cost is holding the planned writes in memory between the two passes. For bulk operations (`UPDATE users SET x = y` with millions of rows) this would matter; for the current scale it doesn't. Phase 3+ with a real cursor API will replace this with streaming.

## Auto-save hook

Right before returning, `process_command` runs:

```rust
if is_write_statement {
    if let Some(path) = db.source_path.clone() {
        pager::save_database(db, &path)?;
    }
}
```

`is_write_statement` is set before the match based on `matches!(&query, Statement::CreateTable(_) | Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_))`. Read-only SELECTs skip the save — no point writing the same bytes again, even if the pager diff would filter the writes out, there's bookkeeping cost in the pager's in-memory work.

If the save fails, the error propagates out. The in-memory state has already been mutated at this point; the caller needs to know disk is out of sync. A more thorough implementation would offer a rollback hook, but without transactions there's not much to roll back to.

## Error types

Everything throws `SQLRiteError`, a thiserror-derived enum in [`src/error.rs`](../src/error.rs):

- `NotImplemented(String)` — feature we recognize but don't execute yet
- `General(String)` — runtime error (type mismatch, UNIQUE violation, divide-by-zero, bad magic bytes)
- `Internal(String)` — invariant violation or bincode encode/decode failure
- `UnknownCommand(String)` — unknown meta-command
- `SqlError(#[from] ParserError)` — bubbled up from sqlparser
- `Io(#[from] std::io::Error)` — any file I/O failure

Displaying is via the thiserror-generated `Display` impl; at the REPL, `main.rs` prints `"An error occured: {err}"` and keeps going.

## Testing

Expression evaluation and statement execution are covered by tests in:

- `src/sql/mod.rs` — integration-style tests that drive the top-level `process_command` with `CREATE` → `INSERT` → `SELECT`/`UPDATE`/`DELETE` sequences and check the result messages and stored state.
- `src/sql/parser/*.rs` — test the AST → internal struct conversion for happy paths and NotImplemented rejections.

There are no dedicated expression-evaluator unit tests; the operator matrix is exercised through full `SELECT`/`UPDATE` tests with arithmetic and predicate variants (`process_command_select_arithmetic_where_test`, `process_command_update_arith_test`, etc). A dedicated `eval_expr` test suite would be a good addition — currently each operator is implicitly covered by at least one end-to-end test.
