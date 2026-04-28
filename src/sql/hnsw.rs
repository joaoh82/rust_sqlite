//! HNSW (Hierarchical Navigable Small World) approximate-nearest-neighbor
//! index. Pure algorithm; no SQL integration in this module.
//!
//! HNSW is the industry-standard ANN algorithm for in-memory vector search:
//! a multi-layer graph where each node lives at some randomly-assigned max
//! layer; higher layers are sparser, layer 0 contains every node. Search
//! starts at the entry point (the node at the current top layer), greedily
//! descends layer-by-layer, then does a beam search at layer 0.
//!
//! ```text
//!     layer 2:   [A] -- [E]                    sparse
//!                 |       |
//!     layer 1:   [A] -- [E] -- [G] -- [J]      mid
//!                 |  /  |  \   |  \   |
//!     layer 0:   [A,B,C,D,E,F,G,H,I,J,...]     dense (every node)
//! ```
//!
//! ## What this module is responsible for
//!
//! - The graph (per-node, per-layer neighbor lists)
//! - Layer assignment for new nodes (geometric distribution)
//! - Insertion: greedy descent + beam search + neighbor pruning
//! - Query: greedy descent + beam search at layer 0, return top-k
//!
//! ## What it is NOT responsible for (yet)
//!
//! - **Storing vectors.** The algorithm calls a `get_vec(node_id) -> &[f32]`
//!   closure to fetch the vector for any node it touches. In Phase 7d.2
//!   that closure will read from the SQL table holding the indexed
//!   column; in tests it reads from an in-memory `Vec<Vec<f32>>`.
//! - **Persistence.** The graph lives in `HashMap<i64, Node>` for now.
//!   Phase 7d.3 wires it into the cell-encoded page format.
//! - **DELETE / UPDATE.** Pre-existing nodes can't be removed today.
//!   Soft-delete + lazy rebuild is the planned approach for 7d.2/7d.3.
//!
//! ## Parameters (per Phase 7 plan Q2 — fixed defaults)
//!
//! - `M = 16`              — max neighbors per node at layers > 0
//! - `m_max0 = 32` (= 2·M) — max neighbors at layer 0
//! - `ef_construction = 200` — beam width during INSERT
//! - `ef_search = 50`      — default beam width during query
//! - `m_l = 1/ln(M) ≈ 0.36`  — layer-assignment scale
//!
//! ## Invariants
//!
//! - Every `node.layers` Vec has length `node_max_layer + 1` for that node.
//! - `node.layers[i]` contains node_ids of neighbors at layer i. Each
//!   neighbor is itself a node in `nodes`; symmetrical (if A → B at layer i
//!   then B → A at layer i, modulo pruning).
//! - `entry_point` is `Some(id)` iff `nodes` is non-empty. The entry node
//!   has the highest max-layer of any node currently in the graph.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Distance metric used by the HNSW index. Must match what the
/// surrounding `vec_distance_*` SQL function would compute on the same
/// pair of vectors — otherwise the index probe and the brute-force
/// fallback would disagree on which rows are "nearest". See
/// `src/sql/executor.rs`'s `vec_distance_l2` / `_cosine` / `_dot` for
/// the canonical implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    L2,
    Cosine,
    Dot,
}

impl DistanceMetric {
    /// Computes the configured distance between two equal-dimension
    /// vectors. Returns `f32::INFINITY` for the cosine/zero-magnitude
    /// edge case; HNSW treats infinity as "worst possible candidate" and
    /// will prefer any finite alternative, which matches the SQL-level
    /// behaviour where `vec_distance_cosine` errors but the optimizer's
    /// fallback path simply skips the offending row.
    pub fn compute(self, a: &[f32], b: &[f32]) -> f32 {
        debug_assert_eq!(a.len(), b.len(), "vector dim mismatch in HNSW distance");
        match self {
            DistanceMetric::L2 => {
                let mut sum = 0.0f32;
                for i in 0..a.len() {
                    let d = a[i] - b[i];
                    sum += d * d;
                }
                sum.sqrt()
            }
            DistanceMetric::Cosine => {
                let mut dot = 0.0f32;
                let mut na = 0.0f32;
                let mut nb = 0.0f32;
                for i in 0..a.len() {
                    dot += a[i] * b[i];
                    na += a[i] * a[i];
                    nb += b[i] * b[i];
                }
                let denom = (na * nb).sqrt();
                if denom == 0.0 {
                    f32::INFINITY
                } else {
                    1.0 - dot / denom
                }
            }
            DistanceMetric::Dot => {
                let mut dot = 0.0f32;
                for i in 0..a.len() {
                    dot += a[i] * b[i];
                }
                -dot
            }
        }
    }
}

