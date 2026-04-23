//! On-disk format for a single secondary-index entry.
//!
//! Each entry maps one indexed value to the rowid of the table row
//! carrying it. For the Phase 3e eager-load model we store one cell per
//! `(value, rowid)` pair on `TableLeaf`-style pages that live in their
//! own per-index B-Tree. The tree's shape is identical to a table's —
//! same leaves, same sibling-chain, same interior pages — so all the 3d
//! machinery carries over. The only thing different is the per-cell
//! encoding, signalled by `KIND_INDEX`.
//!
//! **Encoding.** Uses the shared `[cell_length | kind_tag | body]`
//! prefix. The body mirrors a one-column local cell (so value-block
//! helpers can be reused), except the `rowid` stored here is the
//! *original* row's rowid — the one the index entry points at.
//!
//! ```text
//!   cell_length   varint          bytes after this field
//!   kind_tag      u8 = 0x04       (KIND_INDEX)
//!   rowid         zigzag varint   original row's rowid
//!   value_tag     u8              one of INTEGER/REAL/TEXT/BOOL
//!   value_body    variable        the indexed value
//! ```
//!
//! NULLs are never indexed (see `SecondaryIndex::insert`), so there's
//! no null bitmap — a non-null value is always present.

use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::Value;
use crate::sql::pager::cell::{KIND_INDEX, decode_value, encode_value};
use crate::sql::pager::varint;

/// One `(value, rowid)` pair stored in a per-index B-Tree.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexCell {
    /// Rowid of the row in the base table that carries this value.
    pub rowid: i64,
    /// The indexed value. Always non-NULL (NULLs aren't indexed).
    pub value: Value,
}

impl IndexCell {
    pub fn new(rowid: i64, value: Value) -> Self {
        Self { rowid, value }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if matches!(self.value, Value::Null) {
            return Err(SQLRiteError::Internal(
                "refusing to encode a NULL index cell — NULLs aren't indexed".to_string(),
            ));
        }
        let mut body = Vec::with_capacity(1 + varint::MAX_VARINT_BYTES + 16);
        body.push(KIND_INDEX);
        varint::write_i64(&mut body, self.rowid);
        encode_value(&mut body, &self.value)?;

        let mut out = Vec::with_capacity(body.len() + varint::MAX_VARINT_BYTES);
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn decode(buf: &[u8], pos: usize) -> Result<(IndexCell, usize)> {
        let (body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let body_start = pos + len_bytes;
        let body_end = body_start
            .checked_add(body_len as usize)
            .ok_or_else(|| SQLRiteError::Internal("index cell length overflow".to_string()))?;
        if body_end > buf.len() {
            return Err(SQLRiteError::Internal(format!(
                "index cell extends past buffer: needs {body_start}..{body_end}, have {}",
                buf.len()
            )));
        }
        let body = &buf[body_start..body_end];
        if body.first().copied() != Some(KIND_INDEX) {
            return Err(SQLRiteError::Internal(format!(
                "IndexCell::decode called on non-index entry (kind_tag = {:#x})",
                body.first().copied().unwrap_or(0)
            )));
        }
        let mut cur = 1usize;
        let (rowid, n) = varint::read_i64(body, cur)?;
        cur += n;
        let (value, n) = decode_value(body, cur)?;
        cur += n;
        if cur != body.len() {
            return Err(SQLRiteError::Internal(format!(
                "index cell had {} trailing bytes",
                body.len() - cur
            )));
        }
        Ok((IndexCell { rowid, value }, body_end - pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::pager::cell::Cell;

    #[test]
    fn round_trip_integer_index_cell() {
        let c = IndexCell::new(42, Value::Integer(-7));
        let bytes = c.encode().unwrap();
        let (back, n) = IndexCell::decode(&bytes, 0).unwrap();
        assert_eq!(back, c);
        assert_eq!(n, bytes.len());
    }

    #[test]
    fn round_trip_text_index_cell() {
        let c = IndexCell::new(99, Value::Text("alice".to_string()));
        let bytes = c.encode().unwrap();
        let (back, _) = IndexCell::decode(&bytes, 0).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn peek_rowid_works_on_index_cells() {
        // Cell::peek_rowid skips the length prefix + kind tag and reads
        // the rowid varint — should work uniformly for every kind.
        let c = IndexCell::new(12345, Value::Integer(0));
        let bytes = c.encode().unwrap();
        assert_eq!(Cell::peek_rowid(&bytes, 0).unwrap(), 12345);
    }

    #[test]
    fn null_value_is_rejected() {
        let c = IndexCell::new(1, Value::Null);
        let err = c.encode().unwrap_err();
        assert!(format!("{err}").contains("NULLs aren't indexed"));
    }

    #[test]
    fn decode_rejects_wrong_kind_tag() {
        use crate::sql::pager::cell::KIND_LOCAL;
        let mut buf = Vec::new();
        buf.push(1);
        buf.push(KIND_LOCAL);
        let err = IndexCell::decode(&buf, 0).unwrap_err();
        assert!(format!("{err}").contains("non-index"));
    }
}
