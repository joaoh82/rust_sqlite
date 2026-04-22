//! Overflow storage for cells that don't fit on one table-leaf page.
//!
//! Two pieces live here:
//!
//! - [`OverflowRef`] — the on-page marker that replaces a full cell when
//!   the cell's body is too large to keep inline. It carries the rowid
//!   (so the page's slot directory stays rowid-ordered), the total size
//!   of the external body, and a pointer to the first overflow page of
//!   the chain that holds the body.
//! - [`write_overflow_chain`] / [`read_overflow_chain`] — turn raw bytes
//!   into a chain of `Overflow`-typed pages and back. Each overflow page
//!   reuses the existing 7-byte page header (type tag + next-page + payload
//!   length) — we're not adding a new page format.
//!
//! **Decision to inline the rowid and NOT inline any of the body.** SQLite's
//! leaf-cell scheme keeps a prefix of the body inline before spilling, so
//! small lookups by rowid don't need a chain walk. We'd still have to chase
//! the chain for most columns anyway, so for simplicity this implementation
//! spills the entire body. A later optimization can split cells at a
//! threshold and keep a prefix inline without changing the page layout.
//!
//! **Overflow threshold.** Inserting a cell whose encoded length is more
//! than roughly a quarter of the page payload area (≈ 1000 bytes) is a
//! good candidate for overflow — on a ~4 KiB page you can still keep at
//! least 3-4 cells per page. The exact threshold is the caller's choice;
//! this module just exposes [`OVERFLOW_THRESHOLD`] as a suggestion.

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::cell::{KIND_LOCAL, KIND_OVERFLOW, Cell};
use crate::sql::pager::page::{PAGE_HEADER_SIZE, PAGE_SIZE, PAYLOAD_PER_PAGE, PageType};
use crate::sql::pager::pager::Pager;
use crate::sql::pager::varint;

/// Inline cell-body size above which the caller should consider overflowing.
/// Sized so at least 4 inline cells can coexist on a page alongside their
/// slot directory.
pub const OVERFLOW_THRESHOLD: usize = PAYLOAD_PER_PAGE / 4;

/// On-page marker that stands in for a cell whose body lives in an overflow
/// chain. Rowid is inlined so the page's binary search over slots still
/// works without chasing the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverflowRef {
    pub rowid: i64,
    /// Exact byte count that `read_overflow_chain` must produce; the
    /// caller then feeds those bytes to `LocalCellBody::decode`.
    pub total_body_len: u64,
    /// First page of the Overflow-type chain carrying the body.
    pub first_overflow_page: u32,
}

impl OverflowRef {
    /// Serializes the reference using the shared
    /// `[cell_length varint | kind_tag | body]` prefix; `kind_tag` is
    /// always `KIND_OVERFLOW` for this type.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(1 + varint::MAX_VARINT_BYTES * 2 + 4);
        body.push(KIND_OVERFLOW);
        varint::write_i64(&mut body, self.rowid);
        varint::write_u64(&mut body, self.total_body_len);
        body.extend_from_slice(&self.first_overflow_page.to_le_bytes());

        let mut out = Vec::with_capacity(body.len() + varint::MAX_VARINT_BYTES);
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        out
    }

    pub fn decode(buf: &[u8], pos: usize) -> Result<(OverflowRef, usize)> {
        let (body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let body_start = pos + len_bytes;
        let body_end = body_start
            .checked_add(body_len as usize)
            .ok_or_else(|| SQLRiteError::Internal("overflow ref length overflow".to_string()))?;
        if body_end > buf.len() {
            return Err(SQLRiteError::Internal(format!(
                "overflow ref extends past buffer: needs {body_start}..{body_end}, have {}",
                buf.len()
            )));
        }

        let body = &buf[body_start..body_end];
        if body.first().copied() != Some(KIND_OVERFLOW) {
            return Err(SQLRiteError::Internal(format!(
                "OverflowRef::decode called on non-overflow entry (kind_tag = {:#x})",
                body.first().copied().unwrap_or(0)
            )));
        }
        let mut cur = 1usize;
        let (rowid, n) = varint::read_i64(body, cur)?;
        cur += n;
        let (total_body_len, n) = varint::read_u64(body, cur)?;
        cur += n;
        if cur + 4 > body.len() {
            return Err(SQLRiteError::Internal(
                "overflow ref truncated before first_overflow_page".to_string(),
            ));
        }
        let first_overflow_page = u32::from_le_bytes(body[cur..cur + 4].try_into().unwrap());
        cur += 4;
        if cur != body.len() {
            return Err(SQLRiteError::Internal(format!(
                "overflow ref had {} trailing bytes",
                body.len() - cur
            )));
        }
        Ok((
            OverflowRef {
                rowid,
                total_body_len,
                first_overflow_page,
            },
            body_end - pos,
        ))
    }
}

