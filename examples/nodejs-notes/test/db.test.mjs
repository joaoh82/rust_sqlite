// Integration tests against a real SQLRite Connection. These require
// the @joaoh82/sqlrite Node binding to be built/installed; if it
// isn't, the suite emits a skip notice instead of failing.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

let NotesDB;
let skipReason = null;
try {
  ({ NotesDB } = await import('../src/db.mjs'));
} catch (err) {
  skipReason = `cannot import db.mjs (build the Node SDK first?): ${err.message}`;
}

// Node 24's test runner treats `{ skip: null }` as a skip directive
// (the key's presence matters more than its value), so use this
// helper to conditionally pass the option only when we genuinely
// want to skip.
const maybeSkip = skipReason ? { skip: skipReason } : {};

function withDb(fn) {
  const dir = mkdtempSync(join(tmpdir(), 'sqlrite-notes-test-'));
  const path = join(dir, 'notes.sqlrite');
  try {
    return fn({ dir, path });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('db: schema applies cleanly + stats start at zero', maybeSkip, () => {
  withDb(({ path }) => {
    const db = new NotesDB(path, { dim: 16 });
    try {
      const s = db.stats();
      assert.equal(s.documents, 0);
      assert.equal(s.chunks, 0);
      assert.equal(s.dim, 16);
    } finally {
      db.close();
    }
  });
});

test('db: upsertDocument + insertChunk round-trip', maybeSkip, () => {
  withDb(({ path }) => {
    const db = new NotesDB(path, { dim: 4 });
    try {
      const { id, replaced } = db.upsertDocument({
        path: 'a.md',
        title: 'A',
        mtime: 100,
        content: 'rust embedded database notes',
        contentHash: 'h1',
      });
      assert.ok(id > 0);
      assert.equal(replaced, false);

      db.insertChunk({
        documentId: id,
        ord: 0,
        content: 'rust embedded database notes',
        embedding: [1, 0, 0, 0],
      });

      const s = db.stats();
      assert.equal(s.documents, 1);
      assert.equal(s.chunks, 1);
    } finally {
      db.close();
    }
  });
});

test('db: upsertDocument replaces prior version + cascades chunks', maybeSkip, () => {
  withDb(({ path }) => {
    const db = new NotesDB(path, { dim: 4 });
    try {
      const v1 = db.upsertDocument({
        path: 'a.md',
        title: 'A',
        mtime: 100,
        content: 'one',
        contentHash: 'h1',
      });
      db.insertChunk({
        documentId: v1.id,
        ord: 0,
        content: 'one',
        embedding: [1, 0, 0, 0],
      });

      const v2 = db.upsertDocument({
        path: 'a.md',
        title: 'A v2',
        mtime: 200,
        content: 'two',
        contentHash: 'h2',
      });
      assert.equal(v2.replaced, true);
      assert.notEqual(v2.id, v1.id);

      const s = db.stats();
      assert.equal(s.documents, 1);
      assert.equal(s.chunks, 0); // old chunk got dropped on replace
    } finally {
      db.close();
    }
  });
});

test(
  'db: hybridSearch returns vector + BM25 hits in a sensible order',
  maybeSkip,
  () => {
    withDb(({ path }) => {
      const db = new NotesDB(path, { dim: 4 });
      try {
        const { id: dA } = db.upsertDocument({
          path: 'a.md',
          title: 'A',
          mtime: 1,
          content: 'rust embedded database',
          contentHash: 'h1',
        });
        const { id: dB } = db.upsertDocument({
          path: 'b.md',
          title: 'B',
          mtime: 2,
          content: 'distributed systems and consensus protocols',
          contentHash: 'h2',
        });
        // Two chunks each, with distinct embeddings.
        db.insertChunk({
          documentId: dA,
          ord: 0,
          content: 'rust embedded database',
          embedding: [1, 0, 0, 0],
        });
        db.insertChunk({
          documentId: dB,
          ord: 0,
          content: 'distributed systems and consensus protocols',
          embedding: [0, 1, 0, 0],
        });

        // A query whose embedding aligns with chunk A and whose
        // tokens overlap chunk A — A should win.
        const hits = db.hybridSearch({
          query: 'rust database',
          embedding: [1, 0, 0, 0],
          k: 2,
        });
        assert.ok(hits.length >= 1);
        assert.equal(hits[0].path, 'a.md');
      } finally {
        db.close();
      }
    });
  },
);

test(
  'db: hybridSearch falls back to vector-only when FTS tokens are empty',
  maybeSkip,
  () => {
    withDb(({ path }) => {
      const db = new NotesDB(path, { dim: 4 });
      try {
        const { id } = db.upsertDocument({
          path: 'a.md',
          title: 'A',
          mtime: 1,
          content: 'rust embedded database',
          contentHash: 'h1',
        });
        db.insertChunk({
          documentId: id,
          ord: 0,
          content: 'rust embedded database',
          embedding: [1, 0, 0, 0],
        });
        const hits = db.hybridSearch({
          query: '日本語', // every byte non-ASCII → no FTS tokens
          embedding: [1, 0, 0, 0],
          k: 5,
        });
        assert.equal(hits.length, 1);
        assert.equal(hits[0].path, 'a.md');
      } finally {
        db.close();
      }
    });
  },
);

test('db: deleteDocument cascades to chunks', maybeSkip, () => {
  withDb(({ path }) => {
    const db = new NotesDB(path, { dim: 4 });
    try {
      const { id } = db.upsertDocument({
        path: 'a.md',
        title: 'A',
        mtime: 1,
        content: 'x',
        contentHash: 'h',
      });
      db.insertChunk({
        documentId: id,
        ord: 0,
        content: 'x',
        embedding: [1, 0, 0, 0],
      });
      db.deleteDocument(id);
      const s = db.stats();
      assert.equal(s.documents, 0);
      assert.equal(s.chunks, 0);
    } finally {
      db.close();
    }
  });
});

test('db: listDocuments → path map', maybeSkip, () => {
  withDb(({ path }) => {
    const db = new NotesDB(path, { dim: 4 });
    try {
      db.upsertDocument({
        path: 'a.md',
        title: 'A',
        mtime: 100,
        content: 'x',
        contentHash: 'h1',
      });
      db.upsertDocument({
        path: 'b.md',
        title: 'B',
        mtime: 200,
        content: 'y',
        contentHash: 'h2',
      });
      const map = db.listDocuments();
      assert.equal(map.size, 2);
      assert.equal(map.get('a.md')?.mtime, 100);
      assert.equal(map.get('b.md')?.contentHash, 'h2');
    } finally {
      db.close();
    }
  });
});

test('db: re-open path-backed DB reads back data', maybeSkip, () => {
  withDb(({ path }) => {
    const a = new NotesDB(path, { dim: 4 });
    a.upsertDocument({ path: 'a.md', title: 'A', mtime: 1, content: 'x', contentHash: 'h' });
    a.close();

    const b = new NotesDB(path, { dim: 4 });
    try {
      const s = b.stats();
      assert.equal(s.documents, 1);
      const map = b.listDocuments();
      assert.equal(map.get('a.md')?.mtime, 1);
    } finally {
      b.close();
    }
  });
});
