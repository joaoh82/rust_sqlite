// Tiny zero-dependency Node server for the WASM Ask demo.
//
// Two responsibilities:
//   1. Serve the static files in this directory (so the browser can
//      load index.html + pkg/*.wasm + pkg/*.js without extra setup).
//   2. Proxy POST /api/llm/complete to api.anthropic.com, adding the
//      x-api-key header from the ANTHROPIC_API_KEY env var.
//
// **Why this lives in the example, not the SDK:** the WASM SDK
// deliberately doesn't ship a backend. Q9's whole point is that
// the API key lives in YOUR backend, so we show what the absolute-
// minimum "your backend" looks like. ~70 LOC, no dependencies.
//
// Usage:
//
//     # First-time setup — build the wasm package.
//     make build
//
//     # Then run THIS server (not `python -m http.server`):
//     export ANTHROPIC_API_KEY=sk-ant-…
//     node server.mjs
//
//     # → open http://localhost:8080/
//
// For production patterns on Cloudflare Workers / Vercel Edge /
// Deno Deploy / Firebase Functions / etc., see the worked examples
// in `docs/ask-backend-examples.md`. They're all the same shape:
// receive payload, add x-api-key, forward.

import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PORT = process.env.PORT || 8080;
const ROOT = resolve(fileURLToPath(import.meta.url), "..");
const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;

if (!ANTHROPIC_API_KEY) {
  console.warn(
    "[warn] ANTHROPIC_API_KEY is not set. Static files will serve, but " +
      "POST /api/llm/complete will return 500 for every request.\n" +
      "       Set the var and restart: export ANTHROPIC_API_KEY=sk-ant-…",
  );
}

// Minimal MIME table — enough for the WASM demo. Anything not
// listed falls back to application/octet-stream which the browser
// handles fine for downloads but won't auto-execute as a script.
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

const server = createServer(async (req, res) => {
  // ---------------------------------------------------------------
  // Route: /api/llm/complete (the Ask proxy)
  // ---------------------------------------------------------------
  if (req.method === "POST" && req.url === "/api/llm/complete") {
    if (!ANTHROPIC_API_KEY) {
      res.writeHead(500, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "ANTHROPIC_API_KEY not set on the server" }));
      return;
    }
    try {
      // Read the request body the browser POSTed. Cap the size so a
      // misbehaving client can't OOM the server with a giant payload —
      // 256 KiB is generous for an Ask request (system blocks +
      // schema dump + question; rarely cracks 100 KiB).
      const chunks = [];
      let size = 0;
      const MAX = 256 * 1024;
      for await (const chunk of req) {
        size += chunk.length;
        if (size > MAX) {
          res.writeHead(413, { "content-type": "application/json" });
          res.end(JSON.stringify({ error: "request body too large" }));
          return;
        }
        chunks.push(chunk);
      }
      const body = Buffer.concat(chunks).toString("utf-8");

      // Forward to Anthropic with our API key. The browser never
      // sees this header — that's the whole point of the proxy.
      const upstream = await fetch("https://api.anthropic.com/v1/messages", {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-api-key": ANTHROPIC_API_KEY,
          "anthropic-version": "2023-06-01",
        },
        body,
      });

      // Pipe the response body straight back. We pass through the
      // upstream status code so the browser sees Anthropic's 4xx /
      // 5xx as-is — `db.askParse` and the demo's error handling
      // know how to surface those.
      res.writeHead(upstream.status, { "content-type": "application/json" });
      res.end(await upstream.text());
    } catch (err) {
      res.writeHead(502, { "content-type": "application/json" });
      res.end(
        JSON.stringify({ error: `proxy failed to reach upstream: ${String(err)}` }),
      );
    }
    return;
  }

  // ---------------------------------------------------------------
  // Static files (everything else)
  // ---------------------------------------------------------------
  if (req.method !== "GET" && req.method !== "HEAD") {
    res.writeHead(405).end();
    return;
  }

  // Map / to /index.html for the demo's root URL.
  const url = req.url === "/" ? "/index.html" : req.url;

  // Resolve + sandbox: never serve outside ROOT, even if the URL
  // contains `..`. `normalize` collapses traversals; the
  // startsWith check confirms the result is still inside ROOT.
  const safePath = normalize(join(ROOT, url));
  if (!safePath.startsWith(ROOT)) {
    res.writeHead(403).end();
    return;
  }

  try {
    const info = await stat(safePath);
    if (info.isDirectory()) {
      res.writeHead(403).end();
      return;
    }
    const data = await readFile(safePath);
    const mime = MIME[extname(safePath).toLowerCase()] || "application/octet-stream";
    res.writeHead(200, {
      "content-type": mime,
      "content-length": data.length,
      // Tell browsers the WASM module is fine to compile + execute.
      // Without this the browser's "wasm-strict" mode (Firefox, some
      // Chrome flags) refuses to instantiate.
      "cross-origin-resource-policy": "same-origin",
    });
    res.end(data);
  } catch {
    res.writeHead(404).end("not found");
  }
});

server.listen(PORT, () => {
  console.log(`SQLRite WASM demo:   http://localhost:${PORT}/`);
  console.log(`Ask proxy endpoint:  POST http://localhost:${PORT}/api/llm/complete`);
  if (ANTHROPIC_API_KEY) {
    console.log("ANTHROPIC_API_KEY:   detected (proxy will forward to Anthropic)");
  } else {
    console.log("ANTHROPIC_API_KEY:   NOT SET — Ask proxy will 500 until you set it");
  }
});
