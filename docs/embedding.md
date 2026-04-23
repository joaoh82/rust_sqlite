# Embedding the SQLRite engine

SQLRite ships as a library that other programs can embed — the REPL and desktop app are just thin UIs over the same core. Phase 5 builds out the embedding surface:

- **Phase 5a** — a stable public Rust API (`Connection` / `Statement` / `Rows` / `Row` / `Value`) plus structured row return. ✅ **Landed.**
- **Phase 5b** — a C FFI shim (`libsqlrite.{so,dylib,dll}` + generated `sqlrite.h`) that every non-Rust SDK binds against.
- **Phase 5c – 5f** — Python, Node.js, Go, Rust SDKs published to their respective registries.
- **Phase 5g** — WASM build so the engine runs entirely in a browser tab.

See [roadmap.md](roadmap.md) for the detailed Phase 5 breakdown.

## The Rust public API (Phase 5a)

```rust
use sqlrite::Connection;

// Open a file-backed connection (exclusive lock, auto-save on every
// write). File is materialized if it doesn't exist.
let mut conn = Connection::open("foo.sqlrite")?;

// …or open read-only (shared lock, multi-reader safe):
let ro = Connection::open_read_only("foo.sqlrite")?;

// …or spin up a transient in-memory DB (no file, no locks):
let mem = Connection::open_in_memory()?;

// `execute` — parses and runs one SQL statement. Returns the status
// message the engine produced ("INSERT Statement executed." etc.).
conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);")?;
conn.execute("INSERT INTO users (name) VALUES ('alice');")?;

// `prepare` + `query` — typed row iteration.
let mut stmt = conn.prepare("SELECT id, name FROM users;")?;
let mut rows = stmt.query()?;
while let Some(row) = rows.next()? {
    let id: i64 = row.get(0)?;
    let name: String = row.get_by_name("name")?;
    println!("{id}: {name}");
}
```

### Typed row access

`Row::get::<T>(idx)` and `Row::get_by_name::<T>(name)` both go through the `FromValue` trait. Built-in impls cover:

- `i64`, `f64`, `String`, `bool`
- `Option<T>` — NULL columns resolve to `None`
- raw `Value` — fall-through for untyped access

`FromValue` is `pub trait`, so downstream crates can impl it for their own types (custom enums, chrono timestamps, etc.) without a PR to SQLRite.

### Transactions through the public API

Transactions go through the same `execute` call as everything else — `BEGIN`, `COMMIT`, and `ROLLBACK` are just SQL statements. `Connection::in_transaction()` exposes the flag for callers that want to branch on it.

```rust
conn.execute("BEGIN;")?;
conn.execute("INSERT INTO users (name) VALUES ('bob');")?;
conn.execute("INSERT INTO users (name) VALUES ('carol');")?;
if looks_good {
    conn.execute("COMMIT;")?;   // one WAL commit frame for both inserts
} else {
    conn.execute("ROLLBACK;")?; // restores pre-BEGIN snapshot
}
```

See [Phase 4f notes in roadmap.md](roadmap.md) for the snapshot semantics and the auto-rollback-on-failed-COMMIT guarantee.

### What's deferred

- **Parameter binding.** `stmt.query(&[&"alice"])` is the intended shape but the current implementation takes no arguments — use string interpolation for now. Parameter binding lands with the cursor refactor.
- **Cursor abstraction.** The Pager still eagerly loads every row at open time; `Rows` today wraps an in-memory `Vec`. Phase 5a's follow-up refactor streams rows through the B-Tree on demand — same public API, much lower memory for big SELECTs.

## Non-Rust languages (Phases 5b – 5g)

Shape stays consistent across bindings — `connect(path)` → `prepare(sql)` → `execute()` / `all()` / iteration, plus explicit transaction statements. Each SDK's README will show the language-idiomatic API; see `examples/{python,nodejs,go,wasm}/` as those land.

The C FFI is the ABI every non-Rust binding shares:

```c
#include "sqlrite.h"
SqlriteConnection *conn;
sqlrite_open("foo.sqlrite", &conn);
sqlrite_execute(conn, "INSERT INTO users (name) VALUES ('alice')");
sqlrite_close(conn);
```

Opaque pointer types, explicit `_close` calls, UTF-8 strings, C error codes. Safe to call from any language with a C FFI.

## Distribution (Phase 6)

Phase 6 lands GitHub Actions CI + release automation:

- **crates.io** — `sqlrite` crate
- **PyPI** — `sqlrite` wheels (manylinux x86_64/aarch64, macOS universal, Windows x86_64)
- **npm** — `sqlrite` (Node) + `sqlrite-wasm` (browser) packages
- **Go modules** — `sdk/go/v*` git tags
- **GitHub Releases** — Tauri desktop builds + C FFI prebuilt libraries

A single `git tag v0.2.0 && git push --tags` turns into the full cross-platform, cross-language release.
