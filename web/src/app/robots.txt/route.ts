import { SITE } from "@/lib/site";

// Hand-rolled robots.txt (replaces the typed `robots.ts` metadata route):
// Next's MetadataRoute.Robots can only emit rules + sitemap, and we also
// want to point AI crawlers at the llms.txt surfaces.
export const dynamic = "force-static";

const BODY = `User-Agent: *
Allow: /

Sitemap: ${SITE.url}/sitemap.xml
Host: ${SITE.url}

# LLM-friendly surfaces (https://llmstxt.org/)
# Curated index:  ${SITE.url}/llms.txt
# Full docs text: ${SITE.url}/llms-full.txt
`;

export function GET() {
  return new Response(BODY, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
