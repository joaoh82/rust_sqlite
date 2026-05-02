# @joaoh82/sqlrite-wasm

WebAssembly build of the [SQLRite](https://github.com/joaoh82/rust_sqlite) embedded database engine. Runs entirely in a browser tab — no server, no backend — via `wasm-pack` / `wasm-bindgen`.

> **Why the scoped name?** Same reason as `@joaoh82/sqlrite` (the Node SDK): the unscoped form risks tripping npm's similarity check against `sqlite-wasm` / `sqlite`. Scoping under `@joaoh82` is the standard Node ecosystem pattern (`@napi-rs/*`, `@swc/*`, etc.).

## Install

```bash
npm install @joaoh82/sqlrite-wasm
```

The npm package ships the `bundler` target (webpack / vite / rollup / parcel-friendly). For other build targets, build locally from a repo clone:

```bash
# From a repo checkout:
cd sdk/wasm
wasm-pack build --target web --release      # → pkg/, ES modules for direct browser use
# …or for a bundler (Webpack / Vite):
wasm-pack build --target bundler --release
# …or for Node.js:
wasm-pack build --target nodejs --release
```

## Quick tour

```js
import init, { Database } from '@joaoh82/sqlrite-wasm';

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

### Vector columns + KNN (Phase 7a–7d)

`VECTOR(N)` storage class plus `vec_distance_l2` / `vec_distance_cosine` / `vec_distance_dot` distance functions. Vector literals are bracket arrays.

```js
db.exec("CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(384))");
db.exec("INSERT INTO docs (id, embedding) VALUES (1, [0.1, 0.2, ..., 0.0])");

const top10 = db.query(`
  SELECT id FROM docs
   ORDER BY vec_distance_l2(embedding, [0.1, 0.2, ..., 0.0])
   LIMIT 10
`);
```

HNSW indexes work in the WASM build too — CPU-only, no SIMD on `wasm32`, but algorithmically identical:

```js
db.exec("CREATE INDEX idx_docs_emb ON docs USING hnsw (embedding)");
```

### JSON columns (Phase 7e)

`JSON` / `JSONB` columns are validated at INSERT time. Use `json_extract` / `json_type` / `json_array_length` / `json_object_keys`. Path subset: `$`, `.key`, `[N]`, chained.

```js
db.exec("CREATE TABLE events (id INTEGER PRIMARY KEY, payload JSON)");
db.exec(`INSERT INTO events (payload) VALUES ('{"user": {"name": "alice"}, "score": 42}')`);

const rows = db.query(`SELECT json_extract(payload, '$.user.name') AS name FROM events`);
// → [{ name: 'alice' }]
```

### Natural-language → SQL (Phase 7g.7)

Per Phase 7 plan Q9, the WASM SDK has a **different `ask()` shape** than the other SDKs. The WASM module does the schema-aware prompt construction in-page, but **does NOT make the HTTP request itself.** The caller's JS code does the call, typically routed through their own backend.

#### Why this design

Two reasons direct browser-to-LLM calls don't work:

1. **CORS.** Browsers block direct cross-origin POSTs from a WASM module to `api.anthropic.com` / `api.openai.com` unless the LLM provider serves CORS headers. They don't, by design — they don't want users embedding API keys in client-side JS.
2. **API key exposure.** Even if CORS were OK, putting the API key into a WASM-loaded page exposes it to anyone who opens devtools.

Both problems disappear server-side. Node, Python, Go, Desktop (Tauri runs the call in the Rust backend, not the webview) all do the HTTP from a trusted process. WASM solves it with a split: the browser tab does the prompt building (it has the schema and the rules), then hands the request to the user's backend which holds the API key.

#### Two-step API

```js
import init, { Database } from '@joaoh82/sqlrite-wasm';
await init();

const db = new Database();
db.exec(`CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)`);

// Step 1 — build the LLM-API request body in the browser.
const payload = db.askPrompt('How many users are over 30?');
// payload looks like:
// {
//   model: 'claude-sonnet-4-6',
//   max_tokens: 1024,
//   system: [
//     { type: 'text', text: '<rules block>' },
//     { type: 'text', text: '<schema>...</schema>',
//       cache_control: { type: 'ephemeral' } }
//   ],
//   messages: [{ role: 'user', content: 'How many users are over 30?' }]
// }

// Step 2 — your backend forwards this to Anthropic with your API key.
const apiResponse = await fetch('/api/llm/complete', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(payload),
}).then(r => r.text());     // raw response body string

// Step 3 — hand the raw API response back to WASM.
const result = db.askParse(apiResponse);
// → { sql: 'SELECT id, name FROM users WHERE age > 30',
//     explanation: 'Counts users older than thirty.',
//     usage: { inputTokens, outputTokens, cacheCreationInputTokens, cacheReadInputTokens } }

// Now run the SQL however you want.
const rows = db.query(result.sql);
```

#### Backend proxy (the bit you write)

The WASM SDK is provider-agnostic at the JS boundary, but `askPrompt`'s default output shape is **Anthropic's `/v1/messages` body**, so you can forward it as-is to Anthropic. Your backend just needs to:
1. Receive the JSON payload from the browser.
2. Add the `x-api-key` header from your secure storage.
3. POST to `https://api.anthropic.com/v1/messages`.
4. Pipe the response body back to the browser.

A minimal Node/Express version:

```js
import express from 'express';
const app = express();
app.use(express.json());

app.post('/api/llm/complete', async (req, res) => {
  const upstream = await fetch('https://api.anthropic.com/v1/messages', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-api-key': process.env.ANTHROPIC_API_KEY,
      'anthropic-version': '2023-06-01',
    },
    body: JSON.stringify(req.body),
  });
  res.status(upstream.status);
  res.set('content-type', 'application/json');
  res.send(await upstream.text());
});

app.listen(3000);
```

A minimal Cloudflare Worker / Vercel Edge function is essentially identical — read JSON in, forward with the key header, pass the response body through. **The API key stays on the server**; the browser never sees it.

#### `askPrompt` options

```js
import { AskPromptOptions } from '@joaoh82/sqlrite-wasm';

const opts = new AskPromptOptions();
opts.model = 'claude-haiku-4-5';   // default: 'claude-sonnet-4-6'
opts.max_tokens = 512;             // default: 1024
opts.cache_ttl = '1h';             // default: '5m', also '1h' or 'off'

const payload = db.askPrompt('How many users?', opts);
```

#### Using a non-Anthropic provider on your backend

`askPrompt` produces an Anthropic-shaped body. If your backend talks to OpenAI / Ollama / etc., translate the body server-side before forwarding (the `system` blocks → `system` message, `messages` array stays the same shape). The token-usage fields in `askParse`'s output read from Anthropic's `usage` shape too — for non-Anthropic responses, your backend can normalize the response into that shape before returning it to the browser.

OpenAI / Ollama / Together / ... bindings are tracked in the broader Phase 7 roadmap (`docs/phase-7-plan.md` Q4); the WASM SDK's split design means provider variety lives entirely on your backend, with no SDK changes needed.

#### Verifying prompt-cache hits

The `result.usage.cacheReadInputTokens` field in `askParse`'s output reports tokens served from Anthropic's prompt cache. After the first `askPrompt` call against a given schema, repeat calls within 5 minutes (1 hour with `cacheTtl: '1h'`) should show non-zero `cacheReadInputTokens`. If it stays zero, something in the prefix is invalidating the cache — most likely a timestamp / UUID / non-deterministic field bleeding into the system blocks. The WASM SDK builds the same schema dump the other SDKs do (alphabetically sorted, byte-stable), so as long as your DB schema doesn't change between calls, caching should work.

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
