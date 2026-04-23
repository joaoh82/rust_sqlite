//! Table-leaf page layout: a slot directory + cell content.
//!
//! A `TablePage` operates on the **payload portion** of a page (the 4089
//! bytes that follow the 7-byte per-page header). The 7-byte header itself
//! (page type, next-page, legacy payload-length) is written by the caller
//! when the page is flushed to disk — this module doesn't touch it.
//!
//! Layout inside the payload area (`PAYLOAD_PER_PAGE` = 4089 bytes):
//!
//! ```text
//!   offset 0..2    slot_count                 u16 LE
//!   offset 2..4    cells_top                  u16 LE  (offset where cell
//!                                                      content begins)
//!   offset 4..     slot[0]..slot[n-1]         each u16 LE
//!                                             points at the start of a
//!                                             cell, ordered by rowid
//!   [free space]
//!   offset cells_top..PAYLOAD_PER_PAGE         cell content (unordered
//!                                             in physical position,
//!                                             ordered by slot)
//! ```
//!
//! `cells_top` = PAYLOAD_PER_PAGE on an empty page. Every insert shifts it
//! *down* (toward the slot directory) by the new cell's byte size; every
//! insert also expands the slot directory *up* (away from the header) by
//! 2 bytes.
//!
//! **Deletion leaves holes.** Removing a cell only strips its slot; the cell
//! bytes stay in place. This keeps deletion O(n) in slots-to-shift, not
//! O(page_size) in bytes-to-compact. The hole is reclaimed by a future
//! `vacuum` pass (not yet implemented). `free_space` therefore underreports
//! available space in fragmented pages — caller should treat its answer as
//! "contiguous free bytes", which is what a cell write actually needs.

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::cell::Cell;
use crate::sql::pager::overflow::PagedEntry;
use crate::sql::pager::page::PAYLOAD_PER_PAGE;

/// Byte offsets of the two header fields inside the payload area.
const OFFSET_SLOT_COUNT: usize = 0;
const OFFSET_CELLS_TOP: usize = 2;
const PAGE_PAYLOAD_HEADER: usize = 4;

/// Size of one slot entry (pointer to a cell's start).
const SLOT_SIZE: usize = 2;

/// Result of searching for a rowid in the slot directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Find {
    /// A cell with this rowid lives at `slot`.
    Found(usize),
    /// No cell has this rowid; inserting would place it at `slot`.
    NotFound(usize),
}

/// A table-leaf page. Owns a heap-allocated 4089-byte payload buffer.
pub struct TablePage {
    buf: Box<[u8; PAYLOAD_PER_PAGE]>,
}

impl TablePage {
    /// Creates a fresh, empty page.
    pub fn empty() -> Self {
        let mut buf = Box::new([0u8; PAYLOAD_PER_PAGE]);
        write_u16(&mut buf[..], OFFSET_SLOT_COUNT, 0);
        write_u16(&mut buf[..], OFFSET_CELLS_TOP, PAYLOAD_PER_PAGE as u16);
        Self { buf }
    }

    /// Rehydrates a page from its on-disk payload bytes.
    pub fn from_bytes(bytes: &[u8; PAYLOAD_PER_PAGE]) -> Self {
        Self {
            buf: Box::new(*bytes),
        }
    }

    /// Exposes the raw payload bytes for writing to the pager.
    pub fn as_bytes(&self) -> &[u8; PAYLOAD_PER_PAGE] {
        &self.buf
    }

    pub fn slot_count(&self) -> usize {
        read_u16(&self.buf[..], OFFSET_SLOT_COUNT) as usize
    }

    fn set_slot_count(&mut self, n: usize) {
        write_u16(&mut self.buf[..], OFFSET_SLOT_COUNT, n as u16);
    }

    pub fn cells_top(&self) -> usize {
        read_u16(&self.buf[..], OFFSET_CELLS_TOP) as usize
    }

    fn set_cells_top(&mut self, v: usize) {
        write_u16(&mut self.buf[..], OFFSET_CELLS_TOP, v as u16);
    }

    /// Start of the slot-directory region inside the payload.
    const fn slots_start() -> usize {
        PAGE_PAYLOAD_HEADER
    }

    fn slots_end(&self) -> usize {
        Self::slots_start() + self.slot_count() * SLOT_SIZE
    }

    /// Contiguous free bytes between the end of the slot directory and
    /// the top of the cell-content area.
    pub fn free_space(&self) -> usize {
        // cells_top is always >= slots_end in a well-formed page; clamp the
        // subtraction to avoid panics if the page buffer is corrupt.
        self.cells_top().saturating_sub(self.slots_end())
    }

    /// Returns true if a cell of the given encoded size fits, accounting
    /// for the extra 2 bytes needed for a new slot entry.
    pub fn would_fit(&self, cell_encoded_size: usize) -> bool {
        cell_encoded_size.saturating_add(SLOT_SIZE) <= self.free_space()
    }

