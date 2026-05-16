// Embedding-provider abstractions.
//
// Two providers:
//
//   1. `hash` (default, offline) — a token-bag hash embedder that
//      lets users run the whole pipeline without an API key.
//      Quality is bag-of-words-ish; good for demos and tests, not
//      for production RAG.
//
//   2. `openai` — `text-embedding-3-small`. Pinned to the `dimensions`
//      override so we stay at 384 dims for compatibility with the
//      schema (and for parity with the python-agent example).
//
// All providers share the same surface: `await embed(text)` returns
// a `number[]` of `provider.dim` items.

import { DEFAULT_EMBEDDING_DIM } from './config.mjs';

/**
 * @typedef {object} Embedder
 * @property {string} name
 * @property {number} dim
 * @property {(text: string) => Promise<number[]>} embed
 */

/**
 * Build an embedder by name. Throws if the configuration is invalid
 * (e.g. `openai` without `OPENAI_API_KEY`).
 *
 * @param {{ kind?: string, dim?: number, model?: string, apiKey?: string, fetchFn?: typeof fetch }} opts
 * @returns {Embedder}
 */
export function makeEmbedder(opts = {}) {
  const kind = opts.kind ?? process.env.SQLRITE_NOTES_EMBEDDER ?? 'hash';
  const dim = opts.dim ?? DEFAULT_EMBEDDING_DIM;
  if (kind === 'hash') return makeHashEmbedder(dim);
  if (kind === 'openai') {
    const apiKey = opts.apiKey ?? process.env.OPENAI_API_KEY;
    if (!apiKey) {
      throw new Error(
        'openai embedder: set OPENAI_API_KEY (or pass --embedder hash to run offline).',
      );
    }
    const model = opts.model ?? process.env.SQLRITE_NOTES_OPENAI_MODEL ?? 'text-embedding-3-small';
    return makeOpenAIEmbedder({
      apiKey,
      model,
      dim,
      fetchFn: opts.fetchFn ?? fetch,
    });
  }
  throw new Error(`unknown embedder kind: ${JSON.stringify(kind)} (expected "hash" or "openai")`);
}

// ------------------------------------------------------------------
// Hash embedder
//
// Deterministic, zero-dependency, offline. Maps each whitespace
// token through a tiny FNV-1a hash into one of `dim` slots, scales
// by token frequency, then L2-normalizes the result so cosine
// similarity is meaningful.

/**
 * @param {number} dim
 * @returns {Embedder}
 */
export function makeHashEmbedder(dim) {
  return {
    name: 'hash',
    dim,
    async embed(text) {
      const vec = new Float64Array(dim);
      const tokens = (text || '').toLowerCase().match(/[a-z0-9]+/g) ?? [];
      for (const tok of tokens) {
        const slot = fnv1a32(tok) % dim;
        vec[slot] += 1;
      }
      // L2 normalize (zero-safe).
      let sumSq = 0;
      for (let i = 0; i < vec.length; i++) sumSq += vec[i] * vec[i];
      const norm = Math.sqrt(sumSq);
      if (norm === 0) return Array.from(vec);
      const out = new Array(dim);
      for (let i = 0; i < vec.length; i++) out[i] = vec[i] / norm;
      return out;
    },
  };
}

function fnv1a32(s) {
  // Classic FNV-1a 32-bit, returns a non-negative integer.
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

// ------------------------------------------------------------------
// OpenAI embedder

/**
 * @param {{ apiKey: string, model: string, dim: number, fetchFn: typeof fetch }} args
 * @returns {Embedder}
 */
export function makeOpenAIEmbedder({ apiKey, model, dim, fetchFn }) {
  return {
    name: `openai/${model}`,
    dim,
    async embed(text) {
      const body = JSON.stringify({
        model,
        input: text,
        dimensions: dim,
      });
      const res = await fetchFn('https://api.openai.com/v1/embeddings', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          authorization: `Bearer ${apiKey}`,
        },
        body,
      });
      if (!res.ok) {
        const detail = await safeText(res);
        throw new Error(
          `OpenAI embeddings API error ${res.status}: ${detail.slice(0, 300)}`,
        );
      }
      const json = await res.json();
      const vec = json?.data?.[0]?.embedding;
      if (!Array.isArray(vec)) {
        throw new Error('OpenAI embeddings: malformed response (no data[0].embedding)');
      }
      if (vec.length !== dim) {
        throw new Error(
          `OpenAI embeddings: returned ${vec.length} dims, expected ${dim}`,
        );
      }
      return vec;
    },
  };
}

async function safeText(res) {
  try {
    return await res.text();
  } catch {
    return '';
  }
}
