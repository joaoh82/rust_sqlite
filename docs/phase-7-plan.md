# Phase 7 — AI-era extensions: proposal + plan

**Status:** *approved 2026-04-26 — implementation pending.* The 10 design questions (Q1–Q10) have been resolved by the project owner; see the **Decisions** section below for the canonical answers. Each per-sub-phase section reflects the chosen design. Implementation has not yet started — sub-phase 7a is the next branch to cut.

**Audience:** primarily the project owner deciding what Phase 7 should be; secondarily future-self / contributors trying to understand the rationale once the decisions are made and code lands.

**TL;DR:** turn SQLRite from "small SQLite clone" into "small SQLite clone that's pleasant to use from an LLM agent", by adding the storage + query primitives that modern AI workloads need (vectors, JSON, full-text), the surface that LLMs naturally drive (an MCP server), and a natural-language `ask()` API exposed across every product (REPL meta-command, library method, every SDK, desktop UI, MCP tool). Stay proportional — the entire engine is ~5 kLOC today; Phase 7 should add ~3-4 kLOC, not 20 kLOC.

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
- Phase 7 budget: **~3-4 kLOC of new Rust** across all sub-phases, not counting tests and docs. The bump from 2-3 to 3-4 kLOC vs. the original draft accounts for `.ask` being exposed across every product (one library crate `sqlrite-ask` + adapters for REPL / desktop / 4 SDKs / MCP), not just the REPL. If a sub-phase blows up beyond budget, we re-scope.

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

> **Scope correction (2026-04-27, during 7b implementation):** Operators turned out to be a much bigger parser change than Q6 anticipated. sqlparser-rs (current pinned version) **fails outright** on `<->` and `<#>` ("Expected: an expression, found: ->"). Only `<=>` parses, as MySQL's `Spaceship` (null-safe equality). Supporting all three operators requires either a fork of sqlparser to extend the SQLite dialect, or a string-preprocessing pass that rewrites operators to function calls before handing SQL to the parser — neither is the "tiny parser change" Q6 estimated.
>
> **Decision:** ship 7b with **functions only**. Operators are deferred to a follow-up sub-phase **7b.1**. The KNN use case (`ORDER BY vec_distance_l2(col, [...]) LIMIT k`) still works — just verbose. When 7b.1 lands, queries can switch from function-call form to operator form without any other behavior change.

**Decisions:**

- **Dispatch in the existing expression evaluator.** No new function-registration framework — these are built-in functions like `||` is.
- **Operators land in 7b.1, not 7b.** See scope-correction note above.
- **`ORDER BY` widened to accept arbitrary expressions** as part of 7b. Pre-7b, the parser restricted ORDER BY to bare column refs; without expression support, KNN queries would have been impossible. New shape: `eval_expr` is called per-row to produce sort keys. This is a strict superset — `ORDER BY col` still works because `Expr::Identifier` takes the same path.

**LOC estimate:** ~250 lines for the functions; another ~50 for the ORDER BY parser extension. Total ~300 LOC, slightly over Q-time estimate.

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

### 7g — `ask()` API across the product surface (NL → SQL)

**What.** Natural-language → SQL is a first-class feature available everywhere SQLRite is — not just the REPL. The user types (or the agent passes) a question; we read the schema, build a prompt, call a configured LLM API, parse the response, return the generated SQL (and optionally execute it).

**Surface:**

- **REPL** — `.ask How many users are over 30?` → confirm-and-run UX
- **Rust library** — `Connection::ask("question") -> AskResponse { sql, explanation }`
- **Python SDK** — `conn.ask("question")` → returns `AskResponse(sql, explanation)`; `conn.ask_run("question")` for one-shot generate-and-execute
- **Node.js SDK** — `db.ask("question")` / `db.askRun("question")`
- **Go SDK** — `sqlrite.Ask(db, "question") (AskResponse, error)` and `AskRun(...)`
- **WASM SDK** — `db.ask("question")` (with caveats — see Q9 below)
- **Desktop app** — "Ask" button next to "Run" in the query editor; opens a prompt input, shows the generated SQL inline in the editor for review-and-run
- **MCP server** — additional `ask` tool (the MCP gets the natural-language → SQL flow as a tool, on top of the raw `query`/`execute` tools from 7h)

