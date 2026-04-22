# On-disk file format

A SQLRite database is a single file, by convention named `*.sqlrite`. The file is a sequence of fixed-size 4 KiB (4096-byte) pages.

All multi-byte integers in this format are **little-endian**.

The current on-disk format is **version 3** (Phase 3e). Files produced by earlier versions are rejected on open.

## Page 0 — the database header

The first 4096 bytes of every file are the header page. Only the first 28 bytes carry information; the rest is reserved and zeroed.

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │   16   │ magic:  "SQLRiteFormat\0\0\0"                   │
│    16  │    2   │ format version (u16 LE) = 3                     │
│    18  │    2   │ page size      (u16 LE) = 4096                  │
│    20  │    4   │ total page count (u32 LE), includes page 0      │
│    24  │    4   │ root page of sqlrite_master (u32 LE)            │
│    28  │ 4068   │ reserved / zero                                 │
└────────┴────────┴─────────────────────────────────────────────────┘
```

The magic string is 14 ASCII bytes (`SQLRiteFormat`) padded with two NUL bytes to fill 16 bytes. It's deliberately different from SQLite's `"SQLite format 3\0"` so the two formats can't be confused on inspection.

`decode_header` in [`src/sql/pager/header.rs`](../src/sql/pager/header.rs) validates all three of (magic, format version, page size) on open. A wrong magic produces `not a SQLRite database`; a wrong version or page size produces `unsupported ...` errors.

## Pages 1..page_count — payload pages

Every non-header page starts with a 7-byte header:

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │    1   │ page type tag (u8)                              │
│        │        │   2 = TableLeaf                                 │
│        │        │   3 = Overflow                                  │
│     1  │    4   │ next-page number (u32 LE; 0 = end of chain)     │
│     5  │    2   │ payload length (u16 LE)                         │
│     7  │ 4089   │ payload bytes                                   │
└────────┴────────┴─────────────────────────────────────────────────┘
```

`PAGE_HEADER_SIZE` = 7 and `PAYLOAD_PER_PAGE` = 4089 are constants in [`src/sql/pager/page.rs`](../src/sql/pager/page.rs).

### Page types

| Tag | Variant | Meaning |
|---|---|---|
| `2` | `TableLeaf` | Holds a slot directory and a set of cells representing rows of a table. Leaves for one table are linked by sibling `next_page` pointers. |
| `3` | `Overflow` | Continuation page carrying the spilled body of a single oversized cell. |
| `4` | `InteriorNode` | Interior B-Tree node. Holds a slot directory of divider cells routing to child pages plus a rightmost-child pointer in the payload header. |

Tag `1` is reserved (it was `SchemaRoot` in format v1; unused in v2). Any other tag on open is a corruption error.

For `TableLeaf` and `InteriorNode` pages the `payload length` field in the per-page header is unused (set to 0) — the slot directory inside the payload self-describes. For `Overflow` pages it records how many payload bytes the page carries toward the chain.

### Chaining

Each table is stored as a B-Tree:

- **Leaves** (`TableLeaf` pages) are also linked pairwise via each page's `next_page` field, forming a **sibling chain** in ascending rowid order terminated by `next_page = 0`. This lets sequential scans skip the tree and walk leaves directly.
- **Interior pages** (`InteriorNode`) sit above leaves, routing `find_by_rowid` queries down the tree. They don't use `next_page` (set to 0).

An `Overflow`-tagged page is the start or continuation of a single oversized cell's spilled body. Overflow chains are independent of the tree — an `OverflowRef` on a leaf cell points at the chain's first page.

## TableLeaf payload layout

