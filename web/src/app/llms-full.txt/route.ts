import { buildLlmsFullTxt } from "@/lib/llms";

// Full-text docs + blog as one markdown file (https://llmstxt.org/).
// Generated at build time — the concatenation never runs per-request.
export const dynamic = "force-static";

export function GET() {
  return new Response(buildLlmsFullTxt(), {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
