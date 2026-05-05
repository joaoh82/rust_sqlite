# The Pager

The `Pager` is the component that actually talks to disk. Everything above it — the executor, the meta-commands, the Database itself — is blissfully unaware of file offsets. It hands bytes to the pager; the pager decides what gets written when.

The canonical code is in [`src/sql/pager/pager.rs`](../src/sql/pager/pager.rs).

## What it does

- Owns the open handles for the `.sqlrite` main file **and** its `-wal` sidecar (Phase 4c).
- Maintains three maps of page bytes:
  - **`on_disk`**   — byte snapshot of the main file as last checkpointed. Frozen between checkpoints.
  - **`wal_cache`** — latest committed body for every page whose new contents live in the WAL instead of the main file.
  - **`staged`**    — pages queued for the next commit.
- On `commit(new_header)`:
  1. Compare each staged page against the effective committed state (`wal_cache` layered on `on_disk`).
  2. Append a WAL frame for every page whose bytes actually differ, in ascending page order.
  3. Append a final **commit frame** whose body is the new page 0 (encoded header) and whose `commit_page_count` field carries the post-commit page count. The commit frame is fsync'd — that's the barrier that seals the transaction.
  4. Promote the just-written frames into `wal_cache`; clear `staged`; update `current_header`.

The main file isn't touched. It only changes when the **checkpointer** (Phase 4d) flushes accumulated WAL frames back and truncates the log.

The effect: the *first* commit appends one frame per page; subsequent commits append frames only for what changed.

## Data structures

```rust
pub struct Pager {
    storage: FileStorage,                          // main-file handle (read at open; checkpointer writes it)
    current_header: DbHeader,                      // last committed header, reflecting WAL replay
    on_disk: HashMap<u32, Box<[u8; PAGE_SIZE]>>,   // main-file pages, frozen between checkpoints
    staged: HashMap<u32, Box<[u8; PAGE_SIZE]>>,    // queued writes for the next commit
    wal_cache: HashMap<u32, Box<[u8; PAGE_SIZE]>>, // latest committed WAL bodies, layered on on_disk
    wal: Wal,                                      // open WAL sidecar (see wal.rs)
}
```

`Box<[u8; PAGE_SIZE]>` heap-allocates each 4 KiB page so the hash map isn't dragging 4-KiB value inlines through rehashes.

## Lifecycle

### Opening an existing file

`Pager::open(path)` (read-write, shorthand for `open_with_mode(path, AccessMode::ReadWrite)`):

1. Open the main file read-write, acquire the **exclusive** advisory lock (`flock(LOCK_EX)`).
2. Read and verify page 0 (header) → initial `current_header`.
3. For page numbers `1..page_count`, seek + read the page, store in `on_disk`.
4. Open (or create, if missing) the `-wal` sidecar; acquire its exclusive lock.
5. Replay committed WAL frames into `wal_cache`.
6. If the WAL contains a page-0 frame, decode it and **override** `current_header` — the WAL's copy is always more recent than the main file's.
7. `staged` starts empty.

After open, the pager carries a complete snapshot of what's on disk plus every WAL-resident update since the last checkpoint. A pre-Phase-4c `.sqlrite` file (no sidecar) gets a fresh empty WAL on first open.

### Opening read-only (Phase 4e)

`Pager::open_read_only(path)` (shorthand for `open_with_mode(path, AccessMode::ReadOnly)`) takes a **shared** advisory lock (`flock(LOCK_SH)`) on the main file and on the WAL sidecar (if it exists). Multiple read-only openers coexist; any writer is excluded (POSIX flock). Differences from read-write open:

- Main file opened read-only (no `write(true)` on `OpenOptions`).
- WAL sidecar: **not created** if missing — a read-only caller must not materialize one. With an absent sidecar the Pager serves reads straight from `on_disk` with an empty `wal_cache`.
- `stage_page` / `commit` / `checkpoint` all short-circuit with `General error: cannot commit: database is opened read-only` via the `require_writable` guard. The `wal` field is `Option<Wal>` and becomes `None` only on the "no sidecar" read-only path.

Library-level callers use `sqlrite::open_database_read_only(path, name)`; the REPL exposes it as `--readonly` / `-r`.

### Creating a new file

`Pager::create(path)`:

1. Truncate-or-create the main file; acquire its lock.
2. Encode an empty `sqlrite_master` as page 1 (a `TableLeaf` with zero cells).
3. Write the header with `page_count = 2`, `schema_root_page = 1`; `fsync`.
4. Create a matching empty WAL sidecar at `path + "-wal"` (any stale WAL from a prior crashed session gets truncated).
5. Populate `on_disk` with page 1 so no-op commits don't append frames for it.

