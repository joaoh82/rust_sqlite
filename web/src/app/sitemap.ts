import type { MetadataRoute } from "next";
import { SITE } from "@/lib/site";

// Static route list. Add an entry whenever a new app-router page lands.
// Once the docs grow into an MDX-driven `/docs/[slug]` tree we'll switch this
// over to enumerate the MDX frontmatter; for now the surface is small enough
// to maintain by hand.
const ROUTES: Array<{
  path: string;
  changeFrequency: MetadataRoute.Sitemap[number]["changeFrequency"];
  priority: number;
}> = [
  { path: "/", changeFrequency: "weekly", priority: 1.0 },
  { path: "/docs", changeFrequency: "weekly", priority: 0.9 },
];

export default function sitemap(): MetadataRoute.Sitemap {
  const lastModified = new Date();
  return ROUTES.map(({ path, changeFrequency, priority }) => ({
    url: `${SITE.url}${path === "/" ? "" : path}`,
    lastModified,
    changeFrequency,
    priority,
  }));
}
