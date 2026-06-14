# SQLRite docs — getting started with the embedded Rust database

> SQLRite is an embedded SQL + vector database in Rust. This page is a
> ten-minute tour from `cargo install` to a persistent on-disk `.sqlrite`
> file — transactions, JOINs, HNSW vector search, BM25 full-text, and the
> MCP server. Every SDK wraps the same engine.

<!--
  Markdown rendition of the rendered /docs page (src/app/docs/page.tsx).
  It is served at /docs.md and embedded in /llms-full.txt for LLM crawlers.
  When you change a section of the docs page, update the matching section
  here — the section slugs (## headings) mirror the page's h2 ids.
-->

## Install

SQLRite ships as a CLI binary, a Rust library, an MCP stdio server, and five
language SDKs. Pick whichever matches your project:

```sh
# CLI / REPL — drop into a SQL prompt
cargo install sqlrite-engine

# MCP stdio server
cargo install sqlrite-mcp

# Rust library — imported as `use sqlrite::…`
cargo add sqlrite-engine

# Python · Node · Go
pip install sqlrite
npm install @joaoh82/sqlrite
go get github.com/joaoh82/rust_sqlite/sdk/go
```

Prebuilt installers for the desktop GUI (macOS .dmg, Windows .msi, Linux
AppImage / .deb / .rpm) are attached to every release on GitHub. Installers
are unsigned until Phase 6.1 — see the README for first-launch steps.

## Your first database

Create a file-backed database and run some SQL. Everything works against an
in-memory or on-disk database — the only difference is whether you pass a
path.

```sql
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  age INTEGER
);
INSERT INTO users (name, age) VALUES ('alice', 30);
SELECT * FROM users;
```

## Using the REPL

The REPL is built on rustyline and supports history, syntax highlighting,
bracket matching, and multi-line input. Useful meta commands:

- `.help` — list every meta command
- `.open app.sqlrite` — open or create a file-backed database; auto-save flips on from this point
- `.save app.sqlrite` — explicit flush (rarely needed once `.open` is in play)
- `.tables` — list every table in the current database
- `.ask` — natural-language → SQL via the configured LLM backend (requires `SQLRITE_LLM_API_KEY`)
- `.exit` — leave the prompt

Pass `--readonly` to open the database under a shared lock — multiple
read-only sessions can coexist on the same file.

## Persistence & the WAL

SQLRite stores each database as one `.sqlrite` file plus a sidecar
`<db>.sqlrite-wal`. Pages are 4 KiB; rows live in cell-based pages with a
slot directory; oversized rows spill into an overflow chain.

Commits append a frame per dirty page to the WAL plus a final commit frame
carrying the new page-0 header. The main file stays frozen between
checkpoints — auto-checkpointing fires past 100 frames.

Crash safety: torn or partial trailing WAL frames are silently truncated at
the boundary; the decoded page-0 frame overrides any stale main-file header
on reopen.

## Transactions

SQLRite supports real `BEGIN` / `COMMIT` / `ROLLBACK` with snapshot
isolation. Single level — no savepoints yet.

```sql
BEGIN;
UPDATE users SET age = age + 1 WHERE name = 'alice';
DELETE FROM users WHERE age < 18;
ROLLBACK; -- everything since BEGIN is discarded
```

## JOINs

All four SQL-standard JOIN flavors are supported with explicit `ON`
conditions: `INNER JOIN`, `LEFT [OUTER] JOIN`, `RIGHT [OUTER] JOIN`, and
`FULL [OUTER] JOIN`. Aliases work; multi-join chains left-fold; self-joins
require an alias on at least one side.

```sql
SELECT c.name, o.total
FROM customers AS c
LEFT OUTER JOIN orders AS o
  ON c.id = o.customer_id
WHERE o.id IS NULL;  -- anti-join: customers with no orders
```

The executor uses a plain nested-loop driver. `ON`, `USING (col)`,
`NATURAL`, and `CROSS JOIN` are all supported; comma-separated FROMs
(`FROM a, b`) are not — use an explicit `JOIN` / `CROSS JOIN`. Aggregates,
`GROUP BY`, `DISTINCT`, and `HAVING` compose over join results.

## GROUP BY & aggregates

`COUNT(*)`, `COUNT(col)`, `COUNT(DISTINCT col)`, `SUM`, `AVG`, `MIN`, `MAX`
with optional `GROUP BY` on bare column names. Integer `SUM` stays integer
until a `REAL` arrives or `i64` overflows; `AVG` returns `REAL` (or `NULL`
on empty groups); `MIN` / `MAX` skip NULLs.

```sql
SELECT dept, COUNT(*), AVG(salary)
FROM employees
WHERE active = TRUE
GROUP BY dept
HAVING COUNT(*) > 1
ORDER BY COUNT(*) DESC;
```

`DISTINCT` applies after projection (and after aggregation, when both
apply). `LIKE` / `NOT LIKE` / `ILIKE` use SQLite-style ASCII case folding.
`IN (literal-list)` uses three-valued logic. `HAVING` filters groups after
aggregation and requires `GROUP BY`.

## ALTER TABLE / DROP / VACUUM

Schema evolution is one operation per statement (SQLite parity):

```sql
ALTER TABLE users RENAME TO accounts;
ALTER TABLE accounts RENAME COLUMN name TO display_name;
ALTER TABLE accounts ADD COLUMN verified BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE accounts DROP COLUMN legacy_field;

DROP TABLE IF EXISTS stale_logs;
DROP INDEX IF EXISTS idx_old_search;
```

Released pages go onto a persisted free-list — subsequent `CREATE TABLE` /
`INSERT` reuses them instead of growing the file. Auto-VACUUM kicks in when
the free-list crosses 25% of `page_count`. Manual: `VACUUM;`.

## Prepared statements

Every executable statement accepts `?` placeholders anywhere a value
literal is allowed. The Rust API:

```rust
use sqlrite::{Connection, Value};

let mut conn = Connection::open("app.sqlrite")?;
let mut ins = conn.prepare_cached(
    "INSERT INTO users (name, age) VALUES (?, ?)",
)?;
ins.execute_with_params(&[Value::Text("alice".into()), Value::Integer(30)])?;
ins.execute_with_params(&[Value::Text("bob".into()), Value::Integer(25)])?;

let stmt = conn.prepare_cached("SELECT name FROM users WHERE age > ?")?;
let rows = stmt
    .query_with_params(&[Value::Integer(26)])?
    .collect_all()?;
```

`prepare_cached` keeps a per-connection LRU plan cache (default cap 16;
tune via `set_prepared_cache_capacity`). `Value::Vector(Vec<f32>)` binds
where a bracket-array literal would normally appear — so prepared k-NN
queries still take the HNSW shortcut. Named placeholders (`:foo`, `$1`)
aren't supported yet.

## PRAGMA

`PRAGMA <name>;` reads, `PRAGMA <name> = <value>;` writes. The first wired
pragma is `auto_vacuum`:

```sql
PRAGMA auto_vacuum;        -- read; renders a single-row result
PRAGMA auto_vacuum = 0.5;  -- arm the trigger at 50%
PRAGMA auto_vacuum = 0;    -- arm at 0% (compact on any released page)
PRAGMA auto_vacuum = OFF;  -- disable; equivalent: NONE, 'OFF', 'NONE'
```

Out-of-range values, `NaN`, ±∞, and unknown identifiers are rejected with
typed errors. The setting is per-`Connection` runtime state and isn't
persisted in the file header.

## Vector search

SQLRite supports a `VECTOR(N)` column type with cosine, dot-product, and L2
distance. Build an HNSW index for sub-linear k-NN queries.

```sql
CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(384));
CREATE INDEX docs_emb ON docs(embedding) USING HNSW;

SELECT id, vec_distance_cosine(embedding, ?) AS dist
FROM docs
ORDER BY dist ASC LIMIT 10;
```

## Full-text search

Phase 8 ships an FTS5-style inverted index with BM25 scoring. `fts_match()`
filters and `bm25_score()` ranks; the optimizer recognizes the canonical
pattern and probes the FTS index directly.

```sql
CREATE INDEX docs_body ON docs(body) USING FTS;

SELECT id, body, bm25_score(body, 'rust database') AS score
FROM docs
WHERE fts_match(body, 'rust database')
ORDER BY score DESC LIMIT 10;
```

Compose with vector distance for hybrid retrieval — see
`examples/hybrid-retrieval` in the repository.

## Desktop app

The desktop client is a Svelte 5 + Tauri 2.0 GUI. Three-pane layout: header
(file pickers), sidebar (tables + schema), and a query editor with line
numbers, `⌘/` comment toggle, and selection-aware Run. Download a prebuilt
installer from the latest GitHub release, or run from source:

```sh
cd desktop
npm install
npm run tauri dev
```

## MCP server

`sqlrite-mcp` exposes a SQLRite database as a Model Context Protocol stdio
server. Eight tools out of the box: `list_tables`, `describe_table`,
`query`, `execute`, `schema_dump`, `vector_search`, `bm25_search`, and
`ask`. Wire it into Claude Code, Cursor, or any MCP client.

```sh
sqlrite-mcp /path/to/app.sqlrite
sqlrite-mcp --read-only /path/to/app.sqlrite
```

## Rust crate

```rust
use sqlrite::Connection;

fn main() -> sqlrite::Result<()> {
    let mut conn = Connection::open("app.sqlrite")?;
    conn.execute("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT)")?;
    conn.execute("INSERT INTO users (name) VALUES ('alice')")?;
    for row in conn.query("SELECT id, name FROM users")? {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        println!("{id}: {name}");
    }
    Ok(())
}
```

## Python

```python
import sqlrite

with sqlrite.connect("app.sqlrite") as conn:
    cur = conn.cursor()
    cur.execute("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT)")
    cur.execute("INSERT INTO users (name) VALUES ('alice')")
    for row in cur.execute("SELECT id, name FROM users").fetchall():
        print(row)
```

## Node.js

```js
import { Database } from "@joaoh82/sqlrite";

const db = new Database("app.sqlrite");
db.exec(`CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT)`);
db.prepare("INSERT INTO users (name) VALUES (?)").run("alice");
console.log(db.prepare("SELECT id, name FROM users").all());
```

