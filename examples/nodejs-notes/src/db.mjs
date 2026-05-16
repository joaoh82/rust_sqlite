// SQLRite-backed storage for the notes index.
//
// Owns the schema, migrations, and every SQL string in the project.
// Higher-level modules (ingest.mjs, search.mjs) call into `NotesDB`
// rather than touching SQL directly.
//
// Schema v1 — two tables:
//
//   documents(id, path, title, mtime, content, content_hash)
//     FTS index on `content`.
//
//   chunks(id, document_id, ord, content, embedding VECTOR(dim))
//     HNSW index on `embedding`, FTS index on `content`.
//
// One row per file in `documents`; one row per ~400-token slice in
// `chunks`. Hybrid retrieval queries `chunks` (vector + BM25, fused
// at the SQL level) and joins back to `documents` for path / title.

import { mkdirSync } from 'node:fs';
import { dirname } from 'node:path';

import { Database } from '@joaoh82/sqlrite';

import { q, ident } from './sqlutil.mjs';
import { DEFAULT_EMBEDDING_DIM } from './config.mjs';

const SCHEMA_VERSION = 1;

export class NotesDB {
  /**
   * Open or create a notes database at `path`. Pass `:memory:` for a
   * transient store (useful in tests).
   *
   * @param {string} path
   * @param {{ dim?: number, readOnly?: boolean }} [opts]
   */
  constructor(path, opts = {}) {
    this.path = path;
    this.dim = opts.dim ?? DEFAULT_EMBEDDING_DIM;

    if (path !== ':memory:') {
      mkdirSync(dirname(path), { recursive: true });
    }

    this._db = opts.readOnly
      ? Database.openReadOnly(path)
      : new Database(path);

    if (!opts.readOnly) {
      this._migrate();
    }
  }

  // ------------------------------------------------------------------
  // Migrations

  _migrate() {
    const cur = this._db;
    let current = 0;
    try {
      const row = cur.prepare('SELECT version FROM schema_version').get();
      current = row?.version ?? 0;
    } catch {
      // schema_version table doesn't exist yet — fresh database.
      cur.exec(
        'CREATE TABLE schema_version (version INTEGER PRIMARY KEY)',
      );
      cur.exec(
        `INSERT INTO schema_version (version) VALUES (${q(0)})`,
      );
    }

    if (current < 1) {
      this._applyV1();
      cur.exec(`DELETE FROM schema_version`);
      cur.exec(
        `INSERT INTO schema_version (version) VALUES (${q(SCHEMA_VERSION)})`,
      );
    }
  }

  _applyV1() {
    const dim = this.dim;
    this._db.exec(`
      CREATE TABLE documents (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL UNIQUE,
        title TEXT NOT NULL,
        mtime INTEGER NOT NULL,
        content TEXT NOT NULL,
        content_hash TEXT NOT NULL
      )
    `);
    this._db.exec(`
      CREATE TABLE chunks (
        id INTEGER PRIMARY KEY,
        document_id INTEGER NOT NULL,
        ord INTEGER NOT NULL,
        content TEXT NOT NULL,
        embedding VECTOR(${dim})
      )
    `);
    // FTS indexes give us BM25 ranking via `bm25_score(col, 'q')` —
    // both documents.content (whole-document hits) and chunks.content
    // (passage-level hits) are useful surfaces.
    this._db.exec('CREATE INDEX idx_documents_fts ON documents USING fts (content)');
    this._db.exec('CREATE INDEX idx_chunks_fts ON chunks USING fts (content)');
    // HNSW for semantic KNN over chunk embeddings.
    this._db.exec('CREATE INDEX idx_chunks_emb ON chunks USING hnsw (embedding)');
  }

  // ------------------------------------------------------------------
  // Writes

  /**
   * Upsert a document by `path`. Returns `{ id, replaced }` — `replaced`
   * is true if a previous version of the document was removed first.
   *
   * Chunks are NOT touched here; the caller is responsible for calling
   * `replaceChunks(id, ...)` after re-chunking + re-embedding.
   *
   * @param {{ path: string, title: string, mtime: number, content: string, contentHash: string }} doc
   * @returns {{ id: number, replaced: boolean }}
   */
  upsertDocument(doc) {
    const existing = this._db
      .prepare(`SELECT id FROM documents WHERE path = ${q(doc.path)}`)
      .get();
    let replaced = false;

    if (existing) {
      replaced = true;
      // Delete existing chunks first — referential consistency.
      this._db.exec(`DELETE FROM chunks WHERE document_id = ${q(existing.id)}`);
      this._db.exec(`DELETE FROM documents WHERE id = ${q(existing.id)}`);
    }

    this._db.exec(
      `INSERT INTO documents (path, title, mtime, content, content_hash) VALUES (` +
        `${q(doc.path)}, ${q(doc.title)}, ${q(doc.mtime)}, ${q(doc.content)}, ${q(doc.contentHash)})`,
    );
    const inserted = this._db
      .prepare(`SELECT id FROM documents WHERE path = ${q(doc.path)}`)
      .get();
    if (!inserted) throw new Error('upsertDocument(): row vanished after INSERT');
    return { id: inserted.id, replaced };
  }

