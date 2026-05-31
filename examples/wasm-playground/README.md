# Browser SQL playground (WebAssembly)

> **Live:** **https://sqlritedb.com/playground**

A zero-install, browser-only SQL playground that runs the **full SQLRite
engine entirely in WebAssembly** — no server, no account, nothing to
install. Type SQL, hit Run, see results. Load a sample dataset and poke at
JOINs, `GROUP BY`, and HNSW cosine vector search right on the page. Your
data never leaves the browser tab.

This is example app #4 of the [SQLR-38 example-apps umbrella](../README.md).
It's the highest-marketing-leverage example — a "try it in your browser"
link is the fastest path from a curious dev to a running query.

> [!NOTE]
> Unlike the other examples (which are standalone runnable directories),
> the playground's home is the marketing site itself. The implementation
> lives in [`web/src/app/playground/`](../../web/src/app/playground/) so it
> can ship as a route on https://sqlritedb.com and reuse the site's nav,
> footer, theme, and SEO plumbing. This directory is the canonical README +
> architecture write-up. For a bare, framework-free version of the same
> idea, see the minimal vanilla-HTML demo in [`examples/wasm/`](../wasm/).

## What it shows off

| Feature | How |
|---|---|
| **WASM SDK** (`@joaoh82/sqlrite-wasm`) | The same `sqlrite-engine` crate every SDK uses, built for `wasm32-unknown-unknown`. ~750 KB gzipped, fetched once. |
| **HNSW vector search** | The "Movies" dataset has 12 films with hand-made 4-dim embeddings + a `USING hnsw (embedding) WITH (metric = 'cosine')` index; the sample query ranks them with `vec_distance_cosine` in `ORDER BY`. |
| **JOINs** | The "Northwind" dataset joins 4 tables (order lines → orders → customers → products). |
| **Aggregates / filters** | The "Pokémon" dataset for `WHERE`, `ORDER BY`, `GROUP BY`, `COUNT` / `AVG`. |
| **Transactions** | `BEGIN` / `COMMIT` / `ROLLBACK` work like any other statement. |

## Feature set

- Multi-line SQL editor (CodeMirror 6) with SQL syntax highlighting.
- **Run** button + **Cmd/Ctrl+Enter** shortcut. Multi-statement scripts run
  top to bottom; the last `SELECT`'s rows are shown.
- Results grid with **per-column types**, **NULL highlighting**, and **CSV
  export**.
- **Sample datasets** dropdown — schema + data + an example query in one
  click.
- **Reset DB** — back to a fresh in-memory database.
- **Share** — encodes the editor SQL into the URL hash (`#sql=…`) and copies
  a shareable link.
- **Persistence** — your session survives reloads (see below).
- **Download / Upload `.sql`** — take your session script with you, or load
  someone else's.

## Architecture

```
 Browser tab
 ┌──────────────────────────────────────────────────────────────┐
 │  /playground  (Next.js route, statically prerendered)         │
 │                                                                │
 │   PlaygroundLoader ── dynamic(ssr:false) ──▶ Playground       │
 │                                                  │             │
 │        ┌─────────────────────────────────────────┤            │
 │        ▼                     ▼                     ▼            │
 │   CodeMirror 6          ResultsPane           lib/wasm.ts      │
 │   (SQL editor)          (table + CSV)              │           │
 │                                                    ▼           │
 │                              runtime import("/playground/pkg/  │
 │                                          sqlrite_wasm.js")      │
 │                                                    │           │
 │                                                    ▼           │
 │                              sqlrite_wasm_bg.wasm  (the engine) │
 │                              new Database() · exec() · query()  │
 └──────────────────────────────────────────────────────────────┘
        persistence: OPFS  ─▶  fallback localStorage  ─▶  none
```

**Key files** (under [`web/src/app/playground/`](../../web/src/app/playground/)):

