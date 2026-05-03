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
pub const KIND_INDEX: u8 = 0x04;
/// Phase 7d.3: a single HNSW node's per-layer neighbor lists,
/// serialized into one cell. Body layout (after the shared
/// `cell_length | kind_tag` prefix):
///
/// ```text
///   node_id       zigzag varint   the rowid this graph node represents
///   max_layer     varint          highest layer this node lives in
///   for each layer 0..=max_layer:
///     count       varint          number of neighbors at this layer
///     for each:   zigzag varint   neighbor node_id
/// ```
///
/// `peek_rowid` works uniformly on this kind because it just reads
/// the first varint after the kind tag — exactly the `node_id` here.
pub const KIND_HNSW: u8 = 0x05;

/// Phase 8c: a single FTS posting-list cell. Body layout (after the
/// shared `cell_length | kind_tag` prefix):
///
/// ```text
///   cell_id    zigzag varint   sequential id assigned at save time;
///                              acts as the B-Tree slot key so
///                              `peek_rowid` works uniformly
///   term_len   varint          length of the term in bytes
///                              (0 → this cell is the doc-lengths
///                              sidecar, value below is doc_len)
///   term       term_len bytes  ASCII-lowercased term (per Phase 8 Q3)
///   count      varint          number of (rowid, value) pairs
///   for each:
///     rowid    zigzag varint   the row this posting refers to
///     value    varint          term frequency for this (term, row),
///                              or doc length when term_len == 0
/// ```
///
/// One sidecar cell with `term_len == 0` holds `(rowid, doc_len)`
/// pairs so reload reproduces every indexed doc — including any with
/// zero-token text — without re-tokenizing. All remaining cells are
/// posting cells, one per term.
pub const KIND_FTS_POSTING: u8 = 0x06;

