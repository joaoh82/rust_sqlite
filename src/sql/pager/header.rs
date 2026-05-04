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
///   column type, plus the `KIND_HNSW` cell tag for vector ANN
///   indexes. All Phase 7 storage additions (VECTOR cells, JSON cells,
///   HNSW index nodes) live inside the v4 envelope.
/// - Version 5 (Phase 8c): adds the `KIND_FTS_POSTING` cell tag for
///   persisted FTS posting lists. Bumped **on demand** — a database
///   without any FTS index keeps writing v4. The first save with at
///   least one FTS index attached writes v5 instead. Decoders accept
///   both v4 and v5; v5 reading a v4-shaped DB just sees zero FTS
///   indexes in `sqlrite_master`. See [Phase 8 plan Q10].
/// - Version 6 (SQLR-6): adds a persisted free-page list at header
///   bytes [28..32] (`freelist_head`) plus the `PAGE_TYPE_FREELIST_TRUNK`
///   page tag. Bumped **on demand** — a save that produces no freed
///   pages keeps writing the file's existing version. The first save
///   that yields a non-empty freelist promotes the file to v6.
pub const FORMAT_VERSION_V4: u16 = 4;
pub const FORMAT_VERSION_V5: u16 = 5;
pub const FORMAT_VERSION_V6: u16 = 6;
/// The version a brand-new write defaults to when no FTS index forces
/// a bump. Existing databases keep their on-disk version unchanged
/// across reads + non-FTS writes; FTS-bearing saves switch to V5,
/// freelist-bearing saves switch to V6.
pub const FORMAT_VERSION_BASELINE: u16 = FORMAT_VERSION_V4;

/// Parsed header. `page_count` includes page 0 itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbHeader {
    pub page_count: u32,
    pub schema_root_page: u32,
    /// On-disk format version this header carries. Tracked explicitly
    /// so save can preserve a v4 file as v4 (no FTS, no freelist),
    /// bump it to v5 (FTS), or bump it to v6 (freelist), per the
    /// on-demand promotion rules.
    pub format_version: u16,
    /// First page of the persisted free-page list, or `0` if the list
    /// is empty. The freelist is a chain of trunk pages; each trunk
    /// records up to ~1018 free leaf-page numbers. v4/v5 files don't
    /// carry a freelist on disk — `decode_header` returns `0` for them.
    pub freelist_head: u32,
}

/// Encodes the header into a `PAGE_SIZE`-sized buffer.
pub fn encode_header(h: &DbHeader) -> [u8; PAGE_SIZE] {
    let mut buf = [0u8; PAGE_SIZE];
    buf[0..16].copy_from_slice(MAGIC);
    buf[16..18].copy_from_slice(&h.format_version.to_le_bytes());
    buf[18..20].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
    buf[20..24].copy_from_slice(&h.page_count.to_le_bytes());
    buf[24..28].copy_from_slice(&h.schema_root_page.to_le_bytes());
    buf[28..32].copy_from_slice(&h.freelist_head.to_le_bytes());
    buf
}

/// Decodes the header from a `PAGE_SIZE`-sized buffer. Returns an error if
/// magic bytes, format version, or page size don't match what we wrote.
/// V4, V5, and V6 are accepted; the result's `format_version` echoes
/// what was on disk so a no-op resave preserves it. `freelist_head` is
/// read from bytes [28..32] for V6 files; V4/V5 files have a zero
/// reserved region there, so the field decodes as `0` either way.
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
    if version != FORMAT_VERSION_V4 && version != FORMAT_VERSION_V5 && version != FORMAT_VERSION_V6
    {
        return Err(SQLRiteError::General(format!(
            "unsupported SQLRite format version {version}; this build understands \
             {FORMAT_VERSION_V4}, {FORMAT_VERSION_V5}, and {FORMAT_VERSION_V6}"
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
    let freelist_head = u32::from_le_bytes(buf[28..32].try_into().unwrap());
    Ok(DbHeader {
        page_count,
        schema_root_page,
        format_version: version,
        freelist_head,
    })
}