## Go

```go
import (
    "database/sql"
    _ "github.com/joaoh82/rust_sqlite/sdk/go"
)

db, _ := sql.Open("sqlrite", "app.sqlrite")
db.Exec("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT)")
db.Exec("INSERT INTO users (name) VALUES (?)", "alice")
rows, _ := db.Query("SELECT id, name FROM users")
```

## C FFI

The C ABI is stable and ships with a cbindgen-generated `sqlrite.h`. Opaque
pointer types, thread-local last-error, split `sqlrite_execute` (DDL/DML)
vs `sqlrite_query` / `sqlrite_step` (SELECT iteration).

## WASM

The engine compiles to a ~1.8 MB / 500 KB-gzipped WebAssembly module. Three
`wasm-pack` targets (web, bundler, nodejs). The whole database can live in
a browser tab.

```js
import init, { Database } from "@joaoh82/sqlrite-wasm";

await init();
const db = new Database();
db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)");
```

## Supported SQL

The complete reference lives in `docs/supported-sql.md` in the repo. Quick
summary:

- **DDL:** `CREATE TABLE` with `PRIMARY KEY` / `UNIQUE` / `NOT NULL` / `DEFAULT <literal>`; `CREATE [UNIQUE] INDEX` with `IF NOT EXISTS`, `USING HNSW`, and `USING FTS`; `ALTER TABLE` (RENAME TO / RENAME COLUMN / ADD COLUMN / DROP COLUMN); `DROP TABLE` and `DROP INDEX` with `IF EXISTS`; `VACUUM`
- **DML:** `INSERT` (multi-row VALUES), `SELECT` (projection / `DISTINCT` / `WHERE` / `GROUP BY` / `ORDER BY` / `LIMIT`), `UPDATE`, `DELETE`
- **JOINs:** `INNER`, `LEFT OUTER`, `RIGHT OUTER`, `FULL OUTER` with explicit `ON`
- **Aggregates:** `COUNT(*)`, `COUNT(DISTINCT col)`, `SUM`, `AVG`, `MIN`, `MAX`
- **Predicates:** comparisons, `AND / OR / NOT`, arithmetic, `||`, `IS NULL` / `IS NOT NULL`, `LIKE / NOT LIKE / ILIKE`, `IN (literal-list)` / `NOT IN`
- **Transactions:** `BEGIN` / `COMMIT` / `ROLLBACK` with snapshot isolation; auto-rollback on COMMIT disk failure
- **Prepared statements:** positional `?` binding via `prepare_cached` + `execute_with_params` / `query_with_params`; per-connection LRU plan cache
- **Pragmas:** `PRAGMA auto_vacuum` (read/write); extensible dispatcher
- **Types:** INTEGER, TEXT, REAL, BOOLEAN, NULL, `VECTOR(N)`, `JSON`
- **Functions:** `vec_distance_cosine / dot / l2`, `fts_match`, `bm25_score`, `json_extract`, `json_type`, `json_array_length`, `json_object_keys`

## Errors & limits

Every malformed input path returns a typed `SQLRiteError` instead of
panicking. Common error categories:

- **Parse** — bad SQL syntax, with column hints from `sqlparser`
- **Schema** — duplicate columns, missing tables, unknown identifiers
- **Type** — `'foo'` being inserted into an `INTEGER` column
- **Constraint** — UNIQUE / PRIMARY KEY violations, NOT NULL with no default
- **I/O** — file already locked, WAL truncation, disk full mid-commit

Single-writer rule: multiple read-only openers coexist; any writer excludes
all readers (POSIX flock semantics — readers OR a writer, never both at
once).

## Contributing

SQLRite welcomes pull requests. For larger changes open an issue first. The
codebase is documented phase-by-phase in `docs/` — start at
`docs/_index.md`.

- Build & test: `cargo test`
- Lint: `cargo fmt && cargo clippy`
- Run the example: `cargo run --example quickstart`
