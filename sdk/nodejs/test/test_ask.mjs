// Phase 7g.5 — tests for the natural-language → SQL surface.
//
// Run after `npm run build` produces sqlrite.<platform>-<arch>.node:
//
//     npm test
//
// (`test_ask.mjs` is picked up by the same `node --test test/`
// invocation as `test.mjs` — see package.json's test script.)
//
// Three layers covered, mirroring the Python SDK's test_ask.py:
//
// 1. **AskConfig** — option-object construction, defaults, getters,
//    fromEnv, toString deliberately omits the API key value.
// 2. **db.ask() error paths** — missing API key surfaces a clean
//    Error; closed database rejects.
// 3. **db.ask() / db.askRun() happy path** against a localhost HTTP
//    mock — the mock runs in a worker_thread to bypass Node's
//    main-event-loop deadlock that ureq's blocking sync POST would
//    otherwise create. Same shape as the Python SDK's mock; same
//    insight (napi-rs holds the JS thread for the duration of the
//    Rust call, so an in-event-loop server can't respond).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Worker } from 'node:worker_threads';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { Database, AskConfig } from '../index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------------------
// Mock server bootstrap
//
// Spawned in a worker_thread so the main event loop is free to be
// blocked by the synchronous napi `db.ask()` call. The worker
// listens on an OS-assigned port (port 0), reports it back via
// postMessage, then serves one canned response per request and
// records what it received.
//
// Each test gets its own worker — keeps captured-request state
// per-test and avoids server-restart races between tests.

function startMockServer({ status = 200, body } = {}) {
  return new Promise((resolve, reject) => {
    const worker = new Worker(join(__dirname, 'mock-llm-server.mjs'), {
      // `body ?? SUCCESS_BODY` so a test that doesn't pass body
      // gets the standard success payload — important because the
      // worker can't see SUCCESS_BODY (different module scope).
      workerData: { status, body: body ?? SUCCESS_BODY },
    });
    worker.once('message', (msg) => {
      if (msg.type === 'ready') {
        // Wrap with a `stop()` that asks the worker to drain its
        // last captured request, then terminate.
        const stop = () =>
          new Promise((res) => {
            const onCaptured = (m) => {
              if (m.type === 'captured') {
                worker.removeListener('message', onCaptured);
                worker.terminate().then(() => res(m.captured));
              }
            };
            worker.on('message', onCaptured);
            worker.postMessage({ type: 'stop' });
          });
        resolve({ baseUrl: msg.baseUrl, stop });
      } else {
        reject(new Error(`unexpected worker message: ${JSON.stringify(msg)}`));
      }
    });
    worker.once('error', reject);
  });
}

const SUCCESS_BODY = {
  id: 'msg_test',
  type: 'message',
  role: 'assistant',
  model: 'claude-sonnet-4-6',
  content: [
    {
      type: 'text',
      text: '{"sql": "SELECT id, name FROM users", "explanation": "lists users"}',
    },
  ],
  stop_reason: 'end_turn',
  usage: {
    input_tokens: 1234,
    output_tokens: 56,
    cache_creation_input_tokens: 1000,
    cache_read_input_tokens: 0,
  },
};

// ---------------------------------------------------------------------------
// AskConfig construction + defaults

test('AskConfig defaults match the Rust side', () => {
  const cfg = new AskConfig();
  assert.equal(cfg.provider, 'anthropic');
  assert.equal(cfg.model, 'claude-sonnet-4-6');
  assert.equal(cfg.maxTokens, 1024);
  assert.equal(cfg.cacheTtl, '5m');
  assert.equal(cfg.hasApiKey, false);
});

test('AskConfig accepts an option object', () => {
  const cfg = new AskConfig({
    apiKey: 'sk-ant-test',
    model: 'claude-haiku-4-5',
    maxTokens: 2048,
    cacheTtl: '1h',
  });
  assert.equal(cfg.hasApiKey, true);
  assert.equal(cfg.model, 'claude-haiku-4-5');
  assert.equal(cfg.maxTokens, 2048);
  assert.equal(cfg.cacheTtl, '1h');
});

test('AskConfig cacheTtl accepts aliases', () => {
  for (const raw of ['5m', '5min', '5MIN', '5minutes']) {
    assert.equal(new AskConfig({ cacheTtl: raw }).cacheTtl, '5m');
  }
  for (const raw of ['1h', '1hr', '1HOUR']) {
    assert.equal(new AskConfig({ cacheTtl: raw }).cacheTtl, '1h');
  }
  for (const raw of ['off', 'none', 'DISABLED']) {
    assert.equal(new AskConfig({ cacheTtl: raw }).cacheTtl, 'off');
  }
});

