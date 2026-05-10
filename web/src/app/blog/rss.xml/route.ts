import { getAllPosts } from "@/lib/blog";
import { SITE } from "@/lib/site";

// Posts come from MDX files baked at build time, so the feed is
// build-time-static. Without this, Next renders it on every request.
export const dynamic = "force-static";

function escapeXml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;");
}

export function GET() {
  const posts = getAllPosts();
  const lastBuildDate =
    posts.length > 0
      ? new Date(posts[0].frontmatter.publishedAt).toUTCString()
      : new Date().toUTCString();

  const items = posts
    .map((post) => {
      const url = `${SITE.url}/blog/${post.slug}`;
      const pubDate = new Date(post.frontmatter.publishedAt).toUTCString();
      const categories = post.frontmatter.tags
        .map((t) => `      <category>${escapeXml(t)}</category>`)
        .join("\n");
      return `    <item>
      <title>${escapeXml(post.frontmatter.title)}</title>
      <link>${escapeXml(url)}</link>
      <guid isPermaLink="true">${escapeXml(url)}</guid>
      <pubDate>${pubDate}</pubDate>
      <description>${escapeXml(post.frontmatter.description)}</description>
      <author>noreply@sqlritedb.com (${escapeXml(post.frontmatter.author)})</author>
${categories}
    </item>`;
    })
    .join("\n");

  const xml = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>SQLRite Blog</title>
    <link>${escapeXml(`${SITE.url}/blog`)}</link>
    <atom:link href="${escapeXml(`${SITE.url}/blog/rss.xml`)}" rel="self" type="application/rss+xml" />
    <description>Long-form writing on building SQLRite — an embedded SQL + vector database in Rust.</description>
    <language>en-us</language>
    <lastBuildDate>${lastBuildDate}</lastBuildDate>
${items}
  </channel>
</rss>
`;

  return new Response(xml, {
    headers: {
      "Content-Type": "application/rss+xml; charset=utf-8",
      "Cache-Control": "public, max-age=3600, s-maxage=3600",
    },
  });
}
