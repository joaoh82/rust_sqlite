# Ask — natural-language → SQL

`ask` is SQLRite's natural-language query feature: type a question in English, get back generated SQL ready to run against your database. It ships across **every product surface** — the REPL, the desktop app, all four SDKs (Python / Node.js / Go / WASM), and the embedded Rust library — with a single underlying engine and one consistent set of defaults.

This doc is the canonical reference. For the per-language API details, the SDK READMEs go deeper; for the design decisions, see [`docs/phase-7-plan.md`](phase-7-plan.md) §7g.

---

## Table of contents

- [What `ask` does](#what-ask-does)
- [Architecture](#architecture)
- [Configuration — `SQLRITE_LLM_*` env vars](#configuration--sqlrite_llm_-env-vars)
- [Defaults](#defaults)
- [How to use it from each surface](#how-to-use-it-from-each-surface)
  - [REPL](#repl)
  - [Desktop app](#desktop-app)
  - [Rust library](#rust-library)
  - [Python SDK](#python-sdk)
  - [Node.js SDK](#nodejs-sdk)
  - [Go SDK](#go-sdk)
  - [WASM SDK](#wasm-sdk-the-different-one)
- [The shared `AskResponse` shape](#the-shared-askresponse-shape)
- [Errors and how they surface](#errors-and-how-they-surface)
- [Prompt caching](#prompt-caching)
- [Security notes](#security-notes)
- [Provider support](#provider-support)
- [Cost considerations](#cost-considerations)
- [Limitations](#limitations)

---

## What `ask` does

Given a connection to a SQLRite database and a natural-language question, `ask`:

1. Walks the database's schema (your `CREATE TABLE` statements, alphabetically sorted for prompt-cache stability).
2. Builds a structured prompt — frozen system rules block, then the schema dump wrapped in a cacheable Anthropic prompt-cache breakpoint, then the user's question.
3. Calls the configured LLM provider (Anthropic by default).
4. Parses the model's response into `{sql, explanation, usage}` — tolerant to fenced JSON / leading prose because real LLM output drifts even with strict instructions.

`ask` does NOT execute the SQL by default. The convention across every SDK: `ask()` returns the generated SQL for the caller to review (or hand to a confirm-and-run UX); `ask_run()` (or its language-idiomatic equivalent) is the one-shot generate-and-execute convenience.

---

## Architecture

The Rust crate `sqlrite-ask` (published on crates.io) holds the core machinery. Two halves:

| Half | Where it lives | Wasm-safe? |
|---|---|---|
| Core (prompt construction, response parsing, types) | `sqlrite-ask` lib root | ✅ yes |
| HTTP transport (`AnthropicProvider`, ureq + rustls) | `sqlrite-ask::provider::anthropic`, gated behind `http` feature | ❌ no (ureq doesn't compile to wasm32) |

The engine integration (the `Connection::ask` extension trait + the `sqlrite::ask::ask` family of free functions) lives in `sqlrite-engine` itself, gated behind the engine's `ask` feature (default-on for the CLI binary; off for the WASM SDK and any minimal library embedding).

The schema-dump helper (`sqlrite::ask::schema::dump_schema_for_database`) is **always available**, no feature flag needed — it's pure-engine code that the WASM SDK uses to introspect schemas without pulling in the HTTP transport.

---

## Configuration — `SQLRITE_LLM_*` env vars

Every surface (except WASM, which has the split JS-callback shape) reads the same environment variables for zero-config use:

| Variable | Purpose | Default |
|---|---|---|
| `SQLRITE_LLM_PROVIDER` | LLM provider | `anthropic` |
| `SQLRITE_LLM_API_KEY` | Provider API key | *(required for any LLM call)* |
| `SQLRITE_LLM_MODEL` | Model ID | `claude-sonnet-4-6` |
| `SQLRITE_LLM_MAX_TOKENS` | Per-call max output tokens | `1024` |
| `SQLRITE_LLM_CACHE_TTL` | Anthropic prompt-cache TTL on the schema block | `5m` (also `1h` or `off`) |

Set them once in your shell rc and every surface picks them up:

```sh
export SQLRITE_LLM_API_KEY="sk-ant-…"
# Optional overrides:
# export SQLRITE_LLM_MODEL="claude-haiku-4-5"
# export SQLRITE_LLM_CACHE_TTL="1h"
```

For per-call / per-connection overrides, each SDK exposes an `AskConfig` struct/object you can pass explicitly — see the SDK sections below.

---

## Defaults

Same across every surface:

| Default | Value | Why |
|---|---|---|
| Model | `claude-sonnet-4-6` | Cost/quality sweet spot for NL→SQL. Haiku 4.5 is buggy on joins/aggregates/vectors; Opus 4.7 overkills the task at 5× cost. |
| `max_tokens` | `1024` | SQL output rarely exceeds 500 tokens. Leaves headroom for a long `explanation`. |
| Cache TTL | `5m` | Break-even at 2 calls per cached prefix; right for interactive REPL/notebook use. Set `1h` for editor/desktop sessions where the same DB is queried sporadically over an hour. |
| Provider | `anthropic` | Per Phase 7 plan Q4 — Anthropic-first; OpenAI / Ollama follow-ups planned. |

---

## How to use it from each surface

Every example below assumes `SQLRITE_LLM_API_KEY` is set in the environment. Each section also shows the per-call explicit-config form for non-default keys.

### REPL

```
$ sqlrite my.sqlrite
sqlrite> .ask How many users are over 30?
Generated SQL:
  SELECT COUNT(*) FROM users WHERE age > 30
Rationale: Counts users older than thirty.
Run? [Y/n] y
+-------+
| count |
+-------+
| 47    |
+-------+
```

Confirmation defaults to `y` (just hit enter). `n` skips. Ctrl-C / EOF also skip — paranoid default for LLM-generated SQL. Per Phase 7g.2 retrospective in the roadmap.

### Desktop app

Click the **Ask…** button in the editor toolbar. A composer panel slides in above the editor. Type a question, hit Cmd/Ctrl+Enter (or click "Generate SQL"), and the generated SQL drops into the editor textarea for review. Click **Run** when ready.

The Tauri Rust backend reads `SQLRITE_LLM_API_KEY` from the env Tauri inherited at launch, makes the HTTP call server-side, and returns only `{sql, explanation}` to the webview. **The API key never crosses into the browser-render process.** Same security story as the WASM split, achieved here as a natural side effect of how Tauri's command bridge works.

If `SQLRITE_LLM_API_KEY` is missing, the panel surfaces a clean "missing API key" error in the existing error slot.

### Rust library

```toml
[dependencies]
sqlrite-engine = "0.1"
sqlrite-ask    = "0.1"
```

```rust
use sqlrite::{Connection, ConnectionAskExt};
use sqlrite_ask::AskConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open("foo.sqlrite")?;

    // Path 1: env vars (zero config)
    let cfg = AskConfig::from_env()?;
    let resp = conn.ask("How many users are over 30?", &cfg)?;
    println!("{}", resp.sql);
    println!("{}", resp.explanation);

    // Path 2: explicit config
    let cfg = AskConfig {
        api_key: Some("sk-ant-…".to_string()),
        model: "claude-haiku-4-5".to_string(),
        ..AskConfig::default()
    };
    let resp = conn.ask("count by age", &cfg)?;
    Ok(())
}
```

The `ConnectionAskExt::ask` method is gated behind the engine's `ask` feature (default-on). Equivalent free functions live at `sqlrite::ask::{ask, ask_with_database, ask_with_provider, ask_with_database_and_provider}` — pick whichever shape reads better at the call site.

### Python SDK

```bash
pip install sqlrite
```

```python
import sqlrite

conn = sqlrite.connect("foo.sqlrite")

# Path 1: env vars
resp = conn.ask("How many users are over 30?")
print(resp.sql)
print(resp.explanation)

# Path 2: explicit config
cfg = sqlrite.AskConfig(
    api_key="sk-ant-…",
    model="claude-haiku-4-5",
    cache_ttl="1h",
)
resp = conn.ask("count by age", cfg)

# Path 3: per-connection (set once, reuse)
conn.set_ask_config(cfg)
resp = conn.ask("anything")     # uses cfg

# Convenience: generate + execute
rows = conn.ask_run("list active users").fetchall()
```

`AskConfig.__repr__` deliberately omits the API key value (shows `<set>` or `None`). See [`sdk/python/README.md`](../sdk/python/README.md) for the full reference.

### Node.js SDK

```bash
npm install @joaoh82/sqlrite
```

```js
import { Database, AskConfig } from '@joaoh82/sqlrite';

const db = new Database('foo.sqlrite');

// Path 1: env vars
const resp = db.ask('How many users are over 30?');

// Path 2: explicit config (camelCase per JS convention)
const cfg = new AskConfig({
  apiKey: 'sk-ant-…',
  model: 'claude-haiku-4-5',
  cacheTtl: '1h',
});
const resp = db.ask('count by age', cfg);

// Path 3: per-connection
db.setAskConfig(cfg);
const resp = db.ask('anything');

// Convenience: generate + execute
const rows = db.askRun('list active users');
```

Auto-generated TypeScript types in `index.d.ts`. `AskConfig.toString()` deliberately omits the API key value. See [`sdk/nodejs/README.md`](../sdk/nodejs/README.md) for the full reference.

### Go SDK

```bash
go get github.com/joaoh82/rust_sqlite/sdk/go
```

```go
import (
    "database/sql"
    sqlrite "github.com/joaoh82/rust_sqlite/sdk/go"
)

db, _ := sql.Open("sqlrite", "foo.sqlrite")

// Path 1: env vars (nil cfg)
resp, err := sqlrite.Ask(db, "How many users are over 30?", nil)

// Path 2: explicit config
cfg := &sqlrite.AskConfig{
    APIKey:    "sk-ant-…",
    Model:     "claude-haiku-4-5",
    MaxTokens: 512,
    CacheTTL:  "1h",
}
resp, _ := sqlrite.Ask(db, "count by age", cfg)

// Convenience: generate + execute
rows, _ := sqlrite.AskRun(db, "list active users", nil)
defer rows.Close()

// Context-aware variants for connection-pool acquisition
resp, _ = sqlrite.AskContext(ctx, db, "...", cfg)
rows, _  = sqlrite.AskRunContext(ctx, db, "...", cfg)
```

`(*AskConfig).String()` deliberately omits the API key value. See [`sdk/go/README.md`](../sdk/go/README.md) for the full reference.

### WASM SDK (the different one)

The WASM SDK is **the only surface that requires a backend you control.** Browsers can't call `api.anthropic.com` directly (CORS) and can't safely hold an API key (anyone with DevTools can read it). So the WASM SDK splits the work: the browser builds the prompt and parses the response, your backend does the HTTP call.

```bash
npm install @joaoh82/sqlrite-wasm
```

```js
import init, { Database } from '@joaoh82/sqlrite-wasm';
await init();

const db = new Database();
db.exec(`CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)`);

async function ask(question) {
  // Step 1: build the LLM-API payload locally. No key needed.
  const payload = db.askPrompt(question);

  // Step 2: send to YOUR backend, which adds the key + forwards.
  const apiResponse = await fetch('/api/llm/complete', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(payload),
  }).then(r => r.text());

  // Step 3: parse the model's reply.
  return db.askParse(apiResponse);
}

const result = await ask('How many users are over 30?');
console.log(result.sql);
```

The backend proxy is ~10 lines on any modern serverless platform. **See [`docs/ask-backend-examples.md`](ask-backend-examples.md) for ready-to-deploy templates** — Cloudflare Workers, Vercel Edge Functions, Deno Deploy, Firebase Functions, and Node/Express.

A runnable end-to-end demo (browser + zero-dep Node proxy) lives at [`examples/wasm/`](../examples/wasm/) — `make build && make ask-demo`. See [`sdk/wasm/README.md`](../sdk/wasm/README.md) for the full reference.

---

## The shared `AskResponse` shape

Every surface returns the same logical structure (the field names are spelled idiomatically per language):

| Field | Meaning |
|---|---|
| `sql` | The generated SQL, ready to execute. Empty string if the model declined to generate SQL for the schema. |
| `explanation` | One-sentence rationale from the model. May be empty. |
| `usage.input_tokens` | Tokens billed as input on this call. |
| `usage.output_tokens` | Tokens billed as output on this call. |
| `usage.cache_creation_input_tokens` | Tokens written to the prompt cache (charged at ~1.25× normal). Non-zero on the first call against a fresh schema. |
| `usage.cache_read_input_tokens` | Tokens served from the prompt cache (charged at ~0.1× normal). **Use this to verify caching is working** — see [Prompt caching](#prompt-caching). |

---

## Errors and how they surface

| Failure mode | Where it surfaces | Typical message |
|---|---|---|
| Missing API key | `ask()` call returns / throws | `"missing API key (set SQLRITE_LLM_API_KEY ...)"` |
| HTTP transport error (network) | `ask()` call returns / throws | `"HTTP transport error: <details>"` |
| LLM API 4xx/5xx | `ask()` call returns / throws | `"API returned status <code>: <Anthropic error type+message>"` |
| Model declined (empty SQL) | `ask()` returns response with `sql=""`; `ask_run()` raises with the model's explanation | `"model declined to generate SQL: <explanation>"` |
| Model output unparseable | `ask()` returns / throws | `"model output not valid JSON: <raw>"` |

The parser is tolerant — strict JSON, fenced JSON (`` ```json … ``` ``), and JSON-with-leading-prose all parse — because real LLM output drifts even with strict prompt instructions.

---

## Prompt caching

Anthropic's prompt cache lets the schema dump be served at ~10% of normal input cost on repeat calls. The schema block in `ask`'s prompt carries a `cache_control: ephemeral` marker that's stable across calls (alphabetical column order, byte-identical rules block) so the cache reliably hits.

**Verifying it works:**

```python
# Python — same shape in every SDK
resp1 = conn.ask("first question")
resp2 = conn.ask("second question")
assert resp2.usage.cache_read_input_tokens > 0     # cache hit on the schema
```

If `cache_read_input_tokens` stays zero across repeated asks against the same schema, something is invalidating the cache. Common culprits:
- Timestamps / UUIDs / random IDs leaking into the schema dump (the schema dump is byte-stable, but if you use `ALTER TABLE` between asks, the schema changes).
- Switching models between calls (cache is model-scoped).
- Switching the cache TTL (`5m` vs `1h` are separate cache entries).

For long-running editor / desktop sessions where the same DB is queried sporadically over an hour, set `cache_ttl` to `"1h"` — costs 2× write premium instead of 1.25× but stays alive between asks.

---

## Security notes

### Where the API key lives per surface

| Surface | API key location |
|---|---|
| REPL | Process env (`SQLRITE_LLM_API_KEY`) — visible to other processes owned by the same user, same as any tool you run from a shell. |
| Desktop | Tauri Rust backend's process env. Webview (the JS rendering process) never sees the key. |
| Rust library | Wherever you put it. Read from env, secrets manager, vault, etc. |
| Python / Node / Go SDKs | Wherever you put it. Same flexibility — env, secrets manager, runtime config. |
| WASM | **YOUR backend, never the browser tab.** The browser hands the prompt to your backend, the backend adds the key and forwards. See [`docs/ask-backend-examples.md`](ask-backend-examples.md). |

### What `__repr__` / `String()` / `toString()` shows

Every SDK's `AskConfig` representation **deliberately omits the API key value**. Printing the config in logs / debuggers / Jupyter cells / `console.log` won't leak the secret. Each shows a `<set>` / `<unset>` marker so you can tell whether a key is configured:

```
Python:  AskConfig(provider="anthropic", model="claude-sonnet-4-6", max_tokens=1024, cache_ttl="5m", api_key=<set>)
Node.js: AskConfig(provider="anthropic", model="claude-sonnet-4-6", maxTokens=1024, cacheTtl="5m", apiKey=<set>)
Go:      AskConfig(provider="anthropic", model="claude-sonnet-4-6", maxTokens=1024, cacheTtl="5m", apiKey=<set>)
```

### What `AskResponse` does NOT carry

The `AskResponse` returned to your code carries `{sql, explanation, usage}` — never the API key, never the request body, never the raw API response. Logging an `AskResponse` is safe.

---

## Provider support

Phase 7g.1–7g.7 ships with **Anthropic** as the only built-in provider. Per Phase 7 plan Q4, OpenAI and Ollama follow-ups are planned but not yet implemented. The internal `Provider` trait is open — Rust callers can supply a custom backend via `ask_with_schema_and_provider` (see `sqlrite-ask`'s docs).

**For non-Anthropic providers today, the WASM SDK already offers a clean path**: your backend translates the Anthropic-shaped payload to whatever provider it talks to (the `system` blocks and `messages` array map cleanly to OpenAI's `messages` field, for example). No SDK changes needed; provider variety lives entirely on your backend. Per-SDK native support for OpenAI/Ollama is tracked as a Phase 7g.x follow-up.

---

## Cost considerations

`ask`'s typical cost per call (Anthropic Sonnet 4.6, no cache hit on schema):

- **Input tokens**: ~3,000 — ~500 for the rules block, varies with your schema (a small DB ~500; a 20-table app DB might run 2,000+).
- **Output tokens**: ~50–150 — generated SQL + one-sentence explanation.

At Sonnet 4.6 pricing ($3/MTok input, $15/MTok output): roughly **$0.01 per first call**, then **~$0.001 per cached follow-up call** within the 5-minute TTL.

For high-volume scenarios:
- Set `model: "claude-haiku-4-5"` to drop cost ~3× at the price of slightly worse SQL on complex schemas.
- Set `cache_ttl: "1h"` if your DB is queried sporadically over an hour — costs 2× cache-write but keeps every subsequent call cheap.
- Inspect `resp.usage.cache_read_input_tokens` after each call to confirm caching is hitting.

---

## Limitations

- **No streaming.** `ask` waits for the full response. Streaming would complicate the confirm-and-run flow + the SDK return-type story for marginal UX gain on a small payload.
- **No multi-turn.** Stateless — every call is a fresh prompt. Conversational refinement ("now sort by age") is its own UX problem.
- **No parameter binding in generated SQL.** The model emits literal-inlined SQL, matching the engine's current parameter-binding story (deferred to Phase 5a.2 across the whole stack).
- **Anthropic only at the SDK layer today.** OpenAI / Ollama require translation on your own backend (clean path on the WASM SDK; Rust crate has the `Provider` trait open).

---

## See also

- [`docs/phase-7-plan.md`](phase-7-plan.md) §7g — design decisions + sub-phase breakdown.
- [`docs/ask-backend-examples.md`](ask-backend-examples.md) — ready-to-deploy backend proxies for the WASM SDK (Cloudflare Workers / Vercel Edge / Deno Deploy / Firebase / Express).
- [`sdk/python/README.md`](../sdk/python/README.md), [`sdk/nodejs/README.md`](../sdk/nodejs/README.md), [`sdk/go/README.md`](../sdk/go/README.md), [`sdk/wasm/README.md`](../sdk/wasm/README.md) — per-SDK API references.
- [`examples/wasm/`](../examples/wasm/) — runnable browser demo with the Ask flow.
- [`docs/embedding.md`](embedding.md) — Rust library embedding guide that includes `ConnectionAskExt::ask`.
