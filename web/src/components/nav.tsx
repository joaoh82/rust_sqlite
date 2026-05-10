"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { SITE } from "@/lib/site";
import { GithubIcon } from "./icons";

type NavProps = { variant?: "landing" | "docs" };

export function Nav({ variant = "landing" }: NavProps) {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("keydown", onKey);
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = "";
    };
  }, [open]);

  const close = () => setOpen(false);

  return (
    <nav className="nav" aria-label="Site">
      <div className="wrap nav-inner">
        <Link className="brand" href="/" onClick={close}>
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
        <div className="nav-links" data-variant={variant}>
          {variant === "landing" ? (
            <>
              <a href="#features">Features</a>
              <a href="#architecture">Architecture</a>
              <a href="#roadmap">Roadmap</a>
              <a href="#sdks">SDKs</a>
              <a href="#benchmarks">Benchmarks</a>
              <Link href="/docs">Docs</Link>
              <Link href="/blog">Blog</Link>
            </>
          ) : (
            <>
              <Link href="/#features">Features</Link>
              <Link href="/#architecture">Architecture</Link>
              <Link href="/#roadmap">Roadmap</Link>
              <Link href="/#sdks">SDKs</Link>
              <Link href="/#benchmarks">Benchmarks</Link>
              <Link href="/docs">Docs</Link>
              <Link href="/blog">Blog</Link>
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
        <button
          type="button"
          className="nav-toggle"
          aria-expanded={open}
          aria-controls="mobile-menu"
          aria-label={open ? "Close menu" : "Open menu"}
          onClick={() => setOpen((v) => !v)}
        >
          <span className={`nav-toggle-bars ${open ? "open" : ""}`} />
        </button>
      </div>

      <div
        id="mobile-menu"
        className={`mobile-menu ${open ? "open" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label="Navigation"
        hidden={!open}
      >
        <div className="mobile-menu-inner">
          {variant === "landing" ? (
            <>
              <a href="#features" onClick={close}>
                Features
              </a>
              <a href="#architecture" onClick={close}>
                Architecture
              </a>
              <a href="#roadmap" onClick={close}>
                Roadmap
              </a>
              <a href="#sdks" onClick={close}>
                SDKs
              </a>
              <a href="#benchmarks" onClick={close}>
                Benchmarks
              </a>
              <Link href="/docs" onClick={close}>
                Docs
              </Link>
              <Link href="/blog" onClick={close}>
                Blog
              </Link>
            </>
          ) : (
            <>
              <Link href="/#features" onClick={close}>
                Features
              </Link>
              <Link href="/#architecture" onClick={close}>
                Architecture
              </Link>
              <Link href="/#roadmap" onClick={close}>
                Roadmap
              </Link>
              <Link href="/#sdks" onClick={close}>
                SDKs
              </Link>
              <Link href="/#benchmarks" onClick={close}>
                Benchmarks
              </Link>
              <Link href="/docs" onClick={close}>
                Docs
              </Link>
              <Link href="/blog" onClick={close}>
                Blog
              </Link>
              <Link href="/" onClick={close}>
                Home
              </Link>
            </>
          )}
          <a
            className="nav-cta mobile-menu-cta"
            href={SITE.repo}
            target="_blank"
            rel="noreferrer"
            onClick={close}
          >
            <GithubIcon size={13} />
            <span>View on GitHub</span>
          </a>
        </div>
      </div>
    </nav>
  );
}
