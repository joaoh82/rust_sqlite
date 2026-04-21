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

// The actual encoding/decoding of a page into/out of a `PAGE_SIZE`-byte
// buffer lives in `pager/mod.rs`; those helpers used to live here but were
// inlined once the `Pager` took over raw byte I/O.
