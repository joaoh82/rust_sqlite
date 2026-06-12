import { getDocsMarkdown } from "@/lib/llms";

// Raw-markdown variant of /docs for AI crawlers that prefer markdown over
// rendered HTML. Generated at build time.
export const dynamic = "force-static";

export function GET() {
  return new Response(getDocsMarkdown(), {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}
