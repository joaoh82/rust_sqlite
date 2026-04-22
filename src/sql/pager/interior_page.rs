//! Interior B-Tree page layout.
//!
//! An interior page holds a slot directory of "divider" cells, each pointing
//! at a child page, plus a separate **rightmost-child** pointer that serves
//! as the catch-all for rowids larger than any divider. For N dividers,
//! the page points at N+1 children.
//!
//! ```text
//!   +---- page ---- (routes rowids to child pages by divider) ------+
//!   | divider[0] -> child[0]   (rowids <= divider[0])               |
//!   | divider[1] -> child[1]   (divider[0] < rowids <= divider[1])  |
//!   | ...                                                           |
//!   | divider[n-1] -> child[n-1]                                    |
//!   | rightmost_child          (rowids > divider[n-1])              |
//!   +---------------------------------------------------------------+
//! ```
//!
//! **Interior cell format.** Uses the shared
//! `[cell_length varint | kind_tag u8 | body]` prefix with
//! `kind_tag = KIND_INTERIOR`. Body:
//!
//! - `divider_rowid`  zigzag varint
//! - `child_page`     u32 little-endian
//!
//! `Cell::peek_rowid` works uniformly across all three kinds — interior
//! cells happen to have their divider rowid at the same position as a
//! local cell's row rowid, so binary search over the slot directory just
//! works.
//!
//! **Payload layout** (inside the 4089-byte payload area):
//!
//! ```text
//!   offset 0..2    slot_count         u16 LE
//!   offset 2..4    cells_top          u16 LE
//!   offset 4..8    rightmost_child    u32 LE
//!   offset 8..     slot[0]..slot[n-1] each u16 LE, divider-ordered
//!   [free space]
//!   offset cells_top..end              cell bodies
//! ```
//!
//! Parallel to `TablePage` except for the extra 4-byte `rightmost_child`
//! slot between the cells_top pointer and the slot directory. Call sites
//! that want a uniform "page with cells" abstraction should check the
//! page type tag first and use the appropriate struct.

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::cell::{Cell, KIND_INTERIOR};
use crate::sql::pager::page::PAYLOAD_PER_PAGE;
use crate::sql::pager::varint;

/// Byte offsets inside the payload area.
const OFFSET_SLOT_COUNT: usize = 0;
const OFFSET_CELLS_TOP: usize = 2;
const OFFSET_RIGHTMOST: usize = 4;
const PAGE_PAYLOAD_HEADER: usize = 8;

const SLOT_SIZE: usize = 2;

// -------------------------------------------------------------------------
// InteriorCell

/// One divider in an interior page: "rowids up to `divider_rowid` live in
/// the subtree under `child_page`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteriorCell {
    pub divider_rowid: i64,
    pub child_page: u32,
}

impl InteriorCell {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(1 + varint::MAX_VARINT_BYTES + 4);
        body.push(KIND_INTERIOR);
        varint::write_i64(&mut body, self.divider_rowid);
        body.extend_from_slice(&self.child_page.to_le_bytes());

        let mut out = Vec::with_capacity(body.len() + varint::MAX_VARINT_BYTES);
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        out
    }

    pub fn decode(buf: &[u8], pos: usize) -> Result<(InteriorCell, usize)> {
        let (body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let body_start = pos + len_bytes;
        let body_end = body_start
            .checked_add(body_len as usize)
            .ok_or_else(|| SQLRiteError::Internal("interior cell length overflow".to_string()))?;
        if body_end > buf.len() {
            return Err(SQLRiteError::Internal(format!(
                "interior cell extends past buffer: needs {body_start}..{body_end}, have {}",
                buf.len()
            )));
        }
        let body = &buf[body_start..body_end];
        if body.first().copied() != Some(KIND_INTERIOR) {
            return Err(SQLRiteError::Internal(format!(
                "InteriorCell::decode called on non-interior entry (kind_tag = {:#x})",
                body.first().copied().unwrap_or(0)
            )));
        }
        let mut cur = 1usize;
        let (divider_rowid, n) = varint::read_i64(body, cur)?;
        cur += n;
        if cur + 4 > body.len() {
            return Err(SQLRiteError::Internal(
                "interior cell truncated before child_page".to_string(),
            ));
        }
        let child_page = u32::from_le_bytes(body[cur..cur + 4].try_into().unwrap());
        cur += 4;
        if cur != body.len() {
            return Err(SQLRiteError::Internal(format!(
                "interior cell had {} trailing bytes",
                body.len() - cur
            )));
        }
        Ok((
            InteriorCell {
                divider_rowid,
                child_page,
            },
            body_end - pos,
        ))
    }
}

// -------------------------------------------------------------------------
// InteriorPage

/// An interior B-Tree page. Owns a heap-allocated 4089-byte payload buffer.
pub struct InteriorPage {
    buf: Box<[u8; PAYLOAD_PER_PAGE]>,
}

