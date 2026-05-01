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

Phase 6i ships prebuilt `libsqlrite_c` tarballs as GitHub Release assets on every release, so end users consuming the Go module don't need the Rust toolchain. Each release at `sdk/go/v<V>` includes per-platform tarballs (Linux x86_64/aarch64, macOS aarch64, Windows x86_64) you can extract and point cgo at via `CGO_CFLAGS` / `CGO_LDFLAGS`.

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

### Vector columns + KNN (Phase 7a–7d)

`VECTOR(N)` storage class plus `vec_distance_l2` / `vec_distance_cosine` / `vec_distance_dot` distance functions. Vector literals are JSON-style bracket arrays `[0.1, 0.2, ...]`. Today the Go side bridges them as text — `database/sql` doesn't yet have a typed accessor for vectors:

```go
db.Exec(`CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(384))`)
db.Exec(`INSERT INTO docs (id, embedding) VALUES (1, [0.1, 0.2, ..., 0.0])`)

rows, _ := db.Query(`
    SELECT id FROM docs
     ORDER BY vec_distance_l2(embedding, [0.1, 0.2, ..., 0.0])
     LIMIT 10
`)
defer rows.Close()
for rows.Next() {
    var id int64
    rows.Scan(&id)
}
```

For larger collections, build an HNSW index — the executor uses it automatically:

```go
db.Exec(`CREATE INDEX idx_docs_emb ON docs USING hnsw (embedding)`)
```

### JSON columns (Phase 7e)

`JSON` (and `JSONB` as an alias) columns are validated at INSERT/UPDATE time. Read with `json_extract` / `json_type` / `json_array_length` / `json_object_keys`. Path subset: `$`, `.key`, `[N]`, chained.

```go
db.Exec(`CREATE TABLE events (id INTEGER PRIMARY KEY, payload JSON)`)
db.Exec(`INSERT INTO events (payload) VALUES ('{"user": {"name": "alice"}, "score": 42}')`)

var name string
db.QueryRow(`SELECT json_extract(payload, '$.user.name') FROM events`).Scan(&name)
fmt.Println(name) // alice
```

> `json_object_keys` returns a JSON-array text rather than a table-valued result (set-returning functions aren't supported yet).

### Natural-language → SQL (Phase 7g.6)

`sqlrite.Ask(db, question, *AskConfig)` generates SQL via the configured LLM provider (Anthropic by default). `sqlrite.AskRun(db, question, *AskConfig)` is the convenience that calls `Ask` then immediately executes — returns `*sql.Rows` ready for iteration.

```go
import (
    "database/sql"
    sqlrite "github.com/joaoh82/rust_sqlite/sdk/go"
)

db, _ := sql.Open("sqlrite", "foo.sqlrite")
db.Exec(`CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)`)
db.Exec(`INSERT INTO users (name, age) VALUES ('alice', 30)`)

// Path 1: nil cfg → reads SQLRITE_LLM_API_KEY etc. from env.
resp, err := sqlrite.Ask(db, "How many users are over 30?", nil)
fmt.Println(resp.SQL)          // "SELECT COUNT(*) FROM users WHERE age > 30"
fmt.Println(resp.Explanation)  // "Counts users older than thirty."

// Path 2: explicit per-call config.
cfg := &sqlrite.AskConfig{
    APIKey:    "sk-ant-...",
    Model:     "claude-haiku-4-5",
    MaxTokens: 512,
    CacheTTL:  "1h",          // "5m" (default) | "1h" | "off"
}
resp, _ := sqlrite.Ask(db, "list users over 30", cfg)

// Caller decides whether to run the SQL — Ask does NOT auto-execute.
rows, _ := db.Query(resp.SQL)
defer rows.Close()

// Or one-shot:
rows, _ := sqlrite.AskRun(db, "list users", nil)
```

#### Configuration

Three precedence layers — explicit-wins:

1. **Per-call config**: `sqlrite.Ask(db, "...", &sqlrite.AskConfig{APIKey: "..."})`
2. **Environment vars**: `SQLRITE_LLM_PROVIDER` / `_API_KEY` / `_MODEL` / `_MAX_TOKENS` / `_CACHE_TTL` — picked up automatically when cfg is nil
3. **Built-in defaults**: `anthropic` / `claude-sonnet-4-6` / `1024` / `5m`

The config flows across cgo as a JSON string (`"api_key"`, `"model"`, etc. — snake_case, matching the FFI ABI). Adding fields later is non-breaking; older bindings ignore unknown JSON keys.

#### Context-aware variants

```go
// Pass a context for connection-pool acquisition. The HTTP call to
// the LLM is currently uncancellable (the FFI doesn't expose a
// cancel hook yet); the ctx flows to db.Conn(ctx) and to the
// db.QueryContext call inside AskRunContext.
resp, err := sqlrite.AskContext(ctx, db, "list users", cfg)
rows, err := sqlrite.AskRunContext(ctx, db, "list users", cfg)
```

#### Defaults

`Provider="anthropic"`, `Model="claude-sonnet-4-6"`, `MaxTokens=1024`, `CacheTTL="5m"`. The schema dump goes inside an Anthropic prompt-cache breakpoint — repeat asks against the same DB hit the cache (verify via `resp.Usage.CacheReadInputTokens`).

#### Errors

Missing API key → `error` with message `"sqlrite: ask: missing API key (set SQLRITE_LLM_API_KEY ...)"`. API errors (4xx/5xx) include the status code + Anthropic's structured error type+message. `AskRun()` on an empty SQL response (model declined) returns an error rather than executing the empty string.

#### What `AskResponse` carries

```go
type AskResponse struct {
    SQL         string
    Explanation string
    Usage       AskUsage
}

type AskUsage struct {
    InputTokens              uint64
    OutputTokens             uint64
    CacheCreationInputTokens uint64
    CacheReadInputTokens     uint64
}
```

`AskConfig.String()` deliberately omits the API key value — `fmt.Println(cfg)` shows `apiKey=<set>` or `apiKey=<unset>` without leaking the secret. There's no separate `cfg.HasAPIKey()` method because Go's zero-value semantics make `cfg.APIKey != ""` the idiomatic check.

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