Inside the 4089-byte payload area of a `TableLeaf` page:

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │    2   │ slot_count (u16 LE)                             │
│     2  │    2   │ cells_top  (u16 LE)  offset where cell content  │
│        │        │                      begins (=4089 on an empty  │
│        │        │                      page; shrinks as cells     │
│        │        │                      are added)                 │
│     4  │ 2*n    │ slot[0]..slot[n-1]   each u16 LE, pointing at   │
│        │        │                      the start of a cell. Slots │
│        │        │                      are kept in rowid-ascending│
│        │        │                      order; cell bodies are     │
│        │        │                      physically unordered.      │
│   ...  │  ...   │ [free space]                                    │
│ cells_ │ 4089 - │ cell bodies. Each cell is `cell_length varint`  │
│  top   │ cells_ │ then a typed body (see below).                  │
│        │ top    │                                                 │
└────────┴────────┴─────────────────────────────────────────────────┘
```

Slots grow up from offset 4; cells grow down from offset 4089. Free space is whatever's between them.

## InteriorNode payload layout

An interior page adds a rightmost-child pointer between the `cells_top` field and the slot directory. Layout (4089 bytes):

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │    2   │ slot_count       (u16 LE)                       │
│     2  │    2   │ cells_top        (u16 LE)                       │
│     4  │    4   │ rightmost_child  (u32 LE)  child page number    │
│        │        │                      for rowids larger than any │
│        │        │                      divider on this page       │
│     8  │ 2*n    │ slot[0]..slot[n-1]  each u16 LE, pointing at    │
│        │        │                     a divider cell. Slots are   │
│        │        │                     kept in divider_rowid-      │
│        │        │                     ascending order.            │
│   ...  │  ...   │ [free space]                                    │
│ cells_ │ 4089 - │ divider cell bodies.                            │
│  top   │ cells_ │                                                 │
│        │ top    │                                                 │
└────────┴────────┴─────────────────────────────────────────────────┘
```

An interior with N dividers points at N+1 children: `slot[i].child_page` owns rowids ≤ `slot[i].divider_rowid`, and `rightmost_child` owns everything past the last divider.

## Cell format

A cell is length-prefixed; its body starts with a `kind_tag` byte:

```
cell_length    varint          excludes itself; total bytes of kind_tag + body
kind_tag       u8              0x01 = Local    (full row on a leaf)
                               0x02 = Overflow (pointer to spilled body)
                               0x03 = Interior (divider on an interior node)
                               0x04 = Index    (one entry in an index leaf)
body           variable        depends on kind_tag
```

The shared prefix means `Cell::peek_rowid` works uniformly across all three kinds — useful for binary search over a page's slot directory without decoding full bodies.

### Local cell body

```
rowid          zigzag varint
col_count      varint
null_bitmap    ⌈col_count/8⌉ bytes   bit i of byte ⌊i/8⌋ set = column i is NULL
value_blocks   one block per non-NULL column, in declared column order
```

A value block:

```
tag       u8
  0x00 Integer      i64 zigzag varint
  0x01 Real         f64 little-endian, 8 bytes
  0x02 Text         varint length, UTF-8 bytes
  0x03 Bool         u8 (0 or 1)
body      variable (see tag)
```

### Overflow cell body

When a cell's full local encoding would exceed `OVERFLOW_THRESHOLD` (1022 bytes in the current code), the body is written to a chain of `Overflow` pages instead, and the on-page cell is replaced by a compact marker:

```
rowid                 zigzag varint
total_body_len        varint            bytes in the overflow chain
first_overflow_page   u32 LE            first page of the chain
```

The on-page marker is ~15 bytes. The rowid stays inline so the slot directory's binary search doesn't need to chase the chain.

### Overflow page payload

An `Overflow`-tagged page carries up to 4089 bytes of the chained cell body. The per-page header's `next_page` field points at the next link of the chain (or 0 at the tail); `payload length` records how many payload bytes this page carries.

Reading an overflow cell:

1. Start at `first_overflow_page`.
2. For each page in the chain, take the first `payload_length` bytes of its payload.
3. Stop when `next_page` is 0.
4. Concatenate — the result must equal `total_body_len` bytes, or the file is corrupt.
5. Feed those bytes to `Cell::decode` (they are a complete, properly length-prefixed local cell).

### Interior cell body

Used only on `InteriorNode` pages. Each divider owns all rowids ≤ `divider_rowid`, which route to `child_page`:

```
divider_rowid   zigzag varint
child_page      u32 LE            page holding the subtree for rowids up to divider_rowid
```

A rowid larger than every divider in the page routes to `rightmost_child` (from the payload header).

### Index cell body

Used only on the leaves of a secondary-index B-Tree. Each cell represents one `(indexed_value, rowid)` entry. The cell's rowid (the one `Cell::peek_rowid` sees, right after the kind tag) is the *base table* row's rowid — the value the index points at. The indexed value comes after, using the same tag-plus-body encoding as a `LocalCell` value block.

