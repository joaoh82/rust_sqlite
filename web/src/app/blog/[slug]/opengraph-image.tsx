import { ImageResponse } from "next/og";
import { OG_CONTENT_TYPE, OG_SIZE, OgFrame } from "@/lib/og";
import { getAllPostSlugs, getPostBySlug } from "@/lib/blog";

// nodejs runtime is required because getPostBySlug reads the MDX
// from disk via fs. The route is statically prerendered at build via
// generateStaticParams, so the runtime choice doesn't affect cost
// at request time.
export const runtime = "nodejs";
export const alt = "SQLRite blog post";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export function generateStaticParams() {
  return getAllPostSlugs().map((slug) => ({ slug }));
}

export default async function OgImage({
  params,
}: {
  params: Promise<{ slug: string }>;
}) {
  const { slug } = await params;
  const post = getPostBySlug(slug);
  const title = post?.frontmatter.title ?? "SQLRite blog";
  const subtitle = post?.frontmatter.description;

  return new ImageResponse(
    <OgFrame eyebrow="sqlrite blog" title={title} subtitle={subtitle} />,
    { ...size },
  );
}
