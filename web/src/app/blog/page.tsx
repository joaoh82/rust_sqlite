import Link from "next/link";
import type { Metadata } from "next";
import { Footer } from "@/components/footer";
import { Nav } from "@/components/nav";
import { SITE } from "@/lib/site";
import {
  formatDate,
  getAllPosts,
  getAllTags,
  tagToSlug,
} from "@/lib/blog";

const TITLE = "Blog";
const DESCRIPTION =
  "Long-form writing on building SQLRite — an embedded SQL + vector database in Rust. Design tenets, deep dives, benchmarks, and distribution.";

export const metadata: Metadata = {
  title: TITLE,
  description: DESCRIPTION,
  alternates: { canonical: "/blog" },
  openGraph: {
    type: "website",
    siteName: "SQLRite",
    locale: "en_US",
    url: `${SITE.url}/blog`,
    title: `${TITLE} · SQLRite`,
    description: DESCRIPTION,
  },
  twitter: {
    card: "summary_large_image",
    site: SITE.twitterHandle,
    creator: SITE.twitterHandle,
    title: `${TITLE} · SQLRite`,
    description: DESCRIPTION,
  },
};

const blogJsonLd = {
  "@context": "https://schema.org",
  "@type": "Blog",
  name: "SQLRite Blog",
  description: DESCRIPTION,
  url: `${SITE.url}/blog`,
  publisher: {
    "@type": "Organization",
    name: "SQLRite",
    url: SITE.url,
  },
};

export default function BlogIndexPage() {
  const posts = getAllPosts();
  const tags = getAllTags();

  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(blogJsonLd) }}
      />
      <Nav variant="docs" />
      <section id="blog-index" className="no-border">
        <div className="wrap">
          <div className="sec-head">
            <span className="eyebrow tag">writing · sqlrite</span>
            <div>
              <h2>Notes from building an embedded database in Rust.</h2>
              <p className="sub">
                Design tenets, file format deep-dives, vector search,
                benchmarks, and distribution. New posts roughly every two
                weeks. Subscribe via the{" "}
                <Link
                  href="/blog/rss.xml"
                  style={{ color: "var(--color-accent)" }}
                >
                  RSS feed
                </Link>
                .
              </p>
            </div>
          </div>

          {tags.length > 0 ? (
            <div
              className="blog-tag-row"
              role="navigation"
              aria-label="Tags"
              style={{ marginTop: 32 }}
            >
              {tags.map(({ tag, count }) => (
                <Link
                  key={tag}
                  href={`/blog/tags/${tagToSlug(tag)}`}
                  className="blog-tag"
                >
                  {tag}
                  <span className="blog-tag-count">{count}</span>
                </Link>
              ))}
            </div>
          ) : null}

          <div className="sec-body" style={{ paddingTop: 32 }}>
            <ul className="blog-index-list">
              {posts.map((post) => (
                <li key={post.slug} className="blog-index-item">
                  <Link
                    href={`/blog/${post.slug}`}
                    className="blog-index-link"
                  >
                    <div className="blog-index-meta">
                      <time dateTime={post.frontmatter.publishedAt}>
                        {formatDate(post.frontmatter.publishedAt)}
                      </time>
                      <span className="blog-index-dot" aria-hidden>
                        ·
                      </span>
                      <span>{post.readingTime} min read</span>
                    </div>
                    <h3 className="blog-index-title">
                      {post.frontmatter.title}
                    </h3>
                    <p className="blog-index-desc">
                      {post.frontmatter.description}
                    </p>
                    <div className="blog-index-tags">
                      {post.frontmatter.tags.map((t) => (
                        <span key={t} className="blog-index-tag">
                          {t}
                        </span>
                      ))}
                    </div>
                  </Link>
                </li>
              ))}
            </ul>
          </div>
        </div>
      </section>
      <Footer />
    </>
  );
}
