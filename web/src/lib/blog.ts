import fs from "node:fs";
import path from "node:path";
import { cache } from "react";
import matter from "gray-matter";

export type PostFrontmatter = {
  title: string;
  description: string;
  publishedAt: string;
  updatedAt?: string;
  author: string;
  tags: string[];
  ogImage?: string;
  primaryKeyword?: string;
};

export type Post = {
  slug: string;
  frontmatter: PostFrontmatter;
  content: string;
  readingTime: number;
};

const POSTS_DIR = path.join(process.cwd(), "content", "blog");

// Slugs come from filenames at build time, but the [slug] route is
// dynamic at runtime — validate before path-joining so an unknown
// slug can't probe the filesystem outside POSTS_DIR.
const SLUG_RE = /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/;

function isFrontmatter(value: Record<string, unknown>): value is PostFrontmatter {
  return (
    typeof value.title === "string" &&
    typeof value.description === "string" &&
    typeof value.publishedAt === "string" &&
    typeof value.author === "string" &&
    Array.isArray(value.tags) &&
    value.tags.every((t) => typeof t === "string")
  );
}

function estimateReadingTime(text: string): number {
  const words = text.trim().split(/\s+/).length;
  return Math.max(1, Math.round(words / 220));
}

function parseFile(slug: string, raw: string): Post {
  const { data, content } = matter(raw);
  if (!isFrontmatter(data)) {
    throw new Error(`Post ${slug} has invalid frontmatter`);
  }
  return {
    slug,
    frontmatter: data,
    content,
    readingTime: estimateReadingTime(content),
  };
}

export const getAllPostSlugs = cache((): string[] => {
  if (!fs.existsSync(POSTS_DIR)) return [];
  return fs
    .readdirSync(POSTS_DIR)
    .filter((f) => f.endsWith(".mdx"))
    .map((f) => f.replace(/\.mdx$/, ""));
});

export const getAllPosts = cache((): Post[] => {
  const slugs = getAllPostSlugs();
  const posts = slugs.map((slug) => {
    const filePath = path.join(POSTS_DIR, `${slug}.mdx`);
    return parseFile(slug, fs.readFileSync(filePath, "utf8"));
  });
  return posts.sort(
    (a, b) =>
      new Date(b.frontmatter.publishedAt).getTime() -
      new Date(a.frontmatter.publishedAt).getTime(),
  );
});

export function getPostBySlug(slug: string): Post | null {
  if (!SLUG_RE.test(slug)) return null;
  const filePath = path.join(POSTS_DIR, `${slug}.mdx`);
  try {
    return parseFile(slug, fs.readFileSync(filePath, "utf8"));
  } catch {
    return null;
  }
}

export const getAllTags = cache(
  (): { tag: string; count: number }[] => {
    const counts = new Map<string, number>();
    for (const post of getAllPosts()) {
      for (const tag of post.frontmatter.tags) {
        counts.set(tag, (counts.get(tag) ?? 0) + 1);
      }
    }
    return Array.from(counts.entries())
      .map(([tag, count]) => ({ tag, count }))
      .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
  },
);

export function getPostsByTag(tag: string): Post[] {
  return getAllPosts().filter((p) => p.frontmatter.tags.includes(tag));
}

export function tagToSlug(tag: string): string {
  return tag.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

export function slugToTag(slug: string): string | null {
  const tags = getAllTags().map(({ tag }) => tag);
  return tags.find((t) => tagToSlug(t) === slug) ?? null;
}

export function formatDate(iso: string): string {
  const d = new Date(iso);
  // Pin to UTC: bare-date frontmatter ("2026-04-08") parses as UTC
  // midnight, so a build server west of UTC would otherwise render
  // the previous day.
  return d.toLocaleDateString("en-US", {
    year: "numeric",
    month: "long",
    day: "numeric",
    timeZone: "UTC",
  });
}
