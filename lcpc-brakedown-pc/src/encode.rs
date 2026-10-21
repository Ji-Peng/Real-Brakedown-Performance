// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

/*!
encode a batch of vectors using an expander code

Every encoding goes through one batched kernel, [`encode_col_major`], which
encodes `n_rows` codewords at once (`step/03-column-major`). The batch is
stored **column-major** (position-major, row-minor): codeword position `col`
of row `row` lives at index `col * n_rows + row`, so each codeword position is
one contiguous "column" of `n_rows` elements. The single-vector [`encode`] is
the special case `n_rows = 1`.

Since `step/04-r2l`, the graphs are [`R2lGraph`]s and are evaluated right to
left (Algorithm 3 of SIMD-Spielman): for each output column `r`, the incoming
edges `(l, w)` are gathered, `out[r][row] = sum_l w * in[l][row]` for
`row in 0..n_rows`, and `out[r]` is written once. Input columns are only read.
This replaces the left-to-right scatter of `sprs`'s CSC matrix-vector product.

Field arithmetic uses lazy reduction via [`LazyField`]: for each output column,
unreduced products are accumulated in [`LazyField::Acc`] across the incoming
edges, respecting [`LazyField::MAX_ACCUMULATIONS`] with intermediate folding, and
reduced once per output column into the destination. Each inner iteration
processes one edge across the batch (`step/05-lazy`).

The code itself ([`ExpanderCode`]) is a list of precode/postcode graph pairs
plus a base case. It can be sampled by [`crate::matgen`] for either
code-parameter mode or assembled from externally sampled graphs with
[`ExpanderCode::from_graphs`].
*/

use crate::graph::R2lGraph;

use ff::Field;
use lcpc_fields::LazyField;

/// How the innermost level's precode output is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseCase {
    /// LCPC (mode (a)): the last precode output (at most `baselen` elements) is a
    /// temporary, not part of the codeword; it is Reed-Solomon encoded (Vandermonde
    /// evaluation at `x = 1, 2, ...`) into the last postcode's `n_left` positions.
    ReedSolomon,
    /// SIMD-Spielman (mode (b)): the last precode output is written into the
    /// codeword and the base code is the identity, so the last postcode reads it
    /// directly (its `n_left` equals the last precode's `n_right`).
    Identity,
}

/// A recursive expander code: precode/postcode graphs per level and a base case.
///
/// Level `i` encodes `x_i` (`precodes[i].n_left()` elements) as
/// `[x_i | Enc_{i+1}(y_i) | z_i]` with `y_i = G1_i x_i` and
/// `z_i = G2_i Enc_{i+1}(y_i)`, where `G1_i = precodes[i]` and
/// `G2_i = postcodes[i]`; after the last level, `Enc` is the [`BaseCase`].
#[derive(Clone, Debug)]
pub struct ExpanderCode<F> {
    n_per_row: usize,
    n_cols: usize,
    precodes: Vec<R2lGraph<F>>,
    postcodes: Vec<R2lGraph<F>>,
    base: BaseCase,
}

impl<F> ExpanderCode<F> {
    /// Assemble a code from its graphs, for example graphs sampled by an
    /// external sampler (the test-only glibc sampler that reproduces
    /// SIMD-Spielman's test vectors).
    ///
    /// # Panics
    ///
    /// Panics unless the graph dimensions form a valid code: at least one level,
    /// `precodes[i].n_right() == precodes[i + 1].n_left()`, and the postcode
    /// of level `i` reads exactly the encoding of the precode output, i.e.
    /// `postcodes[i].n_left() == precodes[i].n_right() + postcodes[i + 1].n_left()
    /// + postcodes[i + 1].n_right()`; for the last level, with
    /// [`BaseCase::Identity`], `postcodes[L - 1].n_left() == precodes[L - 1].n_right()`.
    pub fn from_graphs(
        base: BaseCase,
        precodes: Vec<R2lGraph<F>>,
        postcodes: Vec<R2lGraph<F>>,
    ) -> Self {
        assert!(!precodes.is_empty(), "a code needs at least one level");
        assert_eq!(precodes.len(), postcodes.len());
        let levels = precodes.len();
        for i in 0..levels - 1 {
            assert_eq!(
                precodes[i].n_right(),
                precodes[i + 1].n_left(),
                "precode {i} output does not match precode {} input",
                i + 1
            );
            assert_eq!(
                postcodes[i].n_left(),
                precodes[i].n_right() + postcodes[i + 1].n_left() + postcodes[i + 1].n_right(),
                "postcode {i} input is not the encoding of precode {i} output"
            );
        }
        if base == BaseCase::Identity {
            assert_eq!(
                postcodes[levels - 1].n_left(),
                precodes[levels - 1].n_right(),
                "identity base case: last postcode must read the last precode output"
            );
        }

        let n_per_row = precodes[0].n_left();
        let n_cols = n_per_row
            // base-case result (R-S output, or the last precode output for the identity)
            + postcodes[levels - 1].n_left()
            // precode outputs except the input to the base case
            + precodes[..levels - 1].iter().map(R2lGraph::n_right).sum::<usize>()
            // postcode outputs
            + postcodes.iter().map(R2lGraph::n_right).sum::<usize>();
        Self {
            n_per_row,
            n_cols,
            precodes,
            postcodes,
            base,
        }
    }

