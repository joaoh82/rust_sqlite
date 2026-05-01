# @joaoh82/sqlrite (Node.js)

Node.js bindings for [SQLRite](https://github.com/joaoh82/rust_sqlite) — a small, embeddable SQLite clone written in Rust. Shape follows [`better-sqlite3`](https://github.com/WiseLibs/better-sqlite3) (sync API, row-as-object), so Node developers who've used that library can pick this up without reading the docs.

> **Why the scoped name?** npm's registry rejected the unscoped `sqlrite` name as too similar to the existing `sqlite` / `sqlite3` packages. Scoping under `@joaoh82` (my npm user scope) bypasses that check cleanly — same pattern as `@napi-rs/canvas`, `@swc/core`, `@aws-sdk/client-s3`.

## Install

```bash
npm install @joaoh82/sqlrite

# From source in a clone of the repo:
cd sdk/nodejs
npm install
npm run build         # produces sqlrite.<platform>-<arch>.node
```

## Quick tour

```js
import { Database } from '@joaoh82/sqlrite';

// File-backed or in-memory (use `":memory:"` to match better-sqlite3).
const db = new Database('foo.sqlrite');
// const db = new Database(':memory:');

db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)');
db.prepare("INSERT INTO users (name, age) VALUES ('alice', 30)").run();

// Rows come back as objects keyed by column name.
const rows = db.prepare('SELECT id, name, age FROM users').all();
// → [{ id: 1, name: 'alice', age: 30 }]

const first = db.prepare('SELECT id, name FROM users').get();
// → { id: 1, name: 'alice' }

for (const row of db.prepare('SELECT id, name FROM users').iterate()) {
  console.log(row);
}

db.close();
```

### Transactions

```js
db.exec('BEGIN');
db.exec("INSERT INTO users (name) VALUES ('carol')");
if (looksGood) {
  db.exec('COMMIT');
} else {
  db.exec('ROLLBACK');
}

// `inTransaction` is a live getter:
db.exec('BEGIN');
console.log(db.inTransaction); // → true
db.exec('COMMIT');
console.log(db.inTransaction); // → false
```

### Read-only access

```js
const ro = Database.openReadOnly('foo.sqlrite');
console.log(ro.readonly); // → true

// Any write throws:
ro.exec('INSERT INTO users (name) VALUES (...)'); // → Error: ...read-only...
```

### Vector columns + KNN (Phase 7a–7d)

`VECTOR(N)` storage class plus `vec_distance_l2` / `vec_distance_cosine` / `vec_distance_dot` distance functions. Vector literals are bracket arrays: `[0.1, 0.2, ...]`.

```js
db.exec('CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(384))');
db.prepare("INSERT INTO docs (id, embedding) VALUES (1, [0.1, 0.2, ..., 0.0])").run();

const nearest = db
  .prepare(`
    SELECT id FROM docs
     ORDER BY vec_distance_l2(embedding, [0.1, 0.2, ..., 0.0])
     LIMIT 10
  `)
  .all();
```

Build an HNSW index for sub-linear KNN — the executor probes it automatically when the query shape matches:

```js
db.exec('CREATE INDEX idx_docs_emb ON docs USING hnsw (embedding)');
```

### JSON columns (Phase 7e)

`JSON` (and `JSONB` as an alias) columns store text, validated at INSERT/UPDATE time. Read with `json_extract` / `json_type` / `json_array_length` / `json_object_keys`. Path subset: `$`, `.key`, `[N]`, chained.

```js
db.exec('CREATE TABLE events (id INTEGER PRIMARY KEY, payload JSON)');
db.prepare(
  `INSERT INTO events (payload) VALUES ('{"user": {"name": "alice"}, "score": 42}')`
).run();

const row = db
  .prepare("SELECT json_extract(payload, '$.user.name') AS name FROM events")
  .get();
// → { name: 'alice' }
```

> `json_object_keys` returns a JSON-array text rather than a table-valued result (set-returning functions aren't supported yet).

### Natural-language → SQL (Phase 7g.5)

`db.ask(question)` generates SQL from a natural-language question via the configured LLM provider (Anthropic by default). `db.askRun(question)` is the convenience that calls `ask()` then immediately executes the generated SQL — returns rows directly.

```js
import { Database } from '@joaoh82/sqlrite';

const db = new Database('foo.sqlrite');
db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)');
db.exec("INSERT INTO users (name, age) VALUES ('alice', 30)");

// Reads SQLRITE_LLM_API_KEY from the environment.
const resp = db.ask('How many users are over 30?');
console.log(resp.sql);            // "SELECT COUNT(*) FROM users WHERE age > 30"
console.log(resp.explanation);    // "Counts users over the age threshold."

// Caller decides whether to run the SQL — ask() does NOT auto-execute.
const rows = db.prepare(resp.sql).all();
console.log(rows);

// Or one-shot:
const rows2 = db.askRun('list all users');
```

#### Configuration

Three precedence layers — explicit-wins:

1. **Per-call config**: `db.ask('...', new AskConfig({ apiKey: '...' }))`
2. **Per-connection config**: `db.setAskConfig(cfg)` — applies to all subsequent `ask()` / `askRun()` calls on this database
3. **Environment vars** (zero-config fallback): `SQLRITE_LLM_PROVIDER` / `_API_KEY` / `_MODEL` / `_MAX_TOKENS` / `_CACHE_TTL`

```js
import { Database, AskConfig } from '@joaoh82/sqlrite';

// Path 1: env (zero config) — same env vars as REPL/Desktop/Python SDK
const resp = db.ask('how many users?');

// Path 2: explicit per-call (highest precedence)
const cfg = new AskConfig({
  apiKey: 'sk-ant-...',
  model: 'claude-haiku-4-5',
  maxTokens: 512,
  cacheTtl: '1h',          // "5m" (default) | "1h" | "off"
});
const resp = db.ask('how many users?', cfg);

// Path 3: per-connection (set once, reuse)
db.setAskConfig(cfg);
const resp1 = db.ask('how many users?');
const resp2 = db.ask('count by age');

// Or build from env explicitly:
const cfg = AskConfig.fromEnv();
```

#### Defaults

`provider="anthropic"`, `model="claude-sonnet-4-6"`, `maxTokens=1024`, `cacheTtl="5m"`. The schema dump goes inside an Anthropic prompt-cache breakpoint — repeat asks against the same DB hit the cache (verify via `resp.usage.cacheReadInputTokens`).

#### Errors

Missing API key throws `Error("missing API key (set SQLRITE_LLM_API_KEY ...)")`. API errors (4xx/5xx) bubble up as `Error` with the status code + Anthropic's structured error message. `askRun()` on an empty SQL response (model declined) throws with the model's explanation rather than executing the empty string.

#### What `AskResponse` carries

```ts
interface AskResponse {
  sql: string;              // generated SQL, or '' if model declined
  explanation: string;      // one-sentence rationale (may be empty)
  usage: AskUsage;
}

interface AskUsage {
  inputTokens: number;
  outputTokens: number;
  cacheCreationInputTokens: number;
  cacheReadInputTokens: number;
}
```

`AskConfig.toString()` deliberately omits the API key value — printing config in `console.log` won't leak the secret. Shows `apiKey=<set>` or `apiKey=null`.

## API surface

| JS                                   | Purpose                                        |
|--------------------------------------|------------------------------------------------|
| `new Database(path)`                 | Open/create file-backed DB (or `":memory:"`)   |
| `Database.openReadOnly(path)`        | Shared-lock read-only view                     |
| `db.exec(sql)`                       | DDL / DML / BEGIN / COMMIT / ROLLBACK          |
| `db.prepare(sql)` → `Statement`      | Compile a statement for later execution        |
| `db.close()`                         | Release file lock                              |
| `db.inTransaction` (getter)          | Boolean — inside a BEGIN/…/COMMIT              |
| `db.readonly` (getter)               | Boolean — opened via `openReadOnly`            |
| `stmt.run(params?)`                  | Execute a non-query statement                  |
| `stmt.get(params?)`                  | First row as object, or `null`                 |
| `stmt.all(params?)`                  | All rows as array of objects                   |
| `stmt.iterate(params?)`              | Iterate rows (`for … of`); returns array today |
| `stmt.columns()`                     | Projection-order column names                  |

Every error surfaces as a JS `Error` with the Rust engine's message in `.message`.

## Parameter binding

`run(params)` / `get(params)` / `all(params)` / `iterate(params)` accept `undefined`, `null`, or an empty array for DB-API-style forward compatibility, but any **non-empty** array throws — parameter binding isn't in the engine yet (deferred to Phase 5a.2). Inline values into the SQL string for now (with manual escaping).

## How this ships

- [napi-rs](https://napi.rs/) (N-API v9 → Node 18+) for the Rust↔JS boundary.
- Prebuilt `.node` binaries per platform — no `node-gyp` dance on install. Phase 6g's CI publishes them to npm under the `@joaoh82/sqlrite` scope for Linux x86_64/aarch64, macOS aarch64, Windows x86_64. OIDC trusted publishing — no long-lived `NPM_TOKEN` in the repo. Sigstore-signed provenance attestations attached to every release (verify with `npm audit signatures`).
- Sync API, not async — the engine is in-process and most operations finish in microseconds; Promises would add overhead without a payoff.
- Rows decoded directly in Rust (via napi-rs typed wrappers) rather than via a C-FFI detour — same philosophy as the Python SDK.

## Running the tests

```bash
npm run build
npm test                # uses node --test (Node 18+'s built-in runner)
```

## Status

Phase 5d MVP: ✅ — CRUD, transactions, read-only mode, error handling, statement preparation, `columns()` introspection, typed columns back as JS primitives / objects. Parameter binding, prepared-plan caching, `changes`/`lastInsertRowid` tracking, and a real lazy iterator are natural follow-ups once the engine's cursor abstraction lands (5a.2).
