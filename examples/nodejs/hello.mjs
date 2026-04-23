// Minimal walkthrough of the SQLRite Node.js bindings.
//
// Run after:
//
//     cd sdk/nodejs
//     npm install
//     npm run build
//
//     node examples/nodejs/hello.mjs
//
// Shape mirrors `better-sqlite3` so JavaScript devs who've used
// that library can pick this up without reading the docs. Rows
// come back as plain objects keyed by column name.

import { Database } from '../../sdk/nodejs/index.js';

// Pass `:memory:` for a transient in-memory DB (matching better-
// sqlite3's convention); pass a file path like 'foo.sqlrite' for a
// file-backed DB that auto-saves on every write.
const db = new Database(':memory:');

db.exec('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)');
db.exec("INSERT INTO users (name, age) VALUES ('alice', 30)");
db.exec("INSERT INTO users (name, age) VALUES ('bob', 25)");
db.exec("INSERT INTO users (name, age) VALUES ('carol', 40)");

// .columns() reports projection-order names so you know the shape
// without running the query yet.
const select = db.prepare('SELECT id, name, age FROM users');
console.log('Columns:', select.columns());

// .all() gives you an array of row objects — one object per row.
console.log('\nAll users:');
for (const row of select.all()) {
  console.log(`  ${row.id}: ${row.name} (${row.age})`);
}

// .get() returns the first row (or null if the query is empty) —
// handy for lookups by primary key.
const first = select.get();
console.log('\nFirst user:', first);

// Transactions: BEGIN / INSERT / ROLLBACK leaves the table
// unchanged. The `inTransaction` getter is live throughout.
db.exec('BEGIN');
console.log('\ninTransaction:', db.inTransaction);
db.exec("INSERT INTO users (name, age) VALUES ('phantom', 99)");
const midCount = db.prepare('SELECT id FROM users').all().length;
console.log(`Mid-transaction row count: ${midCount}`);
db.exec('ROLLBACK');
console.log('inTransaction after rollback:', db.inTransaction);
const finalCount = db.prepare('SELECT id FROM users').all().length;
console.log(`Post-rollback row count:   ${finalCount}`);

db.close();
