# Backend proxy templates for the WASM SDK's `ask` flow

The WASM SDK builds the LLM-API request body in the browser, but [doesn't make the HTTP call itself](ask.md#wasm-sdk-the-different-one) — CORS plus API-key exposure rule that out. Instead, the browser POSTs the request body to a small backend you control, which adds the API key and forwards to Anthropic.

This page is a copy-paste catalog of that backend on the platforms most people reach for. Every template:

- Accepts a `POST` with the JSON payload `db.askPrompt(...)` produces.
- Reads `ANTHROPIC_API_KEY` from the platform's secrets / env mechanism.
- Forwards to `https://api.anthropic.com/v1/messages` with the right headers.
- Pipes the upstream status + body straight back to the browser, so `db.askParse()` sees Anthropic's exact response shape.

Pick whichever platform you're already on; the shape is identical.

> **Security recap.** The whole point of the proxy is to keep the API key on the server. Never echo the key into the response body, never log full request/response bodies in production (the user's question may contain PII), and lock the route down with whatever auth your app already uses (cookie session, signed origin check, etc.) — these templates are intentionally unauthenticated so they stay readable, but a public unauthenticated proxy is a free Anthropic credit faucet for anyone who finds the URL.

---

## Table of contents

- [Cloudflare Workers](#cloudflare-workers)
- [Vercel Edge Functions](#vercel-edge-functions)
- [Deno Deploy](#deno-deploy)
- [Firebase Cloud Functions (v2)](#firebase-cloud-functions-v2)
- [AWS Lambda (Function URLs)](#aws-lambda-function-urls)
- [Node + Express (self-hosted)](#node--express-self-hosted)
- [Pure Node (zero-dep, the demo template)](#pure-node-zero-dep-the-demo-template)
- [Calling from a different origin (CORS)](#calling-from-a-different-origin-cors)
- [Locking down the proxy](#locking-down-the-proxy)

---

## Cloudflare Workers

**Where the key lives:** `wrangler secret put ANTHROPIC_API_KEY` (encrypted, never in the dashboard or git).

**`wrangler.toml`:**

```toml
name = "sqlrite-ask-proxy"
main = "src/worker.js"
compatibility_date = "2024-12-01"
```

**`src/worker.js`:**

```js
export default {
  async fetch(request, env) {
    if (request.method !== "POST") {
      return new Response("method not allowed", { status: 405 });
    }
    const upstream = await fetch("https://api.anthropic.com/v1/messages", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-api-key": env.ANTHROPIC_API_KEY,
        "anthropic-version": "2023-06-01",
      },
      body: request.body,
    });
    // Pass through the upstream status + body verbatim so the browser's
    // db.askParse() sees Anthropic's exact response (and 4xx/5xx errors
    // surface as-is rather than being remapped).
    return new Response(upstream.body, {
      status: upstream.status,
      headers: { "content-type": "application/json" },
    });
  },
};
```

Deploy:

```sh
wrangler secret put ANTHROPIC_API_KEY      # paste your sk-ant-…
wrangler deploy
```

In your WASM page, point `fetch` at the Worker URL: `fetch('https://sqlrite-ask-proxy.<you>.workers.dev', …)`. If the Worker is on a different origin from the WASM page, see [Calling from a different origin (CORS)](#calling-from-a-different-origin-cors).

---

## Vercel Edge Functions

**Where the key lives:** Vercel dashboard → Project → Settings → Environment Variables → `ANTHROPIC_API_KEY` (Production / Preview / Development as needed).

**`api/llm/complete.js`** (or `app/api/llm/complete/route.js` on App Router):

```js
export const config = { runtime: "edge" };

export default async function handler(request) {
  if (request.method !== "POST") {
    return new Response("method not allowed", { status: 405 });
  }
  const upstream = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-api-key": process.env.ANTHROPIC_API_KEY,
      "anthropic-version": "2023-06-01",
    },
    body: request.body,
  });
  return new Response(upstream.body, {
    status: upstream.status,
    headers: { "content-type": "application/json" },
  });
}
```

App Router variant (`app/api/llm/complete/route.js`):

```js
export const runtime = "edge";

export async function POST(request) {
  const upstream = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-api-key": process.env.ANTHROPIC_API_KEY,
      "anthropic-version": "2023-06-01",
    },
    body: request.body,
  });
  return new Response(upstream.body, {
    status: upstream.status,
    headers: { "content-type": "application/json" },
  });
}
```

The browser then `fetch('/api/llm/complete', …)` — no CORS dance because the page and the function share the origin.

---

## Deno Deploy

**Where the key lives:** Deno Deploy dashboard → Project → Settings → Environment Variables → `ANTHROPIC_API_KEY`.

**`main.ts`:**

```ts
Deno.serve(async (request) => {
  if (request.method !== "POST") {
    return new Response("method not allowed", { status: 405 });
  }
  const upstream = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-api-key": Deno.env.get("ANTHROPIC_API_KEY") ?? "",
      "anthropic-version": "2023-06-01",
    },
    body: request.body,
  });
  return new Response(upstream.body, {
    status: upstream.status,
    headers: { "content-type": "application/json" },
  });
});
```

Deploy:

```sh
deployctl deploy --project=sqlrite-ask-proxy main.ts
```

Local development:

```sh
ANTHROPIC_API_KEY=sk-ant-… deno run --allow-net --allow-env main.ts
```

---

## Firebase Cloud Functions (v2)

**Where the key lives:** Firebase Secret Manager — `firebase functions:secrets:set ANTHROPIC_API_KEY` (NOT `functions.config()`, which is deprecated for v2).

**`functions/index.js`:**

```js
import { onRequest } from "firebase-functions/v2/https";
import { defineSecret } from "firebase-functions/params";

const anthropicKey = defineSecret("ANTHROPIC_API_KEY");

export const llmComplete = onRequest(
  { secrets: [anthropicKey], cors: true, region: "us-central1" },
  async (request, response) => {
    if (request.method !== "POST") {
      response.status(405).send("method not allowed");
      return;
    }
    const upstream = await fetch("https://api.anthropic.com/v1/messages", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-api-key": anthropicKey.value(),
        "anthropic-version": "2023-06-01",
      },
      // request.rawBody is a Buffer; pass it through verbatim so we
      // don't re-serialize and accidentally change byte ordering
      // (which would invalidate Anthropic's prompt cache).
      body: request.rawBody,
    });
    response.status(upstream.status);
    response.set("content-type", "application/json");
    response.send(await upstream.text());
  },
);
```

**`functions/package.json`:**

```json
{
  "type": "module",
  "engines": { "node": "20" },
  "dependencies": { "firebase-functions": "^5.0.0" }
}
```

Deploy:

```sh
firebase functions:secrets:set ANTHROPIC_API_KEY
firebase deploy --only functions:llmComplete
```

The function URL is printed at the end of `firebase deploy` — point your browser `fetch` at it.

---

## AWS Lambda (Function URLs)

A Lambda Function URL gives you an HTTPS endpoint without dragging API Gateway in. **Where the key lives:** Lambda Console → Configuration → Environment variables → `ANTHROPIC_API_KEY` (and consider switching to AWS Secrets Manager for production).

**`index.mjs`** (Node 20+ runtime):

```js
export const handler = async (event) => {
  if (event.requestContext?.http?.method !== "POST") {
    return { statusCode: 405, body: "method not allowed" };
  }
  // Function URLs base64-encode the body when it's "binary"; for JSON
  // POSTs Lambda hands it through as a string. Handle both.
  const body = event.isBase64Encoded
    ? Buffer.from(event.body, "base64").toString("utf-8")
    : event.body;

  const upstream = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-api-key": process.env.ANTHROPIC_API_KEY,
      "anthropic-version": "2023-06-01",
    },
    body,
  });
  return {
    statusCode: upstream.status,
    headers: { "content-type": "application/json" },
    body: await upstream.text(),
  };
};
```

Set the Function URL auth type to `NONE` (or wire it behind your own auth — see [Locking down the proxy](#locking-down-the-proxy)). Browser calls the printed URL directly; if the Lambda lives on a different origin from your WASM page you'll need to enable CORS on the Function URL config.

---

## Node + Express (self-hosted)

For a long-running Node server (Render, Fly.io, your own VPS):

```js
import express from "express";

const app = express();
app.use(express.json({ limit: "256kb" }));

app.post("/api/llm/complete", async (req, res) => {
  const upstream = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-api-key": process.env.ANTHROPIC_API_KEY,
      "anthropic-version": "2023-06-01",
    },
    body: JSON.stringify(req.body),
  });
  res.status(upstream.status);
  res.set("content-type", "application/json");
  res.send(await upstream.text());
});

app.listen(3000, () => console.log("Ask proxy on :3000"));
```

Run:

```sh
ANTHROPIC_API_KEY=sk-ant-… node server.js
```

---

## Pure Node (zero-dep, the demo template)

The runnable [`examples/wasm/server.mjs`](../examples/wasm/server.mjs) is exactly this — no npm install, ~70 LOC. Reproduced here for completeness:

```js
import { createServer } from "node:http";

const PORT = process.env.PORT || 8080;
const KEY = process.env.ANTHROPIC_API_KEY;

createServer(async (req, res) => {
  if (req.method !== "POST" || req.url !== "/api/llm/complete") {
    res.writeHead(404).end();
    return;
  }
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  const body = Buffer.concat(chunks).toString("utf-8");

  const upstream = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-api-key": KEY,
      "anthropic-version": "2023-06-01",
    },
    body,
  });
  res.writeHead(upstream.status, { "content-type": "application/json" });
  res.end(await upstream.text());
}).listen(PORT, () => console.log(`Ask proxy on :${PORT}`));
```

Run:

```sh
ANTHROPIC_API_KEY=sk-ant-… node server.mjs
```

The full [`examples/wasm/server.mjs`](../examples/wasm/server.mjs) also serves the static demo (HTML + WASM) on the same port and adds a 256 KiB body cap + path sandboxing — worth a look as a "minimum production-shaped" template.

---

## Calling from a different origin (CORS)

When the WASM page and the proxy are on different origins (page on `app.example.com`, Worker on `proxy.example.workers.dev`), the browser will reject the response unless the proxy returns the right CORS headers.

Add this to any of the templates above:

```js
// At the start of the handler:
if (request.method === "OPTIONS") {
  return new Response(null, {
    status: 204,
    headers: {
      "access-control-allow-origin": "https://app.example.com",
      "access-control-allow-methods": "POST, OPTIONS",
      "access-control-allow-headers": "content-type",
      "access-control-max-age": "86400",
    },
  });
}

// After building the upstream response:
return new Response(upstream.body, {
  status: upstream.status,
  headers: {
    "content-type": "application/json",
    "access-control-allow-origin": "https://app.example.com",
  },
});
```

**Don't use `*` for `access-control-allow-origin` on the proxy in production** — it lets any site on the internet burn your API quota by hitting your endpoint from a malicious page. Allow-list your actual origins.

---

## Locking down the proxy

The templates above are deliberately unauthenticated to keep the shape readable. Before pointing public traffic at one, add at least one of:

1. **Same-origin only** — host the proxy under your app's domain (Vercel, Next.js API routes, the Pure Node demo) so the browser only ever calls it from your page. Combined with `SameSite=Strict` cookies and an `Origin` header check, this is enough for most internal-tools use cases.

2. **Session cookie / auth header check** — gate the route on whatever your app already uses for the rest of its API. The proxy is just another API route; treat it accordingly.

3. **Rate limit per session/IP** — the proxy can hit Anthropic for any caller. Cloudflare Workers + Cloudflare's rate-limiting rules, Vercel + Upstash rate-limit, or a simple in-memory token bucket on a long-lived server will keep cost bounded if abuse happens.

4. **Origin allow-listing** — check `request.headers.get("origin")` against an explicit allow-list and reject everything else. Doesn't replace auth (an attacker can spoof Origin from a non-browser client) but raises the bar for casual abuse.

5. **Logging hygiene** — the request body contains the user's natural-language question, which may include PII. Log lengths and timing, not bodies.

For ~100 LOC of "production-shaped" Cloudflare Worker that bundles all of these in, see the [Cloudflare Pages + Functions tutorial](https://developers.cloudflare.com/pages/functions/) or the analogous Vercel docs.

---

## See also

- [`docs/ask.md`](ask.md) — the canonical Ask reference (architecture, all SDKs, defaults, errors, prompt caching, security)
- [`sdk/wasm/README.md`](../sdk/wasm/README.md) — WASM SDK API reference, including `askPrompt` / `askParse` shapes
- [`examples/wasm/`](../examples/wasm/) — runnable end-to-end demo: WASM in a browser tab + zero-dep Node proxy
