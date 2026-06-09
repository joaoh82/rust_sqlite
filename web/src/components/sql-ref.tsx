type Row = { stmt: string; feats: string[] };

const SQL_REF: Row[] = [
  {
    stmt: "CREATE TABLE",
    feats: [
      "PRIMARY KEY",
      "UNIQUE",
      "NOT NULL",
      "DEFAULT <literal>",
      "INTEGER",
      "TEXT",
      "REAL",
      "BOOLEAN",
      "VECTOR(N)",
      "JSON",
      "auto-index",
    ],
  },
  {
    stmt: "ALTER TABLE",
    feats: [
      "RENAME TO",
      "RENAME COLUMN",
      "ADD COLUMN [+ DEFAULT backfill]",
      "DROP COLUMN",
      "IF EXISTS",
    ],
  },
  {
    stmt: "DROP TABLE / DROP INDEX",
    feats: [
      "IF EXISTS",
      "auto-indexes refused",
      "freelist reclaim",
      "auto-VACUUM",
    ],
  },
  {
    stmt: "CREATE [UNIQUE] INDEX",
    feats: [
      "IF NOT EXISTS",
      "B-tree (default)",
      "USING HNSW WITH (metric = …)",
      "USING FTS",
      "INTEGER + TEXT + VECTOR",
    ],
  },
  {
    stmt: "INSERT INTO",
    feats: [
      "explicit column list",
      "auto-ROWID",
      "multi-row VALUES",
      "UNIQUE enforcement",
      "JSON validation",
      "vector dim check",
      "DEFAULT padding",
    ],
  },
  {
    stmt: "SELECT",
    feats: [
      "projection",
      "WHERE",
      "ORDER BY ASC|DESC",
      "LIMIT n",
      "DISTINCT",
      "GROUP BY",
      "= literal → index probe",
      "vec_distance_* / k-NN (HNSW)",
      "fts_match / bm25_score",
    ],
  },
  {
    stmt: "JOINs",
    feats: [
      "INNER JOIN",
      "LEFT OUTER",
      "RIGHT OUTER",
      "FULL OUTER",
      "ON <expr>",
      "aliases",
      "self-joins",
      "multi-join chains",
    ],
  },
  {
    stmt: "Predicates",
    feats: [
      "= <> < <= > >=",
      "IS NULL / IS NOT NULL",
      "LIKE / NOT LIKE / ILIKE",
      "IN (literal-list)",
      "AND / OR / NOT",
      "arithmetic + ||",
    ],
  },
  {
    stmt: "Aggregates",
    feats: [
      "COUNT(*)",
      "COUNT(DISTINCT col)",
      "SUM",
      "AVG",
      "MIN",
      "MAX",
      "GROUP BY <col>",
      "HAVING",
    ],
  },
  {
    stmt: "UPDATE",
    feats: [
      "multi-column SET",
      "WHERE",
      "arithmetic in SET",
      "UNIQUE + type checks",
      "FTS / HNSW index maintenance",
    ],
  },
  { stmt: "DELETE", feats: ["WHERE", "full-table", "freelist reclaim"] },
  {
    stmt: "BEGIN / COMMIT / ROLLBACK",
    feats: [
      "snapshot transactions",
      "WAL-backed commit",
      "auto-rollback on disk error",
    ],
  },
  {
    stmt: "VACUUM",
    feats: [
      "manual compaction",
      "auto-VACUUM (25% freelist)",
      "tunable threshold",
      "SQL via PRAGMA",
    ],
  },
  {
    stmt: "PRAGMA",
    feats: [
      "auto_vacuum (read/write)",
      "extensible dispatcher",
      "typed errors on bad values",
    ],
  },
  {
    stmt: "Prepared statements",
    feats: [
      "? placeholders",
      "execute_with_params",
      "query_with_params",
      "per-conn LRU plan cache",
      "Value::Vector binding",
    ],
  },
  {
    stmt: "Functions",
    feats: [
      "json_extract / json_type",
      "json_array_length / json_object_keys",
      "vec_distance_l2 / cosine / dot",
      "fts_match",
      "bm25_score",
    ],
  },
];

const NOT_YET = [
  "subqueries",
  "CTEs (WITH)",
  "HAVING without GROUP BY",
  "CASE WHEN",
  "BETWEEN",
  "GLOB / REGEXP",
  "OFFSET",
  "multi-column ORDER BY",
  "UNION / INTERSECT / EXCEPT",
  "INSERT … SELECT",
  "GROUP BY / DISTINCT over JOINs",
  "CREATE VIEW / TRIGGER",
  "FOREIGN KEY / CHECK",
  "savepoints",
  "named placeholders (:foo, $1)",
];

export function SQLRef() {
  return (
    <section id="sql">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">06 · sql surface</span>
          <div>
            <h2>What SQLRite speaks today.</h2>
            <p className="sub">
              The supported SQL is real — every feature lands with type checks,
              UNIQUE enforcement, and a clean error path instead of a panic.
              JOINs, aggregates, and prepared statements all came in the v0.2.0
              → v0.9.1 wave.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <table className="sql-table">
            <thead>
              <tr>
                <th>Statement</th>
                <th>Features</th>
              </tr>
            </thead>
            <tbody>
              {SQL_REF.map((r) => (
                <tr key={r.stmt}>
                  <td>{r.stmt}</td>
                  <td>
                    {r.feats.map((f) => (
                      <span className="pill" key={f}>
                        {f}
                      </span>
                    ))}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <div
            style={{
              marginTop: 36,
              padding: "20px 22px",
              border: "1px dashed var(--color-line)",
              borderRadius: 8,
              background: "var(--color-bg-card)",
            }}
          >
            <div className="eyebrow" style={{ marginBottom: 10 }}>
              not yet supported
            </div>
            <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
              {NOT_YET.map((s) => (
                <span
                  className="pill mono"
                  key={s}
                  style={{ fontSize: 11 }}
                >
                  {s}
                </span>
              ))}
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