impl InteriorPage {
    /// Creates an empty interior page with the given rightmost-child pointer.
    /// Every interior page must have a valid rightmost-child — even if no
    /// dividers are present, the rightmost pointer serves as the catch-all
    /// route for all rowids.
    pub fn empty(rightmost_child: u32) -> Self {
        let mut buf = Box::new([0u8; PAYLOAD_PER_PAGE]);
        write_u16(&mut buf[..], OFFSET_SLOT_COUNT, 0);
        write_u16(&mut buf[..], OFFSET_CELLS_TOP, PAYLOAD_PER_PAGE as u16);
        write_u32(&mut buf[..], OFFSET_RIGHTMOST, rightmost_child);
        Self { buf }
    }

    pub fn from_bytes(bytes: &[u8; PAYLOAD_PER_PAGE]) -> Self {
        Self {
            buf: Box::new(*bytes),
        }
    }

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

    pub fn rightmost_child(&self) -> u32 {
        read_u32(&self.buf[..], OFFSET_RIGHTMOST)
    }

    #[allow(dead_code)]
    pub fn set_rightmost_child(&mut self, page: u32) {
        write_u32(&mut self.buf[..], OFFSET_RIGHTMOST, page);
    }

    const fn slots_start() -> usize {
        PAGE_PAYLOAD_HEADER
    }

    fn slots_end(&self) -> usize {
        Self::slots_start() + self.slot_count() * SLOT_SIZE
    }

    pub fn free_space(&self) -> usize {
        self.cells_top().saturating_sub(self.slots_end())
    }

