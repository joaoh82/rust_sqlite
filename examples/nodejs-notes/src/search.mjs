// Hybrid retrieval driver for the `search` debug command.
//
// Same shape an LLM would get over MCP through `vector_search` +
// `bm25_search`, but with rendered output for humans.

/**
 * @param {{ db: import('./db.mjs').NotesDB, embedder: import('./embeddings.mjs').Embedder, query: string, k?: number, weight?: number }} args
 */
export async function search({ db, embedder, query, k = 5, weight = 0.5 }) {
  const embedding = await embedder.embed(query);
  return db.hybridSearch({ query, embedding, k, weight });
}

/**
 * Render a list of search results as a human-friendly string.
 *
 * @param {string} query
 * @param {ReturnType<import('./db.mjs').NotesDB['hybridSearch']>} hits
 */
export function renderResults(query, hits) {
  if (hits.length === 0) {
    return `no results for: ${JSON.stringify(query)}\n`;
  }
  const lines = [];
  lines.push(`top ${hits.length} hits for: ${JSON.stringify(query)}`);
  lines.push('');
  for (let i = 0; i < hits.length; i++) {
    const h = hits[i];
    const head = h.title ? `${h.title} — ${h.path}` : h.path;
    lines.push(`${pad(i + 1)}. ${head}  (chunk ${h.ord})`);
    lines.push(indent(truncate(h.content, 280)));
    lines.push('');
  }
  return lines.join('\n');
}

function pad(n) {
  return String(n).padStart(2, ' ');
}

function indent(text) {
  return text
    .split(/\r?\n/)
    .map((l) => `    ${l}`)
    .join('\n');
}

function truncate(text, max) {
  const t = text.replace(/\s+/g, ' ').trim();
  return t.length <= max ? t : `${t.slice(0, max - 1)}…`;
}