test('AskConfig rejects unknown provider', () => {
  assert.throws(
    () => new AskConfig({ provider: 'openai' }),
    /unknown provider/,
  );
});

test('AskConfig rejects unknown cacheTtl', () => {
  assert.throws(
    () => new AskConfig({ cacheTtl: 'forever' }),
    /unknown cacheTtl/,
  );
});

test('AskConfig.toString does NOT leak the API key value', () => {
  const cfg = new AskConfig({ apiKey: 'sk-ant-supersecret' });
  const s = cfg.toString();
  assert.equal(s.includes('sk-ant-supersecret'), false);
  assert.equal(s.includes('<set>'), true);

  const cfg2 = new AskConfig();
  assert.equal(cfg2.toString().includes('null'), true);
});

test('AskConfig empty apiKey treated as not-set', () => {
  // Matches the Rust from_env behavior: empty string → None.
  const cfg = new AskConfig({ apiKey: '' });
  assert.equal(cfg.hasApiKey, false);
});

// ---------------------------------------------------------------------------
// AskConfig.fromEnv — env-var snapshot/restore around each test.
// node:test doesn't have an autouse fixture; we just save+restore
// inline.

function withEnvIsolation(fn) {
  const keys = [
    'SQLRITE_LLM_PROVIDER',
    'SQLRITE_LLM_API_KEY',
    'SQLRITE_LLM_MODEL',
    'SQLRITE_LLM_MAX_TOKENS',
    'SQLRITE_LLM_CACHE_TTL',
  ];
  const before = Object.fromEntries(keys.map((k) => [k, process.env[k]]));
  for (const k of keys) delete process.env[k];
  try {
    fn();
  } finally {
    for (const k of keys) {
      if (before[k] === undefined) {
        delete process.env[k];
      } else {
        process.env[k] = before[k];
      }
    }
  }
}

test('AskConfig.fromEnv with no env returns defaults + no key', () => {
  withEnvIsolation(() => {
    const cfg = AskConfig.fromEnv();
    assert.equal(cfg.provider, 'anthropic');
    assert.equal(cfg.model, 'claude-sonnet-4-6');
    assert.equal(cfg.hasApiKey, false);
    assert.equal(cfg.cacheTtl, '5m');
  });
});

test('AskConfig.fromEnv reads SQLRITE_LLM_* vars', () => {
  withEnvIsolation(() => {
    process.env.SQLRITE_LLM_API_KEY = 'env-key';
    process.env.SQLRITE_LLM_MODEL = 'claude-haiku-4-5';
    process.env.SQLRITE_LLM_MAX_TOKENS = '512';
    process.env.SQLRITE_LLM_CACHE_TTL = '1h';

    const cfg = AskConfig.fromEnv();
    assert.equal(cfg.hasApiKey, true);
    assert.equal(cfg.model, 'claude-haiku-4-5');
    assert.equal(cfg.maxTokens, 512);
    assert.equal(cfg.cacheTtl, '1h');
  });
});

test('AskConfig.fromEnv rejects invalid SQLRITE_LLM_MAX_TOKENS', () => {
  withEnvIsolation(() => {
    process.env.SQLRITE_LLM_MAX_TOKENS = 'not-an-int';
    assert.throws(() => AskConfig.fromEnv(), /MAX_TOKENS/);
  });
});

// ---------------------------------------------------------------------------
// db.ask() error paths (no LLM call needed)

test('db.ask without API key surfaces a clean error', () => {
  withEnvIsolation(() => {
    const db = new Database(':memory:');
    try {
      assert.throws(() => db.ask('How many users?'), /missing API key/);
    } finally {
      db.close();
    }
  });
});

test('db.ask on a closed database rejects', () => {
  const db = new Database(':memory:');
  db.close();
  assert.throws(() => db.ask('anything'), /closed/);
});

test('setAskConfig with no key then ask still raises', () => {
  withEnvIsolation(() => {
    const db = new Database(':memory:');
    try {
      const cfg = new AskConfig(); // no apiKey
      db.setAskConfig(cfg);
      assert.throws(() => db.ask('anything'), /missing API key/);
    } finally {
      db.close();
    }
  });
});

// ---------------------------------------------------------------------------
// db.ask() happy path against the worker-thread localhost mock

