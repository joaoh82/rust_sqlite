// Markdown chunker.
//
// Split a document into ~`targetTokens`-sized chunks with optional
// overlap. The chunker keeps three rules:
//
//   1. Never split mid-paragraph — paragraph boundaries (blank lines)
//      are the smallest atomic unit.
//   2. Carry the closest preceding heading into each chunk as a
//      one-line prefix. Without it, mid-document chunks lose every
//      hint of structure and embeddings + BM25 both degrade.
//   3. Token counting is approximate — `text.split(/\s+/).length`
//      is close enough for chunking, and dodges a heavy tokenizer
//      dependency. The retrieval side never needs exact counts.

const DEFAULT_TARGET = 400;
const DEFAULT_OVERLAP = 60;

/**
 * Strip YAML frontmatter from a markdown document. Returns
 * `{ frontmatter, body }`. Frontmatter is returned as the raw text
 * between the fences (no YAML parsing — we only consult it for the
 * title).
 *
 * @param {string} text
 * @returns {{ frontmatter: string, body: string }}
 */
export function stripFrontmatter(text) {
  if (!text.startsWith('---\n') && !text.startsWith('---\r\n')) {
    return { frontmatter: '', body: text };
  }
  const re = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/;
  const m = text.match(re);
  if (!m) return { frontmatter: '', body: text };
  return { frontmatter: m[1], body: text.slice(m[0].length) };
}

/**
 * Derive a title for the document.
 *
 *   1. `title:` field in YAML frontmatter, if present.
 *   2. First `#` / `##` / `###` heading in the body.
 *   3. The filename stem, supplied by the caller.
 *
 * @param {{ frontmatter: string, body: string, fallback: string }} args
 * @returns {string}
 */
export function deriveTitle({ frontmatter, body, fallback }) {
  if (frontmatter) {
    const m = frontmatter.match(/^title\s*:\s*(?:["']?)(.+?)(?:["']?)\s*$/m);
    if (m) return m[1].trim();
  }
  const heading = body.match(/^#{1,6}\s+(.+?)\s*$/m);
  if (heading) return heading[1].trim();
  return fallback;
}

/**
 * Approximate token count — `whitespace-separated word count` is a
 * fine proxy at the granularities we care about (~hundreds).
 *
 * @param {string} text
 */
export function approxTokens(text) {
  if (!text) return 0;
  return text.trim().split(/\s+/).filter(Boolean).length;
}

/**
 * Chunk markdown body text into ~targetTokens-sized passages.
 *
 * @param {string} body
 * @param {{ targetTokens?: number, overlapTokens?: number }} [opts]
 * @returns {Array<{ ord: number, content: string }>}
 */
export function chunkMarkdown(body, opts = {}) {
  const target = opts.targetTokens ?? DEFAULT_TARGET;
  const overlap = opts.overlapTokens ?? DEFAULT_OVERLAP;

  // Step 1: walk paragraphs, attaching the most-recent heading to
  // each one. Keep the paragraph text verbatim so embeddings see the
  // original prose.
  const blocks = paragraphsWithHeadings(body);

  // Step 2: greedy pack paragraphs into chunks until we cross the
  // token target. Paragraphs that exceed the target on their own get
  // their own chunk — never split mid-paragraph.
  const chunks = [];
  let current = []; // { heading, text }[]
  let currentTokens = 0;
  let lastHeading = '';

  function flush() {
    if (current.length === 0) return;
    chunks.push(renderChunk(current));
    current = [];
    currentTokens = 0;
  }

  for (const block of blocks) {
    const blockTokens = approxTokens(block.text);
    // Keep the heading context — if it changed, we'll surface the new
    // one in this chunk, but otherwise we don't repeat it.
    if (currentTokens > 0 && currentTokens + blockTokens > target) {
      flush();
    }
    if (current.length === 0 && block.heading) {
      lastHeading = block.heading;
      current.push({ heading: block.heading, text: '' });
    } else if (block.heading && block.heading !== lastHeading) {
      lastHeading = block.heading;
      current.push({ heading: block.heading, text: '' });
    }
    current.push({ heading: '', text: block.text });
    currentTokens += blockTokens;
  }
  flush();

  // Step 3: apply overlap by prepending the tail of the previous
  // chunk to the current one. Overlap reduces the chance that a
  // matching sentence sits exactly on a chunk boundary.
  if (overlap > 0 && chunks.length > 1) {
    for (let i = chunks.length - 1; i > 0; i--) {
      const prev = chunks[i - 1];
      const tail = trailingTokens(prev, overlap);
      if (tail) {
        chunks[i] = `${tail}\n\n${chunks[i]}`;
      }
    }
  }

  return chunks
    .map((content, ord) => ({ ord, content: content.trim() }))
    .filter((c) => c.content.length > 0);
}

// ------------------------------------------------------------------
// Helpers

function paragraphsWithHeadings(body) {
  const lines = body.split(/\r?\n/);
  /** @type {Array<{ heading: string, text: string }>} */
  const blocks = [];
  let currentHeading = '';
  let buffer = [];

  function flushBuffer() {
    const text = buffer.join('\n').trim();
    if (text) blocks.push({ heading: currentHeading, text });
    buffer = [];
  }

  for (const line of lines) {
    const headingMatch = line.match(/^(#{1,6})\s+(.+?)\s*$/);
    if (headingMatch) {
      flushBuffer();
      currentHeading = headingMatch[2].trim();
      // Also emit the heading itself as a tiny block so it can be the
      // sole content of a chunk if nothing follows.
      blocks.push({ heading: currentHeading, text: line.trim() });
      continue;
    }
    if (line.trim() === '') {
      flushBuffer();
    } else {
      buffer.push(line);
    }
  }
  flushBuffer();
  return blocks;
}

function renderChunk(parts) {
  // Concat the parts (mix of heading-only blocks and paragraph
  // blocks) into a single text body. Headings are kept inline so the
  // embedded text reads naturally.
  const out = [];
  for (const p of parts) {
    if (p.heading) {
      out.push(`# ${p.heading}`);
    } else if (p.text) {
      out.push(p.text);
    }
  }
  return out.join('\n\n');
}

function trailingTokens(text, n) {
  const tokens = text.split(/\s+/).filter(Boolean);
  if (tokens.length <= n) return text;
  return tokens.slice(tokens.length - n).join(' ');
}
