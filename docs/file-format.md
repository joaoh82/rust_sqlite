# On-disk file format

A SQLRite database is a single file, by convention named `*.sqlrite`. The file is a sequence of fixed-size 4 KiB (4096-byte) pages.

All multi-byte integers in this format are **little-endian**.

## Page 0 — the database header

The first 4096 bytes of every file are the header page. Only the first 28 bytes carry information; the rest is reserved and zeroed.

```
┌────────┬────────┬─────────────────────────────────────────────────┐
│ offset │ length │ content                                         │
├────────┼────────┼─────────────────────────────────────────────────┤
│     0  │   16   │ magic:  "SQLRiteFormat\0\0\0"                   │
│    16  │    2   │ format version (u16 LE) = 1                     │
│    18  │    2   │ page size      (u16 LE) = 4096                  │
│    20  │    4   │ total page count (u32 LE), includes page 0      │
│    24  │    4   │ schema-root page number (u32 LE)                │
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
│        │        │   1 = SchemaRoot                                │
│        │        │   2 = TableData                                 │
│        │        │   3 = Overflow                                  │
│     1  │    4   │ next-page number (u32 LE; 0 = end of chain)     │
│     5  │    2   │ payload length on this page (u16 LE)            │
│     7  │ 4089   │ payload bytes (unused bytes are don't-care)     │
└────────┴────────┴─────────────────────────────────────────────────┘
```

Encoding / decoding of this layout lives in [`src/sql/pager/mod.rs`](../src/sql/pager/mod.rs) (`encode_payload_page`, `decode_page_header`). The per-page header size and maximum payload are exported as `PAGE_HEADER_SIZE` = 7 and `PAYLOAD_PER_PAGE` = 4089 from [`src/sql/pager/page.rs`](../src/sql/pager/page.rs).

### Page types

| Tag | Variant | Meaning |
|---|---|---|
| `1` | `SchemaRoot` | First page of the schema catalog's chain. Exactly one such page per file. |
| `2` | `TableData` | First page of one table's serialized contents. One such page per table. |
| `3` | `Overflow` | Continuation page. Appears only as a link target from another page's `next` field. |

Any other tag on open is a corruption error (`unknown page type tag N`).

## Chaining

If a logical payload (a table's bincode blob, or the schema catalog's bincode blob) is larger than 4089 bytes, it spills across multiple pages. The head page carries its real type (`SchemaRoot` or `TableData`); every continuation is an `Overflow` page. The head's `next` field points at the first overflow; each overflow's `next` points at the one after it; the last page in the chain has `next = 0`.

The chain is strictly linear — no branching, no back-pointers. Reassembly (`read_chain` in [`src/sql/pager/mod.rs`](../src/sql/pager/mod.rs)) is a simple while-loop that concatenates payload bytes until it hits a zero `next`.

## The schema catalog

The schema catalog is the single `SchemaRoot`-headed chain. Its payload bytes are a `bincode` encoding of:

```rust
Vec<(String, u32)>  // (table_name, start_page_of_that_table's_chain)
```

Opening a file loads this catalog first, then walks each `(name, start_page)` pair to load the corresponding table chain. The header's `schema_root_page` field tells the reader where the catalog chain starts.

## Per-table payload

Each table's bincode-encoded bytes are the `Table` struct (see [`src/sql/db/table.rs`](../src/sql/db/table.rs)):

```rust
Table {
    tb_name: String,
    columns: Vec<Column>,
    rows: Rc<RefCell<HashMap<String, Row>>>,
    indexes: HashMap<String, String>,
    last_rowid: i64,
    primary_key: String,
}
```

`Rc<RefCell<T>>` uses serde's `rc` feature; `RefCell` has a first-party serde impl. `Row` and `Column` both derive serde directly.

This is deliberately lazy — Phase 3c will replace "one bincode blob per table" with a cell-based page format where each row is stored as a separate cell, so rows can be read and written without deserializing the whole table. The current format won't survive Phase 3c.

## Layout example

A small database with two tables — `users` (small) and `notes` (small) — typically looks like:

```
page 0   header                       ← magic, version, page_count=4, schema_root=3
page 1   TableData  "notes"  next=0   ← bincode of the notes Table
page 2   TableData  "users"  next=0   ← bincode of the users Table
page 3   SchemaRoot          next=0   ← bincode of [("notes", 1), ("users", 2)]
```

Because table names are sorted alphabetically before writing (see [Design decisions §7](design-decisions.md#7-deterministic-page-number-ordering-when-saving)), the `notes` table lands on page 1 and `users` on page 2 deterministically.

If a table's bincode blob is larger than 4089 bytes, its chain extends before the next table begins:

```
page 0   header                       ← page_count=8, schema_root=7
page 1   TableData  "notes"  next=2   ← first 4089 bytes
page 2   Overflow            next=3
page 3   Overflow            next=0   ← last chunk
page 4   TableData  "users"  next=5
page 5   Overflow            next=6
page 6   Overflow            next=0
page 7   SchemaRoot          next=0   ← [("notes", 1), ("users", 4)]
```

## Invariants

A valid SQLRite file satisfies all of these:

- File length is a multiple of `PAGE_SIZE` (4096).
- File length ≥ `header.page_count × PAGE_SIZE`. (Equality is the norm; the Pager truncates when it shrinks.)
- Page 0's magic, version, and page size are as above.
- Every page in `1..page_count` starts with a valid page-type tag.
- No `next` pointer references a page number ≥ `page_count`.
- No two chains overlap — each non-header page belongs to exactly one chain.
- `schema_root_page` is the first page of exactly one chain, tagged `SchemaRoot`.

These are not all enforced on open — we validate the header strictly and rely on bincode decoding failing noisily if a chain is corrupt. A separate integrity-check command is on the long-term roadmap.

## Evolution

This format will change once Phase 3c lands. The per-page header and chaining mechanism are likely to stay; what lives *inside* the payload bytes will change from "one bincode blob per logical record" to "a sequence of variable-length cells", each cell carrying one row. The format version number in the header exists to signal that transition — version 1 (the current format) will either be migrated on open or rejected.
