//! Page layout primitives.
//!
//! Every database file is a sequence of fixed-size pages. Page 0 holds the
//! `DbHeader`; every other page is a typed, chained payload container.
//!
//! Layout of a non-header page (`PAGE_SIZE` bytes total):
//! ```text
//!   0..1      PageType tag   (u8)
//!   1..5      next-page      (u32 LE; 0 = end of chain)
//!   5..7      payload length (u16 LE; bytes used in the payload area)
//!   7..end    payload bytes
//! ```

use crate::error::{Result, SQLRiteError};

/// Size of every page in bytes. SQLite's default too — small enough to fit
/// in one disk sector group, large enough to carry meaningful payload.
pub const PAGE_SIZE: usize = 4096;

/// Bytes consumed by the per-page header (type + next-ptr + payload-len).
pub const PAGE_HEADER_SIZE: usize = 7;

/// Usable payload bytes per page after subtracting the header.
pub const PAYLOAD_PER_PAGE: usize = PAGE_SIZE - PAGE_HEADER_SIZE;

/// Identifies what kind of content a page holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    /// First page of the schema catalog blob.
    SchemaRoot = 1,
    /// First page of a table's bincode blob.
    TableData = 2,
    /// Continuation page in a multi-page chain (schema or table).
    Overflow = 3,
}

impl PageType {
    pub fn from_u8(v: u8) -> Result<PageType> {
        match v {
            1 => Ok(PageType::SchemaRoot),
            2 => Ok(PageType::TableData),
            3 => Ok(PageType::Overflow),
            other => Err(SQLRiteError::Internal(format!(
                "unknown page type tag {other}"
            ))),
        }
    }
}

/// Encodes a page of payload data into a `PAGE_SIZE`-byte buffer, ready to
/// be written to disk at the page's offset.
pub fn encode_page(ty: PageType, next: u32, payload: &[u8]) -> Result<[u8; PAGE_SIZE]> {
    if payload.len() > PAYLOAD_PER_PAGE {
        return Err(SQLRiteError::Internal(format!(
            "page payload {} bytes exceeds max {PAYLOAD_PER_PAGE}",
            payload.len()
        )));
    }
    let mut buf = [0u8; PAGE_SIZE];
    buf[0] = ty as u8;
    buf[1..5].copy_from_slice(&next.to_le_bytes());
    buf[5..7].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    buf[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload.len()].copy_from_slice(payload);
    Ok(buf)
}

/// Decodes a `PAGE_SIZE`-byte buffer into (page type, next-page, payload bytes).
pub fn decode_page(buf: &[u8]) -> Result<(PageType, u32, Vec<u8>)> {
    if buf.len() != PAGE_SIZE {
        return Err(SQLRiteError::Internal(format!(
            "page buffer length {} != PAGE_SIZE {PAGE_SIZE}",
            buf.len()
        )));
    }
    let ty = PageType::from_u8(buf[0])?;
    let next = u32::from_le_bytes(buf[1..5].try_into().unwrap());
    let payload_len = u16::from_le_bytes(buf[5..7].try_into().unwrap()) as usize;
    if payload_len > PAYLOAD_PER_PAGE {
        return Err(SQLRiteError::Internal(format!(
            "corrupt page: payload length {payload_len} exceeds max"
        )));
    }
    let payload = buf[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload_len].to_vec();
    Ok((ty, next, payload))
}
