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
- `/blog` — index of long-form posts pulled from `content/blog/*.mdx`
- `/blog/[slug]` — per-post detail page (MDX rendered server-side, `Article` JSON-LD, breadcrumb JSON-LD, dynamic OG image, prev/next navigation)
- `/blog/tags/[tag]` — tag pages (one per unique frontmatter tag)
- `/blog/rss.xml` — RSS 2.0 feed

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
  page, `BreadcrumbList` on `/docs`, `Blog` on `/blog`, and
  `BlogPosting` + `BreadcrumbList` on each `/blog/<slug>`. Validate via
  Google's [Rich Results Test](https://search.google.com/test/rich-results).
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
├── content/
│   └── blog/                # MDX posts (one .mdx file per post; frontmatter at top)
├── src/
│   ├── app/
│   │   ├── globals.css      # design tokens + utility CSS (ports the original design's styles.css)
│   │   ├── layout.tsx       # root layout, fonts (Inter + JetBrains Mono via next/font)
│   │   ├── page.tsx         # landing
│   │   ├── docs/page.tsx    # /docs
│   │   ├── blog/            # /blog index, [slug] detail, tags/[tag], rss.xml
│   │   ├── sitemap.ts       # /sitemap.xml — enumerates static + per-post + per-tag URLs
│   │   └── robots.ts        # /robots.txt
│   ├── components/          # one .tsx per landing section (hero, features, roadmap, …)
│   └── lib/
│       ├── blog.ts          # MDX loader: frontmatter parsing, post enumeration, tag helpers
│       ├── og.tsx           # shared OpenGraph frame
│       ├── site.ts          # SITE constants (version, repo URL, social links)
│       └── utils.ts         # shadcn cn() helper
└── components.json          # shadcn/ui config
```

## Blog

The blog is content-driven. Posts live as `.mdx` files in
[`content/blog/`](content/blog) and are rendered server-side via
[`next-mdx-remote`](https://github.com/hashicorp/next-mdx-remote).
Frontmatter is parsed by `gray-matter`.

### Adding a post

Create `content/blog/<slug>.mdx`:

```mdx
---
title: "Your post title"
description: "One-sentence description used in <meta>, OG, RSS."
publishedAt: "2026-05-10"          # ISO date, sorts the index
updatedAt: "2026-05-12"            # optional
author: "Joao Henrique Machado Silva"
tags: ["sqlrite", "rust"]          # also drives /blog/tags/[tag]
primaryKeyword: "rust sql engine"  # optional, for SEO bookkeeping
---

Body text in Markdown / MDX.
```

Then:

- The post is automatically picked up by `/blog`, `/blog/<slug>`,
  every relevant `/blog/tags/<tag>`, the RSS feed, and the sitemap.
- An OG image is generated dynamically from the title +
  description at `/blog/<slug>/opengraph-image`.
- `BlogPosting` JSON-LD and `BreadcrumbList` JSON-LD are injected
  on the detail page.

### Required frontmatter validity

`src/lib/blog.ts` validates frontmatter at load time and throws if
`title`, `description`, `publishedAt`, `author`, or `tags` is
missing / wrong-typed. The build will fail fast in CI rather than
shipping a half-broken post.

### MDX caveats

`<` and `{` in prose can confuse the MDX parser. Wrap them in
backticks or escape (`&lt;`, `\{`). The MDX renderer auto-routes
internal `[link](/foo)` markdown links through `next/link`; external
links open in a new tab via `rel="noreferrer"`.

### Code block highlighting

Fenced code blocks are tokenized at build time by
[`rehype-pretty-code`](https://github.com/rehype-pretty/rehype-pretty-code)
with [Shiki](https://shiki.style/). The plugin is wired up in
[`src/components/blog-mdx.tsx`](src/components/blog-mdx.tsx) using a
`createCssVariablesTheme()` so Shiki emits inline styles like
`color: var(--shiki-token-keyword)`. The mapping from
`--shiki-*` to the blog's color palette (`--color-kw`, `--color-str`,
`--color-num`, …) lives in the `.blog-article-body pre` rule in
[`src/app/globals.css`](src/app/globals.css) — adjust colors there,
not in the component. Code fences without a language fall back to
`plaintext` so unknown / missing language tags don't fail the build.
Inline `code` keeps its existing chip styling (`bypassInlineCode: true`).

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