- `page.tsx` — server component: SEO metadata, JSON-LD, the static shell.
- `PlaygroundLoader.tsx` — `dynamic(..., { ssr: false })` so CodeMirror +
  the WASM engine stay out of SSR and the initial bundle.
- `Playground.tsx` — the orchestrator (state, run loop, datasets, share,
  persistence, download/upload).
- `components/Editor.tsx` — CodeMirror 6 wrapper (theme, SQL mode,
  Cmd/Ctrl+Enter keymap).
- `components/ResultsPane.tsx` — results table, NULL/type rendering, CSV.
- `lib/wasm.ts` — loads + initialises the WASM module exactly once.
- `lib/sql.ts` — quote/comment-aware statement splitter, SELECT detection,
  CSV.
- `lib/datasets.ts` — the three sample datasets.
- `lib/persist.ts` — OPFS / localStorage session storage + share-hash codec.

The WASM artifact is a **pinned, vendored copy** of `sdk/wasm/pkg/` in
[`web/public/playground/pkg/`](../../web/public/playground/pkg/), served as a
static asset. It is loaded with a *runtime* `import()` (kept external via
`/* webpackIgnore */`) rather than bundled, because wasm-pack's glue fetches
its sibling `.wasm` via `import.meta.url` — a shape the Next bundler mangles.

### How persistence works (script replay)

The WASM build is **in-memory only** — there is no `.sqlrite` byte image to
save. So instead of persisting the *database*, the playground persists the
*script*: every mutating statement (`CREATE` / `INSERT` / `UPDATE` /
`DELETE` / tx control) you successfully run is appended to a session log. On
reload, that log is replayed into a fresh in-memory database, reconstructing
the same state. **Download .sql** hands you that exact replayable script.

Storage backend, in priority order:

1. **OPFS** (Origin Private File System) — `session.sql` + `editor.sql`.
2. **localStorage** — when OPFS write access isn't available.
3. **none** — locked-down / private contexts; the playground still works,
   it just won't survive a reload.

## Running locally

The playground is part of the website app:

```sh
cd web
npm install
npm run dev          # http://localhost:3000/playground
```

To refresh the vendored WASM bundle after changing the engine or the WASM
SDK:

```sh
cd sdk/wasm
wasm-pack build --target web --release --out-dir pkg
cp pkg/sqlrite_wasm.js pkg/sqlrite_wasm_bg.wasm pkg/sqlrite_wasm.d.ts \
   pkg/sqlrite_wasm_bg.wasm.d.ts ../../web/public/playground/pkg/
```

CI already builds the WASM target (`wasm-build` job in
[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)) and reports its
gzipped size; the website deploys to Vercel.

## Bundle size

The WASM module is **~750 KB gzipped** (~2.1 MB raw) — comfortably under the
4 MB budget the umbrella set. Shrinking it further (feature-gating unused
engine pieces, `wasm-opt` passes) is a deliberate follow-up, not a blocker.

## Known limitations

- **No binary `.sqlrite` export.** The WASM build is in-memory only and the
  SDK exposes no serialise/deserialise. Persistence is via SQL-script replay
  (above); binary round-trip needs an engine change (an in-memory storage
  backend + `Database.export()/import()` on the SDK) and is tracked as a
  follow-up.
- **OPFS support varies.** Safari added OPFS write support relatively late,
  and some private-browsing modes block it. The playground feature-detects
  and falls back to `localStorage`, then to no persistence.
- **No `ask` (natural-language → SQL).** That needs a server-side API key,
  which a static browser page can't safely hold. It ships in the REPL, MCP
  server, and desktop app instead.
- **Engine gaps surface as errors.** Aggregates over JOIN results and a few
  other shapes aren't implemented yet — they return a normal engine error in
  the results pane. See [`docs/supported-sql.md`](../../docs/supported-sql.md).

## Browser support

Latest two majors of **Chrome, Firefox, and Safari**. Requires WebAssembly
and ES modules (both ~universally available since 2018). Persistence quality
degrades gracefully on browsers without OPFS write support.