**Sketch — REPL:**

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

**Sketch — library:**

```rust
let resp = conn.ask("How many users are over 30?")?;
println!("LLM produced: {}", resp.sql);
// Caller decides whether to execute. The library deliberately does
// NOT auto-execute — the SDK consumer is a developer, not an
// interactive human, and silent execution of LLM-generated SQL is
// dangerous.
let rows = conn.execute(&resp.sql)?;
```

**Layered design.** The work splits into one library layer + several thin adapters:

- **7g.1 — `sqlrite-ask` crate (foundational, ~400 LOC).** New separate crate (not feature-gated on the engine) so the engine stays pure-SQL with no HTTP / async deps. Owns: provider adapters (Anthropic / OpenAI / Ollama), prompt construction, schema introspection helper that walks `sqlrite_master`, the `AskResponse` type, configuration loading from env or a passed config struct. Depends on `sqlrite-engine` for the schema introspection.
- **7g.2 — REPL `.ask` (~80 LOC).** Thin. Calls `sqlrite-ask`, prints the generated SQL, prompts `Y/n`, runs if confirmed. Most of the file is the rustyline integration.
- **7g.3 — Desktop UI (~150 LOC).** New "Ask" button + prompt input + inline SQL preview in the editor. Calls into `sqlrite-ask` from a Tauri command; the command lives in `desktop/src-tauri/`. Schema introspection runs server-side; HTTP call also server-side (so the API key stays in the app process, not the webview).
- **7g.4 — Python SDK (~80 LOC).** PyO3 wrapper around `sqlrite-ask`. `Connection.ask(question)` returns a Python object with `.sql` and `.explanation`. `Connection.ask_run(question)` is the convenience that calls `execute` after.
- **7g.5 — Node.js SDK (~80 LOC).** Same shape via napi-rs. `db.ask(question)` / `db.askRun(question)`.
- **7g.6 — Go SDK (~80 LOC).** cgo wrapper. `sqlrite.Ask(db, question)` returns `(AskResponse, error)`.
- **7g.7 — WASM SDK (~150 LOC, see Q9).** Either skipped, or implemented with a JS-side fetch hook (the WASM binary calls back into JS to make the HTTP request, since `reqwest`'s wasm32 story is messy and CORS/keys are a separate problem).
- **7g.8 — MCP server `ask` tool (~50 LOC).** Wires the existing tool framework from 7h to a single new tool that calls into `sqlrite-ask`.

**Configuration:** the same config struct is accepted everywhere, with sensible env-var defaults:

- `SQLRITE_LLM_PROVIDER` env var: `anthropic` (default) | `openai` | `ollama`
- `SQLRITE_LLM_API_KEY` env var (for cloud providers)
- `SQLRITE_LLM_MODEL` env var (default per provider)
- Library APIs accept an explicit `AskConfig` parameter that, if provided, overrides env vars. Lets SDK consumers pass keys per-connection without env shenanigans.

**Decisions:**

- **Bring-your-own-API-key.** No bundled keys, no proxied service. Users configure once via env or pass a config object.
- **Schema-aware prompt construction.** Dump `sqlrite_master` + column types + sample row counts for each table; include the user's question; demand SQL-only output. ~30-line prompt template, lives in `sqlrite-ask`. Once vector / JSON columns land (7a, 7e), the prompt teaches the LLM about them too — extends naturally.
- **Library returns SQL, doesn't auto-execute.** The caller decides. SDK convenience wrappers (`ask_run` / `askRun` / `AskRun`) exist for the obvious one-shot pattern, but the default API is "generate, return, let me decide."
- **REPL + Desktop ARE auto-execute-with-confirm.** They're interactive — confirming is the natural UX. `ask_run`-equivalent from the CLI/desktop perspective.
- **No streaming.** Wait for the full SQL response, then display. Streaming would complicate the confirm-before-run flow and the SDK return-type story.
- **No multi-turn.** Stateless — every `ask` is a fresh prompt. Conversational refinement is a separate UX problem (could be Phase 7's follow-up).

**Why a separate crate (`sqlrite-ask`) instead of a feature flag on `sqlrite-engine`:**

- The engine is currently pure-SQL with no HTTP / async deps. Adding `reqwest` + `tokio` (or `ureq` + sync) is a real weight bump even behind a feature flag — `cargo metadata` shows them, transitive deps pull in TLS, etc.
- A separate crate lets WASM callers skip it entirely (they have their own fetch story) without playing feature-flag whack-a-mole.
- Easier to evolve independently — provider adapters change much faster than the SQL engine.
- Still gets one publish channel through the existing Phase 6 lockstep — `sqlrite-ask-v<V>` joins the product wave.

**LOC estimate:** ~800-1200 lines total across all layers. The bulk (~400) is in `sqlrite-ask`; the per-product adapters are 50-150 lines each because they're thin wrappers.

**Order within 7g.** 7g.1 ships first (everything else depends on it). 7g.2 (REPL) is the natural second since it's the smallest validation. 7g.3 (Desktop) and 7g.4-6 (SDKs) parallelize after 7g.1. 7g.7 (WASM) and 7g.8 (MCP) come last.

**Open questions handled in Q4 + Q9 + Q10 below.**

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
7a (VECTOR type)              — independent, foundational
  └── 7b (distance functions) — needs 7a
        └── 7b.1 (operators)  — sugar over 7b; deferred from 7b per scope correction
        └── 7c (KNN exec opt) — needs 7b (operators not required)
              └── 7d (HNSW)   — needs 7b/7c

7e (JSON)                  — independent, can interleave anywhere

7f (FTS5)                  — independent, but big — defer if scope tight

7g (ask across products)   — 7g.1 (sqlrite-ask crate) is foundational
                             7g.2 REPL / 7g.3 desktop / 7g.4-6 SDKs / 7g.7 WASM / 7g.8 MCP-tool
                             all parallelize after 7g.1 lands

7h (MCP server)            — useful AFTER 7d + 7f because it can expose them as tools
                             7g.8 (ask-as-MCP-tool) lands inside 7h
```

Two reasonable shipping orders:

**Order A — vector-first (recommended):**

```
7a → 7b → 7c → 7d → 7e → 7g.1 → (7g.2 + 7g.3 + 7g.4 + 7g.5 + 7g.6 + 7g.7) → 7h (incl 7g.8) → 7f
```

Reasoning: vectors are the marquee Phase 7 feature. Get them all the way to "production-quality with HNSW" before sprawling. JSON is a small bolt-on. `.ask`'s prompt construction (7g.1) is more interesting once it can teach the LLM about vector + JSON columns, so 7g lands after 7a/7e. The per-product `.ask` adapters (7g.2–7g.7) parallelize. MCP closes out the wave with `.ask` as one of its tools. FTS goes last because it's optional-scope.

**Order B — agent-surface-first:**

```
7g.1 → 7g.2 → 7h → 7g.3 → 7e → 7a → 7b → 7c → 7d → 7g.4-7 → 7f
```

Reasoning: maximize "agent-shaped" surface area early so the project becomes useful in agent stacks before vectors land. Risk: `.ask`'s prompt has nothing fancy to teach the LLM about until 7a/7e land — schema-aware NL→SQL with no vector or JSON support is just "regular NL→SQL", which already exists in 50 other tools.

Recommend Order A. The first three sub-phases (7a + 7b + 7c) are tractable and end at "you can do KNN search in SQLRite" — a coherent shippable. By the time 7g.1 lands, the prompt has rich types to teach the LLM about, which is what makes a SQLRite-specific NL→SQL more compelling than a generic one.

---

## Decisions (was: open questions)

Q1–Q10 were resolved by the project owner on 2026-04-26. Each question keeps its original options + recommendation as a record of the rationale; the **Decided:** line at the top is the canonical answer the implementation should follow.

### Q1. Is FTS (7f) in or out of Phase 7?

> **Decided: OUT — defer to Phase 8.** Add FTS to the roadmap as its own next-phase work; this plan now covers seven sub-phases (7a–7e + 7g + 7h). **Follow-up note: come back to FTS in Phase 8** — the hybrid-search story (BM25 + vector combined) is genuinely useful for RAG, just not in this wave.

- **In:** Phase 7 totals ~3 kLOC. Hybrid search story is complete. ~9 sub-phases.
- **Out:** Phase 7 totals ~2 kLOC. Hybrid search becomes Phase 8. Faster to ship.

**Recommendation:** out. Defer to Phase 8. Vector + JSON + `.ask` + MCP is a coherent "AI-era" wave; FTS is its own classical-DB topic that deserves the same focus.

### Q2. HNSW parameters: fixed defaults or per-index configurable?

> **Decided: fixed defaults** (`M=16, ef_construction=200, ef_search=50`).

- **Fixed:** `M=16, ef_construction=200, ef_search=50`. Simpler API, less to test. Matches sqlite-vec's defaults.
- **Configurable:** `CREATE INDEX … USING hnsw (col) WITH (m=32, ef_construction=400)`. Power-user knobs, more code, more test matrix.

**Recommendation:** fixed defaults for MVP. Configurable can land as a follow-up if anyone actually asks.

### Q3. JSON storage format

> **Decided: bincoded `serde_json::Value`** for the MVP. JSON indexing remains a future phase.

- **bincoded `serde_json::Value`:** one-line implementation, fast read/write, opaque on disk.
- **Parsed AST as cell-encoded structure:** more code, but lets us index into JSON without a full deserialize.

**Recommendation:** bincoded `Value` for MVP. JSON indexing is a future phase; until then, opaque-on-disk is fine.

### Q4. `.ask` LLM provider — ship one or several?

> **Decided: Anthropic-first.** OpenAI + Ollama as small follow-ups within Phase 7's run.

- **Anthropic-only first:** ~150 LOC of provider adapter, ships fast. OpenAI + Ollama follow.
- **All three at once (Anthropic + OpenAI + Ollama):** ~400 LOC of provider adapters, ships once, more upfront test surface, but each is mostly identical structure.

**Recommendation:** Anthropic-first. The project owner's daily driver matters more than ecosystem-breadth on day one. OpenAI follows in a small follow-up.

(Note: Q4 only governs which provider adapters ship in `sqlrite-ask` itself. The per-SDK and desktop/REPL surfaces — sub-phases 7g.2 through 7g.8 — work the same regardless of how many providers exist underneath.)

### Q5. MCP — roll our own or use a crate?

> **Decided: roll our own.**

- **Roll our own:** ~500 LOC, fits the project's "build it yourself to understand it" theme, no external dep churn.
- **Use a crate:** smaller LOC count, depends on the crate's protocol-completeness + maintenance.

**Recommendation:** roll our own. The MCP wire format is small enough that owning it is fine, and the educational value is real.

### Q6. Operator syntax `<->` `<=>` `<#>` — do we want pgvector-style or stick to function calls?

> **Decided: operators.**

- **Operators:** prettier queries, matches PostgreSQL+pgvector convention, tiny parser change.
- **Functions only:** keeps the SQL surface smaller, less divergence from sqlparser's SQLite dialect.

**Recommendation:** operators. They're the de facto standard in vector-search SQL and writing a proper KNN query without them is verbose.

### Q7. INSERT vector literal syntax — bracket-array or function call?

> **Decided: bracket-array** (`[0.1, 0.2, 0.3]`).

- **`[0.1, 0.2, 0.3]`:** matches Python / JSON / pgvector input format. Requires a small parser hook to recognize bracket arrays as a new expression type.
- **`vector(0.1, 0.2, 0.3)`:** zero parser changes — it's just a function call. Verbose for high-dimensional vectors.

**Recommendation:** bracket-array. The verbosity tax of `vector(0.1, 0.2, ..., 0.384)` for a 384-dim embedding is real, and bracket arrays are the standard literal form across the ecosystem.

### Q9. WASM `.ask` — ship it, defer it, or hand off to JS?

> **Decided: Option B — JS-callback hook.** The WASM module does the schema-aware prompt construction; the caller passes a JS function that does the actual HTTP request. The WASM binary never sees the API key.
>
> **Documentation requirement:** when 7g.7 ships, `sdk/wasm/README.md` MUST get a prominent section explaining the callback pattern with a complete worked example (browser fetch → backend proxy → LLM provider → response back to WASM). The reason this approach exists (CORS + key-in-browser security) needs to be in the README too — otherwise the first user who tries to wire up a direct fetch from the browser will be confused why it doesn't work.

The WASM SDK has a uniquely awkward situation for `.ask`:

- **CORS:** browsers block direct cross-origin POSTs from a WASM module to `api.anthropic.com` / `api.openai.com` unless the LLM provider serves CORS headers (they don't, deliberately — they don't want users embedding raw API keys in client-side JS).
- **API key exposure:** even if CORS were OK, putting the API key into a WASM-loaded page exposes it to anyone with devtools.
- **Both problems disappear server-side.** Node.js, Python, Go, desktop (Tauri runs the call in the Rust backend, not the webview) all do the HTTP from a trusted process.

Three options for WASM specifically:

- **A. Skip:** WASM SDK does not expose `ask()` for now. Users who need it deploy a Node-based proxy or use the cloud-hosted versions of the engine.
- **B. JS-callback hook:** the WASM `db.ask(question)` returns the *generated prompt* and a list of fields, but doesn't make the HTTP call itself. The caller passes a JS function that does the call (typically routed through their own backend). The WASM side only does the schema introspection + prompt construction, never sees the API key.
- **C. Direct HTTP via JS bindings:** the WASM module imports JS `fetch` and the user supplies the API key + provider URL. Insecure for production (key in the browser) but useful for local-only / Electron-style use.

**Recommendation:** B. The "WASM does the schema-aware prompt; the caller does the HTTP" split is the cleanest security story and mirrors how every production browser-side LLM integration is built (call goes through your own backend). A few extra lines of glue for the user, but not a footgun.

### Q10. `sqlrite-ask` crate vs feature flag on `sqlrite-engine`?

> **Decided: separate crate** (`sqlrite-ask`). Adds one product line to the lockstep release wave.

- **Separate crate (`sqlrite-ask`):** zero dep weight on engine consumers who don't want LLM calls; cleaner separation; needs adding to lockstep version-bump + release pipeline.
- **Feature flag (`sqlrite-engine` + feature `ask`):** simpler dep graph; but `cargo metadata` always shows the deps even when the feature is off; transitive TLS deps from `reqwest` etc.

**Recommendation:** separate crate. Engine stays pure-SQL; LLM-stack churn (provider deprecations, API changes) doesn't ripple through engine consumers. Adds one product line to the lockstep release wave (`sqlrite-ask-v<V>`) — same shape as the other publish jobs.

### Q8. File format version bump

> **Decided: bump to v4 at the start of 7a.** Document in `docs/file-format.md` as part of 7a. All Phase 7 storage additions (VECTOR cells, JSON cells, HNSW index nodes) live inside the v4 bump — no v5 mid-Phase-7.

Adding `VECTOR`, `JSON`, and HNSW indexes all change what cells can hold. We should bump the file format version once (probably to v4) at the start of 7a and accept all three additions inside that bump. Old (pre-Phase-7) files stay readable; format-v4 files don't open in pre-Phase-7 SQLRite. Standard pattern.

**Recommendation:** bump to v4 in 7a. Document in `docs/file-format.md`.

---

## Follow-ups parked outside Phase 7

Two items the decision pass deliberately pushed out of scope but should not be forgotten:

- **FTS (BM25) → Phase 8** *(per Q1).* The hybrid-search story (BM25 + vector combined) is genuinely useful for RAG; we deferred only because Phase 7 is already big. Phase 8 should pick this up, plus a small `bm25_score(...)` × `vec_distance_cosine(...)` hybrid-ranking convenience function.
- **WASM `.ask` documentation** *(per Q9).* Sub-phase 7g.7 must land with `sdk/wasm/README.md` explaining the JS-callback pattern + a worked browser → backend → LLM-provider example. Add a checklist item to the 7g.7 PR description so reviewers catch it if missed.

---

## Per-product release implications

The Phase 6 lockstep release pipeline ships every product on every release. Phase 7 changes which products ship which features:

| Product | What 7 adds for it |
|---|---|
| Rust engine (`sqlrite-engine`) | Vector + JSON + HNSW + (optional) FTS at the SQL surface; new `Connection::ask()` re-exported from `sqlrite-ask` |
| C FFI (`sqlrite-ffi`) | Vector + JSON exposed as new C functions; `.ask` exposed via a new `sqlrite_ask()` C function (links `sqlrite-ask`) |
| Python SDK | Vector + JSON exposed as Python-native types (numpy interop where natural); `Connection.ask()` / `ask_run()` |
| Node.js SDK | Same shape — vector + JSON + `db.ask()` / `db.askRun()` |
| WASM SDK | Vector + JSON work; HNSW works (CPU only — no SIMD on wasm32 yet); `db.ask()` ships per Q9 (JS-callback shape — WASM does prompt construction, JS does the HTTP) |
| Go SDK | Vector + JSON via cgo; `sqlrite.Ask(db, ...)` / `AskRun(...)` |
| Desktop | "Ask" button in the query editor — natural-language → SQL preview → confirm-and-run. HTTP call runs in the Tauri Rust backend so the API key stays out of the webview. |
| **`sqlrite-ask` (NEW product)** | New crate. Provider adapters (Anthropic / OpenAI / Ollama), prompt construction, schema introspection helper, `AskConfig` type. Independent release tag `sqlrite-ask-v<V>`. |
| **`sqlrite-mcp` (NEW product)** | New binary. MCP server adapter exposing engine tools. Independent release tag `sqlrite-mcp-v<V>`. The `ask` MCP tool wraps `sqlrite-ask`. |

The two new products mean two extra publish jobs in `release.yml`:

- **`publish-ask`** — `cargo publish -p sqlrite-ask` to crates.io + GitHub Release `sqlrite-ask-v<V>`. Same shape as `publish-crate` for the engine.
- **`publish-mcp`** — `cargo publish -p sqlrite-mcp` to crates.io + GitHub Release `sqlrite-mcp-v<V>` with the prebuilt binary tarballs attached for the same matrix as `publish-ffi` (Linux x86_64/aarch64, macOS aarch64, Windows x86_64). MCP servers are typically run as `npx` / `uvx` / direct binaries; users want a downloadable executable, not "build from source".

**Recommendation:** treat both `sqlrite-ask` and `sqlrite-mcp` as new product lines in the lockstep version bump. Add them to `scripts/bump-version.sh`'s manifest list (now 13 manifests), add the two new tag + publish jobs to `release.yml`. Same lockstep version as everything else. The bump-version script and the tag-all step in release.yml both grow by two entries — small mechanical change, follows the same pattern as adding any other product line.

---

## What this proposal does NOT commit to

For clarity:

- No timeline / weeks-of-work estimate. Each sub-phase ships when it's ready; Phase 6 took ~2 weeks of calendar time across 9 sub-phases, but that pace is unique to the level of focus then.
- No backwards-compat guarantee for HNSW or JSON binary formats during Phase 7 itself. We bump the file format version once at the start of 7a; if internal layouts change between sub-phases (HNSW node format, JSON path encoding), files written by a mid-Phase-7 build may not open with a later mid-Phase-7 build. We promise format stability when Phase 7 closes (file format v4 finalized).
- No commitment that the entire engine has to be rewritten for vectors. The existing cell encoding is fine for them. The work is additive.
- No commitment to multi-modal embeddings, GPU acceleration, distributed indexing, or vector quantization during this phase.

---

## Next steps

1. ~~Project owner answers Q1–Q10.~~ ✅ done 2026-04-26.
2. ~~Update this document with the chosen answers.~~ ✅ done in the same commit that records this status.
3. Cut a branch for sub-phase **7a** (`feat/vector-column-type`).
4. Implementation begins.

If any of the sub-phases turn out scope-misjudged in the doing — too small, too large, missing a hidden complication — re-scope in this document and link a "scope correction" note. The plan is allowed to evolve; that's why it's written down.
