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

`Pager::open(path)`:

1. Open the main file read-write, acquire the exclusive advisory lock (Phase 4a).
2. Read and verify page 0 (header) → initial `current_header`.
3. For page numbers `1..page_count`, seek + read the page, store in `on_disk`.
4. Open (or create, if missing) the `-wal` sidecar; acquire its exclusive lock.
5. Replay committed WAL frames into `wal_cache`.
6. If the WAL contains a page-0 frame, decode it and **override** `current_header` — the WAL's copy is always more recent than the main file's.
7. `staged` starts empty.

After open, the pager carries a complete snapshot of what's on disk plus every WAL-resident update since the last checkpoint. A pre-Phase-4c `.sqlrite` file (no sidecar) gets a fresh empty WAL on first open.

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

## How the diff earns its keep

Consider an `UPDATE` that touches exactly one row in one table:

1. `executor::execute_update` updates the in-memory `Table`.
2. `process_command` calls `pager::save_database`.
3. Every table is re-serialized to bytes and staged into the pager.
4. For the table whose row changed, the bincode bytes differ → those pages are dirty.
5. For every *other* table, the bincode bytes are byte-identical to what's already on disk → those pages stay clean → no writes.

Without the diff, step 3's "re-serialize every table" would trigger a full file rewrite on every statement. With the diff, disk I/O scales with the number of changed tables, not total tables.

This only works because `save_database` iterates tables in sorted order — if the order were random, a table that didn't change might land at a different page number, appearing dirty. See [Design decisions §7](design-decisions.md#7-deterministic-page-number-ordering-when-saving).

## What it doesn't do (yet)

- **No checkpointer.** The WAL grows without bound until the process exits (and even then the sidecar sits on disk). Phase 4d adds a checkpointer that flushes WAL frames back into the main file and truncates the log.
- **No LRU eviction.** `on_disk` + `wal_cache` together grow with the page count. For a 1 GiB database, that's ~1 GiB of page cache. Bounded cache is future work.
- **No free-page management.** When a table shrinks, the committed WAL state reflects the smaller page count but the main file's tail pages still linger until checkpoint. There's no free-list yet.
- **No per-statement granularity.** The whole database is re-serialized on every commit; the diff keeps the *written* set small but the CPU cost of reserialization is unchanged.
- **No shared/exclusive lock graduation.** One process, one open file handle — exclusive across the board. Phase 4e adds multi-reader / single-writer.
- **No `BEGIN` / `COMMIT` / `ROLLBACK`.** Every mutating statement is its own transaction today. Phase 4f layers transactions on the WAL.

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

The 8 WAL-format tests live in [`src/sql/pager/wal.rs`](../src/sql/pager/wal.rs) and cover header / frame round-trips and torn-write recovery in isolation from the Pager.

Higher-level tests in [`src/sql/pager/mod.rs`](../src/sql/pager/mod.rs) cover round-tripping populated databases, rejecting garbage, and handling tables that span multiple pages.
