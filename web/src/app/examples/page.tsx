import Link from "next/link";
import type { Metadata } from "next";
import { Footer } from "@/components/footer";
import { Nav } from "@/components/nav";
import { SITE } from "@/lib/site";

const TITLE = "Examples";
const DESCRIPTION =
  "End-to-end example apps built on SQLRite — the embedded SQL + vector database in Rust. Real-world shapes: AI agents, RAG knowledge bases, local-first desktop apps, browser-only SQL playgrounds, and edge collectors.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/examples" },
  openGraph: {
    type: "website",
    siteName: "SQLRite",
    locale: "en_US",
    url: `${SITE.url}/examples`,
    title: `${TITLE} · SQLRite`,
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: SITE.twitterHandle,
    creator: SITE.twitterHandle,
    title: `${TITLE} · SQLRite`,
    description: DESCRIPTION,
  },
};

const itemListJsonLd = {
  "@context": "https://schema.org",
  "@type": "ItemList",
  name: "SQLRite Examples",
  description: DESCRIPTION,
  url: `${SITE.url}/examples`,
  itemListElement: [
    {
      "@type": "ListItem",
      position: 1,
      item: {
        "@type": "SoftwareSourceCode",
        name: "Python LLM agent with persistent memory",
        url: `${SITE.repo}/tree/main/examples/python-agent`,
        programmingLanguage: "Python",
        description:
          "A CLI chat agent whose long-term memory is a single .sqlrite file. Vector recall via HNSW, lexical recall via BM25, and a structured facts table for deterministic retrieval.",
      },
    },
    {
      "@type": "ListItem",
      position: 2,
      item: {
        "@type": "SoftwareSourceCode",
        name: "Chat with your notes — Node.js + Claude Desktop MCP",
        url: `${SITE.repo}/tree/main/examples/nodejs-notes`,
        programmingLanguage: "JavaScript",
        description:
          "A Node.js CLI that ingests a folder of markdown notes into SQLRite (HNSW + BM25 indexes), then exposes the database to Claude Desktop via sqlrite-mcp --read-only. Hybrid retrieval over your notes from inside the chat client.",
      },
    },
  ],
};

type Example = {
  status: "shipped" | "planned";
  title: string;
  blurb: string;
  bullets: string[];
  language: string;
  repoPath: string;
  features: string[];
};

const EXAMPLES: Example[] = [
  {
    status: "shipped",
    title: "Python LLM agent with long-term memory",
    blurb:
      "A CLI chat agent whose entire long-term memory is one local .sqlrite file. Embeds each turn, hybrid-searches messages + summaries + a structured facts table on every recall, and persists across process restarts. No Postgres, no Redis, no Pinecone — just one file.",
    bullets: [
      "Vector KNN over past turns via HNSW, plus BM25 keyword recall via fts_match / bm25_score",
      "Heuristic fact extraction into a (subject, predicate, object) table — surfaced via plain SQL",
      "Zero-config first-run with a hash embedder + offline echo agent; swap in OpenAI / sentence-transformers / Anthropic via CLI flags",
      "31 offline tests; runs end-to-end without an API key",
    ],
    language: "Python 3.11+",
    repoPath: "examples/python-agent",
    features: ["HNSW", "VECTOR(384)", "BM25 / FTS", "PyO3 SDK"],
  },
  {
    status: "shipped",
    title: "Chat with your notes — Claude Desktop + MCP",
    blurb:
      "A Node.js CLI that ingests a folder of markdown notes into a SQLRite database, then exposes it to Claude Desktop (or any MCP client) via sqlrite-mcp --read-only. Claude calls bm25_search / vector_search / query directly against your local notes — no cloud sync, no custom RAG pipeline.",
    bullets: [
      "Markdown → frontmatter-aware chunker → hash or OpenAI embedder → SQLRite documents + chunks tables",
      "Hybrid retrieval fuses BM25 and vector cosine in a single SQL ORDER BY (see docs/fts.md)",
      "`sqlrite-notes serve` wraps sqlrite-mcp so the Claude Desktop config snippet is one block of JSON",
      "Default embedder is fully offline (zero-dep hash bag-of-words); flip to text-embedding-3-small with OPENAI_API_KEY",
      "40 unit + integration tests; works against the prebuilt @joaoh82/sqlrite npm binaries",
    ],
    language: "Node.js 20+",
    repoPath: "examples/nodejs-notes",
    features: ["HNSW", "BM25 / FTS", "MCP server", "napi-rs SDK"],
  },
];

