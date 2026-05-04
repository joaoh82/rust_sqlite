# Supported SQL

The canonical reference for the SQL surface SQLRite implements today. Parsing is delegated to [`sqlparser`](https://crates.io/crates/sqlparser) using the SQLite dialect, so tokens and grammar follow SQLite — execution only implements the subset below, and anything else is rejected with a typed `NotImplemented` error rather than silently partial behavior.

If you're looking for _how_ to use SQLRite (REPL flow, meta-commands, history, embedding), see [Using SQLRite](usage.md). This document is the strict reference for what statements execute and what semantics they carry.

## Statement at a glance

| Statement | Supported today |
|---|---|
| [`CREATE TABLE`](#create-table) | Columns with `PRIMARY KEY` / `UNIQUE` / `NOT NULL` / `DEFAULT <literal>`; typed columns; auto-indexes on constrained columns |
| [`CREATE [UNIQUE] INDEX`](#create-index) | Single-column named indexes, `IF NOT EXISTS`, persisted as cell-based B-Trees |
| [`INSERT INTO`](#insert-into) | Auto-ROWID, UNIQUE/PK enforcement, clean type errors, NULL/DEFAULT padding |
| [`SELECT`](#select) | `*` or column list, `WHERE`, single-column `ORDER BY`, `LIMIT`; index probing on `col = literal` |
| [`UPDATE`](#update) | Multi-column `SET`, `WHERE`, arithmetic RHS, type + UNIQUE enforcement |
| [`DELETE`](#delete) | `WHERE` predicate or whole-table |
| [`ALTER TABLE`](#alter-table) | `RENAME TO`, `RENAME COLUMN`, `ADD COLUMN`, `DROP COLUMN` (one operation per statement) |
| [`DROP TABLE`](#drop-table) / [`DROP INDEX`](#drop-index) | `IF EXISTS`; single target; auto-indexes refused for `DROP INDEX` |
| [`BEGIN`](#transactions) / [`COMMIT`](#transactions) / [`ROLLBACK`](#transactions) | Snapshot-based; single-level; WAL-backed commit; auto-rollback on COMMIT disk failure |
| [`VACUUM`](#vacuum) | Compacts the file: rewrites every live B-Tree contiguously from page 1 and clears the freelist. Bare `VACUUM;` only — no modifiers. |

Statements the parser accepts (because sqlparser understands them in the SQLite dialect) but SQLRite doesn't execute yet return `SQL Statement not supported yet`. The [Not yet supported](#not-yet-supported) section below enumerates the common ones.

---

## `CREATE TABLE`

```sql
CREATE TABLE <name> (<col> <type> [column_constraint]* [, ...]);
```

### Column types

| Keyword(s) | Storage class | Notes |
|---|---|---|
| `INTEGER`, `INT`, `BIGINT`, `SMALLINT` | Integer (i64) | All four alias to the same 64-bit signed storage class |
| `TEXT`, `VARCHAR` | Text (String) | UTF-8; no length limit enforced (VARCHAR's `(n)` is parsed and ignored) |
| `REAL`, `FLOAT`, `DOUBLE`, `DECIMAL` | Real (f64) | Double-precision; `DECIMAL(p,s)` precision/scale parsed and ignored |
| `BOOLEAN` | Boolean | Stored compactly in the null bitmap's sibling bits; accepts `TRUE` / `FALSE` |
| `VECTOR(N)` | Vector (Vec\<f32\>, fixed dim N) | **Phase 7a.** Dense f32 array of fixed dimension. `N` is required and must be ≥ 1. Inserted as bracket-array literals `[0.1, 0.2, ...]`. Dimension is enforced at INSERT/UPDATE; mismatched-length values are rejected. Distance functions and ANN indexing land in 7b–7d. |
| `JSON`, `JSONB` | Text (canonical JSON) | **Phase 7e.** JSON document stored as canonical UTF-8 text — same as SQLite's JSON1 extension (Q3 scope correction since bincode was removed in Phase 3c). INSERT/UPDATE values are validated via `serde_json::from_str`; malformed JSON is rejected with a typed error and no row is written. `JSONB` is accepted as an alias for `JSON` (PostgreSQL convention; both store as text in our case). Path-style read access via the `json_extract` / `json_type` / `json_array_length` / `json_object_keys` functions below. |

### Column constraints

- `PRIMARY KEY` — one column per table; the column **must** be `INTEGER` and gets auto-ROWID behavior (omitted on INSERT → auto-assigned). Auto-creates an index named `sqlrite_autoindex_<table>_<column>`.
- `UNIQUE` — enforced at INSERT/UPDATE time. Auto-creates an index with the same naming scheme.
- `NOT NULL` — rejects NULL at INSERT/UPDATE. Omitted columns on INSERT are NULL by default (or pick up the column's `DEFAULT`, if any), so a `NOT NULL` without an INSERT-time value or DEFAULT is an error.
- `DEFAULT <literal>` — value substituted when the column is omitted from an INSERT. Accepts integer / real / text / boolean / NULL literals (and unary `+` / `-` on numerics). Function-call defaults like `CURRENT_TIMESTAMP` and other non-literal expressions are rejected at CREATE TABLE time. Explicit `INSERT ... VALUES (..., NULL, ...)` is preserved as NULL — the default only fires for omitted columns (matches SQLite).

### What's **not** enforced at CREATE TABLE time

- **Table-level constraints** (`PRIMARY KEY (col1, col2)`, `FOREIGN KEY`, `CHECK`, `UNIQUE (col1, col2)`) are parsed but ignored.
- **Multi-column `PRIMARY KEY`** — only single-column PKs work; a composite PK is accepted by the parser but treated as no PK.

### Errors returned

- `Table 'foo' already exists.` — duplicate `CREATE TABLE`.
- `'sqlrite_master' is a reserved name used by the internal schema catalog` — you tried to shadow the catalog table.
- `Column 'foo' appears more than once in the table definition` — duplicate column names.
- `PRIMARY KEY column must be INTEGER` — PK on a non-integer column.

---

## `CREATE INDEX`

```sql
CREATE [UNIQUE] INDEX [IF NOT EXISTS] <name> ON <table> (<column>);
```

- Single-column only. Composite indexes (`CREATE INDEX ... ON t (a, b)`) are parsed but rejected at execution.
- The index name is **required**. Anonymous (`CREATE INDEX ON t (col)`) is rejected with `anonymous indexes are not supported`.
- Supported column types: `INTEGER`, `TEXT`. `REAL` and `BOOLEAN` columns cannot be indexed yet.
- `CREATE UNIQUE INDEX` on a column whose existing rows already carry duplicate values is rejected before any change is made — the table + other indexes stay untouched.
- `IF NOT EXISTS` — skips the create if an index with that name already exists. No-op return value in that case.
- Indexes persist as their own cell-based B-Trees (see [Storage model](storage-model.md)).

### Auto-indexes

Every `PRIMARY KEY` and every `UNIQUE` column gets an auto-index at `CREATE TABLE` time:

```
sqlrite_autoindex_<table>_<column>
```

These are full-citizen indexes — they're visible via `.tables`-adjacent catalog queries (once those land), persist across saves, and accelerate equality probes. You don't need to `CREATE INDEX` them yourself.

### HNSW indexes (Phase 7d)

```sql
CREATE INDEX <name> ON <table> USING hnsw (<vector_column>);
```

Builds an [HNSW](https://arxiv.org/abs/1603.09320) approximate-nearest-neighbor index over a `VECTOR(N)` column. The query optimizer recognizes `ORDER BY vec_distance_l2(col, literal) LIMIT k` (or the cosine / dot variants) on an HNSW-indexed column and probes the graph instead of full-scanning.

- Recall@10 ≥ 0.95 at default parameters (`M=16`, `ef_construction=200`, `ef_search=50`). Parameters aren't tunable from SQL yet — see Q2 of [`docs/phase-7-plan.md`](phase-7-plan.md).
- The index is built incrementally on `INSERT`. `DELETE` / `UPDATE` mark the index `needs_rebuild`; the next save rebuilds from current rows.
- Persisted as a `KIND_HNSW` cell tree alongside the regular page hierarchy — open path loads the graph bit-for-bit, no algorithm runs.
- Without an HNSW index, the same `ORDER BY vec_distance_… LIMIT k` query still works — it just brute-force-scans every row (Phase 7c's bounded-heap top-k optimization keeps the memory footprint to O(k)).

### FTS indexes (Phase 8)

```sql
CREATE INDEX <name> ON <table> USING fts (<text_column>);
```

Builds an FTS5-style inverted index with BM25 ranking over a `TEXT` column. Pairs with the [`fts_match`](#fts_match) and [`bm25_score`](#bm25_score) functions for keyword retrieval, and composes with `vec_distance_*` for hybrid retrieval (see [`docs/fts.md`](fts.md)).

- **TEXT columns only.** `INTEGER`, `REAL`, `BOOLEAN`, `VECTOR`, `JSON` columns are rejected at CREATE-INDEX time.
- **Single-column only.** Multi-column FTS is deferred to Phase 8.1.
- **`UNIQUE` is rejected** — UNIQUE has no meaning for an inverted index.
- The index is built incrementally on `INSERT`. `DELETE` / `UPDATE` mark the index `needs_rebuild`; the next save rebuilds from current rows.
- Persisted as a `KIND_FTS_POSTING` cell tree alongside the regular page hierarchy — open path loads posting lists bit-for-bit, no re-tokenization.
- The first save of a database with at least one FTS index promotes the file format from v4 to v5 (on-demand bump per Phase 8 plan Q10).
- Tokenizer is ASCII MVP per Q3: `[^A-Za-z0-9]+` split, lowercased, no stemming, no stop list.
- BM25 parameters are fixed at SQLite FTS5's defaults (`k1 = 1.5`, `b = 0.75`).

---

## `INSERT INTO`

```sql
INSERT INTO <name> (col1, col2, ...) VALUES (v1, v2, ...)
                                  [, (v1, v2, ...) ...];
```

- **Explicit column list is required.** Value-list-only inserts (`INSERT INTO t VALUES (...)`) are not supported yet.
- **`INTEGER PRIMARY KEY` auto-ROWID** — omit the PK column and a ROWID is auto-assigned (max existing + 1, starting at 1).
- **Multi-row inserts** — the parser accepts `VALUES (...), (...), (...)`, and SQLRite runs each row through the type + UNIQUE checks in order. A failure mid-batch leaves the already-inserted rows in place.
- **NULL padding** — columns not named in the column list default to NULL. `NOT NULL` columns must appear in the list (or be the omitted PK).
- **Type validation** happens at INSERT time. A mismatched literal (`INSERT INTO t (age) VALUES ('not-a-number')` where `age` is `INTEGER`) is rejected with a typed error — no panic, no partial write.
- **UNIQUE enforcement** runs *before* any row insert so a failing batch doesn't leave partial state.

### Value literals accepted

| Literal | Example |
|---|---|
| Integer | `42`, `-5`, `0` |
| Real | `3.14`, `-0.001`, `1e10` |
| Text | `'single-quoted'` — doubled single quotes escape: `'it''s'` |
| Boolean | `TRUE`, `FALSE` (case-insensitive) |
| NULL | `NULL` (case-insensitive) |
| Vector | `[0.1, 0.2, 0.3]` — JSON-style bracket-array; integer elements widen to f32 (`[1, 2, 3]` is valid). For `VECTOR(N)` columns; dimension must match the declared `N`. *(Phase 7a)* |

Hex literals, blob literals, and date/time functions are not supported.

---

## `SELECT`

```sql
SELECT {* | col1, col2, ...}
FROM <table>
  [WHERE <expr>]
  [ORDER BY <col> [ASC|DESC]]
  [LIMIT <non-negative-integer>];
```

### What works

- **Projection**: `*` (all columns in declaration order) or a bare column list. Columns not declared on the table are rejected.
- **`WHERE`**: any [expression](#expressions). Evaluated per row; NULL-as-false in WHERE context (three-valued logic collapsed to two-valued for filtering).
- **`ORDER BY`**: single sort key, `ASC` (default) or `DESC`. The sort key can be a bare column reference OR any expression — including function calls — so KNN queries like `ORDER BY vec_distance_l2(embedding, [...]) LIMIT k` work end-to-end *(Phase 7b)*. Sort key types must match; mixing `INTEGER` and `TEXT` across rows under a single `ORDER BY` is a runtime error.
- **`LIMIT`**: non-negative integer literal. `LIMIT 0` is valid (returns zero rows).

### Index probing

The executor includes a tiny optimizer: if the `WHERE` is exactly `<indexed_col> = <literal>` or `<literal> = <indexed_col>`, it probes the index and scans only matching rows. Mixed predicates (`WHERE a = 1 AND b > 2`), range predicates (`WHERE a > 1`), and OR-combined predicates fall back to a full table scan.

### What doesn't work

- **Joins** of any kind (`INNER`, `LEFT OUTER`, `CROSS`, comma-join)
- **Subqueries**, CTEs (`WITH`), views
- **`GROUP BY`**, aggregate functions (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`), `HAVING`
- **`DISTINCT`**
- **`LIKE`**, **`IN`**, **`IS NULL`** / **`IS NOT NULL`**, `BETWEEN`
- **Expressions in the projection list** (`SELECT age + 1 FROM users`) — projection is bare column references only
- **Multi-column `ORDER BY`**, `NULLS FIRST/LAST` (single sort key only; the sort key itself can be an expression as of Phase 7b)
- **`OFFSET`**
- **Column aliases** (`SELECT name AS n FROM users`)

Any of the above reaches the executor as a parsed AST node that execution doesn't handle, producing either `NotImplemented` or a more specific error (e.g., `joins are not supported`).

---

## `UPDATE`

```sql
UPDATE <table> SET col1 = <expr> [, col2 = <expr>]* [WHERE <expr>];
```

- **Multi-column `SET`** — separate assignments with commas.
- **RHS is a full expression** — can reference other columns of the same row:
  ```sql
  UPDATE users SET age = age + 1, updated_at = 'now' WHERE id = 42;
  ```
- **Type enforcement** — the declared column type of each target is checked against the assigned expression's result. Mismatch is a clean error; the row (and all other rows that would have been updated by the same statement) stays untouched.
- **UNIQUE enforcement** — if the update would collide with another row's value on a UNIQUE / PRIMARY KEY column, the whole statement is rejected before any write. No partial updates.
- **NULL assignments** respect `NOT NULL` — `SET col = NULL` on a `NOT NULL` column errors.

---

## `DELETE`

```sql
DELETE FROM <table> [WHERE <expr>];
```

- **No `WHERE`** deletes every row (tables and indexes are preserved; only row data is removed).
- **`WHERE`** uses the same [expression](#expressions) evaluator as `SELECT`.
- Secondary indexes are updated alongside the row deletes so a subsequent `WHERE col = ...` doesn't return stale hits.

---

## `ALTER TABLE`

```sql
ALTER TABLE [IF EXISTS] <table> RENAME TO <new_table>;
ALTER TABLE [IF EXISTS] <table> RENAME COLUMN <old_col> TO <new_col>;
ALTER TABLE [IF EXISTS] <table> ADD COLUMN <col_def>;
ALTER TABLE [IF EXISTS] <table> DROP COLUMN <col>;
```

One operation per statement (SQLite-style). `ALTER TABLE foo RENAME TO bar, ADD COLUMN x ...` is rejected — issue separate statements instead.

### `RENAME TO`

- Reserved-name rejection: cannot rename to `sqlrite_master`.
- Errors if the target name is already a table.
- Auto-indexes whose names embed the old table name (`sqlrite_autoindex_<old>_<col>`) are renamed in lockstep so the schema catalog stays consistent. Explicit indexes carry their user-given name unchanged.

### `RENAME COLUMN`

- Errors if the old column doesn't exist or the new name already exists in the table.
- Re-keys the row storage and updates every dependent index (auto + explicit, secondary / HNSW / FTS) — including auto-index name regeneration.
- Renaming the PRIMARY KEY column is allowed; the table's `primary_key` pointer follows the new name.

### `ADD COLUMN`

- The column definition reuses the same parser that handles CREATE TABLE columns: same types, same `NOT NULL` / `DEFAULT` semantics.
- **Rejected:** `PRIMARY KEY` and `UNIQUE` constraints on the added column. Both would require backfilling the column under uniqueness constraints against existing rows; that path will land alongside multi-column UNIQUE.
- **`NOT NULL` on a non-empty table requires `DEFAULT`.** Without one there's no value to backfill existing rowids with. Same rule SQLite applies.
- With a `DEFAULT`, every existing rowid is backfilled with the default value at ADD COLUMN time. Without a `DEFAULT`, existing rowids read as NULL for the new column.

### `DROP COLUMN`

- **Rejected:** dropping the PRIMARY KEY column.
- **Rejected:** dropping the only remaining column (degenerate table).
- Cascades to every dependent index (auto + explicit, secondary / HNSW / FTS) on the dropped column.
- `CASCADE` / `RESTRICT` modifiers are accepted by the parser and ignored — SQLite has no real distinction here either.

---

## `DROP TABLE`

```sql
DROP TABLE [IF EXISTS] <table>;
```

- Single target per statement. `DROP TABLE a, b, c;` is parsed but rejected with a NotImplemented error.
- Reserved-name rejection: `DROP TABLE sqlrite_master` errors with the same message `CREATE TABLE` uses.
- All indexes attached to the table (auto, explicit, HNSW, FTS) disappear with the table — they live inside the `Table` struct and ride along.
- Without `IF EXISTS`, dropping a table that doesn't exist errors. With it, that's a benign 0-tables-dropped no-op.
- **Disk pages move onto the freelist.** Pages the dropped table occupied are pushed onto a persisted free-page list (SQLR-6) so subsequent `CREATE TABLE` or inserts can reuse them. The file doesn't shrink until [`VACUUM;`](#vacuum) compacts it.

---

## `DROP INDEX`

```sql
DROP INDEX [IF EXISTS] <index_name>;
```

- Single target per statement.
- Walks every table searching for an index with the given name across the secondary B-Tree, HNSW, and FTS index families.
- **Refuses to drop auto-indexes.** `sqlrite_autoindex_*` names are constraint-bound to the column they index — the only way to remove them is to drop the underlying column or table. Same rule SQLite enforces for its `sqlite_autoindex_*` indexes.
- `IF EXISTS` makes a missing index a benign no-op.

---

## Expressions

Expressions work inside `WHERE` (both in `SELECT`, `UPDATE`, `DELETE`) and on the right-hand side of `UPDATE`'s `SET`.

### Operators

| Category | Operators |
|---|---|
| Comparison | `=`, `<>`, `<`, `<=`, `>`, `>=` |
| Logical | `AND`, `OR`, `NOT` |
| Arithmetic | `+`, `-`, `*`, `/`, `%` |
| String | `\|\|` (concatenation) |
| Unary | `+`, `-` |
| Grouping | Parentheses |

### Literals

Same set accepted by `INSERT` (see [Value literals accepted](#value-literals-accepted)).

### Built-in functions

| Function | Returns | Notes |
|---|---|---|
| `vec_distance_l2(a, b)` | Real (f64) | Euclidean distance √Σ(aᵢ−bᵢ)². Smaller is closer. *(Phase 7b)* |
| `vec_distance_cosine(a, b)` | Real (f64) | Cosine distance `1 − (a·b) / (‖a‖·‖b‖)`. Errors on zero-magnitude vectors (cosine is undefined). Smaller is closer; identical vectors return 0.0, orthogonal vectors return 1.0. *(Phase 7b)* |
| `vec_distance_dot(a, b)` | Real (f64) | Negated dot product `−(a·b)`. Negation makes "smaller is closer" consistent with the others. For unit-norm vectors equals `vec_distance_cosine(a, b) - 1`. *(Phase 7b)* |
| `json_extract(json, path)` | Depends on the resolved node | Walks `path` over `json` and returns the resolved value coerced to the closest SQL type — JSON strings → `TEXT`, numbers → `INTEGER` / `REAL`, booleans → `BOOLEAN`, `null` → `NULL`, and composites (`object` / `array`) → their canonical JSON-text serialization. Path defaults to `$` when only one argument is supplied. A path that doesn't resolve returns `NULL`. *(Phase 7e)* |
| `json_type(json[, path])` | Text | One of `'object'`, `'array'`, `'string'`, `'integer'`, `'real'`, `'true'`, `'false'`, `'null'`. Path defaults to `$`. *(Phase 7e)* |
| `json_array_length(json[, path])` | Integer | Number of elements in the JSON array at `path`. Errors if the resolved node is not an array. Path defaults to `$`. *(Phase 7e)* |
| `json_object_keys(json[, path])` | Text (JSON-array string) | Returns the object's keys as a JSON-array text in insertion order — e.g. `'["a","b","c"]'`. Path defaults to `$`. **Diverges from SQLite**, which exposes keys as a *table-valued* function (one row per key). SQLRite has no set-returning functions yet, so we return the keys as a JSON array and let callers parse if needed. *(Phase 7e)* |
| `fts_match(col, 'q')` | Bool | True iff the row contains at least one tokenized query term in `col`. Requires an FTS index on `col`; errors otherwise. Tokenization rules: ASCII split + lowercase, no stemming, no stop-list. Multi-token queries use any-term (OR) semantics. *(Phase 8b)* |
| `bm25_score(col, 'q')` | Real (f64) | Per-row BM25 relevance score for the given query. Higher is more relevant. Requires an FTS index on `col`. Pairs with `fts_match` in the canonical `WHERE … ORDER BY bm25_score(...) DESC LIMIT k` shape, which the optimizer probes via the inverted index instead of scanning rows. *(Phase 8b)* |

All three vector-distance functions take exactly two arguments, both of which must be vectors of the same dimension. Either argument can be a column reference (`embedding`), a bracket-array literal (`[0.1, 0.2, 0.3]`), or any sub-expression that evaluates to a vector. Mismatched dimensions error with `vector dimensions don't match (lhs=N, rhs=M)`.

The KNN ranking pattern that motivates this set:

```sql
SELECT id, title FROM docs
ORDER BY vec_distance_l2(embedding, [0.1, 0.2, ..., 0.0])
LIMIT 10;
```

> **Operator forms (`<->` `<=>` `<#>`) are not supported yet.** They're the de facto pgvector convention but blocked on a sqlparser limitation — will land as a Phase 7b.1 follow-up. Use the function-call form for now.

#### JSON path syntax

The `json_*` functions accept a string path argument with a small subset of JSONPath:

| Token | Meaning |
|---|---|
| `$` | Root of the document (default if path is omitted). |
| `.key` | Object member access. Bare keys only — no quoted-string variant yet. |
| `[N]` | Array index (0-based). Negative indices are not supported. |

Tokens chain naturally: `$.user.tags[0]`, `$[2].name`, `$.matrix[1][0]`. A malformed path (unbalanced brackets, missing `$`) errors at runtime with a typed message; a well-formed path that simply doesn't resolve returns `NULL`.

```sql
CREATE TABLE events (id INTEGER PRIMARY KEY, payload JSON);

INSERT INTO events (payload) VALUES
  ('{"user": {"name": "alice", "tags": ["admin", "ops"]}, "score": 42}'),
  ('{"user": {"name": "bob", "tags": []}, "score": 7}');

SELECT id,
       json_extract(payload, '$.user.name') AS name,
       json_extract(payload, '$.user.tags[0]') AS first_tag,
       json_array_length(payload, '$.user.tags') AS tag_count,
       json_type(payload, '$.score') AS score_type
  FROM events
 WHERE json_extract(payload, '$.user.name') = 'alice';
```

#### FTS query patterns

```sql
CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);
INSERT INTO docs (body) VALUES ('rust embedded database');
INSERT INTO docs (body) VALUES ('postgres relational database server');
CREATE INDEX docs_fts ON docs USING fts (body);

-- Lexical filter (Boolean predicate).
SELECT id FROM docs WHERE fts_match(body, 'database');

-- Top-k by BM25 relevance — the optimizer probes the inverted index
-- when WHERE / ORDER BY share the same query string and direction.
SELECT id FROM docs
 WHERE fts_match(body, 'embedded database')
 ORDER BY bm25_score(body, 'embedded database') DESC
 LIMIT 5;
```

See [`docs/fts.md`](fts.md) for the canonical FTS reference (tokenizer rules, BM25 parameters, persistence, hybrid retrieval with `vec_distance_*`).

### Type coercion in arithmetic

- **Integer-only ops stay integer.** `1 + 2` → `3` (Integer).
- **Any `REAL` operand promotes to `f64`.** `1 + 2.0` → `3.0` (Real).
- **Divide/modulo by zero** returns a typed runtime error rather than panicking: `division by zero` for `/` and `%`.
- **`TEXT` in arithmetic context** errors — `'hello' + 1` is not silently coerced.

### NULL handling

SQLRite follows standard SQL three-valued logic:

- **Comparisons involving NULL** (`NULL = 1`, `1 < NULL`) evaluate to unknown, which behaves as `false` inside `WHERE`. Neither the NULL = NULL equality nor the NULL <> NULL inequality is true — use `IS NULL` / `IS NOT NULL` for explicit null tests (both **not yet supported**).
- **Logical operators with NULL**: `NULL AND false` → `false`, `NULL AND true` → `NULL`, `NULL OR true` → `true`, `NOT NULL` → `NULL`. The short-circuit rules prevent NULL from propagating when one operand already decides the result.
- **Arithmetic with NULL**: any operand NULL → result NULL. `NULL + 1` → `NULL`.
- **String concat with NULL**: `'foo' || NULL` → `NULL` (same propagation as arithmetic).

### Case sensitivity

- **Keywords** (`SELECT`, `FROM`, `AND`, `TRUE`, `NULL`, …) are case-insensitive. `select`, `SELECT`, `SeLeCt` all parse.
- **Identifiers** (table names, column names) are **case-sensitive** — no normalization is applied at definition or lookup time. `CREATE TABLE Users (…)` followed by `SELECT * FROM users` fails with `Table doesn't exist`. (This is the opposite of SQLite's default; we'll revisit once the cursor refactor in Phase 5 lands.)
- **String literals** preserve case: `'Alice'` stays `Alice`.

---

## Transactions

```sql
BEGIN;
  INSERT INTO users (name) VALUES ('alice');
  UPDATE counters SET n = n + 1 WHERE name = 'signups';
COMMIT;
```

Or:

```sql
BEGIN;
  DELETE FROM users WHERE banned = TRUE;
ROLLBACK;  -- nothing was actually deleted
```

### Semantics

- **`BEGIN`** deep-clones the in-memory database into a snapshot held on `db.txn`. Auto-save is **suppressed** while the transaction is open — mutations accumulate in memory.
- **`COMMIT`** flushes every accumulated change to the WAL in one atomic commit frame and drops the snapshot. Readers of the file after COMMIT see all of the transaction's changes at once.
- **`ROLLBACK`** replaces the live state with the snapshot and drops the snapshot. Nothing hits disk.

### Details that matter

- **Nested `BEGIN` is rejected** with `a transaction is already open`. No savepoints yet.
- **`BEGIN` on a read-only database** (`sqlrite --readonly foo.sqlrite`) is rejected with `cannot execute: database is opened read-only`.
- **Runtime errors mid-transaction do NOT auto-rollback.** If an `INSERT` fails inside a transaction (UNIQUE violation, type mismatch, bad syntax), the transaction stays open. The caller decides whether to `ROLLBACK` or `COMMIT` whatever succeeded before the failure.
- **`COMMIT`'s disk write failing DOES auto-rollback.** If the save at COMMIT time errors (disk full, permission denied, checksum mismatch), SQLRite restores the pre-BEGIN snapshot and surfaces `COMMIT failed — transaction rolled back: <underlying error>`. Leaving in-flight mutations live after a failed COMMIT would be unsafe — any subsequent non-transactional statement's auto-save would silently publish partial work.
- **Cost**: `BEGIN` is `O(N)` in the total size of the in-memory database because of the snapshot clone. On a huge database, opening a transaction just to run a single read-only query is wasteful — use a plain `SELECT` instead.
- **Visibility to other processes**: with POSIX file locks (Phase 4a–4e), a writer excludes all concurrent readers anyway, so "uncommitted transaction state leaking to a concurrent reader" isn't a concern — no concurrent reader exists during an open transaction.

---

## `VACUUM`

```sql
VACUUM;
```

Compacts the database file: rewrites every live table, index, HNSW graph, FTS posting tree, and `sqlrite_master` itself contiguously from page 1, drops the freelist, and lets the next checkpoint truncate the tail.

- **Bare `VACUUM;` only.** Modifiers — `VACUUM FULL`, `VACUUM REINDEX`, table targets, `TO ... PERCENT`, `BOOST` — are parsed (sqlparser supports them) but rejected at execution with `VACUUM modifiers (FULL, REINDEX, table targets, etc.) are not supported`.
- **Refused inside a transaction.** `BEGIN; VACUUM;` errors with `VACUUM cannot run inside a transaction`. Use `COMMIT;` first, then `VACUUM;`.
- **No-op on in-memory databases.** Returns a `VACUUM is a no-op for in-memory databases` status string and does nothing — there's no file to compact.
- **Status string** carries pages and bytes reclaimed: `VACUUM completed. <N> pages reclaimed (<B> bytes).`
- **Format-version side effect.** A v4/v5 file that has been promoted to v6 by an earlier drop stays at v6 after VACUUM (v6 is a strict superset; we don't downgrade). A file that's already at v4/v5 because no drop ever happened on it doesn't get bumped by VACUUM.

When to run it: any time after a string of `DROP TABLE` / `DROP INDEX` / `ALTER TABLE DROP COLUMN` operations if you care about file size. SQLRite reuses freelist pages on subsequent inserts, so a write-heavy workload may not need VACUUM at all — its main use is reclaiming space when you don't expect to grow back.

---

## Read-only databases

A REPL launched with `sqlrite --readonly foo.sqlrite` (or `sqlrite::open_database_read_only(path, name)` programmatically) takes a shared POSIX advisory lock instead of an exclusive one. In that mode:

- `SELECT` works normally.
- Every write statement (`INSERT`, `UPDATE`, `DELETE`, `CREATE TABLE`, `CREATE INDEX`) is rejected **before** touching memory with `cannot execute: database is opened read-only`. The in-memory state never diverges from disk.
- `BEGIN` is rejected.
- Multiple read-only openers of the same file coexist (shared flock). Any read-write opener blocks all read-only openers and vice versa — POSIX's "many readers OR one writer, not both" semantics.

---

## Statement-level rules

- **One statement per call** — `process_command` / `Connection::execute` expects a single statement. Multi-statement strings (`"INSERT …; INSERT …;"`) are rejected with `Expected a single query statement, but there are N`. For multi-statement execution, use the SDK's `executescript` / `execute_batch` helpers (Phases 5c/5d).
- **Trailing semicolons** are optional. Both `SELECT 1` and `SELECT 1;` parse.
- **Empty / comment-only input** is a benign no-op — no error, no auto-save triggered.
- **Multi-line statements** work. The REPL (via rustyline) buffers continuation lines until a terminating semicolon is seen.

---

## Not yet supported

For context when you hit `NotImplemented`. See [Roadmap](roadmap.md) for when these land:

### Joins & composition
- `INNER` / `LEFT OUTER` / `RIGHT OUTER` / `CROSS JOIN`, comma joins
- Subqueries (scalar, `IN (SELECT ...)`, correlated)
- CTEs (`WITH`), recursive CTEs
- Views (`CREATE VIEW`)

### Aggregation & grouping
- `GROUP BY`, `HAVING`
- Aggregate functions (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `GROUP_CONCAT`)
- `DISTINCT`

### Predicate & expression
- `LIKE`, `GLOB`, `REGEXP`
- `IN (...)`, `NOT IN`, `BETWEEN`
- `IS NULL`, `IS NOT NULL` (pending — use `col = NULL` is NOT a workaround since it's always false; the only current way to select NULL rows is to rely on the NULL-as-false-in-WHERE behavior being absent when the column isn't referenced)
- `CASE WHEN ... THEN ... END`
- Expressions in the `SELECT` projection list
- Column aliases (`AS`)
- Built-in functions (`LENGTH`, `UPPER`, `LOWER`, `COALESCE`, `IFNULL`, date/time, `printf`, …)

### DDL
- `ALTER TABLE` extras: multi-operation (`ALTER TABLE foo RENAME TO bar, ADD COLUMN x ...`), `ALTER COLUMN ... SET / DROP DEFAULT`, `ALTER COLUMN ... TYPE`
- `ADD COLUMN` constraint extras: `PRIMARY KEY` and `UNIQUE` on the added column (would need backfill + uniqueness against existing rows)
- `DROP TABLE` / `DROP INDEX` extras: multi-target (`DROP TABLE a, b, c;`)
- `CREATE VIEW`, `CREATE TRIGGER`
- Table-level constraints (composite PK, composite UNIQUE, `FOREIGN KEY`, `CHECK`)
- Non-literal `DEFAULT` expressions (`CURRENT_TIMESTAMP`, function calls, column references)
- Composite / multi-column indexes

### Transactions
- Savepoints (`SAVEPOINT`, `RELEASE SAVEPOINT`, `ROLLBACK TO SAVEPOINT`)
- Isolation-level control (`BEGIN IMMEDIATE`, `BEGIN EXCLUSIVE`)

### Query shape
- `OFFSET`
- Multi-column `ORDER BY`
- `UNION`, `INTERSECT`, `EXCEPT`
- `INSERT ... SELECT`
- `UPDATE ... FROM`, `DELETE ... USING`

### Session / schema
- Multiple attached databases (`ATTACH DATABASE`, `DETACH DATABASE`)
- `PRAGMA` statements beyond what the parser accepts (none currently executed)
- `REPLACE INTO`, `INSERT OR IGNORE`, `INSERT OR REPLACE` (conflict-resolution clauses)

---

## Cross-reference

- [Using SQLRite](usage.md) — REPL flow, meta-commands, history, read-only mode
- [Embedding](embedding.md) — the `Connection` / `Statement` / `Rows` API surfacing the same SQL
- [Storage model](storage-model.md) — how columns, rows, and indexes live in memory and on disk
- [SQL engine](sql-engine.md) — how a query flows from tokens to executor to rows
- [Roadmap](roadmap.md) — when each [Not yet supported](#not-yet-supported) entry lands
