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
