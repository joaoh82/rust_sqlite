// End-to-end ingest + search test against the test/fixtures notes.
// Skips cleanly if the @joaoh82/sqlrite Node binding isn't built.

import test from 'node:test';
import assert from 'node:assert/strict';
import {
  mkdtempSync,
  mkdirSync,
  rmSync,
  writeFileSync,
  utimesSync,
  unlinkSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { makeHashEmbedder } from '../src/embeddings.mjs';

let NotesDB, ingest, refresh, search;
let skipReason = null;
try {
  ({ NotesDB } = await import('../src/db.mjs'));
  ({ ingest, refresh } = await import('../src/ingest.mjs'));
  ({ search } = await import('../src/search.mjs'));
} catch (err) {
  skipReason = `cannot import (build the Node SDK first?): ${err.message}`;
}

const maybeSkip = skipReason ? { skip: skipReason } : {};

const here = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(here, 'fixtures');

test(
  'ingest fixtures → search recalls the right note for each query',
  maybeSkip,
  async () => {
    const dir = mkdtempSync(join(tmpdir(), 'sqlrite-notes-itest-'));
    const path = join(dir, 'notes.sqlrite');
    try {
      const embedder = makeHashEmbedder(64);
      const db = new NotesDB(path, { dim: embedder.dim });
      try {
        const stats = await ingest({ db, root: fixturesDir, embedder });
        assert.ok(stats.files >= 3, `expected ≥3 files, got ${stats.files}`);
        assert.ok(stats.chunks >= 3, `expected ≥3 chunks, got ${stats.chunks}`);

        const crdtHits = await search({
          db,
          embedder,
          query: 'collaborative editing CRDT',
          k: 3,
        });
        assert.ok(crdtHits.length > 0);
        assert.equal(crdtHits[0].path, 'crdts.md');

        const pgHits = await search({
          db,
          embedder,
          query: 'WAL replication',
          k: 3,
        });
        assert.ok(pgHits.length > 0);
        assert.equal(pgHits[0].path, 'postgres.md');

        const runHits = await search({
          db,
          embedder,
          query: 'marathon training long run',
          k: 3,
        });
        assert.ok(runHits.length > 0);
        assert.equal(runHits[0].path, 'running.md');
      } finally {
        db.close();
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  },
);

test(
  'refresh: unchanged files are skipped; changed files re-embedded; deleted removed',
  maybeSkip,
  async () => {
    const dir = mkdtempSync(join(tmpdir(), 'sqlrite-notes-itest-'));
    const sourceDir = join(dir, 'notes');
    const dbPath = join(dir, 'notes.sqlrite');
    try {
      mkdirSync(sourceDir, { recursive: true });
      writeFileSync(join(sourceDir, 'keep.md'), '# Keep\n\nshould stay verbatim.\n');
      writeFileSync(
        join(sourceDir, 'change.md'),
        '# Change\n\noriginal body about postgres.\n',
      );
      writeFileSync(join(sourceDir, 'remove.md'), '# Remove\n\nwill be deleted later.\n');

      const embedder = makeHashEmbedder(32);
      const db = new NotesDB(dbPath, { dim: embedder.dim });
      try {
        const first = await ingest({ db, root: sourceDir, embedder });
        assert.equal(first.files, 3);

        writeFileSync(
          join(sourceDir, 'change.md'),
          '# Change\n\nrewritten body about distributed systems.\n',
        );
        const futureSec = Math.floor(Date.now() / 1000) + 5;
        utimesSync(join(sourceDir, 'change.md'), futureSec, futureSec);
        unlinkSync(join(sourceDir, 'remove.md'));

        const second = await refresh({ db, root: sourceDir, embedder });
        assert.equal(second.files, 1, 'one file changed');
        assert.equal(second.skipped, 1, 'one file unchanged');
        assert.equal(second.deleted, 1, 'one file removed');

        const docs = db.listDocuments();
        assert.equal(docs.size, 2);
        assert.ok(docs.has('keep.md'));
        assert.ok(docs.has('change.md'));
        assert.ok(!docs.has('remove.md'));
      } finally {
        db.close();
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  },
);