const pillStyle: React.CSSProperties = {
  fontSize: 11,
  padding: "2px 8px",
  border: "1px solid var(--color-line)",
  borderRadius: 999,
  color: "var(--color-fg-mute)",
  fontFamily:
    "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
  whiteSpace: "nowrap",
};

const cardStyle: React.CSSProperties = {
  border: "1px solid var(--color-line)",
  borderLeft: "2px solid var(--color-accent)",
  borderRadius: 8,
  padding: "28px 28px 24px 28px",
  background: "var(--color-bg-card)",
  display: "flex",
  flexDirection: "column",
  gap: 16,
};

export default function ExamplesIndexPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(itemListJsonLd) }}
      />
      <Nav variant="docs" />
      <section id="examples-index" className="no-border">
        <div className="wrap">
          <div className="sec-head">
            <span className="eyebrow tag">examples · sqlrite</span>
            <div>
              <h2>Apps built on SQLRite.</h2>
              <p className="sub">
                Longer, opinionated examples that exercise SQLRite
                end-to-end in real-world shapes — not just a SELECT
                tutorial. Each app pairs a runnable repo with a README
                walking through the architecture and the SQLRite
                features it leans on. Tracked under the{" "}
                <a
                  href={`${SITE.repo}`}
                  style={{ color: "var(--color-accent)" }}
                >
                  example-apps umbrella
                </a>{" "}
                — more landing as they ship.
              </p>
            </div>
          </div>

          <ul
            style={{
              listStyle: "none",
              padding: 0,
              margin: "48px 0 0 0",
              display: "grid",
              gap: 24,
            }}
          >
            {EXAMPLES.map((ex) => (
              <li key={ex.title} style={cardStyle}>
                <div
                  style={{
                    display: "flex",
                    flexWrap: "wrap",
                    gap: 12,
                    alignItems: "baseline",
                    justifyContent: "space-between",
                  }}
                >
                  <h3 style={{ margin: 0, fontSize: 22 }}>{ex.title}</h3>
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                    <span style={pillStyle}>
                      {ex.status === "shipped" ? "shipped" : "planned"}
                    </span>
                    <span style={pillStyle}>{ex.language}</span>
                  </div>
                </div>
                <p style={{ margin: 0, color: "var(--color-fg-mute)" }}>
                  {ex.blurb}
                </p>
                <ul
                  style={{
                    margin: 0,
                    paddingLeft: 20,
                    color: "var(--color-fg-mute)",
                  }}
                >
                  {ex.bullets.map((b) => (
                    <li key={b} style={{ marginBottom: 6 }}>
                      {b}
                    </li>
                  ))}
                </ul>
                <div
                  style={{
                    display: "flex",
                    flexWrap: "wrap",
                    gap: 6,
                    marginTop: 4,
                  }}
                >
                  {ex.features.map((f) => (
                    <span key={f} style={pillStyle}>
                      {f}
                    </span>
                  ))}
                </div>
                <div
                  style={{
                    display: "flex",
                    gap: 16,
                    flexWrap: "wrap",
                    marginTop: 8,
                  }}
                >
                  <a
                    className="btn"
                    href={`${SITE.repo}/tree/main/${ex.repoPath}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    View on GitHub →
                  </a>
                  <a
                    href={`${SITE.repo}/blob/main/${ex.repoPath}/README.md`}
                    target="_blank"
                    rel="noreferrer"
                    style={{
                      color: "var(--color-accent)",
                      alignSelf: "center",
                      fontSize: 14,
                    }}
                  >
                    Read the README →
                  </a>
                </div>
              </li>
            ))}
          </ul>

          <p
            style={{
              marginTop: 48,
              color: "var(--color-fg-mute)",
              fontSize: 14,
            }}
          >
            More examples in flight: a Tauri + Svelte journaling
            desktop app, a browser SQL playground (WASM), and a Go
            edge/IoT event collector. See{" "}
            <Link href="/docs">/docs</Link> for the engine reference.
          </p>
        </div>
      </section>
      <Footer />
    </>
  );
}
