# Phase 7 — AI-era extensions: proposal + plan

**Status:** *proposal, not yet implemented.* This document is the design + scoping pass before any code lands. The roadmap entry for Phase 7 in `roadmap.md` is intentionally a one-line stub until this doc resolves into shipped sub-phases.

**Audience:** primarily the project owner deciding what Phase 7 should be; secondarily future-self / contributors trying to understand the rationale once the decisions are made and code lands.

**TL;DR:** turn SQLRite from "small SQLite clone" into "small SQLite clone that's pleasant to use from an LLM agent", by adding the storage + query primitives that modern AI workloads need (vectors, JSON, full-text), the surface that LLMs naturally drive (an MCP server), and a small natural-language convenience for humans (`.ask` REPL command). Stay proportional — the entire engine is ~5 kLOC today; Phase 7 should add ~2-3 kLOC, not 20 kLOC.

---

## Why bother — what "AI-era" means here

Three forces have changed what an embedded database needs to do:

1. **Retrieval-augmented generation (RAG) is now a baseline pattern.** Every LLM-using app stores embeddings somewhere and does similarity search at query time. Today people reach for Pinecone / Weaviate / Chroma / pgvector / sqlite-vec. An embedded database without vectors is awkward to use in this stack — you end up running two databases.
2. **LLMs are read/write users of databases, not just humans.** An agent given access to a SQL database wants schema introspection, query plans, error messages it can recover from, and a stable RPC surface. MCP (Model Context Protocol) is becoming the standard "shape" for that surface.
3. **JSON is the universal interchange format for LLM output.** Every structured output is JSON; every tool call is JSON. Storing JSON without losing structure (and querying into it) is now table stakes.

SQLite's response to all three has been to grow extensions: sqlite-vec for vectors, FTS5 for full-text, JSON1 for JSON. SQLRite can do the same — and because we control the engine, we can implement these as first-class types rather than virtual-table hacks. That's actually a more interesting learning exercise than wiring extensions to a pre-built engine.

The "Phase 7" framing is a deliberate echo of how the project has evolved through phases 0–6: take a single classical-database concern (parsing, storage, indexes, durability, distribution) and build it from scratch to understand it. Phase 7 picks the AI-shaped concerns.

---

## Scope philosophy

What Phase 7 IS:

- **Implementation of the primitives.** ANN index from scratch (HNSW). JSON column with path queries. Full-text BM25 index. These are the "build it yourself to understand it" payoffs.
- **The surface agents need.** An MCP server adapter so an LLM can drive SQLRite through Claude / Codex / etc. without us writing language-specific glue.
- **A small NL→SQL convenience.** REPL `.ask` that calls a configured LLM API and shows the SQL before running it. Not a research project — a thin wrapper. The educational interest is more in the prompt+schema construction than in the LLM call.

What Phase 7 is NOT:

- **Hosting / training models.** No local model weights, no `cargo install sqlrite-llm`. We integrate; we don't host.
- **A vector database.** We're a SQL engine that happens to do similarity search well. We're not competing with Weaviate / Pinecone on scale, replication, or distributed indexing.
- **GPU-anything.** CPU only. HNSW + cosine-distance on f32 is fast enough for the engine's target sizes (millions of vectors, not billions).
- **Multi-modal.** Text embeddings only (i.e., `VECTOR(N)` of f32). Image embeddings are the same shape underneath; we don't need to pick a story for them.
- **Approximate distance.** Brute-force scans use full precision; HNSW returns the same f32 distance values as a brute-force scan would. No int8 / fp16 tricks (yet).

Numbers to sanity-check scope:

- Engine today: ~5 kLOC of Rust, plus 7 SDKs.
- sqlite-vec (the closest comp): ~1500 LOC of C for vector + brute-force + IVF. We'll be larger because we have HNSW (more code than IVF) but smaller in places because we don't have to pretend to be a virtual table.
- Phase 7 budget: **~2-3 kLOC of new Rust** across all sub-phases, not counting tests and docs. If a sub-phase blows up, we re-scope.

---

## Sub-phase breakdown

Eight proposed sub-phases. Order is deliberate — each builds on the previous, and any can be a stopping point if we want to ship a release wave with just the first few.

Numbering picks up after Phase 6 (which used 6a–6i), so Phase 7 sub-phases are 7a–7h.

### 7a — `VECTOR(N)` column type (storage only)

