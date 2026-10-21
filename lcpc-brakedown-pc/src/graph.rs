// Copyright 2026 RealBrakedownPerf Authors
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

/*!
right-to-left (transposed) storage of a bipartite expander graph

Since `step/04-r2l`, every expander graph of the code is stored as an
[`R2lGraph`]: for each right (output) node `r`, the contiguous list of its
incoming edges `(l, w)` (left node index and weight). This is the compressed
sparse row (CSR) form of the `n_right x n_left` matrix that upstream LCPC
stored as a compressed sparse column (CSC) `sprs::CsMat`.

Graphs are always *sampled* left to right, in the RNG order of the code
specification (one left node at a time, see [`crate::matgen`]), and then
transposed once by [`R2lGraph::from_left_edges`], a stable two-pass counting
sort that runs in `O(n_left + n_right + E)` time. Because the sort is stable,
the incoming edges of each right node appear in increasing left-node order (and,
for parallel edges, in sampling order). That is the order of SIMD-Spielman's
`O(R * E)` transpose in `ExpanderGraph.h`; with eager field arithmetic the
order does not affect the result anyway.

Evaluation (Algorithm 3 of SIMD-Spielman, right-to-left "gather") then writes
each output column exactly once, after reading its incoming left columns, and
never writes an input column; see [`crate::encode::encode_column_block`].
*/

/// A bipartite expander graph stored right to left (CSR over right nodes).
///
/// Right node `r` has incoming edges `row_ptr[r]..row_ptr[r + 1]`; edge `e`
/// connects left node `col_idx[e]` to `r` with weight `weights[e]`. Node
/// indices and edge offsets are `u32`, which halves the index traffic compared
/// with `usize` and is ample for every supported size (construction panics if a
/// graph does not fit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct R2lGraph<F> {
    n_left: usize,
    n_right: usize,
    row_ptr: Vec<u32>,
    col_idx: Vec<u32>,
    weights: Vec<F>,
}

impl<F: Copy> R2lGraph<F> {
    /// Transpose a graph given left to right (CSC over left nodes) into R2L form.
    ///
    /// Left node `l` has outgoing edges `left_ptr[l]..left_ptr[l + 1]`; edge `e`
    /// goes to right node `right_idx[e]` with weight `weights[e]`. Parallel edges
    /// (the same `(l, r)` pair more than once) are kept as separate edges.
    ///
    /// # Panics
    ///
    /// Panics if the arrays are inconsistent (`left_ptr` not of length
    /// `n_left + 1`, not non-decreasing from 0 to `right_idx.len()`, or
    /// `weights.len() != right_idx.len()`), if a right index is out of range,
    /// or if `n_left`, `n_right`, or the edge count exceed `u32::MAX`.
    pub fn from_left_edges(
        n_left: usize,
        n_right: usize,
        left_ptr: &[usize],
        right_idx: &[usize],
        weights: &[F],
    ) -> Self {
        assert_eq!(
            left_ptr.len(),
            n_left + 1,
            "left_ptr must have n_left + 1 entries"
        );
        assert_eq!(left_ptr[0], 0, "left_ptr must start at 0");
        assert_eq!(*left_ptr.last().unwrap(), right_idx.len());
        assert_eq!(weights.len(), right_idx.len());
        Self::transpose(n_left, n_right, right_idx.len(), weights, |l| {
            let (start, end) = (left_ptr[l], left_ptr[l + 1]);
            assert!(start <= end, "left_ptr must be non-decreasing");
            (start..end).map(|e| (right_idx[e], e))
        })
    }

    /// Transpose a graph in which every left node has exactly `degree` outgoing
    /// edges, given left to right: edge `l * degree + d` goes to right node
    /// `right_idx[l * degree + d]` with weight `weights[l * degree + d]`.
    ///
    /// This is the layout of SIMD-Spielman's `l2r_node_connections` and
    /// `l2r_node_weights` (`ExpanderGraph.h`), so graphs sampled by an external
    /// sampler in that layout can be fed to the encoder unchanged.
    ///
    /// # Panics
    ///
    /// Panics if `right_idx.len() != n_left * degree`, if `weights.len()` differs
    /// from it, if a right index is out of range, or if a size exceeds `u32::MAX`.
    pub fn from_left_edges_fixed_degree<I>(
        n_left: usize,
        n_right: usize,
        degree: usize,
        right_idx: &[I],
        weights: &[F],
    ) -> Self
    where
        I: Copy + Into<u64>,
    {
        let n_edges = n_left
            .checked_mul(degree)
            .expect("edge count overflows usize");
        assert_eq!(
            right_idx.len(),
            n_edges,
            "need n_left * degree right indices"
        );
        assert_eq!(weights.len(), n_edges, "need n_left * degree weights");
        Self::transpose(n_left, n_right, n_edges, weights, |l| {
            (l * degree..(l + 1) * degree).map(|e| {
                let r = usize::try_from(right_idx[e].into()).expect("right index overflows usize");
                (r, e)
            })
        })
    }

