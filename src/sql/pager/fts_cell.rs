//! On-disk format for one FTS posting list (Phase 8c).
//!
//! Each cell carries either a posting list for one term — `(term,
//! [(rowid, term_freq), ...])` — or, in a single sidecar cell with
//! `term.is_empty()`, the per-doc length map `(rowid, doc_len)`. Cells
//! live on `TableLeaf` pages identical to regular table data trees, so
//! the slot directory + sibling `next_page` chain + interior-page
//! mechanics from Phase 3d work without FTS-specific page plumbing.
//!
//! Reusing the table-tree shape lets `Cell::peek_rowid` work uniformly
//! across cell kinds: it skips `cell_length | kind_tag` and reads the
//! first varint, which is `cell_id` here. `cell_id` is a sequential
//! integer assigned at save time (1, 2, 3 …), not a row identifier —
//! the B-Tree just needs an ordered key for slot directory binary
//! search; the actual data is keyed by `term`.
//!
//! ```text
//!   cell_length   varint          bytes after this field
//!   kind_tag      u8 = 0x06       (KIND_FTS_POSTING)
//!   cell_id       zigzag varint   sequential B-Tree slot key
//!   term_len      varint          length of `term` in bytes; 0 → sidecar
//!   term          term_len bytes  ASCII-lowercased term per Phase 8 Q3
//!   count         varint          number of (rowid, value) pairs
//!   for each:
//!     rowid       zigzag varint   the row this entry belongs to
//!     value       varint          term frequency, or doc length when
//!                                 term_len == 0 (sidecar cell)
//! ```
//!
//! One sidecar cell suffices for the entire index: it lists every
//! indexed doc with its tokenized length, including the zero-length
//! corner case (a row whose text tokenizes to nothing — still indexed
//! so `len()` and `total_docs` round-trip). Posting cells follow.
//!
//! No null bitmap, no per-field type tag — every field has a fixed
//! type. The encoding is deliberately minimal because long posting
//! lists dominate disk usage on real corpora.

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::cell::KIND_FTS_POSTING;
use crate::sql::pager::varint;

/// One FTS posting list cell — either a per-term postings entry or the
/// single doc-lengths sidecar (when `term.is_empty()`).
#[derive(Debug, Clone, PartialEq)]
pub struct FtsPostingCell {
    /// Sequential id assigned at save time. Acts as the B-Tree slot
    /// directory key; never persisted as part of the index logic.
    pub cell_id: i64,
    /// Lowercased ASCII term. Empty on the doc-lengths sidecar.
    pub term: String,
    /// `(rowid, value)` pairs. `value` is term frequency for posting
    /// cells, doc length for the sidecar.
    pub entries: Vec<(i64, u32)>,
}

impl FtsPostingCell {
    pub fn posting(cell_id: i64, term: String, entries: Vec<(i64, u32)>) -> Self {
        Self {
            cell_id,
            term,
            entries,
        }
    }

    /// Constructs the doc-lengths sidecar cell (term left empty).
    pub fn doc_lengths(cell_id: i64, entries: Vec<(i64, u32)>) -> Self {
        Self {
            cell_id,
            term: String::new(),
            entries,
        }
    }

