export function Architecture() {
  return (
    <section id="architecture">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">03 · architecture</span>
          <div>
            <h2>From SQL string to disk page in seven layers.</h2>
            <p className="sub">
              SQLRite mirrors SQLite&rsquo;s classic two-half split: a front end
              that turns SQL into a plan, and a back end that turns that plan
              into bytes.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="arch">
            <div className="arch-row">
              <div className="arch-label">Input</div>
              <div className="arch-box">
                <div>
                  <span className="name">REPL · SDK · FFI · WASM · MCP</span>
                </div>
                <div className="desc">SQL text + bindings</div>
              </div>
            </div>
            <div className="arch-arrow">↓</div>
            <div className="arch-row">
              <div className="arch-label">Front end</div>
              <div className="arch-pair">
                <div className="arch-box">
                  <div>
                    <span className="name">Tokenizer / Parser</span>
                  </div>
                  <div className="desc">sqlparser · SQLite dialect</div>
                </div>
                <div className="arch-box">
                  <div>
                    <span className="name">Planner / Optimizer</span>
                  </div>
                  <div className="desc">index probes · HNSW · FTS</div>
                </div>
              </div>
            </div>
            <div className="arch-arrow">↓</div>
            <div className="arch-row">
              <div className="arch-label">Executor</div>
              <div className="arch-box accent-box">
                <div>
                  <span className="name">
                    SQLRite VM — row iteration · expressions · UNIQUE + type
                    checks · vector / FTS scoring
                  </span>
                </div>
                <div className="desc">core</div>
              </div>
            </div>
            <div className="arch-arrow">↓</div>
            <div className="arch-row">
              <div className="arch-label">Back end</div>
              <div className="arch-pair">
                <div className="arch-box">
                  <div>
                    <span className="name">B-Tree · HNSW · FTS posting</span>
                  </div>
                  <div className="desc">interior + leaf, overflow chains</div>
                </div>
                <div className="arch-box">
                  <div>
                    <span className="name">Pager</span>
                  </div>
                  <div className="desc">snapshot + staging diff</div>
                </div>
              </div>
            </div>
            <div className="arch-arrow">↓</div>
            <div className="arch-row">
              <div className="arch-label">Durability</div>
              <div className="arch-pair">
                <div className="arch-box">
                  <div>
                    <span className="name">WAL · &lt;db&gt;.sqlrite-wal</span>
                  </div>
                  <div className="desc">framed, checksummed, recoverable</div>
                </div>
                <div className="arch-box">
                  <div>
                    <span className="name">OS file lock</span>
                  </div>
                  <div className="desc">flock SH/EX</div>
                </div>
              </div>
            </div>
            <div className="arch-arrow">↓</div>
            <div className="arch-row">
              <div className="arch-label">Storage</div>
              <div className="arch-box">
                <div>
                  <span className="name">Single .sqlrite file</span>
                </div>
                <div className="desc">
                  4 KiB pages · header · cells · slot dir
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
