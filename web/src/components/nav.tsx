import Link from "next/link";
import { SITE } from "@/lib/site";
import { GithubIcon } from "./icons";

type NavProps = { variant?: "landing" | "docs" };

export function Nav({ variant = "landing" }: NavProps) {
  return (
    <nav className="nav">
      <div className="wrap nav-inner">
        <Link className="brand" href="/">
          <span className="brand-mark">▸</span>
          <span>sqlrite</span>
          <span
            className="dimmer mono"
            style={{ fontSize: 11, marginLeft: 4 }}
          >
            v{SITE.version}
            {variant === "docs" ? " / docs" : ""}
          </span>
        </Link>
        <div className="nav-links">
          {variant === "landing" ? (
            <>
              <a href="#features">Features</a>
              <a href="#architecture">Architecture</a>
              <a href="#roadmap">Roadmap</a>
              <a href="#sdks">SDKs</a>
              <a href="#benchmarks">Benchmarks</a>
              <Link href="/docs">Docs</Link>
            </>
          ) : (
            <>
              <Link href="/#features">Features</Link>
              <Link href="/#architecture">Architecture</Link>
              <Link href="/#roadmap">Roadmap</Link>
              <Link href="/#sdks">SDKs</Link>
              <Link href="/#benchmarks">Benchmarks</Link>
            </>
          )}
          <a
            className="nav-cta"
            href={SITE.repo}
            target="_blank"
            rel="noreferrer"
          >
            <GithubIcon size={13} />
            {variant === "landing" ? "Star" : "GitHub"}
            {variant === "landing" ? (
              <span className="nav-star">★</span>
            ) : null}
          </a>
        </div>
      </div>
    </nav>
  );
}
