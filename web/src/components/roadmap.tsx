type Status = "done" | "active" | "next";
type Phase = {
  num: string;
  title: string;
  status: Status;
  desc: string;
  bullets: string[];
};

const PHASES: Phase[] = [
  {
    num: "Phase 0",
    title: "Modernization",
    status: "done",
    desc:
      "Rust 2024 edition, resolver 3, every dependency on current majors.",
    bullets: [
      "rustyline 18 · clap 4 · sqlparser 0.61",
      "thiserror 2 · env_logger 0.11",
    ],
  },
  {
    num: "Phase 1",
    title: "SQL execution surface",
    status: "done",
    desc:
      "CREATE / INSERT / SELECT / UPDATE / DELETE with WHERE, ORDER BY, LIMIT.",
    bullets: [
      "Expressions: comparisons, AND/OR/NOT, arithmetic, ||",
      "Auto-ROWID, UNIQUE enforcement, type checks",
      "REPL with history, syntax highlighting, bracket matching",
    ],
  },
  {
    num: "Phase 2",
    title: "On-disk persistence",
    status: "done",
    desc:
      "Single-file .sqlrite format with a 4 KiB page layout and crash-safe header writes.",
    bullets: [
      "Typed payload pages chained via next-pointers",
      ".open / .save / .tables meta commands",
    ],
  },
  {
    num: "Phase 3",
    title: "On-disk B-Tree + auto-save pager",
    status: "done",
    desc:
      "Cell-based pages, interior + leaf nodes, overflow chains, secondary indexes.",
    bullets: [
      "Auto-save on every committing statement",
      "sqlrite_master is itself a real cell-based table",
      "Auto-indexes for PK + UNIQUE; CREATE [UNIQUE] INDEX",
    ],
  },
  {
    num: "Phase 2.5",
    title: "Tauri 2.0 desktop app",
    status: "done",
    desc:
      "Cross-platform GUI in Svelte 5; engine extracted into a reusable library.",
    bullets: [
      "Engine became Send + Sync (Arc<Mutex<_>>)",
      "Workspace: root + desktop/src-tauri",
      "Three-pane dark UI with sticky-header result grid",
    ],
  },
  {
    num: "Phase 4",
    title: "Durability and concurrency",
    status: "done",
    desc:
      "WAL, checkpointer, exclusive/shared locks, and real BEGIN/COMMIT/ROLLBACK.",
    bullets: [
      "4a–4e: file lock, WAL codec, WAL-aware pager, checkpointer, multi-reader/single-writer",
      "4f: BEGIN / COMMIT / ROLLBACK with snapshot isolation",
      "Torn-write recovery via rolling-sum checksum frames",
      "Auto-checkpoint past 100 frames, idempotent",
    ],
  },
  {
    num: "Phase 5",
    title: "Embedding surface — six SDKs",
    status: "done",
    desc:
      "Public Connection / Statement / Rows API plus Rust, Python, Node, Go, C FFI, and WASM bindings.",
    bullets: [
      "5a: Rust public API (param binding shipped in 9g)",
      "5b–5e: C FFI · Python (PyO3) · Node (napi-rs) · Go (database/sql)",
      "5g: WASM — ~1.8 MB / ~500 KB gzipped, browser-native",
    ],
  },
  {
    num: "Phase 6",
    title: "Release engineering + CI/CD",
    status: "done",
    desc:
      "Lockstep versioning across eleven manifests; OIDC trusted publishing across PyPI, npm, crates.io.",
    bullets: [
      "6a: bump-version.sh — one dispatch, eleven manifests",
      "6b: parallel CI across three OSes",
      "6c–6i: trusted publishers + desktop installers for 7 platform/format combos",
    ],
  },
  {
    num: "Phase 7",
    title: "AI-era extensions",
    status: "done",
    desc:
      "Vector / embedding column type, HNSW ANN index, JSON column type, ask() (natural-language → SQL), MCP server.",
    bullets: [
      "7a–7d: VECTOR(N) + cosine/dot/L2 + HNSW + persistence",
      "7e: JSON column type + json_extract / json_type / json_array_length",
      "7g: ask() across REPL, desktop, Python, Node, Go, WASM, MCP",
      "7h: sqlrite-mcp — JSON-RPC over stdio, eight tools",
    ],
  },
  {
    num: "Phase 8",
    title: "Full-text search + hybrid retrieval",
    status: "done",
    desc:
      "FTS5-style inverted index, BM25 scoring, hybrid (lexical + semantic) retrieval. Bumps file format v4 → v5.",
    bullets: [
      "8a: tokenizer + BM25 + posting-list (pure algorithms)",
      "8b: fts_match() / bm25_score() + try_fts_probe optimizer hook",
      "8c: cell-encoded posting persistence + on-demand format bump",
      "8d–8e: hybrid retrieval worked example + bm25_search MCP tool",
    ],
  },
  {
    num: "Phase 9",
    title: "SQL surface + DX follow-ups (v0.2.0 → v0.9.1)",
    status: "done",
    desc:
      "After v0.2.0 closed the file-format bump, the next nine sub-phases shipped the SQL surface that had been parked under \"possible extras\" — JOINs, aggregates, prepared statements, PRAGMA, and the storage hygiene around them.",
    bullets: [
      "9a (v0.3.0): DEFAULT clause + DROP TABLE/INDEX + ALTER TABLE",
      "9b–9c (v0.4.0–v0.5.0): free-list, manual VACUUM, auto-VACUUM",
      "9d (v0.5.1): IS NULL / IS NOT NULL + Option<Value> INSERT pipeline",
      "9e (v0.6.0): GROUP BY + COUNT/SUM/AVG/MIN/MAX + DISTINCT + LIKE + IN",
      "9f (v0.7.0): JOINs — INNER, LEFT, RIGHT, FULL OUTER",
      "9g (v0.9.0): prepared statements + ? param binding + plan cache",
      "9h (v0.9.0): HNSW probe widened to cosine + dot via WITH (metric = …)",
      "9i (v0.9.1): PRAGMA dispatcher + auto_vacuum knob",
    ],
  },
  {
    num: "Phase 10",
    title: "Benchmarks vs SQLite + DuckDB",
    status: "done",
    desc:
      "Twelve-workload bench harness (SQLR-4 / SQLR-16) with a pluggable Driver trait. Pinned-host runs published.",
    bullets: [
      "Read-by-PK · transactional CRUD · analytical slices · vector + FTS retrieval",
      "Bundled SQLite + DuckDB drivers; criterion-based",
      "Excluded from CI — `make bench` runs locally",
    ],
  },
  {
    num: "What's next",
    title: "Possible extras",
    status: "next",
    desc:
      "Smaller, well-scoped follow-ups that slot in where they make sense — see the canonical roadmap doc for the full list.",
    bullets: [
      "Subqueries + CTEs · HAVING · CASE WHEN · BETWEEN · GLOB / REGEXP",
      "GROUP BY / DISTINCT over JOINs · multi-column ORDER BY · OFFSET · UNION",
      "Composite + expression indexes · CREATE VIEW / TRIGGER · FOREIGN KEY / CHECK",
      "Concurrent writes via MVCC + BEGIN CONCURRENT (design sketch in repo)",
      "Savepoints · more pragmas (journal_mode, synchronous, cache_size)",
      "Code signing for desktop installers (Phase 6.1)",
    ],
  },
];

const STATUS_LABEL: Record<Status, string> = {
  done: "shipped",
  active: "in progress",
  next: "planned",
};

export function Roadmap() {
  return (
    <section id="roadmap">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">04 · roadmap</span>
          <div>
            <h2>Phased, shippable, public.</h2>
            <p className="sub">
              Every phase is independently usable and merges to{" "}
              <span className="mono">main</span> before the next starts. Ten
              phases shipped through v0.9.1; the remaining list at the bottom
              is small, well-scoped follow-ups.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 40 }}>
          <div className="timeline">
            {PHASES.map((p) => (
              <div className={`phase ${p.status}`} key={p.num}>
                <div className="phase-head">
                  <span className="phase-num">{p.num}</span>
                  <span className="phase-title">{p.title}</span>
                  <span className="phase-status">
                    {STATUS_LABEL[p.status]}
                  </span>
                </div>
                <p className="phase-desc">{p.desc}</p>
                {p.bullets.length > 0 && (
                  <ul className="phase-bullets">
                    {p.bullets.map((b, i) => (
                      <li key={i}>
                        <span>{b}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