    /// Encodes the cell into a freshly-allocated `Vec<u8>`. The result
    /// starts with the shared `cell_length | kind_tag` prefix and is
    /// directly usable as a slot-directory entry on a `TableLeaf`-style
    /// page.
    pub fn encode(&self) -> Result<Vec<u8>> {
        // Body capacity guess: 1 (kind) + 10 (cell_id) + 5 (term_len)
        // + term + 5 (count) + per-pair 10 (rowid) + 5 (value).
        let pair_bytes = self.entries.len() * 15;
        let mut body = Vec::with_capacity(1 + 10 + 5 + self.term.len() + 5 + pair_bytes);

        body.push(KIND_FTS_POSTING);
        varint::write_i64(&mut body, self.cell_id);
        varint::write_u64(&mut body, self.term.len() as u64);
        body.extend_from_slice(self.term.as_bytes());
        varint::write_u64(&mut body, self.entries.len() as u64);
        for (rowid, value) in &self.entries {
            varint::write_i64(&mut body, *rowid);
            varint::write_u64(&mut body, *value as u64);
        }

        let mut out = Vec::with_capacity(body.len() + varint::MAX_VARINT_BYTES);
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decodes one cell starting at `pos`. Returns the cell plus the
    /// total bytes consumed (including the leading length varint).
    pub fn decode(buf: &[u8], pos: usize) -> Result<(FtsPostingCell, usize)> {
        let (body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let body_start = pos + len_bytes;
        let body_end = body_start
            .checked_add(body_len as usize)
            .ok_or_else(|| SQLRiteError::Internal("FTS cell length overflow".to_string()))?;
        if body_end > buf.len() {
            return Err(SQLRiteError::Internal(format!(
                "FTS cell extends past buffer: needs {body_start}..{body_end}, have {}",
                buf.len()
            )));
        }
        let body = &buf[body_start..body_end];
        if body.first().copied() != Some(KIND_FTS_POSTING) {
            return Err(SQLRiteError::Internal(format!(
                "FtsPostingCell::decode called on non-FTS entry (kind_tag = {:#x})",
                body.first().copied().unwrap_or(0)
            )));
        }

        let mut cur = 1usize;
        let (cell_id, n) = varint::read_i64(body, cur)?;
        cur += n;

        let (term_len, n) = varint::read_u64(body, cur)?;
        cur += n;
        // Sanity: a single term shouldn't exceed a few KB even with
        // pathological input. The whole cell body sits inside one page
        // (~4 KiB), so a giant term length is almost certainly a
        // corrupt cell — bail before allocating.
        if term_len as usize > body.len().saturating_sub(cur) {
            return Err(SQLRiteError::Internal(format!(
                "FTS cell {cell_id}: term_len {term_len} exceeds remaining body \
                 ({}) — corrupt cell?",
                body.len() - cur
            )));
        }
        let term_bytes = &body[cur..cur + term_len as usize];
        cur += term_len as usize;
        let term = std::str::from_utf8(term_bytes)
            .map_err(|e| {
                SQLRiteError::Internal(format!("FTS cell {cell_id}: term not valid UTF-8: {e}"))
            })?
            .to_string();

        let (count, n) = varint::read_u64(body, cur)?;
        cur += n;
        // Sanity: a single posting list shouldn't exceed corpus size.
        // 8 GiB worth of entries (8 bytes per rowid alone) is firmly in
        // "corrupt cell" territory.
        if count > 1 << 28 {
            return Err(SQLRiteError::Internal(format!(
                "FTS cell {cell_id}: claims {count} entries (>2^28) — corrupt cell?"
            )));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let (rowid, n) = varint::read_i64(body, cur)?;
            cur += n;
            let (value_u64, n) = varint::read_u64(body, cur)?;
            cur += n;
            // Term frequencies and doc lengths fit in u32 (a doc with
            // 4 billion tokens is implausible). Reject with a clean
            // error instead of silently truncating.
            if value_u64 > u32::MAX as u64 {
                return Err(SQLRiteError::Internal(format!(
                    "FTS cell {cell_id}: value {value_u64} exceeds u32::MAX — corrupt cell?"
                )));
            }
            entries.push((rowid, value_u64 as u32));
        }

        if cur != body.len() {
            return Err(SQLRiteError::Internal(format!(
                "FTS cell {cell_id} had {} trailing bytes",
                body.len() - cur
            )));
        }

        Ok((
            FtsPostingCell {
                cell_id,
                term,
                entries,
            },
            len_bytes + body_len as usize,
        ))
    }

    /// True iff this cell is the doc-lengths sidecar.
    pub fn is_doc_lengths(&self) -> bool {
        self.term.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(cell: &FtsPostingCell) {
        let bytes = cell.encode().expect("encode");
        let (decoded, consumed) = FtsPostingCell::decode(&bytes, 0).expect("decode");
        assert_eq!(
            consumed,
            bytes.len(),
            "decode should consume the whole cell"
        );
        assert_eq!(&decoded, cell);
    }

    #[test]
    fn posting_cell_round_trips() {
        let cell = FtsPostingCell::posting(7, "rust".to_string(), vec![(1, 2), (3, 1), (5, 7)]);
        round_trip(&cell);
    }

    #[test]
    fn doc_lengths_sidecar_round_trips() {
        let cell = FtsPostingCell::doc_lengths(1, vec![(1, 12), (2, 20), (3, 0), (4, 7)]);
        assert!(cell.is_doc_lengths());
        round_trip(&cell);
    }

    #[test]
    fn empty_postings_round_trips() {
        // Edge case: an FTS cell with zero entries shouldn't be
        // emitted in practice (the term would be pruned by remove()),
        // but the format must still round-trip.
        let cell = FtsPostingCell::posting(2, "ghost".to_string(), vec![]);
        round_trip(&cell);
    }

    #[test]
    fn negative_and_large_rowids_round_trip() {
        // Rowids are zigzag-encoded; cover both signs.
        round_trip(&FtsPostingCell::posting(
            3,
            "x".to_string(),
            vec![(-1, 1), (i64::MAX, 99), (i64::MIN, 1)],
        ));
    }

    #[test]
    fn long_term_round_trips() {
        // A 1024-byte term — well within page capacity. Tokenizer
        // wouldn't actually emit this in practice, but encode/decode
        // must still survive.
        let term = "a".repeat(1024);
        let cell = FtsPostingCell::posting(4, term, vec![(1, 1)]);
        round_trip(&cell);
    }

    #[test]
    fn long_posting_list_round_trips() {
        // 5000 entries — exercises the count + pair-loop paths.
        let entries: Vec<(i64, u32)> = (0..5000_i64).map(|i| (i, ((i * 3) as u32) + 1)).collect();
        let cell = FtsPostingCell::posting(5, "common".to_string(), entries);
        round_trip(&cell);
    }

    #[test]
    fn decode_rejects_wrong_kind_tag() {
        let mut bad = Vec::new();
        varint::write_u64(&mut bad, 1); // body_len
        bad.push(0x01); // KIND_LOCAL, not KIND_FTS_POSTING
        let err = FtsPostingCell::decode(&bad, 0).unwrap_err();
        assert!(format!("{err}").contains("non-FTS entry"));
    }

    #[test]
    fn decode_rejects_truncated_buffer() {
        let cell = FtsPostingCell::posting(1, "rust".to_string(), vec![(1, 2), (5, 3)]);
        let bytes = cell.encode().expect("encode");
        for chop in 1..=3 {
            let truncated = &bytes[..bytes.len() - chop];
            assert!(
                FtsPostingCell::decode(truncated, 0).is_err(),
                "expected error chopping {chop} byte(s) from end of {} byte cell",
                bytes.len()
            );
        }
    }

    #[test]
    fn decode_rejects_invalid_utf8_term() {
        // Hand-craft a cell whose term bytes aren't valid UTF-8.
        let mut body = Vec::new();
        body.push(KIND_FTS_POSTING);
        varint::write_i64(&mut body, 1); // cell_id
        varint::write_u64(&mut body, 2); // term_len
        body.extend_from_slice(&[0xFF, 0xFE]); // not valid UTF-8
        varint::write_u64(&mut body, 0); // count = 0
        let mut out = Vec::new();
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        let err = FtsPostingCell::decode(&out, 0).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("utf-8"));
    }

    #[test]
    fn decode_rejects_implausible_count() {
        // Hand-craft a cell with count = 2^29 (above the corruption sanity bound).
        let mut body = Vec::new();
        body.push(KIND_FTS_POSTING);
        varint::write_i64(&mut body, 1);
        varint::write_u64(&mut body, 4);
        body.extend_from_slice(b"term");
        varint::write_u64(&mut body, 1u64 << 29);
        let mut out = Vec::new();
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        let err = FtsPostingCell::decode(&out, 0).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("corrupt"));
    }
}
