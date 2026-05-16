// Markdown → SQLRite ingest pipeline.
//
// Walks a directory of `.md` / `.markdown` files, chunks each one,
// embeds every chunk, and writes documents + chunks into the DB.
// Two entry points:
//
//   - `ingest(...)` — full reindex of a directory. Used by `init`.
//   - `refresh(...)` — incremental: skip files whose mtime + content
//     hash haven't changed since the last run. Used by `refresh`.
//
// Both flow through `ingestImpl`, which splits the work into three
// phases: PLAN (read-only diff against the current DB) → DELETE (drop
// stale documents/chunks; close + reopen the DB) → INSERT (write new
// rows). The close/reopen between DELETE and INSERT is a workaround
// for an engine bug where the HNSW chunk index panics when rows are
// deleted and re-inserted in the same connection lifetime — see the
// "Known limitations" section of this example's README.

import { readFile, readdir, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join, relative, basename, extname } from 'node:path';

import {
  stripFrontmatter,
  deriveTitle,
  chunkMarkdown,
} from './chunker.mjs';
import { DEFAULT_CHUNK_TOKENS, DEFAULT_CHUNK_OVERLAP } from './config.mjs';

/**
 * @typedef {object} IngestStats
 * @property {number} files
 * @property {number} chunks
 * @property {number} skipped
 * @property {number} deleted
 * @property {number} elapsedMs
 */

/**
 * Find every markdown file under `root` (recursive). Ignores hidden
 * directories (`.git`, `.obsidian`, etc.) and `node_modules` so a
 * dropped-in Obsidian vault doesn't suck in junk.
 *
 * @param {string} root
 * @returns {Promise<string[]>}
 */
export async function findMarkdownFiles(root) {
  const out = [];
  await walk(root, out);
  out.sort();
  return out;
}

async function walk(dir, out) {
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch {
    return; // root may not exist; let the caller surface the message.
  }
  for (const ent of entries) {
    const full = join(dir, ent.name);
    if (ent.isDirectory()) {
      if (ent.name.startsWith('.') || ent.name === 'node_modules') continue;
      await walk(full, out);
      continue;
    }
    if (!ent.isFile()) continue;
    const ext = extname(ent.name).toLowerCase();
    if (ext === '.md' || ext === '.markdown') out.push(full);
  }
}

/**
 * Re-ingest every file under `root` — replaces any existing rows for
 * the same path. Use for the `init` flow.
 *
 * @param {{ db: NotesDB, root: string, embedder: import('./embeddings.mjs').Embedder, logger?: (s: string) => void, chunkOpts?: { targetTokens?: number, overlapTokens?: number } }} args
 * @returns {Promise<IngestStats>}
 */
export async function ingest(args) {
  return ingestImpl({ ...args, mode: 'full' });
}

/**
 * Incremental re-ingest. Skips files whose mtime + content hash
 * matches what's already in the DB. Deletes documents whose file is
 * gone from disk.
 *
 * @param {{ db: NotesDB, root: string, embedder: import('./embeddings.mjs').Embedder, logger?: (s: string) => void, chunkOpts?: { targetTokens?: number, overlapTokens?: number } }} args
 * @returns {Promise<IngestStats>}
 */
export async function refresh(args) {
  return ingestImpl({ ...args, mode: 'incremental' });
}

/**
 * @param {{ db: NotesDB, root: string, embedder: import('./embeddings.mjs').Embedder, logger?: (s: string) => void, chunkOpts?: { targetTokens?: number, overlapTokens?: number }, mode: 'full' | 'incremental' }} args
 * @returns {Promise<IngestStats>}
 */
