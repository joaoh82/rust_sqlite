# Phase 8 plan — Full-text search (BM25) + hybrid retrieval

**Status:** *draft 2026-05-03 — awaiting answers to Q1–Q10.* Once the open questions are resolved, each per-sub-phase section gets refined and implementation kicks off at sub-phase 8a.

**TL;DR.** Add an FTS5-style inverted index + BM25 ranking to SQLRite so users can do keyword search and (more importantly) **hybrid retrieval** — combining BM25 lexical scores with vector similarity (Phase 7d's HNSW) for RAG. Stay proportional: ~700-900 LOC of new engine code spread over six small sub-phases. Reuses the integration shape Phase 7d laid down for HNSW (CREATE INDEX … USING …, optimizer probe shortcut, persistent cell type, dedicated page tree).

---

## Why this exists (and why it was deferred)

[Phase 7's Q1](phase-7-plan.md) deferred FTS to Phase 8. Two reasons to revisit it now:

1. **Hybrid retrieval is the modern RAG standard.** Vector-only retrieval (Phase 7d) misses keyword-grounded queries — the user's literal query terms matter when they're rare or technical. Lexical + semantic combined consistently beats either alone. Without BM25 we're shipping half a RAG stack.
2. **The integration shape is already proven.** Phase 7d's HNSW work taught us the pattern: `IndexMethod` enum arm → `try_X_probe` optimizer hook → dedicated cell-kind tag → standalone algorithm module behind a feature flag. FTS plugs into the same surface; the design risk is low.

What FTS gives users:

```sql
-- Build a full-text index on a TEXT column.
CREATE INDEX docs_fts ON docs USING fts(body);

-- Keyword search, ranked by BM25 (top-k by relevance).
SELECT id, title FROM docs
WHERE fts_match(body, 'rust embedded database')
ORDER BY bm25_score(body, 'rust embedded database') DESC
LIMIT 10;

-- Hybrid retrieval — combine lexical + vector scores at any weighting.
-- vec_distance_cosine returns DISTANCE (lower = better), so we invert it.
SELECT id, title FROM docs
WHERE fts_match(body, 'rust embedded database')
ORDER BY
    0.5 * bm25_score(body, 'rust embedded database')
  + 0.5 * (1.0 - vec_distance_cosine(embedding, [0.12, -0.04, /* ... */]))
DESC
LIMIT 10;
```

---

## Sub-phases

Six chunks. Each ends with a working build, full test suite, and a commit on `main`. The first three are the load-bearing 7d-shaped trio (algorithm → SQL → persistence); the last three are surfacing + polish.

### 8a — Standalone algorithms (~250 LOC + tests)

New `src/sql/fts/` module with three free-standing pieces, no SQL integration yet:

- **`tokenizer.rs`** — splits text into terms. ASCII for the MVP (split on non-alphanumeric, lowercase). ~50 LOC.
- **`bm25.rs`** — given term frequencies + document length + corpus stats, produce a relevance score. Standard BM25 formula with `k1=1.5`, `b=0.75` (the SQLite FTS5 defaults, fixed for the MVP). ~80 LOC.
- **`posting_list.rs`** — in-memory inverted index: `BTreeMap<term, BTreeMap<rowid, term_freq>>` plus per-document length cache. Add / remove / query operations. ~120 LOC.

Tests: tokenizer round-trips, BM25 numeric reproducibility against a hand-computed reference, posting-list correctness on 1000-doc synthetic corpus.

**Mirrors the shape of `src/sql/hnsw.rs` (Phase 7d.1).** Pure algorithm, no engine dep, easy to test in isolation.

### 8b — SQL integration (~250 LOC + tests)

Wire 8a into the executor. New surfaces:

- `CREATE INDEX <name> ON <table> USING fts(<col>)` — adds an `IndexMethod::Fts` arm to the existing dispatcher (`src/sql/executor.rs` lines 383–388 + 482–485).
- `fts_match(col, 'query')` scalar — Boolean predicate, true if the row's column matches any of the query terms in the FTS index. Wired in `eval_function` (`src/sql/executor.rs:1176–1216`) alongside `vec_distance_*` and `json_extract`.
- `bm25_score(col, 'query')` scalar — returns the per-row BM25 relevance score as `Value::Real`. Same dispatch.
- `try_fts_probe` optimizer hook — recognizes `WHERE fts_match(col, 'q') ORDER BY bm25_score(col, 'q') LIMIT k` and serves it from the FTS index instead of full-scanning. Mirrors `try_hnsw_probe` (`src/sql/executor.rs:757–838`).
- INSERT / UPDATE / DELETE wiring — incremental updates on INSERT (single-row append, cheap); DELETE / UPDATE mark the index `needs_rebuild` and rebuild from current rows on next save (same as HNSW per Q7).

Tests: end-to-end CREATE INDEX → INSERT → fts_match in WHERE → bm25_score in ORDER BY → LIMIT k → expected rows in expected order, on a 100-doc corpus.

### 8c — Persistence (~250 LOC + tests)

Make FTS indexes survive `cargo build && cargo run --quiet -- foo.sqlrite` reopen.

- New `KIND_FTS_POSTING: u8 = 0x06` cell tag in `src/sql/pager/cell.rs:56–74`.
- New `src/sql/pager/fts_cell.rs` — encodes/decodes posting-list cells. One cell per (term, postings) pair; long posting lists chain via overflow cells. Modeled after `hnsw_cell.rs:7–70`.
- Each FTS index gets its own page tree parallel to secondary indexes + HNSW indexes.
- Open path: load cells directly into the in-memory inverted index — bit-for-bit reproduction, no algorithm runs.
- File-format version bump: **on-demand** per Q10. Existing v4 databases with no FTS indexes keep working as v4; first FTS-index creation rewrites page 0 to v5.

Tests: round-trip integrity (build index → save → reopen → query) under realistic-sized corpus (1k docs); concurrent reader doesn't see partial state.

### 8d — Hybrid retrieval (mostly docs, ~50 LOC)

The hybrid story from Q8: arithmetic composition over the existing `bm25_score` + `vec_distance_cosine` functions. No new SQL function needed — the example at the top of this doc works out of the box once 8b lands.

Worked example in `examples/hybrid-retrieval/`:
- Small corpus (the SQLRite `docs/` directory itself, indexed by sentence) + a tiny embedding model (or pre-baked vectors).
- A handful of queries showing pure-BM25, pure-vector, and 50/50 hybrid retrieval.
- A short README walking through what the LLM-era takeaway is — when each shape wins.

### 8e — MCP `bm25_search` tool (~50 LOC)

Adds the `bm25_search` tool to `sqlrite-mcp` (mirroring `vector_search` from Phase 7h). Per Q9 — surfaces FTS prominently to LLM agents driving SQLRite over MCP.

Schema: `{ table, column, query, k?, metric? }`. Returns rows in descending BM25 order. Uses the optimizer hook from 8b.

### 8f — Docs + smoke test (~docs only)

- `docs/supported-sql.md` — new section under CREATE INDEX for `USING fts`; new entries under "Functions" for `fts_match` and `bm25_score`.
- `docs/architecture.md` — add `src/sql/fts/` to the engine module map.
- `docs/file-format.md` — document the `KIND_FTS_POSTING` cell layout + the v4→v5 bump.
- `docs/sql-engine.md` — add the FTS optimizer hook alongside the HNSW one.
- `docs/smoke-test.md` — add a CREATE INDEX … USING fts step + a hybrid-retrieval round-trip.
- New `docs/fts.md` — the canonical reference for FTS / BM25 / hybrid retrieval, mirroring `docs/ask.md`'s shape.

---

## Open design questions

The same shape Phase 7's plan used. Each Q has a recommendation; the user resolves before coding starts.

### Q1. MATCH-operator syntax: `fts_match(col, 'q')` function or `col MATCH 'q'` operator?

SQLite's FTS5 uses `WHERE col MATCH 'query'`. `sqlparser` 0.61's SQLite dialect doesn't expose a `BinaryOperator::Match` variant for us to dispatch on, so making `MATCH` work in SQLRite means either custom post-parse rewriting or patching sqlparser.

| Option | Pros | Cons |
|---|---|---|
| **A. Function call** `fts_match(col, 'q')` | Zero parser changes. Composes with everything else in WHERE. Just another scalar fn in the existing dispatcher. | Doesn't look like SQLite's MATCH. |
| B. Custom pre-parse rewrite | Looks like SQLite. | Have to write a hand-rolled scanner over user input — fragile, and fights against sqlparser's role. |
| C. Patch sqlparser | Looks like SQLite. | Forks sqlparser or waits on upstream PR; either is too big a tax for this. |

**Recommendation: A.** Function-call shape ships clean and doesn't compromise the rest of the engine. Documented limitation: "SQLRite's FTS uses `fts_match(col, 'q')` instead of SQLite's `col MATCH 'q'` because our SQL parser doesn't expose the MATCH operator." If sqlparser adds the variant later, we add Option B as syntactic sugar in a follow-up.

### Q2. Multi-column FTS in one index, or one index per column?

SQLite's FTS5: `CREATE VIRTUAL TABLE docs USING fts5(title, body);` — one virtual table indexes multiple columns of the underlying data, and `MATCH 'q'` searches all of them.

| Option | Pros | Cons |
|---|---|---|
| **A. One index per column** | Mirrors HNSW (one index per vector column). Simpler optimizer hook. Composes via OR: `WHERE fts_match(title, 'q') OR fts_match(body, 'q')`. | Two indexes' worth of disk + memory for the common "search title or body" case. Composing scores across columns is on the user. |
| B. Multi-column index | Matches SQLite. Saves disk for the common case. | Bigger persistence change; per-column field-weighting (FTS5's `bm25(docs_fts, 10.0, 1.0)`) is a whole sub-feature. |

**Recommendation: A.** Start single-column, add multi-column as a follow-up if real users ask. Documented in `docs/fts.md`.

### Q3. Tokenizer: ASCII-only or Unicode-aware from day one?

- **ASCII** (split on `[^A-Za-z0-9]+`, lowercase): ~50 LOC, no deps. Misses CJK, mishandles accented Latin (`café` ≠ `cafe`).
- **Unicode** (use the `unicode-segmentation` crate's word-boundary splitter): correct everywhere. Adds a small dep (~30 KB compiled).

**Recommendation: ASCII for MVP.** Most RAG corpora are English / ASCII-Latin enough that this isn't blocking. Document the limitation prominently. Phase 8.1 can add Unicode tokenization with a `unicode = ["dep:unicode-segmentation"]` cargo feature.

### Q4. Stemming?

- **No stemming** (default): "running" and "run" are different terms. RAG queries typically rely on exact lexical matches anyway — stemming actively hurts technical-term retrieval ("python" the language vs "pythons" the snakes is a real concern in tech docs).
- **Snowball-style stemming**: ~200 LOC + dep, English-only without more work.

**Recommendation: no stemming.** Modern RAG approaches use raw tokens; semantic matching goes through the vector half.

### Q5. Stop words?

- **No stop list**: keeps every token, BM25's IDF naturally downweights common terms ("the" gets near-zero weight in any non-trivial corpus).
- **English stop list**: smaller index, faster queries, locks out non-English text.

**Recommendation: no stop list.** BM25's math is the right tool; stop lists are a pre-BM25 hack.

### Q6. Filtered FTS: how does `fts_match(col, 'q') AND status = 'published'` work?

The optimizer hook needs to handle a WHERE that combines an FTS predicate with other conditions.

| Option | Pros | Cons |
|---|---|---|
| **A. FTS pre-filter, scalar-eval the rest** | Simple. Mirrors how HNSW + WHERE works today. | Misses index-on-status combinations (would do an FTS scan + sequential filter even if `status` is indexed). |
| B. Multi-index intersection | Faster on selective filters. | Complex optimizer + more failure modes. |

**Recommendation: A.** Same approach as Phase 7d's HNSW + WHERE composition. The "selective non-FTS filter" case is a tractable Phase 9 optimization.

### Q7. DELETE / UPDATE on FTS-indexed tables: incremental or rebuild?

Phase 7d's HNSW marks the index `needs_rebuild` on DELETE / UPDATE and rebuilds from current rows at next save.

| Option | Pros | Cons |
|---|---|---|
| **A. Mark `needs_rebuild`, rebuild at save** | Same code shape as HNSW — small reuse. Simple, correct. | Slow on big tables; DELETEs in transaction → rebuild at COMMIT could be a noticeable pause. |
| B. Incremental remove + tombstone list | Fast updates. | More state to track + more bug surface; tombstone compaction is a real follow-up. |

**Recommendation: A.** Match HNSW's shape so the engine has one rebuild story across both index types. Document the cost; B is a Phase 8.1 if anyone hits the perf wall.

### Q8. Hybrid retrieval: arithmetic composition or typed `hybrid_score(...)`?

- **Arithmetic** (recommended): `ORDER BY 0.5 * bm25_score(col, 'q') + 0.5 * (1.0 - vec_distance_cosine(vec, [...])) DESC LIMIT k` — works the moment 8b lands; no new function needed.
- **Typed function**: `hybrid_score(0.5, bm25_score(...), 0.5, vec_distance_cosine(...))` — semantic clarity, but it's just sugar over arithmetic.

**Recommendation: arithmetic.** Document the pattern in `docs/fts.md` with the `1.0 - vec_distance_cosine` inversion gotcha called out. The first-class typed function adds API weight without earning its keep — composability via raw arithmetic is more flexible (different aggregations, three-way fusion if a user later wires in a different score, etc.).

### Q9. MCP tool: add `bm25_search` to `sqlrite-mcp`?

Phase 7h's spec listed `bm25_search` as a future tool gated behind FTS. Now that FTS is shipping:

- **Yes**: ~50 LOC of glue (mirrors `vector_search`). Surfaces FTS prominently to LLM agents driving SQLRite over MCP. Tool description teaches the LLM about lexical retrieval as an option alongside `vector_search` + `query`.
- **No**: LLM uses `query` tool with `WHERE fts_match(col, 'q')`. Works fine.

**Recommendation: yes.** Symmetric with `vector_search`; LLM clients benefit from one less affordance to fish for.

### Q10. File-format version bump strategy: always-bump or on-demand?

The disk format is currently v4 (bumped in 7a for VECTOR). Adding `KIND_FTS_POSTING` requires another bump.

| Option | Pros | Cons |
|---|---|---|
| **A. On-demand bump** (only when first FTS index is created) | Existing v4 databases without FTS keep working unmodified. Zero migration friction for current users. | Two valid in-the-wild versions (v4 and v5) to support. |
| B. Always-bump on next release | Single version to maintain. | Every existing user's database file needs to be opened-and-resaved by sqlrite-engine 0.1.26+; old engine versions can't open them. |

**Recommendation: A.** On-demand. Existing v0.1.x databases continue to open against v0.1.26+ without surprise; the bump only kicks in when the user actually creates an FTS index. Documented in `docs/file-format.md`.

---

## Implementation order + dependencies

```
8a (algorithms)        — standalone, no deps; foundational
  └── 8b (SQL surface) — needs 8a
        └── 8c (persistence) — needs 8b
              ├── 8d (hybrid docs + example)  — parallel after 8c
              ├── 8e (MCP bm25_search tool)   — parallel after 8c
              └── 8f (docs + smoke test)      — last; documents what shipped
```

Sub-phases land as their own PR + release-PR + release wave (continuing the lockstep cadence). The 8d / 8e / 8f trio likely fold into one PR each since they're small.

**Suggested release cadence:**

| Release | Lands |
|---|---|
| v0.2.0 | 8a + 8b + 8c (the load-bearing FTS work — major-version bump because the file format changed and users see a new SQL surface) |
| v0.2.1 | 8d (hybrid example + docs) |
| v0.2.2 | 8e (MCP tool) |
| v0.2.3 | 8f (final docs sweep) |

Argument for the 0.1.x → 0.2.x bump: the file format changed (v4 → v5) and we're adding a substantial new SQL surface. The 0.1.x cycle covered everything from "modernize the codebase" through Phase 7's AI-era extensions; v0.2.0 is the right place to mark the FTS arrival.

Alternatively, fold all six sub-phases into one v0.1.26 release if the work runs small. We'll see how 8a-8c size up before deciding.

---

## Total scope estimate

| Sub-phase | LOC (engine) | LOC (tests) | LOC (docs) |
|---|---|---|---|
| 8a — algorithms | ~250 | ~200 | — |
| 8b — SQL integration | ~250 | ~300 | — |
| 8c — persistence | ~250 | ~200 | — |
| 8d — hybrid example | ~50 | — | ~150 |
| 8e — MCP tool | ~50 | ~100 | — |
| 8f — docs sweep | — | — | ~600 |
| **Total** | **~850** | **~800** | **~750** |

About 2.4 kLOC overall, ~850 of which is engine code. Comparable to Phase 7d (HNSW) which clocked in around 1.8 kLOC across its three sub-phases.

---

## Out of scope (deferred to Phase 8.1+ or beyond)

- **Multi-column FTS** (Q2) — single-column for the MVP.
- **Unicode tokenization** (Q3) — ASCII for the MVP, follow-up cargo feature.
- **Stemming + stop words** (Q4 + Q5) — not on the roadmap.
- **Configurable BM25 parameters** — `k1` and `b` are fixed at SQLite-FTS5 defaults (1.5 / 0.75). Per-column field-weighting deferred.
- **Phrase queries** (`MATCH '"exact phrase"'`) — single-token matching only for the MVP. Phrase queries need positional postings, doubling the index size. Phase 8.1.
- **Query operators** (`AND` / `OR` / `NOT` inside the query string) — for the MVP, `fts_match(col, 'a b c')` matches rows containing `a` OR `b` OR `c` (any-term). Boolean query syntax deferred to Phase 8.1.
- **Highlight / snippet generation** (`snippet(col, 'q')`) — not in the MVP. Easy to add later if users want it.
- **Multi-index intersection optimizer** (Q6) — FTS pre-filter + scalar WHERE for the MVP.
- **Incremental DELETE / UPDATE** (Q7) — rebuild-on-save for the MVP.

---

## Risks + things to watch

1. **Persistence is the load-bearing risk.** Phase 7d.3 estimated 300 LOC for HNSW persistence, shipped at ~600. FTS posting lists could similarly blow the estimate — long posting lists need overflow chaining. Watch for it; budget +50% on 8c if signs of growth appear early.
2. **Tokenizer edge cases.** Numeric tokens, hyphenated words, URL-like text — even ASCII tokenization has surprising corners. Test against a realistic corpus early, not just synthetic.
3. **Index update during transaction.** A `BEGIN; INSERT ... INSERT ... COMMIT;` block currently does N incremental FTS updates inside the in-memory snapshot, then the auto-save fires once at COMMIT. The persistence code (8c) needs to write a single batched update rather than N per-row writes. Mirrors how HNSW persistence handles transactions.
4. **MCP `bm25_search` and the `sqlrite-mcp` `--no-default-features` build.** The current `--no-default-features` MCP build (six tools, no `ask`) should also lose `bm25_search` — wait, that doesn't quite work because `bm25_search` doesn't depend on the LLM. Resolution: `bm25_search` is gated behind a new `fts` cargo feature on the engine, default-on. The MCP server's tool list already follows the engine's cargo features for `ask`; same shape for `fts`.

---

## Quick reference: how Phase 8 plugs into the engine

Concrete file:line touch points for anyone reading the eventual PRs:

| Component | File | Phase 8 change |
|---|---|---|
| `IndexMethod` enum | `src/sql/executor.rs:482-485` | add `Fts` variant |
| `IndexType::Custom` dispatch | `src/sql/executor.rs:383-388` | add `"fts"` arm |
| `try_*_probe` optimizer hook | `src/sql/executor.rs:757-838` (HNSW reference) | new `try_fts_probe` |
| Scalar-fn dispatcher | `src/sql/executor.rs:1176-1216` | add `fts_match` + `bm25_score` arms |
| Cell kind tag table | `src/sql/pager/cell.rs:56-74` | add `KIND_FTS_POSTING: u8 = 0x06` |
| Cell encoding template | `src/sql/pager/hnsw_cell.rs:7-70` | model new `fts_cell.rs` after this |
| Table schema (per-table indexes) | `src/sql/db/table.rs` | add `fts_indexes: Vec<FtsIndex>` (mirrors `hnsw_indexes`) |
| New module | `src/sql/fts/` | tokenizer + bm25 + posting_list + (later) cell |
| MCP tool | `sqlrite-mcp/src/tools/bm25_search.rs` (NEW) | mirrors `vector_search.rs` |
| Engine `fts` feature | `Cargo.toml` `[features]` | add `fts` (default-on); gate everything above |

---

## See also

- [`docs/phase-7-plan.md`](phase-7-plan.md) — the template this plan follows; specifically the §7d (HNSW) sub-phases that 8a-8c mirror most closely.
- [`docs/architecture.md`](architecture.md) — workspace + engine module map.
- [`docs/sql-engine.md`](sql-engine.md) — `process_command` + executor architecture; FTS optimizer hook will appear here once 8b lands.
- [`docs/file-format.md`](file-format.md) — page format reference; `KIND_FTS_POSTING` documented here once 8c lands.
- [`docs/mcp.md`](mcp.md) — MCP server tool reference; `bm25_search` documented here once 8e lands.
- [`docs/ask.md`](ask.md) — natural-language → SQL feature; once FTS ships, the LLM's prompt gets to teach the model about `fts_match` + `bm25_score` (small follow-up to `sqlrite-ask`'s system rules).