    /// Raw byte offset, within the payload, where the cell for `slot`
    /// begins. Used by readers that decode the cell body with a type
    /// other than `PagedEntry` — e.g., index-cell leaves carry
    /// `IndexCell`s instead of row cells.
    pub fn slot_offset_raw(&self, slot: usize) -> Result<usize> {
        self.slot_offset(slot)
    }

    fn slot_offset(&self, slot: usize) -> Result<usize> {
        if slot >= self.slot_count() {
            return Err(SQLRiteError::Internal(format!(
                "slot {slot} out of bounds (count = {})",
                self.slot_count()
            )));
        }
        let at = Self::slots_start() + slot * SLOT_SIZE;
        Ok(read_u16(&self.buf[..], at) as usize)
    }

    fn set_slot_offset(&mut self, slot: usize, offset: usize) {
        let at = Self::slots_start() + slot * SLOT_SIZE;
        write_u16(&mut self.buf[..], at, offset as u16);
    }

    /// Reads the rowid of the cell at a given slot without decoding the
    /// full cell body. Used by `find` to binary-search the directory.
    pub fn rowid_at(&self, slot: usize) -> Result<i64> {
        let offset = self.slot_offset(slot)?;
        Cell::peek_rowid(&self.buf[..], offset)
    }

    /// Decodes the full cell stored at `slot`.
    pub fn cell_at(&self, slot: usize) -> Result<Cell> {
        let offset = self.slot_offset(slot)?;
        let (cell, _) = Cell::decode(&self.buf[..], offset)?;
        Ok(cell)
    }

    /// Binary-search for `rowid` in the slot directory.
    pub fn find(&self, rowid: i64) -> Result<Find> {
        let mut lo = 0usize;
        let mut hi = self.slot_count();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_rowid = self.rowid_at(mid)?;
            match mid_rowid.cmp(&rowid) {
                std::cmp::Ordering::Equal => return Ok(Find::Found(mid)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(Find::NotFound(lo))
    }

    /// Returns the cell with this rowid, or `None` if it's not on the page.
    pub fn lookup(&self, rowid: i64) -> Result<Option<Cell>> {
        match self.find(rowid)? {
            Find::Found(slot) => Ok(Some(self.cell_at(slot)?)),
            Find::NotFound(_) => Ok(None),
        }
    }

    /// Iterates `(rowid, Cell)` pairs in ascending rowid order. This is
    /// O(N × cell-decode) — only use it when you actually need the bodies.
    pub fn iter(&self) -> TablePageIter<'_> {
        TablePageIter { page: self, pos: 0 }
    }

    /// Inserts `cell` as a local entry in rowid order. See
    /// [`TablePage::insert_entry`] for the lower-level primitive that also
    /// accepts overflow pointers.
    pub fn insert(&mut self, cell: &Cell) -> Result<()> {
        let encoded = cell.encode()?;
        self.insert_entry(cell.rowid, &encoded)
    }

    /// Inserts either kind of paged entry. Delegates to
    /// [`TablePage::insert_entry`] after encoding.
    pub fn insert_paged_entry(&mut self, entry: &PagedEntry) -> Result<()> {
        let encoded = entry.encode()?;
        self.insert_entry(entry.rowid(), &encoded)
    }

    /// Inserts pre-encoded bytes at the slot that keeps the directory in
    /// rowid order. Fails with `Internal("page full")` if the bytes plus a
    /// new slot wouldn't fit, and with `Internal("duplicate rowid")` if a
    /// cell or overflow pointer with the same rowid already lives on the
    /// page. Callers should check `would_fit(encoded.len())` first — this
    /// method asserts the fit.
    pub fn insert_entry(&mut self, rowid: i64, encoded: &[u8]) -> Result<()> {
        match self.find(rowid)? {
            Find::Found(_) => Err(SQLRiteError::Internal(format!(
                "duplicate rowid {rowid} — caller must delete before re-inserting"
            ))),
            Find::NotFound(insert_at) => {
                if !self.would_fit(encoded.len()) {
                    return Err(SQLRiteError::Internal(format!(
                        "page full: entry of {} bytes + slot wouldn't fit in {} bytes of free space",
                        encoded.len(),
                        self.free_space()
                    )));
                }

                // Write entry content at the new cells_top.
                let new_cells_top = self.cells_top() - encoded.len();
                self.buf[new_cells_top..new_cells_top + encoded.len()].copy_from_slice(encoded);
                self.set_cells_top(new_cells_top);

                // Shift slot entries [insert_at..n) up by one to make room,
                // then write the new slot and bump the count.
                let old_count = self.slot_count();
                let shift_start = Self::slots_start() + insert_at * SLOT_SIZE;
                let shift_end = Self::slots_start() + old_count * SLOT_SIZE;
                self.buf
                    .copy_within(shift_start..shift_end, shift_start + SLOT_SIZE);
                self.set_slot_count(old_count + 1);
                self.set_slot_offset(insert_at, new_cells_top);
                Ok(())
            }
        }
    }

    /// Decodes the paged entry at `slot`. Either a local cell or an
    /// overflow pointer.
    pub fn entry_at(&self, slot: usize) -> Result<PagedEntry> {
        let offset = self.slot_offset(slot)?;
        let (entry, _) = PagedEntry::decode(&self.buf[..], offset)?;
        Ok(entry)
    }

    /// Removes the cell with `rowid`. Returns `Ok(true)` if it was found
    /// and removed, `Ok(false)` if the page didn't contain it. The cell's
    /// bytes stay in place — only its slot is dropped.
    pub fn delete(&mut self, rowid: i64) -> Result<bool> {
        let slot = match self.find(rowid)? {
            Find::Found(s) => s,
            Find::NotFound(_) => return Ok(false),
        };
        let old_count = self.slot_count();
        // Shift slots [slot+1..n) down by one.
        let shift_start = Self::slots_start() + (slot + 1) * SLOT_SIZE;
        let shift_end = Self::slots_start() + old_count * SLOT_SIZE;
        self.buf
            .copy_within(shift_start..shift_end, shift_start - SLOT_SIZE);
        self.set_slot_count(old_count - 1);
        Ok(true)
    }
}

pub struct TablePageIter<'a> {
    page: &'a TablePage,
    pos: usize,
}

