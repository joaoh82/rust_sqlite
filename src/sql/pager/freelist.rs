//! Persisted free-page list (SQLR-6).
//!
//! After `DROP TABLE` / `DROP INDEX` / `ALTER TABLE DROP COLUMN`, the next
//! `save_database` no longer references the dropped object's pages. Without
//! a freelist those pages would either be re-numbered (every later table
//! shifts down → high write amplification) or stranded as orphans on disk.
//!
//! The freelist solves both: pages no longer referenced from `sqlrite_master`
//! go on a persisted stack rooted at `header.freelist_head`, so subsequent
//! saves can pull from there before extending the file. The unrelated tables
//! that didn't change keep their page numbers and their pages re-stage byte-
//! identical → the diff pager skips writing them.
//!
//! ## On-disk layout
//!
//! Each freelist trunk page carries the standard 7-byte page header with
//! `page_type = PAGE_TYPE_FREELIST_TRUNK (5)` and `next_page` pointing at the
//! next trunk in the chain (`0` = end). The 4089-byte payload area holds:
//!
//! ```text
//!   0..2      count: u16 LE       — number of free leaf-page IDs that follow
//!   2..2+4*N  page_ids[N]: u32 LE — free leaf-page numbers, ascending
//! ```
//!
//! A trunk holds up to `(PAYLOAD_PER_PAGE - 2) / 4 = 1021` free page IDs.
//! Larger freelists chain across multiple trunks. The trunk pages themselves
//! are part of the live page set (they hold metadata) and are *not* on the
//! freelist they encode.

use std::collections::VecDeque;

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::page::{PAGE_HEADER_SIZE, PAGE_SIZE, PAYLOAD_PER_PAGE};
use crate::sql::pager::pager::Pager;

/// Page-type tag for a freelist trunk page. Distinct from existing page tags
/// (`2 = TableLeaf`, `3 = Overflow`, `4 = InteriorNode`); `1` was retired.
pub const PAGE_TYPE_FREELIST_TRUNK: u8 = 5;

/// Maximum number of free page IDs a single trunk page can hold.
/// `PAYLOAD_PER_PAGE` is 4089; we reserve 2 bytes for the count and 4 bytes
/// per ID, giving `(4089 - 2) / 4 = 1021`.
pub const FREELIST_IDS_PER_TRUNK: usize = (PAYLOAD_PER_PAGE - 2) / 4;

/// Encodes a single freelist trunk page into the given buffer.
///
/// `next_trunk` is the page number of the next trunk in the chain, or `0`
/// to mark the end. `page_ids` must have at most `FREELIST_IDS_PER_TRUNK`
/// entries.
pub fn encode_trunk(buf: &mut [u8; PAGE_SIZE], next_trunk: u32, page_ids: &[u32]) -> Result<()> {
    if page_ids.len() > FREELIST_IDS_PER_TRUNK {
        return Err(SQLRiteError::Internal(format!(
            "freelist trunk overflow: {} ids exceeds capacity {}",
            page_ids.len(),
            FREELIST_IDS_PER_TRUNK
        )));
    }
    // Zero out the buffer; trailing payload bytes after the encoded IDs are
    // unused and must be deterministic so the diff pager can skip an
    // unchanged trunk.
    buf.fill(0);
    buf[0] = PAGE_TYPE_FREELIST_TRUNK;
    buf[1..5].copy_from_slice(&next_trunk.to_le_bytes());
    // Per-page `payload_length` field (bytes 5..7) is unused for trunks —
    // the count field inside the payload self-describes the entries.
    buf[5..7].copy_from_slice(&0u16.to_le_bytes());
    let count = page_ids.len() as u16;
    buf[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 2].copy_from_slice(&count.to_le_bytes());
    let mut off = PAGE_HEADER_SIZE + 2;
    for id in page_ids {
        buf[off..off + 4].copy_from_slice(&id.to_le_bytes());
        off += 4;
    }
    Ok(())
}

/// Decodes one freelist trunk page. Returns `(next_trunk, page_ids)`.
fn decode_trunk(buf: &[u8; PAGE_SIZE]) -> Result<(u32, Vec<u32>)> {
    if buf[0] != PAGE_TYPE_FREELIST_TRUNK {
        return Err(SQLRiteError::General(format!(
            "expected freelist trunk page (tag {PAGE_TYPE_FREELIST_TRUNK}), got tag {}",
            buf[0]
        )));
    }
    let next_trunk = u32::from_le_bytes(buf[1..5].try_into().unwrap());
    let count = u16::from_le_bytes(
        buf[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 2]
            .try_into()
            .unwrap(),
    ) as usize;
    if count > FREELIST_IDS_PER_TRUNK {
        return Err(SQLRiteError::General(format!(
            "freelist trunk count {count} exceeds capacity {FREELIST_IDS_PER_TRUNK}"
        )));
    }
    let mut ids = Vec::with_capacity(count);
    let mut off = PAGE_HEADER_SIZE + 2;
    for _ in 0..count {
        let id = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        ids.push(id);
        off += 4;
    }
    Ok((next_trunk, ids))
}

/// Walks the freelist chain rooted at `head` and returns every free leaf-page
/// ID. The trunk pages themselves are *not* included — they're live metadata.
/// Returns the trunk page numbers separately so the caller can release them
/// (the next save re-encodes the freelist from scratch and frees the old
/// trunks for reuse).
///
/// `head == 0` → empty freelist; returns `(vec![], vec![])`.
pub fn read_freelist(pager: &Pager, head: u32) -> Result<(Vec<u32>, Vec<u32>)> {
    let mut leaves: Vec<u32> = Vec::new();
    let mut trunks: Vec<u32> = Vec::new();
    let mut cursor = head;
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    while cursor != 0 {
        if !visited.insert(cursor) {
            return Err(SQLRiteError::General(format!(
                "freelist cycle detected at trunk page {cursor}"
            )));
        }
        let buf = pager.read_page(cursor).ok_or_else(|| {
            SQLRiteError::General(format!(
                "freelist trunk page {cursor} is past page_count or unreadable"
            ))
        })?;
        let (next, ids) = decode_trunk(buf)?;
        leaves.extend(ids);
        trunks.push(cursor);
        cursor = next;
    }
    Ok((leaves, trunks))
}

