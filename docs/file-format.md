# On-disk file format

A SQLRite database is a single file, by convention named `*.sqlrite`. The file is a sequence of fixed-size 4 KiB (4096-byte) pages.

All multi-byte integers in this format are **little-endian**.

The current on-disk format is **version 2** (Phase 3c). Files produced by earlier versions are rejected on open.

## Page 0 — the database header

The first 4096 bytes of every file are the header page. Only the first 28 bytes carry information; the rest is reserved and zeroed.

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │   16   │ magic:  "SQLRiteFormat\0\0\0"                   │
│    16  │    2   │ format version (u16 LE) = 2                     │
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
| `2` | `TableLeaf` | Holds a slot directory and a set of cells representing rows of a table. |
| `3` | `Overflow` | Continuation page carrying the spilled body of a single oversized cell. |

Tag `1` is reserved (it was `SchemaRoot` in format v1; unused in v2). Any other tag on open is a corruption error.

For `TableLeaf` pages the `payload length` field in the per-page header is unused (set to 0) — the slot directory inside the payload self-describes. For `Overflow` pages it records how many payload bytes the page carries toward the chain.

### Chaining

`TableLeaf` pages within a single table are linked via the per-page `next_page` field, forming a linear chain terminated by `next_page = 0`. An `Overflow`-tagged page is the start or continuation of a single oversized cell's spilled body.

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

## Cell format

A cell is one row. Two kinds exist, distinguished by a `kind_tag` byte right after the length prefix:

```
cell_length    varint          excludes itself; total bytes of kind_tag + body
kind_tag       u8              0x01 = Local, 0x02 = Overflow
body           variable        depends on kind_tag
```

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

## The schema catalog: `sqlrite_master`

The schema catalog is itself a table named `sqlrite_master`, stored in the same `TableLeaf` format as any user table. Its schema is hardcoded into the engine so the open path can bootstrap:

```sql
CREATE TABLE sqlrite_master (
  name        TEXT PRIMARY KEY,
  sql         TEXT NOT NULL,
  rootpage    INTEGER NOT NULL,
  last_rowid  INTEGER NOT NULL
);
```

Each user table gets one row in `sqlrite_master`:

- **name** — the table name
- **sql** — the CREATE TABLE statement, synthesized on save from the in-memory column metadata, re-parsed on open via `sqlparser` to reconstruct the columns
- **rootpage** — first `TableLeaf` page of the user table's row chain
- **last_rowid** — the last rowid assigned to the user table (so auto-increment can pick up where it left off)

The header's `schema_root_page` field points at the first `TableLeaf` of `sqlrite_master`.

`sqlrite_master` is not exposed through `.tables`, `db.tables`, or `SELECT` — it's internal. The name is reserved: attempting to `CREATE TABLE sqlrite_master (...)` fails at parse time.

## Layout example

A small database with two user tables — `users` (small) and `notes` (small):

```
page 0   header                                     ← page_count=4, schema_root=3
page 1   TableLeaf  "notes"          next=0         ← cells for notes
page 2   TableLeaf  "users"          next=0         ← cells for users
page 3   TableLeaf  sqlrite_master   next=0         ← 2 rows, one per table above
```

Table names are sorted alphabetically before writing (see [Design decisions §7](design-decisions.md#7-deterministic-page-number-ordering-when-saving)), so `notes` lands before `users`. `sqlrite_master` always comes last so user tables get stable page numbers across saves.

If a table's rows exceed one leaf, the leaves chain:

```
page 0   header                                     ← page_count=8, schema_root=7
page 1   TableLeaf  "big"  next=2                   ← first batch of cells
page 2   TableLeaf  "big"  next=3                   ← more cells
page 3   TableLeaf  "big"  next=0
page 4   TableLeaf  "small"  next=0
page 5   Overflow  next=6                           ← spilled cell body
page 6   Overflow  next=0
page 7   TableLeaf  sqlrite_master  next=0
```

(Overflow pages can appear anywhere in the file; the page-number ordering above is conceptual.)

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

Version 2 is the current on-disk format. It introduces cell-based rows and `sqlrite_master`.

The page header (7 bytes) and chaining mechanism are stable across future phases. Phase 3d (on-disk B-Tree) will add an `InteriorNode` page type (tag `4`) that sits *above* `TableLeaf` pages in a tree; leaf content stays in the current cell format. Format version bumps again when that lands.