**What.** Add a `VECTOR(dimension)` data type to `CREATE TABLE`. Dense fixed-dimension f32 array. Stored as a length-prefixed cell payload (re-uses the Phase 3c cell encoding — the cell body is just `dim` × 4 bytes of little-endian f32).

**Syntax sketch:**

```sql
CREATE TABLE docs (
  id INTEGER PRIMARY KEY,
  title TEXT,
  embedding VECTOR(384)
);

INSERT INTO docs VALUES (1, 'first', [0.1, 0.2, 0.3, ..., 0.0]);
```

**Decisions baked in:**

- **f32, not f64.** Industry-standard for embeddings. Halves storage vs f64. No int8 quantization yet.
- **Fixed dimension per column.** Validated at INSERT — wrong dimension is a clean type error. Variable-dim per-row is a complication we don't need.
- **No NULLs in vectors yet.** A NULL row gets a NULL cell; you can't have a vector with a NULL element.

**LOC estimate:** ~300 lines. Touches `parser/create.rs`, `parser/insert.rs`, `db/table.rs`, `pager/cell.rs`, the executor for type-checking + display.

**Tests:** roundtrip insert+select, dimension mismatch errors, large dimensions (e.g., 1536 for OpenAI ada-002 size).

---

### 7b — Distance functions + KNN syntax

**What.** Three SQL functions and a KNN-style operator. All return f32; usable in `SELECT`, `WHERE`, `ORDER BY`.

```sql
SELECT id, title, vec_distance_l2(embedding, [0.1, ...]) AS dist
FROM docs
ORDER BY dist
LIMIT 10;

-- Or with the pgvector-style operator (sugar over vec_distance_*):
SELECT id, title FROM docs ORDER BY embedding <-> [0.1, ...] LIMIT 10;
```

**Functions:**

- `vec_distance_l2(a, b)` — Euclidean distance √Σ(aᵢ−bᵢ)²
- `vec_distance_cosine(a, b)` — 1 − (a·b) / (‖a‖·‖b‖)
- `vec_distance_dot(a, b)` — −(a·b) — negated so smaller-is-closer matches the others

**Operators (syntactic sugar):**

- `<->`  → `vec_distance_l2`
- `<=>`  → `vec_distance_cosine`
- `<#>`  → `vec_distance_dot`

**Decisions:**

- **Dispatch in the existing expression evaluator.** No new function-registration framework — these are built-in functions like `||` is.
- **Operators land in the parser as new infix tokens.** sqlparser's SQLite dialect doesn't have these; we either extend the dialect or post-process the AST. Either is fine.

**LOC estimate:** ~250 lines.

**Tests:** all three distance metrics against hand-computed values; operator parsing; KNN result ordering.

---

### 7c — Brute-force KNN executor optimization

**What.** Recognize the pattern `ORDER BY <distance-expr> LIMIT k` and execute it with a bounded min-heap (size k) instead of a full sort. O(N log k) instead of O(N log N).

**Why a separate sub-phase.** 7b makes it work; 7c makes it fast enough to be useful on millions of rows. Worth shipping as its own commit so the perf delta is visible in benchmarks.

**LOC estimate:** ~150 lines including a tiny benchmark to prove the speedup.

---

### 7d — HNSW ANN index

**What.** A new index variant: `CREATE INDEX ix_docs_embedding ON docs USING hnsw (embedding)`. The optimizer probes it for the same `ORDER BY <distance> LIMIT k` pattern from 7c, returning approximate-but-fast results.

**Algorithm choice:**

- **HNSW** (Hierarchical Navigable Small World). Industry default. Simple to implement (~500-700 LOC). Good recall at small k. Works well in-memory; persistence is the slightly-tricky part for us.
- **Not IVF, not LSH, not Annoy.** HNSW dominates in benchmarks for the index sizes SQLRite cares about. Picking one keeps the project focused.

**Persistence:**

- Each HNSW node = one cell. Cell body: `node_id (varint) | layer (u8) | neighbor_count (varint) | neighbor_ids[N] (varint each)`.
- The whole index lives in its own page tree (same shape as the secondary indexes from Phase 3e, just with a different cell payload).
- Insert into an HNSW-indexed table = standard table INSERT + index-side neighbor-update. Update neighbors transactionally with the row insert.

**Decisions to make before implementation** (see Open Questions):

