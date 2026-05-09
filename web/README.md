# SQLRite website

Marketing + docs site for [SQLRite](https://github.com/joaoh82/rust_sqlite).
Lives inside the same repo as the engine for now; it is intentionally
self-contained (independent `package.json`, no Cargo coupling) so it can be
extracted into its own repository later without rewrites.

## Stack

- Next.js 15 (App Router) + React 19
- TypeScript (strict)
- Tailwind CSS v4 (CSS-first `@theme` config in [`src/app/globals.css`](src/app/globals.css))
- shadcn/ui infrastructure (`components.json` + `cn` helper) — components are added on demand
- `lucide-react` for icons

## Pages

- `/` — landing (hero with animated REPL, features, architecture, roadmap, SDK switcher, SQL surface, desktop showcase, blog series, footer)
- `/docs` — Getting Started page (sticky sidebar nav + on-page TOC)

## SEO surface

Each public route ships full search/social metadata. The pieces:

- **Per-route `<title>` + `<meta name="description">`** — declared via the
  Next App-Router `metadata` export on each `page.tsx` (and a site-wide
  template in [`src/app/layout.tsx`](src/app/layout.tsx)).
- **Canonical URL** — `alternates.canonical` on every page; prevents the
  `/docs` tree (and any future hash/query variants) from being treated as
  duplicates.
- **OpenGraph + Twitter Card** — full set of `og:*` and `twitter:*` tags per
  route. Heads-up: Next 15 does **not** deep-merge `openGraph` / `twitter`
  between layout and page, so site-wide fields (`siteName`, `card`,
  `site`/`creator`) are restated on each page-level override.
- **Auto-generated OG images** — every page has a sibling
  `opengraph-image.tsx` + `twitter-image.tsx` rendered via
  `next/og`'s `ImageResponse` at the edge. Layout lives in
  [`src/lib/og.tsx`](src/lib/og.tsx) so each route just supplies a
  page-specific eyebrow / title / subtitle. The brand mark is inlined as
  SVG (Satori's dynamic-font fallback 400s on uncommon glyphs).
- **`/sitemap.xml` + `/robots.txt`** — Next 15 metadata routes
  ([`src/app/sitemap.ts`](src/app/sitemap.ts),
  [`src/app/robots.ts`](src/app/robots.ts)). Add a route to the `ROUTES`
  list when shipping a new page.
- **JSON-LD structured data** — `SoftwareApplication` schema on the landing
  page, `BreadcrumbList` on `/docs`. Validate via Google's
  [Rich Results Test](https://search.google.com/test/rich-results).
- **Search Console verification** — fill in the placeholder tokens in
  `metadata.verification` ([`src/app/layout.tsx`](src/app/layout.tsx)) once
  Google Search Console + Bing Webmaster Tools issue them.

The canonical site URL + Twitter handle live in
[`src/lib/site.ts`](src/lib/site.ts) (`SITE.url`, `SITE.twitterHandle`) —
update both there if the domain or handle ever changes.

## Local development

```sh
cd web
npm install
npm run dev      # http://localhost:3000
```

Other commands:

```sh
npm run build      # production build
npm run typecheck  # tsc --noEmit
npm run lint       # next lint (ESLint)
```

## Project structure

```
web/
├── src/
│   ├── app/
│   │   ├── globals.css      # design tokens + utility CSS (ports the original design's styles.css)
│   │   ├── layout.tsx       # root layout, fonts (Inter + JetBrains Mono via next/font)
│   │   ├── page.tsx         # landing
│   │   └── docs/page.tsx    # /docs
│   ├── components/          # one .tsx per landing section (hero, features, roadmap, …)
│   └── lib/
│       ├── site.ts          # SITE constants (version, repo URL, social links)
│       └── utils.ts         # shadcn cn() helper
└── components.json          # shadcn/ui config
```

The design tokens (colors, typography, spacing) live in `globals.css`'s
`@theme` block. The page-level CSS (sections, terminal, feature grid,
roadmap timeline, etc.) is intentionally hand-rolled — it ports the
prototype's `styles.css` 1:1 rather than reaching for component-library
abstractions.

## Updating the version

The displayed version is in [`src/lib/site.ts`](src/lib/site.ts). Update it
when the engine cuts a new release.

## Deploying

The site is a static-friendly Next.js app and deploys to Vercel out of the
box. Point Vercel at the `web/` directory:

- **Root Directory:** `web`
- **Framework Preset:** Next.js (auto-detected)
- No environment variables required.

For other hosts, `next build` produces a standard Next.js output suitable
for any Node-friendly runtime.

## License

MIT — same as the rest of the repo.