async function ingestImpl({ db, root, embedder, logger, chunkOpts, mode }) {
  const log = logger ?? (() => {});
  const t0 = Date.now();
  const target = chunkOpts?.targetTokens ?? DEFAULT_CHUNK_TOKENS;
  const overlap = chunkOpts?.overlapTokens ?? DEFAULT_CHUNK_OVERLAP;

  const files = await findMarkdownFiles(root);
  if (files.length === 0) {
    log(`no markdown files found under ${root}`);
    return { files: 0, chunks: 0, skipped: 0, deleted: 0, elapsedMs: 0 };
  }

  // ----------------------------------------------------------------
  // PHASE 1 — plan. Read the current DB state, hash each on-disk
  // file, build the change set. No writes yet.
  const existing = db.listDocuments();
  /** @type {Array<{ relPath: string, abs: string, mtime: number, text: string, hash: string, priorId: number | null }>} */
  const planUpserts = [];
  /** @type {number[]} */
  const planDeletes = [];
  let skipped = 0;
  const seenPaths = new Set();

  for (const abs of files) {
    const rel = relative(root, abs);
    const text = await readFile(abs, 'utf8');
    const fstat = await stat(abs);
    const mtime = Math.floor(fstat.mtimeMs / 1000);
    const hash = sha256Hex(text);
    seenPaths.add(rel);
    const prior = existing.get(rel);

    if (mode === 'incremental' && prior && prior.mtime === mtime && prior.contentHash === hash) {
      skipped++;
      continue;
    }
    planUpserts.push({
      relPath: rel,
      abs,
      mtime,
      text,
      hash,
      priorId: prior?.id ?? null,
    });
  }
  // Files that vanished from disk — only when refreshing.
  if (mode === 'incremental') {
    for (const [path, prior] of existing) {
      if (!seenPaths.has(path)) planDeletes.push(prior.id);
    }
  }
  // Full ingest implicitly replaces every existing doc that we're
  // re-ingesting. Drop docs no longer present on disk too, so a
  // re-run of `init` against a different source dir doesn't leave
  // orphans behind.
  if (mode === 'full') {
    for (const [path, prior] of existing) {
      if (!seenPaths.has(path)) planDeletes.push(prior.id);
    }
  }

  // Embed BEFORE touching the DB. If anything throws here (e.g. a
  // network embedding call fails) we haven't mutated anything.
  /** @type {Array<{ plan: typeof planUpserts[number], title: string, body: string, chunks: Array<{ ord: number, content: string, embedding: number[] }> }>} */
  const embedded = [];
  let totalEmbedded = 0;
  for (const p of planUpserts) {
    const { frontmatter, body } = stripFrontmatter(p.text);
    const title = deriveTitle({
      frontmatter,
      body,
      fallback: basename(p.abs, extname(p.abs)),
    });
    const chunks = chunkMarkdown(body, { targetTokens: target, overlapTokens: overlap });
    if (chunks.length === 0) {
      log(`skipped empty: ${p.relPath}`);
      continue;
    }
    const embeds = [];
    for (const c of chunks) {
      const v = await embedder.embed(c.content);
      embeds.push({ ord: c.ord, content: c.content, embedding: v });
      totalEmbedded++;
    }
    embedded.push({ plan: p, title, body, chunks: embeds });
    if (embedded.length % 10 === 0) {
      log(`  embedded ${embedded.length}/${planUpserts.length} files (${totalEmbedded} chunks)…`);
    }
  }

  const hasMutations = planDeletes.length > 0 || embedded.some((e) => e.plan.priorId !== null);

  // ----------------------------------------------------------------
  // PHASE 2 — deletes (and replacing-deletes).
  //
  // The engine's HNSW index has a bug where rows deleted and re-
  // inserted within the same connection lifetime can corrupt the
  // index's stored vectors (see ../README.md "Known limitations").
  // Closing + reopening the connection between the delete-pass and
  // the insert-pass forces a full index rebuild on next open,
  // sidestepping the issue. We only pay this cost when there's
  // actually something to delete; pure-INSERT runs (first `init`)
  // skip this hop entirely.
  if (hasMutations) {
    db.transaction(() => {
      for (const id of planDeletes) db.deleteDocument(id);
      for (const e of embedded) {
        if (e.plan.priorId !== null) db.deleteDocument(e.plan.priorId);
      }
    });
    db.reopen();
  }

  // ----------------------------------------------------------------
  // PHASE 3 — inserts.
  let totalChunks = 0;
  for (const e of embedded) {
    db.transaction(() => {
      const { id } = db.upsertDocument({
        path: e.plan.relPath,
        title: e.title,
        mtime: e.plan.mtime,
        content: e.body,
        contentHash: e.plan.hash,
      });
      for (const c of e.chunks) {
        db.insertChunk({
          documentId: id,
          ord: c.ord,
          content: c.content,
          embedding: c.embedding,
        });
      }
    });
    totalChunks += e.chunks.length;
  }

  return {
    files: embedded.length,
    chunks: totalChunks,
    skipped,
    deleted: planDeletes.length,
    elapsedMs: Date.now() - t0,
  };
}

function sha256Hex(input) {
  return createHash('sha256').update(input).digest('hex');
}
