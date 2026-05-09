import {
  CHART_GROUPS,
  DEBT_NOTES,
  HEADLINE_STATS,
  RUN_META,
  type Bar,
  type ChartGroup,
} from "@/lib/benchmarks";

/**
 * Render a single workload row: the workload label on the left, then one
 * horizontal bar per driver. Bar widths normalize to the *slowest* bar in the
 * row (clamped at a small min so a tiny driver isn't invisible) — so shorter
 * = faster, which lines up with the "lower is better" framing in the suite.
 */
function WorkloadBars({ series }: { series: ChartGroup["series"][number] }) {
  // Per-row max for normalization. Cross-row comparison would need a global
  // max, but mixing µs / ms / s in one bar would compress the small numbers
  // to nothing — per-row keeps each comparison legible.
  const maxNs = Math.max(...series.bars.map((b) => b.median_ns));

  return (
    <div className="bench-row">
      <div className="bench-row-head">
        <span className="bench-row-name">{series.workload}</span>
        {series.note ? (
          <span className="bench-row-note">{series.note}</span>
        ) : null}
      </div>
      <div className="bench-bars">
        {series.bars.map((b) => (
          <BarRow key={b.driver} bar={b} maxNs={maxNs} />
        ))}
      </div>
    </div>
  );
}

function BarRow({ bar, maxNs }: { bar: Bar; maxNs: number }) {
  // Clamp the floor so a 50× faster bar still has a visible sliver (1.5%).
  const widthPct = Math.max((bar.median_ns / maxNs) * 100, 1.5);
  const toneClass = bar.highlight
    ? "bench-bar accent"
    : bar.tone
      ? `bench-bar tone-${bar.tone}`
      : "bench-bar";

  return (
    <div className="bench-bar-line">
      <span className="bench-driver">{bar.driver}</span>
      <div className="bench-bar-track">
        <div
          className={toneClass}
          style={{ width: `${widthPct}%` }}
          aria-hidden="true"
        />
        <span className="bench-value mono">{bar.label}</span>
      </div>
    </div>
  );
}

function StatBlock({
  metric,
  label,
  detail,
}: (typeof HEADLINE_STATS)[number]) {
  return (
    <div className="bench-stat">
      <div className="bench-stat-metric mono">{metric}</div>
      <div className="bench-stat-label">{label}</div>
      <div className="bench-stat-detail dim">{detail}</div>
    </div>
  );
}

export function Benchmarks() {
  return (
    <section id="benchmarks">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">07 · benchmarks</span>
          <div>
            <h2>Honest numbers, published in public.</h2>
            <p className="sub">
              Twelve workloads against SQLite (WAL+NORMAL) and DuckDB on a
              pinned-host run. The point isn&rsquo;t to win — SQLite has 25
              years of optimization behind it — it&rsquo;s to baseline future
              engine work, prove the differentiator workloads deliver, and
              ground the roadmap with evidence.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="bench-stats">
            {HEADLINE_STATS.map((s) => (
              <StatBlock key={s.label} {...s} />
            ))}
          </div>

          <div className="bench-charts">
            {CHART_GROUPS.map((group) => (
              <div className="bench-group" key={group.title}>
                <div className="bench-group-head">
                  <h3>{group.title}</h3>
                  <p className="dim">{group.subtitle}</p>
                </div>
                {group.series.map((s) => (
                  <WorkloadBars key={s.workload} series={s} />
                ))}
              </div>
            ))}
          </div>

          <div className="bench-debts">
            <div className="eyebrow" style={{ marginBottom: 12 }}>
              engineering debts the bench surfaced
            </div>
            <p className="dim" style={{ fontSize: 14, marginBottom: 16 }}>
              The suite ships with the gap measured + the workaround
              documented + the task linked. Each is &ldquo;investigation, not a
              release gate.&rdquo;
            </p>
            <ul className="bench-debt-list">
              {DEBT_NOTES.map((d) => (
                <li key={d.ticket}>
                  <span className="mono accent">{d.ticket}</span>
                  <span className="bench-debt-workload">{d.workload}</span>
                  <span className="bench-debt-symptom">{d.symptom}</span>
                  <span className="bench-debt-cause dim">{d.cause}</span>
                </li>
              ))}
            </ul>
          </div>

          <div className="bench-method">
            <span className="dim">
              Run:{" "}
              <span className="mono">{RUN_META.date}</span> ·{" "}
              {RUN_META.host} · commit{" "}
              <span className="mono">{RUN_META.commit}</span>
            </span>
            <div
              style={{ display: "flex", gap: 14, marginTop: 8, flexWrap: "wrap" }}
            >
              <a className="accent mono" href={RUN_META.docPath}>
                full headline table →
              </a>
              <a className="accent mono" href={RUN_META.sourcePath}>
                raw JSON envelope →
              </a>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