/// Stages the freelist chain into the pager. `free_pages` is the full set
/// of pages that need persisted-on-the-freelist treatment — both the trunk
/// pages (metadata) and the leaf entries (the actually-free leaves).
///
/// The chain consumes some of `free_pages` as its own trunks: a trunk
/// page IS a free page that's been temporarily borrowed for metadata.
/// Returns the new `freelist_head` (or `0` if `free_pages` is empty).
///
/// Encoding: `T = ceil(N / (IDS_PER_TRUNK + 1))` trunks; each trunk
/// (except possibly the last) carries `IDS_PER_TRUNK` leaf IDs. Leaves
/// are stored ascending within each trunk for deterministic on-disk
/// bytes (so an unchanged freelist re-stages byte-identical → diff skip).
pub fn stage_freelist(pager: &mut Pager, free_pages: Vec<u32>) -> Result<u32> {
    if free_pages.is_empty() {
        return Ok(0);
    }
    // Sort + dedup so the on-disk ordering is deterministic. A duplicate
    // in the freelist would be a serious bug elsewhere (double-free), but
    // dedupping here is a cheap defensive guardrail.
    let mut ids = free_pages;
    ids.sort_unstable();
    ids.dedup();

    // Solve N = T + L where L = number of leaf slots used, T = number of
    // trunks, and L ≤ T * IDS_PER_TRUNK. Smallest T satisfying that is
    // ceil(N / (IDS_PER_TRUNK + 1)) — the +1 accounts for the trunk
    // page itself absorbing one of the N pages.
    let n = ids.len();
    let t = n.div_ceil(FREELIST_IDS_PER_TRUNK + 1);

    // Take the highest-numbered T pages as trunks. This keeps the leaf
    // IDs ascending within each trunk and matches the "drain low first"
    // policy of the allocator on subsequent saves.
    let leaves_count = n - t;
    let trunk_pages: Vec<u32> = ids.split_off(leaves_count);
    let leaves = ids;

    // Lay out leaves across trunks, IDS_PER_TRUNK per trunk.
    let mut chunks: Vec<&[u32]> = leaves.chunks(FREELIST_IDS_PER_TRUNK).collect();
    // If there are more trunks than chunks (e.g. N=1 → T=1, L=0), pad
    // with empty chunks so every trunk gets staged.
    while chunks.len() < trunk_pages.len() {
        chunks.push(&[]);
    }

    for (i, chunk) in chunks.iter().enumerate() {
        let next = if i + 1 < trunk_pages.len() {
            trunk_pages[i + 1]
        } else {
            0
        };
        let mut buf = [0u8; PAGE_SIZE];
        encode_trunk(&mut buf, next, chunk)?;
        pager.stage_page(trunk_pages[i], buf);
    }

    Ok(trunk_pages[0])
}

/// Helper: collect a freelist into a `VecDeque<u32>` sorted ascending —
/// the ordering the `PageAllocator` uses to draw next free pages from.
pub fn freelist_to_deque(leaves: Vec<u32>) -> VecDeque<u32> {
    let mut sorted = leaves;
    sorted.sort_unstable();
    sorted.dedup();
    VecDeque::from(sorted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_freelist_round_trip() {
        let mut buf = [0u8; PAGE_SIZE];
        encode_trunk(&mut buf, 0, &[]).unwrap();
        let (next, ids) = decode_trunk(&buf).unwrap();
        assert_eq!(next, 0);
        assert!(ids.is_empty());
    }

    #[test]
    fn single_chunk_round_trip() {
        let mut buf = [0u8; PAGE_SIZE];
        let pages = [3u32, 7, 12, 99];
        encode_trunk(&mut buf, 42, &pages).unwrap();
        let (next, ids) = decode_trunk(&buf).unwrap();
        assert_eq!(next, 42);
        assert_eq!(ids, pages);
    }

    #[test]
    fn full_chunk_fits_capacity() {
        let mut buf = [0u8; PAGE_SIZE];
        let pages: Vec<u32> = (1..=FREELIST_IDS_PER_TRUNK as u32).collect();
        encode_trunk(&mut buf, 0, &pages).unwrap();
        let (next, ids) = decode_trunk(&buf).unwrap();
        assert_eq!(next, 0);
        assert_eq!(ids.len(), FREELIST_IDS_PER_TRUNK);
        assert_eq!(ids[0], 1);
        assert_eq!(
            ids[FREELIST_IDS_PER_TRUNK - 1],
            FREELIST_IDS_PER_TRUNK as u32
        );
    }

    #[test]
    fn over_capacity_errors() {
        let mut buf = [0u8; PAGE_SIZE];
        let pages: Vec<u32> = (1..=(FREELIST_IDS_PER_TRUNK as u32 + 1)).collect();
        let err = encode_trunk(&mut buf, 0, &pages).unwrap_err();
        assert!(format!("{err}").contains("freelist trunk overflow"));
    }

    #[test]
    fn wrong_tag_errors_on_decode() {
        let mut buf = [0u8; PAGE_SIZE];
        buf[0] = 2; // TableLeaf, not freelist
        let err = decode_trunk(&buf).unwrap_err();
        assert!(format!("{err}").contains("expected freelist trunk page"));
    }
}