/// An on-page entry: either a full local cell, or a pointer to an overflow
/// chain carrying the cell's body.
#[derive(Debug, Clone, PartialEq)]
pub enum PagedEntry {
    Local(Cell),
    Overflow(OverflowRef),
}

impl PagedEntry {
    pub fn rowid(&self) -> i64 {
        match self {
            PagedEntry::Local(c) => c.rowid,
            PagedEntry::Overflow(r) => r.rowid,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            PagedEntry::Local(c) => c.encode(),
            PagedEntry::Overflow(r) => Ok(r.encode()),
        }
    }

    /// Dispatches on the kind tag and returns the appropriate variant.
    pub fn decode(buf: &[u8], pos: usize) -> Result<(PagedEntry, usize)> {
        match Cell::peek_kind(buf, pos)? {
            KIND_LOCAL => {
                let (c, n) = Cell::decode(buf, pos)?;
                Ok((PagedEntry::Local(c), n))
            }
            KIND_OVERFLOW => {
                let (r, n) = OverflowRef::decode(buf, pos)?;
                Ok((PagedEntry::Overflow(r), n))
            }
            other => Err(SQLRiteError::Internal(format!(
                "unknown paged-entry kind tag {other:#x} at offset {pos}"
            ))),
        }
    }
}

/// Writes `bytes` into a chain of Overflow-typed pages starting at
/// `start_page`, using consecutive page numbers. Returns the first page
/// number *after* the chain (i.e., the next free page to hand out).
pub fn write_overflow_chain(
    pager: &mut Pager,
    bytes: &[u8],
    start_page: u32,
) -> Result<u32> {
    if bytes.is_empty() {
        return Err(SQLRiteError::Internal(
            "refusing to write an empty overflow chain — caller should inline instead".to_string(),
        ));
    }
    let mut current_page = start_page;
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let chunk_len = remaining.len().min(PAYLOAD_PER_PAGE);
        let (chunk, rest) = remaining.split_at(chunk_len);
        let next = if rest.is_empty() { 0 } else { current_page + 1 };
        pager.stage_page(current_page, encode_overflow_page(next, chunk)?);
        current_page += 1;
        remaining = rest;
    }
    Ok(current_page)
}

/// Walks an overflow chain starting at `first_page` and concatenates its
/// payload bytes. Reads exactly `total_body_len` bytes — a mismatch between
/// what the chain carries and what the OverflowRef claims is a corruption
/// error.
pub fn read_overflow_chain(
    pager: &Pager,
    first_page: u32,
    total_body_len: u64,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(total_body_len as usize);
    let mut current = first_page;
    while current != 0 {
        let raw = pager.read_page(current).ok_or_else(|| {
            SQLRiteError::Internal(format!(
                "overflow chain references missing page {current}"
            ))
        })?;
        let ty_byte = raw[0];
        if ty_byte != PageType::Overflow as u8 {
            return Err(SQLRiteError::Internal(format!(
                "page {current} was supposed to be Overflow but is type {ty_byte}"
            )));
        }
        let next = u32::from_le_bytes(raw[1..5].try_into().unwrap());
        let payload_len = u16::from_le_bytes(raw[5..7].try_into().unwrap()) as usize;
        if payload_len > PAYLOAD_PER_PAGE {
            return Err(SQLRiteError::Internal(format!(
                "overflow page {current} reports payload_len {payload_len} > max"
            )));
        }
        out.extend_from_slice(&raw[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload_len]);
        current = next;
    }
    if out.len() as u64 != total_body_len {
        return Err(SQLRiteError::Internal(format!(
            "overflow chain produced {} bytes, OverflowRef claimed {total_body_len}",
            out.len()
        )));
    }
    Ok(out)
}

