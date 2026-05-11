# Concurrent writes plan — MVCC + `BEGIN CONCURRENT`

> 📘 **Looking for the user-facing reference?** This is the original
> design proposal, kept as the historical record of the decisions
> that shaped Phase 11. For the shipped surface — SQL, embedding API,
> SDK error mapping, REPL meta-commands, durability story,
> limitations — read [**`concurrent-writes.md`**](concurrent-writes.md)
> first; come back here when you want the *why* and the
> sequencing discussion.

**Status:** shipped end-to-end through Phase 11.11a (May 2026); a small set of follow-ups remain explicitly parked — see the [roadmap](roadmap.md#phase-11--concurrent-writes-via-mvcc--begin-concurrent-sqlr-22-in-flight--see-concurrent-writes-planmd). Drafted 2026-05-07.
**Inspiration:** [Turso](https://turso.tech) — a SQLite-compatible engine, written in Rust, that implements multi-version concurrency control to lift SQLite's single-writer ceiling. See [`turso/core/mvcc/`](https://github.com/tursodatabase/turso/tree/main/core/mvcc) and the [Turso concurrent-writes docs](https://docs.turso.tech/tursodb/concurrent-writes).
**Tracks:** [SQLR-22](https://app.marvinapp.io/) (Marvin).

This document proposes adding **multi-version concurrency control (MVCC)** and a **`BEGIN CONCURRENT`** transaction mode to SQLRite, enabling multiple writers in the same process to make progress in parallel under snapshot isolation, with row-level write-write conflict detection at commit. It is intentionally a *plan* — there is no code yet.

The output of this work, when shipped, would put SQLRite in the same conceptual bucket as Turso's `--experimental-mvcc`: the file format stays compatible with the existing v5 layout, the legacy serialized path keeps working, and apps opt in by setting a journal mode and using a new `BEGIN CONCURRENT` statement.

---

## 1. Why bother

SQLite's single-writer rule is the design choice users hit first when they scale. It's not a bug — it's the consequence of a single global file lock (`PENDING`/`EXCLUSIVE`) that serializes commit ordering — but in workloads where two concurrent writers touch *disjoint* rows it costs throughput unnecessarily.

Turso's measurements report **up to 4× the write throughput of SQLite** when the workload is dominated by non-conflicting writes ([Turso v0.5.0 blog post](https://turso.tech/blog/turso-0.5.0); [Beyond the single-writer limitation](https://turso.tech/blog/beyond-the-single-writer-limitation-with-tursos-concurrent-writes)). For SQLRite — which today inherits SQLite's single-writer model end-to-end (advisory `flock(LOCK_EX)` plus a single `Pager` per process; see [pager.md](pager.md) and [Decision 11 in design-decisions.md](design-decisions.md)) — the same uplift is theoretically available, *if* the engine learns to track row versions and detect write-write conflicts at commit instead of holding an exclusive write lock for the duration.

This is also a natural fit for SQLRite's stated remit (a hands-on study of database internals). MVCC, snapshot isolation, optimistic concurrency control, and version-chain garbage collection are textbook database-systems topics; implementing them turns the engine into a credible reference for those topics rather than a SQLite clone with extra storage tricks.

---

## 2. What Turso does

Turso's MVCC implementation is the most directly relevant prior art — same target SQL, same general layout, same Rust language, and they've already navigated most of the design questions we'd face. A summary of the moving parts (cross-checked against [`core/mvcc/mod.rs`](https://github.com/tursodatabase/turso/blob/main/core/mvcc/mod.rs) and [`core/mvcc/database/mod.rs`](https://github.com/tursodatabase/turso/blob/main/core/mvcc/database/mod.rs)):

### 2.1 Activation

```sql
PRAGMA journal_mode = mvcc;     -- per-connection switch
BEGIN CONCURRENT;
-- writes
COMMIT;                          -- may return SQLITE_BUSY → caller retries
```

Compiled in behind `--experimental-mvcc`. `BEGIN CONCURRENT` doesn't acquire any locks; `BEGIN IMMEDIATE` / `BEGIN DEFERRED` retain SQLite's exclusive-write semantics.

### 2.2 In-memory version index

The MVCC store keeps an in-memory map keyed by `RowID { table_id, row_key }` whose value is a chain of `RowVersion` records. Each version carries:

- `begin: TxTimestampOrID` — the commit timestamp (or transaction ID, while uncommitted) at which this version becomes visible
- `end: Option<TxTimestampOrID>` — when it stops being visible (a later UPDATE/DELETE pushes a new head onto the chain)
- the row payload itself

Visibility for a reader transaction with begin-timestamp `T` is the textbook snapshot-isolation rule: pick the version whose `begin <= T < end`.

The Hekaton paper (Larson et al., *VLDB 2011*) is the explicit reference. The same paper also covers the validation phase used at commit and the garbage-collection rules sketched in §7 below.

### 2.3 Read path

- If the row is present in the MVCC index → use it (latest-wins; all writes funnel through the index, so the in-memory copy is the source of truth).
- Otherwise → fall through to the existing pager → WAL → main-file read path. Result is materialized into the index on first touch ("eager loaded" today, which is the source of Turso's memory-pressure caveats).

### 2.4 Write path

Writes go entirely to the MVCC index, tagged with the transaction ID. They're invisible to other transactions until the writer commits. The transaction also tracks a **read set** and a **write set** for validation.

### 2.5 Commit / conflict detection

Three phases:

1. Take a commit timestamp from a logical clock.
2. Walk the write set; for each row, check whether *any* version newer than the transaction's begin timestamp exists in the index. If yes → write-write conflict. The transaction is aborted and the caller sees `SQLITE_BUSY` (or, in libSQL terms, `Busy` / `BusySnapshot`).
3. Otherwise, stamp the new versions with the commit timestamp, append a WAL log record describing the row deltas, and fsync. The pager's WAL machinery is reused unchanged.

Conflict detection is **row-level**, not page-level. This is the critical contrast with the upstream SQLite `BEGIN CONCURRENT` patch, which only resolves at page granularity and forces aborts when two transactions touch unrelated rows that happen to share a page.

### 2.6 Checkpointing

The MVCC log is "eventually checkpointed into the SQLite database file via the WAL." Specifically: the writer that checkpoints walks the committed-but-unmaterialized portion of the version chain, materializes the latest visible version of each row into the page cache, and lets the regular pager flush it to the WAL/main file. Recovery on reopen relies on the WAL plus the persistent on-disk state — the in-memory MVCC log is rebuilt empty.

### 2.7 Garbage collection

Versions whose `end` timestamp is older than the oldest active reader's begin-timestamp are dead and may be reclaimed. Turso's implementation today uses a contiguous `Vec` per row guarded by an `RwLock`, with a scan-based GC pass that runs opportunistically. The known issue ([turso#3499](https://github.com/tursodatabase/turso/issues/3499)) is that the lock is the bottleneck under heavy contention; a wait-free chain is on their roadmap.

### 2.8 Limitations (as of writing)

- `CREATE INDEX` is not yet supported in MVCC mode — neither maintaining secondary indexes through the version chain nor reading them under snapshot isolation.
- `AUTOINCREMENT` is not supported (the global counter is not yet versioned).
- `wal_checkpoint(TRUNCATE)` is the only checkpoint variant; it blocks readers and writers.
- "Eager loading" — first access to a table loads it fully into the version index. Memory cost scales with the table, not the working set.
- Row deltas are full row copies, not column-level diffs ([turso#3498](https://github.com/tursodatabase/turso/issues/3498)).
- I/O is synchronous; `io_uring` adoption is on the roadmap ([turso#1848](https://github.com/tursodatabase/turso/issues/1848)).

These are honest limits, not gotchas. They suggest where SQLRite's plan should explicitly punt, and where it might do better by virtue of being smaller.

---

## 3. SQLRite's current concurrency model

A clear-eyed inventory of what we have today, since the gap is what the plan has to bridge.

### 3.1 Process-level locking ([`src/sql/pager/pager.rs`](../src/sql/pager/pager.rs))

`Pager::open` takes `flock(LOCK_EX | LOCK_NB)` on both the main file and the WAL sidecar via `fs2`. `Pager::open_read_only` takes `flock(LOCK_SH | LOCK_NB)`. Reads and writes are mutually exclusive *across* processes — POSIX flock semantics, no shared-memory coordination file. **Within** a process, the engine runs one writer because the public surface only exposes one `Connection` per `Database`/`Pager`.

### 3.2 Transactions ([`src/sql/db/database.rs:149-195`](../src/sql/db/database.rs))

`BEGIN` deep-clones the entire `tables: HashMap<String, Table>` into a `TxnSnapshot` stashed on `Database::txn`. `ROLLBACK` swaps it back. `COMMIT` clears the snapshot and the next `save_database` call appends one WAL commit frame. Auto-save is suppressed while a transaction is open. Nested begins are rejected. There is no concept of read-set / write-set tracking, no logical clock, no row-version metadata.

### 3.3 Storage ([`src/sql/pager/`](../src/sql/pager/) + [Decision 6 in design-decisions.md](design-decisions.md))

- 4 KiB pages, cell-encoded B-trees per table and per secondary index.
- **`save_database` rebuilds every B-tree bottom-up on every commit** from sorted in-memory rows. This is correct-by-construction but assumes a single committer — concurrent committers would each want to rebuild the same tree from a different in-memory state.
- The pager owns three page maps (`on_disk` / `wal_cache` / `staged`), all `HashMap<u32, Box<[u8; PAGE_SIZE]>>`. There is no per-page locking or versioning; reads inside the engine are sequential.

### 3.4 Connection model ([`src/connection.rs`](../src/connection.rs))

`Connection` owns its `Database` by value. It is `Send`, **not `Sync`**, and has no `clone()`. The engine's documented embedding pattern when callers want multi-threaded access is to wrap the connection in `Mutex<Connection>` — i.e. serialize access externally.

### 3.5 What this means

To get from "one writer, one process" to "many writers, one process, conflict-detected at commit" we need to change every layer:

1. **Connection layer**: turn `Connection` into a thin handle on a shared `Database`/`Pager` so multiple connections can coexist within a process. Today there's only ever one `Connection` per `Database`.
2. **Transaction layer**: replace deep-clone snapshots with per-transaction read/write sets and a logical clock. Snapshots scale O(database size) on every BEGIN, which is unworkable with many concurrent transactions.
3. **Storage layer**: introduce an MVCC version index sitting in front of the existing pager. Writes land in the index and only become pager-visible at checkpoint time; reads consult the index first, the pager second.
4. **Commit path**: validate write sets against committed-after-begin versions. On conflict, return a typed `Busy` error and let the caller retry. On success, append a WAL transaction record covering the row deltas.
5. **Checkpoint**: drain the version index into the existing bottom-up B-tree rebuild path. The legacy commit path doesn't go away — `BEGIN` and `BEGIN IMMEDIATE` keep their current single-writer semantics — but `BEGIN CONCURRENT` short-circuits past it until checkpoint time.

The good news: the existing WAL and the existing on-disk format are reusable as-is. Turso's checkpoint design ("MVCC log → page cache → WAL → main file") works because the WAL is the same WAL with or without MVCC. The same is true for SQLRite's WAL.

---

## 4. Proposed design

A high-level sketch of the layered architecture this proposal targets.

```
┌──────────────────────────────────────────────────────────┐
│  Connection (handle) ── Connection (handle) ── ...       │   <- many per process
├──────────────────────────────────────────────────────────┤
│              Shared Database (Arc<DatabaseInner>)         │
│                                                          │
│    Schema cache    │    MVCC store    │   Logical clock  │
├────────────────────┴────────────────┬─┴──────────────────┤
│                                     │                    │
│   Pager (existing) ─ WAL ─ main file│   GC worker        │
└─────────────────────────────────────┴────────────────────┘
```

### 4.1 Logical clock

A monotonic `u64` counter, per-`Database`. Hands out `begin_ts` at `BEGIN CONCURRENT` and `commit_ts` at the start of validation. Wrapped in `AtomicU64`; no contention because each transaction calls it twice. Persisted to the WAL header on each commit so reopens resume past the highest committed timestamp. (Turso's [`MvccClock`](https://github.com/tursodatabase/turso/blob/main/core/mvcc/clock.rs) is the same shape.)

### 4.2 Version index

```rust
pub struct MvStore {
    // (table_id, rowid) -> head of version chain
    versions: DashMap<RowID, RowVersionChain>,
    clock: MvccClock,
    active: ActiveTxRegistry,   // for GC: min active begin_ts
}

pub struct RowVersion {
    begin: TxTsOrId,            // commit_ts once committed; tx_id while in-flight
    end:   Option<TxTsOrId>,    // None == latest visible version
    payload: Row,               // for v0; column deltas later (turso#3498-style)
    next: Option<Arc<RowVersion>>,
}
```

A few intentional simplifications versus Turso for v0:

- One chain per row, behind `RwLock` (or `parking_lot::RwLock`). The wait-free chain is a known follow-up; it's not on the v0 critical path.
- Full-row payloads (defer column-level deltas).
- Lazy materialization from the pager on first touch. Deviates from Turso's "load whole table eagerly", which they themselves treat as debt; we'd be starting from the better default.

### 4.3 Transaction record

```rust
pub struct ConcurrentTx {
    id: TxId,
    begin_ts: u64,
    state: TxState,                          // Active | Committed | Aborted
    read_set: HashSet<RowID>,                // for serializable-isolation upgrade later
    write_set: HashMap<RowID, Arc<RowVersion>>,
    schema_snapshot: Arc<SchemaSnapshot>,    // pinned at BEGIN
}
```

Read-set tracking is optional for snapshot isolation but cheap to keep — and unlocks serializable later if we want it.

### 4.4 Read protocol

```
read(row_id, begin_ts):
    if row_id in MvStore.versions:
        walk chain, return version with begin <= begin_ts < end
    else:
        bytes = pager.read_via_existing_path(row_id)
        materialize into MvStore.versions as a single committed-at-zero version
        return it
```

### 4.5 Commit protocol

```
commit(tx):
    commit_ts = clock.tick()
    for row_id in tx.write_set:
        chain = MvStore.versions[row_id]
        head = chain.read()
        if head.begin > tx.begin_ts:        # someone committed under us
            return Err(Busy)
    for row_id, new_version in tx.write_set:
        new_version.begin = commit_ts
        MvStore.versions[row_id].push(new_version)
    pager.append_wal_log_record(tx.write_set, commit_ts)   # one fsync
    return Ok
```

The validation pass walks the write set only — typically tiny. The fsync is one barrier per transaction, same cost as today's `commit_transaction`.

### 4.6 Checkpoint

Existing checkpoint logic ([Phase 4d](roadmap.md)) folds WAL frames into the main file when frame count crosses `AUTO_CHECKPOINT_THRESHOLD_FRAMES`. The MVCC variant adds a step before that: walk the version index, pick the latest committed version per row visible at `min(active begin_ts) - 1`, hand it to `save_database` (which rebuilds the B-tree). Older versions get GC'd at the same time. Schema changes still require an exclusive lock.

### 4.7 Garbage collection

A separate worker (or piggy-backed on commit) walks `MvStore.versions` and prunes versions whose `end` timestamp is below the oldest active reader's `begin_ts`. v0 can run this synchronously inside `commit` — sweep the rows touched in the write set only — and add an explicit background sweep later.

### 4.8 Compatibility with existing transactions

`BEGIN` and `BEGIN IMMEDIATE` keep their current semantics — full deep-clone snapshot, exclusive write, no row-version tracking. They remain the right choice for DDL. A `BEGIN IMMEDIATE` running on the same connection blocks new `BEGIN CONCURRENT` commits (they get `Busy`) and waits for in-flight ones to finish. This matches Turso's "exclusive vs concurrent" coexistence rule.

---

## 5. Phased plan

Sequenced so each sub-phase is independently shippable, follows SQLRite's existing roadmap discipline (every phase ends with a working build + full test suite + a commit on `main`), and the engine never regresses while in flight. Tentatively numbered as **Phase 10** (Phase 9 = benchmarks; Phase 8 = FTS, shipped). Slot may shift depending on roadmap rebalancing.

### Phase 10.1 — Multi-connection foundation

Goal: more than one `Connection` can target the same `Database` within a process, with no behavior change otherwise.

- Split `Database` into `DatabaseInner` (state) + `Connection` (handle that holds `Arc<DatabaseInner>`).
- Move the `Pager` behind a `Mutex` or `RwLock` inside `DatabaseInner`.
- New `Database::connect() -> Connection`. The existing `Connection::open` path opens the database and returns a freshly-connected handle.
- Make the engine `Send + Sync` end-to-end. No new SQL surface yet.
- Tests: spawn N threads, each running independent `BEGIN; INSERT; COMMIT` against the same DB, prove the existing single-writer locking still serializes them correctly with no panics.

### Phase 10.2 — Logical clock + transaction registry

- Add `MvccClock` with `tick() -> u64` + `now() -> u64`.
- Add `ActiveTxRegistry` exposing `min_active_begin_ts()` for the GC.
- Persist the clock high-water-mark in the WAL header (extend `WalHeader` with a 16-byte field; bump `WAL_FORMAT_VERSION` from 1 → 2 — backward-compatible because the WAL is regenerated on checkpoint).
- Tests: round-trip the clock through reopen, reject WAL files where the persisted timestamp is non-monotonic.

### Phase 10.3 — `MvStore` skeleton + reads

- Implement `MvStore` and `RowVersion`. v0: one chain per row behind `RwLock<Vec<RowVersion>>`.
- Wire reads through the store: lazy-load on first touch, return the visible version for the caller's `begin_ts`.
- New SQL gate: `PRAGMA journal_mode = mvcc` switches the connection into MVCC mode for reads. Writes still go through the legacy path.
- Tests: snapshot isolation reads — one connection inserts and commits, a second connection that began before the commit doesn't see the new row; a third connection that began after does.

### Phase 10.4 — `BEGIN CONCURRENT` writes

- Extend the parser to map `BEGIN CONCURRENT` (sqlparser already recognizes the modifier) to a new `TxKind::Concurrent`.
- Route writes from `TxKind::Concurrent` transactions into the `MvStore`'s write set rather than the live `tables` map.
- Implement the commit-time validation loop. New `SQLRiteError::Busy` and `SQLRiteError::BusySnapshot` variants ([decision: use both, mirroring Turso, so SDKs can map cleanly](https://github.com/tursodatabase/turso/blob/main/core/error.rs)).
- WAL log record format: a new frame kind carrying `(table_id, rowid, op, payload)` tuples. Distinct from the existing per-page commit frame; the checkpointer flattens log records into page-level updates.
- Tests:
  - two concurrent inserts on disjoint rowids both commit
  - two concurrent updates on the same rowid: one commits, one aborts with `Busy`
  - aborted transaction's writes never become visible
  - retry-after-`Busy` succeeds

### Phase 10.5 — Checkpoint + crash recovery

> **Status (roadmap 11.9 — May 2026):** The crash-recovery half landed in roadmap Phase 11.9. WAL format is bumped to v3; commits append a typed `MvccCommitBatch` frame before the legacy save's fsync; reopen replays those frames into `MvStore` and seeds `MvccClock` past the highest `commit_ts`. The checkpoint-drain half — folding MVCC log records into pager-level updates and re-enabling the `Mvcc → Wal` journal-mode downgrade — is the remaining slice and stays parked for a follow-up.

- ~~Extend the checkpointer to drain MVCC log records into pager-level updates before folding the WAL into the main file.~~ *Deferred — see status note above.*
- Crash recovery: on open, replay WAL log records into `MvStore`, then replay pager-level commit frames as today. **(Shipped — 11.9.)**
- Tests: kill the process mid-MVCC-commit (between log-record append and version-chain push), reopen, verify the committed transaction is visible and the half-written one is not. **(Shipped — 11.9 covers the clean-drop case which exercises the same recovery codepath; a real OS-kill test is parked with the checkpoint-drain follow-up.)**

### Phase 10.6 — Garbage collection

- Per-commit sweep over the write set's chains.
- Background sweep behind a new `PRAGMA mvcc_gc_interval_ms` (default 1000).
- Memory-pressure trigger: if `MvStore` size > `mvcc_max_memory_bytes`, force a sweep at the next safe point.
- Tests: chain length stays bounded under heavy single-row update pressure.

### Phase 10.7 — Indexes under MVCC (deferred-by-design — separate later phase)

Index maintenance under MVCC is hard enough that Turso explicitly punted on it. SQLRite should ship the v0 with `CREATE INDEX` rejected when `journal_mode = mvcc`, and tackle indexes as a follow-up phase once the v0 is stable. Sketched solution: each secondary-index entry becomes a `RowVersion` itself, keyed by `(index_id, key, rowid)`. The cost is one version chain per indexed (column, row) pair.

### Phase 10.8 — Public API + SDK propagation

- Surface `Connection::begin_concurrent() -> Result<()>` as a typed convenience wrapper.
- Surface `SQLRiteError::Busy` and `SQLRiteError::BusySnapshot` through the FFI shim ([`sqlrite-ffi/`](../sqlrite-ffi/)) and each SDK ([Python](../sdk/python/), [Node](../sdk/nodejs/), [Go](../sdk/go/), [WASM](../sdk/wasm/)). The right level of abstraction: an exception per SDK plus a typed retry helper.
- New REPL meta-command `.spawn` that opens an additional connection to the same DB so users can demo `BEGIN CONCURRENT` interactively.
- New benchmark workload in [`benchmarks/`](../benchmarks/) that pits SQLRite-MVCC against SQLite + SQLRite-default + Turso on a "N writers, mostly disjoint rows" scenario. Slots into the existing SQLR-16 harness as a Group D differentiator workload.

### Phase 10.9 — Docs

> **Status (roadmap 11.12 — May 2026):** Shipped. The canonical user-facing reference at [`docs/concurrent-writes.md`](concurrent-writes.md) covers the SQL surface, embedding API, SDK error mapping, REPL meta-commands, durability story, and limitations as of Phase 11.11a. This plan-doc is now the historical record. Cross-references in `_index.md`, `supported-sql.md`, `embedding.md`, and `design-decisions.md` point at the canonical doc; a runnable example lives at [`examples/rust/concurrent_writers.rs`](../examples/rust/concurrent_writers.rs).

- Promote this plan to `docs/concurrent-writes.md` (the canonical user-facing reference), keeping `concurrent-writes-plan.md` as the historical design document. **(Shipped — 11.12.)**
- Update [roadmap.md](roadmap.md), [`docs/_index.md`](_index.md), [supported-sql.md](supported-sql.md), [embedding.md](embedding.md), [design-decisions.md](design-decisions.md). **(Shipped — 11.12.)**
- Add a worked example under `examples/rust/concurrent_writers.rs`. **(Shipped — 11.12.)**

---

## 6. SQL & API surface

```sql
-- Opt in (per connection, per session)
PRAGMA journal_mode = mvcc;

-- Snapshot-isolation transaction with row-level conflict detection at commit
BEGIN CONCURRENT;
UPDATE accounts SET balance = balance - 50 WHERE id = 1;
UPDATE accounts SET balance = balance + 50 WHERE id = 2;
COMMIT;   -- may return SQLRITE_BUSY → caller retries

-- Existing transactions still work (single-writer, exclusive)
BEGIN;             -- aka BEGIN DEFERRED, identical to today
BEGIN IMMEDIATE;   -- new alias of BEGIN; documented for Turso parity
```

```rust
// Rust embedding API
let conn = db.connect()?;          // new — multi-connection
conn.execute("PRAGMA journal_mode = mvcc")?;

loop {
    conn.execute("BEGIN CONCURRENT")?;
    conn.execute("UPDATE accounts SET balance = balance - 50 WHERE id = 1")?;
    conn.execute("UPDATE accounts SET balance = balance + 50 WHERE id = 2")?;
    match conn.execute("COMMIT") {
        Ok(_) => break,
        Err(SQLRiteError::Busy | SQLRiteError::BusySnapshot) => {
            conn.execute("ROLLBACK").ok();
            continue;
        }
        Err(e) => return Err(e),
    }
}
```

A typed `Connection::with_concurrent_tx<F>(f: F)` helper that hides the retry loop is worth considering once the bare API is shaken out.

---

## 7. File-format implications

- The on-disk `.sqlrite` file format (currently v5) **does not change**. MVCC state lives in memory and in the WAL.
- The WAL header gains a `clock_high_water: u64` field. Bump `WAL_FORMAT_VERSION` from 1 → 2. Pre-v2 WALs open as v1 (clock starts from 0); the next checkpoint rewrites the header at v2.
- A new WAL frame kind for MVCC log records. Existing per-page commit frames keep their format. Both kinds can coexist in the same WAL — the checkpointer drains log records first, then per-page commits, then truncates.

This keeps `sqlrite-engine` v0.x readers compatible with v1.x files and vice versa as long as MVCC isn't enabled at write time.

---

## 8. Risks, open questions, and explicit non-goals

### Risks

- **Bottom-up B-tree rebuild ([Decision 6](design-decisions.md))** is the biggest architectural mismatch. Today every commit rebuilds the whole tree. With MVCC the rebuild only runs at checkpoint, so the cost is amortized — but tables that grow large between checkpoints can produce huge checkpoint stalls. Mitigation: in-place B-tree updates are an obvious follow-up, separately tracked.
- **Memory growth** — the version index holds full row payloads. Mitigation: column-level deltas (Turso's [#3498](https://github.com/tursodatabase/turso/issues/3498)) and an `mvcc_max_memory_bytes` cap.
- **Lock contention on the per-row chain** — `RwLock<Vec<RowVersion>>` is good enough for v0 but won't scale to extreme concurrency. Mitigation: wait-free chain (Turso's [#3499](https://github.com/tursodatabase/turso/issues/3499)) as a v1 follow-up.
- **DDL under MVCC** — schema changes need an exclusive promotion of the lock. v0 should reject DDL inside a `BEGIN CONCURRENT` block with a clean error.
- **Multi-process MVCC is out of scope.** Turso is intra-process; the in-memory version index has no cross-process visibility, and adding it would require a shared-memory coordination file (the same machinery SQLite's WAL uses for read marks). Multi-process writes continue to be serialized by the existing `flock(LOCK_EX)`. Documented as a non-goal until someone has a workload that justifies the engineering.

### Open questions

- **Should `journal_mode = mvcc` be per-connection or per-database?** Turso treats it per-connection; SQLite's `journal_mode` is per-database. Per-connection is more flexible (a maintenance connection can stay in WAL mode while app connections use MVCC), but per-database is closer to user expectation.
- **Should `MvStore` materialize the whole table on first touch, or one row at a time?** Per-row is cheaper and avoids Turso's eager-load complaint. The cost is one `pager.read` per cold row.
- **Does `BEGIN CONCURRENT` upgrade to exclusive on the first write to a versioned table that has no `MvStore` entry?** The simpler answer is no — first read materializes the row into the index, no lock upgrade needed.
- **AUTOINCREMENT.** Globally-incrementing rowid counters are not naturally versioned. Acceptable v0 answer: reject INSERT into an AUTOINCREMENT table from a `BEGIN CONCURRENT` block (matches Turso). Better answer: a serializable counter sequence service. Defer.
- **Hermitage-style anomaly tests.** Turso's [`hermitage_tests.rs`](https://github.com/tursodatabase/turso/blob/main/core/mvcc/database/hermitage_tests.rs) is a port of [Martin Kleppmann's hermitage suite](https://github.com/ept/hermitage). SQLRite should adopt the same tests as the snapshot-isolation conformance bar.

### Non-goals (v0)

- Concurrent reads of an in-progress checkpoint (Turso's `wal_checkpoint(TRUNCATE)` blocks; ours can too).
- Automatic backoff in the SDK retry helpers — the caller picks the policy.
- Cross-process MVCC.
- Asynchronous I/O / `io_uring`.

---

## 9. Estimated effort

Order-of-magnitude only — each sub-phase is a roadmap entry and gets a real estimate when scheduled.

| Sub-phase | Rough size | Notes |
|---|---|---|
| 10.1 multi-connection | M | mostly mechanical, tests heavy |
| 10.2 clock + tx registry | S | small, isolated |
| 10.3 `MvStore` reads | M | new module, lazy materialization |
| 10.4 `BEGIN CONCURRENT` writes | L | the meat |
| 10.5 checkpoint + recovery | M | reuses existing checkpointer |
| 10.6 GC | S–M | one synchronous + one background |
| 10.7 indexes under MVCC | L | deferred |
| 10.8 SDK propagation | M | one slice per SDK |
| 10.9 docs | S | one pass once 10.4 lands |

Total before 10.7: ≈ 4–6 weeks of focused work for a single engineer, given the existing test discipline. Index maintenance under MVCC (10.7) is its own multi-week effort and may slip to a separate phase.

---

## 10. References

- [Turso concurrent writes — official docs](https://docs.turso.tech/tursodb/concurrent-writes)
- [Turso source — `core/mvcc/`](https://github.com/tursodatabase/turso/tree/main/core/mvcc)
- [Turso blog — Beyond the single-writer limitation](https://turso.tech/blog/beyond-the-single-writer-limitation-with-tursos-concurrent-writes)
- [Turso blog — v0.5.0 release](https://turso.tech/blog/turso-0.5.0)
- [Turso DeepWiki — MVCC architecture overview](https://deepwiki.com/tursodatabase/turso)
- Larson, Blanas, Diaconu, Freedman, Patel, Zwilling — [*High-Performance Concurrency Control Mechanisms for Main-Memory Databases*, VLDB 2011](https://www.microsoft.com/en-us/research/wp-content/uploads/2011/01/main-mem-cc-techreport.pdf) — the Hekaton MVCC paper Turso cites.
- Levandoski, Lomet, Sengupta — [*The Bw-Tree: A B-tree for New Hardware Platforms*, ICDE 2013](https://www.microsoft.com/en-us/research/publication/the-bw-tree-a-b-tree-for-new-hardware-platforms/) — referenced by Turso for the lock-free index path.
- [SQLite `BEGIN CONCURRENT` (experimental, page-level)](https://sqlite.org/cgi/src/doc/begin-concurrent/doc/begin_concurrent.md)
- Martin Kleppmann — [Hermitage anomaly test suite](https://github.com/ept/hermitage)
- SQLRite internals: [pager.md](pager.md), [file-format.md](file-format.md), [design-decisions.md](design-decisions.md), [roadmap.md](roadmap.md)
