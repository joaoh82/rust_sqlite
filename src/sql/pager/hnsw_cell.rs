//! On-disk format for a single HNSW graph node (Phase 7d.3).
//!
//! Each cell carries one node's per-layer neighbor lists. The cells live
//! on `TableLeaf`-style pages identical to a regular table's data tree —
//! same slot directory, same sibling `next_page` chain, same interior-
//! page mechanics from Phase 3d. The only thing different is the per-cell
//! body, signaled by `KIND_HNSW`.
//!
//! Reusing the table-tree shape lets `Cell::peek_rowid` work uniformly
//! across all cell kinds: it skips `cell_length | kind_tag` and reads the
//! first varint, which is `node_id` here. So slot-directory binary
//! search by node_id works without HNSW-specific code in the page-level
//! plumbing.
//!
//! ```text
//!   cell_length   varint          bytes after this field
//!   kind_tag      u8 = 0x05       (KIND_HNSW)
//!   node_id       zigzag varint   the rowid this graph node represents
//!   max_layer     varint          highest layer this node lives in
//!   for layer in 0..=max_layer:
//!     count       varint          number of neighbors at this layer
//!     for each neighbor:
//!       neighbor  zigzag varint   neighbor's node_id
//! ```
//!
//! No null bitmap — every field is always present. No type tag — every
//! field has a fixed type (varint or zigzag varint). The encoding is
//! deliberately minimal because HNSW indexes can have N nodes each with
//! up to ~M·log(N) total neighbors, and we don't want the per-cell
//! overhead to dominate disk usage.

use crate::error::{Result, SQLRiteError};
use crate::sql::pager::cell::KIND_HNSW;
use crate::sql::pager::varint;

/// One HNSW node's persisted form. `layers[i]` is the list of neighbor
/// node_ids at layer i; the node lives at every layer 0..=layers.len()-1.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswNodeCell {
    pub node_id: i64,
    /// `layers[0]` is the densest layer (always present); `layers.len()`
    /// equals the node's max_layer + 1.
    pub layers: Vec<Vec<i64>>,
}

impl HnswNodeCell {
    pub fn new(node_id: i64, layers: Vec<Vec<i64>>) -> Self {
        Self { node_id, layers }
    }

