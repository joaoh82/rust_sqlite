# Full-text search + hybrid retrieval

`fts` is SQLRite's keyword-search feature: build an inverted index on a `TEXT` column with `CREATE INDEX … USING fts (col)`, then ask BM25-relevance-ranked queries from any product surface. It composes naturally with [`vec_distance_cosine`](phase-7-plan.md) (Phase 7d) for **hybrid retrieval** — the modern RAG default that fuses lexical exact-match with semantic embedding-space proximity.

This doc is the canonical reference. For the design rationale (resolved Q1–Q10), see [`docs/phase-8-plan.md`](phase-8-plan.md).

---

## Table of contents

- [What FTS does](#what-fts-does)
- [Quick start](#quick-start)
- [SQL surface](#sql-surface)
  - [`CREATE INDEX … USING fts (col)`](#create-index--using-fts-col)
  - [`fts_match(col, 'q')`](#fts_matchcol-q)
  - [`bm25_score(col, 'q')`](#bm25_scorecol-q)
- [Hybrid retrieval](#hybrid-retrieval)
- [Tokenizer rules](#tokenizer-rules)
- [BM25 parameters](#bm25-parameters)
- [Lifecycle: INSERT / DELETE / UPDATE](#lifecycle-insert--delete--update)
- [Persistence + file format](#persistence--file-format)
- [Optimizer hook](#optimizer-hook)
- [How to use it from each surface](#how-to-use-it-from-each-surface)
- [Limitations](#limitations)
- [See also](#see-also)

---

## What FTS does

Given a `TEXT` column with an FTS index, you can:

1. **Match** rows by query terms: `WHERE fts_match(body, 'rust embedded')` — true if the row contains *any* of the query's tokens (post-tokenization).
2. **Rank** by relevance: `ORDER BY bm25_score(body, 'rust embedded') DESC LIMIT k` — top-k by BM25, served from the inverted index in O(query-term-count × k log k).
3. **Fuse** with vector similarity: `ORDER BY 0.5 * bm25_score(...) + 0.5 * (1.0 - vec_distance_cosine(...)) DESC LIMIT k` — hybrid retrieval via raw arithmetic.

FTS does **not** auto-index every TEXT column. It only kicks in for columns with an explicit `CREATE INDEX … USING fts (col)`. Mirrors how HNSW vector indexes opt in.

---

## Quick start

```sql
-- 1. Schema + data
CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);
INSERT INTO docs (body) VALUES ('rust is a systems programming language');
INSERT INTO docs (body) VALUES ('sqlite is an embedded database engine');
INSERT INTO docs (body) VALUES ('postgres is a relational database server');

-- 2. Build the FTS index (any time after the column exists; the index
--    backfills from current rows and stays in lockstep on INSERT).
CREATE INDEX docs_fts ON docs USING fts (body);

-- 3a. Lexical filter — true / false predicate.
SELECT id FROM docs WHERE fts_match(body, 'database');

-- 3b. Relevance ranking — top-k.
SELECT id FROM docs
 WHERE fts_match(body, 'embedded database')
 ORDER BY bm25_score(body, 'embedded database') DESC
 LIMIT 5;

-- 3c. Hybrid: BM25 + vector cosine, equal weights.
--    (Assumes an `embedding VECTOR(N)` column on the same table.)
SELECT id FROM docs
 WHERE fts_match(body, 'embedded database')
 ORDER BY 0.5 * bm25_score(body, 'embedded database')
        + 0.5 * (1.0 - vec_distance_cosine(embedding, [0.12, -0.04, /* ... */]))
DESC LIMIT 5;
```

For a runnable hybrid example with hand-baked embeddings, see [`examples/hybrid-retrieval/`](../examples/hybrid-retrieval/).

---

## SQL surface

### `CREATE INDEX … USING fts (col)`

```sql
CREATE INDEX <name> ON <table> USING fts (<column>);
```

Constraints:

- **Single-column only.** Multi-column FTS (`fts(title, body)`) is deferred to Phase 8.1 ([Q2](phase-8-plan.md#q2-multi-column-fts-in-one-index-or-one-index-per-column)).
- **TEXT columns only.** `INTEGER`, `REAL`, `BOOLEAN`, `VECTOR`, `JSON` columns are rejected with a clear error message at CREATE-INDEX time.
- **`UNIQUE` is meaningless and rejected.** UNIQUE on an inverted index doesn't have a sensible interpretation.
- **Name uniqueness spans all index kinds.** A B-Tree, HNSW, or other FTS index on the same table can't share a name; `IF NOT EXISTS` silently no-ops in that case.
- **Backfill is automatic.** Existing rows are tokenized + indexed at CREATE time. New rows added via INSERT update the index incrementally.

### `fts_match(col, 'q')`

Boolean predicate; returns `TRUE` if the row has at least one query term in its tokenized representation, `FALSE` otherwise. Most-common shape:

```sql
WHERE fts_match(body, 'rust embedded')
```

Multi-token queries use **any-term** (OR) semantics — a row matches if *any* of `rust` / `embedded` is in its tokenized body. Boolean operators inside the query string (`AND` / `OR` / `NOT`) and phrase queries (`MATCH '"exact phrase"'`) are deferred to Phase 8.1.

The function requires a TEXT column with an FTS index attached. Without one, it errors at evaluation time:

```text
fts_match(body, ...): no FTS index on column 'body' (run CREATE INDEX <name> ON <table> USING fts(body) first)
```

This contrasts with SQLite's `MATCH` operator, which is parser-level; SQLRite uses a function-call shape because `sqlparser`'s SQLite dialect doesn't expose a `BinaryOperator::Match` variant we can dispatch on ([Q1](phase-8-plan.md#q1-match-operator-syntax)).

### `bm25_score(col, 'q')`

Per-row BM25 relevance score; returns `Value::Real` (`f64`). Higher = more relevant. Use in `ORDER BY` for top-k retrieval:

```sql
ORDER BY bm25_score(body, 'rust embedded') DESC LIMIT 10
```

Notes:

- **`DESC`** is the conventional direction. `ASC` works (returns least-relevant first) but disables the optimizer probe — see [Optimizer hook](#optimizer-hook).
- **Same query string** must appear in `WHERE fts_match(...)` and `ORDER BY bm25_score(...)`. The optimizer recognizes the joint pattern; mismatched strings fall through to a slow per-row evaluation.
- **Same caveats as `fts_match`:** requires an FTS index on the column; errors clearly otherwise.

The math is the standard BM25+ variant (`k1 = 1.5`, `b = 0.75`, fixed at SQLite FTS5 defaults). Full formula in [`src/sql/fts/bm25.rs`](../src/sql/fts/bm25.rs).

---

## Hybrid retrieval

Vector-only retrieval misses keyword-grounded queries; lexical-only retrieval misses paraphrases. Hybrid combines both via SQL arithmetic:

```sql
SELECT id, title FROM docs
 WHERE fts_match(body, 'rust embedded database')
 ORDER BY 0.5 * bm25_score(body, 'rust embedded database')
        + 0.5 * (1.0 - vec_distance_cosine(embedding, [...]))
DESC LIMIT 10;
```

Two gotchas:

1. **Cosine returns *distance*, not similarity.** `vec_distance_cosine` returns `1 - cos(a, b)` — `0` for identical, `1` for orthogonal, `2` for diametrically opposite. Lower is closer. Hybrid scoring assumes "higher is better", so the SQL inverts: `1.0 - vec_distance_cosine(...)`. Forgetting this flip silently inverts the ranking.
2. **`WHERE fts_match` filters before scoring.** The optimizer pre-filters rows with no lexical match, so docs the embedding *would* have surfaced (paraphrases with no shared tokens) never get scored. Tradeoff per [Q6](phase-8-plan.md#q6-filtered-fts).

Why arithmetic and not a typed `hybrid_score(...)` function? Composability — different aggregations (`MAX`, three-way fusion, RRF) all work via the same arithmetic; a typed function would lock in one shape. See [Q8](phase-8-plan.md#q8-hybrid-retrieval).

The runnable [`examples/hybrid-retrieval/`](../examples/hybrid-retrieval/) walkthrough has weight-tuning sketches and a production checklist (BM25 normalization, real embeddings, cross-encoder re-rank).

---

## Tokenizer rules

Resolves [Q3](phase-8-plan.md#q3-tokenizer): ASCII MVP. The tokenizer ([`src/sql/fts/tokenizer.rs`](../src/sql/fts/tokenizer.rs)) splits on `[^A-Za-z0-9]+` and lowercases.

| Input | Tokens |
|---|---|
| `"Hello, World!"` | `["hello", "world"]` |
| `"co-op"` | `["co", "op"]` |
| `"rust2026"` | `["rust2026"]` (digits are alphanumeric) |
| `"FooBar BAZ"` | `["foobar", "baz"]` |
| `"café"` | `["caf"]` (non-ASCII bytes act as separators) |
| `"日本語"` | `[]` (every byte is non-ASCII) |
| `""` | `[]` |

**No stemming and no stop-list** ([Q4](phase-8-plan.md#q4-stemming) + [Q5](phase-8-plan.md#q5-stop-words)). BM25's IDF naturally downweights common terms; modern RAG pipelines rely on exact lexical matches for technical retrieval ("python the language" vs "pythons the snakes").

Unicode-aware tokenization is deferred to Phase 8.1 behind a `unicode` cargo feature.

---

## BM25 parameters

Fixed at SQLite FTS5's defaults for the MVP:

| Parameter | Value | Meaning |
|---|---|---|
| `k1` | `1.5` | Term-frequency saturation. Higher → less aggressive saturation. |
| `b` | `0.75` | Length-normalization weight. `0` → off; `1` → fully proportional. |

Per-call overrides aren't exposed yet. See `Bm25Params` in [`src/sql/fts/bm25.rs`](../src/sql/fts/bm25.rs:33) — the public struct is shaped to grow per-call configuration without breaking signatures.

---

## Lifecycle: INSERT / DELETE / UPDATE

| Operation | FTS-index behavior |
|---|---|
| **CREATE INDEX … USING fts** | Backfills from current rows; tokenizes each cell; index is ready immediately. |
| **INSERT** | Incremental update on the in-memory `PostingList` — single-row tokenize + posting append. Cheap. |
| **DELETE** | Marks the index `needs_rebuild = true`. The next save (auto-save on the next write, or explicit `COMMIT`) rebuilds the posting list from current rows before serializing. |
| **UPDATE** on the indexed TEXT column | Same as DELETE: marks dirty + rebuild on save. |
| **UPDATE** on a different column | No effect on FTS state. |
| **DROP INDEX** | Not yet supported — Phase 8.1 follow-up. Drop the table to remove the index. |

Resolves [Q7](phase-8-plan.md#q7-delete--update-on-fts-indexed-tables): rebuild-on-save mirrors HNSW's strategy. Incremental delete + tombstone compaction is a Phase 8.1 candidate when someone hits the perf wall.

---

## Persistence + file format

The in-memory `PostingList` ([`src/sql/fts/posting_list.rs`](../src/sql/fts/posting_list.rs)) survives `save_database` → `open_database` round-trips byte-equivalently. Each FTS index is a B-Tree of `KIND_FTS_POSTING` cells parallel to the table's data tree, with a sidecar cell carrying per-doc lengths so `total_docs` stays honest in BM25 even after a row tokenized to zero tokens.

**On-demand v4 → v5 file-format bump** ([Q10](phase-8-plan.md#q10-file-format-version-bump-strategy)): existing v4 databases without FTS keep writing v4; the first save with at least one FTS index attached promotes the file to v5. Decoders accept both versions. Zero migration friction for v0.1.x users who don't use FTS.

Cell format reference: [`docs/file-format.md`](file-format.md).

---

## Optimizer hook

The executor's `try_fts_probe` ([`src/sql/executor.rs`](../src/sql/executor.rs)) recognizes the canonical top-k shape:

```text
SELECT … FROM <t>
 WHERE fts_match(<col>, '<q>')
 ORDER BY bm25_score(<col>, '<q>') DESC
 LIMIT k
```

When matched, the engine serves top-k directly from the inverted index in O(query-term-count × k log k) — no full-row scan. Falls through to per-row scalar evaluation if:

- ORDER BY direction is `ASC` (BM25 is "higher = better"; ASC almost certainly indicates user error).
- The function name isn't `bm25_score`.
- The query string in `WHERE fts_match` doesn't match the one in `ORDER BY bm25_score` literally.
- `WHERE` references columns other than the one(s) the optimizer can prove are subsumed.

Mirrors `try_hnsw_probe` for vector KNN. Same WHERE-drop posture per [Q6](phase-8-plan.md#q6-filtered-fts) — additional WHERE conditions on the optimizer fast path are silently skipped; the canonical FTS query has only `WHERE fts_match(...)` so the trade-off is rarely visible. A correctness-preserving multi-index composer is deferred to a later phase.

---

## How to use it from each surface

The SQL surface is identical across every product. Here's how each entry point exposes it:

### REPL

```sh
$ cargo run -- mydb.sqlrite
sqlrite> CREATE INDEX docs_fts ON docs USING fts (body);
sqlrite> SELECT id FROM docs WHERE fts_match(body, 'rust') ORDER BY bm25_score(body, 'rust') DESC LIMIT 5;
```

### Rust library

```rust
use sqlrite::{Connection, Result};

let mut conn = Connection::open_in_memory()?;
conn.execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);")?;
conn.execute("INSERT INTO docs (body) VALUES ('rust embedded database');")?;
conn.execute("CREATE INDEX docs_fts ON docs USING fts (body);")?;

let stmt = conn.prepare(
    "SELECT id FROM docs WHERE fts_match(body, 'rust') \
     ORDER BY bm25_score(body, 'rust') DESC LIMIT 5;"
)?;
let mut rows = stmt.query()?;
while let Some(row) = rows.next()? {
    let id: i64 = row.get_by_name("id")?;
    println!("{id}");
}
# Ok::<_, sqlrite::error::SQLRiteError>(())
```

### MCP server (`bm25_search` tool)

LLM clients driving SQLRite over MCP get a typed shortcut that wraps the canonical SQL:

```jsonc
{
  "name": "bm25_search",
  "arguments": {
    "table": "docs",
    "column": "body",
    "query": "rust embedded database",
    "k": 5
  }
}
```

See [`mcp.md`](mcp.md) for the full MCP tool catalog. Symmetric with `vector_search` for vector KNN.

### Python / Node.js / Go / WASM SDKs

The SDKs go through the same Connection API as the Rust library — call `execute()` to issue `CREATE INDEX … USING fts (...)`, call `prepare()` + `query()` for `fts_match` / `bm25_score` reads. There's no SDK-specific FTS shortcut; the SQL composes the same way everywhere.

### Desktop app

Just type the SQL into the query editor. The app embeds the engine directly, so the SQL surface is identical.

---

## Limitations

Per the Phase 8 plan's "Out of scope" section, the MVP intentionally defers:

- **Multi-column FTS** — single-column per index ([Q2](phase-8-plan.md#q2-multi-column-fts-in-one-index-or-one-index-per-column)). Compose via `OR`: `WHERE fts_match(title, 'q') OR fts_match(body, 'q')`.
- **Unicode tokenization** — ASCII MVP per Q3. CJK / accented Latin won't be searchable until the `unicode` cargo feature lands in Phase 8.1.
- **Stemming + stop words** — none ([Q4](phase-8-plan.md#q4-stemming) + [Q5](phase-8-plan.md#q5-stop-words)).
- **Configurable `k1` / `b`** — fixed at FTS5 defaults; per-column field-weighting deferred.
- **Phrase queries** (`MATCH '"exact phrase"'`) — not in the MVP. Phrase queries need positional postings, doubling the index size. Phase 8.1 candidate.
- **Boolean query operators** (`AND` / `OR` / `NOT` inside the query string) — MVP uses any-term semantics. Boolean syntax deferred.
- **Highlight / snippet generation** (`snippet(col, 'q')`) — not in the MVP. Easy to add later if users want it.
- **Multi-index intersection optimizer** — FTS pre-filter + scalar WHERE ([Q6](phase-8-plan.md#q6-filtered-fts)).
- **Incremental DELETE / UPDATE** — rebuild-on-save instead ([Q7](phase-8-plan.md#q7-delete--update-on-fts-indexed-tables)).

A single posting cell that exceeds page capacity (~4 KiB) errors loudly; overflow chaining is a Phase 8.1 stretch goal. This shouldn't bite real corpora — even `'the'` in a million-row English corpus stays under the limit with varint encoding.

---

## See also

- [`docs/phase-8-plan.md`](phase-8-plan.md) — design doc with all Q1–Q10 resolutions and per-sub-phase scope.
- [`docs/file-format.md`](file-format.md) — `KIND_FTS_POSTING` cell layout, on-demand v5 bump.
- [`docs/sql-engine.md`](sql-engine.md) — `try_fts_probe` optimizer hook in the executor pipeline.
- [`docs/supported-sql.md`](supported-sql.md) — `USING fts` syntax + `fts_match` / `bm25_score` function reference.
- [`docs/mcp.md`](mcp.md) — `bm25_search` MCP tool.
- [`examples/hybrid-retrieval/`](../examples/hybrid-retrieval/) — runnable hybrid-retrieval walkthrough with weight-tuning sketches and production checklist.
- [`docs/ask.md`](ask.md) — natural-language → SQL across every surface; the prompt may pick up `fts_match` / `bm25_score` once 8f.1 wires them into the system rules.
