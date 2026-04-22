//! Cell format: one row per cell, hand-rolled length-prefixed encoding.
//!
//! A cell represents a single row in a table, identified by its ROWID. The
//! layout is deliberately SQLite-adjacent but not bit-compatible:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ cell_length    varint      total bytes *after* this field,       │
//! │                            including the kind tag below          │
//! │ kind_tag       u8          0x01 = local cell (this module)       │
//! │                            0x02 = overflow pointer (see          │
//! │                            `OverflowRef` in `overflow.rs`)       │
//! │ rowid          zigzag varint                                     │
//! │ col_count      varint      number of declared columns            │
//! │ null_bitmap    ⌈col_count/8⌉ bytes                               │
//! │                 bit 0 of byte 0 = column 0, little-endian order  │
//! │ value_blocks   one block per non-NULL column, in column order    │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! A value block is a one-byte tag followed by type-specific bytes:
//!
//! ```text
//!   0x00 Integer    i64 zigzag-varint
//!   0x01 Real       f64 little-endian, 8 bytes
//!   0x02 Text       varint length, UTF-8 bytes
//!   0x03 Bool       u8 (0 or 1)
//! ```
//!
//! Design notes:
//!
//! - The null bitmap is duplicated information (the stream of value blocks
//!   could also carry a "Null" tag), but it's faster to skip over absent
//!   columns when projecting, and more compact when many columns are null.
//! - Integer values are stored as i64 on disk even though the in-memory
//!   `Row::Integer` storage today uses i32. Widening is lossless and makes
//!   the format stable against a future storage widening.
//! - Real values are f64 fixed-width rather than an encoded variant — the
//!   value is already floating-point, so entropy-based compression wouldn't
//!   help much, and fixed-width keeps decoding simple.
//! - `cell_length` does not include its own bytes. This lets a reader skip
//!   a cell without decoding it: `advance by (cell_length varint) bytes +
//!   cell_length value`.

use crate::error::{Result, SQLRiteError};
use crate::sql::db::table::Value;
use crate::sql::pager::varint;

/// Cell kind tags — first byte of every cell's body after the length prefix.
/// Readers dispatch on this to produce one of:
/// - a local [`Cell`] (this module) — a full row on a leaf page
/// - an `OverflowRef` (in the sibling `overflow` module) — a pointer to a
///   spilled cell body on a leaf page
/// - an `InteriorCell` (in `interior_page`) — a divider on an interior
///   tree node pointing at a child page
pub const KIND_LOCAL: u8 = 0x01;
pub const KIND_OVERFLOW: u8 = 0x02;
pub const KIND_INTERIOR: u8 = 0x03;

/// Value type tag stored in each non-NULL value block.
pub mod tag {
    pub const INTEGER: u8 = 0;
    pub const REAL: u8 = 1;
    pub const TEXT: u8 = 2;
    pub const BOOL: u8 = 3;
}

/// A decoded cell: one row's worth of values plus its rowid.
///
/// `values` is indexed by declared column position. `None` means the column
/// was NULL in this cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub rowid: i64,
    pub values: Vec<Option<Value>>,
}

impl Cell {
    pub fn new(rowid: i64, values: Vec<Option<Value>>) -> Self {
        Self { rowid, values }
    }