The fresh DB is durable immediately — a valid header + empty schema on disk, a valid empty WAL alongside it.

### Staging writes

The higher-level `save_database` ([`src/sql/pager/mod.rs`](../src/sql/pager/mod.rs)) does the actual re-serialization:

```rust
pub fn save_database(db: &mut Database, path: &Path) -> Result<()> {
    let same_path = db.source_path.as_deref() == Some(path);
    let mut pager = /* re-attach or open or create */;
    pager.clear_staged();

    let mut next_free_page = 1;
    let mut catalog_entries = Vec::new();

    let mut table_names: Vec<&String> = db.tables.keys().collect();
    table_names.sort();                         // deterministic ordering
    for name in table_names {
        let bytes = bincode::encode(&db.tables[name])?;
        let start = next_free_page;
        next_free_page = stage_chain(&mut pager, &bytes, PageType::TableData, start)?;
        catalog_entries.push((name.clone(), start));
    }
    let catalog_bytes = bincode::encode(&catalog_entries)?;
    let schema_root = next_free_page;
    next_free_page = stage_chain(&mut pager, &catalog_bytes, PageType::SchemaRoot, schema_root)?;

    pager.commit(DbHeader { page_count: next_free_page, schema_root_page: schema_root })?;
    if same_path { db.pager = Some(pager); }
    Ok(())
}
```

Every page is staged via `pager.stage_page(n, bytes)`, which is just `staged.insert(n, bytes)`.

### Committing

`commit(new_header)` (WAL-backed, Phase 4c):

```rust
// 1. Diff staged against the effective committed state.
let dirty: Vec<_> = staged.into_iter()
    .filter(|(n, bytes)| {
        let existing = wal_cache.get(n).or_else(|| on_disk.get(n));
        match existing {
            Some(e) => e != bytes,
            None => true,
        }
    })
    .collect();
dirty.sort_by_key(|(n, _)| *n);

// 2. Append one dirty frame per changed page.
for (n, bytes) in &dirty {
    wal.append_frame(*n, bytes, None)?;
}

// 3. Seal the transaction with a commit frame for page 0.
//    Body = encoded new header, commit_page_count = new page count.
//    append_frame fsyncs when commit_page_count.is_some().
let page0 = encode_header(&new_header);
wal.append_frame(0, &page0, Some(new_header.page_count))?;

// 4. Promote everything we just wrote into wal_cache.
for (n, bytes) in dirty { wal_cache.insert(n, bytes); }
wal_cache.insert(0, page0);
self.current_header = new_header;
```

The sort keeps the WAL append order deterministic and matches ascending-page iteration order for the future checkpointer.

`commit` returns the number of dirty *data* frames (the commit frame is implicit and not counted). Unit tests assert on that count to confirm the diff still works — a repeated identical commit writes zero data frames, only the commit frame.

### Reads

`read_page(page_num)`:

1. `staged` — pending writes always shadow everything.
2. Bounds check: `page_num < current_header.page_count`. Pages logically dropped by a shrink-commit stay in `on_disk` until checkpoint, but readers don't see them.
3. `wal_cache` — committed WAL updates since the last checkpoint.
4. `on_disk` — the frozen main-file snapshot.

### Checkpointing (Phase 4d)

`checkpoint()` folds accumulated WAL pages back into the main file and truncates the WAL. It fires automatically from `commit` once `Wal::frame_count() >= AUTO_CHECKPOINT_THRESHOLD_FRAMES` (currently 100) and can also be triggered explicitly. The sequence uses **two fsync barriers**, matching SQLite's ordering so no reordered writeback can expose a half-migrated file:

1. Write every `wal_cache` data page at its `page_num * PAGE_SIZE` offset in the main file.
2. **`fsync`** — data pages must hit stable storage *before* the header publishes them.
3. Overwrite the main-file header with `current_header`.
4. `set_len(page_count * PAGE_SIZE)` — shrink the main file if we dropped pages.
5. **`fsync`** — header + truncate durable together. This is the checkpoint's commit point.
6. `Wal::truncate()` — resets the sidecar to header-only, rolls the salt, bumps the checkpoint sequence.
7. Drain `wal_cache` entries (minus page 0) into `on_disk`; drop on_disk entries past the new page count.