/// Value type tag stored in each non-NULL value block.
pub mod tag {
    pub const INTEGER: u8 = 0;
    pub const REAL: u8 = 1;
    pub const TEXT: u8 = 2;
    pub const BOOL: u8 = 3;
    /// Phase 7a — dense f32 vector. Layout after the tag byte:
    /// `dim (varint) | dim × 4 bytes f32 little-endian`.
    /// dim is self-describing (varint) so `decode_value` can read the
    /// payload without consulting schema metadata.
    pub const VECTOR: u8 = 4;
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
        for v in self.values.iter().flatten() {
            encode_value(&mut body, v)?;
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
        buf.get(kind_pos).copied().ok_or_else(|| {
            SQLRiteError::Internal("paged cell truncated before kind tag".to_string())
        })
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

pub(super) fn encode_value(out: &mut Vec<u8>, value: &Value) -> Result<()> {
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
        Value::Vector(v) => {
            out.push(tag::VECTOR);
            // dim as varint so the decoder doesn't need schema context.
            varint::write_u64(out, v.len() as u64);
            // Each f32 as 4 little-endian bytes; total payload = 4·dim.
            for x in v {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
        Value::Null => {
            return Err(SQLRiteError::Internal(
                "Null values are encoded via the null bitmap, not a value block".to_string(),
            ));
        }
    }
    Ok(())
}

pub(super) fn decode_value(buf: &[u8], pos: usize) -> Result<(Value, usize)> {
    let tag = *buf
        .get(pos)
        .ok_or_else(|| SQLRiteError::Internal(format!("value block truncated at offset {pos}")))?;
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
                return Err(SQLRiteError::Internal("Text value truncated".to_string()));
            }
            let s = std::str::from_utf8(&buf[text_start..text_end])
                .map_err(|e| SQLRiteError::Internal(format!("Text value is not valid UTF-8: {e}")))?
                .to_string();
            Ok((Value::Text(s), 1 + n + (len as usize)))
        }
        tag::BOOL => {
            let byte = *buf
                .get(body_start)
                .ok_or_else(|| SQLRiteError::Internal("Bool value truncated".to_string()))?;
            Ok((Value::Bool(byte != 0), 1 + 1))
        }
        tag::VECTOR => {
            // Layout: tag (1 byte, already consumed) | dim (varint)
            //       | dim × 4 bytes f32 LE.
            let (dim, n) = varint::read_u64(buf, body_start)?;
            let dim = dim as usize;
            let elements_start = body_start + n;
            let elements_end = elements_start + dim * 4;
            if elements_end > buf.len() {
                return Err(SQLRiteError::Internal(format!(
                    "Vector value truncated: needs {dim} × 4 = {} bytes",
                    dim * 4
                )));
            }
            let mut out = Vec::with_capacity(dim);
            for i in 0..dim {
                let off = elements_start + i * 4;
                let arr: [u8; 4] = buf[off..off + 4].try_into().unwrap();
                out.push(f32::from_le_bytes(arr));
            }
            Ok((Value::Vector(out), 1 + n + dim * 4))
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
                // Any non-PI real number works for the round-trip
                // assertion; clippy's `approx_constant` lint rejects
                // 3.14 because it thinks we meant `f64::consts::PI`.
                Some(Value::Real(2.5)),
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
        round_trip(&Cell::new(
            9,
            vec![None, None, None, None, None, None, None, None, None],
        ));
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
        round_trip(&Cell::new(
            1,
            vec![Some(Value::Bool(true)), Some(Value::Bool(false))],
        ));
    }

    #[test]
    fn real_edges() {
        // f64::NAN != NaN, so we can't round_trip() it; cover the typical edges.
        for v in [
            0.0f64,
            1.0,
            -1.0,
            f64::MIN,
            f64::MAX,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            round_trip(&Cell::new(1, vec![Some(Value::Real(v))]));
        }
    }

    // -----------------------------------------------------------------
    // Phase 7a — VECTOR(N) cell encoding round-trips
    // -----------------------------------------------------------------

    #[test]
    fn vector_round_trip_small() {
        // 3-dim vector — the canonical "first test that exercises the
        // wire format" shape. Covers the tag::VECTOR dispatch + varint
        // dim + dim×4 little-endian f32 layout.
        let v = vec![0.1f32, 0.2, 0.3];
        round_trip(&Cell::new(1, vec![Some(Value::Vector(v))]));
    }

    #[test]
    fn vector_round_trip_high_dim() {
        // 384 elements — OpenAI's text-embedding-3-small dimension. Bigger
        // than a single varint encoding step, exercises a realistic shape.
        let v: Vec<f32> = (0..384).map(|i| i as f32 * 0.01).collect();
        round_trip(&Cell::new(7, vec![Some(Value::Vector(v))]));
    }

    #[test]
    fn vector_round_trip_edge_values() {
        // Cover f32 edges — Inf/NaN are surprising values to find in
        // user data but the encoder shouldn't choke.
        let v = vec![
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            f32::MIN,
            f32::MAX,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        // NaN isn't equal to itself so we can't use round_trip(); inline
        // the encode→decode and assert bit patterns instead.
        let cell = Cell::new(2, vec![Some(Value::Vector(v.clone()))]);
        let bytes = cell.encode().expect("encode");
        let (decoded, _) = Cell::decode(&bytes, 0).expect("decode");
        match &decoded.values[0] {
            Some(Value::Vector(out)) => {
                assert_eq!(out.len(), v.len());
                for (i, (a, b)) in out.iter().zip(v.iter()).enumerate() {
                    assert_eq!(
                        a.to_bits(),
                        b.to_bits(),
                        "element {i} bits mismatch: out {a:?}, expected {b:?}"
                    );
                }
            }
            other => panic!("decoded into wrong variant: {other:?}"),
        }
    }

    #[test]
    fn vector_round_trip_mixed_with_other_columns() {
        // A row with INTEGER + TEXT + VECTOR columns — exercises the
        // null-bitmap + sequential value-block decode path with a
        // VECTOR cell in the middle.
        let cell = Cell::new(
            42,
            vec![
                Some(Value::Integer(7)),
                Some(Value::Text("alpha".to_string())),
                Some(Value::Vector(vec![1.0, 2.0, 3.0, 4.0])),
                Some(Value::Bool(true)),
            ],
        );
        round_trip(&cell);
    }

    #[test]
    fn vector_decode_truncated_buffer_errors() {
        // Build a real vector cell, then chop the last few bytes so the
        // f32 array runs past the buffer end.
        let cell = Cell::new(1, vec![Some(Value::Vector(vec![1.0, 2.0, 3.0]))]);
        let bytes = cell.encode().expect("encode");
        for chop in 1..=4 {
            let truncated = &bytes[..bytes.len() - chop];
            assert!(
                Cell::decode(truncated, 0).is_err(),
                "expected error decoding {} bytes short of full {}",
                chop,
                bytes.len()
            );
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
        let cell = Cell::new(1, vec![Some(Value::Text("some text here".to_string()))]);
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
        buf.push(5); // cell_length
        buf.push(KIND_LOCAL); // kind_tag
        buf.push(0); // rowid = 0
        buf.push(1); // col_count = 1
        buf.push(0); // null bitmap
        buf.push(0xFE); // bad value tag
        let err = Cell::decode(&buf, 0).unwrap_err();
        assert!(format!("{err}").contains("unknown value tag"));
    }

    #[test]
    fn decode_rejects_wrong_kind_tag() {
        // Length prefix followed by the overflow kind tag. Cell::decode must
        // refuse — this is what PagedEntry::decode is for.
        let mut buf = Vec::new();
        buf.push(1); // cell_length = just the kind byte
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
            .map(|i| {
                if i % 2 == 0 {
                    Some(Value::Integer(i))
                } else {
                    None
                }
            })
            .collect();
        round_trip(&Cell::new(1, values));

        // 9 columns: bitmap is 2 bytes.
        let values: Vec<Option<Value>> = (0..9)
            .map(|i| {
                if i % 3 == 0 {
                    Some(Value::Integer(i))
                } else {
                    None
                }
            })
            .collect();
        round_trip(&Cell::new(1, values));
    }
}