    /// Message length (inputs of the first precode).
    #[inline]
    pub fn n_per_row(&self) -> usize {
        self.n_per_row
    }

    /// Codeword length.
    #[inline]
    pub fn codeword_len(&self) -> usize {
        self.n_cols
    }

    /// Precode graphs `G1_0, ..., G1_{L-1}`.
    #[inline]
    pub fn precodes(&self) -> &[R2lGraph<F>] {
        &self.precodes
    }

    /// Postcode graphs `G2_0, ..., G2_{L-1}`.
    #[inline]
    pub fn postcodes(&self) -> &[R2lGraph<F>] {
        &self.postcodes
    }

    /// The base case.
    #[inline]
    pub fn base_case(&self) -> BaseCase {
        self.base
    }
}

/// encode a vector given a code of corresponding length
///
/// This is [`encode_col_major`] with a batch of one row.
pub fn encode<F, T>(mut xi: T, code: &ExpanderCode<F>)
where
    F: LazyField,
    T: AsMut<[F]>,
{
    encode_col_major(xi.as_mut(), 1, code);
}

/// Encode a batch of `n_rows` vectors stored column-major in `buf`.
///
/// `buf` holds `n_cols * n_rows` elements, where `n_cols = code.codeword_len()`;
/// element `(row, col)` is `buf[col * n_rows + row]`. On entry, columns
/// `0..code.n_per_row()` hold the messages; the remaining columns are overwritten
/// with the redundancy part of each codeword, so on return row `row` is the
/// codeword that [`encode`] would produce for that row's message.
///
/// # Panics
///
/// Panics if `n_rows == 0` or `buf.len() != n_cols * n_rows`.
pub fn encode_col_major<F>(buf: &mut [F], n_rows: usize, code: &ExpanderCode<F>)
where
    F: LazyField,
{
    let (precodes, postcodes) = (code.precodes(), code.postcodes());
    let levels = precodes.len();

    // check sizes
    assert!(n_rows > 0, "encode_col_major needs at least one row");
    let n_cols = code.codeword_len();
    assert_eq!(buf.len(), n_cols * n_rows);

    // compute precodes all the way down (all but the last for the R-S base case)
    let n_inplace = match code.base_case() {
        BaseCase::ReedSolomon => levels - 1,
        BaseCase::Identity => levels,
    };
    let mut in_start = 0usize;
    for precode in &precodes[..n_inplace] {
        let in_end = in_start + precode.n_left();
        let (in_arr, out_arr) = buf.split_at_mut(in_end * n_rows);
        encode_column_block(
            precode,
            &in_arr[in_start * n_rows..],
            &mut out_arr[..precode.n_right() * n_rows],
            n_rows,
        );
        in_start = in_end;
    }

    // base case; afterwards `in_start` is one past the last precode output and
    // `out_start` is where the deepest postcode writes
    let (mut in_start, mut out_start) = match code.base_case() {
        BaseCase::ReedSolomon => {
            // evaluate last precode into temporary column-major storage
            let precode = &precodes[levels - 1];
            let in_end = in_start + precode.n_left();
            let mut in_arr = vec![F::ZERO; precode.n_right() * n_rows];
            encode_column_block(
                precode,
                &buf[in_start * n_rows..in_end * n_rows],
                &mut in_arr,
                n_rows,
            );

            // now evaluate Reed-Solomon code on the result
            let out_end = in_end + postcodes[levels - 1].n_left();
            reed_solomon_column_block(&in_arr, &mut buf[in_end * n_rows..out_end * n_rows], n_rows);

            (in_end + precode.n_right(), out_end)
        }
        // identity: the last precode output already sits in the codeword
        BaseCase::Identity => {
            let end = in_start + precodes[levels - 1].n_right();
            (end, end)
        }
    };

    for (precode, postcode) in precodes.iter().rev().zip(postcodes.iter().rev()) {
        // move input pointer backward
        in_start -= precode.n_right();

        let (in_arr, out_arr) = buf.split_at_mut(out_start * n_rows);
        encode_column_block(
            postcode,
            &in_arr[in_start * n_rows..out_start * n_rows],
            &mut out_arr[..postcode.n_right() * n_rows],
            n_rows,
        );

        out_start += postcode.n_right();
    }

    assert_eq!(in_start, code.n_per_row());
    assert_eq!(out_start, n_cols);
}