    /// Serializes the cell into freshly allocated bytes. The encoding starts
    /// with the shared `[cell_length | kind_tag]` prefix so readers can
    /// dispatch to the right decoder; `kind_tag` is always `KIND_LOCAL`
    /// for this type.
    pub fn encode(&self) -> Result<Vec<u8>> {
        // Build everything after `cell_length` first (kind_tag + body), so
        // we can write the length prefix once we know the size.
        let mut body = Vec::new();
        body.push(KIND_LOCAL);
        varint::write_i64(&mut body, self.rowid);
        varint::write_u64(&mut body, self.values.len() as u64);
        encode_null_bitmap(&mut body, &self.values);
        for v in &self.values {
            if let Some(v) = v {
                encode_value(&mut body, v)?;
            }
        }

        let mut out = Vec::with_capacity(body.len() + varint::MAX_VARINT_BYTES);
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Returns the byte length of the encoded form. Convenient for
    /// fit-in-page calculations without actually encoding.
    pub fn encoded_len(&self) -> Result<usize> {
        // Computing the exact length requires knowing each value's encoded
        // size, which is cheapest by encoding; we re-use `encode()` and
        // accept the allocation.
        Ok(self.encode()?.len())
    }

    /// Reads the rowid out of an encoded entry (either a local cell or an
    /// overflow pointer), skipping the rest. Used by binary search on a
    /// page's slot directory — both kinds have rowid at the same position
    /// relative to the kind tag.
    pub fn peek_rowid(buf: &[u8], pos: usize) -> Result<i64> {
        let (_body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let body_start = pos + len_bytes;
        // Skip the kind_tag byte.
        if body_start >= buf.len() {
            return Err(SQLRiteError::Internal(
                "paged cell truncated before kind tag".to_string(),
            ));
        }
        let (rowid, _) = varint::read_i64(buf, body_start + 1)?;
        Ok(rowid)
    }

    /// Returns the total encoded length (including the `cell_length` prefix)
    /// of the cell-or-overflow-ref that starts at `buf[pos]`. Does not
    /// fully decode the body.
    pub fn encoded_size_at(buf: &[u8], pos: usize) -> Result<usize> {
        let (body_len, len_bytes) = varint::read_u64(buf, pos)?;
        Ok(len_bytes + body_len as usize)
    }

    /// Peeks the kind tag (`KIND_LOCAL` or `KIND_OVERFLOW`) of an entry
    /// without full decode.
    pub fn peek_kind(buf: &[u8], pos: usize) -> Result<u8> {
        let (_body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let kind_pos = pos + len_bytes;
        buf.get(kind_pos)
            .copied()
            .ok_or_else(|| SQLRiteError::Internal("paged cell truncated before kind tag".to_string()))
    }

    /// Decodes a local cell starting at `buf[pos]`. Returns
    /// `(cell, bytes_consumed)`. Errors if the entry at `pos` is not a
    /// local cell (e.g., it's an overflow pointer instead) — callers that
    /// can't be sure should go through `PagedEntry::decode`.
    pub fn decode(buf: &[u8], pos: usize) -> Result<(Cell, usize)> {
        let (body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let body_start = pos + len_bytes;
        let body_end = body_start
            .checked_add(body_len as usize)
            .ok_or_else(|| SQLRiteError::Internal("cell length overflow".to_string()))?;
        if body_end > buf.len() {
            return Err(SQLRiteError::Internal(format!(
                "cell extends past buffer: needs bytes {body_start}..{body_end}, have {}",
                buf.len()
            )));
        }

        let body = &buf[body_start..body_end];
        if body.is_empty() {
            return Err(SQLRiteError::Internal(
                "paged cell body is empty (no kind tag)".to_string(),
            ));
        }
        let kind_tag = body[0];
        if kind_tag != KIND_LOCAL {
            return Err(SQLRiteError::Internal(format!(
                "Cell::decode called on non-local entry (kind_tag = {kind_tag:#x})"
            )));
        }
        let mut cur = 1usize;

        let (rowid, n) = varint::read_i64(body, cur)?;
        cur += n;
        let (col_count_u, n) = varint::read_u64(body, cur)?;
        cur += n;
        let col_count = col_count_u as usize;

        let bitmap_bytes = col_count.div_ceil(8);
        if cur + bitmap_bytes > body.len() {
            return Err(SQLRiteError::Internal(
                "cell body truncated before null bitmap ends".to_string(),
            ));
        }
        let bitmap = &body[cur..cur + bitmap_bytes];
        cur += bitmap_bytes;

        let mut values = Vec::with_capacity(col_count);
        for col in 0..col_count {
            if is_null(bitmap, col) {
                values.push(None);
            } else {
                let (v, n) = decode_value(body, cur)?;
                cur += n;
                values.push(Some(v));
            }
        }

        if cur != body.len() {
            return Err(SQLRiteError::Internal(format!(
                "cell body had {} trailing bytes after last value",
                body.len() - cur
            )));
        }

        Ok((Cell { rowid, values }, body_end - pos))
    }
}

fn encode_null_bitmap(out: &mut Vec<u8>, values: &[Option<Value>]) {
    let n = values.len().div_ceil(8);
    let start = out.len();
    out.resize(start + n, 0);
    for (i, v) in values.iter().enumerate() {
        if v.is_none() {
            let byte_idx = start + (i / 8);
            let bit = i % 8;
            out[byte_idx] |= 1 << bit;
        }
    }
}

fn is_null(bitmap: &[u8], col: usize) -> bool {
    let byte = col / 8;
    let bit = col % 8;
    bitmap.get(byte).is_some_and(|b| (b >> bit) & 1 == 1)
}

fn encode_value(out: &mut Vec<u8>, value: &Value) -> Result<()> {
    match value {
        Value::Integer(i) => {
            out.push(tag::INTEGER);
            varint::write_i64(out, *i);
        }
        Value::Real(f) => {
            out.push(tag::REAL);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Text(s) => {
            out.push(tag::TEXT);
            let bytes = s.as_bytes();
            varint::write_u64(out, bytes.len() as u64);
            out.extend_from_slice(bytes);
        }
        Value::Bool(b) => {
            out.push(tag::BOOL);
            out.push(if *b { 1 } else { 0 });
        }
        Value::Null => {
            return Err(SQLRiteError::Internal(
                "Null values are encoded via the null bitmap, not a value block".to_string(),
            ));
        }
    }
    Ok(())
}

fn decode_value(buf: &[u8], pos: usize) -> Result<(Value, usize)> {
    let tag = *buf.get(pos).ok_or_else(|| {
        SQLRiteError::Internal(format!("value block truncated at offset {pos}"))
    })?;
    let body_start = pos + 1;
    match tag {
        tag::INTEGER => {
            let (v, n) = varint::read_i64(buf, body_start)?;
            Ok((Value::Integer(v), 1 + n))
        }
        tag::REAL => {
            let end = body_start + 8;
            if end > buf.len() {
                return Err(SQLRiteError::Internal(
                    "Real value truncated: needs 8 bytes".to_string(),
                ));
            }
            let arr: [u8; 8] = buf[body_start..end].try_into().unwrap();
            Ok((Value::Real(f64::from_le_bytes(arr)), 1 + 8))
        }
        tag::TEXT => {
            let (len, n) = varint::read_u64(buf, body_start)?;
            let text_start = body_start + n;
            let text_end = text_start + (len as usize);
            if text_end > buf.len() {
                return Err(SQLRiteError::Internal(
                    "Text value truncated".to_string(),
                ));
            }
            let s = std::str::from_utf8(&buf[text_start..text_end])
                .map_err(|e| SQLRiteError::Internal(format!("Text value is not valid UTF-8: {e}")))?
                .to_string();
            Ok((Value::Text(s), 1 + n + (len as usize)))
        }
        tag::BOOL => {
            let byte = *buf.get(body_start).ok_or_else(|| {
                SQLRiteError::Internal("Bool value truncated".to_string())
            })?;
            Ok((Value::Bool(byte != 0), 1 + 1))
        }
        other => Err(SQLRiteError::Internal(format!(
            "unknown value tag {other:#x} at offset {pos}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(cell: &Cell) {
        let bytes = cell.encode().unwrap();
        let (back, consumed) = Cell::decode(&bytes, 0).unwrap();
        assert_eq!(&back, cell);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn empty_cell_no_columns() {
        round_trip(&Cell::new(1, vec![]));
    }

    #[test]
    fn integer_only_cell() {
        round_trip(&Cell::new(
            42,
            vec![Some(Value::Integer(1)), Some(Value::Integer(-1000))],
        ));
    }

    #[test]
    fn mixed_types_cell() {
        round_trip(&Cell::new(
            100,
            vec![
                Some(Value::Integer(7)),
                Some(Value::Text("hello".to_string())),
                Some(Value::Real(3.14)),
                Some(Value::Bool(true)),
            ],
        ));
    }

    #[test]
    fn nulls_interspersed() {
        round_trip(&Cell::new(
            5,
            vec![
                Some(Value::Integer(1)),
                None,
                Some(Value::Text("middle".to_string())),
                None,
                None,
                Some(Value::Bool(false)),
            ],
        ));
    }

    #[test]
    fn all_null_cell() {
        round_trip(&Cell::new(9, vec![None, None, None, None, None, None, None, None, None]));
    }

    #[test]
    fn large_text_cell() {
        let big = "abc".repeat(10_000);
        round_trip(&Cell::new(1, vec![Some(Value::Text(big))]));
    }

    #[test]
    fn utf8_text_cell() {
        round_trip(&Cell::new(
            1,
            vec![Some(Value::Text("héllo 🦀 世界".to_string()))],
        ));
    }

    #[test]
    fn negative_and_large_rowids() {
        round_trip(&Cell::new(i64::MIN, vec![Some(Value::Integer(1))]));
        round_trip(&Cell::new(i64::MAX, vec![Some(Value::Integer(1))]));
        round_trip(&Cell::new(-1, vec![Some(Value::Integer(1))]));
    }

    #[test]
    fn bool_edges() {
        round_trip(&Cell::new(1, vec![Some(Value::Bool(true)), Some(Value::Bool(false))]));
    }

    #[test]
    fn real_edges() {
        // f64::NAN != NaN, so we can't round_trip() it; cover the typical edges.
        for v in [0.0f64, 1.0, -1.0, f64::MIN, f64::MAX, f64::INFINITY, f64::NEG_INFINITY] {
            round_trip(&Cell::new(1, vec![Some(Value::Real(v))]));
        }
    }

    #[test]
    fn encoding_null_directly_is_rejected() {
        let bad = Cell::new(1, vec![Some(Value::Null)]);
        let err = bad.encode().unwrap_err();
        assert!(format!("{err}").contains("Null values are encoded"));
    }

    #[test]
    fn decode_rejects_truncated_buffer() {
        let cell = Cell::new(
            1,
            vec![Some(Value::Text("some text here".to_string()))],
        );
        let bytes = cell.encode().unwrap();
        let truncated = &bytes[..bytes.len() - 5];
        assert!(Cell::decode(truncated, 0).is_err());
    }

    #[test]
    fn decode_rejects_unknown_value_tag() {
        // Construct a well-formed local cell whose value block carries a
        // bogus tag byte.
        //   cell_length varint = 5
        //   kind_tag               = 0x01 (local)
        //   rowid varint           = 0
        //   col_count varint       = 1
        //   null bitmap            = 0 (column 0 is not null)
        //   value tag              = 0xFE (bogus)
        let mut buf = Vec::new();
        buf.push(5);            // cell_length
        buf.push(KIND_LOCAL);   // kind_tag
        buf.push(0);            // rowid = 0
        buf.push(1);            // col_count = 1
        buf.push(0);            // null bitmap
        buf.push(0xFE);         // bad value tag
        let err = Cell::decode(&buf, 0).unwrap_err();
        assert!(format!("{err}").contains("unknown value tag"));
    }

    #[test]
    fn decode_rejects_wrong_kind_tag() {
        // Length prefix followed by the overflow kind tag. Cell::decode must
        // refuse — this is what PagedEntry::decode is for.
        let mut buf = Vec::new();
        buf.push(1);            // cell_length = just the kind byte
        buf.push(KIND_OVERFLOW);
        let err = Cell::decode(&buf, 0).unwrap_err();
        assert!(format!("{err}").contains("non-local"));
    }

    #[test]
    fn concatenated_cells_read_sequentially() {
        let c1 = Cell::new(1, vec![Some(Value::Integer(100))]);
        let c2 = Cell::new(2, vec![Some(Value::Text("two".to_string()))]);
        let c3 = Cell::new(3, vec![None]);

        let mut buf = Vec::new();
        buf.extend_from_slice(&c1.encode().unwrap());
        buf.extend_from_slice(&c2.encode().unwrap());
        buf.extend_from_slice(&c3.encode().unwrap());

        let (d1, n1) = Cell::decode(&buf, 0).unwrap();
        let (d2, n2) = Cell::decode(&buf, n1).unwrap();
        let (d3, n3) = Cell::decode(&buf, n1 + n2).unwrap();
        assert_eq!(d1, c1);
        assert_eq!(d2, c2);
        assert_eq!(d3, c3);
        assert_eq!(n1 + n2 + n3, buf.len());
    }

    #[test]
    fn null_bitmap_byte_boundary() {
        // Cell with exactly 8 columns: bitmap is exactly 1 byte.
        let values: Vec<Option<Value>> = (0..8)
            .map(|i| if i % 2 == 0 { Some(Value::Integer(i)) } else { None })
            .collect();
        round_trip(&Cell::new(1, values));

        // 9 columns: bitmap is 2 bytes.
        let values: Vec<Option<Value>> = (0..9)
            .map(|i| if i % 3 == 0 { Some(Value::Integer(i)) } else { None })
            .collect();
        round_trip(&Cell::new(1, values));
    }
}