test('db.ask returns parsed AskResponse', async () => {
  const db = new Database(':memory:');
  try {
    db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)');
    const mock = await startMockServer();
    let resp;
    try {
      const cfg = new AskConfig({ apiKey: 'test-key', baseUrl: mock.baseUrl });
      resp = db.ask('How many users are over 30?', cfg);
    } finally {
      await mock.stop();
    }
    assert.equal(resp.sql, 'SELECT id, name FROM users');
    assert.equal(resp.explanation, 'lists users');
    assert.equal(resp.usage.inputTokens, 1234);
    assert.equal(resp.usage.cacheCreationInputTokens, 1000);
    assert.equal(resp.usage.cacheReadInputTokens, 0);
  } finally {
    db.close();
  }
});

test('db.ask sends schema + cache-control + auth headers', async () => {
  const db = new Database(':memory:');
  try {
    db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)');
    const mock = await startMockServer();
    try {
      const cfg = new AskConfig({ apiKey: 'test-key', baseUrl: mock.baseUrl });
      db.ask('list users', cfg);
    } finally {
      var captured = await mock.stop();
    }
    assert.equal(captured.body.model, 'claude-sonnet-4-6');
    assert.equal(captured.body.max_tokens, 1024);
    assert.equal(captured.body.messages[0].role, 'user');
    assert.equal(captured.body.messages[0].content, 'list users');
    assert.match(captured.body.system[1].text, /CREATE TABLE users/);
    assert.equal(captured.body.system[1].cache_control.type, 'ephemeral');
    // Headers (lowercased by the worker for case-insensitive lookup).
    assert.equal(captured.headers['x-api-key'], 'test-key');
    assert.equal(captured.headers['anthropic-version'], '2023-06-01');
  } finally {
    db.close();
  }
});

test('setAskConfig persists across calls', async () => {
  const db = new Database(':memory:');
  try {
    db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY)');
    const mock = await startMockServer();
    let r1, r2;
    try {
      const cfg = new AskConfig({ apiKey: 'persisted', baseUrl: mock.baseUrl });
      db.setAskConfig(cfg);
      r1 = db.ask('first');
      r2 = db.ask('second');
    } finally {
      await mock.stop();
    }
    assert.equal(r1.sql, 'SELECT id, name FROM users');
    assert.equal(r2.sql, 'SELECT id, name FROM users');
  } finally {
    db.close();
  }
});

test('db.askRun executes the generated SQL and returns rows', async () => {
  const db = new Database(':memory:');
  try {
    db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)');
    db.exec("INSERT INTO users (name, age) VALUES ('alice', 30)");
    db.exec("INSERT INTO users (name, age) VALUES ('bob', 25)");
    const mock = await startMockServer();
    let rows;
    try {
      const cfg = new AskConfig({ apiKey: 'test-key', baseUrl: mock.baseUrl });
      rows = db.askRun('list users', cfg);
    } finally {
      await mock.stop();
    }
    assert.equal(rows.length, 2);
    const names = rows.map((r) => r.name).sort();
    assert.deepEqual(names, ['alice', 'bob']);
  } finally {
    db.close();
  }
});

test('db.askRun on empty SQL response throws with model explanation', async () => {
  const declineBody = { ...SUCCESS_BODY };
  declineBody.content = [
    {
      type: 'text',
      text: '{"sql": "", "explanation": "schema lacks a widgets table"}',
    },
  ];
  const db = new Database(':memory:');
  try {
    const mock = await startMockServer({ body: declineBody });
    try {
      const cfg = new AskConfig({ apiKey: 'test-key', baseUrl: mock.baseUrl });
      assert.throws(
        () => db.askRun('how many widgets?', cfg),
        /declined.*widgets table/,
      );
    } finally {
      await mock.stop();
    }
  } finally {
    db.close();
  }
});

test('API 4xx response surfaces as JS Error with the structured message', async () => {
  const db = new Database(':memory:');
  try {
    const mock = await startMockServer({
      status: 400,
      body: {
        type: 'error',
        error: {
          type: 'invalid_request_error',
          message: 'max_tokens too large',
        },
      },
    });
    try {
      const cfg = new AskConfig({ apiKey: 'test-key', baseUrl: mock.baseUrl });
      assert.throws(
        () => db.ask('anything', cfg),
        /400.*invalid_request_error.*max_tokens too large/,
      );
    } finally {
      await mock.stop();
    }
  } finally {
    db.close();
  }
});
