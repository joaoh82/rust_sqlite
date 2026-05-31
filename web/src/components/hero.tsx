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
              SQLRite — an embedded SQL +{" "}
              <span className="accent-word">vector database</span> in Rust.
            </h1>
            <p className="hero-tag">
              SQLRite is a from-scratch SQLite alternative — a single-file
              embedded database in Rust with a real B-tree, write-ahead log,
              transactions, JOINs, aggregates, HNSW vector search, BM25
              full-text search, and bindings for six languages. Built to teach
              what databases actually do.
            </p>
            <div className="cta-row">
              <Link className="btn btn-primary" href="/docs">
                Get started <span>→</span>
              </Link>
              <Link className="btn" href="/playground">
                ▸ Try it in your browser
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
