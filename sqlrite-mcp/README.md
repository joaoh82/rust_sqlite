# `sqlrite-mcp`

Model Context Protocol (MCP) server for [SQLRite](https://github.com/joaoh82/rust_sqlite). Wraps a SQLRite database as a stdio-spawned subprocess that LLM agents (Claude Code, Cursor, Codex, the official `mcp-inspector`, anything that speaks the MCP wire protocol) can drive without custom integration code.

**Phase 7h** in the project's roadmap. The natural-language → SQL `ask` tool (Phase 7g.8) ships in the same crate behind a default-on cargo feature.

## Install

```sh
cargo install sqlrite-mcp
```

Or grab a pre-built tarball from the [GitHub Releases](https://github.com/joaoh82/rust_sqlite/releases) page (Linux x86_64 / aarch64, macOS arm64, Windows x86_64 — no Rust toolchain required).

For a lean build with no LLM machinery (six pure-SQL tools only — drops the `ask` tool, drops `ureq` + `rustls` from the dep tree):

```sh
cargo install sqlrite-mcp --no-default-features
```

## Wiring it in

### Claude Code (`~/.claude.json`)

```json
{
  "mcpServers": {
    "sqlrite": {
      "command": "sqlrite-mcp",
      "args": ["/absolute/path/to/your.sqlrite"],
      "env": { "SQLRITE_LLM_API_KEY": "sk-ant-…" }
    }
  }
}
```

The `env` block is only needed for the `ask` tool. For analytics-style use (LLM reads but never mutates), add `"--read-only"` to `args`.

### Cursor / `mcp-inspector` / your own MCP client

Same shape — `command: sqlrite-mcp`, `args: [database-path]`. The seven tools surface automatically once the client calls `tools/list`.

## The seven tools

| Tool | Purpose |
|---|---|
| `list_tables` | List user-defined tables |
| `describe_table` | Column metadata + row count for one table |
| `query` | Run a SELECT, return rows as JSON |
| `execute` | Run DDL / DML / transactions (disabled in `--read-only`) |
| `schema_dump` | Full `CREATE TABLE` script |
| `vector_search` | k-NN over a VECTOR column (uses HNSW index when present) |
| `ask` *(7g.8)* | Natural-language → SQL via Anthropic (gated behind `ask` feature) |

For the full per-tool reference (input schemas, examples, error shapes), see [`docs/mcp.md`](https://github.com/joaoh82/rust_sqlite/blob/main/docs/mcp.md).

## Read-only mode

```sh
sqlrite-mcp /path/to/db.sqlrite --read-only
```

Opens the database with a shared lock and hides the `execute` tool from `tools/list`. Multiple `--read-only` processes can sit on the same file concurrently. Recommended whenever an LLM doesn't need write access.

## In-memory mode

```sh
sqlrite-mcp --in-memory
```

Fresh ephemeral database per server lifetime. State dies with the process. Useful for one-off LLM scratchpads.

## Design

- **Hand-rolled JSON-RPC 2.0** over line-delimited JSON on stdio. ~1100 LOC for the whole binary; no tokio, no async runtime, no third-party MCP framework. Same dep-frugal theme as the rest of the project.
- **Synchronous, single-client** dispatch. The engine isn't safe for concurrent mutation; serial dispatch matches the model.
- **Stdout owned by the protocol.** The binary redirects fd 1 to fd 2 at startup so any errant `println!` from anywhere in the dep tree (notably the engine's REPL-convenience prints in `process_command`) goes to stderr instead of corrupting the JSON-RPC channel.
- **Tool errors vs. protocol errors.** SQL parse failures, dimension mismatches, "unknown table" — surfaced as `result.isError: true` so the LLM reads the message and retries. Malformed requests, unknown methods, server-not-initialized — surfaced as standard JSON-RPC `error` codes.

For the full design rationale (hand-roll vs. crate, why stdio-only, the seven-tool decision), see [`docs/phase-7-plan.md`](https://github.com/joaoh82/rust_sqlite/blob/main/docs/phase-7-plan.md) §7h.

## Documentation

- [Full MCP server reference](https://github.com/joaoh82/rust_sqlite/blob/main/docs/mcp.md)
- [Ask feature reference (every SDK surface)](https://github.com/joaoh82/rust_sqlite/blob/main/docs/ask.md)
- [SQLRite engine + ecosystem README](https://github.com/joaoh82/rust_sqlite/blob/main/README.md)

## License

MIT. Same as the rest of the project.
