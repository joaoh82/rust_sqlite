import Link from "next/link";
import type { Metadata } from "next";
import { Footer } from "@/components/footer";
import { Nav } from "@/components/nav";
import { SITE } from "@/lib/site";

// SEO targeting: primary "SQLRite documentation / getting started with the
// embedded Rust database"; secondary "embedded database tutorial Rust",
// "vector search Rust quickstart", "BM25 Rust", "MCP server SQLite".
// See web/seo/keywords.md.
const TITLE =
  "SQLRite docs — getting started with the embedded Rust database";
const DESCRIPTION =
  "Install SQLRite, open your first .sqlrite file, and run SQL — transactions, JOINs, HNSW vector search, BM25 full-text, an MCP server, and six language SDKs.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/docs" },
  openGraph: {
    type: "article",
    siteName: "SQLRite",
    locale: "en_US",
    url: `${SITE.url}/docs`,
    title: TITLE,
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: SITE.twitterHandle,
    creator: SITE.twitterHandle,
    title: TITLE,
    description: DESCRIPTION,
  },
};

const breadcrumbJsonLd = {
  "@context": "https://schema.org",
  "@type": "BreadcrumbList",
  itemListElement: [
    {
      "@type": "ListItem",
      position: 1,
      name: "Home",
      item: SITE.url,
    },
    {
      "@type": "ListItem",
      position: 2,
      name: "Docs",
      item: `${SITE.url}/docs`,
    },
  ],
};