/// Encodes a single `Overflow`-typed page holding `payload` bytes. Shared
/// with the rest of the pager via the standard 7-byte page header layout.
fn encode_overflow_page(next: u32, payload: &[u8]) -> Result<[u8; PAGE_SIZE]> {
    if payload.len() > PAYLOAD_PER_PAGE {
        return Err(SQLRiteError::Internal(format!(
            "overflow page payload {} exceeds max {PAYLOAD_PER_PAGE}",
            payload.len()
        )));
    }
    let mut buf = [0u8; PAGE_SIZE];
    buf[0] = PageType::Overflow as u8;
    buf[1..5].copy_from_slice(&next.to_le_bytes());
    buf[5..7].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    buf[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload.len()].copy_from_slice(payload);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::db::table::Value;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-overflow-{pid}-{nanos}-{name}.sqlrite"));
        p
    }

    #[test]
    fn overflow_ref_round_trip() {
        let r = OverflowRef {
            rowid: 42,
            total_body_len: 123_456,
            first_overflow_page: 7,
        };
        let bytes = r.encode();
        let (back, consumed) = OverflowRef::decode(&bytes, 0).unwrap();
        assert_eq!(back, r);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn paged_entry_dispatches_on_kind() {
        let local = Cell::new(1, vec![Some(Value::Integer(10))]);
        let local_bytes = local.encode().unwrap();
        let (decoded, _) = PagedEntry::decode(&local_bytes, 0).unwrap();
        assert_eq!(decoded, PagedEntry::Local(local));

        let overflow = OverflowRef {
            rowid: 2,
            total_body_len: 5000,
            first_overflow_page: 13,
        };
        let overflow_bytes = overflow.encode();
        let (decoded, _) = PagedEntry::decode(&overflow_bytes, 0).unwrap();
        assert_eq!(decoded, PagedEntry::Overflow(overflow));
    }

    #[test]
    fn peek_rowid_works_for_both_kinds() {
        let local = Cell::new(99, vec![Some(Value::Integer(1))]);
        let local_bytes = local.encode().unwrap();
        assert_eq!(Cell::peek_rowid(&local_bytes, 0).unwrap(), 99);

        let overflow = OverflowRef {
            rowid: -7,
            total_body_len: 100,
            first_overflow_page: 42,
        };
        let overflow_bytes = overflow.encode();
        assert_eq!(Cell::peek_rowid(&overflow_bytes, 0).unwrap(), -7);
    }

    #[test]
    fn write_then_read_overflow_chain() {
        let path = tmp_path("chain");
        let mut pager = Pager::create(&path).unwrap();

        // A blob that definitely spans multiple pages.
        let blob: Vec<u8> = (0..10_000).map(|i| (i % 251) as u8).collect();
        let pages_needed = blob.len().div_ceil(PAYLOAD_PER_PAGE) as u32;
        let start = 10u32;
        let next_free = write_overflow_chain(&mut pager, &blob, start).unwrap();
        assert_eq!(next_free, start + pages_needed);

        pager
            .commit(crate::sql::pager::header::DbHeader {
                page_count: next_free,
                schema_root_page: 1,
            })
            .unwrap();

        // Fresh pager to verify we read from disk.
        drop(pager);
        let pager = Pager::open(&path).unwrap();
        let back = read_overflow_chain(&pager, start, blob.len() as u64).unwrap();
        assert_eq!(back, blob);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_overflow_chain_rejects_length_mismatch() {
        let path = tmp_path("mismatch");
        let mut pager = Pager::create(&path).unwrap();
        let blob = vec![1u8; 500];
        let next = write_overflow_chain(&mut pager, &blob, 10).unwrap();
        pager
            .commit(crate::sql::pager::header::DbHeader {
                page_count: next,
                schema_root_page: 1,
            })
            .unwrap();

        // Claim more bytes than the chain actually carries.
        let err = read_overflow_chain(&pager, 10, 999).unwrap_err();
        assert!(format!("{err}").contains("overflow chain produced"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_chain_is_rejected() {
        let path = tmp_path("empty");
        let mut pager = Pager::create(&path).unwrap();
        let err = write_overflow_chain(&mut pager, &[], 10).unwrap_err();
        assert!(format!("{err}").contains("empty overflow chain"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn overflow_threshold_is_reasonable() {
        // The threshold should leave room for at least 4 cells per page.
        assert!(OVERFLOW_THRESHOLD <= PAYLOAD_PER_PAGE / 4);
        // And it should be comfortably larger than a typical small cell.
        assert!(OVERFLOW_THRESHOLD > 200);
    }
}
