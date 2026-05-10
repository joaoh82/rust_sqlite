import Link from "next/link";
import { formatDate, getAllPosts } from "@/lib/blog";
import { SITE } from "@/lib/site";

const MAX_FEATURED = 4;

export function Blog() {
  const posts = getAllPosts().slice(0, MAX_FEATURED);

  return (
    <section id="blog">
      <div className="wrap">
        <div className="sec-head">
          <span className="eyebrow tag">09 · written in public</span>
          <div>
            <h2>Read the blog.</h2>
            <p className="sub">
              SQLRite is a learning project as much as a database. Each phase is
              paired with a long-form post on the design choices behind it.
              {posts.length === 0 ? (
                <>
                  {" "}
                  Older essays still live on{" "}
                  <a
                    href={SITE.medium}
                    target="_blank"
                    rel="noreferrer"
                    style={{ color: "var(--color-accent)" }}
                  >
                    Medium
                  </a>
                  .
                </>
              ) : null}
            </p>
          </div>
        </div>
        <div className="sec-body" style={{ paddingTop: 32 }}>
          {posts.length > 0 ? (
            <>
              <div className="blog-list">
                {posts.map((p) => (
                  <Link className="blog-item" key={p.slug} href={`/blog/${p.slug}`}>
                    <span className="num">
                      {formatDate(p.frontmatter.publishedAt)}
                    </span>
                    <h3>{p.frontmatter.title}</h3>
                    <p className="dim" style={{ fontSize: 14 }}>
                      {p.frontmatter.description}
                    </p>
                    <span className="arrow">read post →</span>
                  </Link>
                ))}
              </div>
              <div
                style={{
                  marginTop: 28,
                  display: "flex",
                  gap: 12,
                  flexWrap: "wrap",
                }}
              >
                <Link className="btn btn-primary" href="/blog">
                  All posts →
                </Link>
                <a
                  className="btn"
                  href={SITE.medium}
                  target="_blank"
                  rel="noreferrer"
                >
                  Series on Medium
                </a>
              </div>
            </>
          ) : null}
        </div>
      </div>
    </section>
  );
}