/// Evaluate one expander graph on a column block, right to left (gather) with
/// unreduced lazy accumulation (Plan 01 §3.4, Plan 02 §3-4).
///
/// `input` holds `graph.n_left()` columns and `output` holds `graph.n_right()`
/// columns, each of `n_rows` contiguous elements. Output column `r` is
/// overwritten with `sum w * input[l]` over the incoming edges `(l, w)` of `r`,
/// element-wise across the column; a right node without edges gets zeros.
///
/// Kept out of line so that `scripts/check_codegen.py` can find and disassemble
/// it (one symbol per field, e.g. `encode_column_block::<Mont127>`).
#[inline(never)]
pub fn encode_column_block<F>(graph: &R2lGraph<F>, input: &[F], output: &mut [F], n_rows: usize)
where
    F: LazyField,
{
    assert!(n_rows > 0);
    assert_eq!(input.len(), graph.n_left() * n_rows);
    assert_eq!(output.len(), graph.n_right() * n_rows);

    let mut scratch = vec![F::Acc::default(); n_rows];

    for (r, dst) in output.chunks_exact_mut(n_rows).enumerate() {
        let (lefts, weights) = graph.incoming(r);
        let num_edges = lefts.len();
        if num_edges == 0 {
            dst.fill(F::ZERO);
            continue;
        }

        let mut since_fold = 0;

        for e in 0..num_edges {
            if since_fold >= F::MAX_ACCUMULATIONS {
                for acc in scratch.iter_mut() {
                    F::fold(acc);
                }
                since_fold = 1;
            }

            let w = F::prepare_weight(&weights[e]);
            let l = lefts[e] as usize * n_rows;
            let src = &input[l..l + n_rows];

            for (acc, s) in scratch.iter_mut().zip(src) {
                F::mac(acc, &w, s);
            }

            since_fold += 1;
        }

        for (d, acc) in dst.iter_mut().zip(scratch.iter_mut()) {
            *d = F::reduce(*acc);
            *acc = F::Acc::default();
        }
    }
}

/// Reed-Solomon encoding via the Vandermonde matrix, column-major.
///
/// `xi` holds the message columns and `xo` the output columns (`n_rows` elements
/// each). Output column `k` is the message polynomial evaluated at `x = k + 1`,
/// computed with Horner's rule element-wise across the column.
#[inline(never)]
pub fn reed_solomon_column_block<F>(xi: &[F], xo: &mut [F], n_rows: usize)
where
    F: Field,
{
    let mut x = F::ONE;
    for r in xo.chunks_exact_mut(n_rows) {
        r.fill(F::ZERO);
        for j in (0..xi.len() / n_rows).rev() {
            let coeff = &xi[j * n_rows..(j + 1) * n_rows];
            for (o, c) in r.iter_mut().zip(coeff) {
                *o *= x;
                *o += c;
            }
        }
        x += F::ONE;
    }
}

/// Copy a `rows x cols` block out of a row-major source into a transposed destination.
///
/// Element `(i, j)` of the block, read from `src[i * src_stride + j]`, is written to
/// `dst[j * dst_stride + i]`. The copy is tiled so that both sides touch a few cache
/// lines at a time; it is used to move between the row-major matrix of `lcpc-2d` and
/// the column-major batch of [`encode_col_major`].
pub(super) fn transpose_into<F: Copy>(
    src: &[F],
    src_stride: usize,
    dst: &mut [F],
    dst_stride: usize,
    rows: usize,
    cols: usize,
) {
    const TILE: usize = 16;
    for i0 in (0..rows).step_by(TILE) {
        let i1 = (i0 + TILE).min(rows);
        for j0 in (0..cols).step_by(TILE) {
            let j1 = (j0 + TILE).min(cols);
            for i in i0..i1 {
                for j in j0..j1 {
                    dst[j * dst_stride + i] = src[i * src_stride + j];
                }
            }
        }
    }
}
