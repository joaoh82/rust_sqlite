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

## Responsive design

The site is mobile-first and verified at 375px (iPhone SE), 390px
(iPhone 14), 768px (iPad), and 1024px+. Key conventions:

- **Breakpoints** live at the bottom of [`src/app/globals.css`](src/app/globals.css):
  900px (tablet), 760px (mobile nav cutover), 640px (phone), and 380px
  (very small phones). Section-level grids declare their own breakpoints
  inline near their styles (features, bench bars, footer, etc.).
- **Nav** ([`src/components/nav.tsx`](src/components/nav.tsx)) is a
  client component. Below 760px the inline links collapse into a 44×44
  hamburger that opens a full-width drawer; Esc closes; the body scroll
  is locked while open.
- **Docs** ([`src/app/docs/page.tsx`](src/app/docs/page.tsx)) hides the
  desktop sidebar and on-page TOC under 1000px and 720px respectively
  and shows a sticky `<details>`-driven section list in their place.
- **Tap targets** — primary buttons (`.btn`), the hamburger, install-bar
  copy, mobile menu links, and the docs section toggle are all ≥ 44px
  tall on phones. Footer / docs sidebar inline nav links stay at ~36px,
  which is the common compromise for dense navigation lists.
- **Horizontal scroll** is guarded globally with `html { overflow-x:
  clip }`. We use `clip` instead of `hidden` so `position: sticky` keeps
  working for the nav and the docs sidebar. Long URLs / unbroken tokens
  in prose use `overflow-wrap: anywhere` so they don't blow out the
  viewport.
- **Tables and code blocks** scroll horizontally inside their container
  (`overflow-x: auto`); the SQL surface table on `/` reflows into
  stacked cards under 640px since its second column is a long pill list.
- **Viewport / theme color** — set via the `viewport` export in
  [`src/app/layout.tsx`](src/app/layout.tsx); the dark `#0b0c0e`
  `themeColor` keeps mobile browser chrome from flashing white.

When adding new sections, declare the breakpoint logic alongside the
section's styles rather than at the bottom of the file — it keeps the
section self-contained and the global breakpoint block reserved for
typography / spacing baseline tweaks.

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
