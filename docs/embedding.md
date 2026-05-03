# Embedding the SQLRite engine

SQLRite ships as a library that other programs can embed — the REPL and desktop app are just thin UIs over the same core. Phase 5 built out the embedding surface across every reasonable runtime; Phase 7g/7h layered the natural-language `ask()` family + an MCP server on top.

- ✅ **Phase 5a** — stable public Rust API (`Connection` / `Statement` / `Rows` / `Row` / `Value`) plus structured row return. Parameter binding + a streaming cursor abstraction are deferred to **5a.2**.
- ✅ **Phase 5b** — C FFI shim (`libsqlrite_c.{so,dylib,dll}` + cbindgen-generated `sqlrite.h`) that every non-Rust SDK binds against.
- ✅ **Phase 5c – 5e** — Python (PyO3 → PyPI), Node.js (napi-rs → npm), Go (cgo against the FFI shim → git tag) SDKs published to their respective registries.
- ⏳ **Phase 5f** — Rust crate polish (deferred — Phase 6c shipped the actual crates.io publish; 5f's polish work folded into ongoing maintenance).
- ✅ **Phase 5g** — WASM build (`@joaoh82/sqlrite-wasm` on npm) so the engine runs entirely in a browser tab.
- ✅ **Phase 7g** — `ask()` natural-language → SQL family across every embedding surface — see [`ask.md`](ask.md).
- ✅ **Phase 7h** — [`sqlrite-mcp`](mcp.md), a Model Context Protocol stdio server that wraps a database for LLM agents (Claude Code, Cursor, `mcp-inspector`, …) without any custom integration code on the LLM side. Sibling product to the SDKs, not a replacement — use the SDKs when *your* code drives the database, use the MCP server when an *LLM agent* drives it.

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

### Natural-language → SQL — `sqlrite::ask` + `sqlrite-ask`

*Phases 7g.1 + 7g.2.* The companion crate [`sqlrite-ask`](../sqlrite-ask/) provides the LLM-talking machinery (provider adapters, prompt construction, response parsing) over a deliberately small surface — `&str` schema in, generated SQL out. The engine wraps it under a new `ask` feature (default-on) so library users get the ergonomic `Connection::ask` form without composing the schema dump themselves.

```toml
[dependencies]
# `ask` is a default feature on sqlrite-engine; opt out with
# default-features = false if you don't want the LLM stack pulled in.
sqlrite-engine = "0.1"
sqlrite-ask    = "0.1"
```

```rust
use sqlrite::{Connection, ConnectionAskExt};
use sqlrite_ask::{AskConfig, AskResponse};

let conn = Connection::open("foo.sqlrite")?;
let cfg  = AskConfig::from_env()?;          // reads SQLRITE_LLM_API_KEY etc.
let resp: AskResponse = conn.ask("How many users are over 30?", &cfg)?;

println!("Generated SQL: {}", resp.sql);
println!("Rationale: {}",     resp.explanation);
println!("Tokens: in={}, out={}, cache_hit={}",
    resp.usage.input_tokens,
    resp.usage.output_tokens,
    resp.usage.cache_read_input_tokens);

// Caller decides whether to execute the generated SQL — the library
// does NOT auto-execute. SDK convenience wrappers (Python's
// `conn.ask_run()`, Node's `db.askRun()`, etc.) add a one-shot
// generate-and-execute helper, but the default Rust API is
// "generate, return, let me decide".
let mut conn = conn;  // need &mut for execute
let _ = conn.execute(&resp.sql)?;
```

**Where what lives:**

| Crate | Provides |
|---|---|
| `sqlrite-engine` (with `ask` feature) | `sqlrite::ConnectionAskExt`, `sqlrite::ask::ask` / `ask_with_database` / `ask_with_provider` / `ask_with_database_and_provider`, `sqlrite::ask::schema::dump_schema_for_connection` / `_for_database`. Pure engine-side glue: dump schema → call into `sqlrite-ask`. |
| `sqlrite-ask` | `ask_with_schema` / `ask_with_schema_and_provider`, `AskConfig`, `AskResponse`, `AskError`, `Provider` trait + `AnthropicProvider`. Pure `&str` inputs, no engine dep — keeps the LLM stack independently testable + plugable. |

**Provider:** Anthropic only in 7g.1; the `Provider` trait lets OpenAI / Ollama slot in without touching consumers. `AnthropicProvider` does sync `ureq` POSTs to `/v1/messages`. **Defaults:** `claude-sonnet-4-6`, `max_tokens: 1024`, 5-minute prompt-cache TTL on the schema dump (configurable via `AskConfig::cache_ttl` / `SQLRITE_LLM_CACHE_TTL`).

**Why the split** (Phase 7g.2 retro): the REPL binary needed to import the LLM crate to wire up `.ask`, but `sqlrite-ask` 0.1.18 imported `sqlrite-engine` for the `Connection` integration. That's a cargo cycle (`engine[bin] → sqlrite-ask → engine[lib]`) — even with `optional = true`, the static cycle detector rejects the graph. Flipping the dep direction broke it: `sqlrite-ask` is pure now, the engine carries the integration weight behind a feature flag. See `docs/roadmap.md` for the full retrospective.

**REPL surface** *(7g.2)*: type `.ask <question>` at the prompt. Prints generated SQL + rationale, asks `Run? [Y/n]`, executes through the same `process_command` pipeline as a typed statement on confirm. Requires `SQLRITE_LLM_API_KEY`.

**Per-product wrappers shipped across 7g.3 – 7g.8** — desktop "Ask…" composer, Python/Node/Go SDKs' `conn.ask()` / `db.ask()`, WASM SDK with the split JS-callback shape (so the API key stays out of the browser tab; see [`ask-backend-examples.md`](ask-backend-examples.md) for backend templates), and the MCP `ask` tool exposed by [`sqlrite-mcp`](mcp.md). [`ask.md`](ask.md) is the canonical reference covering all of them.

## The C FFI (Phase 5b)

The `sqlrite-ffi/` crate wraps the Rust API in a C ABI that every non-Rust SDK binds against. Build the shared library:

```bash
cargo build --release -p sqlrite-ffi
# produces target/release/libsqlrite_c.{so,dylib,dll} + libsqlrite_c.a
```

The matching `sqlrite.h` is generated by `cbindgen` at build time and committed at `sqlrite-ffi/include/sqlrite.h` so consumers can grab it without running cargo themselves.

```c
#include "sqlrite.h"

struct SqlriteConnection *conn;
sqlrite_open_in_memory(&conn);
sqlrite_execute(conn, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
sqlrite_execute(conn, "INSERT INTO users (name) VALUES ('alice')");

struct SqlriteStatement *stmt;
sqlrite_query(conn, "SELECT id, name FROM users", &stmt);
while (sqlrite_step(stmt) == Row) {
    int64_t id;
    char *name;
    sqlrite_column_int64(stmt, 0, &id);
    sqlrite_column_text(stmt, 1, &name);
    printf("%lld %s\n", (long long)id, name);
    sqlrite_free_string(name);  // heap-allocated text columns must be freed
}
sqlrite_finalize(stmt);
sqlrite_close(conn);
```

The runnable [`examples/c/hello.c`](../examples/c/hello.c) walks through all of this end-to-end (`cd examples/c && make run`).

### Memory rules

| API                       | Ownership of return                          |
|---------------------------|----------------------------------------------|
| `sqlrite_open*`           | caller frees via `sqlrite_close`             |
| `sqlrite_query`           | caller frees via `sqlrite_finalize`          |
| `sqlrite_column_text`     | caller frees via `sqlrite_free_string`       |
| `sqlrite_column_name`     | caller frees via `sqlrite_free_string`       |
| `sqlrite_last_error`      | library-owned thread-local, do *not* free   |

### Error handling

Every mutating call returns a `SqlriteStatus` int. `Ok = 0`; nonzero means check `sqlrite_last_error()` for the descriptive message. `sqlrite_step` additionally returns `Row` (102) to signal a row is available or `Done` (101) when the query is exhausted.

## Language SDKs (Phases 5c – 5g)

Shape stays consistent across bindings — `connect(path)` → `cursor`/`prepare` → `execute` / `query` / iteration, plus explicit transaction statements.

### Python (Phase 5c) ✅

`sdk/python/` — PyO3 (`abi3-py38`) + maturin, PEP 249 / stdlib-`sqlite3` shape:

```python
import sqlrite

with sqlrite.connect("foo.sqlrite") as conn:
    cur = conn.cursor()
    cur.execute("INSERT INTO users (name) VALUES ('alice')")
    for row in cur.execute("SELECT id, name FROM users"):
        print(row)  # tuples
```

The Python binding wraps the Rust `Connection` directly (not via the C FFI) — PyO3 marshals types cheaper than a C round-trip. Build via `cd sdk/python && maturin develop`; tests via `python -m pytest sdk/python/tests/`. Phase 6f publishes abi3-py38 wheels to PyPI on every release via OIDC trusted publishing.

Full API tour: [`sdk/python/README.md`](../sdk/python/README.md); runnable walkthrough: [`examples/python/hello.py`](../examples/python/hello.py).

### Node.js (Phase 5d) ✅

`sdk/nodejs/` — napi-rs 2.x (N-API v9, Node 18+), `better-sqlite3`-style sync API:

```js
import { Database } from 'sqlrite';

const db = new Database('foo.sqlrite');
db.prepare("INSERT INTO users (name) VALUES ('alice')").run();
for (const row of db.prepare('SELECT id, name FROM users').all()) {
  console.log(row); // { id: 1, name: 'alice' } — object per row
}
db.close();
```

Unlike the C SDK, the Node.js binding wraps the Rust `Connection` directly (via napi-rs, no C FFI hop). Phase 6g publishes prebuilt `.node` binaries per platform under the `@joaoh82/sqlrite` scope on npm, with sigstore-signed provenance attestations via OIDC trusted publishing — no `node-gyp` install step on the user side. TypeScript definitions (`index.d.ts`) auto-generated from the Rust source.

Full API tour: [`sdk/nodejs/README.md`](../sdk/nodejs/README.md); runnable walkthrough: [`examples/nodejs/hello.mjs`](../examples/nodejs/hello.mjs).

### Go (Phase 5e) ✅

`sdk/go/` — cgo-linked against `libsqlrite_c` from Phase 5b, implementing `database/sql/driver`:

```go
import (
    "database/sql"
    _ "github.com/joaoh82/rust_sqlite/sdk/go"
)

db, _ := sql.Open("sqlrite", "foo.sqlrite")
_, _ = db.Exec("INSERT INTO users (name) VALUES ('alice')")
rows, _ := db.Query("SELECT id, name FROM users")
for rows.Next() {
    var id int64; var name string
    rows.Scan(&id, &name)
}
```

Unlike the Python and Node.js bindings, Go goes through the C ABI — cgo is Go's FFI shape, so leveraging the existing `sqlrite-ffi` shim is natural and free. The driver implements every major `database/sql/driver` interface (`Driver`, `Conn`, `Stmt`, `Rows`, `Tx`, plus the context-aware variants), so every standard library construct works: `QueryRow`, `Prepare`, transactions via `db.Begin()`, `*sql.Stmt.ExecContext`, etc.

Full API tour: [`sdk/go/README.md`](../sdk/go/README.md); runnable walkthrough: [`examples/go/hello.go`](../examples/go/hello.go).

### WASM (Phase 5g) ✅

`sdk/wasm/` — `wasm-bindgen` compiles the Rust engine directly to `wasm32-unknown-unknown`. The whole database runs in a browser tab:

```js
import init, { Database } from '@joaoh82/sqlrite-wasm';
await init();

const db = new Database();
db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
db.exec("INSERT INTO users (name) VALUES ('alice')");
const rows = db.query("SELECT id, name FROM users");
// → [{ id: 1, name: 'alice' }]
```

**In-memory only** in the MVP — file-backed mode needs OS file locks and a `-wal` sidecar that don't exist in a tab's sandbox. OPFS-backed persistence is a natural follow-up.

Build locally via `wasm-pack build --target web --release` (or `bundler` / `nodejs`). Phase 6h publishes `@joaoh82/sqlrite-wasm` (bundler target) to npm via `wasm-pack build` + `npm publish` (OIDC trusted publisher) on every release.

The root engine crate is feature-gated (`cli` for rustyline/clap/env_logger; `file-locks` for fs2) so `default-features = false` strips out everything that wouldn't compile on `wasm32-unknown-unknown`.

Full API tour: [`sdk/wasm/README.md`](../sdk/wasm/README.md); runnable browser demo: [`examples/wasm/`](../examples/wasm/) (`cd examples/wasm && make` spins up a local SQL console).

---

A fix in the Rust engine propagates through one wrapper update per language rather than four separate binding rewrites.

## Distribution (Phase 6)

Phase 6 lands GitHub Actions CI + release automation:

- **crates.io** — `sqlrite-engine` crate (published under a different name from the `sqlrite` lib target because the short name was already taken; users `cargo add sqlrite-engine` but still write `use sqlrite::…`)
- **PyPI** — `sqlrite` wheels (manylinux x86_64/aarch64, macOS universal, Windows x86_64)
- **npm** — `@joaoh82/sqlrite` (Node) + `@joaoh82/sqlrite-wasm` (browser) packages
- **Go modules** — `sdk/go/v*` git tags
- **GitHub Releases** — Tauri desktop builds + C FFI prebuilt libraries

A single `git tag v0.2.0 && git push --tags` turns into the full cross-platform, cross-language release.