/// Per-node metadata: a list of neighbor IDs for each layer this node
/// lives in. `layers[0]` is layer 0 (densest); `layers[layers.len() - 1]`
/// is the highest layer this node reaches.
#[derive(Debug, Clone, Default)]
pub struct Node {
    /// Indexed by layer (0 = dense). `layers[i]` is the neighbor list
    /// for this node at layer i. Always sorted-by-distance is *not* a
    /// guaranteed invariant — pruning maintains it after each
    /// modification, but during insert we may briefly hold an
    /// unsorted set.
    pub layers: Vec<Vec<i64>>,
}

impl Node {
    /// Maximum layer this node reaches. Equals `layers.len() - 1`.
    pub fn max_layer(&self) -> usize {
        self.layers.len() - 1
    }
}

/// HNSW algorithm parameters. Phase 7 ships fixed defaults (Q2 in the
/// plan); this struct is `Clone + Copy` so callers wanting to fork an
/// experimental tuning can do so without touching the index itself.
#[derive(Debug, Clone, Copy)]
pub struct HnswParams {
    pub m: usize,
    pub m_max0: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub m_l: f32,
}

impl Default for HnswParams {
    fn default() -> Self {
        let m = 16;
        Self {
            m,
            m_max0: 2 * m,
            ef_construction: 200,
            ef_search: 50,
            m_l: 1.0 / (m as f32).ln(),
        }
    }
}

/// In-memory HNSW graph. See module docs for the model.
#[derive(Debug, Clone)]
pub struct HnswIndex {
    pub params: HnswParams,
    pub distance: DistanceMetric,
    /// Node id of the entry point. `None` iff the index is empty.
    /// At all times this is the id of the node with the highest
    /// max-layer; if multiple nodes tie for the top layer, the
    /// most-recently-promoted one wins.
    pub entry_point: Option<i64>,
    /// Highest layer currently populated. 0 when the index has at
    /// most one node, grows as new nodes get assigned higher layers.
    pub top_layer: usize,
    /// Node id → its per-layer neighbor lists.
    pub nodes: HashMap<i64, Node>,
    /// xorshift64 RNG state for layer assignment. Seeded explicitly via
    /// `new` so tests can pin a known sequence.
    rng_state: u64,
}

impl HnswIndex {
    /// Builds an empty HNSW index with default parameters and the given
    /// distance metric + RNG seed. A seed of 0 is mapped to a small
    /// nonzero constant — xorshift gets stuck at zero.
    pub fn new(distance: DistanceMetric, seed: u64) -> Self {
        let seed = if seed == 0 { 0x9E3779B97F4A7C15 } else { seed };
        Self {
            params: HnswParams::default(),
            distance,
            entry_point: None,
            top_layer: 0,
            nodes: HashMap::new(),
            rng_state: seed,
        }
    }

