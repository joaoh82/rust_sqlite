"use client";

import { useEffect, useRef, useState } from "react";
import { useTrack } from "@/lib/analytics";

// Client-side docs search backed by Pagefind. The index is produced by the
// `postbuild` script (package.json) from the prerendered HTML in
// `.next/server/app` and served from `/pagefind/`. Pages opt in via the
// `data-pagefind-body` attribute (/docs and blog articles). In `next dev`
// no index exists — the box renders a "built at build time" hint instead.

type PagefindSearchResult = {
  id: string;
  data: () => Promise<{
    url: string;
    excerpt: string;
    meta: { title?: string };
  }>;
};

type Pagefind = {
  search: (query: string) => Promise<{ results: PagefindSearchResult[] }>;
};

type ResultRow = { id: string; url: string; title: string; excerpt: string };

// Pagefind indexes `.next/server/app/**/*.html`, so raw result URLs look
// like `/docs.html` or `/blog/<slug>.html` — map them back to routes.
function cleanUrl(url: string): string {
  return url.replace(/\.html$/, "").replace(/\/index$/, "/");
}

let pagefindPromise: Promise<Pagefind | null> | null = null;

// The specifier is a runtime variable (not a literal) so neither TypeScript
// nor the bundler tries to resolve it — the script only exists after the
// postbuild Pagefind run.
const PAGEFIND_URL = "/pagefind/pagefind.js";

function loadPagefind(): Promise<Pagefind | null> {
  pagefindPromise ??= import(
    /* webpackIgnore: true */ /* turbopackIgnore: true */ PAGEFIND_URL
  )
    .then((mod) => mod as Pagefind)
    .catch(() => null);
  return pagefindPromise;
}

export function DocsSearch() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<ResultRow[]>([]);
  const [open, setOpen] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const track = useTrack();
  const rootRef = useRef<HTMLDivElement>(null);
  const trackTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Run the search (debounced) whenever the query changes.
  useEffect(() => {
    if (!query.trim()) {
      setResults([]);
      setOpen(false);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(async () => {
      const pagefind = await loadPagefind();
      if (cancelled) return;
      if (!pagefind) {
        setUnavailable(true);
        setOpen(true);
        return;
      }
      const res = await pagefind.search(query);
      const rows = await Promise.all(
        res.results.slice(0, 8).map(async (r) => {
          const d = await r.data();
          return {
            id: r.id,
            url: cleanUrl(d.url),
            title: d.meta.title ?? cleanUrl(d.url),
            excerpt: d.excerpt,
          };
        }),
      );
      if (cancelled) return;
      setResults(rows);
      setOpen(true);

      // Report the query once typing settles, not per keystroke.
      if (trackTimer.current) clearTimeout(trackTimer.current);
      trackTimer.current = setTimeout(() => {
        track("docs-search-query", {
          query,
          results: res.results.length,
        });
      }, 1200);
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [query, track]);

  // Close on click-away / Escape.
  useEffect(() => {
    function onPointerDown(e: PointerEvent) {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    }
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, []);

  return (
    <div
      className="docs-search"
      ref={rootRef}
      role="search"
      data-pagefind-ignore
    >
      <input
        type="search"
        className="docs-search-input"
        placeholder="Search the docs…"
        aria-label="Search the docs"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onFocus={() => {
          void loadPagefind();
          if (results.length > 0 || unavailable) setOpen(true);
        }}
      />
      {open && (
        <div className="docs-search-results">
          {unavailable ? (
            <p className="docs-search-empty">
              Search index unavailable — it&rsquo;s generated at build time
              (<code>npm run build</code>).
            </p>
          ) : results.length === 0 ? (
            <p className="docs-search-empty">
              No results for &ldquo;{query}&rdquo;.
            </p>
          ) : (
            <ul>
              {results.map((r) => (
                <li key={r.id}>
                  <a href={r.url} onClick={() => setOpen(false)}>
                    <span className="docs-search-title">{r.title}</span>
                    <span
                      className="docs-search-excerpt"
                      // Pagefind escapes page content and only injects its
                      // own <mark> highlight tags — safe to render.
                      dangerouslySetInnerHTML={{ __html: r.excerpt }}
                    />
                  </a>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
