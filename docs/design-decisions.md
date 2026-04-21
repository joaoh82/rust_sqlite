# Design decisions

Short records of the major "why" choices in the codebase. If you read the source and something looks surprising, it's probably intentional for one of these reasons.

---

## 1. Use `sqlparser` rather than writing a parser

**Decision.** SQL parsing is delegated entirely to the [`sqlparser`](https://crates.io/crates/sqlparser) crate (SQLite dialect). The internal parser modules under `src/sql/parser/` only convert the crate's AST into trimmed-down structs we actually use.

**Why.** The project's goal is to learn database internals — storage, indexing, durability, concurrency. Writing a SQL tokenizer and recursive-descent parser is a different project and would consume time without advancing the stated goal. `sqlparser` is maintained, battle-tested, and covers enough of SQL that we won't run into "the parser doesn't support this" issues for a long time.

**Cost.** We're exposed to `sqlparser`'s API churn — the Phase 0 modernization had to deal with ~10 breaking changes in `sqlparser` 0.17 → 0.61. That's acceptable; we isolate the coupling to the parser module.

**Artifact.** An empty `src/sql/tokenizer.rs` lingers as a historical placeholder. It's not used.

---

## 2. 4 KiB page size, fixed at compile time

**Decision.** `PAGE_SIZE = 4096` in [`src/sql/pager/page.rs`](../src/sql/pager/page.rs) is a compile-time constant.

**Why.** 4 KiB is SQLite's default too. It matches typical OS page size, fits inside a single disk sector group on common storage, and is large enough that the 7-byte per-page header is negligible overhead.

**Cost.** A variable page size would be more flexible but adds per-file configuration noise and complicates the Pager's cache. Not worth it for this project. If it ever needs to change, the header carries the page size (currently validated == 4096 on open), so a later implementation could make it configurable and still read old files.

---

## 3. Single-file database (one `.sqlrite` per DB)

**Decision.** One file holds everything — schema, data, indexes. No separate journal or WAL file (yet).

**Why.** Matches SQLite's model, which is the whole point of this project. A directory-per-database scheme would avoid some pager complexity but then moving/copying a database becomes multi-file, which is a usability regression.

**Cost.** All I/O is serialized through one file descriptor. Phase 4 will add a `.sqlrite-wal` sibling file so readers don't block writers — but that's WAL specifically, not a structural split.

---

## 4. Header written last on save; magic bytes check on open

**Decision.** [`pager::save_database`](../src/sql/pager/mod.rs) stages every payload page first, commits them, and only after all pages are on disk does [`Pager::commit`](../src/sql/pager/pager.rs) write page 0 (the header). On open, [`decode_header`](../src/sql/pager/header.rs) rejects anything without the `SQLRiteFormat\0\0\0` magic bytes.

**Why.** Best-effort crash safety without a journal or WAL. If the process dies mid-save:

- If the header hasn't been written, page 0 stays zeroed (fresh file) or carries the previous header (reused file). Either way, open either rejects the file (good — the user knows something broke) or reads the previous consistent state (best case). No silent half-written database.
- Once WAL lands in Phase 4, this becomes obsolete.

**Cost.** We do one extra header write on every save. That's cheap (4 KiB, once).

---

## 5. `bincode` for on-disk encoding (Phases 2–3b)

**Decision.** Tables and the schema catalog are serialized with [`bincode` 2.0](https://crates.io/crates/bincode) using its serde integration. Every `#[derive(Serialize, Deserialize)]` type in the storage model round-trips for free.

**Why.** Zero extra code for a real on-disk format; the in-memory structures already have serde derives from earlier iterations. This is the fastest path to "the database actually persists" — we're not writing a SQLite-grade record format before we have a B-Tree.

**Cost.** bincode is not a stable on-disk format. A future refactor (Phase 3c → cell-based rows) *will* break file compatibility. We've accepted this — file format stability isn't a promise until Phase 5 or later.

---

## 6. Long-lived `Pager` with an in-memory page snapshot

**Decision.** When a database is opened, the `Pager` reads *every* page into memory (the `on_disk` map) and keeps the file open. Auto-saves stage the new page contents and commit; commit diffs staged vs. snapshot and writes only pages whose bytes changed.

**Why.** Auto-save is triggered after every SQL statement. Without a cache the whole file would be rewritten after every statement, which scales poorly as the DB grows. Keeping a byte snapshot in RAM lets commit skip pages whose content is identical — meaning a one-row UPDATE of one table doesn't rewrite the pages of unrelated tables.

**Cost.** Memory usage is O(page count) — every page is resident in RAM even if the application isn't actively reading it. For now that's fine; Phase 3d's proper B-Tree will invert this (LRU page cache with a bounded memory budget). Until then, small-to-medium databases fit easily.

---

## 7. Deterministic page-number ordering when saving

**Decision.** [`save_database`](../src/sql/pager/mod.rs) sorts table names alphabetically before writing. Same DB contents → same bytes at same page numbers, every time.

**Why.** The Pager's diff-based commit needs *positionally* stable page contents to detect "no change". If the writer chose a random order, a table that hasn't changed might land at a different page number, marking it dirty and forcing a write. Sorting eliminates that source of spurious writes.

**Cost.** One `Vec::sort()` per save. Negligible.

---

## 8. Runtime `Value` separate from on-disk `Row` enum

**Decision.** [`Row`](../src/sql/db/table.rs) (on-disk / in-memory storage) stores `BTreeMap<i64, i32>` for Integer columns, `BTreeMap<i64, String>` for Text, etc. The [`Value`](../src/sql/db/table.rs) enum used at query-evaluation time is separate and carries `Integer(i64)`, `Text(String)`, `Real(f64)`, `Bool(bool)`, `Null`.

**Why.** The storage types pick compact representations (i32, f32) suited to the existing naive layout, while the runtime `Value` uses the widest sensible variants (i64, f64) for arithmetic. Keeping them separate avoids losing precision on comparison and makes NULL a first-class runtime value without hacking around the storage's inability to hold NULL for numeric columns.

**Cost.** An extra conversion at the read/write boundary (`Row::get(rowid) → Value`, `set_value(col, rowid, Value)`). Negligible; these boundaries are already the places where we're doing work.

---

## 9. `NULL`-as-false in `WHERE` clauses

**Decision.** In [`eval_predicate`](../src/sql/executor.rs), a `WHERE` expression evaluating to `NULL` is treated as `false` — the row does *not* match.

**Why.** Matches SQL's three-valued logic in spirit: `NULL` propagates through comparisons, and a `WHERE` requires a definitely-true predicate. Doing full 3VL would mean tracking "unknown" as a third boolean state through the evaluator and returning `Option<bool>` from `as_bool`. For a learning project with no aggregates, implicit coercion to `false` is equivalent for every case we actually execute.

**Cost.** Diverges subtly from strict SQL on corner cases that involve `NULL` propagation through `NOT` or `AND`/`OR`. If this matters later, the evaluator can be upgraded to 3VL without touching callers.

---

## 10. No transactions yet

**Decision.** `BEGIN` / `COMMIT` / `ROLLBACK` aren't supported. Every statement implicitly commits.

**Why.** Transactions require either undo logging or a WAL, and Phase 4 is where those land. Shipping fake transactions that don't actually roll back would be a bug farm.

**Cost.** Atomic multi-statement operations aren't possible today. Users who want them should wait for Phase 4.

---

## 11. Sub-phase granularity in Phase 3

**Decision.** Phase 3 is split into 3a (auto-save), 3b (Pager + diffing commits), 3c (cell-based pages), 3d (B-Tree), 3e (secondary indexes). Each is an independent commit.

**Why.** The full Phase 3 is ~2000 lines of code. Landing it as one patch makes review impossible and hides regressions. The sub-phases are each small enough to understand in isolation. They also provide natural decision points — 3c's cell encoding in particular is a design choice worth pausing on.

**Cost.** Every intermediate phase has to be consistent on its own, which means a bit of "throwaway" glue that later phases replace. Accepted — the educational value of shippable slices is higher than the engineering cost of rewrites.