export default function DocsPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{
          __html: JSON.stringify(breadcrumbJsonLd),
        }}
      />
      <Nav variant="docs" />
      <details className="docs-mobile-menu">
        <summary>
          <span>On this page</span>
        </summary>
        <div className="docs-mobile-menu-panel">
          <div className="section-label">Getting started</div>
          <a href="#install">Install</a>
          <a href="#first-db">Your first database</a>
          <a href="#repl">Using the REPL</a>
          <a href="#persistence">Persistence &amp; the WAL</a>
          <a href="#transactions">Transactions</a>
          <a href="#joins">JOINs</a>
          <a href="#aggregates">GROUP BY &amp; aggregates</a>
          <a href="#alter-drop">ALTER / DROP / VACUUM</a>
          <a href="#prepared">Prepared statements</a>
          <a href="#pragma">PRAGMA</a>
          <a href="#vector">Vector search</a>
          <a href="#fts">Full-text search</a>
          <a href="#desktop">Desktop app</a>
          <a href="#mcp">MCP server</a>
          <div className="section-label">Embedding</div>
          <a href="#sdk-rust">Rust crate</a>
          <a href="#sdk-python">Python</a>
          <a href="#sdk-node">Node.js</a>
          <a href="#sdk-go">Go</a>
          <a href="#sdk-c">C FFI</a>
          <a href="#sdk-wasm">WASM</a>
          <div className="section-label">Reference</div>
          <a href="#supported">Supported SQL</a>
          <a href="#errors">Errors &amp; limits</a>
          <a href="#contributing">Contributing</a>
        </div>
      </details>
      <div className="docs-shell">
        <aside className="docs-side">
          <div className="section-label">Getting started</div>
          <a href="#install">Install</a>
          <a href="#first-db">Your first database</a>
          <a href="#repl">Using the REPL</a>
          <a href="#persistence">Persistence &amp; the WAL</a>
          <a href="#transactions">Transactions</a>
          <a href="#joins">JOINs</a>
          <a href="#aggregates">GROUP BY &amp; aggregates</a>
          <a href="#alter-drop">ALTER / DROP / VACUUM</a>
          <a href="#prepared">Prepared statements</a>
          <a href="#pragma">PRAGMA</a>
          <a href="#vector">Vector search</a>
          <a href="#fts">Full-text search</a>
          <a href="#desktop">Desktop app</a>
          <a href="#mcp">MCP server</a>
          <div className="section-label">Embedding</div>
          <a href="#sdk-rust">Rust crate</a>
          <a href="#sdk-python">Python</a>
          <a href="#sdk-node">Node.js</a>
          <a href="#sdk-go">Go</a>
          <a href="#sdk-c">C FFI</a>
          <a href="#sdk-wasm">WASM</a>
          <div className="section-label">Reference</div>
          <a href="#supported">Supported SQL</a>
          <a href="#errors">Errors &amp; limits</a>
          <a href="#contributing">Contributing</a>
        </aside>

        <main className="docs-main">
          <span className="eyebrow">docs · getting started</span>
          <h1 style={{ marginTop: 18 }}>
            SQLRite docs — getting started with the embedded Rust database
          </h1>
          <p className="lede">
            SQLRite is an embedded SQL + vector database in Rust. This page is
            a ten-minute tour from <code>cargo install</code> to a persistent
            on-disk <code>.sqlrite</code> file — transactions, JOINs, HNSW
            vector search, BM25 full-text, and the MCP server. Skip ahead to{" "}
            <a href="#vector">vector search</a>,{" "}
            <a href="#fts">full-text search</a>, the{" "}
            <a href="#mcp">MCP server</a>, or pick the SDK that fits your
            language at the <a href="#sdk-rust">bottom</a> — they all wrap the
            same engine.
          </p>

          <h2 id="install">Install</h2>
          <p>
            SQLRite ships as a CLI binary, a Rust library, an MCP stdio server,
            and five language SDKs. Pick whichever matches your project:
          </p>
          <pre>
            <span className="cmt"># CLI / REPL — drop into a SQL prompt</span>
            {"\n"}
            <span className="prompt">$</span> cargo install sqlrite-engine
            {"\n\n"}
            <span className="cmt"># MCP stdio server</span>
            {"\n"}
            <span className="prompt">$</span> cargo install sqlrite-mcp
            {"\n\n"}
            <span className="cmt"># Rust library — imported as `use sqlrite::…`</span>
            {"\n"}
            <span className="prompt">$</span> cargo add sqlrite-engine
            {"\n\n"}
            <span className="cmt"># Python · Node · Go</span>
            {"\n"}
            <span className="prompt">$</span> pip install sqlrite{"\n"}
            <span className="prompt">$</span> npm install @joaoh82/sqlrite{"\n"}
            <span className="prompt">$</span> go get
            github.com/joaoh82/rust_sqlite/sdk/go
          </pre>

          <div className="callout">
            <strong>Prebuilt installers</strong> for the desktop GUI (macOS
            .dmg, Windows .msi, Linux AppImage / .deb / .rpm) are attached to
            every release on GitHub. The header exposes <code>New…</code> /{" "}
            <code>Open…</code> / <code>Save As…</code> buttons; installers are
            unsigned until Phase 6.1 — see the README for first-launch steps.
          </div>

          <h2 id="first-db">Your first database</h2>
          <p>
            Create a file-backed database and run some SQL. Everything below
            works against an in-memory or on-disk database — the only
            difference is whether you pass a path.
          </p>
          <pre>
            <span className="prompt">$</span> sqlrite{"\n\n"}
            SQLRite — {SITE.version}
            {"\n"}
            <span className="cmt">
              Connected to a transient in-memory database.
            </span>
            {"\n"}
            <span className="cmt">
              Use &apos;.open FILENAME&apos; to reopen on a persistent database.
            </span>
            {"\n"}
            sqlrite&gt; <span className="kw">CREATE TABLE</span> users (
            {"\n"}
            {"   ...> "}id <span className="kw">INTEGER PRIMARY KEY</span>,
            {"\n"}
            {"   ...> "}name <span className="kw">TEXT NOT NULL</span>{" "}
            <span className="kw">UNIQUE</span>,{"\n"}
            {"   ...> "}age <span className="kw">INTEGER</span>
            {"\n"}
            {"   ...> "});{"\n"}
            sqlrite&gt; <span className="kw">INSERT INTO</span> users (name,
            age) <span className="kw">VALUES</span> (
            <span className="str">&apos;alice&apos;</span>,{" "}
            <span className="num">30</span>);{"\n"}
            sqlrite&gt; <span className="kw">SELECT</span> *{" "}
            <span className="kw">FROM</span> users;{"\n"}
            +----+-------+-----+{"\n"}| id | name &nbsp;| age |{"\n"}
            +----+-------+-----+{"\n"}| 1 &nbsp;| alice | 30 &nbsp;|{"\n"}
            +----+-------+-----+
          </pre>

          <h2 id="repl">Using the REPL</h2>
          <p>
            The REPL is built on rustyline and supports history, syntax
            highlighting, bracket matching, and multi-line input. Useful meta
            commands:
          </p>
          <ul>
            <li>
              <code>.help</code> — list every meta command
            </li>
            <li>
              <code>.open app.sqlrite</code> — open or create a file-backed
              database; auto-save flips on from this point
            </li>
            <li>
              <code>.save app.sqlrite</code> — explicit flush (rarely needed
              once <code>.open</code> is in play)
            </li>
            <li>
              <code>.tables</code> — list every table in the current database
            </li>
            <li>
              <code>.ask</code> — natural-language → SQL via the configured LLM
              backend (requires <code>SQLRITE_LLM_API_KEY</code>)
            </li>
            <li>
              <code>.exit</code> — leave the prompt
            </li>
          </ul>
          <p>
            Pass <code>--readonly</code> to open the database under a shared
            lock — multiple read-only sessions can coexist on the same file.
          </p>

          <h2 id="persistence">Persistence &amp; the WAL</h2>
          <p>
            SQLRite stores each database as one <code>.sqlrite</code> file plus
            a sidecar <code>&lt;db&gt;.sqlrite-wal</code>. Pages are 4 KiB; rows
            live in cell-based pages with a slot directory; oversized rows
            spill into an overflow chain.
          </p>
          <p>
            Commits append a frame per dirty page to the WAL plus a final
            commit frame carrying the new page-0 header. The main file stays
            frozen between checkpoints — auto-checkpointing fires past 100
            frames.
          </p>
          <p>
            <strong>Crash safety:</strong> torn or partial trailing WAL frames
            are silently truncated at the boundary; the decoded page-0 frame
            overrides any stale main-file header on reopen.
          </p>

          <h2 id="transactions">Transactions</h2>
          <p>
            SQLRite supports real <code>BEGIN</code> / <code>COMMIT</code> /{" "}
            <code>ROLLBACK</code> with snapshot isolation. Single level — no
            savepoints yet.
          </p>
          <pre>
            sqlrite&gt; <span className="kw">BEGIN</span>;{"\n"}
            sqlrite&gt; <span className="kw">UPDATE</span> users{" "}
            <span className="kw">SET</span> age = age +{" "}
            <span className="num">1</span> <span className="kw">WHERE</span>{" "}
            name = <span className="str">&apos;alice&apos;</span>;{"\n"}
            sqlrite&gt; <span className="kw">DELETE FROM</span> users{" "}
            <span className="kw">WHERE</span> age &lt;{" "}
            <span className="num">18</span>;{"\n"}
            sqlrite&gt; <span className="kw">ROLLBACK</span>;{" "}
            <span className="cmt">
              -- everything since BEGIN is discarded
            </span>
          </pre>

          <h2 id="joins">JOINs</h2>
          <p>
            All four SQL-standard JOIN flavors are supported with explicit{" "}
            <code>ON</code> conditions:{" "}
            <code>INNER JOIN</code>, <code>LEFT [OUTER] JOIN</code>,{" "}
            <code>RIGHT [OUTER] JOIN</code>, and{" "}
            <code>FULL [OUTER] JOIN</code>. Aliases work; multi-join chains
            left-fold; self-joins require an alias on at least one side.
          </p>
          <pre>
            <span className="kw">SELECT</span> c.name, o.total{"\n"}
            <span className="kw">FROM</span> customers <span className="kw">AS</span> c{"\n"}
            <span className="kw">LEFT OUTER JOIN</span> orders <span className="kw">AS</span> o{"\n"}
            {"  "}<span className="kw">ON</span> c.id = o.customer_id{"\n"}
            <span className="kw">WHERE</span> o.id <span className="kw">IS NULL</span>;{"  "}
            <span className="cmt">-- anti-join: customers with no orders</span>
          </pre>
          <p>
            The executor uses a plain nested-loop driver — adequate for an
            embedded learning database. Hash / merge joins on equi-join shapes
            are a future optimization.{" "}
            <code>ON</code>, <code>USING (col)</code>, <code>NATURAL</code>, and{" "}
            <code>CROSS JOIN</code> are all supported (a <code>USING</code> /{" "}
            <code>NATURAL</code> column shows once in <code>SELECT *</code>).
            Comma-separated FROMs (<code>FROM a, b</code>) are not — use an
            explicit <code>JOIN</code> / <code>CROSS JOIN</code>. Aggregates /{" "}
            <code>GROUP BY</code> over a join lands once subqueries do.
          </p>

          <h2 id="aggregates">GROUP BY &amp; aggregates</h2>
          <p>
            <code>COUNT(*)</code>, <code>COUNT(col)</code>,{" "}
            <code>COUNT(DISTINCT col)</code>, <code>SUM</code>, <code>AVG</code>,{" "}
            <code>MIN</code>, <code>MAX</code> with optional{" "}
            <code>GROUP BY</code> on bare column names. Integer{" "}
            <code>SUM</code> stays integer until a <code>REAL</code> arrives or{" "}
            <code>i64</code> overflows; <code>AVG</code> returns{" "}
            <code>REAL</code> (or <code>NULL</code> on empty groups);{" "}
            <code>MIN</code> / <code>MAX</code> skip NULLs. Empty-group results
            are <code>0</code> for counts and <code>NULL</code> for the rest.
          </p>
          <pre>
            <span className="kw">SELECT</span> dept,{" "}
            <span className="kw">COUNT</span>(*),{" "}
            <span className="kw">AVG</span>(salary){"\n"}
            <span className="kw">FROM</span> employees{"\n"}
            <span className="kw">WHERE</span> active = <span className="kw">TRUE</span>{"\n"}
            <span className="kw">GROUP BY</span> dept{"\n"}
            <span className="kw">ORDER BY</span> <span className="kw">COUNT</span>(*){" "}
            <span className="kw">DESC</span>;
          </pre>
          <p>
            <code>DISTINCT</code> applies after projection (and after
            aggregation, when both apply). <code>LIKE</code> /{" "}
            <code>NOT LIKE</code> / <code>ILIKE</code> use SQLite-style ASCII
            case folding.{" "}
            <code>IN (literal-list)</code> uses three-valued logic.{" "}
            <code>HAVING</code> isn&rsquo;t supported yet — wrap the aggregate
            in a subquery once subqueries land.
          </p>

          <h2 id="alter-drop">ALTER TABLE / DROP / VACUUM</h2>
          <p>
            Schema evolution is one operation per statement (SQLite parity):
          </p>
          <pre>
            <span className="kw">ALTER TABLE</span> users <span className="kw">RENAME TO</span> accounts;{"\n"}
            <span className="kw">ALTER TABLE</span> accounts <span className="kw">RENAME COLUMN</span> name <span className="kw">TO</span> display_name;{"\n"}
            <span className="kw">ALTER TABLE</span> accounts <span className="kw">ADD COLUMN</span> verified <span className="kw">BOOLEAN</span> <span className="kw">NOT NULL</span> <span className="kw">DEFAULT</span> <span className="kw">FALSE</span>;{"\n"}
            <span className="kw">ALTER TABLE</span> accounts <span className="kw">DROP COLUMN</span> legacy_field;{"\n\n"}
            <span className="kw">DROP TABLE</span> <span className="kw">IF EXISTS</span> stale_logs;{"\n"}
            <span className="kw">DROP INDEX</span> <span className="kw">IF EXISTS</span> idx_old_search;
          </pre>
          <p>
            Released pages go onto a persisted free-list — subsequent{" "}
            <code>CREATE TABLE</code> / <code>INSERT</code> reuses them
            instead of growing the file. Auto-VACUUM kicks in when the
            free-list crosses 25% of <code>page_count</code> (skipped on
            tiny / in-memory / read-only databases). Manual:
          </p>
          <pre><span className="kw">VACUUM</span>;</pre>

          <h2 id="prepared">Prepared statements</h2>
          <p>
            Every executable statement accepts <code>?</code> placeholders
            anywhere a value literal is allowed. The Rust API:
          </p>
          <pre>
            <span className="kw">use</span> sqlrite::{"{Connection, Value}"};{"\n\n"}
            <span className="kw">let mut</span> conn = Connection::open(<span className="str">&quot;app.sqlrite&quot;</span>)?;{"\n"}
            <span className="kw">let mut</span> ins = conn.prepare_cached({"\n"}
            {"    "}<span className="str">&quot;INSERT INTO users (name, age) VALUES (?, ?)&quot;</span>,{"\n"}
            )?;{"\n"}
            ins.execute_with_params(&amp;[Value::Text(<span className="str">&quot;alice&quot;</span>.into()), Value::Integer(<span className="num">30</span>)])?;{"\n"}
            ins.execute_with_params(&amp;[Value::Text(<span className="str">&quot;bob&quot;</span>.into()), Value::Integer(<span className="num">25</span>)])?;{"\n\n"}
            <span className="kw">let</span> stmt = conn.prepare_cached(<span className="str">&quot;SELECT name FROM users WHERE age &gt; ?&quot;</span>)?;{"\n"}
            <span className="kw">let</span> rows = stmt{"\n"}
            {"    "}.query_with_params(&amp;[Value::Integer(<span className="num">26</span>)])?{"\n"}
            {"    "}.collect_all()?;
          </pre>
          <p>
            <code>prepare_cached</code> keeps a per-connection LRU plan cache
            (default cap 16; tune via <code>set_prepared_cache_capacity</code>)
            so a hot SQL string parses exactly once across the
            connection&rsquo;s lifetime. <code>Value::Vector(Vec&lt;f32&gt;)</code>{" "}
            binds where a bracket-array literal would normally appear — so
            prepared k-NN queries still take the HNSW shortcut. Named
            placeholders (<code>:foo</code>, <code>$1</code>) aren&rsquo;t
            supported yet.
          </p>

          <h2 id="pragma">PRAGMA</h2>
          <p>
            <code>PRAGMA &lt;name&gt;;</code> reads, <code>PRAGMA &lt;name&gt; = &lt;value&gt;;</code>{" "}
            writes. The dispatcher is in place; the first wired pragma is{" "}
            <code>auto_vacuum</code>:
          </p>
          <pre>
            <span className="kw">PRAGMA</span> auto_vacuum;{"             "}<span className="cmt">-- read; renders a single-row result</span>{"\n"}
            <span className="kw">PRAGMA</span> auto_vacuum = <span className="num">0.5</span>;{"      "}<span className="cmt">-- arm the trigger at 50%</span>{"\n"}
            <span className="kw">PRAGMA</span> auto_vacuum = <span className="num">0</span>;{"        "}<span className="cmt">-- arm at 0% (compact on any released page)</span>{"\n"}
            <span className="kw">PRAGMA</span> auto_vacuum = <span className="kw">OFF</span>;{"      "}<span className="cmt">-- disable; equivalent: NONE, &apos;OFF&apos;, &apos;NONE&apos;</span>
          </pre>
          <p>
            Out-of-range values, <code>NaN</code>, ±∞, and unknown identifiers
            are rejected with typed errors — the trigger never silently
            saturates. The setting is per-<code>Connection</code> runtime
            state and isn&rsquo;t persisted in the file header. Other pragmas
            (<code>journal_mode</code>, <code>synchronous</code>,{" "}
            <code>cache_size</code>, <code>page_size</code>, …) will land as
            they earn their keep — adding a new pragma is a single arm in{" "}
            <code>execute_pragma</code>.
          </p>

          <h2 id="vector">Vector search</h2>
          <p>
            SQLRite supports a <code>VECTOR(N)</code> column type with cosine,
            dot-product, and L2 distance. Build an HNSW index for sub-linear
            k-NN queries.
          </p>
          <pre>
            <span className="kw">CREATE TABLE</span> docs (id{" "}
            <span className="kw">INTEGER PRIMARY KEY</span>, body{" "}
            <span className="kw">TEXT</span>, embedding{" "}
            <span className="kw">VECTOR</span>(
            <span className="num">384</span>));{"\n"}
            <span className="kw">CREATE INDEX</span> docs_emb{" "}
            <span className="kw">ON</span> docs(embedding){" "}
            <span className="kw">USING HNSW</span>;{"\n\n"}
            <span className="kw">SELECT</span> id,{" "}
            <span className="kw">vec_distance_cosine</span>
            (embedding, ?) <span className="kw">AS</span> dist{"\n"}
            <span className="kw">FROM</span> docs{"\n"}
            <span className="kw">ORDER BY</span> dist{" "}
            <span className="kw">ASC</span>{" "}
            <span className="kw">LIMIT</span>{" "}
            <span className="num">10</span>;
          </pre>

          <h2 id="fts">Full-text search</h2>
          <p>
            Phase 8 ships an FTS5-style inverted index with BM25 scoring.{" "}
            <code>fts_match()</code> filters and <code>bm25_score()</code>{" "}
            ranks; the optimizer recognizes the canonical pattern and probes
            the FTS index directly.
          </p>
          <pre>
            <span className="kw">CREATE INDEX</span> docs_body{" "}
            <span className="kw">ON</span> docs(body){" "}
            <span className="kw">USING FTS</span>;{"\n\n"}
            <span className="kw">SELECT</span> id, body,{" "}
            <span className="kw">bm25_score</span>
            (body, <span className="str">&apos;rust database&apos;</span>){" "}
            <span className="kw">AS</span> score{"\n"}
            <span className="kw">FROM</span> docs{"\n"}
            <span className="kw">WHERE</span>{" "}
            <span className="kw">fts_match</span>
            (body, <span className="str">&apos;rust database&apos;</span>)
            {"\n"}
            <span className="kw">ORDER BY</span> score{" "}
            <span className="kw">DESC</span>{" "}
            <span className="kw">LIMIT</span>{" "}
            <span className="num">10</span>;
          </pre>
          <p>
            Compose with vector distance for hybrid retrieval — see
            {" "}
            <a
              href={`${SITE.repo}/blob/main/examples/hybrid-retrieval`}
              style={{ color: "var(--color-accent)" }}
            >
              examples/hybrid-retrieval
            </a>
            .
          </p>

          <h2 id="desktop">Desktop app</h2>
          <p>
            The desktop client is a Svelte 5 + Tauri 2.0 GUI. Three-pane
            layout: header (file pickers), sidebar (tables + schema), and a
            query editor with line numbers, <code>⌘/</code> comment toggle,
            and selection-aware Run.
          </p>
          <p>
            Download a prebuilt installer from the{" "}
            <a
              href={SITE.releasesLatest}
              style={{ color: "var(--color-accent)" }}
            >
              latest release
            </a>
            , or run from source:
          </p>
          <pre>
            <span className="prompt">$</span> cd desktop{"\n"}
            <span className="prompt">$</span> npm install{"\n"}
            <span className="prompt">$</span> npm run tauri dev
          </pre>

          <h2 id="mcp">MCP server</h2>
          <p>
            <code>sqlrite-mcp</code> exposes a SQLRite database as a Model
            Context Protocol stdio server. Eight tools out of the box:{" "}
            <code>list_tables</code>, <code>describe_table</code>,{" "}
            <code>query</code>, <code>execute</code>, <code>schema_dump</code>,{" "}
            <code>vector_search</code>, <code>bm25_search</code>, and{" "}
            <code>ask</code>. Wire it into Claude Code, Cursor, or any MCP
            client.
          </p>
          <pre>
            <span className="prompt">$</span> sqlrite-mcp /path/to/app.sqlrite
            {"\n"}
            <span className="prompt">$</span> sqlrite-mcp --read-only
            /path/to/app.sqlrite
          </pre>

          <h2 id="sdk-rust">Rust crate</h2>
          <pre>
            <span className="kw">use</span> sqlrite::Connection;{"\n\n"}
            <span className="kw">fn</span> main() -&gt; sqlrite::Result&lt;()&gt;
            {" {"}
            {"\n"}
            {"    "}
            <span className="kw">let mut</span> conn = Connection::open(
            <span className="str">&quot;app.sqlrite&quot;</span>)?;{"\n"}
            {"    "}conn.execute(
            <span className="str">
              &quot;CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY,
              name TEXT)&quot;
            </span>
            )?;{"\n"}
            {"    "}conn.execute(
            <span className="str">
              &quot;INSERT INTO users (name) VALUES (&apos;alice&apos;)&quot;
            </span>
            )?;{"\n"}
            {"    "}
            <span className="kw">for</span> row{" "}
            <span className="kw">in</span> conn.query(
            <span className="str">
              &quot;SELECT id, name FROM users&quot;
            </span>
            )? {"{"}
            {"\n"}
            {"        "}
            <span className="kw">let</span> id: i64 = row.get(
            <span className="num">0</span>)?;{"\n"}
            {"        "}
            <span className="kw">let</span> name: String = row.get(
            <span className="num">1</span>)?;{"\n"}
            {"        "}println!(
            <span className="str">&quot;{`{id}: {name}`}&quot;</span>);{"\n"}
            {"    "}
            {"}"}
            {"\n"}
            {"    "}Ok(()){"\n"}
            {"}"}
          </pre>

          <h2 id="sdk-python">Python</h2>
          <pre>
            <span className="kw">import</span> sqlrite{"\n\n"}
            <span className="kw">with</span> sqlrite.connect(
            <span className="str">&quot;app.sqlrite&quot;</span>){" "}
            <span className="kw">as</span> conn:{"\n"}
            {"    "}cur = conn.cursor(){"\n"}
            {"    "}cur.execute(
            <span className="str">
              &quot;CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY,
              name TEXT)&quot;
            </span>
            ){"\n"}
            {"    "}cur.execute(
            <span className="str">
              &quot;INSERT INTO users (name) VALUES
              (&apos;alice&apos;)&quot;
            </span>
            ){"\n"}
            {"    "}
            <span className="kw">for</span> row{" "}
            <span className="kw">in</span> cur.execute(
            <span className="str">
              &quot;SELECT id, name FROM users&quot;
            </span>
            ).fetchall():{"\n"}
            {"        "}
            <span className="kw">print</span>(row)
          </pre>

          <h2 id="sdk-node">Node.js</h2>
          <pre>
            <span className="kw">import</span> {"{"} Database {"}"}{" "}
            <span className="kw">from</span>{" "}
            <span className="str">&quot;@joaoh82/sqlrite&quot;</span>;{"\n\n"}
            <span className="kw">const</span> db ={" "}
            <span className="kw">new</span> Database(
            <span className="str">&quot;app.sqlrite&quot;</span>);{"\n"}
            db.exec(
            <span className="str">
              {
                "`CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT)`"
              }
            </span>
            );{"\n"}
            db.prepare(
            <span className="str">
              &quot;INSERT INTO users (name) VALUES (?)&quot;
            </span>
            ).run(<span className="str">&quot;alice&quot;</span>);{"\n"}
            console.log(db.prepare(
            <span className="str">
              &quot;SELECT id, name FROM users&quot;
            </span>
            ).all());
          </pre>

          <h2 id="sdk-go">Go</h2>
          <pre>
            <span className="kw">import</span> ({"\n"}
            {"    "}
            <span className="str">&quot;database/sql&quot;</span>
            {"\n"}
            {"    "}_{" "}
            <span className="str">
              &quot;github.com/joaoh82/rust_sqlite/sdk/go&quot;
            </span>
            {"\n"}
            ){"\n\n"}
            db, _ := sql.Open(
            <span className="str">&quot;sqlrite&quot;</span>,{" "}
            <span className="str">&quot;app.sqlrite&quot;</span>);{"\n"}
            db.Exec(
            <span className="str">
              &quot;CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY,
              name TEXT)&quot;
            </span>
            );{"\n"}
            db.Exec(
            <span className="str">
              &quot;INSERT INTO users (name) VALUES (?)&quot;
            </span>
            ,{" "}
            <span className="str">&quot;alice&quot;</span>);{"\n"}
            rows, _ := db.Query(
            <span className="str">
              &quot;SELECT id, name FROM users&quot;
            </span>
            );
          </pre>

          <h2 id="sdk-c">C FFI</h2>
          <p>
            The C ABI is stable and ships with a cbindgen-generated{" "}
            <code>sqlrite.h</code>. Opaque pointer types, thread-local
            last-error, split <code>sqlrite_execute</code> (DDL/DML) vs{" "}
            <code>sqlrite_query</code> / <code>sqlrite_step</code> (SELECT
            iteration).
          </p>

          <h2 id="sdk-wasm">WASM</h2>
          <p>
            The engine compiles to a ~1.8 MB / 500 KB-gzipped WebAssembly
            module. Three <code>wasm-pack</code> targets (web, bundler,
            nodejs). The whole database can live in a browser tab.
          </p>
          <pre>
            <span className="kw">import</span> init, {"{"} Database {"}"}{" "}
            <span className="kw">from</span>{" "}
            <span className="str">&quot;@joaoh82/sqlrite-wasm&quot;</span>;
            {"\n\n"}
            <span className="kw">await</span> init();{"\n"}
            <span className="kw">const</span> db ={" "}
            <span className="kw">new</span> Database();{"\n"}
            db.exec(
            <span className="str">
              &quot;CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)&quot;
            </span>
            );
          </pre>

          <h2 id="supported">Supported SQL</h2>
          <p>
            The complete reference lives in <code>docs/supported-sql.md</code>{" "}
            in the repo. Quick summary:
          </p>
          <ul>
            <li>
              <strong>DDL:</strong> <code>CREATE TABLE</code> with{" "}
              <code>PRIMARY KEY</code> / <code>UNIQUE</code> /{" "}
              <code>NOT NULL</code> / <code>DEFAULT &lt;literal&gt;</code>;{" "}
              <code>CREATE [UNIQUE] INDEX</code> with{" "}
              <code>IF NOT EXISTS</code>, <code>USING HNSW</code>, and{" "}
              <code>USING FTS</code>; <code>ALTER TABLE</code> (RENAME TO /
              RENAME COLUMN / ADD COLUMN / DROP COLUMN); <code>DROP TABLE</code>{" "}
              and <code>DROP INDEX</code> with <code>IF EXISTS</code>;{" "}
              <code>VACUUM</code>
            </li>
            <li>
              <strong>DML:</strong> <code>INSERT</code> (multi-row VALUES),{" "}
              <code>SELECT</code> (projection / <code>DISTINCT</code> /{" "}
              <code>WHERE</code> / <code>GROUP BY</code> /{" "}
              <code>ORDER BY</code> / <code>LIMIT</code>),{" "}
              <code>UPDATE</code>, <code>DELETE</code>
            </li>
            <li>
              <strong>JOINs:</strong> <code>INNER</code>,{" "}
              <code>LEFT OUTER</code>, <code>RIGHT OUTER</code>,{" "}
              <code>FULL OUTER</code> with explicit <code>ON</code>
            </li>
            <li>
              <strong>Aggregates:</strong> <code>COUNT(*)</code>,{" "}
              <code>COUNT(DISTINCT col)</code>, <code>SUM</code>,{" "}
              <code>AVG</code>, <code>MIN</code>, <code>MAX</code>
            </li>
            <li>
              <strong>Predicates:</strong> comparisons,{" "}
              <code>AND / OR / NOT</code>, arithmetic, <code>||</code>,{" "}
              <code>IS NULL</code> / <code>IS NOT NULL</code>,{" "}
              <code>LIKE / NOT LIKE / ILIKE</code>,{" "}
              <code>IN (literal-list)</code> / <code>NOT IN</code>
            </li>
            <li>
              <strong>Transactions:</strong> <code>BEGIN</code> /{" "}
              <code>COMMIT</code> / <code>ROLLBACK</code> with snapshot
              isolation; auto-rollback on COMMIT disk failure
            </li>
            <li>
              <strong>Prepared statements:</strong> positional <code>?</code>{" "}
              binding via <code>prepare_cached</code> +{" "}
              <code>execute_with_params</code> /{" "}
              <code>query_with_params</code>; per-connection LRU plan cache
            </li>
            <li>
              <strong>Pragmas:</strong> <code>PRAGMA auto_vacuum</code>{" "}
              (read/write); extensible dispatcher
            </li>
            <li>
              <strong>Types:</strong> INTEGER, TEXT, REAL, BOOLEAN, NULL,{" "}
              <code>VECTOR(N)</code>, <code>JSON</code>
            </li>
            <li>
              <strong>Functions:</strong>{" "}
              <code>vec_distance_cosine / dot / l2</code>,{" "}
              <code>fts_match</code>, <code>bm25_score</code>,{" "}
              <code>json_extract</code>, <code>json_type</code>,{" "}
              <code>json_array_length</code>, <code>json_object_keys</code>
            </li>
          </ul>

          <h2 id="errors">Errors &amp; limits</h2>
          <p>
            Every malformed input path returns a typed{" "}
            <code>SQLRiteError</code> instead of panicking. Common error
            categories:
          </p>
          <ul>
            <li>
              <strong>Parse</strong> — bad SQL syntax, with column hints from{" "}
              <code>sqlparser</code>
            </li>
            <li>
              <strong>Schema</strong> — duplicate columns, missing tables,
              unknown identifiers
            </li>
            <li>
              <strong>Type</strong> — <code>&apos;foo&apos;</code> being
              inserted into an <code>INTEGER</code> column
            </li>
            <li>
              <strong>Constraint</strong> — UNIQUE / PRIMARY KEY violations,
              NOT NULL with no default
            </li>
            <li>
              <strong>I/O</strong> — file already locked, WAL truncation, disk
              full mid-commit
            </li>
          </ul>
          <div className="callout">
            <strong>Single-writer rule.</strong> Multiple read-only openers
            coexist; any writer excludes all readers (POSIX flock semantics —
            readers OR a writer, never both at once).
          </div>

          <h2 id="contributing">Contributing</h2>
          <p>
            SQLRite welcomes pull requests. For larger changes open an issue
            first. The codebase is documented phase-by-phase in{" "}
            <code>docs/</code> — start at <code>docs/_index.md</code>.
          </p>
          <ul>
            <li>
              Build &amp; test: <code>cargo test</code>
            </li>
            <li>
              Lint: <code>cargo fmt &amp;&amp; cargo clippy</code>
            </li>
            <li>
              Run the example: <code>cargo run --example quickstart</code>
            </li>
          </ul>

          <div className="docs-cta">
            <a className="btn btn-primary" href={SITE.repo}>
              View on GitHub
            </a>
            <a className="btn" href={SITE.discord}>
              Join the Discord
            </a>
            <Link className="btn" href="/blog">
              Read the SQLRite blog
            </Link>
            <Link className="btn" href="/">
              ← Back to the SQLRite home page
            </Link>
          </div>
        </main>

        <aside className="toc" aria-label="On this page">
          <h2 className="toc-title">On this page</h2>
          <a href="#install">Install</a>
          <a href="#first-db">Your first database</a>
          <a href="#repl">Using the REPL</a>
          <a href="#persistence">Persistence &amp; WAL</a>
          <a href="#transactions">Transactions</a>
          <a href="#joins">JOINs</a>
          <a href="#aggregates">Aggregates</a>
          <a href="#alter-drop">ALTER / DROP / VACUUM</a>
          <a href="#prepared">Prepared statements</a>
          <a href="#pragma">PRAGMA</a>
          <a href="#vector">Vector search</a>
          <a href="#fts">Full-text search</a>
          <a href="#desktop">Desktop app</a>
          <a href="#mcp">MCP server</a>
          <a href="#sdk-rust">Rust</a>
          <a href="#sdk-python">Python</a>
          <a href="#sdk-node">Node.js</a>
          <a href="#sdk-go">Go</a>
          <a href="#sdk-c">C FFI</a>
          <a href="#sdk-wasm">WASM</a>
          <a href="#supported">Supported SQL</a>
          <a href="#errors">Errors &amp; limits</a>
          <a href="#contributing">Contributing</a>
        </aside>
      </div>
      <Footer />
    </>
  );
}
