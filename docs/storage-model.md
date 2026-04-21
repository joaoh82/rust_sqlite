# Storage model

How data lives *in memory* while a database is open. For how it's laid out *on disk*, see [file-format.md](file-format.md).

The canonical code is [`src/sql/db/table.rs`](../src/sql/db/table.rs).

## The big picture

```
Database
├── db_name: "users.sqlrite"
├── source_path: Some("/path/to/users.sqlrite")
├── pager: Some(Pager)
└── tables: HashMap<String, Table>
      "users"  -> Table
      "notes"  -> Table
      ...

Table
├── tb_name: "users"
├── columns: Vec<Column>         ordered, schema
├── rows: Rc<RefCell<HashMap<String, Row>>>
│                               keyed by column name
│                               value = one column's full data
├── indexes: HashMap<String, String>   (placeholder; not used yet)
├── last_rowid: i64
└── primary_key: "id" | "-1"    "-1" = table has no explicit PK

Column
├── column_name: "name"
├── datatype: DataType::Text
├── is_pk: false
├── not_null: true
├── is_unique: true
├── is_indexed: true
└── index: Index::Text(BTreeMap<String, i64>)

Row   (stored per column inside Table.rows)
├── Integer(BTreeMap<rowid: i64, value: i32>)
├── Text(BTreeMap<rowid: i64, value: String>)
├── Real(BTreeMap<rowid: i64, value: f32>)
├── Bool(BTreeMap<rowid: i64, value: bool>)
└── None
```

## Column-oriented layout

The surprising thing is that `Table.rows` is *not* keyed by rowid. It's keyed by **column name**, and each value is a `Row` enum holding a `BTreeMap<rowid, value>` for that one column.

In other words, to read "what's at row 5?" you have to ask each column's BTreeMap for key 5:

```
users.rows["id"]    = Row::Integer({5 => 5})
users.rows["name"]  = Row::Text({5 => "alice"})
users.rows["age"]   = Row::Integer({5 => 30})
```

All columns share the same keyset. Insertion pads missing columns with NULL-ish values (see below), so every rowid has an entry in every column's map.

### Why column-oriented?

Historical rather than principled. The first pass of the codebase in 2020 used this shape, probably because it made per-column index maintenance straightforward. Phase 3c will replace it with row-oriented cells as part of moving to real B-Tree storage.

### Consequences

- Projecting a column (e.g., `SELECT name FROM users`) is one BTreeMap scan. Efficient.
- Reassembling a row (`SELECT *`) requires N map lookups for N columns. Each lookup is O(log R), so row reconstruction is O(C log R). Not terrible for small N but not ideal.
- Deleting a row means removing the rowid from every column's BTreeMap and every index's BTreeMap. Handled by [`Table::delete_row`](../src/sql/db/table.rs).

## ROWID

Every row has a `rowid: i64`. It's the implicit primary key when no explicit `INTEGER PRIMARY KEY` is declared, and it's the alias for the PK when one is. `Table.last_rowid` tracks the most recent assignment and is bumped on each insert.

