import test from 'node:test';
import assert from 'node:assert/strict';

import { makeHashEmbedder, makeOpenAIEmbedder, makeEmbedder } from '../src/embeddings.mjs';

test('hash embedder — deterministic, unit-norm, requested dim', async () => {
  const emb = makeHashEmbedder(384);
  const a = await emb.embed('rust embedded database');
  const b = await emb.embed('rust embedded database');
  assert.equal(a.length, 384);
  assert.deepEqual(a, b);
  const norm = Math.sqrt(a.reduce((s, x) => s + x * x, 0));
  assert.ok(Math.abs(norm - 1) < 1e-9 || norm === 0, `unit norm, got ${norm}`);
});

test('hash embedder — different inputs produce different vectors', async () => {
  const emb = makeHashEmbedder(64);
  const a = await emb.embed('alpha beta gamma');
  const b = await emb.embed('delta epsilon zeta');
  assert.notDeepEqual(a, b);
});

test('hash embedder — empty text returns zero vector', async () => {
  const emb = makeHashEmbedder(32);
  const a = await emb.embed('');
  assert.equal(a.length, 32);
  assert.ok(a.every((x) => x === 0));
});

test('makeEmbedder defaults to hash', () => {
  const emb = makeEmbedder({ dim: 16 });
  assert.equal(emb.name, 'hash');
  assert.equal(emb.dim, 16);
});

test('makeEmbedder openai without API key throws clear error', () => {
  const prev = process.env.OPENAI_API_KEY;
  delete process.env.OPENAI_API_KEY;
  try {
    assert.throws(
      () => makeEmbedder({ kind: 'openai', dim: 384 }),
      /OPENAI_API_KEY/,
    );
  } finally {
    if (prev !== undefined) process.env.OPENAI_API_KEY = prev;
  }
});

test('makeEmbedder unknown kind throws', () => {
  assert.throws(() => makeEmbedder({ kind: 'word2vec', dim: 8 }), /unknown embedder/);
});

test('openai embedder talks to a mocked fetch and validates shape', async () => {
  const calls = [];
  const fakeFetch = async (url, init) => {
    calls.push({ url, init });
    return new Response(
      JSON.stringify({ data: [{ embedding: new Array(8).fill(0.5) }] }),
      { status: 200, headers: { 'content-type': 'application/json' } },
    );
  };
  const emb = makeOpenAIEmbedder({
    apiKey: 'sk-test',
    model: 'text-embedding-3-small',
    dim: 8,
    fetchFn: fakeFetch,
  });
  const v = await emb.embed('hello world');
  assert.equal(v.length, 8);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, 'https://api.openai.com/v1/embeddings');
  const body = JSON.parse(calls[0].init.body);
  assert.equal(body.model, 'text-embedding-3-small');
  assert.equal(body.dimensions, 8);
  assert.equal(body.input, 'hello world');
  assert.match(calls[0].init.headers.authorization, /^Bearer /);
});

test('openai embedder surfaces API errors', async () => {
  const fakeFetch = async () =>
    new Response('rate limited', { status: 429 });
  const emb = makeOpenAIEmbedder({
    apiKey: 'sk-test',
    model: 'm',
    dim: 4,
    fetchFn: fakeFetch,
  });
  await assert.rejects(emb.embed('x'), /OpenAI embeddings API error 429/);
});

test('openai embedder rejects wrong-dim responses', async () => {
  const fakeFetch = async () =>
    new Response(JSON.stringify({ data: [{ embedding: [1, 2, 3] }] }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  const emb = makeOpenAIEmbedder({
    apiKey: 'sk-test',
    model: 'm',
    dim: 8,
    fetchFn: fakeFetch,
  });
  await assert.rejects(emb.embed('x'), /returned 3 dims, expected 8/);
});