**Crash safety.**
- Crash between 1 and 2: data-page writes are buffered, header untouched — the main file still reads as its pre-checkpoint self. WAL is intact; retry rewrites the same bytes.
- Crash between 2 and 5: data pages durable, header old. WAL still holds the authoritative bytes including page 0; next open overrides the stale header from wal_cache[0] and reads resolve correctly.
- Crash between 5 and 6: main file fully migrated, WAL lingers. Next open sees wal_cache entries byte-identical to the main file; the next checkpoint cleans the stale WAL.

`checkpoint()` returns the number of data pages written to the main file (excluding the header). Back-to-back checkpoints return 0 the second time. **`wal.truncate()` runs before the in-memory cache swap** so that a `wal.truncate` I/O failure leaves the Pager in a well-defined state (main file consistent on disk, stale WAL present, wal_cache still populated) rather than an uninspectable intermediate.

## How the diff earns its keep

Consider an `UPDATE` that touches exactly one row in one table:

1. `executor::execute_update` updates the in-memory `Table`.
2. `process_command` calls `pager::save_database`.
3. Every table is re-serialized to bytes and staged into the pager.
4. For the table whose row changed, the bincode bytes differ → those pages are dirty.
5. For every *other* table, the bincode bytes are byte-identical to what's already on disk → those pages stay clean → no writes.

Without the diff, step 3's "re-serialize every table" would trigger a full file rewrite on every statement. With the diff, disk I/O scales with the number of changed tables, not total tables.

This only works because `save_database` iterates tables in sorted order — if the order were random, a table that didn't change might land at a different page number, appearing dirty. See [Design decisions §7](design-decisions.md#7-deterministic-page-number-ordering-when-saving).

## Free-page list and VACUUM (SQLR-6)

Save now uses a [`PageAllocator`](../src/sql/pager/allocator.rs) instead of a bare `next_free_page` counter. The allocator pulls pages from three sources, in preference order:

1. **Per-table preferred pool** — every table/index/master is given the page numbers it occupied last save (collected by walking from its old `rootpage`). An unchanged table re-stages byte-identical pages at the same numbers, so the diff pager skips every write for it.
2. **Global freelist** — pages from dropped tables/indexes that are recorded in the persisted freelist (rooted at `header.freelist_head`).
3. **Extend** — `next_extend++`, monotonic past the high-water mark.

After staging, pages that were live before this save but didn't get restaged this round (e.g., the leaves of a dropped table) move onto the new freelist. The freelist itself is encoded into a chain of `FreelistTrunk` pages — each trunk holds up to 1021 free leaf-page numbers plus a `next_page` pointer to the following trunk. Trunks consume some of the free pages they describe (a trunk page IS a free page borrowed for metadata), so a freelist of N pages takes `ceil(N / 1022)` trunks and persists `N − T` leaf entries.

`VACUUM;` (a SQL statement) calls [`vacuum_database`](../src/sql/pager/mod.rs), which is `save_database` with empty per-table preferred pools and an empty initial freelist. Allocation falls through to extend on every page → contiguous layout from page 1, no freelist trunks, file truncates to the new high-water mark on the next checkpoint.

