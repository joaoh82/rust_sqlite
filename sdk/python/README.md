# sqlrite (Python)

Python bindings for [SQLRite](https://github.com/joaoh82/rust_sqlite) — a small, embeddable SQLite clone written in Rust. Shape follows PEP 249 / the stdlib `sqlite3` module, so most callers can pick it up without reading the docs.

## Install

```bash
# From PyPI:
pip install sqlrite

# From source in a clone of the repo:
pip install maturin
cd sdk/python
maturin develop --release
```

## Quick tour

```python
import sqlrite

# File-backed or in-memory (use `":memory:"` to match sqlite3 convention).
conn = sqlrite.connect("foo.sqlrite")
# conn = sqlrite.connect(":memory:")

cur = conn.cursor()
cur.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
cur.execute("INSERT INTO users (name, age) VALUES ('alice', 30)")
cur.execute("INSERT INTO users (name, age) VALUES ('bob', 25)")

cur.execute("SELECT id, name, age FROM users")
for row in cur:
    print(row)  # (1, 'alice', 30), then (2, 'bob', 25)

# Column metadata (PEP 249 `.description`):
for col in cur.description:
    print(col[0])  # 'id', 'name', 'age'

conn.close()
```

### Transactions

```python
with sqlrite.connect("foo.sqlrite") as conn:
    cur = conn.cursor()
    cur.execute("BEGIN")
    cur.execute("INSERT INTO users (name) VALUES ('carol')")
    if looks_good:
        conn.commit()
    else:
        conn.rollback()
# The context manager automatically commits on clean exit
# and rolls back on exception, then closes the connection.
```

### Read-only access

```python
conn = sqlrite.connect_read_only("foo.sqlrite")
# Any write raises sqlrite.SQLRiteError. Multiple read-only
# connections on the same file coexist (shared OS lock).
```

## API surface

| Function / Method                | Purpose                                          |
|----------------------------------|--------------------------------------------------|
| `sqlrite.connect(db)`            | Open or create a file-backed DB (or `:memory:`)  |
| `sqlrite.connect_read_only(db)`  | Open an existing file with a shared lock         |
| `Connection.cursor()`            | Returns a new `Cursor`                           |
| `Connection.execute(sql, ...)`   | Shortcut for `cursor().execute(...)`             |
| `Connection.commit()` / `.rollback()` | Close the current transaction               |
| `Connection.close()`             | Drop the connection (releases OS file lock)      |
| `Connection.in_transaction`      | `bool` — inside a `BEGIN … COMMIT/ROLLBACK`      |
| `Connection.read_only`           | `bool` — opened via `connect_read_only`          |
| `Cursor.execute(sql, params=None)` | Run one statement                             |
| `Cursor.executemany(sql, seq)`   | DB-API placeholder (params deferred)             |
| `Cursor.executescript(sql)`      | `;`-separated batch of statements                |
| `Cursor.fetchone()`              | Next row as a tuple, or `None`                   |
| `Cursor.fetchmany(size=None)`    | Up to `size` more rows as a list of tuples       |
| `Cursor.fetchall()`              | All remaining rows                               |
| `Cursor.description`             | PEP 249 7-tuples per column (name + Nones)       |
| `Cursor.rowcount`                | `-1` (not tracked yet — returns as PEP 249 says) |
| `iter(cursor)`                   | `for row in cursor: …`                           |
| `sqlrite.SQLRiteError`           | All engine failures bubble up as this            |

## Parameter binding

`execute(sql, params)` accepts `None` and empty tuples/lists for DB-API compatibility, but any *non-empty* `params` raises `TypeError` with a clear message — the underlying engine doesn't support prepared-statement parameter binding yet (deferred to Phase 5a.2). For now, inline values into the SQL (with manual escaping).

## Running the tests

```bash
maturin develop
python -m pytest tests/
```

## How this ships

- PyO3 (`abi3-py38`) for the Rust-Python boundary — one wheel works on every CPython 3.8+ release, no per-version rebuild.
- maturin as the build backend, emitting standard `.whl` files that pip can install directly.
- Phase 6f's CI publishes abi3-py38 wheels to PyPI for manylinux x86_64/aarch64, macOS aarch64, and Windows x86_64 (plus an sdist) on every release. OIDC trusted publishing — no long-lived PyPI token in the repo.

## Status

Phase 5c MVP: ✅ — basic CRUD, transactions, context managers, read-only mode, iteration. Parameter binding, CursorRow namedtuples, and type converters (datetime, Decimal) are natural follow-ups.
