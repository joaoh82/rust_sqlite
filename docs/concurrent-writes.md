# Concurrent writes — MVCC + `BEGIN CONCURRENT`

User-facing reference for SQLRite's multi-version concurrency control. For the original design discussion + sequencing decisions, see [`concurrent-writes-plan.md`](concurrent-writes-plan.md); this doc covers the *shipped* surface as of Phase 11.11a (May 2026).

---

## TL;DR

```sql
PRAGMA journal_mode = mvcc;            -- once per database
BEGIN CONCURRENT;
UPDATE accounts SET balance = balance - 50 WHERE id = 1;
UPDATE accounts SET balance = balance + 50 WHERE id = 2;
COMMIT;                                -- may return Busy → caller retries
```

Two writers on *disjoint* rows now make progress in parallel; two writers on the *same* row see the second commit fail fast with [`SQLRiteError::Busy`](../src/error.rs), which the caller retries. The data structure backing this is a per-row in-memory version chain ([`MvStore`](../src/mvcc/store.rs)) sitting in front of the existing pager; the on-disk format is unchanged — durability piggybacks on the WAL via a new `MvccCommitBatch` frame (Phase 11.9). Reads inside a `BEGIN CONCURRENT` transaction see a stable BEGIN-time snapshot.

The story is the same one Turso ships with `--experimental-mvcc`, narrowed for SQLRite's single-process scope.

---

## Why MVCC

SQLite (and pre-Phase-11 SQLRite) serializes every writer through a single exclusive lock. Two writers touching *unrelated rows* still wait on each other — the lock is page- or file-granularity, not row-granularity. For workloads where most writes don't actually conflict, that's throughput left on the table.

