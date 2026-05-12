import { NextResponse, type NextRequest } from "next/server";
import { postHogMiddleware } from "@posthog/next";

// Gate on the env var so preview deploys that haven't configured the secret
// still run middleware as a no-op. `postHogMiddleware` reads the API key from
// `NEXT_PUBLIC_POSTHOG_KEY` itself; we just avoid wiring it up when unset.
const posthog = process.env.NEXT_PUBLIC_POSTHOG_KEY
  ? postHogMiddleware({ proxy: true })
  : null;

export default function middleware(request: NextRequest) {
  if (posthog) {
    return posthog(request);
  }
  return NextResponse.next();
}

export const config = {
  // Matchers OR together. We need the middleware to run on:
  //   1. `/ingest/*` — the PostHog proxy paths (e.g. /ingest/static/array.js,
  //      /ingest/e/, /ingest/decide/). These contain file extensions so the
  //      generic pattern below would skip them.
  //   2. Page routes — but NOT Next internals, the favicon, any path with a
  //      file extension (sitemap, robots, rss, images, fonts…), or the
  //      dynamic OG/Twitter image metadata routes. That keeps Vercel
  //      middleware invocations bounded to actual page navigations.
  matcher: [
    "/ingest/:path*",
    "/((?!_next/static|_next/image|favicon\\.ico|.*\\..*|.*opengraph-image.*|.*twitter-image.*).*)",
  ],
};
