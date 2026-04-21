# The Pager

The `Pager` is the component that actually talks to disk. Everything above it — the executor, the meta-commands, the Database itself — is blissfully unaware of file offsets. It hands bytes to the pager; the pager decides what gets written when.

The canonical code is in [`src/sql/pager/pager.rs`](../src/sql/pager/pager.rs).

## What it does

- Owns the one open file handle for the database.
- Maintains two maps of page bytes:
  - **`on_disk`** — byte snapshot of what we believe is currently on disk
  - **`staged`** — pages queued for the next commit
- On `commit(new_header)`:
  1. Compare each staged page against the `on_disk` snapshot.
  2. Write only pages whose bytes actually differ.
  3. Write the header (always).
  4. Truncate the file if the new page count shrank.
  5. `fsync`.
  6. Promote staged → on_disk; drop on_disk entries beyond the new page count; clear staged.

The effect: the *first* commit writes everything; subsequent commits write only what changed.

## Data structures

```rust
pub struct Pager {
    storage: FileStorage,                         // thin File wrapper
    current_header: DbHeader,                     // last committed header
    on_disk: HashMap<u32, Box<[u8; PAGE_SIZE]>>,  // last-known file contents
    staged: HashMap<u32, Box<[u8; PAGE_SIZE]>>,   // queued writes
}
```

`Box<[u8; PAGE_SIZE]>` heap-allocates each 4 KiB page so the hash map isn't dragging 4-KiB value inlines through rehashes.

## Lifecycle

### Opening an existing file

`Pager::open(path)`:

1. Open the file read-write.
2. Read and verify page 0 (header).
3. For page numbers `1..page_count`, seek + read the page, store in `on_disk`.
4. `staged` starts empty.

After open, the pager carries a complete snapshot of what's on disk. The cost is O(file size) on open and proportional memory — fine for small-to-medium DBs, revisited in Phase 3d.

### Creating a new file

`Pager::create(path)`:

1. Truncate-or-create the file.
2. Encode an empty schema catalog (`Vec<(String, u32)>::new()`) into a `SchemaRoot` page at page 1.
3. Write the header with `page_count = 2`, `schema_root_page = 1`.
4. Populate `on_disk` with page 1 so no-op commits don't rewrite it.
5. `fsync`.

The fresh DB is durable immediately — it has a valid empty schema and the file is big enough for a reader to detect the empty state.

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

`commit(new_header)`:

```rust
let dirty: Vec<_> = staged.into_iter()
    .filter(|(n, bytes)| match on_disk.get(n) {
        Some(existing) => existing != bytes,
        None => true,
    })
    .collect();
dirty.sort_by_key(|(n, _)| *n);  // sequential I/O

for (n, bytes) in &dirty {
    seek_to(n * PAGE_SIZE);
    write_all(bytes);
}
write_header(&new_header);
if new_header.page_count < self.current_header.page_count {
    truncate_to_pages(new_header.page_count);
}
fsync();

for (n, bytes) in dirty { on_disk.insert(n, bytes); }
on_disk.retain(|&n, _| n < new_header.page_count);
self.current_header = new_header;
```

The sort is important: it turns what would be scattered writes into sequential ones, letting the OS batch them into larger `write()` calls internally and keeping the head movement bounded on spinning disks (still worthwhile on SSDs for write coalescing).

`commit` returns the number of dirty writes, which a test asserts on to confirm the diff actually took effect.

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

- **No LRU eviction.** `on_disk` grows with the page count. For a 1 GiB database, that's 1 GiB of page cache. Phase 3d will bound this.
- **No free-page management.** When a table shrinks, we rewrite the whole thing and truncate the tail. There's no free-list to reuse pages inside the file. Phase 3c/3d will add one.
- **No per-statement granularity.** The whole database is re-serialized on every commit. Phase 3c (cell-based pages) is where individual row writes become possible without rewriting the whole table's blob.
- **No crash recovery beyond "reject files with bad magic".** The file can still be torn if a write lands halfway through. Phase 4 (WAL) addresses this.
- **No concurrency.** One process, one open file handle. Phase 4 also brings OS file locks.

## Interaction with `Database`

`Database` holds an `Option<Pager>`:

- `None` → in-memory only. Writes don't touch disk.
- `Some(pager)` → file-backed. `save_database(db, path)` takes the pager off, drives it, and re-attaches on success.

Being able to `take()` and put back a pager avoids threading lifetimes through the call stack. It does mean a mid-save failure leaves `db.pager == None` — which is fine, the next `.save` or auto-save will re-open the file from scratch.

## Testing

Unit tests in [`src/sql/pager/pager.rs`](../src/sql/pager/pager.rs):

- `create_then_open_round_trips` — verify `create` writes a valid empty DB and `open` can read it back.
- `commit_writes_only_dirty_pages` — the diff test: write 3 pages, re-stage 2 unchanged + 1 changed, assert commit only wrote 1 page.
- `commit_truncates_file_when_page_count_shrinks` — confirm the file gets smaller when the DB gets smaller.

Higher-level tests in [`src/sql/pager/mod.rs`](../src/sql/pager/mod.rs) cover round-tripping populated databases, rejecting garbage, and handling tables that span multiple pages.