Phase 11 replaces the lock-for-the-whole-transaction model with **optimistic concurrency control**: writes run against a per-transaction snapshot; the engine only checks for conflicts at `COMMIT`, and only on the row IDs the transaction actually touched. The shape is straight out of [Hekaton (Larson et al., VLDB 2011)](https://www.microsoft.com/en-us/research/wp-content/uploads/2011/01/main-mem-cc-techreport.pdf).

What you get:

- **Disjoint-row writers run in parallel.** A `BEGIN CONCURRENT` on connection A and one on connection B can both progress; commit ordering is decided by a process-wide logical clock, not by lock acquisition.
- **Snapshot-isolated reads.** A reader inside `BEGIN CONCURRENT` sees the database as it was at BEGIN time, regardless of what other writers commit in the meantime.
- **Row-level conflict detection.** The unit of conflict is `(table, rowid)`, not a page or a table.
- **Same on-disk format.** Existing `.sqlrite` files open unchanged. The toggle is `PRAGMA journal_mode = mvcc;`.

What you don't get (v0; see [Limitations](#limitations)):

- Cross-process MVCC. The version index is in-memory only; multi-process writers still serialize through the pager's `flock`.
- `CREATE INDEX` while `journal_mode = mvcc`. Index maintenance under MVCC is Phase 11.10 (deferred-by-design).
- DDL inside `BEGIN CONCURRENT`. Rejected with a typed error; commit your DDL outside the concurrent transaction.

---

## Quick start

```sql
-- 1. Opt the database into MVCC. Per-database setting; survives reopens.
PRAGMA journal_mode = mvcc;

-- 2. Multi-row update inside a concurrent transaction.
BEGIN CONCURRENT;
INSERT INTO orders (id, customer, total) VALUES (1, 'alice', 100);
UPDATE inventory SET stock = stock - 1 WHERE sku = 'WIDGET-A';
COMMIT;

-- 3. If two concurrent transactions touch the same row, the second
--    commit fails with Busy. Retry with a fresh BEGIN CONCURRENT.
```

The same end-to-end thing from Rust:

```rust
use sqlrite::{Connection, SQLRiteError};

let mut conn = Connection::open("orders.sqlrite")?;
conn.execute("PRAGMA journal_mode = mvcc")?;

loop {
    conn.execute("BEGIN CONCURRENT")?;
    conn.execute("INSERT INTO orders (id, customer, total) VALUES (1, 'alice', 100)")?;
    conn.execute("UPDATE inventory SET stock = stock - 1 WHERE sku = 'WIDGET-A'")?;
    match conn.execute("COMMIT") {
        Ok(_) => break,
        Err(e) if e.is_retryable() => {
            conn.execute("ROLLBACK").ok();
            continue;
        }
        Err(e) => return Err(e.into()),
    }
}
# Ok::<(), sqlrite::SQLRiteError>(())
```

[`SQLRiteError::is_retryable`](../src/error.rs) covers both `Busy` (write-write conflict at commit) and `BusySnapshot` (the snapshot the read path expected has been GC'd) — see [Error semantics](#error-semantics).

A complete runnable version of this loop lives in [`examples/rust/concurrent_writers.rs`](../examples/rust/concurrent_writers.rs).

---

## Conceptual model

### The version chain

For every `(table, rowid)` SQLRite has touched under `BEGIN CONCURRENT`, the [`MvStore`](../src/mvcc/store.rs) holds an ordered chain of `RowVersion`s:

```
                 begin=ts1                 begin=ts3                  begin=ts7
                 end=Some(ts3)             end=Some(ts7)              end=None
                ┌────────────┐           ┌────────────┐             ┌────────────┐
   rowid 42  ─→ │ Present {  │ ──next──→ │ Present {  │ ──next────→ │ Tombstone   │
                │  balance:  │           │  balance:  │             │ (DELETE)   │
                │   100      │           │   150      │             │            │
                │ }          │           │ }          │             │            │
                └────────────┘           └────────────┘             └────────────┘
```

A version is **visible** to a transaction with begin-timestamp `T` when `begin <= T < end` (the textbook snapshot-isolation rule). New writes push a new head onto the chain at commit time, capping the previous latest version's `end` to the new `commit_ts`.

### Timestamps come from a process-wide logical clock

[`MvccClock`](../src/mvcc/clock.rs) is an `AtomicU64` that hands out `begin_ts` at `BEGIN CONCURRENT` and `commit_ts` at the start of validation. The clock's high-water mark is persisted in the WAL header (Phase 11.2's WAL v2) and seeded past the highest replayed `commit_ts` on reopen (Phase 11.9), so timestamps don't reuse the same value across restarts.

### Commit-time validation

When a `BEGIN CONCURRENT` transaction commits, the engine:

1. Allocates a `commit_ts` from the clock.
2. Walks the write-set. For each `(table, rowid)`, if any committed version's `begin > tx.begin_ts`, somebody else superseded us → return `SQLRiteError::Busy`.
3. Otherwise, for each row in the write-set, push a new `RowVersion` onto the chain at `commit_ts`, capping the previous latest's `end`.
4. Append an `MvccCommitBatch` frame to the WAL; the legacy page-commit's fsync covers it (Phase 11.9).
5. Mirror the writes into `Database::tables` so the legacy read path stays correct after commit.
6. Drop the transaction's `TxHandle` and run a per-commit GC sweep over the write-set's chains.

### Reads

Reads via [`Statement::query`](../src/connection.rs) (Phase 11.5) consult `MvStore` first when a `BEGIN CONCURRENT` is open on the connection. If the row has a version visible at the transaction's `begin_ts`, that's the answer; otherwise the read falls through to the legacy table → pager path. This means a reader inside `BEGIN CONCURRENT` sees a consistent BEGIN-time snapshot for as long as the transaction is open.

Reads *outside* `BEGIN CONCURRENT` still go through the legacy path — they see the latest committed state, exactly as before Phase 11. That's the keystone of the design: nothing about the existing non-concurrent codepath changed; MVCC is layered on top, opt-in.

---

## SQL surface

### `PRAGMA journal_mode`

| Form | Effect |
|---|---|
| `PRAGMA journal_mode;` | Read — returns the current mode as a single-row `wal` / `mvcc` result |
| `PRAGMA journal_mode = mvcc;` | Switch this database into MVCC mode |
| `PRAGMA journal_mode = wal;` | Switch back to the legacy WAL-backed pager |

Case-insensitive on both the pragma name and the value. Quoted values (`'mvcc'`) work; numeric values are rejected. Unknown modes return a typed error.

The setting is **per-database**, not per-connection — every [`Connection::connect`](#sibling-handles) sibling sees the same value. Switching `Mvcc → Wal` is rejected if `MvStore` carries committed versions; call [`Connection::vacuum_mvcc`](#vacuum_mvcc) first to drain the store.

### `BEGIN CONCURRENT`

Opens a concurrent transaction. Requires `PRAGMA journal_mode = mvcc;` first.

```sql
BEGIN CONCURRENT;
-- DML against the per-tx snapshot
SELECT … ;     -- sees BEGIN-time state
INSERT … ;
UPDATE … ;
DELETE … ;
COMMIT;        -- or ROLLBACK
```

Rules (each surfaces as a typed error):

- Plain `BEGIN CONCURRENT` against a `Wal`-mode database is rejected.
- Nested transactions (`BEGIN CONCURRENT` inside an open one, or `BEGIN` inside one) are rejected.
- DDL inside `BEGIN CONCURRENT` is rejected — `CREATE TABLE`, `CREATE INDEX`, `DROP TABLE`, `DROP INDEX`, `ALTER TABLE`, `VACUUM` all bounce, the transaction stays open so the caller can `ROLLBACK`.
- Read-only databases reject `BEGIN CONCURRENT`.

`COMMIT` may surface `SQLRiteError::Busy` or `SQLRiteError::BusySnapshot`. The transaction is dropped on either; the caller's loop should `continue` after a `ROLLBACK`.

### `COMMIT` / `ROLLBACK`

Inside an open `BEGIN CONCURRENT`, plain `COMMIT` validates the write-set and either commits or returns `Busy`. Plain `ROLLBACK` drops the per-tx state and returns control. Both also work outside `BEGIN CONCURRENT` (they fall through to the legacy single-writer transaction control).

---

## Embedding API

### Sibling handles

A single `Connection::open` is the only path that touches the file. Mint additional handles with [`Connection::connect`](../src/connection.rs):

```rust
let primary = Connection::open("orders.sqlrite")?;
let secondary = primary.connect();
let tertiary  = primary.connect();
```

Every sibling shares the same `Arc<Mutex<Database>>`. Each sibling can hold its own independent `BEGIN CONCURRENT` — that's the whole point of multi-handle MVCC. Sibling handles are `Send + Sync`, so it's safe to send them across threads.

Sibling propagation across each SDK (Phase 11.7 + 11.8):

| SDK | Sibling API | Retryable-error type |
|---|---|---|
| C FFI | `sqlrite_connect_sibling(existing, out)` | `SqlriteStatus::Busy` / `BusySnapshot`; `sqlrite_status_is_retryable` |
| Python | `conn.connect()` | `sqlrite.BusyError` / `sqlrite.BusySnapshotError` (both subclass `SQLRiteError`) |
| Node.js | `db.connect()` | `errorKind(message)` returns `'Busy'` / `'BusySnapshot'` / `'Other'` |
| Go | `database/sql` pool + cross-pool path registry (Phase 11.11c) | `errors.Is(err, sqlrite.ErrBusy)` / `ErrBusySnapshot`; `sqlrite.IsRetryable(err)` |
| WASM | *(deferred — single-threaded runtime)* | *(deferred)* |

For Go, every `sql.Open("sqlrite", path)` against a file-backed read-write DB routes through a process-level path registry (Phase 11.11c) — multiple `sql.Open` calls for the same canonical path mint sibling handles off a shared primary, so each `*sql.DB`'s pool can issue its own `BEGIN CONCURRENT` against the same backing engine. `:memory:` opens stay isolated by design; read-only opens (via `sqlrite.OpenReadOnly`) take a shared lock and bypass the registry. See [`sdk/go/README.md`](../sdk/go/README.md#multi-handle-reads--writes-phase-1111c) for the runnable cross-pool example.

### The retry loop

The canonical shape is the same in every language:

```rust
loop {
    conn.execute("BEGIN CONCURRENT")?;
    conn.execute(/* writes */)?;
    match conn.execute("COMMIT") {
        Ok(_) => break,
        Err(e) if e.is_retryable() => {
            conn.execute("ROLLBACK").ok();
            continue;
        }
        Err(e) => return Err(e.into()),
    }
}
```

SQLRite intentionally **does not** ship an automatic-backoff retry helper — the right policy (immediate retry, exponential backoff, capped attempts, jittered, etc.) depends on the workload. The retryable-error classification is the only piece the SDK guarantees.

### `Connection::vacuum_mvcc` <a id="vacuum_mvcc"></a>

Per-commit GC sweeps the write-set's chains automatically. For a deterministic full drain (memory-pressure testing, debug snapshots, `Mvcc → Wal` downgrade prep), call [`conn.vacuum_mvcc()`](../src/connection.rs) — returns the count of versions reclaimed across the whole store. Both paths are safe against in-flight readers: a reader inside `BEGIN CONCURRENT` keeps every version its `begin_ts` snapshot still needs visible.

---

## REPL multi-handle demo (Phase 11.11a)

The `sqlrite` REPL ships with three meta-commands for interactive MVCC demos. The prompt always shows the active handle (`sqlrite[A]>`, `sqlrite[B]>`):

| Command | Effect |
|---|---|
| `.spawn` | Mint a sibling handle off the active one and switch to it |
| `.use NAME` | Switch the active handle (case-insensitive); errors with the list of valid names on miss |
| `.conns` | List every handle, mark the active one with `*`, tag handles in an open `BEGIN CONCURRENT` |

End-to-end demo:

```text
sqlrite[A]> PRAGMA journal_mode = mvcc;
sqlrite[A]> CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);
sqlrite[A]> INSERT INTO t (id, v) VALUES (1, 0);
sqlrite[A]> .spawn
Spawned sibling handle 'B' and switched to it. 2 handles open.
sqlrite[B]> .use A
sqlrite[A]> BEGIN CONCURRENT;
sqlrite[A]> UPDATE t SET v = 100 WHERE id = 1;
sqlrite[A]> .conns
2 handle(s):
  * A (BEGIN CONCURRENT)
    B
sqlrite[A]> .use B
sqlrite[B]> BEGIN CONCURRENT;
sqlrite[B]> UPDATE t SET v = 200 WHERE id = 1;
sqlrite[B]> COMMIT;
sqlrite[B]> .use A
sqlrite[A]> COMMIT;
An error occured: Busy: write-write conflict on t/1: another transaction
committed this row at ts=3 (after our begin_ts=1); transaction rolled
back, retry with a fresh BEGIN CONCURRENT
sqlrite[A]> .use B
sqlrite[B]> SELECT * FROM t;
+----+-----+
| id | v   |
+----+-----+
| 1  | 200 |
+----+-----+
```

---

## Error semantics

| Variant | When | Retryable |
|---|---|---|
| `SQLRiteError::Busy` | A `BEGIN CONCURRENT` `COMMIT` lost the validation race — some other transaction superseded one of our row writes after our `begin_ts` | yes |
| `SQLRiteError::BusySnapshot` | A snapshot the read path expected has been GC'd; surfaces from `Statement::query` when a long-lived reader's `begin_ts` predates the GC watermark | yes |
| Any other variant | Programming error or storage failure — not retryable | no |

`SQLRiteError::is_retryable()` is the single classifier — every SDK's retryable-error helper is a wrapper over the same predicate.

---

## Durability and recovery

### WAL log records (Phase 11.9)

Every successful `BEGIN CONCURRENT` commit writes **two** WAL records: the legacy per-page commit frames *and* a new typed `MvccCommitBatch` frame distinguished by the sentinel `page_num = u32::MAX`. The MVCC frame is appended buffered; the legacy save's commit-frame fsync covers both — so a crash between commits either keeps both writes or loses both.

The MVCC frame body encodes `commit_ts + record stream` (per-record: op tag, table name, rowid, optional column-value pairs). The encoder caps each batch at 4 KiB (the frame body size); multi-frame batches for very large transactions are a deferred follow-up.

### Reopen replay

`pager::open_database` walks every recovered MVCC frame and re-pushes the row versions into `MvStore` via `MvStore::push_committed`. The `MvccClock` is seeded past `max(WAL header's clock_high_water, max(commit_ts among replayed batches))` so post-restart transactions can never hand out a regressed `begin_ts`.

### What's parked

The checkpoint half of plan-doc §10.5 — folding MVCC log records back into pager-level updates so a WAL truncate doesn't lose them, and re-enabling the `Mvcc → Wal` journal-mode downgrade once the store is drainable — is the remaining slice. The legacy save mirror still covers durability of the visible row state on the read path, so the gap is foundation work, not a correctness regression.

### WAL format version

| Version | Adds |
|---|---|
| v1 | Pre-Phase-11 baseline. Reads cleanly today. |
| v2 (Phase 11.2) | `clock_high_water: u64` in the WAL header (bytes 24..32) |
| v3 (Phase 11.9) | MVCC log-record frames (`page_num = u32::MAX`) |

Decoders accept v1..=v3. A v2 reader on a v3 WAL emits a clean "unsupported WAL format version" diagnostic instead of silently dropping MVCC frames.

---

## Limitations

- **`CREATE INDEX` is rejected while `journal_mode = mvcc`.** Index maintenance under MVCC is Phase 11.10 (deferred-by-design — Turso explicitly punted on the same problem).
- **DDL inside `BEGIN CONCURRENT` is rejected.** Run DDL outside the concurrent transaction, then begin a fresh one.
- **Cross-process MVCC is out of scope.** The version index is in-memory only; multi-process writers still serialize through the pager's `flock(LOCK_EX)`. SQLRite has no shared-memory coordination file.
- **No automatic backoff in retry helpers.** Callers pick the policy.
- **FTS / HNSW indexes are not maintained inside `BEGIN CONCURRENT`.** The per-row commit-apply path covers B-tree secondary indexes only; tables under MVCC writers shouldn't have FTS or HNSW indexes attached if you need the search index to stay current.
- **`AUTOINCREMENT` is not specifically guarded** — two concurrent INSERTs that each allocate the same rowid surface as `Busy` at the second commit. The plan's "reject AUTOINCREMENT under MVCC" gate is a clean follow-up.
- **Memory growth is bounded only via GC.** Per-commit sweeps + `vacuum_mvcc()` cover most cases; for adversarial workloads where readers hold long-lived `begin_ts` snapshots, the chains can grow until the longest-lived reader closes.
- **Bottom-up B-tree rebuild on every save.** The architectural mismatch flagged in the plan-doc still applies. MVCC amortizes the rebuild to checkpoint time only once the checkpoint-drain follow-up lands; until then, every concurrent commit's mirror write to `Database::tables` triggers the legacy `save_database` rebuild path. Fine for v0 workloads; will matter at scale.

---

## See also

- [`docs/concurrent-writes-plan.md`](concurrent-writes-plan.md) — original design proposal + sequencing decisions. Historical; the current doc reflects shipped reality.
- [`docs/supported-sql.md`](supported-sql.md) — full SQL reference; the `PRAGMA journal_mode` and `BEGIN CONCURRENT` sections cross-link here.
- [`docs/embedding.md`](embedding.md) — embedding API + multi-handle examples.
- [`docs/file-format.md`](file-format.md) — WAL frame layout, MVCC log-record body, clock-high-water field.
- [`docs/design-decisions.md`](design-decisions.md) §12a–§12h — the design notes accumulated across Phase 11 sub-phases.
- [`docs/roadmap.md`](roadmap.md#phase-11--concurrent-writes-via-mvcc--begin-concurrent-sqlr-22-in-flight--see-concurrent-writes-planmd) — phase-by-phase shipped vs deferred status.
- [`examples/rust/concurrent_writers.rs`](../examples/rust/concurrent_writers.rs) — runnable retry-loop example.

External:

- [Turso concurrent writes](https://docs.turso.tech/tursodb/concurrent-writes) — the direct inspiration; we cite their issues throughout the plan-doc.
- [Hekaton (Larson et al., VLDB 2011)](https://www.microsoft.com/en-us/research/wp-content/uploads/2011/01/main-mem-cc-techreport.pdf) — the optimistic MVCC paper Turso (and now SQLRite) builds on.
- [Hermitage anomaly test suite](https://github.com/ept/hermitage) — snapshot-isolation conformance bar; SQLRite has not yet ported these (a clean follow-up).
