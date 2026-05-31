import Link from "next/link";

// Homepage "try it" card. Sits directly under the hero (above the fold) so
// the highest-leverage call to action — run SQLRite without installing
// anything — is the first thing a visitor can click. SQLR-42.
export function PlaygroundCard() {
  return (
    <section className="pg-home no-border" aria-label="Browser playground">
      <div className="wrap">
        <Link href="/playground" className="pg-home-card">
          <div className="pg-home-copy">
            <span className="eyebrow tag">try it · zero install</span>
            <h2>Run SQLRite in your browser.</h2>
            <p>
              The full engine compiled to WebAssembly — SQL editor, sample
              datasets, and HNSW vector search, all running in your tab. No
              server, no signup, nothing to install.
            </p>
            <span className="pg-home-link">
              Open the playground <span aria-hidden="true">→</span>
            </span>
          </div>
          <pre className="pg-home-snippet" aria-hidden="true">
            <code>{`SELECT title, genre, year
FROM movies
ORDER BY vec_distance_cosine(
  embedding, [0.85, 0.05, 0.40, 0.05]
)
LIMIT 5;`}</code>
          </pre>
        </Link>
      </div>
    </section>
  );
}
