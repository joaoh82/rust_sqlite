import { buildLlmsTxt } from "@/lib/llms";

// Curated llms.txt index (https://llmstxt.org/). Generated at build time.
export const dynamic = "force-static";

export function GET() {
  return new Response(buildLlmsTxt(), {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