impl<'a> Iterator for TablePageIter<'a> {
    type Item = Result<Cell>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.page.slot_count() {
            return None;
        }
        let cell = self.page.cell_at(self.pos);
        self.pos += 1;
        Some(cell)
    }
}

fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn write_u16(buf: &mut [u8], offset: usize, value: u16) {
    let bytes = value.to_le_bytes();
    buf[offset] = bytes[0];
    buf[offset + 1] = bytes[1];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::db::table::Value;

    fn int_cell(rowid: i64, v: i64) -> Cell {
        Cell::new(rowid, vec![Some(Value::Integer(v))])
    }

    fn text_cell(rowid: i64, s: &str) -> Cell {
        Cell::new(rowid, vec![Some(Value::Text(s.to_string()))])
    }

    #[test]
    fn empty_page_metadata_matches_spec() {
        let p = TablePage::empty();
        assert_eq!(p.slot_count(), 0);
        assert_eq!(p.cells_top(), PAYLOAD_PER_PAGE);
        // Free = PAYLOAD_PER_PAGE - 4 (payload header, no slots yet).
        assert_eq!(p.free_space(), PAYLOAD_PER_PAGE - PAGE_PAYLOAD_HEADER);
    }

    #[test]
    fn insert_lookup_iterate() {
        let mut p = TablePage::empty();
        p.insert(&int_cell(2, 20)).unwrap();
        p.insert(&int_cell(1, 10)).unwrap();
        p.insert(&int_cell(3, 30)).unwrap();
        assert_eq!(p.slot_count(), 3);

        // Lookups return the right cells regardless of insertion order.
        assert_eq!(p.lookup(1).unwrap(), Some(int_cell(1, 10)));
        assert_eq!(p.lookup(2).unwrap(), Some(int_cell(2, 20)));
        assert_eq!(p.lookup(3).unwrap(), Some(int_cell(3, 30)));
        assert_eq!(p.lookup(4).unwrap(), None);

        // Iteration yields cells in rowid-ascending order.
        let got: Vec<Cell> = p.iter().map(|r| r.unwrap()).collect();
        assert_eq!(got, vec![int_cell(1, 10), int_cell(2, 20), int_cell(3, 30)]);
    }

    #[test]
    fn duplicate_rowid_is_rejected() {
        let mut p = TablePage::empty();
        p.insert(&int_cell(1, 100)).unwrap();
        let err = p.insert(&int_cell(1, 200)).unwrap_err();
        assert!(format!("{err}").contains("duplicate rowid"));
    }

    #[test]
    fn delete_then_reinsert() {
        let mut p = TablePage::empty();
        p.insert(&int_cell(1, 10)).unwrap();
        p.insert(&int_cell(2, 20)).unwrap();
        p.insert(&int_cell(3, 30)).unwrap();

        assert!(p.delete(2).unwrap());
        assert_eq!(p.slot_count(), 2);
        assert_eq!(p.lookup(2).unwrap(), None);

        // Lookups on the survivors still work.
        assert_eq!(p.lookup(1).unwrap(), Some(int_cell(1, 10)));
        assert_eq!(p.lookup(3).unwrap(), Some(int_cell(3, 30)));

        // Rowid 2 can be re-inserted (with different body, even).
        p.insert(&int_cell(2, 999)).unwrap();
        assert_eq!(p.lookup(2).unwrap(), Some(int_cell(2, 999)));
    }

    #[test]
    fn delete_missing_rowid_reports_false() {
        let mut p = TablePage::empty();
        p.insert(&int_cell(1, 10)).unwrap();
        assert!(!p.delete(999).unwrap());
        assert_eq!(p.slot_count(), 1);
    }

    #[test]
    fn bytes_round_trip_through_from_bytes() {
        let mut p = TablePage::empty();
        p.insert(&text_cell(1, "alpha")).unwrap();
        p.insert(&text_cell(2, "beta")).unwrap();
        p.insert(&text_cell(3, "gamma")).unwrap();

        let bytes = *p.as_bytes();
        let p2 = TablePage::from_bytes(&bytes);
        assert_eq!(p2.slot_count(), 3);
        assert_eq!(p2.lookup(1).unwrap(), Some(text_cell(1, "alpha")));
        assert_eq!(p2.lookup(2).unwrap(), Some(text_cell(2, "beta")));
        assert_eq!(p2.lookup(3).unwrap(), Some(text_cell(3, "gamma")));
    }

    #[test]
    fn find_returns_insertion_slot() {
        let mut p = TablePage::empty();
        p.insert(&int_cell(10, 0)).unwrap();
        p.insert(&int_cell(20, 0)).unwrap();
        p.insert(&int_cell(30, 0)).unwrap();

        // Before the first element.
        assert_eq!(p.find(5).unwrap(), Find::NotFound(0));
        // Between two.
        assert_eq!(p.find(15).unwrap(), Find::NotFound(1));
        // After the last.
        assert_eq!(p.find(999).unwrap(), Find::NotFound(3));
        // Exact hit.
        assert_eq!(p.find(20).unwrap(), Find::Found(1));
    }

    #[test]
    fn would_fit_gates_insert() {
        let mut p = TablePage::empty();
        // Fill the page with ~200-byte cells until it says no.
        let body = "x".repeat(200);
        let mut rid = 0i64;
        let mut inserted = 0usize;
        loop {
            rid += 1;
            let c = text_cell(rid, &body);
            let size = c.encoded_len().unwrap();
            if !p.would_fit(size) {
                break;
            }
            p.insert(&c).unwrap();
            inserted += 1;
        }
        // Sanity: a 4089-byte page minus overhead should hold 15-20 such cells.
        assert!(inserted > 10 && inserted < 50);

        // After rejecting, the next insert *must* fail (free_space too small).
        let overflow = text_cell(rid, &body);
        let err = p.insert(&overflow).unwrap_err();
        assert!(format!("{err}").contains("page full"));
    }

    #[test]
    fn free_space_tracks_inserts_and_deletes_by_slot_only() {
        // Deletion leaves holes — free_space drops after insert but doesn't
        // fully recover after delete. Document the behavior explicitly.
        let mut p = TablePage::empty();
        let initial = p.free_space();

        let c = int_cell(1, 42);
        let cell_size = c.encoded_len().unwrap();
        p.insert(&c).unwrap();
        let after_insert = p.free_space();
        assert_eq!(after_insert, initial - cell_size - SLOT_SIZE);

        p.delete(1).unwrap();
        let after_delete = p.free_space();
        // We recovered the slot (2 bytes) but the cell content is still there.
        assert_eq!(after_delete, initial - cell_size);
    }

    #[test]
    fn mixed_types_and_nulls_round_trip_on_a_page() {
        let mut p = TablePage::empty();
        p.insert(&Cell::new(
            1,
            vec![
                Some(Value::Integer(10)),
                Some(Value::Text("hi".to_string())),
                None,
            ],
        ))
        .unwrap();
        p.insert(&Cell::new(
            2,
            vec![None, Some(Value::Real(2.5)), Some(Value::Bool(true))],
        ))
        .unwrap();

        let got: Vec<Cell> = p.iter().map(|r| r.unwrap()).collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].rowid, 1);
        assert_eq!(got[0].values[2], None);
        assert_eq!(got[1].rowid, 2);
        assert_eq!(got[1].values[0], None);
    }

    #[test]
    fn peek_rowid_matches_cell_at() {
        let mut p = TablePage::empty();
        p.insert(&int_cell(42, 0)).unwrap();
        p.insert(&int_cell(7, 0)).unwrap();
        p.insert(&int_cell(100, 0)).unwrap();
        // Slot order = rowid order: 7, 42, 100.
        assert_eq!(p.rowid_at(0).unwrap(), 7);
        assert_eq!(p.rowid_at(1).unwrap(), 42);
        assert_eq!(p.rowid_at(2).unwrap(), 100);
    }
}
