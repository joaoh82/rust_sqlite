// End-to-end tests for the sqlrite Node.js bindings.
//
// Uses Node 18+'s built-in `node:test` runner so we don't add a
// test framework dep. Run from sdk/nodejs via:
//
//     npm run build   # produces sqlrite.<platform>-<arch>.node
//     npm test
//
// The tests walk the full Node → napi-rs → Rust → SQLRite pipeline,
// so a passing suite is strong evidence the binding is usable.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { Database } from '../index.js';

function tmpDbPath(name) {
  const dir = mkdtempSync(join(tmpdir(), `sqlrite-node-${name}-`));
  return {
    path: join(dir, 'db.sqlrite'),
    cleanup: () => rmSync(dir, { recursive: true, force: true }),
  };
}

// ---------------------------------------------------------------------------
// Basic CRUD + iteration

test('in-memory round-trip', () => {
  const db = new Database(':memory:');
  db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)');
  db.exec("INSERT INTO users (name, age) VALUES ('alice', 30)");
  db.exec("INSERT INTO users (name, age) VALUES ('bob', 25)");
  const rows = db.prepare('SELECT id, name, age FROM users').all();
  assert.equal(rows.length, 2);
  assert.deepEqual(rows[0], { id: 1, name: 'alice', age: 30 });
  assert.deepEqual(rows[1], { id: 2, name: 'bob', age: 25 });
  db.close();
});

test('get returns first row or null', () => {
  const db = new Database(':memory:');
  db.exec('CREATE TABLE t (x INTEGER PRIMARY KEY)');
  db.exec('INSERT INTO t (x) VALUES (42)');
  assert.deepEqual(db.prepare('SELECT x FROM t').get(), { x: 42 });

  db.exec('DELETE FROM t');
  assert.equal(db.prepare('SELECT x FROM t').get(), null);
  db.close();
});

test('iterate returns rows in a for-of loop', () => {
  const db = new Database(':memory:');
  db.exec('CREATE TABLE t (x INTEGER PRIMARY KEY)');
  for (let i = 1; i <= 5; i++) {
    db.exec(`INSERT INTO t (x) VALUES (${i})`);
  }
  const xs = [];
  for (const row of db.prepare('SELECT x FROM t').iterate()) {
    xs.push(row.x);
  }
  assert.deepEqual(xs, [1, 2, 3, 4, 5]);
  db.close();
});

test('columns exposes projection order', () => {
  const db = new Database(':memory:');
  db.exec('CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT, c TEXT)');
  db.exec("INSERT INTO t (a, b, c) VALUES (1, 'x', 'y')");
  const cols = db.prepare('SELECT a, b, c FROM t').columns();
  assert.deepEqual(cols, ['a', 'b', 'c']);
  db.close();
});

// ---------------------------------------------------------------------------
// Transactions + flags

test('BEGIN / ROLLBACK round-trip with inTransaction flag', () => {
  const db = new Database(':memory:');
  db.exec('CREATE TABLE t (x INTEGER PRIMARY KEY)');
  db.exec('INSERT INTO t (x) VALUES (1)');

  assert.equal(db.inTransaction, false);
  db.exec('BEGIN');
  assert.equal(db.inTransaction, true);
  db.exec('INSERT INTO t (x) VALUES (2)');
  db.exec('ROLLBACK');
  assert.equal(db.inTransaction, false);

  const rows = db.prepare('SELECT x FROM t').all();
  assert.equal(rows.length, 1);
  assert.equal(rows[0].x, 1);
  db.close();
});

test('BEGIN / COMMIT persists rows', () => {
  const db = new Database(':memory:');
  db.exec('CREATE TABLE t (x INTEGER PRIMARY KEY)');
  db.exec('BEGIN');
  db.exec('INSERT INTO t (x) VALUES (1)');
  db.exec('INSERT INTO t (x) VALUES (2)');
  db.exec('COMMIT');
  const xs = db.prepare('SELECT x FROM t').all().map((r) => r.x);
  assert.deepEqual(xs, [1, 2]);
  db.close();
});

// ---------------------------------------------------------------------------
// File-backed + read-only

test('file-backed DB persists across connections', () => {
  const { path, cleanup } = tmpDbPath('persist');
  try {
    {
      const db = new Database(path);
      db.exec('CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)');
      db.exec("INSERT INTO items (label) VALUES ('a')");
      db.exec("INSERT INTO items (label) VALUES ('b')");
      db.close();
    }
    const db2 = new Database(path);
    const rows = db2.prepare('SELECT label FROM items').all();
    assert.deepEqual(
      rows.map((r) => r.label).sort(),
      ['a', 'b']
    );
    db2.close();
  } finally {
    cleanup();
  }
});

test('openReadOnly rejects writes', () => {
  const { path, cleanup } = tmpDbPath('ro');
  try {
    {
      const db = new Database(path);
      db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY)');
      db.exec('INSERT INTO t (id) VALUES (1)');
      db.close();
    }
    const ro = Database.openReadOnly(path);
    assert.equal(ro.readonly, true);
    assert.throws(
      () => ro.exec('INSERT INTO t (id) VALUES (2)'),
      /read-only/
    );
    ro.close();
  } finally {
    cleanup();
  }
});

// ---------------------------------------------------------------------------
// Error paths

test('bad SQL throws with engine message', () => {
  const db = new Database(':memory:');
  assert.throws(
    () => db.exec('THIS IS NOT SQL'),
    /./ // any non-empty message
  );
  db.close();
});

test('non-empty params throws until Phase 5a.2 lands', () => {
  const db = new Database(':memory:');
  // `name` is TEXT without PK/UNIQUE, so repeated inserts of the
  // same value don't collide — we can exercise the four accepted
  // "no params" variants back-to-back.
  db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)');
  const stmt = db.prepare("INSERT INTO t (name) VALUES ('x')");
  stmt.run();
  stmt.run(null);
  stmt.run(undefined);
  stmt.run([]);
  // Non-empty array is rejected with a clear message.
  assert.throws(() => stmt.run([1]), /parameter binding/);
  db.close();
});

test('closed DB throws on any operation', () => {
  const db = new Database(':memory:');
  db.close();
  assert.throws(() => db.exec('SELECT 1'), /closed/);
  assert.throws(() => db.prepare('SELECT 1'), /closed/);
});
