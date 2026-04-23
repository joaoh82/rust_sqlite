# sqlrite-wasm

WebAssembly build of the [SQLRite](https://github.com/joaoh82/rust_sqlite) embedded database engine. Runs entirely in a browser tab — no server, no backend — via `wasm-pack` / `wasm-bindgen`.

## Install

```bash
npm install sqlrite-wasm   # once Phase 6e's CI/CD release lands
```

Or build locally from a repo clone:

```bash
# From a repo checkout:
cd sdk/wasm
wasm-pack build --target web --release      # → pkg/
# …or for a bundler (Webpack / Vite):
wasm-pack build --target bundler --release
# …or for Node.js:
wasm-pack build --target nodejs --release
```

## Quick tour

```js
import init, { Database } from 'sqlrite-wasm';

// Async init — fetches the .wasm file and wires up memory.
// Nothing else in the module works until this resolves.
await init();

const db = new Database();   // always in-memory in the WASM build

db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)");
db.exec("INSERT INTO users (name, age) VALUES ('alice', 30)");

const rows = db.query("SELECT id, name, age FROM users");
// → [{ id: 1, name: 'alice', age: 30 }]
```

See [`../../examples/wasm/`](../../examples/wasm/) for a runnable HTML demo — a tiny SQL console that `make build && make serve` spins up on `http://localhost:8080`.

### Transactions

```js
db.exec("BEGIN");
db.exec("INSERT INTO users (name) VALUES ('carol')");
if (looksGood) {
    db.exec("COMMIT");
} else {
    db.exec("ROLLBACK");    // restores pre-BEGIN state
}

console.log(db.inTransaction);  // false once COMMIT / ROLLBACK runs
```

## API surface

| JS                              | Purpose                                              |
|---------------------------------|------------------------------------------------------|
| `new Database()`                | In-memory DB (only mode in the WASM build)           |
| `db.exec(sql)`                  | DDL / DML / BEGIN / COMMIT / ROLLBACK — no return    |
| `db.query(sql)`                 | SELECT — returns `Array<Object>`, one entry per row  |
| `db.columns(sql)`               | Column names a SELECT would produce                  |
| `db.inTransaction` (getter)     | `true` inside BEGIN/…/COMMIT                         |
| `db.readonly` (getter)          | Always `false` (no RO path in WASM)                  |
| `db.free()`                     | Releases the underlying state before GC              |

Rows come back as plain objects keyed by column name, matching the [Node.js SDK's](../nodejs/README.md) shape. Projection order is preserved (`Object.keys(row)` matches the SELECT list).

## Scope of the MVP

- **In-memory only.** `Connection::open(path)` doesn't have a reasonable browser semantic — the OS file locks and `-wal` sidecar that file-backed mode needs don't exist in a tab's sandbox. We only expose `Connection::open_in_memory()`. Persistence via the browser's OPFS (Origin Private File System) is a natural follow-up but out of scope here.
- **No prepared-statement object.** Unlike the Python / Node / Go / Rust SDKs, the WASM build collapses `prepare → step → finalize` into the one-shot `db.query(sql)`. The engine still does the work internally; JS just sees a single call. The added objects + lifetimes don't earn their keep in the in-memory MVP.
- **Parameter binding** follows the same "not yet, 5a.2 will add it" story as every other SDK.

## Build sizes

A release build (`wasm-pack build --target web --release`) produces roughly:

| File                     | Size (before gzip) |
|--------------------------|-------------------|
| `sqlrite_wasm_bg.wasm`   | ~1.8 MB            |
| `sqlrite_wasm.js` (glue) | ~14 KB             |

The wasm gzips to ~500 KB; browsers serve it compressed.

## How this is wired

- Depends on `sqlrite` with `default-features = false`, so the `cli` feature (rustyline + clap + env_logger — not wasm-safe) and `file-locks` feature (fs2 — no POSIX flock in wasm32) are both off. The engine's file-locking code is `#[cfg]`-gated behind `file-locks` and compiles to a no-op when the feature is absent.
- `console_error_panic_hook` (default-on feature) turns Rust panics into readable `console.error` stack traces in devtools.
- Release profile uses `opt-level = "z"` + LTO + `codegen-units = 1` + symbol stripping — wasm binary size is the main cost center on the wire.
- Rows are marshalled to JS via `serde_wasm_bindgen` with `serialize_maps_as_objects(true)` (so each row is a plain JS `Object`, not a `Map`) and `serde_json`'s `preserve_order` feature (so column keys come across in projection order, not alphabetical).

## Status

Phase 5g MVP: ✅ — in-memory CRUD, transactions, columns(), panic hook, serialization behavior matches the Node.js SDK. OPFS-backed persistence, prepared-statement objects, and parameter binding are natural follow-ups.
