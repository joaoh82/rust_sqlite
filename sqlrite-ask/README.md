# `sqlrite-ask`

Natural-language → SQL adapter for [SQLRite](https://github.com/joaoh82/rust_sqlite). Pure-Rust LLM transport (Anthropic-first; OpenAI / Ollama bindings on the roadmap), no async runtime, no third-party LLM SDK — built directly on `ureq` + `serde_json`. Phase 7g.1 of the project.

## What it does

Given a `&str` schema dump (a sequence of `CREATE TABLE` statements) plus a `&str` question, returns the generated SQL plus a one-sentence explanation:

```rust
use sqlrite_ask::{ask_with_schema, AskConfig};

let schema = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);";
let cfg = AskConfig::from_env()?;        // reads SQLRITE_LLM_API_KEY etc.
let resp = ask_with_schema(schema, "How many users are over 30?", &cfg)?;

println!("SQL:         {}", resp.sql);
println!("Explanation: {}", resp.explanation);
println!("Cache hit:   {} input tokens", resp.usage.cache_read_input_tokens);
```

The crate is **pure** — it doesn't know about `Connection`, `Database`, or any engine type. The schema-dump-aware integration with the engine lives in the `sqlrite-engine` crate's `ask` feature instead. See the **Most users want** section below for which entry point to reach for.

## Most users want…

This crate is the LLM transport layer. **Most callers don't need to use it directly** — the engine's `ask` feature wraps it in an ergonomic `Connection::ask` extension trait that introspects the schema for you:

```toml
[dependencies]
sqlrite-engine = "0.2"
# `sqlrite-engine`'s `ask` feature is on by default, which pulls
# `sqlrite-ask` transitively. You don't usually need to depend on
# this crate directly.
```

```rust
use sqlrite::{Connection, ConnectionAskExt};
use sqlrite::ask::AskConfig;

let conn = Connection::open("foo.sqlrite")?;
let cfg  = AskConfig::from_env()?;
let resp = conn.ask("How many users are over 30?", &cfg)?;
```

Reach for `sqlrite-ask` directly only if:

- You want to build the schema dump yourself (custom introspection, schema generation, schema fragments) and call `ask_with_schema` over a `&str`.
- You're targeting a runtime where `sqlrite-engine` won't compile (e.g. wasm32, where the WASM SDK uses `sqlrite-ask` with `default-features = false` to skip the `ureq` HTTP transport).
- You want to plug a custom `Provider` impl in via `ask_with_schema_and_provider` for testing / proxying / non-Anthropic backends.

## Configuration

Three layers of precedence: per-call argument > `AskConfig::from_env()` > defaults. Env vars:

| Variable | Default | Purpose |
|---|---|---|
| `SQLRITE_LLM_PROVIDER` | `anthropic` | Provider |
| `SQLRITE_LLM_API_KEY` | *(none)* | Provider API key (required for any LLM call) |
| `SQLRITE_LLM_MODEL` | `claude-sonnet-4-6` | Model ID |
| `SQLRITE_LLM_MAX_TOKENS` | `1024` | Per-call max output tokens |
| `SQLRITE_LLM_CACHE_TTL` | `5m` | Anthropic prompt-cache TTL on the schema dump (`5m`, `1h`, `off`) |

`AskConfig`'s `Debug` impl deliberately omits the API key value — printing the config in logs / debuggers won't leak the secret.

Full reference for the env vars + the per-surface usage notes lives in the project's [`docs/ask.md`](https://github.com/joaoh82/rust_sqlite/blob/main/docs/ask.md).

## Features

- `default = ["http"]` — pulls `ureq` + `rustls` for the HTTP transport.
- `http` — the Anthropic provider that POSTs to `api.anthropic.com/v1/messages`.

For wasm32 builds, depend on this crate with `default-features = false` to keep just the prompt-construction + response-parsing helpers (`AskConfig`, `AskResponse`, `parse_response`, `ask_with_schema_and_provider<P: Provider>`) without dragging in the HTTP transport. The WASM SDK does this — see the [WASM SDK README](https://github.com/joaoh82/rust_sqlite/blob/main/sdk/wasm/README.md#natural-language--sql-phase-7g7) for the JS-callback shape it uses instead.

## Architecture notes

- **Hand-rolled JSON request/response shapes** in `serde_json` — there is no official Anthropic Rust SDK and rolling our own matches the project's "build it ourselves" theme. ~120 LOC of types vs ~400 LOC + a tokio runtime via a third-party SDK.
- **Sync `ureq` over async `reqwest`** — per `ask()` call we make exactly one POST. The sync surface is the right fit and avoids pulling tokio into every embedder. (rejected `reqwest::blocking` because it pulls tokio in even on the blocking path.)
- **Schema dump is byte-stable** — alphabetically sorted, deterministic column ordering — so the prompt's `cache_control: ephemeral` block reliably hits Anthropic's prompt cache on repeat asks.
- **Tolerant response parsing** — accepts strict JSON, fenced JSON (` ```json … ``` `), and JSON-with-leading-prose. Real LLM output drifts even with strict instructions; the parser is forgiving so the caller doesn't have to be.
- **No engine dep** — `sqlrite-ask` 0.1.18 had a `sqlrite-engine` path-dep that created a cargo cycle (`sqlrite-engine[bin] → sqlrite-ask → sqlrite-engine[lib]`). Dropped in 0.1.19; the engine integration moved into `sqlrite-engine`'s new `ask` feature. See the **v0.1.19 dep-direction flip retrospective** in [`docs/roadmap.md`](https://github.com/joaoh82/rust_sqlite/blob/main/docs/roadmap.md).

## Sibling products

- The [`sqlrite-mcp`](https://github.com/joaoh82/rust_sqlite/blob/main/docs/mcp.md) Model Context Protocol server exposes `ask` as a tool any MCP client (Claude Code, Cursor, `mcp-inspector`) can call. Phase 7g.8.
- Per-language SDKs (`sqlrite` on PyPI, `@joaoh82/sqlrite` on npm, `github.com/joaoh82/rust_sqlite/sdk/go`, `@joaoh82/sqlrite-wasm` on npm) all expose the `ask()` family in idiomatic shapes — see [`docs/ask.md`](https://github.com/joaoh82/rust_sqlite/blob/main/docs/ask.md) for the per-SDK reference.

## License

MIT. Same as the rest of the project.