Format-version side effect: a save that produces a non-empty freelist promotes the file from v4/v5 to v6 (mirrors Phase 8c's v4→v5 FTS rule). VACUUM clears the freelist but doesn't downgrade — v6 is a strict superset.

### Auto-VACUUM trigger (SQLR-10)

After SQLR-6, the file still required a manual `VACUUM;` to actually shrink — the freelist absorbed orphan pages but the high-water mark stayed put. SQLR-10 adds a heuristic that fires `vacuum_database` automatically after a page-releasing DDL (`DROP TABLE`, `DROP INDEX`, `ALTER TABLE DROP COLUMN`) when the freelist exceeds a configurable fraction of `page_count`.

Configuration lives on `Database::auto_vacuum_threshold: Option<f32>` and is exposed at the connection level via `Connection::set_auto_vacuum_threshold` / `auto_vacuum_threshold`. Defaults: `Some(0.25)` (SQLite parity at 25%); pass `None` to opt out per connection. The threshold is per-`Connection` runtime state and is not persisted in the file header — every reopen starts at the default. A SQL-level `PRAGMA auto_vacuum` is tracked separately (out of scope for SQLR-10).

The trigger lives at the end of [`process_command_with_render`](../src/sql/mod.rs), immediately after the auto-save. Order matters: the freelist isn't accurate until the bottom-up rebuild runs during save, so we save first, then check the ratio. The check itself is `freelist::should_auto_vacuum(pager, threshold)`, which:

- skips databases under `MIN_PAGES_FOR_AUTO_VACUUM` (16 pages = 64 KiB) so tiny files don't churn,
- counts both leaf and trunk pages in the freelist (trunks are reclaimable bytes too),
- returns `true` iff `(leaves + trunks) / page_count > threshold`.

Auto-VACUUM is also skipped mid-transaction (no save → freelist is stale and the compact would publish in-flight work) and on in-memory databases (no file). The path bypasses `executor::execute_vacuum` — that wrapper builds a user-facing status string and rejects in-transaction calls, both wrong for a silent maintenance hook — and calls `vacuum_database` directly.

## What it doesn't do (yet)

- **No LRU eviction.** `on_disk` + `wal_cache` together grow with the page count. For a 1 GiB database, that's ~1 GiB of page cache. Bounded cache is future work.
- **No per-statement granularity.** The whole database is re-serialized on every commit; the diff keeps the *written* set small but the CPU cost of reserialization is unchanged.
- **No concurrent reader-and-writer.** Phase 4e graduated to shared/exclusive lock modes (multi-reader *or* single-writer), but POSIX flock can't give us both at once. True concurrent access would need a shared-memory coordination file with read marks — not on the roadmap.
- **Savepoints / nested transactions.** Phase 4f added top-level `BEGIN` / `COMMIT` / `ROLLBACK` (snapshot-based rollback, auto-save suppressed inside a transaction), but nested `BEGIN` is rejected — real savepoints aren't on the roadmap.

## Interaction with `Database`

`Database` holds an `Option<Pager>`:

- `None` → in-memory only. Writes don't touch disk.
- `Some(pager)` → file-backed. `save_database(db, path)` takes the pager off, drives it, and re-attaches on success.

Being able to `take()` and put back a pager avoids threading lifetimes through the call stack. It does mean a mid-save failure leaves `db.pager == None` — which is fine, the next `.save` or auto-save will re-open the file from scratch.

## Testing

Unit tests in [`src/sql/pager/pager.rs`](../src/sql/pager/pager.rs):

- `create_then_open_round_trips` — verify `create` writes a valid empty DB and `open` can read it back.
- `create_spawns_wal_sidecar` — Phase 4c: `create` produces a header-only `-wal` file alongside the main file.
- `commit_writes_only_dirty_pages` — the diff test: write 3 pages, re-stage 2 unchanged + 1 changed, assert the commit only wrote 1 dirty data frame.
- `commit_leaves_main_file_untouched_and_shrink_hides_dropped_pages` — confirms the main file stays frozen across commits, the WAL grows, and a shrink-commit hides dropped pages via the bounds check even though their bytes linger in the main file until checkpoint.
- `wal_replay_on_reopen_restores_committed_state` — end-to-end: close a Pager after a commit, reopen, verify every staged page comes back via WAL replay.
- `two_commits_only_stage_the_delta` — two identical commits produce zero dirty data frames the second time (only the implicit commit frame).
- `second_pager_on_same_file_is_rejected` — Phase 4a: exclusive lock rejects simultaneous openers.
- **Phase 4d checkpoint suite:**
  - `explicit_checkpoint_folds_wal_into_main_file_and_truncates_wal` — after checkpoint, main file holds the committed content, WAL is back to header-only.
  - `checkpoint_is_idempotent` — back-to-back checkpoints: second is a no-op.
  - `checkpoint_with_shrink_truncates_main_file` — a shrink-commit followed by checkpoint actually makes the main file smaller.
  - `auto_checkpoint_fires_past_frame_threshold` — enough commits trigger auto-checkpoint; the WAL ends up empty on its own.
  - `reopen_after_crash_mid_checkpoint_recovers_via_wal` — closing before checkpoint is equivalent to a crash mid-checkpoint; WAL replay restores the post-commit view.
- **Phase 4e shared/exclusive suite:**
  - `two_read_only_openers_coexist` — two `open_read_only` calls on the same file succeed simultaneously.
  - `read_write_blocks_read_only_and_vice_versa` — POSIX flock semantics: RO excludes RW and vice versa.
  - `read_only_pager_rejects_mutations` — `commit` / `checkpoint` return typed errors; reads still work.
  - `read_only_open_without_wal_sidecar_succeeds` — a deleted sidecar isn't recreated in RO mode; reads fall back to the main file.

The 9 WAL-format tests live in [`src/sql/pager/wal.rs`](../src/sql/pager/wal.rs) and cover header / frame round-trips, torn-write recovery, and the orphan-dirty replay invariant in isolation from the Pager.

Higher-level tests in [`src/sql/pager/mod.rs`](../src/sql/pager/mod.rs) cover round-tripping populated databases, rejecting garbage, and handling tables that span multiple pages.
