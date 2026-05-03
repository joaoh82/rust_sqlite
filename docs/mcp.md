# MCP server — `sqlrite-mcp`

`sqlrite-mcp` wraps a SQLRite database as a [Model Context Protocol](https://modelcontextprotocol.io/) server over stdio. LLM agents — Claude Code, Cursor, Codex, Anthropic's `mcp-inspector`, your own client — spawn it as a subprocess and get a fixed set of tools for driving the database. No custom integration code on the LLM side.

This is **Phase 7h** in the project's roadmap. The natural-language → SQL `ask` tool is **Phase 7g.8**, shipped in the same crate behind a default-on cargo feature.

This page is the canonical reference. For the design rationale (hand-rolled JSON-RPC vs. crate, why stdio-only, the original seven-tool decision tree), see [`docs/phase-7-plan.md`](phase-7-plan.md) §7h. The eighth tool (`bm25_search`) was added in Phase 8e — see [`docs/phase-8-plan.md`](phase-8-plan.md) Q9.

---

## Table of contents

- [What it does](#what-it-does)
- [Install](#install)
- [Wiring it into MCP clients](#wiring-it-into-mcp-clients)
  - [Claude Code](#claude-code)
  - [Cursor](#cursor)
  - [`mcp-inspector` (debugging)](#mcp-inspector-debugging)
- [The eight tools](#the-eight-tools)
- [Read-only mode](#read-only-mode)
- [Environment variables](#environment-variables)
- [Wire format](#wire-format)
- [Security notes](#security-notes)
- [Limitations](#limitations)
- [Troubleshooting](#troubleshooting)

---

## What it does

Given an MCP client (the LLM or its harness), `sqlrite-mcp`:

1. Reads a SQLRite database file (or runs in-memory) from the path it's spawned with.
2. Speaks JSON-RPC 2.0 over stdio: line-delimited JSON in on stdin, line-delimited JSON out on stdout.
3. Exposes eight tools: `list_tables`, `describe_table`, `query`, `execute`, `schema_dump`, `vector_search`, `bm25_search`, `ask` (the last one gated behind the `ask` cargo feature, default-on).
4. Stays purely synchronous — one tool call at a time, in arrival order. Mirrors the engine's own thread-safety model; clients pipelining requests get serialized completion.

The whole binary is ~1100 LOC of hand-rolled protocol + dispatch + tool handlers. No tokio, no async runtime, no MCP framework — same dep-frugal approach as the rest of the project.

---

## Install

### From crates.io (recommended)

```sh
cargo install sqlrite-mcp
```

By default this builds with the `ask` feature on, which pulls `sqlrite-ask` and its `ureq` + `rustls` HTTP transport. For a leaner build with no LLM machinery (the six pure-SQL tools only), pass `--no-default-features`:

```sh
cargo install sqlrite-mcp --no-default-features
```

### From a GitHub release

Each release ships a per-platform tarball with a pre-built `sqlrite-mcp` binary — no Rust toolchain needed. Download from the [Releases page](https://github.com/joaoh82/rust_sqlite/releases) (`sqlrite-mcp-vX.Y.Z-{linux-x86_64,linux-aarch64,macos-aarch64,windows-x86_64}.tar.gz`), extract, and put `sqlrite-mcp` somewhere on your `PATH`.

### From source

```sh
git clone https://github.com/joaoh82/rust_sqlite
cd rust_sqlite
cargo build --release -p sqlrite-mcp
# Binary at: ./target/release/sqlrite-mcp
```

---

## Wiring it into MCP clients

### Claude Code

Add to your `~/.claude.json`:

```json
{
  "mcpServers": {
    "sqlrite": {
      "command": "sqlrite-mcp",
      "args": ["/absolute/path/to/your.sqlrite"],
      "env": {
        "SQLRITE_LLM_API_KEY": "sk-ant-…"
      }
    }
  }
}
```

The `env` block is only needed if you want the `ask` tool to work. Otherwise the six pure-SQL tools work without any environment setup.

For a read-only setup (recommended for analytics-style usage where you never want the LLM to mutate the database):

```json
{
  "mcpServers": {
    "sqlrite": {
      "command": "sqlrite-mcp",
      "args": ["/absolute/path/to/your.sqlrite", "--read-only"]
    }
  }
}
```

After editing, restart Claude Code so it spawns the new server config.

### Cursor

Cursor's MCP UI takes the same shape — `command: sqlrite-mcp`, `args: ["/path/to/db.sqlrite"]`. Cursor surfaces the tools/list result in its tool-picker the next time you open a chat.

### `mcp-inspector` (debugging)

The official [MCP Inspector](https://github.com/modelcontextprotocol/inspector) is the easiest way to verify a server works without setting up a full LLM client:

```sh
npx @modelcontextprotocol/inspector sqlrite-mcp /path/to/your.sqlrite
```

Open the URL it prints. You'll see the eight tools, can call them with hand-typed JSON arguments, and watch the JSON-RPC traffic in both directions. **First thing to check after deploying — confirms the wire-up before pointing a real LLM at it.**

---

## The eight tools

| Tool | Purpose | Required input |
|---|---|---|
| [`list_tables`](#list_tables) | Discover what's in the database | — |
| [`describe_table`](#describe_table) | Column metadata + row count | `name` |
| [`query`](#query) | Run a SELECT, return rows | `sql` |
| [`execute`](#execute) | Run DDL / DML / transactions | `sql` |
| [`schema_dump`](#schema_dump) | Full `CREATE TABLE` script | — |
| [`vector_search`](#vector_search) | k-NN over a VECTOR column | `table`, `column`, `embedding` |
| [`bm25_search`](#bm25_search) *(8e)* | Top-k by BM25 over an FTS column | `table`, `column`, `query` |
| [`ask`](#ask) *(7g.8)* | Natural-language → SQL | `question` |

### `list_tables`

Returns every user-defined table name as a JSON array of strings, sorted alphabetically. Excludes the engine's `sqlrite_master` catalog. Cheapest tool — usually the LLM's first call.

```jsonc
// Request
{ "name": "list_tables", "arguments": {} }

// Response (tool result text):
"[\"orders\", \"products\", \"users\"]"
```

### `describe_table`

Column metadata for one table: name, declared type, primary-key / NOT NULL / UNIQUE flags, plus the current row count.

```jsonc
// Request
{ "name": "describe_table", "arguments": { "name": "users" } }

// Response (tool result text):
"{
  \"name\": \"users\",
  \"columns\": [
    { \"name\": \"id\", \"type\": \"Integer\", \"primary_key\": true,
      \"not_null\": true, \"unique\": true },
    { \"name\": \"name\", \"type\": \"Text\", \"primary_key\": false,
      \"not_null\": false, \"unique\": false }
  ],
  \"row_count\": 42
}"
```

The `name` argument must match `[A-Za-z_][A-Za-z0-9_]*` — quoted/exotic SQLite identifiers aren't accepted (an attacker-controlled `name` would otherwise concatenate into the row-count query).

### `query`

Runs a SELECT statement, returns rows as a JSON array of objects (key = column name). Other statement types (INSERT / UPDATE / DELETE / CREATE / etc.) are rejected at the tool layer with a redirect-to-`execute` message.

Default row cap: 100. Override with `limit` up to a hard ceiling of 1000. The tool also caps total response bytes at 64 KiB; truncated results carry `truncated: true`, `truncation_reason`, and `total_seen` fields so the LLM knows there's more.

```jsonc
{
  "name": "query",
  "arguments": {
    "sql": "SELECT id, name, age FROM users WHERE age > 30 ORDER BY age DESC",
    "limit": 50
  }
}
```

### `execute`

DDL, DML, and transaction control (CREATE / INSERT / UPDATE / DELETE / DROP / ALTER / BEGIN / COMMIT / ROLLBACK). Returns the engine's status string ("3 rows inserted", "table users created"). SELECT goes through `query` instead.

**Disabled in `--read-only` mode** — hidden from `tools/list`, and rejected with a clear tool-error if a client calls it anyway.

### `schema_dump`

Returns the full database schema as a sequence of `CREATE TABLE` statements (the same dump the `ask` tool feeds the LLM). Useful for priming the LLM's context with the whole schema in one call rather than walking every table with `describe_table`. Tables are emitted alphabetically so output is deterministic.

### `vector_search`

k-NN lookup against a VECTOR column. Picks up an HNSW index automatically if one exists (`CREATE INDEX … USING hnsw`); otherwise brute-force scans.

```jsonc
{
  "name": "vector_search",
  "arguments": {
    "table": "documents",
    "column": "embedding",
    "embedding": [0.12, -0.04, 0.88, /* ... */],
    "k": 5,
    "metric": "cosine"
  }
}
```

Supported metrics: `l2` (default, Euclidean), `cosine`, `dot`. `embedding` must match the column's declared dimension. Returns the matching rows in ascending distance order (the numeric distance value is not included — the engine doesn't yet support function calls in SELECT projections; if you need the value, recompute it client-side from the returned vector).

### `bm25_search`

*Phase 8e — symmetric with `vector_search` for keyword retrieval.*

Top-k lookup against an FTS-indexed TEXT column, ranked by BM25. Wraps the canonical `WHERE fts_match(col, 'q') ORDER BY bm25_score(col, 'q') DESC LIMIT k` SQL so the LLM doesn't have to remember the WHERE pre-filter, the DESC direction, or string quoting. Picks up the engine's FTS optimizer hook automatically.

```jsonc
{
  "name": "bm25_search",
  "arguments": {
    "table": "documents",
    "column": "body",
    "query": "rust embedded database",
    "k": 5
  }
}
```

Requires a `CREATE INDEX … USING fts (column)` on the column; errors clearly otherwise (the message names the missing CREATE INDEX so the LLM can recover). The query is tokenized with the same rules used to build the index (ASCII split + lowercase, no stemming or stop list — see [`docs/fts.md`](fts.md)). Returns matching rows in descending BM25-relevance order.

For hybrid retrieval (BM25 + vector) the LLM can either call both `bm25_search` and `vector_search` and fuse client-side, or compose them in a single SQL via the `query` tool — see the worked example in [`examples/hybrid-retrieval/`](../examples/hybrid-retrieval/).

### `ask`

*Phase 7g.8 — gated behind the crate's default-on `ask` cargo feature.*

Generates SQL from a natural-language question, grounded in this database's schema. Returns `{ sql, explanation, usage }`. Optionally executes the SQL inline (`execute: true`).

```jsonc
{
  "name": "ask",
  "arguments": {
    "question": "How many users signed up last week?",
    "execute": true
  }
}

// Response (tool result text):
"{
  \"sql\": \"SELECT COUNT(*) FROM users WHERE created_at > date('now', '-7 days')\",
  \"explanation\": \"Counts rows in users with created_at within the last 7 days.\",
  \"usage\": { \"input_tokens\": 412, \"output_tokens\": 28,
              \"cache_creation_input_tokens\": 0, \"cache_read_input_tokens\": 412 },
  \"executed\": true,
  \"rows\": [ { \"COUNT(*)\": 17 } ]
}"
```

**Requires `SQLRITE_LLM_API_KEY` in the spawned process's environment.** MCP clients pass env vars via their server-config block (see the Claude Code wiring example above).

Per-call overrides: `model`, `max_tokens`, `cache_ttl` (`5m` / `1h` / `off`). All optional — they layer over [`AskConfig::from_env`](ask.md#configuration--sqlrite_llm_-env-vars) (per-call > env > defaults). The full reference for the underlying `ask` machinery — config precedence, prompt caching, error taxonomy, security model — lives in [`docs/ask.md`](ask.md).

---

## Read-only mode

Pass `--read-only` to open the database with a shared lock and disable the `execute` tool:

```sh
sqlrite-mcp /path/to/db.sqlrite --read-only
```

Effects:

- Multiple `--read-only` processes can sit on the same DB file concurrently (shared lock; same semantics as `Connection::open_read_only`).
- `tools/list` omits `execute` entirely — the LLM doesn't see it, doesn't try to call it.
- A client that calls `execute` anyway gets a tool-error: *"the `execute` tool is disabled in read-only mode (--read-only)..."*. Belt + suspenders.
- The `ask` tool's `execute: true` option falls back to "report SQL but don't run it" for non-SELECT generated SQL, with `execute_error` carrying the explanation.

**Recommended for** any analytics-style use case where the LLM should be able to read and explore but never mutate.

---

## Environment variables

| Variable | Purpose |
|---|---|
| `SQLRITE_MCP_DATABASE` | Fallback database path if the CLI arg is omitted. Useful for MCP client configs that don't pass args nicely. |
| `SQLRITE_LLM_API_KEY` | Anthropic API key for the `ask` tool. Required if you want `ask` to work; ignored otherwise. |
| `SQLRITE_LLM_MODEL` | Override the default model (`claude-sonnet-4-6`). |
| `SQLRITE_LLM_MAX_TOKENS` | Override max output tokens (default `1024`). |
| `SQLRITE_LLM_CACHE_TTL` | Anthropic prompt-cache TTL: `5m`, `1h`, or `off` (default `5m`). |
| `SQLRITE_LLM_PROVIDER` | Provider (currently `anthropic` only). |

The four `SQLRITE_LLM_*` variables are the same ones every other surface (REPL, desktop, Python / Node / Go SDKs) reads — see [`docs/ask.md`](ask.md#configuration--sqlrite_llm_-env-vars) for the full reference.

---

## Wire format

JSON-RPC 2.0 over stdio. One JSON value per line on each direction, UTF-8, terminated by `\n`. No length prefixes, no Content-Length headers. We declare protocol version `2025-11-25` (see [the MCP spec](https://modelcontextprotocol.io/specification/2025-11-25/basic)).

Methods we implement:

| Method | Direction | Purpose |
|---|---|---|
| `initialize` | client → server | Lifecycle handshake. Returns `serverInfo`, `protocolVersion`, `capabilities`. |
| `notifications/initialized` | client → server | Notification — completes the handshake. |
| `notifications/cancelled` | client → server | No-op (we run tools synchronously, so by the time we'd see the cancel the tool's already done). |
| `tools/list` | client → server | Returns the tool registry for this server (omits `execute` under `--read-only`, omits `ask` if built without the `ask` feature). |
| `tools/call` | client → server | Invokes a tool. Tool execution errors come back as `result.isError: true`; protocol errors as standard JSON-RPC `error` codes. |
| `ping` | client → server | Returns `{}`. Convenient for liveness checks. |
| `shutdown` | client → server | Returns null. The actual process exits when stdin closes. |

Stderr is reserved for diagnostics (panics, the MCP startup banner). Anything written there is invisible to the protocol but visible in the MCP client's "server log" pane — handy for debugging.

---

## Security notes

- **The MCP server inherits its parent's trust model.** Whoever spawns `sqlrite-mcp` decides what database it opens, what env vars it sees, what filesystem it can reach. There's no auth/authz layer in the binary itself — that's intentional, because stdio-spawned subprocesses always run under the spawner's privileges.

- **Don't give an LLM `execute` access by default.** Recommend `--read-only` for any "let an LLM explore the data" use case. Reserve write access for explicitly-trusted, supervised flows.

- **The `ask` tool's API key never leaves the server process.** The MCP client passes `SQLRITE_LLM_API_KEY` once at spawn time via its server-config `env` block; tool calls never echo it back, and `AskConfig`'s `Debug` impl deliberately omits it (see [`docs/ask.md`](ask.md#security-notes)).

- **Logging hygiene:** the `query`, `execute`, and `ask` tools receive arbitrary user-supplied SQL and questions. The server emits these to stderr only on errors (so the MCP log pane is useful for debugging) — but if you wrap `sqlrite-mcp`'s stderr in your own logging stack, treat the contents as potentially sensitive.

- **Stdout is owned by the protocol.** As of the engine-stdout-pollution cleanup, the SQLRite engine itself doesn't print to stdout — the REPL-convenience prints (CREATE schema, INSERT row dump, SELECT result table) all moved out: SELECT's rendered table comes back inside [`CommandOutput::rendered`](../src/sql/mod.rs) for the REPL to print itself, the others were dropped entirely. The MCP binary additionally redirects process fd 1 → fd 2 at startup as **defense in depth** — protects against future regressions if a contributor (or a transitive dep) ever reintroduces a stray `print!`. See `sqlrite-mcp/src/stdio_redirect.rs` for the dance.

---

## Limitations

- **Stdio transport only.** No HTTP / SSE / WebSocket transport. Stdio covers every modern MCP client; if you need a long-lived HTTP-shaped server (Anthropic Skills hosting, etc.), wrap `sqlrite-mcp` in a tiny HTTP→stdio adapter.
- **One database per server process.** Spawn multiple processes if you need to attach multiple databases.
- **No concurrent tool calls.** Strictly serial dispatch. The engine isn't safe for concurrent mutation; documented behavior, not a bug.
- **No subscription / change-notification primitives.** `tools/listChanged` is `false` — the tool set is static for a given binary version + feature set.
- **`vector_search` doesn't return distance values.** The engine doesn't yet support function calls in SELECT projections; rows come back in the right order but without the numeric distance attached.
- **Aggregate SELECTs (COUNT/SUM/AVG/...).** The engine doesn't support these yet (Phase 8 work). The `query` tool surfaces the engine's parse error verbatim if the LLM tries one.
- **`ask` is Anthropic-only.** OpenAI / Ollama bindings live on `sqlrite-ask`'s roadmap (Phase 7g follow-ups, see [`docs/phase-7-plan.md`](phase-7-plan.md) §7g Q4). When they land, they propagate to the MCP `ask` tool with no MCP-side changes.

---

## Troubleshooting

**`MCP client says "server didn't speak MCP" or "invalid JSON".`**
Almost always stdout pollution. Run `sqlrite-mcp /path/to/db.sqlrite < /dev/null > /tmp/mcp-out.log 2>&1` and check `/tmp/mcp-out.log` — anything before the first `{"jsonrpc":"2.0",…}` is the problem. The binary already redirects engine stdout to stderr, but a custom Rust patch or a third-party crate could still slip a `println!` through.

**`Error: ANTHROPIC_API_KEY not found / SQLRITE_LLM_API_KEY missing`** (when calling `ask`).
The MCP client didn't pass the env var to the spawned subprocess. Edit your client's server-config `env` block (see [Claude Code wiring](#claude-code) above), then restart the client.

**`error: failed to open database: ...`**
Check the path is absolute, the file exists (or pass `--in-memory` for an ephemeral DB), and the process has read-write permission (or use `--read-only`).

**`error: --in-memory and a database path are mutually exclusive`**
Pick one. `--in-memory` is for "fresh database per server lifetime"; a path opens an existing file.

**`tool returned isError: true with "Not Implemented error: ..."`**
The engine doesn't support that SQL feature yet. Most common offender: aggregates (COUNT/SUM/AVG). Restructure the query (or use `vector_search` for k-NN). The engine's [`docs/supported-sql.md`](supported-sql.md) is the canonical "what works" list.

---

## See also

- [`docs/ask.md`](ask.md) — canonical reference for the `ask` feature across every surface (REPL, desktop, Rust library, Python / Node / Go / WASM, and now MCP).
- [`docs/ask-backend-examples.md`](ask-backend-examples.md) — backend proxy templates (relevant only for the WASM SDK; the MCP server holds the API key in the server process directly).
- [`docs/supported-sql.md`](supported-sql.md) — what SQL the engine actually executes today.
- [`docs/phase-7-plan.md`](phase-7-plan.md) — design rationale for Phase 7 and §7h specifically.
- [`sqlrite-mcp/README.md`](../sqlrite-mcp/README.md) — short crates.io-facing readme.
