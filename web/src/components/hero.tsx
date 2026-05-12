import Link from "next/link";
import { SITE } from "@/lib/site";
import { GithubIcon } from "./icons";
import { InstallBar } from "./install-bar";
import { Terminal } from "./terminal";

export function Hero() {
  return (
    <section className="hero no-border">
      <div className="wrap">
        <div className="hero-grid">
          <div>
            <span className="eyebrow">
              v{SITE.version} · MIT licensed · open source
            </span>
            <h1 style={{ marginTop: 24 }}>
              An embedded SQL database,{" "}
              <span className="accent-word">built from scratch</span> in Rust.
            </h1>
            <p className="hero-tag">
              SQLRite is a from-the-ground-up reimagining of SQLite — a
              single-file engine with a real B-tree, write-ahead log,
              transactions, JOINs, aggregates, vector search, full-text search,
              and bindings for six languages. Built to teach what databases
              actually do.
            </p>
            <div className="cta-row">
              <Link className="btn btn-primary" href="/docs">
                Get started <span>→</span>
              </Link>
              <a
                className="btn"
                href={SITE.repo}
                target="_blank"
                rel="noreferrer"
              >
                <GithubIcon size={14} />
                View source
              </a>
            </div>
            <div className="hero-meta">
              <span>
                <span className="dot" /> Phases 0–11 shipped — concurrent writes live
              </span>
              <span>· v{SITE.version} on crates.io · PyPI · npm</span>
              <span>· Rust 2024 edition</span>
            </div>
          </div>
          <div>
            <Terminal />
            <InstallBar cmd="cargo install sqlrite-engine" />
          </div>
        </div>
      </div>
    </section>
  );
}