  /**
   * Insert one chunk row. Embedding must match `this.dim`.
   *
   * @param {{ documentId: number, ord: number, content: string, embedding: number[] }} chunk
   */
  insertChunk({ documentId, ord, content, embedding }) {
    if (embedding.length !== this.dim) {
      throw new Error(
        `insertChunk(): embedding dim ${embedding.length} ≠ schema dim ${this.dim}`,
      );
    }
    this._db.exec(
      `INSERT INTO chunks (document_id, ord, content, embedding) VALUES (` +
        `${q(documentId)}, ${q(ord)}, ${q(content)}, ${q(embedding)})`,
    );
  }

  /**
   * Drop a document and every chunk pointing at it.
   *
   * @param {number} documentId
   */
  deleteDocument(documentId) {
    this._db.exec(`DELETE FROM chunks WHERE document_id = ${q(documentId)}`);
    this._db.exec(`DELETE FROM documents WHERE id = ${q(documentId)}`);
  }

  // ------------------------------------------------------------------
  // Reads

  /**
   * Map of path → { id, mtime, content_hash }. Used by `refresh` to
   * decide which files changed.
   *
   * @returns {Map<string, { id: number, mtime: number, contentHash: string }>}
   */
  listDocuments() {
    const rows = this._db
      .prepare('SELECT id, path, mtime, content_hash FROM documents')
      .all();
    const map = new Map();
    for (const r of rows) {
      map.set(r.path, {
        id: r.id,
        mtime: r.mtime,
        contentHash: r.content_hash,
      });
    }
    return map;
  }

  /**
   * Hybrid top-k search over chunks. Combines BM25 lexical with vector
   * cosine in a single `ORDER BY` (see `docs/fts.md`).
   *
   * If `query` produces no FTS tokens (e.g. a single non-ASCII word),
   * we fall back to vector-only ranking — otherwise the FTS pre-filter
   * would return an empty set.
   *
   * @param {{ query: string, embedding: number[], k?: number, weight?: number }} args
   * @returns {Array<{ chunk_id: number, document_id: number, path: string, title: string, ord: number, content: string }>}
   */
  hybridSearch({ query, embedding, k = 5, weight = 0.5 }) {
    if (embedding.length !== this.dim) {
      throw new Error(
        `hybridSearch(): embedding dim ${embedding.length} ≠ schema dim ${this.dim}`,
      );
    }
    const tokens = ftsTokenize(query);
    const ftsQuery = tokens.join(' ');
    const w = clamp01(weight);

    let chunkRows;
    if (ftsQuery.length === 0) {
      chunkRows = this._db
        .prepare(
          `SELECT id, document_id, ord, content FROM chunks ` +
            `ORDER BY vec_distance_cosine(embedding, ${q(embedding)}) ASC ` +
            `LIMIT ${q(k)}`,
        )
        .all();
    } else {
      // Hybrid: fts_match pre-filters, ORDER BY fuses BM25 + cosine.
      chunkRows = this._db
        .prepare(
          `SELECT id, document_id, ord, content FROM chunks ` +
            `WHERE fts_match(content, ${q(ftsQuery)}) ` +
            `ORDER BY ${q(w)} * bm25_score(content, ${q(ftsQuery)}) ` +
            `+ ${q(1 - w)} * (1.0 - vec_distance_cosine(embedding, ${q(embedding)})) ` +
            `DESC LIMIT ${q(k)}`,
        )
        .all();
      // If FTS pre-filter happened to find nothing (every token is
      // unknown to the index), fall back to vector-only so the agent
      // always gets *some* recall to ground on.
      if (chunkRows.length === 0) {
        chunkRows = this._db
          .prepare(
            `SELECT id, document_id, ord, content FROM chunks ` +
              `ORDER BY vec_distance_cosine(embedding, ${q(embedding)}) ASC ` +
              `LIMIT ${q(k)}`,
          )
          .all();
      }
    }

    return chunkRows.map((row) => {
      const doc = this._db
        .prepare(
          `SELECT path, title FROM documents WHERE id = ${q(row.document_id)}`,
        )
        .get();
      return {
        chunk_id: row.id,
        document_id: row.document_id,
        path: doc?.path ?? '',
        title: doc?.title ?? '',
        ord: row.ord,
        content: row.content,
      };
    });
  }