    // Stable counting-sort transpose. `edges_of(l)` yields `(r, e)` for every
    // outgoing edge of left node `l` in sampling order, where `e` indexes `weights`.
    fn transpose<It>(
        n_left: usize,
        n_right: usize,
        n_edges: usize,
        weights: &[F],
        edges_of: impl Fn(usize) -> It,
    ) -> Self
    where
        It: Iterator<Item = (usize, usize)>,
    {
        let fits = |x: usize| u32::try_from(x).is_ok();
        assert!(
            fits(n_left) && fits(n_right) && fits(n_edges),
            "graph too large for u32 indices"
        );

        // pass 1: in-degree of each right node, stored at row_ptr[r + 1]
        let mut row_ptr = vec![0u32; n_right + 1];
        for l in 0..n_left {
            for (r, _) in edges_of(l) {
                assert!(
                    r < n_right,
                    "right index {r} out of range (n_right = {n_right})"
                );
                row_ptr[r + 1] += 1;
            }
        }

        // prefix sum: row_ptr[r] becomes the first edge slot of right node r
        for r in 0..n_right {
            row_ptr[r + 1] += row_ptr[r];
        }
        assert_eq!(row_ptr[n_right] as usize, n_edges);

        // pass 2: stable scatter in left-node (sampling) order; every slot of
        // `new_weights` is overwritten exactly once (the copy only initializes it)
        let mut cursor = row_ptr[..n_right].to_vec();
        let mut col_idx = vec![0u32; n_edges];
        let mut new_weights = weights.to_vec();
        for l in 0..n_left {
            for (r, e) in edges_of(l) {
                let pos = cursor[r] as usize;
                cursor[r] += 1;
                col_idx[pos] = l as u32;
                new_weights[pos] = weights[e];
            }
        }

        Self {
            n_left,
            n_right,
            row_ptr,
            col_idx,
            weights: new_weights,
        }
    }
}

impl<F> R2lGraph<F> {
    /// Number of left (input) nodes.
    #[inline]
    pub fn n_left(&self) -> usize {
        self.n_left
    }

    /// Number of right (output) nodes.
    #[inline]
    pub fn n_right(&self) -> usize {
        self.n_right
    }

    /// Total number of edges (parallel edges counted separately).
    #[inline]
    pub fn n_edges(&self) -> usize {
        self.col_idx.len()
    }

    /// Edge offsets per right node (length `n_right + 1`).
    #[inline]
    pub fn row_ptr(&self) -> &[u32] {
        &self.row_ptr
    }

    /// Left-node index of each edge, grouped by right node.
    #[inline]
    pub fn col_idx(&self) -> &[u32] {
        &self.col_idx
    }

    /// Weight of each edge, grouped by right node.
    #[inline]
    pub fn weights(&self) -> &[F] {
        &self.weights
    }

    /// The incoming edges of right node `r`: left-node indices and weights.
    #[inline]
    pub fn incoming(&self, r: usize) -> (&[u32], &[F]) {
        let start = self.row_ptr[r] as usize;
        let end = self.row_ptr[r + 1] as usize;
        (&self.col_idx[start..end], &self.weights[start..end])
    }
}

#[cfg(test)]
mod tests {
    use super::R2lGraph;

    // Naive O(R * E) transpose, as in SIMD-Spielman's ExpanderGraph.h.
    fn naive(n_right: usize, left_of: &[usize], right_of: &[usize], w: &[u64]) -> R2lGraph<u64> {
        let mut row_ptr = vec![0u32];
        let mut col_idx = Vec::new();
        let mut weights = Vec::new();
        for r in 0..n_right {
            for e in 0..right_of.len() {
                if right_of[e] == r {
                    col_idx.push(left_of[e] as u32);
                    weights.push(w[e]);
                }
            }
            row_ptr.push(col_idx.len() as u32);
        }
        R2lGraph {
            n_left: left_of.iter().max().map_or(0, |m| m + 1),
            n_right,
            row_ptr,
            col_idx,
            weights,
        }
    }

    #[test]
    fn counting_sort_matches_naive_transpose() {
        // fixed degree 3, with parallel edges and an isolated right node (5)
        let n_left = 4;
        let n_right = 6;
        let right_idx: Vec<u32> = vec![2, 0, 2, 1, 4, 3, 0, 0, 1, 4, 2, 3];
        let weights: Vec<u64> = (100..112).collect();
        let g = R2lGraph::from_left_edges_fixed_degree(n_left, n_right, 3, &right_idx, &weights);
        let left_of: Vec<usize> = (0..12).map(|e| e / 3).collect();
        let right_of: Vec<usize> = right_idx.iter().map(|&r| r as usize).collect();
        assert_eq!(g, naive(n_right, &left_of, &right_of, &weights));
        assert_eq!(g.incoming(5), (&[][..], &[][..]));
        assert_eq!(g.incoming(0), (&[0u32, 2, 2][..], &[101u64, 106, 107][..]));

        // the same graph given as CSC must transpose identically
        let left_ptr: Vec<usize> = (0..=n_left).map(|l| 3 * l).collect();
        let g2 = R2lGraph::from_left_edges(n_left, n_right, &left_ptr, &right_of, &weights);
        assert_eq!(g, g2);
    }

    #[test]
    fn variable_degree_csc() {
        // left node 1 has no edges
        let left_ptr = [0usize, 2, 2, 5];
        let right_idx = [1usize, 0, 0, 1, 2];
        let weights = [1u64, 2, 3, 4, 5];
        let g = R2lGraph::from_left_edges(3, 3, &left_ptr, &right_idx, &weights);
        assert_eq!(g.row_ptr(), &[0, 2, 4, 5]);
        assert_eq!(g.col_idx(), &[0, 2, 0, 2, 2]);
        assert_eq!(g.weights(), &[2, 3, 1, 4, 5]);
        assert_eq!(g.incoming(2), (&[2u32][..], &[5u64][..]));
        assert_eq!(g.n_edges(), 5);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn rejects_out_of_range_right_index() {
        let _ = R2lGraph::from_left_edges_fixed_degree(1, 2, 1, &[2u32], &[0u64]);
    }
}
