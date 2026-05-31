import type { Metadata } from "next";
import Link from "next/link";
import { Footer } from "@/components/footer";
import { Nav } from "@/components/nav";
import { SITE } from "@/lib/site";
import PlaygroundLoader from "./PlaygroundLoader";

const TITLE = "SQL Playground";
const DESCRIPTION =
  "Run SQLRite — an embedded SQL + vector database in Rust — entirely in your browser. The full engine compiled to WebAssembly: SQL editor, sample datasets, HNSW vector search, CSV export. No install, no server.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/playground" },
  openGraph: {
    type: "website",
    siteName: "SQLRite",
    locale: "en_US",
    url: `${SITE.url}/playground`,
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

// WebApplication structured data — tells search engines this route is an
// interactive tool, not just an article.
const webAppJsonLd = {
  "@context": "https://schema.org",
  "@type": "WebApplication",
  name: "SQLRite SQL Playground",
  description: DESCRIPTION,
  url: `${SITE.url}/playground`,
  applicationCategory: "DeveloperApplication",
  operatingSystem: "Any (WebAssembly)",
  browserRequirements: "Requires WebAssembly and ES modules.",
  isAccessibleForFree: true,
  offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
  author: {
    "@type": "Person",
    name: "Joao Henrique Machado Silva",
    url: SITE.socials.github,
  },
};

export default function PlaygroundPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(webAppJsonLd) }}
      />
      <Nav variant="docs" />
      <section id="playground" className="no-border">
        <div className="wrap">
          <div className="sec-head">
            <span className="eyebrow tag">playground · wasm</span>
            <div>
              <h1>Run SQLRite in your browser.</h1>
              <p className="sub">
                The complete SQLRite engine — B-tree storage, transactions,
                JOINs, aggregates, BM25 full-text, and HNSW vector search —
                compiled to WebAssembly and running entirely in this tab. No
                server, no account, nothing to install. Your data never leaves
                the browser. Try a sample dataset, or write your own SQL and
                hit <kbd>⌘</kbd>/<kbd>Ctrl</kbd>+<kbd>Enter</kbd>.
              </p>
            </div>
          </div>

          <div style={{ marginTop: 32 }}>
            <PlaygroundLoader />
          </div>

          <div className="pg-notes">
            <h2>How it works</h2>
            <p>
              The engine is the same Rust crate published as{" "}
              <a href={SITE.cratesIo} target="_blank" rel="noreferrer">
                <code>sqlrite-engine</code>
              </a>
              , built for <code>wasm32-unknown-unknown</code> with{" "}
              <a href={SITE.npmWasm} target="_blank" rel="noreferrer">
                <code>@joaoh82/sqlrite-wasm</code>
              </a>
              . The WASM module is ~750&nbsp;KB gzipped and fetched once on
              first load. Because the browser build is in-memory only, the
              playground persists your <em>SQL session</em> (the statements you
              run) to{" "}
              <abbr title="Origin Private File System">OPFS</abbr> — replaying
              it on reload to rebuild the same database. Use{" "}
              <strong>Download .sql</strong> to take that script with you.
            </p>

            <h2>Known limitations</h2>
            <ul>
              <li>
                <strong>In-memory engine.</strong> The WASM build has no
                file-backed mode, so there is no binary <code>.sqlrite</code>{" "}
                export yet — persistence is via SQL-script replay. Binary
                round-trip is a tracked follow-up.
              </li>
              <li>
                <strong>OPFS support varies.</strong> Where OPFS write access
                isn&apos;t available (some private-browsing modes, older
                Safari), the playground falls back to{" "}
                <code>localStorage</code>, and if that&apos;s blocked too, the
                session simply isn&apos;t persisted across reloads.
              </li>
              <li>
                <strong>No <code>ask</code> (natural-language → SQL).</strong>{" "}
                That feature needs a server-side API key, which a static
                browser page can&apos;t hold. It ships in the REPL, MCP server,
                and desktop app instead.
              </li>
              <li>
                Aggregates over JOIN results and a few other surfaces
                aren&apos;t implemented in the engine yet — see{" "}
                <a
                  href={`${SITE.repo}/blob/main/docs/supported-sql.md`}
                  target="_blank"
                  rel="noreferrer"
                >
                  the supported-SQL reference
                </a>
                .
              </li>
            </ul>

            <p style={{ color: "var(--color-fg-mute)" }}>
              Want the full architecture write-up? See the{" "}
              <a
                href={`${SITE.repo}/tree/main/examples/wasm-playground`}
                target="_blank"
                rel="noreferrer"
              >
                playground README
              </a>{" "}
              and the other <Link href="/examples">example apps</Link>.
            </p>
          </div>
        </div>
      </section>
      <Footer />
    </>
  );
}
