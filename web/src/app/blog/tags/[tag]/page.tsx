import Link from "next/link";
import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { Footer } from "@/components/footer";
import { Nav } from "@/components/nav";
import { SITE } from "@/lib/site";
import {
  formatDate,
  getAllTags,
  getPostsByTag,
  slugToTag,
  tagToSlug,
} from "@/lib/blog";

type RouteParams = { params: Promise<{ tag: string }> };

export function generateStaticParams() {
  return getAllTags().map(({ tag }) => ({ tag: tagToSlug(tag) }));
}

export async function generateMetadata({
  params,
}: RouteParams): Promise<Metadata> {
  const { tag: tagSlug } = await params;
  const tag = slugToTag(tagSlug);
  if (!tag) return {};

  const title = `Posts tagged "${tag}"`;
  const description = `SQLRite blog posts tagged ${tag}.`;
  return {
    title,
    description,
    alternates: { canonical: `/blog/tags/${tagSlug}` },
    openGraph: {
      type: "website",
      siteName: "SQLRite",
      locale: "en_US",
      url: `${SITE.url}/blog/tags/${tagSlug}`,
      title: `${title} · SQLRite`,
      description,
    },
    twitter: {
      card: "summary_large_image",
      site: SITE.twitterHandle,
      creator: SITE.twitterHandle,
      title: `${title} · SQLRite`,
      description,
    },
  };
}

export default async function TagPage({ params }: RouteParams) {
  const { tag: tagSlug } = await params;
  const tag = slugToTag(tagSlug);
  if (!tag) notFound();

  const posts = getPostsByTag(tag);

  return (
    <>
      <Nav variant="docs" />
      <section id="blog-tag" className="no-border">
        <div className="wrap">
          <div className="sec-head">
            <span className="eyebrow tag">tag</span>
            <div>
              <h2>Posts tagged &ldquo;{tag}&rdquo;.</h2>
              <p className="sub">
                {posts.length} {posts.length === 1 ? "post" : "posts"}.{" "}
                <Link href="/blog" style={{ color: "var(--color-accent)" }}>
                  All posts →
                </Link>
              </p>
            </div>
          </div>
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
