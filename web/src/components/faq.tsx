import { SITE } from "@/lib/site";

// Landing-page FAQ. Real <h3> question / <p> answer pairs mirrored 1:1 into
// FAQPage JSON-LD — that pairing is what drives Google featured snippets /
// AI Overviews, so keep the visible copy and the JSON-LD in sync (both are
// generated from the same FAQS array). Honest answers only: no features
// the roadmap hasn't shipped (see web/seo/keywords.md conventions).

type Faq = { id: string; question: string; answer: string };

const FAQS: Faq[] = [
  {
    id: "what-is-sqlrite",
    question: "What is SQLRite?",
    answer:
      "SQLRite is an embedded SQL + vector database written from scratch in Rust. Like SQLite, it stores a whole database in a single file and runs inside your process — no server. On top of the classic engine (B-tree storage, WAL, transactions, JOINs, aggregates) it adds the parts AI workloads need: HNSW vector search, BM25 full-text search, JSON columns, and a built-in MCP server.",
  },
  {
    id: "sqlrite-vs-sqlite",
    question: "How is SQLRite different from SQLite?",
    answer:
      "SQLite is a battle-tested C library; SQLRite is an independent Rust engine with a SQLite-style design, built to be read and extended. SQLRite ships vector search (VECTOR columns + HNSW indexes), BM25 full-text search, MVCC concurrent writes via BEGIN CONCURRENT, natural-language-to-SQL, and an MCP server as first-class features instead of extensions. SQLite remains the right default for maximum maturity; SQLRite targets embedded AI / retrieval workloads and learning how databases work.",
  },
  {
    id: "vector-search",
    question: "Does SQLRite support vector search?",
    answer:
      "Yes. Declare a VECTOR(N) column, build an HNSW index with CREATE INDEX … USING HNSW, and query with vec_distance_cosine / dot / L2 in an ORDER BY … LIMIT k. The optimizer recognizes the k-NN pattern and probes the HNSW index for sub-linear search, and you can combine the distance with bm25_score() for hybrid retrieval.",
  },
  {
    id: "full-text-search",
    question: "Does SQLRite support full-text search?",
    answer:
      "Yes. CREATE INDEX … USING FTS builds an FTS5-style inverted index; fts_match() filters and bm25_score() ranks with BM25. The index persists in the same single .sqlrite file as your tables.",
  },
  {
    id: "languages",
    question: "What languages can I use SQLRite from?",
    answer:
      "Six surfaces wrap the same engine: a native Rust crate (sqlrite-engine), Python (PyO3), Node.js (napi-rs), Go (database/sql driver), a C FFI with a generated sqlrite.h header, and a WebAssembly build for the browser. There's also a CLI REPL and a Tauri desktop GUI.",
  },
  {
    id: "browser-wasm",
    question: "Can SQLRite run in the browser?",
    answer:
      "Yes — the engine compiles to a ~500 KB-gzipped WebAssembly module with web, bundler, and nodejs targets. The playground on this site runs the full engine in your tab, including HNSW vector search, with OPFS persistence.",
  },
  {
    id: "mcp-server",
    question: "What is the SQLRite MCP server?",
    answer:
      "sqlrite-mcp exposes any .sqlrite database to AI agents over the Model Context Protocol (stdio). It ships eight tools — list_tables, describe_table, query, execute, schema_dump, vector_search, bm25_search, and ask — and a --read-only mode that opens the file under a shared lock and hides execute.",
  },
  {
    id: "production-ready",
    question: "Is SQLRite production-ready?",
    answer:
      "SQLRite is a young, well-tested engine built in public — honest about what it is. It has WAL crash safety, typed errors, snapshot-isolated transactions, and a benchmark suite against SQLite and DuckDB, but it hasn't had SQLite's decades of hardening. Use it for embedded AI prototypes, tools, and learning; reach for SQLite when you need maximum maturity.",
  },
];

const faqJsonLd = {
  "@context": "https://schema.org",
  "@type": "FAQPage",
  mainEntity: FAQS.map((f) => ({
    "@type": "Question",
    name: f.question,
    acceptedAnswer: { "@type": "Answer", text: f.answer },
  })),
};

export function FAQ() {
  return (
    <section id="faq">
      <script
        type="application/ld+json"
        // Stringified from the static FAQS array above — not user input.
        dangerouslySetInnerHTML={{ __html: JSON.stringify(faqJsonLd) }}
      />
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">10 · faq</span>
          <div>
            <h2>Frequently asked questions.</h2>
            <p className="sub">
              The short answers. The long ones live in the{" "}
              <a href={`${SITE.url}/docs`}>docs</a> and the{" "}
              <a href={SITE.repo}>repository</a>.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="faq-list">
            {FAQS.map((f) => (
              <div className="faq-item" key={f.id} id={f.id}>
                <h3>{f.question}</h3>
                <p>{f.answer}</p>
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
