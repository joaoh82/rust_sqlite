import fs from "node:fs";
import path from "node:path";
import { cache } from "react";
import { getAllPosts } from "@/lib/blog";
import { SITE } from "@/lib/site";

// Builders for the llms.txt convention (https://llmstxt.org/):
//   /llms.txt      — curated plain-text index of canonical URLs + summaries
//   /llms-full.txt — concatenated full-text markdown of the docs surface
// Both are generated at build time (the route handlers are force-static) so
// they cost nothing at request time. All URLs are absolute against SITE.url.

const DOCS_MD_PATH = path.join(
  process.cwd(),
  "content",
  "docs",
  "getting-started.md",
);

export const getDocsMarkdown = cache((): string => {
  const raw = fs.readFileSync(DOCS_MD_PATH, "utf8");
  // The sync-reminder comment is for maintainers, not for crawlers.
  return raw.replace(/<!--[\s\S]*?-->\n?/g, "");
});

export const buildLlmsTxt = cache((): string => {
  const posts = getAllPosts();
  const postLines = posts
    .map(
      (p) =>
        `- [${p.frontmatter.title}](${SITE.url}/blog/${p.slug}): ${p.frontmatter.description}`,
    )
    .join("\n");

  return `# SQLRite

> SQLRite is an embedded SQL + vector database written in Rust — a
> SQLite-style single-file engine with WAL transactions, MVCC concurrent
> writes, HNSW vector search, BM25 full-text search, an MCP server, and six
> language SDKs (Rust, Python, Node.js, Go, C, WASM). MIT licensed.

Current version: ${SITE.version}. Docs in markdown: ${SITE.url}/docs.md and
${SITE.url}/llms-full.txt.

## Docs

- [Getting started](${SITE.url}/docs): install the CLI / crate / SDKs, open a .sqlrite file, transactions, JOINs, aggregates, prepared statements, PRAGMA, vector + full-text search, the desktop app, and the MCP server
- [Getting started (markdown)](${SITE.url}/docs.md): the same page served as plain markdown
- [SQL playground](${SITE.url}/playground): the full engine compiled to WebAssembly, runnable in the browser — no install
- [Examples](${SITE.url}/examples): runnable snippets per language and use case

## Blog

${postLines}

## Reference

- [GitHub repository](${SITE.repo}): source, issues, and the in-repo developer guide (docs/)
- [API docs on docs.rs](${SITE.docsRs}): Rust API reference for the sqlrite-engine crate
- [crates.io](${SITE.cratesIo}): the published Rust crate
- [Releases](${SITE.releases}): prebuilt desktop installers, MCP binaries, and C FFI tarballs

## Optional

- [Blog RSS feed](${SITE.url}/blog/rss.xml): subscribe to new posts
- [PyPI package](${SITE.pypi}): Python SDK
- [npm package](${SITE.npm}): Node.js SDK
`;
});

export const buildLlmsFullTxt = cache((): string => {
  const posts = getAllPosts();

  const header = `# SQLRite — full documentation

> SQLRite is an embedded SQL + vector database written in Rust. This file
> concatenates the documentation and blog content from ${SITE.url} as plain
> markdown for LLM consumption. Curated index: ${SITE.url}/llms.txt

Canonical pages: ${SITE.url}/docs (docs), ${SITE.url}/blog (blog),
${SITE.url}/playground (in-browser playground). Source: ${SITE.repo}

---

`;

  const docs = getDocsMarkdown().trim();

  const postSections = posts
    .map((p) => {
      const fm = p.frontmatter;
      const date = fm.updatedAt ?? fm.publishedAt;
      return [
        `# ${fm.title}`,
        "",
        `> ${fm.description}`,
        "",
        `Canonical URL: ${SITE.url}/blog/${p.slug} · Published ${fm.publishedAt}${
          fm.updatedAt ? ` · Updated ${date}` : ""
        }`,
        "",
        p.content.trim(),
      ].join("\n");
    })
    .join("\n\n---\n\n");

  return `${header}${docs}\n\n---\n\n${postSections}\n`;
});