    /// True if no nodes have been inserted yet.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Number of nodes currently in the index.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Phase 7d.3 — produces (node_id, layers) pairs in ascending node_id
    /// order, suitable for serializing the graph to disk via the
    /// `HnswNodeCell` wire format. The graph's metadata
    /// (entry_point + top_layer) is recoverable from the nodes alone:
    /// top_layer = max(max_layer); entry_point = any node at top_layer.
    /// So we don't ship a separate metadata cell.
    pub fn serialize_nodes(&self) -> Vec<(i64, Vec<Vec<i64>>)> {
        let mut out: Vec<(i64, Vec<Vec<i64>>)> = self
            .nodes
            .iter()
            .map(|(id, n)| (*id, n.layers.clone()))
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// Phase 7d.3 — rebuilds an HnswIndex from a stream of (node_id, layers)
    /// pairs as produced by `serialize_nodes` and round-tripped through
    /// `HnswNodeCell` encode/decode. The rebuilt index has the same nodes,
    /// same neighbor lists, same entry_point + top_layer as the source.
    /// `seed` is fresh; the deserialized index is never inserted into via
    /// the algorithmic `insert` path so the seed only matters if a caller
    /// later calls `insert` after deserializing (then it controls layer
    /// assignment for the appended node).
    pub fn from_persisted_nodes<I>(distance: DistanceMetric, seed: u64, nodes: I) -> Self
    where
        I: IntoIterator<Item = (i64, Vec<Vec<i64>>)>,
    {
        let mut idx = Self::new(distance, seed);
        let mut top_layer = 0usize;
        let mut entry_point: Option<i64> = None;
        for (id, layers) in nodes {
            let max_layer = layers.len().saturating_sub(1);
            if max_layer > top_layer || entry_point.is_none() {
                top_layer = max_layer;
                entry_point = Some(id);
            }
            idx.nodes.insert(id, Node { layers });
        }
        idx.top_layer = top_layer;
        idx.entry_point = entry_point;
        idx
    }

    /// Inserts a node into the graph. The node id must be unique;
    /// re-inserting an existing id is a no-op (returns without error).
    /// `vec` is the new node's vector; `get_vec` looks up the vector
    /// for any other node id the algorithm touches.
    pub fn insert<F>(&mut self, node_id: i64, vec: &[f32], get_vec: F)
    where
        F: Fn(i64) -> Vec<f32>,
    {
        if self.nodes.contains_key(&node_id) {
            return;
        }

        // First node: trivial case. Becomes entry point at layer 0.
        if self.is_empty() {
            self.nodes.insert(
                node_id,
                Node {
                    layers: vec![Vec::new()],
                },
            );
            self.entry_point = Some(node_id);
            self.top_layer = 0;
            return;
        }

        // Pick a layer for this new node.
        let target_layer = self.pick_layer();

        // Pre-allocate the new node's layer lists (empty for now;
        // populated below).
        let new_node = Node {
            layers: vec![Vec::new(); target_layer + 1],
        };
        self.nodes.insert(node_id, new_node);

        // Greedy descent from top down to (target_layer + 1) — at each
        // layer above our target, advance the entry point to the
        // single closest node. We don't add edges at these layers
        // because the new node doesn't live there.
        let mut entry = self.entry_point.expect("non-empty index has entry point");
        for layer in (target_layer + 1..=self.top_layer).rev() {
            let nearest = self.search_layer(vec, &[entry], 1, layer, &get_vec);
            if let Some((_, id)) = nearest.into_iter().next() {
                entry = id;
            }
        }

        // Beam search + connect at each layer the new node lives in.
        // We work top-down; the entry point for each layer is the best
        // candidate found at the layer above.
        let mut entries = vec![entry];
        for layer in (0..=target_layer).rev() {
            let candidates =
                self.search_layer(vec, &entries, self.params.ef_construction, layer, &get_vec);

            // Pick up to M neighbors from candidates (M_max0 at layer 0
            // since we allow more connections at the dense layer).
            let m_max = if layer == 0 {
                self.params.m_max0
            } else {
                self.params.m
            };
            let neighbors: Vec<i64> = candidates
                .iter()
                .take(self.params.m)
                .map(|(_, id)| *id)
                .collect();

            // Wire up the bidirectional edges.
            self.nodes.get_mut(&node_id).expect("just inserted").layers[layer] = neighbors.clone();

            for &nb in &neighbors {
                let nb_layers = &mut self.nodes.get_mut(&nb).expect("neighbor must exist").layers;
                if layer >= nb_layers.len() {
                    // Neighbor doesn't actually live at this layer — shouldn't
                    // happen because search_layer only returns nodes at this
                    // layer, but defend against it.
                    continue;
                }
                nb_layers[layer].push(node_id);

                // Prune the neighbor's edge list if it's now over its M_max
                // budget. Pruning policy: keep the closest M_max nodes
                // by distance. (Distance recomputed; no precomputed values.)
                if nb_layers[layer].len() > m_max {
                    let nb_vec = get_vec(nb);
                    let mut by_dist: Vec<(f32, i64)> = nb_layers[layer]
                        .iter()
                        .map(|id| (self.distance.compute(&nb_vec, &get_vec(*id)), *id))
                        .collect();
                    by_dist
                        .sort_by(|(da, _), (db, _)| da.partial_cmp(db).unwrap_or(Ordering::Equal));
                    by_dist.truncate(m_max);
                    nb_layers[layer] = by_dist.into_iter().map(|(_, id)| id).collect();
                }
            }

            // Carry the candidate set forward as entry points for the
            // next (lower) layer.
            entries = candidates.into_iter().map(|(_, id)| id).collect();
        }

        // If this new node lives higher than the current top, promote it.
        if target_layer > self.top_layer {
            self.top_layer = target_layer;
            self.entry_point = Some(node_id);
        }
    }

    /// Returns the k nearest node ids to `query`, in distance-ascending
    /// order (closest first). Empty index returns an empty Vec.
    pub fn search<F>(&self, query: &[f32], k: usize, get_vec: F) -> Vec<i64>
    where
        F: Fn(i64) -> Vec<f32>,
    {
        if self.is_empty() || k == 0 {
            return Vec::new();
        }

        // Greedy descent from the top down to layer 1.
        let mut entry = self.entry_point.expect("non-empty index has entry point");
        for layer in (1..=self.top_layer).rev() {
            let nearest = self.search_layer(query, &[entry], 1, layer, &get_vec);
            if let Some((_, id)) = nearest.into_iter().next() {
                entry = id;
            }
        }

        // Beam search at layer 0 with width = max(ef_search, k).
        let ef = self.params.ef_search.max(k);
        let candidates = self.search_layer(query, &[entry], ef, 0, &get_vec);

        candidates.into_iter().take(k).map(|(_, id)| id).collect()
    }

    /// Runs a beam search at one layer starting from `entries`, returning
    /// the top-`ef` nearest nodes to `query` found, sorted by distance
    /// ascending.
    ///
    /// This is the workhorse of both insert and search. The two priority
    /// queues — "candidates" (nodes still to expand) and "results"
    /// (current best ef found) — terminate when the closest unexpanded
    /// candidate is farther than the worst kept result.
    fn search_layer<F>(
        &self,
        query: &[f32],
        entries: &[i64],
        ef: usize,
        layer: usize,
        get_vec: &F,
    ) -> Vec<(f32, i64)>
    where
        F: Fn(i64) -> Vec<f32>,
    {
        let mut visited: HashSet<i64> = HashSet::with_capacity(ef * 2);
        // candidates: min-heap of (distance, id) — pop closest first.
        let mut candidates: BinaryHeap<MinHeapItem> = BinaryHeap::with_capacity(ef * 2);
        // results: max-heap of (distance, id) — top is the worst kept.
        let mut results: BinaryHeap<MaxHeapItem> = BinaryHeap::with_capacity(ef);

        for &id in entries {
            if !visited.insert(id) {
                continue;
            }
            let d = self.distance.compute(query, &get_vec(id));
            candidates.push(MinHeapItem { dist: d, id });
            results.push(MaxHeapItem { dist: d, id });
        }

        while let Some(MinHeapItem {
            dist: c_dist,
            id: c_id,
        }) = candidates.pop()
        {
            // If the closest unexpanded candidate is worse than the
            // worst kept result, no further expansion can improve the
            // result set. Bail.
            if let Some(worst) = results.peek() {
                if results.len() >= ef && c_dist > worst.dist {
                    break;
                }
            }

            // Expand: visit each neighbor of c_id at this layer.
            let neighbors = self
                .nodes
                .get(&c_id)
                .and_then(|n| n.layers.get(layer))
                .cloned()
                .unwrap_or_default();
            for nb in neighbors {
                if !visited.insert(nb) {
                    continue;
                }
                let d = self.distance.compute(query, &get_vec(nb));
                let admit = if results.len() < ef {
                    true
                } else {
                    d < results.peek().unwrap().dist
                };
                if admit {
                    candidates.push(MinHeapItem { dist: d, id: nb });
                    results.push(MaxHeapItem { dist: d, id: nb });
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        // Drain results into a sorted vec. results is a max-heap, so
        // popping gives descending order; reverse for ascending.
        let mut out: Vec<(f32, i64)> = Vec::with_capacity(results.len());
        while let Some(item) = results.pop() {
            out.push((item.dist, item.id));
        }
        out.reverse();
        out
    }

    /// Picks a layer for a new node using the standard HNSW geometric
    /// distribution: `L = floor(-ln(uniform) * m_l)`. With M=16, mL ≈ 0.36,
    /// so:
    ///   - P(L=0) ≈ 1 - 1/M = 15/16
    ///   - P(L=1) ≈ 1/16 - 1/256
    ///   - P(L=2) ≈ 1/256 - …
    /// i.e., most new nodes live only at layer 0; a few percolate up.
    fn pick_layer(&mut self) -> usize {
        let u = self.next_uniform().max(1e-6); // guard log(0)
        let layer = (-u.ln() * self.params.m_l).floor() as usize;
        // Cap at top_layer + 1 to keep the graph from sprouting empty
        // layers above the current top — matches the original HNSW
        // paper's recommendation.
        layer.min(self.top_layer + 1)
    }

    /// Pulls a uniform-on-(0, 1] f32 from the internal xorshift state.
    /// Top 24 bits of the next u64, divided by 2^24 — gives 24-bit
    /// uniform precision, plenty for layer assignment.
    fn next_uniform(&mut self) -> f32 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        ((x >> 40) as u32) as f32 / (1u32 << 24) as f32
    }
}

// -----------------------------------------------------------------
// Heap items
//
// Rust's BinaryHeap is a max-heap that uses Ord. f32 doesn't impl Ord
// (NaN), so we wrap (distance, id) pairs and provide custom Ord that
// uses partial_cmp with NaN treated as Greater (NaN sorts as worst).
//
// MinHeapItem inverts the comparison so BinaryHeap<MinHeapItem> behaves
// as a min-heap — top is the smallest distance, popping gives ascending
// order.
//
// MaxHeapItem uses the natural ordering — top is the largest distance.

#[derive(Debug, Clone, Copy)]
struct MinHeapItem {
    dist: f32,
    id: i64,
}

impl PartialEq for MinHeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist && self.id == other.id
    }
}
impl Eq for MinHeapItem {}
impl PartialOrd for MinHeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for MinHeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse so smallest distance bubbles to top.
        other
            .dist
            .partial_cmp(&self.dist)
            .unwrap_or(Ordering::Equal)
            .then(other.id.cmp(&self.id))
    }
}

