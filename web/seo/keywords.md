# SQLRite keyword sheet

Last updated: 2026-05-12 (SQLR-33 — initial keyword research + on-page SEO pass).

## How to read this

Three buckets — head terms (high volume, hard), mid-tail (the sweet spot
SQLRite can realistically rank for), and long-tail / informational
queries (drive blog topics). For each entry:

- **Target page** — where on the site the term should land.
- **Primary keyword** — the dominant query the page is optimized for.
- **Secondary keywords** — supporting phrases woven into headings, body,
  and metadata.
- **Intent** — `informational` (someone learning), `commercial` (someone
  evaluating a tool), `navigational` (someone looking for SQLRite by
  name).
- **Priority** — `P0` (rewrite now), `P1` (next pass), `P2` (later /
  blog backlog).

Relative ordering matters more than absolute volume — we have no SEO
budget, just SERP inspection and a hunch about what an embedded-database
shopper actually types. Free tools that worked during the first pass:
Google's "people also ask" / autocomplete, the SERP for the literal
phrase, and Ahrefs' free keyword generator.

## Conventions

- Canonical host: `https://sqlritedb.com`. Never link to a different
  origin in canonical-eligible copy.
- All metadata `description` fields should clear ~150 chars; ~160 max
  before Google truncates.
- Don't promise features that don't exist. No "distributed SQLRite",
  no "replication", no "production-grade" until the roadmap says so.
- Voice is technical, slightly playful, no marketing buzzwords. "Built
  to teach what databases actually do" stays — that's the brand.

## Head terms — high volume, low realistic CTR for now

| Primary | Secondary | Intent | Priority | Target page | Notes |
| --- | --- | --- | --- | --- | --- |
| embedded database | embedded SQL, single-file database, embedded SQLite | commercial | P0 | `/` | We won't win this — but the landing page should still cleanly contain the phrase in H1 + first 160 chars. |
| SQLite alternative | SQLite-compatible, modern SQLite, SQLite in Rust | commercial | P0 | `/` and benchmarks section | Comparison framing on the benchmarks section earns a sliver of long-tail SQLite-vs traffic. |
| Rust database | embedded Rust database, Rust SQL crate | commercial | P0 | `/` | Mentioned in H1 + features intro. |
| vector database | embedded vector DB, vector search database | commercial | P1 | `/` (vector section anchor) | The market is owned by hosted services; SQLRite competes on "embedded + SQL + vector in one file". |

## Mid-tail — the sweet spot we can actually rank for

| Primary | Secondary | Intent | Priority | Target page | Suggested H1 / H2 | Meta description draft |
| --- | --- | --- | --- | --- | --- | --- |
| embedded database in Rust | embedded SQL engine Rust, single-file DB Rust | commercial | P0 | `/` | "SQLRite — an embedded SQL + vector database in Rust" (H1) | "SQLRite is an embedded SQL + vector database in Rust. SQLite-style single-file format, WAL transactions, HNSW vector search, BM25 full-text, six SDKs." |
| SQLite alternative for Rust | Rust SQLite alternative, SQLite-compatible Rust crate | commercial | P0 | `/` (hero + features) | Feature section retains "13 features" framing; copy mentions "SQLite-compatible API". | Same as landing — composition handled in lede + first feature copy. |
| embedded vector search Rust | vector search in Rust, HNSW Rust embedded | commercial | P0 | `/docs#vector` and `/` vector feature | "Built-in vector search with HNSW" (H3 in features) | "Add HNSW vector search to your Rust app with a single CREATE INDEX. Cosine / dot / L2 distance, sub-linear k-NN, no external service." |
| embedded database with HNSW | HNSW SQLite, vector index embedded DB | informational | P1 | `/docs#vector` | `/docs` H2 ("Vector search") stays as-is. | Captured by /docs metadata. |
| sqlite-compatible Rust crate | drop-in SQLite Rust, sqlite parser Rust | navigational | P1 | `/` features section | "Single-file format" / "Supported SQL" tags carry the term. | n/a |
| Rust embedded SQL engine | Rust SQL parser engine, embedded SQL crate | commercial | P1 | `/` | Features intro. | n/a |
| embedded SQL + vector database | embedded SQL vector DB, vector + SQL one file | commercial | P0 | `/` | New H1 leads with this. | Landing description. |
| Rust embedded database with WAL | WAL Rust crate, embedded database WAL | informational | P1 | `/docs#persistence` | `/docs` "Persistence & the WAL" section. | Captured by /docs metadata; long-tail blog candidate. |

## Long-tail / informational — easy wins + blog backlog

