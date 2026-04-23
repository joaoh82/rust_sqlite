# sqlrite-ffi

C ABI shim around the SQLRite engine. Produces `libsqlrite_c.{so,dylib,dll}` + `libsqlrite_c.a` + `sqlrite.h` — the universal surface every non-Rust SDK (Python / Node.js / Go / raw C) binds against.

## Build

```bash
cargo build --release -p sqlrite-ffi
# → target/release/libsqlrite_c.{so,dylib,dll}
#   target/release/libsqlrite_c.a
#   sqlrite-ffi/include/sqlrite.h   (regenerated via build.rs)
```

The cbindgen header is also committed at [`include/sqlrite.h`](include/sqlrite.h) so downstream C consumers can grab it without running cargo.

## Use from C

```c
#include "sqlrite.h"

int main(void) {
    struct SqlriteConnection *conn;
    if (sqlrite_open_in_memory(&conn) != Ok) return 1;

    sqlrite_execute(conn,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);");
    sqlrite_execute(conn,
        "INSERT INTO users (name) VALUES ('alice');");

    struct SqlriteStatement *stmt;
    sqlrite_query(conn, "SELECT id, name FROM users;", &stmt);
    while (sqlrite_step(stmt) == Row) {
        int64_t id;
        char *name;
        sqlrite_column_int64(stmt, 0, &id);
        sqlrite_column_text(stmt, 1, &name);
        printf("%lld %s\n", (long long)id, name);
        sqlrite_free_string(name);
    }
    sqlrite_finalize(stmt);
    sqlrite_close(conn);
    return 0;
}
```

Full runnable sample + Makefile: [`../examples/c/`](../examples/c/) — `cd ../examples/c && make run` builds the library and the demo end-to-end.

## API shape at a glance

| C function                          | Purpose                                                       |
|-------------------------------------|---------------------------------------------------------------|
| `sqlrite_open(path, &conn)`         | Open/create a file (exclusive lock)                           |
| `sqlrite_open_read_only(path, &conn)` | Open existing file (shared lock)                            |
| `sqlrite_open_in_memory(&conn)`     | Transient in-memory DB                                        |
| `sqlrite_close(conn)`               | Release connection + locks                                    |
| `sqlrite_execute(conn, sql)`        | DDL / DML / BEGIN / COMMIT / ROLLBACK                         |
| `sqlrite_query(conn, sql, &stmt)`   | Compile + run a SELECT; yields a statement handle             |
| `sqlrite_step(stmt)`                | Advance to next row — returns `Row` / `Done` / error          |
| `sqlrite_column_count(stmt, &n)`    | Number of columns in the current row                          |
| `sqlrite_column_name(stmt, i, &s)`  | Column name (caller frees via `sqlrite_free_string`)          |
| `sqlrite_column_int64(stmt, i, &v)` | Integer value                                                 |
| `sqlrite_column_double(stmt, i, &v)`| Double value (Int → Double coerces)                           |
| `sqlrite_column_text(stmt, i, &s)`  | Text value (caller frees)                                     |
| `sqlrite_column_is_null(stmt, i, &b)` | 0/1 flag                                                    |
| `sqlrite_finalize(stmt)`            | Release statement handle                                      |
| `sqlrite_free_string(ptr)`          | Release a string returned by this library                     |
| `sqlrite_in_transaction(conn)`      | 1 inside BEGIN/…/COMMIT, 0 outside                            |
| `sqlrite_is_read_only(conn)`        | 1 if opened read-only                                         |
| `sqlrite_last_error()`              | Thread-local last-error string (do not free)                  |

## Status codes

```
Ok              = 0
Error           = 1   // check sqlrite_last_error() for the message
InvalidArgument = 2   // null pointer / bad UTF-8 / missing NUL
Done            = 101 // sqlrite_step: end of rows
Row             = 102 // sqlrite_step: row available
```

## Memory ownership

- Opaque handles (`SqlriteConnection*`, `SqlriteStatement*`) are caller-owned — close/finalize when done.
- Strings returned by `sqlrite_column_text` / `sqlrite_column_name` are heap-allocated — free via `sqlrite_free_string`.
- `sqlrite_last_error()` returns a thread-local pointer; do *not* free, and don't hang onto it past the next FFI call on the same thread.
- Every input `char *` must be NUL-terminated UTF-8; the library copies what it needs before returning.

## Thread safety

- A single `SqlriteConnection` is `!Sync` — don't share across threads without external synchronization.
- The last-error slot is thread-local, so two threads calling FFI functions concurrently (each on their own connection) don't race on error reporting.

## Why `libsqlrite_c` and not `libsqlrite`?

The Rust crate hierarchy has `sqlrite` as the root engine crate. A cdylib also named `sqlrite` would collide with the rlib at doctest time. `sqlrite_c` disambiguates cleanly and signals "this is the C-ABI surface" at the linker level. SDKs link against `-lsqlrite_c`.