  /**
   * BM25 top-k over `documents.content` — useful for the debug
   * `search --mode=bm25-docs` shape.
   *
   * @param {string} query
   * @param {number} k
   */
  bm25DocumentsSearch(query, k = 5) {
    const tokens = ftsTokenize(query);
    if (tokens.length === 0) return [];
    const ftsQuery = tokens.join(' ');
    return this._db
      .prepare(
        `SELECT id, path, title FROM documents ` +
          `WHERE fts_match(content, ${q(ftsQuery)}) ` +
          `ORDER BY bm25_score(content, ${q(ftsQuery)}) DESC ` +
          `LIMIT ${q(k)}`,
      )
      .all();
  }

  /** Quick row counts for `stats`. */
  stats() {
    const dRow = this._db.prepare('SELECT COUNT(*) AS c FROM documents').get();
    const cRow = this._db.prepare('SELECT COUNT(*) AS c FROM chunks').get();
    return {
      documents: Number(dRow?.c ?? 0),
      chunks: Number(cRow?.c ?? 0),
      dim: this.dim,
    };
  }

  // ------------------------------------------------------------------
  // Transactions

  /**
   * Run `fn` inside a single transaction. Commits on success, rolls
   * back on any thrown error. Synchronous — the engine is sync.
   *
   * @template T
   * @param {() => T} fn
   * @returns {T}
   */
  transaction(fn) {
    this._db.exec('BEGIN');
    try {
      const result = fn();
      this._db.exec('COMMIT');
      return result;
    } catch (err) {
      try {
        this._db.exec('ROLLBACK');
      } catch {
        // Ignore — the engine is in an unknown state; surface the
        // original error to the caller.
      }
      throw err;
    }
  }

  /** Raw escape hatch — used by tests for ad-hoc SQL. */
  raw() {
    return this._db;
  }

  /**
   * Close the underlying engine connection and re-open it at the same
   * path. Used by the ingest pipeline to work around the engine's
   * HNSW-after-delete bug (see the example's README). After this
   * call the wrapper still works exactly as before — only the
   * underlying connection is fresh, which forces a clean index
   * rebuild on the next read.
   *
   * @param {{ readOnly?: boolean }} [opts]
   */
  reopen(opts = {}) {
    if (this.path === ':memory:') {
      throw new Error('reopen(): in-memory databases cannot be reopened (state would be lost)');
    }
    this._db.close();
    this._db = opts.readOnly
      ? Database.openReadOnly(this.path)
      : new Database(this.path);
  }

  close() {
    this._db.close();
  }
}

// ------------------------------------------------------------------
// FTS tokenizer mirror.
//
// The engine's FTS tokenizer (docs/fts.md) splits on `[^A-Za-z0-9]+`
// and lowercases. We replicate it in JS so we can pre-check whether a
// query string would yield any tokens — if not, the FTS WHERE clause
// matches nothing and we should fall back to vector-only.

const TOKEN_RE = /[A-Za-z0-9]+/g;
const STOPWORDS = new Set([
  'a', 'an', 'and', 'or', 'the', 'is', 'are', 'was', 'were', 'be', 'been',
  'in', 'on', 'at', 'to', 'of', 'for', 'with', 'by', 'as', 'it', 'this',
  'that', 'these', 'those', 'i', 'you', 'we', 'they', 'he', 'she',
]);

/**
 * Tokenize a query the same way the engine's FTS tokenizer would,
 * then drop a tiny stop-list to avoid `fts_match` ballooning into a
 * full-table scan on filler words. (The engine has no stop list of
 * its own — that's intentional, see `docs/fts.md`. But for retrieval
 * we definitely don't want "the" + "is" to drive ranking.)
 *
 * @param {string} text
 * @returns {string[]}
 */
export function ftsTokenize(text) {
  if (!text) return [];
  const matches = text.match(TOKEN_RE) ?? [];
  return matches
    .map((t) => t.toLowerCase())
    .filter((t) => t.length > 1 && !STOPWORDS.has(t));
}

function clamp01(x) {
  if (!Number.isFinite(x)) return 0.5;
  if (x < 0) return 0;
  if (x > 1) return 1;
  return x;
}
