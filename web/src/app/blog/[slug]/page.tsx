import Link from "next/link";
import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { Footer } from "@/components/footer";
import { Nav } from "@/components/nav";
import { BlogMDX } from "@/components/blog-mdx";
import { SITE } from "@/lib/site";
import {
  formatDate,
  getAllPostSlugs,
  getAllPosts,
  getPostBySlug,
  tagToSlug,
} from "@/lib/blog";

type RouteParams = { params: Promise<{ slug: string }> };

export function generateStaticParams() {
  return getAllPostSlugs().map((slug) => ({ slug }));
}

export async function generateMetadata({
  params,
}: RouteParams): Promise<Metadata> {
  const { slug } = await params;
  const post = getPostBySlug(slug);
  if (!post) return {};

  const url = `${SITE.url}/blog/${post.slug}`;
  return {
    title: post.frontmatter.title,
    description: post.frontmatter.description,
    authors: [{ name: post.frontmatter.author }],
    keywords: post.frontmatter.tags,
    alternates: { canonical: `/blog/${post.slug}` },
    openGraph: {
      type: "article",
      siteName: "SQLRite",
      locale: "en_US",
      url,
      title: post.frontmatter.title,
      description: post.frontmatter.description,
      publishedTime: post.frontmatter.publishedAt,
      modifiedTime: post.frontmatter.updatedAt,
      authors: [post.frontmatter.author],
      tags: post.frontmatter.tags,
    },
    twitter: {
      card: "summary_large_image",
      site: SITE.twitterHandle,
      creator: SITE.twitterHandle,
      title: post.frontmatter.title,
      description: post.frontmatter.description,
    },
  };
}

export default async function BlogPostPage({ params }: RouteParams) {
  const { slug } = await params;
  const post = getPostBySlug(slug);
  if (!post) notFound();

  const url = `${SITE.url}/blog/${post.slug}`;
  const allPosts = getAllPosts();
  const idx = allPosts.findIndex((p) => p.slug === post.slug);
  // Posts sort newest-first, so idx-1 is newer and idx+1 is older.
  const newer = idx > 0 ? allPosts[idx - 1] : null;
  const older =
    idx >= 0 && idx < allPosts.length - 1 ? allPosts[idx + 1] : null;

  const articleJsonLd = {
    "@context": "https://schema.org",
    "@type": "BlogPosting",
    headline: post.frontmatter.title,
    description: post.frontmatter.description,
    datePublished: post.frontmatter.publishedAt,
    dateModified: post.frontmatter.updatedAt ?? post.frontmatter.publishedAt,
    author: {
      "@type": "Person",
      name: post.frontmatter.author,
      url: SITE.socials.github,
    },
    publisher: {
      "@type": "Organization",
      name: "SQLRite",
      url: SITE.url,
    },
    mainEntityOfPage: {
      "@type": "WebPage",
      "@id": url,
    },
    url,
    image: `${url}/opengraph-image`,
    keywords: post.frontmatter.tags.join(", "),
  };

  const breadcrumbJsonLd = {
    "@context": "https://schema.org",
    "@type": "BreadcrumbList",
    itemListElement: [
      { "@type": "ListItem", position: 1, name: "Home", item: SITE.url },
      {
        "@type": "ListItem",
        position: 2,
        name: "Blog",
        item: `${SITE.url}/blog`,
      },
      {
        "@type": "ListItem",
        position: 3,
        name: post.frontmatter.title,
        item: url,
      },
    ],
  };

  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(articleJsonLd) }}
      />
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(breadcrumbJsonLd) }}
      />
      <Nav variant="docs" />

      <article className="blog-article">
        <header className="blog-article-head">
          <nav className="blog-crumbs" aria-label="Breadcrumb">
            <Link href="/blog">← All posts</Link>
          </nav>
          <h1 className="blog-article-title">{post.frontmatter.title}</h1>
          <p className="blog-article-lede">{post.frontmatter.description}</p>
          <div className="blog-article-meta">
            <span>
              By{" "}
              <a
                href={SITE.socials.github}
                target="_blank"
                rel="noreferrer"
                style={{ color: "var(--color-fg)" }}
              >
                {post.frontmatter.author}
              </a>
            </span>
            <span aria-hidden>·</span>
            <time dateTime={post.frontmatter.publishedAt}>
              {formatDate(post.frontmatter.publishedAt)}
            </time>
            <span aria-hidden>·</span>
            <span>{post.readingTime} min read</span>
          </div>
          <div className="blog-article-tags">
            {post.frontmatter.tags.map((t) => (
              <Link
                key={t}
                href={`/blog/tags/${tagToSlug(t)}`}
                className="blog-tag"
              >
                {t}
              </Link>
            ))}
          </div>
        </header>

        <div className="blog-article-body">
          <BlogMDX source={post.content} />
        </div>

        <footer className="blog-article-foot">
          <div className="blog-cta-card">
            <h3>Liked this post?</h3>
            <p>
              SQLRite is open source. Star it on GitHub, install via{" "}
              <code>cargo install sqlrite-engine</code>, or read the{" "}
              <Link href="/docs">docs</Link>.
            </p>
            <div className="cta-row">
              <a className="btn btn-primary" href={SITE.repo}>
                Star on GitHub
              </a>
              <Link className="btn" href="/docs">
                Read the docs
              </Link>
              <Link className="btn" href="/blog">
                More posts
              </Link>
            </div>
          </div>

          {(older || newer) && (
            <nav className="blog-pager" aria-label="Post navigation">
              {older ? (
                <Link
                  href={`/blog/${older.slug}`}
                  className="blog-pager-link"
                >
                  <span className="blog-pager-label">
                    <span aria-hidden>← </span>Older
                  </span>
                  <span className="blog-pager-title">
                    {older.frontmatter.title}
                  </span>
                </Link>
              ) : (
                <span />
              )}
              {newer ? (
                <Link
                  href={`/blog/${newer.slug}`}
                  className="blog-pager-link blog-pager-link-right"
                >
                  <span className="blog-pager-label">
                    Newer<span aria-hidden> →</span>
                  </span>
                  <span className="blog-pager-title">
                    {newer.frontmatter.title}
                  </span>
                </Link>
              ) : (
                <span />
              )}
            </nav>
          )}
        </footer>
      </article>
      <Footer />
    </>
  );
}
