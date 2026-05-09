/**
 * Benchmark data extracted from the canonical pinned-host run:
 *   benchmarks/results/2026-05-08-apple-ac84d560.json
 *
 * Numbers here mirror the headline table in `docs/benchmarks.md`.
 * When a new pinned-host run lands:
 *   1. Update RUN_META below (date, commit, source path).
 *   2. Update median_ns / label per affected bar.
 *   3. Sanity-check the per-group narrative (slowest stays slowest, etc.).
 *
 * Times are stored in nanoseconds so per-group normalization is a single
 * scalar division. The label string is what's shown in the UI — pre-formatted
 * with the right unit because µs / ms / s mixing inside one group is the
 * common case.
 */

export type Bar = {
  driver: string;
  median_ns: number;
  label: string;
  /** Mark the row as the SQLRite "win" — gets the accent treatment. */
  highlight?: boolean;
  /** When non-null, bar bg uses this oklch hue from the design tokens. */
  tone?: "good" | "info" | "warn" | "muted";
};

export type Series = {
  workload: string;
  /** Optional inline note below the workload label. */
  note?: string;
  bars: Bar[];
};

export type ChartGroup = {
  title: string;
  subtitle: string;
  series: Series[];
};

export const RUN_META = {
  date: "2026-05-08",
  host: "Apple M1 Pro · macOS · aarch64",
  commit: "ac84d560",
  sourcePath:
    "https://github.com/joaoh82/rust_sqlite/blob/main/benchmarks/results/2026-05-08-apple-ac84d560.json",
  docPath:
    "https://github.com/joaoh82/rust_sqlite/blob/main/docs/benchmarks.md",
} as const;

/** Three big numbers above the charts. */
export const HEADLINE_STATS = [
  {
    metric: "~50×",
    label: "HNSW vs brute-force k-NN",
    detail: "10k × 384-dim vectors, cosine top-10 — 120.88 ms → 2.40 ms.",
  },
  {
    metric: "1.6–1.9×",
    label: "Read-path gap vs SQLite",
    detail:
      "Within ~2× of SQLite (WAL+NORMAL) on W1 read-by-PK and W6 index lookup after SQLR-23.",
  },
  {
    metric: "608 µs",
    label: "Hybrid retrieval at 1k docs",
    detail: "0.5 × bm25_score + 0.5 × (1 − vec_distance_cosine).",
  },
] as const;

