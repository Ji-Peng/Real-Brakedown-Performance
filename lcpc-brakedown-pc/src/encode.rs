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

Since `step/03-column-major`, every encoding goes through one batched kernel,
[`encode_col_major`], which encodes `n_rows` codewords at once. The batch is
stored **column-major** (position-major, row-minor): codeword position `col`
of row `row` lives at index `col * n_rows + row`, so each codeword position is
one contiguous "column" of `n_rows` elements.

Each graph edge `(l, r, w)` is then applied to a whole column at once,
`out[r][row] += w * in[l][row]` for `row in 0..n_rows`: the edge and its
weight are loaded once per batch instead of once per row, both columns are
traversed with unit stride (full cache-line use), and consecutive rows carry no
data dependency on each other. The single-vector [`encode`] is the special
case `n_rows = 1`, whose column-major layout is the plain vector.

Field arithmetic is still eager (one modular multiply and add per edge and
row), and the graph is still `sprs`'s compressed sparse matrices, evaluated in
their storage order (left-to-right for LCPC's CSC matrices). Because field
addition is associative and commutative, the result is bit-exact with the
row-by-row `sprs` matrix-vector products this replaces.
*/

use ff::Field;
use num_traits::Num;
use sprs::{CsMat, MulAcc};

// given a set of precodes and postcodes, output length of codeword
pub(super) fn codeword_length<F>(precodes: &[CsMat<F>], postcodes: &[CsMat<F>]) -> usize
where
    F: Field + Num,
{
    assert!(!precodes.is_empty());
    assert_eq!(precodes.len(), postcodes.len());

    // input
    precodes[0].cols()
    // R-S result
        + postcodes.last().unwrap().cols()
    // precode outputs (except input to R-S, which is not part of codeword)
        + precodes.iter().take(precodes.len() - 1).map(|pc| pc.rows()).sum::<usize>()
    // postcode outputs
        + postcodes.iter().map(|pc| pc.rows()).sum::<usize>()
}

/// encode a vector given a code of corresponding length
///
/// This is [`encode_col_major`] with a batch of one row.
pub fn encode<F, T>(mut xi: T, precodes: &[CsMat<F>], postcodes: &[CsMat<F>])
where
    F: Field + Num + MulAcc,
    T: AsMut<[F]>,
{
    encode_col_major(xi.as_mut(), 1, precodes, postcodes);
}

/// Encode a batch of `n_rows` vectors stored column-major in `buf`.
///
/// `buf` holds `n_cols * n_rows` elements, where `n_cols` is the codeword length
/// of the code given by `precodes` and `postcodes`; element `(row, col)` is
/// `buf[col * n_rows + row]`. On entry, columns `0..n_per_row` (with
/// `n_per_row = precodes[0].cols()`) hold the messages; the remaining columns are
/// overwritten with the redundancy part of each codeword, so on return row `row`
/// is the codeword that [`encode`] would produce for that row's message.
///
/// # Panics
///
/// Panics if `n_rows == 0` or `buf.len() != n_cols * n_rows`.
pub fn encode_col_major<F>(
    buf: &mut [F],
    n_rows: usize,
    precodes: &[CsMat<F>],
    postcodes: &[CsMat<F>],
) where
    F: Field + Num + MulAcc,
{
    // check sizes
    assert!(n_rows > 0, "encode_col_major needs at least one row");
    let n_cols = codeword_length(precodes, postcodes);
    assert_eq!(buf.len(), n_cols * n_rows);

    // compute precodes all the way down
    let mut in_start = 0usize;
    for precode in precodes.iter().take(precodes.len() - 1) {
        // compute matrix product over the whole column block
        let in_end = in_start + precode.cols();
        let (in_arr, out_arr) = buf.split_at_mut(in_end * n_rows);
        mac_column_block(
            precode,
            &in_arr[in_start * n_rows..],
            &mut out_arr[..precode.rows() * n_rows],
            n_rows,
        );

        in_start = in_end;
    }

    // base-case code: Reed-Solomon
    let (mut in_start, mut out_start) = {
        // first, evaluate last precode into temporary column-major storage
        let precode = precodes.last().unwrap();
        let in_end = in_start + precode.cols();
        let mut in_arr = vec![<F as Field>::ZERO; precode.rows() * n_rows];
        mac_column_block(
            precode,
            &buf[in_start * n_rows..in_end * n_rows],
            &mut in_arr,
            n_rows,
        );

        // now evaluate Reed-Solomon code on the result
        let out_end = in_end + postcodes.last().unwrap().cols();
        reed_solomon_column_block(&in_arr, &mut buf[in_end * n_rows..out_end * n_rows], n_rows);

        (in_end + precode.rows(), out_end)
    };

    for (precode, postcode) in precodes.iter().rev().zip(postcodes.iter().rev()) {
        // move input pointer backward
        in_start -= precode.rows();

        // compute matrix product over the whole column block
        let (in_arr, out_arr) = buf.split_at_mut(out_start * n_rows);
        mac_column_block(
            postcode,
            &in_arr[in_start * n_rows..],
            &mut out_arr[..postcode.rows() * n_rows],
            n_rows,
        );

        out_start += postcode.rows();
    }

    assert_eq!(in_start, precodes[0].cols());
    assert_eq!(out_start, n_cols);
}

/// Sparse matrix times column block: `output = mat * input`, column-major.
///
/// `input` holds `mat.cols()` columns and `output` holds `mat.rows()` columns, each
/// of `n_rows` contiguous elements. Every nonzero `w` at `(r, l)` adds `w * input[l]`
/// to `output[r]` element-wise across the column, with eager field arithmetic.
/// Kept out of line so that `scripts/check_codegen.py` can find and disassemble it.
#[inline(never)]
fn mac_column_block<F>(mat: &CsMat<F>, input: &[F], output: &mut [F], n_rows: usize)
where
    F: Field + MulAcc,
{
    assert_eq!(input.len(), mat.cols() * n_rows);
    assert_eq!(output.len(), mat.rows() * n_rows);
    output.fill(<F as Field>::ZERO);

    // apply one edge (l, r, w) to the whole column block
    let mut edge = |l: usize, r: usize, w: &F| {
        let src = &input[l * n_rows..(l + 1) * n_rows];
        let dst = &mut output[r * n_rows..(r + 1) * n_rows];
        for (d, s) in dst.iter_mut().zip(src) {
            d.mul_acc(w, s);
        }
    };

    // walk the edges in storage order: by input node for CSC (LCPC's matgen),
    // by output node for CSR
    let csc = mat.is_csc();
    for (outer, vec) in mat.outer_iterator().enumerate() {
        for (inner, w) in vec.iter() {
            if csc {
                edge(outer, inner, w);
            } else {
                edge(inner, outer, w);
            }
        }
    }
}

/// Reed-Solomon encoding via the Vandermonde matrix, column-major.
///
/// `xi` holds the message columns and `xo` the output columns (`n_rows` elements
/// each). Output column `k` is the message polynomial evaluated at `x = k + 1`,
/// computed with Horner's rule element-wise across the column.
#[inline(never)]
fn reed_solomon_column_block<F>(xi: &[F], xo: &mut [F], n_rows: usize)
where
    F: Field,
{
    let mut x = <F as Field>::ONE;
    for r in xo.chunks_exact_mut(n_rows) {
        r.fill(<F as Field>::ZERO);
        for j in (0..xi.len() / n_rows).rev() {
            let coeff = &xi[j * n_rows..(j + 1) * n_rows];
            for (o, c) in r.iter_mut().zip(coeff) {
                *o *= x;
                *o += c;
            }
        }
        x += <F as Field>::ONE;
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