- HNSW parameters (M, ef_construction, ef_search) — fixed defaults vs configurable per-index?
- How to handle DELETE — true deletion or soft-delete + rebuild? (HNSW doesn't have great delete-in-place semantics.)

**LOC estimate:** ~700-900 lines. The big sub-phase.

**Tests:** recall@k vs brute-force baseline (should be ≥ 0.95 on standard benchmark vectors); insert performance; delete semantics; persistence roundtrip.

---

### 7e — JSON column type + path queries

**What.** New `JSON` data type. Store as bincoded `serde_json::Value` (or as a parsed AST — see open questions). Support a small set of extraction functions:

- `json_extract(col, '$.path')` — returns the value at the path, NULL if absent
- `json_array_length(col, '$.path')` — array length, NULL for non-array
- `json_object_keys(col, '$.path')` — TEXT array of keys, NULL for non-object
- `json_type(col, '$.path')` — `'null'`, `'bool'`, `'number'`, `'string'`, `'array'`, `'object'`

**Why this matters for AI-era specifically.** LLM tool-call outputs are JSON. RAG citation arrays are JSON. Agent scratchpads are JSON. Storing them as TEXT and re-parsing on every query is wasteful.

**Decisions:**

- **JSON path subset.** Just `$.foo`, `$.foo.bar`, `$.arr[0]`, `$.foo[*]`. Not the full JSONPath spec.
- **No JSON indexing yet.** `WHERE json_extract(col, '$.foo') = 'bar'` falls back to full scan. Indexing JSON paths is its own future phase.

**LOC estimate:** ~400 lines (most of it the path parser + executor).

---

### 7f — Full-text search with BM25

**What.** `FTS5`-style virtual-ish table for keyword search. `CREATE VIRTUAL TABLE docs_fts USING fts(title, body);`. Match queries with `MATCH 'query string'` and rank with BM25.

**Decisions:**

- **Inverted index, posting lists, BM25 ranking.** Same primitives FTS5 uses. ~600-800 LOC.
- **Tokenizer.** Just whitespace-and-punctuation for MVP. Stemming and ICU come later if needed.
- **Hybrid search story.** No syntax sugar for "BM25 score + vector distance combined" yet — users do `ORDER BY 0.5 * bm25_score + 0.5 * vec_distance_cosine(...)` themselves. Hybrid-as-first-class is a future phase.

**LOC estimate:** ~600-800 lines.

**Open question:** is FTS in scope for Phase 7, or should it be its own Phase 8? It's the largest sub-phase by LOC and arguably orthogonal to the LLM-era theme. Strongest argument for keeping it: BM25 + vector together (hybrid search) is the modern standard for RAG retrieval. Strongest argument for splitting: doubles the implementation budget.

---

### 7g — `.ask` REPL command (NL → SQL)

**What.** A new meta-command: `.ask How many users signed up last week?` reads the current schema, builds a prompt, calls a configured LLM API, parses the response, shows the generated SQL, asks for `Y/n` confirmation, executes.

**Sketch:**

```
sqlrite> .ask How many users are over 30?
Generated SQL:
  SELECT COUNT(*) FROM users WHERE age > 30;
Run? [Y/n] y
+-------+
| count |
+-------+
| 47    |
+-------+
```

**Configuration:**

- `SQLRITE_LLM_PROVIDER` env var: `anthropic` (default) | `openai` | `ollama`
- `SQLRITE_LLM_API_KEY` env var (for cloud providers)
- `SQLRITE_LLM_MODEL` env var (default per provider)

**Decisions:**

- **Bring-your-own-API-key.** No bundled keys, no proxied service. Users configure once.
- **Prompt construction.** Schema-aware — dump `sqlrite_master` + sample row counts for each table; include the user's question; demand SQL-only output. ~30-line prompt template.
- **No streaming.** Wait for the full SQL response, then display. Streaming would complicate the confirm-before-run flow.
- **No multi-turn.** Stateless — every `.ask` is a fresh prompt. Conversational refinement is a separate UX problem.

**LOC estimate:** ~300-400 lines (HTTP client + provider adapters + prompt construction + REPL integration).

**Open question:** which provider's HTTP shape to ship first? Anthropic is what the project's owner uses (per the global notes about Claude API skill); OpenAI has wider compatibility in the ecosystem. I'd lean Anthropic-first with OpenAI-compatible as a follow-up — the project owner's daily-driver path matters more than catering to every reader.

---

### 7h — MCP server adapter

**What.** A new binary `sqlrite-mcp` (separate from the REPL `sqlrite` binary) that wraps a SQLRite database as an MCP server. LLM agents (Claude, Codex, etc.) connect over stdio, get a fixed set of tools, can drive the database without any custom integration.

**Tools exposed:**

- `list_tables()` → schema
- `describe_table(name)` → columns, indexes, sample row count
- `execute(sql)` → status + affected rows
- `query(sql)` → rows as JSON
- `vector_search(table, embedding, k)` → KNN results (only available if 7d's HNSW is built)
- `bm25_search(table, query, k)` → BM25 results (only if 7f's FTS is built)

**Why a separate binary.** MCP servers run as long-lived stdio processes. The REPL is interactive. They're the same engine but very different lifecycles. Two binaries, one lib (the engine), no shared-state weirdness.

**LOC estimate:** ~400-500 lines (MCP protocol implementation + tool definitions + binary entrypoint).

**Open question:** roll our own MCP wire-format (one Tokio + serde_json file) vs use an existing crate? The MCP protocol is small enough (JSON-RPC over stdio + a defined tool/resource shape) that rolling it ourselves stays educational. There are crates like `mcp-server-rs` we could use; preference depends on whether the spec is stable enough that a hand-rolled version won't bitrot.

---

## Implementation order + dependencies

```
7a (VECTOR type)           — independent, foundational
  └── 7b (distances)       — needs 7a
        └── 7c (KNN exec)  — needs 7b
              └── 7d (HNSW)— needs 7b/7c

7e (JSON)                  — independent, can interleave anywhere

7f (FTS5)                  — independent, but big — defer if scope tight

7g (.ask)                  — independent, useful early for "show off" purposes

7h (MCP)                   — useful AFTER 7d + 7f because it can expose them as tools
```

Two reasonable shipping orders:

**Order A — vector-first (recommended):**

```
7a → 7b → 7c → 7d → 7e → 7g → 7h → 7f
```

Reasoning: vectors are the marquee Phase 7 feature. Get them all the way to "production-quality with HNSW" before sprawling. JSON is a small bolt-on. `.ask` and MCP are the "show off" demos. FTS goes last because it's optional-scope.

**Order B — agent-surface-first:**

```
7g → 7h → 7e → 7a → 7b → 7c → 7d → 7f
```

Reasoning: maximize "agent-shaped" surface area early so the project becomes useful in agent stacks before vectors land. Risk: `.ask` and MCP without vector search are less impressive demos.

Recommend Order A. The first three sub-phases (7a + 7b + 7c) are tractable in a small amount of work and end at "you can do KNN search in SQLRite". That's a coherent shippable.

---

## Open questions — decide before starting

These need a call from the project owner before implementation kicks off. Listed in rough order of "biggest impact on scope":

### Q1. Is FTS (7f) in or out of Phase 7?

- **In:** Phase 7 totals ~3 kLOC. Hybrid search story is complete. ~9 sub-phases.
- **Out:** Phase 7 totals ~2 kLOC. Hybrid search becomes Phase 8. Faster to ship.

**Recommendation:** out. Defer to Phase 8. Vector + JSON + `.ask` + MCP is a coherent "AI-era" wave; FTS is its own classical-DB topic that deserves the same focus.

### Q2. HNSW parameters: fixed defaults or per-index configurable?

- **Fixed:** `M=16, ef_construction=200, ef_search=50`. Simpler API, less to test. Matches sqlite-vec's defaults.
- **Configurable:** `CREATE INDEX … USING hnsw (col) WITH (m=32, ef_construction=400)`. Power-user knobs, more code, more test matrix.

**Recommendation:** fixed defaults for MVP. Configurable can land as a follow-up if anyone actually asks.

### Q3. JSON storage format

- **bincoded `serde_json::Value`:** one-line implementation, fast read/write, opaque on disk.
- **Parsed AST as cell-encoded structure:** more code, but lets us index into JSON without a full deserialize.

**Recommendation:** bincoded `Value` for MVP. JSON indexing is a future phase; until then, opaque-on-disk is fine.

### Q4. `.ask` LLM provider — ship one or several?

- **Anthropic-only first:** ~300 LOC, ships fast. OpenAI-compatible follows.
- **All three at once (Anthropic + OpenAI + Ollama):** ~500 LOC, ships once, more upfront test surface.

**Recommendation:** Anthropic-first. The project owner's daily driver matters more than ecosystem-breadth on day one. OpenAI follows in a small follow-up.

### Q5. MCP — roll our own or use a crate?

- **Roll our own:** ~500 LOC, fits the project's "build it yourself to understand it" theme, no external dep churn.
- **Use a crate:** smaller LOC count, depends on the crate's protocol-completeness + maintenance.

**Recommendation:** roll our own. The MCP wire format is small enough that owning it is fine, and the educational value is real.

### Q6. Operator syntax `<->` `<=>` `<#>` — do we want pgvector-style or stick to function calls?

- **Operators:** prettier queries, matches PostgreSQL+pgvector convention, tiny parser change.
- **Functions only:** keeps the SQL surface smaller, less divergence from sqlparser's SQLite dialect.

**Recommendation:** operators. They're the de facto standard in vector-search SQL and writing a proper KNN query without them is verbose.

### Q7. INSERT vector literal syntax — bracket-array or function call?

- **`[0.1, 0.2, 0.3]`:** matches Python / JSON / pgvector input format. Requires a small parser hook to recognize bracket arrays as a new expression type.
- **`vector(0.1, 0.2, 0.3)`:** zero parser changes — it's just a function call. Verbose for high-dimensional vectors.

**Recommendation:** bracket-array. The verbosity tax of `vector(0.1, 0.2, ..., 0.384)` for a 384-dim embedding is real, and bracket arrays are the standard literal form across the ecosystem.

### Q8. File format version bump

Adding `VECTOR`, `JSON`, and HNSW indexes all change what cells can hold. We should bump the file format version once (probably to v4) at the start of 7a and accept all three additions inside that bump. Old (pre-Phase-7) files stay readable; format-v4 files don't open in pre-Phase-7 SQLRite. Standard pattern.

**Recommendation:** bump to v4 in 7a. Document in `docs/file-format.md`.

---

## Per-product release implications

The Phase 6 lockstep release pipeline ships every product on every release. Phase 7 changes which products ship which features:

| Product | What 7 adds for it |
|---|---|
| Rust engine | Everything — engine-level features |
| C FFI | Vector type + KNN search exposed as new C functions; JSON likewise |
| Python | Vector + JSON exposed as Python-native types (numpy interop?); `.ask` not exposed (REPL-specific) |
| Node.js | Same as Python — vector + JSON; `.ask` not exposed |
| WASM | Vector + JSON work; HNSW works (CPU only — no SIMD on wasm32 yet); `.ask` not exposed |
| Go | Vector + JSON via cgo; `.ask` not exposed |
| Desktop | UI for vector queries? (out of scope for Phase 7, future polish) |
| MCP server | New product — its own crate, its own release tag `sqlrite-mcp-v<V>` |

The MCP server addition means an extra `publish-mcp` job in `release.yml` (parallels publish-go's "tag + GitHub Release" pattern, no registry upload).

**Recommendation:** treat MCP as an 8th product line in the lockstep version bump. Add it to `scripts/bump-version.sh`'s manifest list, add a tag + release job to `release.yml`. Same lockstep version as everything else.

---

## What this proposal does NOT commit to

For clarity:

- No timeline / weeks-of-work estimate. Each sub-phase ships when it's ready; Phase 6 took ~2 weeks of calendar time across 9 sub-phases, but that pace is unique to the level of focus then.
- No backwards-compat guarantee for HNSW or JSON binary formats during Phase 7 itself. We bump the file format version once at the start of 7a; if internal layouts change between sub-phases (HNSW node format, JSON path encoding), files written by a mid-Phase-7 build may not open with a later mid-Phase-7 build. We promise format stability when Phase 7 closes (file format v4 finalized).
- No commitment that the entire engine has to be rewritten for vectors. The existing cell encoding is fine for them. The work is additive.
- No commitment to multi-modal embeddings, GPU acceleration, distributed indexing, or vector quantization during this phase.

---

## What lands in roadmap.md when this proposal is approved

Once the open questions are answered, the Phase 7 stub in `roadmap.md` gets replaced with:

```
## Phase 7 — AI-era extensions

Sub-phases 7a–7g (FTS deferred to Phase 8 per Q1). See
docs/phase-7-plan.md for the full design rationale. Each
sub-phase ships as its own PR + release wave through the
Phase 6 pipeline.
```

Plus per-sub-phase entries that get filled in as they ship — the same shape as Phase 6's sub-phase status list.

---

## Next steps

1. Project owner answers Q1–Q8.
2. Update this document with the chosen answers (so it becomes a record of decisions, not just a proposal).
3. Cut a branch for sub-phase 7a (`feat/vector-column-type`).
4. Implementation begins.

If any of the sub-phases turn out scope-misjudged in the doing — too small, too large, missing a hidden complication — re-scope in this document and link a "scope correction" note. The plan is allowed to evolve; that's why it's written down.