| Primary | Intent | Priority | Target page | Notes |
| --- | --- | --- | --- | --- |
| how to add vector search to SQLite | informational | P1 | future blog post | Open as blog backlog ticket. Working title: "Adding HNSW vector search to a SQLite-style engine". |
| SQLite vs SQLRite benchmarks | informational / commercial | P0 | benchmarks section + `/blog/sqlrite-vs-sqlite-benchmarks` | Benchmarks H2 now leads with "SQLRite vs SQLite benchmarks". Blog post already exists. |
| embedded database for desktop Tauri apps | commercial | P1 | `/docs#desktop` + future blog post | Existing docs section is short; expand later. Blog candidate: "Shipping a Tauri 2 app with an embedded SQL database." |
| natural language to SQL Rust crate | informational | P1 | `/docs` (.ask section, currently terse) + blog | Open ticket: "Doc the `.ask` REPL + `ConnectionAskExt::ask` API" + dedicated blog. |
| MCP server for SQLite | informational | P0 | `/docs#mcp` | Docs section stays. Worth a dedicated blog post: "Wiring a SQLite-style engine into Claude Code via MCP". |
| rust embedded database with WAL | informational | P1 | `/docs#persistence` | Already covered; long-tail traffic. |
| how to do hybrid retrieval in Rust | informational | P2 | future blog post (vector + BM25) | We already have `examples/hybrid-retrieval`. Blog candidate. |
| BM25 full-text search Rust | informational | P1 | `/docs#fts` | Section title already mentions BM25; meta copy captures it. |
| single-file SQL database | informational | P1 | `/` | Features H3 stays. |
| WASM SQL database in browser | informational | P2 | `/docs#sdk-wasm` + future blog | Blog candidate: "Running a SQL engine in a browser tab with WASM". |

## Per-page primary + secondary registry

This is the authoritative cross-reference for the on-page rewrites
landed in SQLR-33. Per the acceptance criteria, every P0 page records
its primary + secondary keyword here.

### `/` (landing)

- **Primary:** embedded SQL + vector database in Rust
- **Secondary:** embedded database, SQLite alternative, Rust database,
  HNSW vector search, BM25 full-text search, MCP server
- **H1:** "SQLRite — an embedded SQL + vector database in Rust."
- **Meta description (≤ 160 chars):** "SQLRite is an embedded SQL +
  vector database in Rust. SQLite-style single-file format, WAL
  transactions, HNSW vector search, BM25 full-text, six SDKs."

### `/docs`

- **Primary:** SQLRite documentation / getting started with the
  embedded Rust database
- **Secondary:** embedded database tutorial Rust, vector search Rust
  quickstart, BM25 Rust, MCP server SQLite
- **H1:** "SQLRite docs — getting started with the embedded Rust
  database."
- **Meta description:** "Install SQLRite, open your first .sqlrite
  file, and run SQL — transactions, JOINs, HNSW vector search, BM25
  full-text, and six language SDKs."

### Benchmarks section (`#benchmarks` on `/`)

There is no dedicated `/benchmarks` route — benchmarks live as a
section on the landing page. The H2 + sub-copy carries the
comparison query.

- **Primary:** SQLRite vs SQLite benchmarks
- **Secondary:** Rust embedded database benchmark, SQLite alternative
  performance
- **H2:** "SQLRite vs SQLite benchmarks — honest numbers, published in
  public."
- **Body sentence carries:** "Twelve workloads against SQLite
  (WAL+NORMAL) and DuckDB …"

### `/blog`

- **Primary:** building an embedded database in Rust
- **Secondary:** Rust database blog, SQLite-style engine notes
- Existing H2 + meta cover this; no rewrite this pass.

## Internal-linking notes (SQLR-33 sweep)

- Audit confirmed no `click here` / `read more` / `here.` anchors in
  `web/src`. Anchors are descriptive (`Read the docs`, `Star on
  GitHub`, `All posts`, post-title pager links).
- `/docs` is a single-page guide with anchored sections rather than
  many child pages, so the "every doc page links to ≥ 2 sibling
  pages" rule is met via the global `<Footer />` (links to `/blog`,
  `/blog/rss.xml`, GitHub) plus the in-page docs CTA card. The CTA
  card now exposes an explicit `/blog` link as a second descriptive
  in-site sibling (besides `/`).
- Landing hero's primary CTA continues to point at `/docs`. Feature
  cards covering Vector / FTS / MCP / SDKs already act as the "top
  docs entry points" via the rest of the landing IA (`Architecture`,
  `Roadmap`, `SDKShowcase`).

## Re-crawl plan

After every rewrite pass, re-run a free crawler (Screaming Frog up to
500 URLs is plenty) against `https://sqlritedb.com` and verify:

- **No duplicate `<title>`** across `/`, `/docs`, `/blog`, blog
  posts, blog tag pages.
- **No duplicate meta description** across the same set.
- **No duplicate H1** per page.
- **No orphan pages** — every URL in `/sitemap.xml` is reachable
  from at least one other URL.

`src/app/sitemap.ts` is the canonical URL list; keep it in sync when a
new route ships.

## Open backlog (split out of SQLR-33)

- Blog post: "Adding HNSW vector search to a SQLite-style engine".
- Blog post: "Wiring a SQLite-style engine into Claude Code via MCP".
- Blog post: "Shipping a Tauri 2 app with an embedded SQL database".
- Blog post: "Running a SQL engine in a browser tab with WASM".
- Docs expansion: dedicate a `.ask` / natural-language-to-SQL section
  in `/docs` once the API stabilizes.
