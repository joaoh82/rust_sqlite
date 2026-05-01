// Worker-thread localhost HTTP mock for test_ask.mjs.
//
// Why a worker, not the main event loop: napi-rs's `db.ask()` is a
// synchronous Rust call that blocks the JS thread for the duration
// of the underlying ureq POST. If the mock server were on the main
// event loop, its incoming-request callback couldn't run while
// db.ask() was blocking — classic deadlock (same shape as the
// Python GIL deadlock we fixed in 7g.4 by releasing the GIL during
// the call; here we side-step by hosting the server in a separate
// worker thread that has its own event loop).
//
// Protocol:
//   - On startup: post `{type: 'ready', baseUrl}` so the parent can
//     point AskConfig.baseUrl at us.
//   - On HTTP POST: capture the request (path + headers + parsed
//     JSON body) into a local var; reply with the canned status +
//     body from workerData.
//   - On `{type: 'stop'}` from parent: post `{type: 'captured',
//     captured: {...}}` then close the server. Parent calls
//     worker.terminate() once it sees the captured message.

import { createServer } from 'node:http';
import { parentPort, workerData } from 'node:worker_threads';

const status = workerData?.status ?? 200;
const body = workerData?.body ?? {};

let captured = null;

const server = createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const raw = Buffer.concat(chunks).toString('utf-8');
    let parsed = null;
    try {
      parsed = raw ? JSON.parse(raw) : null;
    } catch {
      parsed = raw;
    }
    captured = {
      path: req.url,
      method: req.method,
      // Lower-case header names for case-insensitive lookup; HTTP
      // header names are case-insensitive per RFC 7230 but we want
      // tests to be deterministic regardless of how Node decided to
      // present them.
      headers: Object.fromEntries(
        Object.entries(req.headers).map(([k, v]) => [k.toLowerCase(), v]),
      ),
      body: parsed,
    };
    res.statusCode = status;
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify(body));
  });
});

server.listen(0, '127.0.0.1', () => {
  const { port } = server.address();
  parentPort.postMessage({
    type: 'ready',
    baseUrl: `http://127.0.0.1:${port}`,
  });
});

parentPort.on('message', (msg) => {
  if (msg?.type === 'stop') {
    // Send the captured request back BEFORE we close — the parent's
    // stop() helper waits for this message to know the test can
    // assert on it.
    parentPort.postMessage({ type: 'captured', captured });
    server.close();
  }
});