    /// Encodes the cell into a freshly-allocated `Vec<u8>`. The result
    /// starts with the shared `cell_length | kind_tag` prefix and is
    /// directly usable as a slot-directory entry on a `TableLeaf`-style
    /// page.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.layers.is_empty() {
            return Err(SQLRiteError::Internal(format!(
                "HNSW node {} has zero layers — every node lives at layer 0 minimum",
                self.node_id
            )));
        }

        // Body capacity guess: 1 (kind) + 10 (node_id) + 5 (max_layer)
        // + per-layer overhead. Most nodes are layer-0-only so the
        // typical body is ~1 + 10 + 1 + 1 + M·10 ≈ 175 bytes for M=16.
        let layer_bytes = self.layers.iter().map(|l| 5 + l.len() * 10).sum::<usize>();
        let mut body = Vec::with_capacity(1 + 10 + 5 + layer_bytes);

        body.push(KIND_HNSW);
        varint::write_i64(&mut body, self.node_id);
        // max_layer = layers.len() - 1
        varint::write_u64(&mut body, (self.layers.len() - 1) as u64);
        for layer in &self.layers {
            varint::write_u64(&mut body, layer.len() as u64);
            for n in layer {
                varint::write_i64(&mut body, *n);
            }
        }

        let mut out = Vec::with_capacity(body.len() + varint::MAX_VARINT_BYTES);
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decodes one cell starting at `pos`. Returns the cell plus the
    /// total bytes consumed (including the leading length varint).
    pub fn decode(buf: &[u8], pos: usize) -> Result<(HnswNodeCell, usize)> {
        let (body_len, len_bytes) = varint::read_u64(buf, pos)?;
        let body_start = pos + len_bytes;
        let body_end = body_start
            .checked_add(body_len as usize)
            .ok_or_else(|| SQLRiteError::Internal("HNSW cell length overflow".to_string()))?;
        if body_end > buf.len() {
            return Err(SQLRiteError::Internal(format!(
                "HNSW cell extends past buffer: needs {body_start}..{body_end}, have {}",
                buf.len()
            )));
        }
        let body = &buf[body_start..body_end];
        if body.first().copied() != Some(KIND_HNSW) {
            return Err(SQLRiteError::Internal(format!(
                "HnswNodeCell::decode called on non-HNSW entry (kind_tag = {:#x})",
                body.first().copied().unwrap_or(0)
            )));
        }

        let mut cur = 1usize;
        let (node_id, n) = varint::read_i64(body, cur)?;
        cur += n;
        let (max_layer_u64, n) = varint::read_u64(body, cur)?;
        cur += n;

        let layer_count = (max_layer_u64 as usize)
            .checked_add(1)
            .ok_or_else(|| SQLRiteError::Internal("HNSW max_layer overflow".to_string()))?;
        // Sanity: max_layer is in practice ≤ ~10 for N ≤ 1B with
        // m_l ≈ 0.36. A wildly-large value almost certainly means a
        // corrupt cell — bail before allocating an enormous Vec.
        if layer_count > 64 {
            return Err(SQLRiteError::Internal(format!(
                "HNSW node {node_id} claims max_layer {} (>= 64) — corrupt cell?",
                layer_count - 1
            )));
        }

        let mut layers = Vec::with_capacity(layer_count);
        for _ in 0..layer_count {
            let (count, n) = varint::read_u64(body, cur)?;
            cur += n;
            // Same sanity bound — a single layer's neighbor list shouldn't
            // exceed `2 · M_max0` even after pruning bugs. 256 is a
            // generous cap.
            if count > 256 {
                return Err(SQLRiteError::Internal(format!(
                    "HNSW node {node_id} layer claims {count} neighbors (>256) — corrupt cell?"
                )));
            }
            let mut neighbors = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let (id, n) = varint::read_i64(body, cur)?;
                cur += n;
                neighbors.push(id);
            }
            layers.push(neighbors);
        }

        if cur != body.len() {
            return Err(SQLRiteError::Internal(format!(
                "HNSW cell had {} trailing bytes",
                body.len() - cur
            )));
        }

        Ok((
            HnswNodeCell { node_id, layers },
            len_bytes + body_len as usize,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(cell: &HnswNodeCell) {
        let bytes = cell.encode().expect("encode");
        let (decoded, consumed) = HnswNodeCell::decode(&bytes, 0).expect("decode");
        assert_eq!(
            consumed,
            bytes.len(),
            "decode should consume the whole cell"
        );
        assert_eq!(&decoded, cell);
    }

    #[test]
    fn single_layer_node_round_trips() {
        // Most common case: a layer-0-only node with a handful of neighbors.
        let cell = HnswNodeCell::new(42, vec![vec![1, 2, 3, 5, 8]]);
        round_trip(&cell);
    }

    #[test]
    fn multi_layer_node_round_trips() {
        let cell = HnswNodeCell::new(
            17,
            vec![
                vec![1, 2, 3, 4, 5, 6, 7, 8], // layer 0 (densest)
                vec![1, 3, 7],                // layer 1
                vec![3],                      // layer 2 (sparsest)
            ],
        );
        round_trip(&cell);
    }

    #[test]
    fn empty_neighbor_layer_round_trips() {
        // A node can have an empty layer (e.g. if its only neighbor was
        // pruned away). The encoding must still survive.
        let cell = HnswNodeCell::new(5, vec![vec![1, 2], vec![]]);
        round_trip(&cell);
    }

    #[test]
    fn node_id_negative_and_large() {
        // node_id is zigzag-encoded; cover both signs.
        round_trip(&HnswNodeCell::new(-1, vec![vec![]]));
        round_trip(&HnswNodeCell::new(i64::MAX, vec![vec![1, 2]]));
        round_trip(&HnswNodeCell::new(i64::MIN, vec![vec![3, 4]]));
    }

    #[test]
    fn zero_layers_is_rejected_at_encode() {
        let bad = HnswNodeCell::new(1, vec![]);
        let err = bad.encode().unwrap_err();
        assert!(format!("{err}").contains("zero layers"));
    }

    #[test]
    fn decode_rejects_wrong_kind_tag() {
        // Build something that looks like a cell with an arbitrary
        // (non-HNSW) tag byte and confirm decode bails.
        let mut bad = Vec::new();
        varint::write_u64(&mut bad, 1); // body_len
        bad.push(0x01); // KIND_LOCAL, not KIND_HNSW
        let err = HnswNodeCell::decode(&bad, 0).unwrap_err();
        assert!(format!("{err}").contains("non-HNSW entry"));
    }

    #[test]
    fn decode_rejects_truncated_buffer() {
        let cell = HnswNodeCell::new(1, vec![vec![10, 20, 30]]);
        let bytes = cell.encode().expect("encode");
        for chop in 1..=3 {
            let truncated = &bytes[..bytes.len() - chop];
            assert!(
                HnswNodeCell::decode(truncated, 0).is_err(),
                "expected error chopping {chop} byte(s) from end of {} byte cell",
                bytes.len()
            );
        }
    }

    #[test]
    fn decode_rejects_implausible_max_layer() {
        // Hand-craft a cell whose max_layer is 100 (above the 64 sanity bound).
        let mut body = Vec::new();
        body.push(KIND_HNSW);
        varint::write_i64(&mut body, 0); // node_id
        varint::write_u64(&mut body, 100); // max_layer = 100 → 101 layers
        let mut out = Vec::new();
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        let err = HnswNodeCell::decode(&out, 0).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("corrupt"));
    }
}