```
rowid           zigzag varint     base table row that carries this value
value_tag       u8                0x00 Integer / 0x01 Real / 0x02 Text / 0x03 Bool
value_body      variable          encoded per the Local cell's value-block rules
```

NULL values are never indexed — `SecondaryIndex::insert` skips them — so there's no null bitmap here; a non-null value is always present.

## The schema catalog: `sqlrite_master`

The schema catalog is itself a table named `sqlrite_master`, stored in the same `TableLeaf` format as any user table. Its schema is hardcoded into the engine so the open path can bootstrap:

```sql
CREATE TABLE sqlrite_master (
  type        TEXT NOT NULL,
  name        TEXT PRIMARY KEY,
  sql         TEXT NOT NULL,
  rootpage    INTEGER NOT NULL,
  last_rowid  INTEGER NOT NULL
);
```

There's one row per user table **and** one row per secondary index:

- **type** — either `'table'` or `'index'`
- **name** — the table or index name
- **sql** — the `CREATE TABLE` / `CREATE INDEX` statement, synthesized on save from in-memory metadata and re-parsed on open via `sqlparser` to reconstruct the schema
- **rootpage** — for a `'table'` row, the root of the table's B-Tree; for an `'index'` row, the root of the index's B-Tree
- **last_rowid** — the last rowid assigned to the table (so auto-increment picks up where it left off); `0` for `'index'` rows (meaningless there)

Save order is fixed for deterministic page numbers: every user table first (alphabetical), then every index (sorted by `(table, index_name)`), then `sqlrite_master` itself. Each `SecondaryIndex` produces its own `TableLeaf` chain whose cells are `KIND_INDEX` entries.

The header's `schema_root_page` field points at the first `TableLeaf` of `sqlrite_master`.

`sqlrite_master` is not exposed through `.tables`, `db.tables`, or `SELECT` — it's internal. The name is reserved: attempting to `CREATE TABLE sqlrite_master (...)` fails at parse time.

## Layout example

A small database with two user tables — `users` (small) and `notes` (small), each fitting in one leaf:

```
page 0   header                                     ← page_count=4, schema_root=3
page 1   TableLeaf  "notes"          next=0         ← cells for notes
page 2   TableLeaf  "users"          next=0         ← cells for users
page 3   TableLeaf  sqlrite_master   next=0         ← 2 rows, one per table above
```

