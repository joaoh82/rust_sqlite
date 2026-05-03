# Hybrid retrieval (BM25 + vector)

Phase 8d worked example. Combines `bm25_score` ([Phase 8 plan](../../docs/phase-8-plan.md) — lexical, exact-term matching) with `vec_distance_cosine` ([Phase 7 plan](../../docs/phase-7-plan.md) — semantic, embedding-space proximity) into a single `ORDER BY`. Same corpus, three rankings, one short Rust file.

```bash
cargo run --example hybrid-retrieval
```

Expected output (truncated to the rankings):

```
===  1. Pure BM25 (lexical) ===
  1. doc3  "sqlite is an embedded database engine"
  2. doc4  "postgres is a powerful relational database server"

===  2. Pure vector (semantic) ===
  1. doc3  "sqlite is an embedded database engine"
  2. doc4  "postgres is a powerful relational database server"
  3. doc6  "redis caches data in memory for fast lookups"

===  3. Hybrid (50% BM25 + 50% inverted cosine) ===
  1. doc3  "sqlite is an embedded database engine"
  2. doc4  "postgres is a powerful relational database server"
```

## What the example shows

The query is `"small embedded database"`. The corpus is six tech blurbs with hand-picked 4-dim embeddings (axes are roughly: systems / scripting / database / web). The pre-baked vectors stand in for what an embedding model would give you in production — the math is identical; the worked example just doesn't pull in a 1 GB transformer.

- **Pure BM25** finds two docs that literally contain `embedded` or `database`. It cannot return a third — no other row shares any query term, and the FTS optimizer hook serves the top-k from rows that do match. The "small" token finds zero hits because nothing in the corpus uses that word.
- **Pure vector** ranks _every_ doc by cosine distance to the query embedding `[0.0, 0.0, 0.9, 0.2]`. It surfaces `doc6` ("redis … in memory for fast lookups") in third place — semantically related to "database/storage" but containing none of the literal query terms. Lexical search would never return it.
- **Hybrid** sums a normalized BM25 score with `1.0 - vec_distance_cosine` (cosine returns _distance_, lower = closer, so we invert it for "higher is better"). It picks the same top-2 as pure BM25 because those are the only docs in the FTS-match set. The fusion's value isn't visible on _this_ query — a deliberate choice; see "When hybrid wins" below.

## When each shape wins

| Scenario | Pure BM25 | Pure vector | Hybrid |
|---|---|---|---|
| Query has rare exact terms (`"redis"`, a SKU, an error code) | ✅ Wins | ❌ Spurious neighbours | ✅ BM25 dominates the score sum |
| Query is conceptual with no overlap (`"in-memory cache"` vs corpus that uses "lookup table") | ❌ Zero hits | ✅ Finds the analog | (degenerate — `WHERE fts_match` returns ∅) |
| Query has both terms _and_ semantic intent (`"fast embedded SQL"`) | Returns several near-tied lexical hits | Reorders by closeness to true intent | ✅ Best of both — wins consistently |
| LLM-generated paraphrases of the user's question | ❌ Vocabulary drift kills recall | ✅ Survives paraphrase | ✅ |
| Code search, log search, SKU lookup | ✅ | ❌ | ✅ |

The 50/50 weight is a default, not a decision. Most production RAG stacks tune the weights per workload (e.g. 0.3/0.7 vector-heavy for paraphrased queries, 0.7/0.3 lexical-heavy for technical docs). Different aggregations also work — `MAX`, `MIN`, [reciprocal rank fusion](https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf) — and SQLRite's arithmetic composition is flexible enough to express any of them. We picked plain weighted sum because it's the most-obvious default and lets you change weights by editing two numbers.

## Two gotchas

### 1. Cosine returns _distance_, not similarity

`vec_distance_cosine` returns `1 - cos(a, b)`: `0` for identical, `1` for orthogonal, `2` for diametrically opposite. Lower is closer. Hybrid scoring assumes "higher is better" everywhere, so we invert: `1.0 - vec_distance_cosine(...)`. Forgetting this flip is the most common mistake — the resulting ranking is the _opposite_ of what you want, and it'll look subtly broken.

### 2. `WHERE fts_match` filters before the score is computed

The optimizer hook recognizes `WHERE fts_match(col, 'q') ORDER BY bm25_score(col, 'q') DESC LIMIT k` and serves it from the FTS index — fast, scales to millions of rows. The catch: when the hybrid `ORDER BY` mixes in `vec_distance_cosine`, the query still requires a `WHERE fts_match(...)` clause for the optimizer to recognize the FTS shape. That filter eliminates rows with no lexical match before the vector half of the score gets a chance to rank them.

If your goal is "find docs that match _either_ the lexical OR the semantic query", drop the `WHERE` clause and let the engine score every row (slower, but correct). For most RAG workloads, the `WHERE` filter is a feature — you want lexical pre-filtering to keep latency tractable on large corpora.

## Tuning the weights

The example uses a 50/50 weight as a starting point. To experiment:

```sql
-- 70% lexical, 30% semantic (technical-doc bias)
ORDER BY 0.7 * bm25_score(body, 'q') + 0.3 * (1.0 - vec_distance_cosine(embedding, [...])) DESC

-- 30% lexical, 70% semantic (paraphrase / RAG bias)
ORDER BY 0.3 * bm25_score(body, 'q') + 0.7 * (1.0 - vec_distance_cosine(embedding, [...])) DESC

-- Three-way: add a recency boost
ORDER BY 0.5 * bm25_score(body, 'q')
       + 0.4 * (1.0 - vec_distance_cosine(embedding, [...]))
       + 0.1 * (julianday('now') - julianday(created_at))
DESC
```

(The `julianday` function isn't in SQLRite yet — that's just a sketch of how the SQL composition extends naturally.)

## Production checklist

- **Normalize the BM25 score range.** BM25 is unbounded above; cosine distance is in `[0, 2]`. Without normalization, a high-IDF rare term can dominate the sum. The example skips normalization because the corpus is six docs and the BM25 scores are already ~`[0, 2]`. Real corpora need a min-max or z-score normalization step (computed offline or via a sliding window).
- **Use real embeddings.** Pre-baked toy vectors are for the example. In production you'll call an embedding model — `sqlrite-ask`'s LLM adapters can host one, or use the [`fastembed-rs`](https://crates.io/crates/fastembed) family for a local model.
- **Tune `k` end-to-end.** The `LIMIT k` clamps the result set. Hybrid retrieval typically needs to over-fetch (e.g. `LIMIT 50`) and re-rank with a cross-encoder for the final top 5 or 10.

## See also

- [`docs/phase-8-plan.md`](../../docs/phase-8-plan.md) — Q8 (arithmetic vs typed `hybrid_score(...)`) explains why SQLRite does this with raw arithmetic instead of a dedicated function.
- [`docs/phase-7-plan.md`](../../docs/phase-7-plan.md) — vector indexing context.