export const CHART_GROUPS: ChartGroup[] = [
  {
    title: "Vector top-10 · the HNSW win",
    subtitle:
      "Cosine distance, 10k × 384-dim corpus. Same engine, same data — only the index probe changes. Lower is better.",
    series: [
      {
        workload: "W10 · cosine top-10 · 10k × 384-dim",
        note: "~50× faster — HNSW probe vs brute-force scan",
        bars: [
          {
            driver: "Brute-force scan",
            median_ns: 120_880_000,
            label: "120.88 ms",
            tone: "muted",
          },
          {
            driver: "HNSW (M=16, ef_search=50)",
            median_ns: 2_400_000,
            label: "2.40 ms",
            highlight: true,
          },
        ],
      },
    ],
  },
  {
    title: "Read paths · OLTP baseline",
    subtitle:
      "After SQLR-23 (prepared statements + ? bindings) closed the parser tax, SQLRite tracks SQLite within ~2× on hot read paths. Lower is better.",
    series: [
      {
        workload: "W1 · read-by-PK · 10k probes",
        note: "1.9× — was 4.8× pre-SQLR-23",
        bars: [
          {
            driver: "SQLite (WAL+NORMAL)",
            median_ns: 2_091,
            label: "2.09 µs",
            tone: "info",
          },
          {
            driver: "SQLRite",
            median_ns: 3_923,
            label: "3.92 µs",
            highlight: true,
          },
        ],
      },
      {
        workload: "W6 · secondary index · 10k probes",
        note: "1.6× — was 4.2× pre-SQLR-23",
        bars: [
          {
            driver: "SQLite (WAL+NORMAL)",
            median_ns: 2_560,
            label: "2.56 µs",
            tone: "info",
          },
          {
            driver: "SQLRite",
            median_ns: 4_040,
            label: "4.04 µs",
            highlight: true,
          },
        ],
      },
    ],
  },
  {
    title: "Full-text · BM25 + hybrid retrieval",
    subtitle:
      "1k-doc tech-blurb corpus. SQLite FTS5 is the lexical comparator; the hybrid query (BM25 + cosine) is SQLRite-only. Lower is better.",
    series: [
      {
        workload: "W11 · BM25 top-10",
        note: "21× behind SQLite FTS5 — was 43× pre-SQLR-23",
        bars: [
          {
            driver: "SQLite FTS5",
            median_ns: 23_650,
            label: "23.65 µs",
            tone: "info",
          },
          {
            driver: "SQLRite",
            median_ns: 501_630,
            label: "501.63 µs",
            highlight: true,
          },
        ],
      },
      {
        workload: "W12 · Hybrid (BM25 + cosine fusion)",
        note: "RAG-shaped query, no SQL-engine comparator",
        bars: [
          {
            driver: "SQLRite",
            median_ns: 607_900,
            label: "607.90 µs",
            highlight: true,
          },
        ],
      },
    ],
  },
  {
    title: "Analytical aggregates · DuckDB's home turf",
    subtitle:
      "Columnar engines win on big aggregations. We publish the gap honestly — SQLRite isn't competing on this axis, but the suite proves the differentiator workloads still deliver. Lower is better.",
    series: [
      {
        workload: "W7 · SUM(v) · 1M rows",
        bars: [
          {
            driver: "DuckDB",
            median_ns: 478_780,
            label: "478.78 µs",
            tone: "good",
          },
          {
            driver: "SQLite (WAL+NORMAL)",
            median_ns: 31_570_000,
            label: "31.57 ms",
            tone: "info",
          },
          {
            driver: "SQLRite",
            median_ns: 103_620_000,
            label: "103.62 ms",
            highlight: true,
          },
        ],
      },
      {
        workload: "W8 · GROUP BY · cardinality 10",
        bars: [
          {
            driver: "DuckDB",
            median_ns: 949_750,
            label: "949.75 µs",
            tone: "good",
          },
          {
            driver: "SQLite (WAL+NORMAL)",
            median_ns: 366_520_000,
            label: "366.52 ms",
            tone: "info",
          },
          {
            driver: "SQLRite",
            median_ns: 197_320_000,
            label: "197.32 ms",
            highlight: true,
          },
        ],
      },
    ],
  },
];

/** Engineering debts the bench surfaced — published honestly. */
export const DEBT_NOTES: ReadonlyArray<{
  ticket: string;
  workload: string;
  symptom: string;
  cause: string;
}> = [
  {
    ticket: "SQLR-18",
    workload: "W4 single-row INSERT",
    symptom: "~579× vs SQLite",
    cause: "Bottom-up B-tree rebuild on every COMMIT.",
  },
  {
    ticket: "SQLR-19",
    workload: "W8 GROUP BY · card-100k",
    symptom: "Skipped by default — ~245 s/iter",
    cause: "Vec-backed group store; should be HashMap.",
  },
  {
    ticket: "SQLR-20",
    workload: "W9 INNER JOIN · 10k×10k",
    symptom: "~14M× vs SQLite",
    cause:
      "Nested-loop driver doesn't push ON predicate to the inner-side index.",
  },
  {
    ticket: "SQLR-21",
    workload: "W11 / W12 corpus cap",
    symptom: "FTS doc-lengths sidecar capped at ~1,360 docs",
    cause: "Phase 8.1 — overflow chaining for posting + sidecar cells.",
  },
];
