import { SITE } from "@/lib/site";

export function Desktop() {
  return (
    <section id="desktop">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">08 · desktop</span>
          <div>
            <h2>A native client for browsing your data.</h2>
            <p className="sub">
              Cross-platform, dark by default, written in Svelte 5 + Tauri 2.0.
              The header has <code>New…</code> / <code>Open…</code> /{" "}
              <code>Save As…</code> buttons; the editor has live line numbers,{" "}
              <span className="kbd">⌘/</span> comment toggle, and
              selection-aware Run.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="desktop-shell">
            <div className="dt-bar">
              <span className="dt-dot r" />
              <span className="dt-dot y" />
              <span className="dt-dot g" />
              <span className="dt-title">SQLRite — app.sqlrite</span>
            </div>
            <div className="dt-body">
              <aside className="dt-side">
                <div className="group">tables</div>
                <div className="item active">▸ users</div>
                <div className="item">▸ sessions</div>
                <div className="item">▸ events</div>
                <div className="item">▸ docs (FTS)</div>
                <div className="item">▸ sqlrite_master</div>
                <div className="group" style={{ marginTop: 12 }}>
                  indexes
                </div>
                <div className="item">◇ idx_events_ts</div>
                <div className="item">◇ docs_embedding (HNSW)</div>
                <div className="item">◇ docs_body (FTS)</div>
              </aside>
              <div className="dt-main">
                <div className="dt-editor">
                  <div>
                    <span className="dt-line-nums">1</span>
                    <span className="cmt">-- top earners</span>
                  </div>
                  <div>
                    <span className="dt-line-nums">2</span>
                    <span className="kw">SELECT</span> name, age{" "}
                    <span className="kw">FROM</span> users
                  </div>
                  <div>
                    <span className="dt-line-nums">3</span>
                    <span className="kw">WHERE</span> age &gt;{" "}
                    <span className="num">25</span>
                  </div>
                  <div>
                    <span className="dt-line-nums">4</span>
                    <span className="kw">ORDER BY</span> age{" "}
                    <span className="kw">DESC</span>{" "}
                    <span className="kw">LIMIT</span>{" "}
                    <span className="num">5</span>;
                  </div>
                </div>
                <div className="dt-results">
                  <span className="ok">✓ 3 rows · 1.2ms</span>
                  <table>
                    <thead>
                      <tr>
                        <th>name</th>
                        <th>age</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr>
                        <td>alice</td>
                        <td>31</td>
                      </tr>
                      <tr>
                        <td>cara</td>
                        <td>29</td>
                      </tr>
                      <tr>
                        <td>dan</td>
                        <td>27</td>
                      </tr>
                    </tbody>
                  </table>
                </div>
              </div>
            </div>
          </div>
          <div
            style={{
              display: "flex",
              gap: 12,
              marginTop: 24,
              flexWrap: "wrap",
            }}
          >
            <a className="btn" href={SITE.releasesLatest}>
              macOS · .dmg
            </a>
            <a className="btn" href={SITE.releasesLatest}>
              Windows · .msi
            </a>
            <a className="btn" href={SITE.releasesLatest}>
              Linux · .AppImage / .deb / .rpm
            </a>
          </div>
        </div>
      </div>
    </section>
  );
}
