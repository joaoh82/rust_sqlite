//! Database file header (page 0).
//!
//! The first 28 bytes of every `.sqlrite` file identify the format and point
//! at the schema catalog. The rest of page 0 is reserved for future use.

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::page::PAGE_SIZE;

/// File magic. Distinct from SQLite's `"SQLite format 3\0"` so the formats
/// can't be confused on inspection.
pub const MAGIC: &[u8; 16] = b"SQLRiteFormat\0\0\0";

/// On-disk format revision. Bump when the page layout changes incompatibly.
///
/// History:
/// - Version 1 (Phases 2 / 3a / 3b): schema catalog and table data were
///   opaque bincode blobs chained across typed payload pages.
/// - Version 2 (Phases 3c / 3d): tables are stored as cell-based B-Trees;
///   the schema catalog is itself a table called `sqlrite_master` with
///   four columns `(name, sql, rootpage, last_rowid)`.
/// - Version 3 (Phase 3e): `sqlrite_master` gains a `type` column
///   (first), distinguishing `'table'` and `'index'` rows; secondary
///   indexes persist as their own cell-based B-Trees whose cells use
///   the new `KIND_INDEX` format.
/// - Version 4 (Phase 7): cell encoding gains the `KIND_VECTOR` value
///   tag (length-prefixed dense f32 array) for the new `VECTOR(N)`
///   column type. Per the Phase 7 plan (`docs/phase-7-plan.md` Q8),
///   later Phase 7 sub-phases (JSON, HNSW indexes) will add their own
///   value/cell tags inside this same v4 envelope — no v5 mid-Phase-7.
pub const FORMAT_VERSION: u16 = 4;

/// Parsed header. `page_count` includes page 0 itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbHeader {
    pub page_count: u32,
    pub schema_root_page: u32,
}

/// Encodes the header into a `PAGE_SIZE`-sized buffer.
pub fn encode_header(h: &DbHeader) -> [u8; PAGE_SIZE] {
    let mut buf = [0u8; PAGE_SIZE];
    buf[0..16].copy_from_slice(MAGIC);
    buf[16..18].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    buf[18..20].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
    buf[20..24].copy_from_slice(&h.page_count.to_le_bytes());
    buf[24..28].copy_from_slice(&h.schema_root_page.to_le_bytes());
    buf
}

/// Decodes the header from a `PAGE_SIZE`-sized buffer. Returns an error if
/// magic bytes, format version, or page size don't match what we wrote.
pub fn decode_header(buf: &[u8]) -> Result<DbHeader> {
    if buf.len() != PAGE_SIZE {
        return Err(SQLRiteError::Internal(format!(
            "header buffer length {} != PAGE_SIZE {PAGE_SIZE}",
            buf.len()
        )));
    }
    if &buf[0..16] != MAGIC {
        return Err(SQLRiteError::General(
            "file is not a SQLRite database (bad magic bytes)".to_string(),
        ));
    }
    let version = u16::from_le_bytes(buf[16..18].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(SQLRiteError::General(format!(
            "unsupported SQLRite format version {version}; this build understands {FORMAT_VERSION}"
        )));
    }
    let page_size = u16::from_le_bytes(buf[18..20].try_into().unwrap()) as usize;
    if page_size != PAGE_SIZE {
        return Err(SQLRiteError::General(format!(
            "unsupported page size {page_size}; this build expects {PAGE_SIZE}"
        )));
    }
    let page_count = u32::from_le_bytes(buf[20..24].try_into().unwrap());
    let schema_root_page = u32::from_le_bytes(buf[24..28].try_into().unwrap());
    Ok(DbHeader {
        page_count,
        schema_root_page,
    })
}
