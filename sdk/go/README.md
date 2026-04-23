# sqlrite (Go)

A `database/sql`-compatible driver for [SQLRite](https://github.com/joaoh82/rust_sqlite) — a small, embeddable SQLite clone written in Rust. Use it the same way you'd use any other Go SQL driver:

```go
import (
    "database/sql"
    _ "github.com/joaoh82/rust_sqlite/sdk/go"
)

db, _ := sql.Open("sqlrite", "foo.sqlrite")
// or: sql.Open("sqlrite", ":memory:")
```

## Install

The Go module lives at `github.com/joaoh82/rust_sqlite/sdk/go`. Because the binding uses cgo to call into the `libsqlrite_c` shared library shipped by [`sqlrite-ffi`](../../sqlrite-ffi), you need to build that library once before running `go test` / `go run`:

```bash
# From a repo clone:
cargo build --release -p sqlrite-ffi   # produces target/release/libsqlrite_c.{so,dylib,dll}

# Then, from inside your Go project:
go get github.com/joaoh82/rust_sqlite/sdk/go
```

Phase 6e will publish prebuilt `libsqlrite_c` binaries as GitHub Release assets so end users consuming the Go module don't need the Rust toolchain.

## Quick tour

```go
package main

import (
    "database/sql"
    "fmt"
    "log"

    _ "github.com/joaoh82/rust_sqlite/sdk/go"
)

func main() {
    db, err := sql.Open("sqlrite", ":memory:")
    if err != nil {
        log.Fatal(err)
    }
    defer db.Close()

    _, _ = db.Exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
    _, _ = db.Exec("INSERT INTO users (name) VALUES ('alice')")

    rows, _ := db.Query("SELECT id, name FROM users")
    defer rows.Close()
    for rows.Next() {
        var id int64
        var name string
        rows.Scan(&id, &name)
        fmt.Printf("%d: %s\n", id, name)
    }
}
```

### Transactions

```go
tx, _ := db.Begin()
_, _ = tx.Exec("INSERT INTO users (name) VALUES ('carol')")
if looksGood {
    tx.Commit()
} else {
    tx.Rollback() // restores pre-BEGIN snapshot
}
```

### Read-only connections

`database/sql`'s `Open` doesn't have a read-only flag, so we expose a package-level helper:

```go
ro := sqlrite.OpenReadOnly("foo.sqlrite") // returns a *sql.DB
defer ro.Close()
// Reads work; any Exec throws with "read-only" in the message.
```

## API surface

| Symbol                                  | Purpose                                        |
|-----------------------------------------|------------------------------------------------|
| `sqlrite.DriverName`                    | `"sqlrite"` — pass to `sql.Open`               |
| `sqlrite.OpenReadOnly(path)`            | Returns a `*sql.DB` with shared-lock semantics |
| `sql.Open("sqlrite", path)`             | Standard `database/sql` entry point            |
| `*sql.DB.Exec(sql, args...)`            | DDL / DML / BEGIN / COMMIT / ROLLBACK          |
| `*sql.DB.Query(sql, args...)`           | SELECT — returns `*sql.Rows`                   |
| `*sql.DB.Begin()`                       | Start a transaction (default isolation only)   |
| `*sql.Rows.Scan(&dest...)`              | Typed column extraction                        |

All standard `database/sql` features work — `QueryRow`, `Prepare`, `Exec`/`Query` under `*sql.Tx`, context-aware variants (`QueryContext` etc.). Rows come back as primitive Go values: `int64`, `float64`, `string`, `nil` for NULL.

## Parameter binding

`Exec(sql, args...)` / `Query(sql, args...)` accept the standard variadic args for forward compatibility, but any **non-empty** arg slice returns an error — parameter binding isn't in the engine yet (deferred to Phase 5a.2). Inline values into the SQL for now.

## How this works

- Implements `database/sql/driver`'s `Driver`, `Conn`, `Stmt`, `Rows`, `Tx`, and the Context-aware + extended interfaces (`ConnBeginTx`, `ExecerContext`, `QueryerContext`, `Pinger`).
- cgo bridges Go calls into the `sqlrite-ffi` C ABI. Each method acquires a connection-scoped Mutex before touching the C handle — the engine is single-writer per file, so serializing on the Go side maps cleanly to that.
- Column type detection is by attempt: `sqlrite_column_int64` → `_double` → `_text` (the last is lenient and renders Int/Real/Bool via their Display).
- `RunResult.LastInsertId()` and `RowsAffected()` both return 0 today — the engine doesn't track those at the public API layer yet. The shape is reserved so future tracking doesn't break callers.

## Running the tests

```bash
cargo build --release -p sqlrite-ffi   # one-time
cd sdk/go && go test -v ./...
```

## Status

Phase 5e MVP: ✅ — CRUD, transactions, file-backed + read-only, `QueryRow`/`Scan` round-trip, `database/sql`'s context-aware interfaces, error surfacing through the driver layer. Parameter binding, prepared-plan caching, and `LastInsertId`/`RowsAffected` tracking land with the engine-level 5a.2 cursor work.