    pub fn would_fit(&self, cell_encoded_size: usize) -> bool {
        cell_encoded_size.saturating_add(SLOT_SIZE) <= self.free_space()
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

    /// Divider rowid of the cell at `slot`, without full decode.
    pub fn divider_at(&self, slot: usize) -> Result<i64> {
        let offset = self.slot_offset(slot)?;
        Cell::peek_rowid(&self.buf[..], offset)
    }

    pub fn cell_at(&self, slot: usize) -> Result<InteriorCell> {
        let offset = self.slot_offset(slot)?;
        let (c, _) = InteriorCell::decode(&self.buf[..], offset)?;
        Ok(c)
    }

    /// Inserts a divider in ascending `divider_rowid` order. Returns an
    /// error if the new divider duplicates an existing one — the bulk-build
    /// code that populates interior pages always passes strictly increasing
    /// dividers, so a duplicate means a programmer-level bug.
    pub fn insert_divider(&mut self, divider_rowid: i64, child_page: u32) -> Result<()> {
        // Binary search for position.
        let mut lo = 0usize;
        let mut hi = self.slot_count();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_rowid = self.divider_at(mid)?;
            match mid_rowid.cmp(&divider_rowid) {
                std::cmp::Ordering::Equal => {
                    return Err(SQLRiteError::Internal(format!(
                        "duplicate interior divider {divider_rowid}"
                    )));
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        let insert_at = lo;

        let encoded = InteriorCell {
            divider_rowid,
            child_page,
        }
        .encode();

        if !self.would_fit(encoded.len()) {
            return Err(SQLRiteError::Internal(format!(
                "interior page full: cell of {} bytes + slot wouldn't fit in {} bytes",
                encoded.len(),
                self.free_space()
            )));
        }

        let new_cells_top = self.cells_top() - encoded.len();
        self.buf[new_cells_top..new_cells_top + encoded.len()].copy_from_slice(&encoded);
        self.set_cells_top(new_cells_top);

        let old_count = self.slot_count();
        let shift_start = Self::slots_start() + insert_at * SLOT_SIZE;
        let shift_end = Self::slots_start() + old_count * SLOT_SIZE;
        self.buf
            .copy_within(shift_start..shift_end, shift_start + SLOT_SIZE);
        self.set_slot_count(old_count + 1);
        self.set_slot_offset(insert_at, new_cells_top);
        Ok(())
    }

    /// Returns the child page that `rowid` routes to: the first divider
    /// with `rowid <= divider` owns the subtree; if `rowid` is larger than
    /// every divider, the rightmost child catches it.
    #[allow(dead_code)]
    pub fn child_for(&self, rowid: i64) -> Result<u32> {
        // Find the lowest slot whose divider >= rowid.
        let mut lo = 0usize;
        let mut hi = self.slot_count();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_rowid = self.divider_at(mid)?;
            if mid_rowid < rowid {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < self.slot_count() {
            let c = self.cell_at(lo)?;
            Ok(c.child_page)
        } else {
            Ok(self.rightmost_child())
        }
    }

    /// Returns the child page to descend into to find the smallest rowid
    /// under this interior. If there are dividers, it's slot 0's child;
    /// otherwise it's the rightmost (which is also the only) child.
    pub fn leftmost_child(&self) -> Result<u32> {
        if self.slot_count() == 0 {
            Ok(self.rightmost_child())
        } else {
            Ok(self.cell_at(0)?.child_page)
        }
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

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
    let bytes = value.to_le_bytes();
    buf[offset..offset + 4].copy_from_slice(&bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interior_cell_round_trip() {
        let c = InteriorCell {
            divider_rowid: 42,
            child_page: 7,
        };
        let bytes = c.encode();
        let (back, n) = InteriorCell::decode(&bytes, 0).unwrap();
        assert_eq!(back, c);
        assert_eq!(n, bytes.len());
    }

    #[test]
    fn peek_rowid_works_on_interior_cells() {
        let c = InteriorCell {
            divider_rowid: -99,
            child_page: 13,
        };
        let bytes = c.encode();
        assert_eq!(Cell::peek_rowid(&bytes, 0).unwrap(), -99);
    }

    #[test]
    fn decode_rejects_wrong_kind_tag() {
        use crate::sql::pager::cell::KIND_LOCAL;
        // Length + wrong kind.
        let mut buf = Vec::new();
        buf.push(1);
        buf.push(KIND_LOCAL);
        let err = InteriorCell::decode(&buf, 0).unwrap_err();
        assert!(format!("{err}").contains("non-interior"));
    }

    #[test]
    fn empty_interior_has_rightmost_but_no_dividers() {
        let p = InteriorPage::empty(99);
        assert_eq!(p.rightmost_child(), 99);
        assert_eq!(p.slot_count(), 0);
        assert_eq!(p.child_for(0).unwrap(), 99);
        assert_eq!(p.child_for(i64::MAX).unwrap(), 99);
    }

    #[test]
    fn insert_dividers_and_route_by_rowid() {
        let mut p = InteriorPage::empty(100);
        p.insert_divider(10, 1).unwrap();
        p.insert_divider(20, 2).unwrap();
        p.insert_divider(30, 3).unwrap();
        assert_eq!(p.slot_count(), 3);

        // rowids <= 10 → child 1
        assert_eq!(p.child_for(5).unwrap(), 1);
        assert_eq!(p.child_for(10).unwrap(), 1);
        // rowids in (10, 20] → child 2
        assert_eq!(p.child_for(11).unwrap(), 2);
        assert_eq!(p.child_for(20).unwrap(), 2);
        // rowids in (20, 30] → child 3
        assert_eq!(p.child_for(21).unwrap(), 3);
        assert_eq!(p.child_for(30).unwrap(), 3);
        // rowids > 30 → rightmost
        assert_eq!(p.child_for(31).unwrap(), 100);
        assert_eq!(p.child_for(i64::MAX).unwrap(), 100);
    }

    #[test]
    fn inserts_out_of_order_still_route_correctly() {
        let mut p = InteriorPage::empty(0);
        p.insert_divider(30, 3).unwrap();
        p.insert_divider(10, 1).unwrap();
        p.insert_divider(20, 2).unwrap();
        // After all three, the slot directory should be in divider-ascending order.
        assert_eq!(p.divider_at(0).unwrap(), 10);
        assert_eq!(p.divider_at(1).unwrap(), 20);
        assert_eq!(p.divider_at(2).unwrap(), 30);
    }

    #[test]
    fn duplicate_divider_rejected() {
        let mut p = InteriorPage::empty(0);
        p.insert_divider(10, 1).unwrap();
        let err = p.insert_divider(10, 99).unwrap_err();
        assert!(format!("{err}").contains("duplicate interior divider"));
    }

    #[test]
    fn bytes_round_trip() {
        let mut p = InteriorPage::empty(99);
        p.insert_divider(1, 10).unwrap();
        p.insert_divider(5, 20).unwrap();
        let bytes = *p.as_bytes();

        let p2 = InteriorPage::from_bytes(&bytes);
        assert_eq!(p2.rightmost_child(), 99);
        assert_eq!(p2.slot_count(), 2);
        assert_eq!(p2.cell_at(0).unwrap().divider_rowid, 1);
        assert_eq!(p2.cell_at(0).unwrap().child_page, 10);
        assert_eq!(p2.cell_at(1).unwrap().divider_rowid, 5);
        assert_eq!(p2.cell_at(1).unwrap().child_page, 20);
    }

    #[test]
    fn leftmost_child_picks_first_slot_or_rightmost() {
        let p = InteriorPage::empty(42);
        // No dividers → rightmost is the leftmost.
        assert_eq!(p.leftmost_child().unwrap(), 42);

        let mut q = InteriorPage::empty(99);
        q.insert_divider(10, 1).unwrap();
        q.insert_divider(20, 2).unwrap();
        // First slot is divider=10, child=1.
        assert_eq!(q.leftmost_child().unwrap(), 1);
    }

    #[test]
    fn many_dividers_fit_on_one_page() {
        // An interior cell is ~8 bytes; ~4080/10 ≈ 400 cells per page.
        let mut p = InteriorPage::empty(0);
        let mut inserted = 0usize;
        for i in 1..1000 {
            let cell = InteriorCell {
                divider_rowid: i,
                child_page: i as u32,
            };
            let size = cell.encode().len();
            if !p.would_fit(size) {
                break;
            }
            p.insert_divider(i, i as u32).unwrap();
            inserted += 1;
        }
        // Should comfortably exceed 100 dividers per page.
        assert!(inserted > 100);
    }
}