When the user omits an `INTEGER PRIMARY KEY` column in an INSERT, `insert_row` auto-assigns `last_rowid + 1`. When the user supplies an explicit value, that value becomes the new rowid (and `last_rowid` advances to it, if it's larger).

Non-integer PRIMARY KEY columns are unusual; the code handles them but auto-assign only kicks in for the integer case.

## Indexes

Each `Column` carries an `Index`:

```rust
enum Index {
    Integer(BTreeMap<i32, i64>),  // value -> rowid
    Text(BTreeMap<String, i64>),  // value -> rowid
    None,                          // not indexed
}
```

Indexes exist only on columns marked `PRIMARY KEY` or `UNIQUE`, and only for Integer and Text types today. Real and Bool columns get `Index::None` even if declared unique, and UNIQUE enforcement falls back to a linear scan.

The index is maintained inline with every insert, update, and delete:

- [`insert_row`](../src/sql/db/table.rs) inserts into the column's index after successfully inserting into the Row map.
- [`set_value`](../src/sql/db/table.rs) removes the old index entry (a `retain`-based scan, since we look up by rowid) then inserts the new one.
- [`delete_row`](../src/sql/db/table.rs) strips every index entry pointing at the rowid being deleted.

Indexes are also used on write paths for UNIQUE-constraint checks — `validate_unique_constraint` (called before every INSERT) does O(log R) `contains_key` lookups.

They are **not yet used on read paths.** Today's `SELECT` always does a full table scan via [`Table::rowids`](../src/sql/db/table.rs). A planner that can turn `WHERE id = 5` into an index probe is Phase 3+ work.

## NULL handling

Storage for NULL is inconsistent by type, which is a known wart:

- **Text columns** can store NULL by encoding the literal string `"Null"` in the BTreeMap. Reads special-case this back to `Value::Null` in `Row::get`. Consequence: a user who actually inserts the string `'Null'` into a Text column will read back `NULL`. Acceptable for now, will be cleaned up in Phase 3c.
- **Integer / Real / Bool** columns can't store NULL. If a user omits such a column in INSERT, `insert_row` returns a `Type mismatch` error. This is stricter than SQL (which allows NULL by default unless `NOT NULL` is declared) but safer than the old behavior of panicking on `"Null".parse::<i32>()`.

A proper NULL-bitmap mechanism is on the Phase 3c to-do list.

## Runtime `Value` vs storage `Row`

When the executor evaluates an expression, it works with [`Value`](../src/sql/db/table.rs) — a runtime enum with wider variants:

```rust
pub enum Value {
    Integer(i64),
    Text(String),
    Real(f64),
    Bool(bool),
    Null,
}
```

Conversion:
- `Row::get(rowid) → Option<Value>` widens storage `i32` to `Value::Integer(i64)`, `f32` to `Value::Real(f64)`.
- `Table::set_value(col, rowid, Value)` narrows back with `as` casts. The declared column type enforces what's allowed (e.g., a `Value::Text` into an Integer column is a type error, not a silent corruption).

This split keeps the executor type-agnostic — it just uses `Value` arithmetic — while storage stays compact.

## Lifecycle: write paths

### INSERT

1. [`executor::execute_insert`](../src/sql/mod.rs) (inline in the `Statement::Insert` arm): delegate to `InsertQuery::new` to get `(table_name, columns, rows)`.
2. Look up the table via `db.get_table_mut`.
3. For each row of values:
   a. Check every value's column exists on the table.
   b. Call `Table::validate_unique_constraint` — uses the column indexes for O(log R) lookups.
   c. Call `Table::insert_row` — auto-assigns the PK if missing, then writes every column's BTreeMap + every index's BTreeMap in lockstep.

### UPDATE

1. [`executor::execute_update`](../src/sql/executor.rs): resolve assignment targets to column names, verify they exist.
2. Two passes to avoid borrow-checker fights:
   - Read pass (`&db`): walk every rowid, evaluate WHERE, for matching rows evaluate the assignment RHS expressions, collect planned writes.
   - Write pass (`&mut db`): for each `(rowid, [(col, value)])`, call `Table::set_value`.
3. `set_value` enforces the column's declared type and UNIQUE constraint before mutation. It refreshes the index (old entry removed, new entry inserted).

### DELETE

1. [`executor::execute_delete`](../src/sql/executor.rs): same two-pass pattern.
2. Read pass collects matching rowids.
3. Write pass calls `Table::delete_row(rowid)` for each.

### CREATE TABLE

1. `CreateQuery::new` produces a `ParsedColumn` list with type + constraints.
2. `Table::new` builds the `Table` struct: allocates an empty `BTreeMap` per column in `rows`, builds `Column` structs including empty `Index`es on UNIQUE/PK columns.
3. Top-level dispatcher inserts the table into `db.tables`.

## Lifecycle: read paths

### SELECT

1. [`executor::execute_select`](../src/sql/executor.rs) looks up the table.
2. Resolves projection to a concrete ordered column list.
3. Walks every rowid from `Table::rowids` (grabbed from the first column's BTreeMap, since all columns share the same keyset).
4. For each rowid:
   - If a WHERE clause exists, evaluate `eval_predicate` against the row's data.
   - If matched, keep the rowid for later.
5. Sort the matched rowids if ORDER BY is present.
6. Truncate to LIMIT.
7. Render as a prettytable string, column by column via `Table::get_value`.

Full table scan, every time. Index-backed lookups are Phase 3+.

## What Phase 3c changes

Phase 3c (cell-based storage) will:

- Replace `Rc<RefCell<HashMap<String, Row>>>` with a page-oriented layout: each table is a set of pages, each page holds a sequence of variable-length row cells.
- Let rows be read and written one at a time without re-serializing the whole table's bincode blob on every statement.
- Give NULL a proper null-bitmap per cell, ending the `"Null"`-string hack.
- Be the layer the B-Tree (3d) sits on top of.

The `Database`, `Column`, `Index`, and `Value` abstractions will survive; the `Row` enum and the HashMap-of-BTreeMaps inside `Table` won't. Most executor code won't need to change — it already goes through `Table::rowids` and `Table::get_value`/`set_value`/etc, which can be reimplemented over the new storage.