Table names are sorted alphabetically before writing (see [Design decisions §7](design-decisions.md#7-deterministic-page-number-ordering-when-saving)), so `notes` lands before `users`. `sqlrite_master` always comes last so user tables get stable page numbers across saves.

When a table outgrows one leaf, its leaves chain via sibling `next_page`, and an `InteriorNode` page at the top routes lookups down:

```
page 0   header                                     ← page_count=7, schema_root=6
page 1   TableLeaf  "big"     next=2                ← rows 1..N1 (ascending rowid)
page 2   TableLeaf  "big"     next=3                ← rows N1+1..N2
page 3   TableLeaf  "big"     next=0                ← rows N2+1..N3   (end of chain)
page 4   InteriorNode "big"   next=0                ← root of the "big" tree
                                                      rightmost_child = page 3
                                                      dividers: (rowid=N1 → 1),
                                                                (rowid=N2 → 2)
page 5   TableLeaf  sqlrite_master  next=0
page 6   ... (unused in this example; see below)
```

A single-leaf table keeps its root pointing directly at the leaf — no interior layer is created. Taller trees (say, hundreds of leaves) grow an extra interior level: the root becomes an `InteriorNode` whose children are themselves `InteriorNode`s, each routing to a handful of leaves.

Overflow pages live independently of the tree; an `OverflowRef` cell on a leaf carries the first overflow page number. They can appear anywhere in the file.

## Invariants

A valid SQLRite file satisfies all of these:

- File length is a multiple of `PAGE_SIZE` (4096).
- File length ≥ `header.page_count × PAGE_SIZE`. (Equality is the norm; the Pager truncates when it shrinks.)
- Page 0's magic, version, and page size match the current constants.
- Every page in `1..page_count` starts with a valid page-type tag (2 or 3).
- No `next` pointer references a page number ≥ `page_count`.
- No two leaf chains overlap — each `TableLeaf` page belongs to exactly one table.
- `schema_root_page` is the first `TableLeaf` of `sqlrite_master`, which contains at minimum the rows for every user table in `db.tables`.

These are not all enforced on open — we validate the header strictly and rely on cell decoding failing noisily if a chain is corrupt. A separate integrity-check command is on the long-term roadmap.

## Format evolution

- **v1** (Phases 2 / 3a / 3b) — schema catalog and table data were opaque `bincode` blobs chained across typed payload pages.
- **v2** (Phases 3c / 3d) — cell-based storage and `sqlrite_master`. Phase 3d added interior pages without a version bump.
- **v3** (Phase 3e, current) — `sqlrite_master` gains a `type` column; secondary indexes persist as their own cell-based B-Trees whose leaves carry `KIND_INDEX` cells.

The page header (7 bytes) and chaining mechanism are stable across future phases. Phase 4's WAL introduces a sibling file (`.sqlrite-wal`) rather than changing the main file format.

## Write-Ahead Log (Phase 4b — standalone module; wiring in 4c)

A second file alongside the `.sqlrite`, named `<stem>.sqlrite-wal`, records page changes **before** they land in the main file. Readers consult the WAL first to get the latest view of a page; a periodic checkpointer (Phase 4d) applies the accumulated frames back into the main file and truncates the WAL.

### WAL header (first 32 bytes)

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │    8   │ magic:  "SQLRWAL\0"                             │
│     8  │    4   │ format version (u32 LE) = 1                     │
│    12  │    4   │ page size      (u32 LE) = 4096                  │
│    16  │    4   │ salt (u32 LE) — rolled each checkpoint          │
│    20  │    4   │ checkpoint seq (u32 LE) — increments per ckpt   │
│    24  │    8   │ reserved / zero                                 │
└────────┴────────┴─────────────────────────────────────────────────┘
```

### Frames

Each frame is `FRAME_HEADER_SIZE + PAGE_SIZE` = **4112 bytes**:

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │    4   │ page number (u32 LE)                            │
│     4  │    4   │ commit-page-count (u32 LE)                      │
│        │        │   0  = dirty frame (part of an open transaction)│
│        │        │  >0  = commit frame; value = total page count   │
│        │        │        in the main file after this transaction  │
│     8  │    4   │ salt (u32 LE) — copied from WAL header          │
│    12  │    4   │ checksum (u32 LE) — rolling sum over the first  │
│        │        │   12 header bytes and the PAGE_SIZE body        │
│    16  │ 4096   │ page body                                       │
└────────┴────────┴─────────────────────────────────────────────────┘
```

### Torn-write recovery

On open the reader walks every frame from `WAL_HEADER_SIZE`, validating salt and checksum. The first invalid or incomplete frame marks the end of the usable log — its bytes and anything after stay on disk but are treated as nonexistent. Callers get a clean in-memory index of `(page → latest-committed-frame-offset)` and a `last_commit_offset` boundary; uncommitted frames (no commit frame after them) are invisible to reads.

This means a crash mid-write can leave a partial trailing frame, and the next open will still reconstruct a consistent view — as long as the last successful commit frame made it to disk (via `fsync`, which `append_frame` does only for commit frames).

### Checksum

Rolling sum, `rotate_left(1) + byte`, over the first 12 header bytes plus the body. Order-sensitive, catches bit flips and byte shuffles without needing a crypto-grade dep.

### Salt

Rolled per checkpoint (Phase 4d). Prevents stale frames from an earlier generation of the WAL from being interpreted as valid after a truncate — their salt won't match the header's.

## Process-level locking

Starting with Phase 4a, every `Pager::open` / `Pager::create` takes a non-blocking OS **exclusive advisory lock** on the file (`fs2::FileExt::try_lock_exclusive` — `flock(LOCK_EX | LOCK_NB)` on Unix, `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)` on Windows). A second SQLRite process that tries to open the same file while another process already has it open fails immediately with:

```
database '/path/to/file.sqlrite' is already opened by another process (…)
```

The lock is tied to the underlying `File` descriptor, so it releases automatically when the `Pager` drops — no explicit unlock call. Tests and application code therefore need to scope `Database` lifetimes (or explicitly `drop` them) when they want to reopen the same file for verification.

**Single-writer-exclusive only**, for now. Phase 4e will upgrade to shared + exclusive lock modes once the WAL is in place so multiple readers can coexist with a single writer.
