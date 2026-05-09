type Feature = {
  id: string;
  title: string;
  body: string;
  tags: string[];
};

const FEATURES: Feature[] = [
  {
    id: "01",
    title: "Single-file format",
    body:
      "One .sqlrite file per database. 4 KiB pages. Magic header, format version, schema-root pointer. Currently file format v5.",
    tags: ["pager", "v5"],
  },
  {
    id: "02",
    title: "On-disk B-Tree",
    body:
      "Real cell-based pages with a slot directory. Interior + leaf nodes. Overflow chains for oversized rows.",
    tags: ["btree", "indexes"],
  },
  {
    id: "03",
    title: "Write-Ahead Log",
    body:
      "32-byte header, 4112-byte frames, rolling-sum checksums. Torn-write recovery and atomic commits.",
    tags: ["wal", "fsync"],
  },
  {
    id: "04",
    title: "Real transactions",
    body:
      "BEGIN / COMMIT / ROLLBACK with snapshot isolation. Auto-rollback if the commit's disk write fails.",
    tags: ["acid"],
  },
  {
    id: "05",
    title: "JOINs + aggregates",
    body:
      "Four JOIN flavors with explicit ON. GROUP BY + COUNT/SUM/AVG/MIN/MAX, DISTINCT, LIKE, IN, IS NULL.",
    tags: ["joins", "group by"],
  },
  {
    id: "06",
    title: "Prepared statements",
    body:
      "? placeholders bind anywhere a literal is allowed — including vector arguments to k-NN. Per-connection LRU plan cache.",
    tags: ["params", "plan cache"],
  },
  {
    id: "07",
    title: "Vector search · HNSW",
    body:
      "VECTOR(N) column type with cosine / dot / L2 distance. HNSW index per metric for sub-linear k-NN.",
    tags: ["ann", "rag"],
  },
  {
    id: "08",
    title: "Full-text search · BM25",
    body:
      "FTS5-style inverted index with BM25 scoring. fts_match() / bm25_score() functions, hybrid retrieval ready.",
    tags: ["fts", "hybrid"],
  },
  {
    id: "09",
    title: "Free-list + auto-VACUUM",
    body:
      "DROP TABLE / DROP INDEX / DROP COLUMN release pages onto a free-list. Auto-VACUUM compacts past 25%, tunable via PRAGMA.",
    tags: ["storage", "pragma"],
  },
  {
    id: "10",
    title: "Six language SDKs",
    body:
      "Rust crate, Python (PyO3), Node.js (napi-rs), Go (database/sql), C FFI, and WASM for the browser.",
    tags: ["bindings"],
  },
  {
    id: "11",
    title: "Tauri desktop GUI · MCP server",
    body:
      "Cross-platform Svelte 5 + Tauri 2.0 client. sqlrite-mcp exposes the database as an MCP stdio server.",
    tags: ["gui", "agents"],
  },
  {
    id: "12",
    title: "Built to be read",
    body:
      "Every phase is shippable on its own and documented. The codebase is the textbook.",
    tags: ["pedagogy"],
  },
];

export function Features() {
  return (
    <section id="features">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">02 · features</span>
          <div>
            <h2>An honest database engine, all the way down.</h2>
            <p className="sub">
              No mocks, no shortcuts. SQLRite implements the parts of SQLite
              that matter — a paged file format, a B-tree, a WAL, locks, JOINs,
              aggregates — and extends them with the parts AI workloads need:
              vector search, full-text search, hybrid retrieval, and an MCP
              adapter.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="features">
            {FEATURES.map((f) => (
              <div className="feat" key={f.id}>
                <div className="feat-id">
                  {f.id} / {String(FEATURES.length).padStart(2, "0")}
                </div>
                <h3>{f.title}</h3>
                <p>{f.body}</p>
                <div className="tags">
                  {f.tags.map((t) => (
                    <span key={t}>{t}</span>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