#[derive(Debug, Clone, Copy)]
struct MaxHeapItem {
    dist: f32,
    id: i64,
}

impl PartialEq for MaxHeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist && self.id == other.id
    }
}
impl Eq for MaxHeapItem {}
impl PartialOrd for MaxHeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for MaxHeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // Natural so largest distance bubbles to top.
        self.dist
            .partial_cmp(&other.dist)
            .unwrap_or(Ordering::Equal)
            .then(self.id.cmp(&other.id))
    }
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift to generate test vectors.
    fn random_vec(state: &mut u64, dim: usize) -> Vec<f32> {
        (0..dim)
            .map(|_| {
                let mut x = *state;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *state = x;
                ((x >> 40) as u32) as f32 / (1u32 << 24) as f32
            })
            .collect()
    }

    /// Brute-force nearest-neighbors baseline for recall comparison.
    fn brute_force_topk(
        vectors: &[Vec<f32>],
        query: &[f32],
        k: usize,
        metric: DistanceMetric,
    ) -> Vec<i64> {
        let mut by_dist: Vec<(f32, i64)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (metric.compute(query, v), i as i64))
            .collect();
        by_dist.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        by_dist.into_iter().take(k).map(|(_, id)| id).collect()
    }

    /// recall@k — fraction of the brute-force top-k that the HNSW
    /// search also returned (in any order).
    fn recall_at_k(hnsw_result: &[i64], baseline: &[i64]) -> f32 {
        let baseline_set: HashSet<i64> = baseline.iter().copied().collect();
        let hits = hnsw_result
            .iter()
            .filter(|id| baseline_set.contains(id))
            .count();
        hits as f32 / baseline.len() as f32
    }

    #[test]
    fn empty_index_returns_empty_search() {
        let idx = HnswIndex::new(DistanceMetric::L2, 42);
        let vectors: Vec<Vec<f32>> = vec![];
        let result = idx.search(&[0.0; 4], 5, |id| vectors[id as usize].clone());
        assert!(result.is_empty());
    }

    #[test]
    fn single_node_returns_only_itself() {
        let mut idx = HnswIndex::new(DistanceMetric::L2, 42);
        let v0 = vec![1.0, 2.0, 3.0];
        let vectors = vec![v0.clone()];
        idx.insert(0, &v0, |id| vectors[id as usize].clone());
        let result = idx.search(&[0.0; 3], 5, |id| vectors[id as usize].clone());
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn duplicate_insert_is_noop() {
        let mut idx = HnswIndex::new(DistanceMetric::L2, 42);
        let v0 = vec![1.0, 2.0];
        let vectors = vec![v0.clone()];
        idx.insert(0, &v0, |id| vectors[id as usize].clone());
        idx.insert(0, &v0, |id| vectors[id as usize].clone());
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn k_zero_returns_empty() {
        let mut idx = HnswIndex::new(DistanceMetric::L2, 42);
        let vectors = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        for (i, v) in vectors.iter().enumerate() {
            idx.insert(i as i64, v, |id| vectors[id as usize].clone());
        }
        let result = idx.search(&[0.5, 0.5], 0, |id| vectors[id as usize].clone());
        assert!(result.is_empty());
    }

    #[test]
    fn small_graph_finds_exact_nearest() {
        // 5 well-separated points in 2D — HNSW should find the exact
        // nearest with no recall loss for k=1 and k=3.
        let vectors: Vec<Vec<f32>> = vec![
            vec![0.0, 0.0],
            vec![10.0, 0.0],
            vec![0.0, 10.0],
            vec![10.0, 10.0],
            vec![5.0, 5.0],
        ];
        let mut idx = HnswIndex::new(DistanceMetric::L2, 42);
        for (i, v) in vectors.iter().enumerate() {
            idx.insert(i as i64, v, |id| vectors[id as usize].clone());
        }

        // Query at (1, 1): nearest is (0, 0).
        let result = idx.search(&[1.0, 1.0], 1, |id| vectors[id as usize].clone());
        assert_eq!(result, vec![0]);

        // Query at (5.5, 5.5): top-3 should be id=4 (5,5), then any
        // two of the corners at distance ~7.78.
        let result = idx.search(&[5.5, 5.5], 3, |id| vectors[id as usize].clone());
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], 4, "closest to (5.5,5.5) should be id=4");
    }

    #[test]
    fn recall_at_10_is_high_on_random_vectors_l2() {
        // Standard recall test: 1000 random vectors in 8D, query for
        // top-10 with HNSW, compare to brute-force ground truth.
        // Modern HNSW papers target recall@10 > 0.95; we should clear
        // that comfortably on this small benchmark.
        let mut state: u64 = 0xDEADBEEF;
        let dim = 8;
        let n = 1000;
        let queries = 20;
        let k = 10;

        let vectors: Vec<Vec<f32>> = (0..n).map(|_| random_vec(&mut state, dim)).collect();

        let mut idx = HnswIndex::new(DistanceMetric::L2, 42);
        for (i, v) in vectors.iter().enumerate() {
            idx.insert(i as i64, v, |id| vectors[id as usize].clone());
        }

        let mut total_recall = 0.0f32;
        for _ in 0..queries {
            let q = random_vec(&mut state, dim);
            let hnsw_top = idx.search(&q, k, |id| vectors[id as usize].clone());
            let baseline = brute_force_topk(&vectors, &q, k, DistanceMetric::L2);
            total_recall += recall_at_k(&hnsw_top, &baseline);
        }
        let avg_recall = total_recall / queries as f32;
        assert!(
            avg_recall >= 0.95,
            "recall@{k} dropped below 0.95: avg={avg_recall:.3}"
        );
    }

    #[test]
    fn recall_at_10_is_high_on_random_vectors_cosine() {
        // Same shape as the L2 test but with cosine distance, to
        // exercise the alternative metric through the same pipeline.
        let mut state: u64 = 0xC0FFEE;
        let dim = 16;
        let n = 500;
        let queries = 20;
        let k = 10;

        let vectors: Vec<Vec<f32>> = (0..n).map(|_| random_vec(&mut state, dim)).collect();

        let mut idx = HnswIndex::new(DistanceMetric::Cosine, 42);
        for (i, v) in vectors.iter().enumerate() {
            idx.insert(i as i64, v, |id| vectors[id as usize].clone());
        }

        let mut total_recall = 0.0f32;
        for _ in 0..queries {
            let q = random_vec(&mut state, dim);
            let hnsw_top = idx.search(&q, k, |id| vectors[id as usize].clone());
            let baseline = brute_force_topk(&vectors, &q, k, DistanceMetric::Cosine);
            total_recall += recall_at_k(&hnsw_top, &baseline);
        }
        let avg_recall = total_recall / queries as f32;
        assert!(
            avg_recall >= 0.95,
            "cosine recall@{k} dropped below 0.95: avg={avg_recall:.3}"
        );
    }

    #[test]
    fn entry_point_promotes_when_higher_layer_node_inserted() {
        // The graph's entry point should always be a node at the
        // current top layer. Insert two nodes; if the second lands at
        // a higher layer, it becomes the entry point.
        // We can't easily force a particular layer (it's randomized),
        // so check the invariant: after every insert, the entry node's
        // max_layer == top_layer.
        let mut state: u64 = 0xABCDEF;
        let mut idx = HnswIndex::new(DistanceMetric::L2, 42);
        let dim = 4;
        let mut vectors: Vec<Vec<f32>> = Vec::new();
        for i in 0..50 {
            vectors.push(random_vec(&mut state, dim));
            let v = vectors[i].clone();
            idx.insert(i as i64, &v, |id| vectors[id as usize].clone());

            // Check invariant.
            let entry = idx.entry_point.expect("non-empty");
            let entry_max = idx.nodes[&entry].max_layer();
            assert_eq!(
                entry_max, idx.top_layer,
                "entry-point invariant broken at step {i}: entry {entry} has max_layer {entry_max}, top_layer is {}",
                idx.top_layer
            );
        }
    }

    #[test]
    fn neighbor_lists_respect_m_max() {
        // After inserting 200 points with M=16 (so M_max0 = 32), no
        // node should have more than 32 neighbors at layer 0 or more
        // than 16 at any higher layer.
        let mut state: u64 = 0x123456;
        let mut idx = HnswIndex::new(DistanceMetric::L2, 42);
        let dim = 4;
        let mut vectors: Vec<Vec<f32>> = Vec::new();
        for i in 0..200 {
            vectors.push(random_vec(&mut state, dim));
            let v = vectors[i].clone();
            idx.insert(i as i64, &v, |id| vectors[id as usize].clone());
        }

        for (id, node) in &idx.nodes {
            for (layer, neighbors) in node.layers.iter().enumerate() {
                let cap = if layer == 0 {
                    idx.params.m_max0
                } else {
                    idx.params.m
                };
                assert!(
                    neighbors.len() <= cap,
                    "node {id} layer {layer} has {} > cap {cap}",
                    neighbors.len()
                );
            }
        }
    }

    #[test]
    fn deterministic_with_fixed_seed() {
        // Same seed + same insert order → same graph topology.
        // Catches accidental sources of nondeterminism (HashMap
        // iteration order, etc.).
        let mut state: u64 = 0x999;
        let dim = 4;
        let n = 50;
        let vectors: Vec<Vec<f32>> = (0..n).map(|_| random_vec(&mut state, dim)).collect();

        let mut idx_a = HnswIndex::new(DistanceMetric::L2, 42);
        let mut idx_b = HnswIndex::new(DistanceMetric::L2, 42);
        for (i, v) in vectors.iter().enumerate() {
            idx_a.insert(i as i64, v, |id| vectors[id as usize].clone());
            idx_b.insert(i as i64, v, |id| vectors[id as usize].clone());
        }

        // Same top layer.
        assert_eq!(idx_a.top_layer, idx_b.top_layer);
        // Same entry point.
        assert_eq!(idx_a.entry_point, idx_b.entry_point);
        // Same node count and same per-node max-layer for every id.
        // (Neighbor list contents may differ trivially if HashMap
        // iteration sneaked in; if this fails, fix the source first.)
        assert_eq!(idx_a.nodes.len(), idx_b.nodes.len());
        for (id, node_a) in &idx_a.nodes {
            let node_b = idx_b.nodes.get(id).expect("missing id");
            assert_eq!(node_a.max_layer(), node_b.max_layer(), "id={id}");
        }
    }
}
