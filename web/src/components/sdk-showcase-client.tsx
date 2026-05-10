"use client";

import { useState } from "react";
import { InstallBar } from "./install-bar";

export type SdkEntry = {
  key: string;
  name: string;
  install: string;
  version: string;
  registry: string;
  note: string;
  ext: string;
  /** Pre-rendered shiki output. Built server-side at compile time. */
  codeHtml: string;
};

export function SDKShowcaseClient({ sdks }: { sdks: SdkEntry[] }) {
  const [tab, setTab] = useState(sdks[0]?.key ?? "");
  const sdk = sdks.find((s) => s.key === tab) ?? sdks[0];
  if (!sdk) return null;

  return (
    <section id="sdks">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">05 · embedding</span>
          <div>
            <h2>One engine. Six languages.</h2>
            <p className="sub">
              The same Rust core — wrapped, never reimplemented. SDKs ship as
              prebuilt binaries so there&rsquo;s no toolchain to install just to
              use the database.
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          <div className="sdk-tabs" role="tablist">
            {sdks.map((s) => (
              <button
                key={s.key}
                role="tab"
                aria-selected={tab === s.key}
                className={`sdk-tab ${tab === s.key ? "active" : ""}`}
                onClick={() => setTab(s.key)}
              >
                {s.name}
              </button>
            ))}
          </div>
          <div className="sdk-panel">
            <div className="sdk-meta">
              <h3>{sdk.name}</h3>
              <p
                className="dim"
                style={{ marginTop: 6, fontSize: 13.5 }}
              >
                {sdk.note}
              </p>
              <InstallBar cmd={sdk.install} />
              <div style={{ marginTop: 18 }}>
                <div className="meta-row">
                  <span>version</span>
                  <span className="v">{sdk.version}</span>
                </div>
                <div className="meta-row">
                  <span>registry</span>
                  <span className="v">{sdk.registry}</span>
                </div>
                <div className="meta-row">
                  <span>license</span>
                  <span className="v">MIT</span>
                </div>
              </div>
            </div>
            <div className="code-block">
              <div className="code-head">
                <span>example.{sdk.ext}</span>
                <span>· copy-pasteable</span>
              </div>
              <div
                className="code-body"
                dangerouslySetInnerHTML={{ __html: sdk.codeHtml }}
              />
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
