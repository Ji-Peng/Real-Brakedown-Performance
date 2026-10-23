// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.
//
// Modified 2026 by the RealBrakedownPerf authors (Apache-2.0, see NOTICE):
// column-major batched encoding, R2L evaluation, lazy reduction,
// multi-edge blocking, LLC slicing, expander-level worker pool, codegen tuning.
// The per-commit changes are in the git history since step/00-import
// (upstream conroi/lcpc f0e2116).

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
unreduced products are accumulated in [`LazyField::Acc`] across incoming
edges, respecting [`LazyField::MAX_ACCUMULATIONS`] with intermediate folding, and
reduced once per output column into the destination. Multi-edge blocking
(`step/06-blocking`) processes `BLOCK_FACTOR = 4` edges simultaneously in the
inner loop, keeping the row accumulator in registers and reducing scratch memory
traffic by 4x.

The code itself ([`ExpanderCode`]) is a list of precode/postcode graph pairs
plus a base case. It can be sampled by [`crate::matgen`] for either
code-parameter mode or assembled from externally sampled graphs with
[`ExpanderCode::from_graphs`].
*/

use crate::graph::R2lGraph;
use crate::thread_pool::{split_range, SenseReversingBarrier, ThreadPool, WorkerJob};

use ff::Field;
use lcpc_fields::LazyField;

/// Compile-time multi-edge blocking factor B (Plan 01 §3.5, Plan 03 §2.2, Decision D8).
///
/// B edges are processed simultaneously per inner iteration, keeping the row accumulator
/// in scalar registers across B edges and reducing memory traffic to the scratch accumulator buffer by Bx.
pub const BLOCK_FACTOR: usize = 4;

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

/// Encode a batch of `n_rows` rows stored row-major in `comm` using `slices` horizontal slices (Plan 04 §2).
///
/// If `slices == 0`, the recommended adaptive slice count from [`crate::cache::adaptive_slice_count`] is used.
/// If `slices == 1`, all rows are processed in a single batch (previous behaviour).
/// If `slices > 1`, rows are processed in `slices` horizontal slices, keeping the working set
/// of each slice within the host CPU's LLC budget.
///
/// On entry, `comm[row * n_cols .. row * n_cols + n_per_row]` contains the message for `row`.
/// On return, the remaining columns `n_per_row..n_cols` hold the redundancy of each codeword.
pub fn encode_row_major_slices<F: LazyField>(
    comm: &mut [F],
    n_rows: usize,
    code: &ExpanderCode<F>,
    slices: usize,
) {
    if n_rows == 0 {
        return;
    }
    let n_cols = code.codeword_len();
    let n_per_row = code.n_per_row();
    assert_eq!(comm.len(), n_rows * n_cols);

    let effective_s = if slices == 0 {
        crate::cache::adaptive_slice_count(n_per_row, n_rows, std::mem::size_of::<F>())
    } else {
        slices
    };

    let s = effective_s.clamp(1, n_rows);
    let base_r = n_rows / s;
    let max_h = if s == 1 {
        n_rows
    } else {
        base_r.max(n_rows - (s - 1) * base_r)
    };

    let mut buf = vec![F::ZERO; n_cols * max_h];

    for slice_idx in 0..s {
        let r_start = slice_idx * base_r;
        let r_end = if slice_idx == s - 1 {
            n_rows
        } else {
            (slice_idx + 1) * base_r
        };
        let h = r_end - r_start;
        if h == 0 {
            continue;
        }

        let slice_buf = &mut buf[..n_cols * h];

        // 1. Transpose row-major messages from comm into column-major slice_buf
        transpose_into(
            &comm[r_start * n_cols..],
            n_cols,
            slice_buf,
            h,
            h,
            n_per_row,
        );

        // 2. Encode this slice
        encode_col_major(slice_buf, h, code);

        // 3. Transpose column-major redundancy back into row-major comm
        transpose_into(
            &slice_buf[n_per_row * h..],
            h,
            &mut comm[r_start * n_cols + n_per_row..],
            n_cols,
            n_cols - n_per_row,
            h,
        );
    }
}

struct RowMajorSliceJob<'a, F: LazyField> {
    comm: *mut F,
    buf: *mut F,
    n_rows: usize,
    n_cols: usize,
    n_per_row: usize,
    code: &'a ExpanderCode<F>,
    slices: usize,
    base_r: usize,
    max_h: usize,
    temp_rs: *mut F,
}

// Safety: the raw pointers refer to buffers exclusively borrowed by the
// coordinator for the duration of `ThreadPool::execute`. Workers partition
// source columns, output graph nodes, Reed-Solomon columns, and final rows into
// disjoint ranges. Barriers separate each read/write phase, and execute waits
// for every worker before those borrows can end.
unsafe impl<'a, F: LazyField> Send for RowMajorSliceJob<'a, F> {}
// Safety: see the partitioning and lifetime invariant above.
unsafe impl<'a, F: LazyField> Sync for RowMajorSliceJob<'a, F> {}

impl<'a, F: LazyField> WorkerJob for RowMajorSliceJob<'a, F> {
    fn run(&self, tid: usize, num_threads: usize, barrier: &SenseReversingBarrier) {
        let precodes = self.code.precodes();
        let postcodes = self.code.postcodes();
        let levels = precodes.len();
        let n_inplace = match self.code.base_case() {
            BaseCase::ReedSolomon => levels - 1,
            BaseCase::Identity => levels,
        };

        let mut scratch = vec![F::Acc::default(); self.max_h];

        for slice_idx in 0..self.slices {
            let r_start = slice_idx * self.base_r;
            let r_end = if slice_idx == self.slices - 1 {
                self.n_rows
            } else {
                (slice_idx + 1) * self.base_r
            };
            let h = r_end - r_start;
            if h == 0 {
                continue;
            }

            let slice_buf = self.buf;
            let comm = self.comm;

            // 1. Collaborative Transpose of messages from comm into slice_buf
            let (c_start, c_end) = split_range(self.n_per_row, tid, num_threads);
            for c in c_start..c_end {
                for r in 0..h {
                    unsafe {
                        let val = *comm.add((r_start + r) * self.n_cols + c);
                        *slice_buf.add(c * h + r) = val;
                    }
                }
            }
            barrier.wait();

            // 2. Collaborative expander evaluation on slice_buf
            let mut in_start = 0usize;
            for precode in &precodes[..n_inplace] {
                let in_end = in_start + precode.n_left();
                let out_start = in_end;
                let (r_start_node, r_end_node) = split_range(precode.n_right(), tid, num_threads);

                unsafe {
                    let in_slice = std::slice::from_raw_parts(
                        slice_buf.add(in_start * h),
                        precode.n_left() * h,
                    );
                    let out_slice = std::slice::from_raw_parts_mut(
                        slice_buf.add((out_start + r_start_node) * h),
                        (r_end_node - r_start_node) * h,
                    );
                    encode_column_block_range_shard::<F, BLOCK_FACTOR>(
                        precode,
                        in_slice,
                        out_slice,
                        h,
                        r_start_node,
                        r_end_node,
                        &mut scratch,
                    );
                }

                barrier.wait();
                in_start = in_end;
            }

            // Base case
            let (mut in_start, mut out_start) = match self.code.base_case() {
                BaseCase::ReedSolomon => {
                    let precode = &precodes[levels - 1];
                    let in_end = in_start + precode.n_left();
                    let (r_start_node, r_end_node) =
                        split_range(precode.n_right(), tid, num_threads);

                    unsafe {
                        let in_slice = std::slice::from_raw_parts(
                            slice_buf.add(in_start * h),
                            precode.n_left() * h,
                        );
                        let out_slice = std::slice::from_raw_parts_mut(
                            self.temp_rs.add(r_start_node * h),
                            (r_end_node - r_start_node) * h,
                        );
                        encode_column_block_range_shard::<F, BLOCK_FACTOR>(
                            precode,
                            in_slice,
                            out_slice,
                            h,
                            r_start_node,
                            r_end_node,
                            &mut scratch,
                        );
                    }
                    barrier.wait();

                    let out_end = in_end + postcodes[levels - 1].n_left();
                    let n_rs_out = postcodes[levels - 1].n_left();
                    let (c_start_rs, c_end_rs) = split_range(n_rs_out, tid, num_threads);

                    unsafe {
                        let in_rs = std::slice::from_raw_parts(self.temp_rs, precode.n_right() * h);
                        let out_rs = std::slice::from_raw_parts_mut(
                            slice_buf.add((in_end + c_start_rs) * h),
                            (c_end_rs - c_start_rs) * h,
                        );
                        reed_solomon_column_block_range_shard(
                            in_rs, out_rs, h, c_start_rs, c_end_rs,
                        );
                    }
                    barrier.wait();

                    (in_end + precode.n_right(), out_end)
                }
                BaseCase::Identity => {
                    let end = in_start + precodes[levels - 1].n_right();
                    (end, end)
                }
            };

            // Postcodes (bottom-up)
            for (precode, postcode) in precodes.iter().rev().zip(postcodes.iter().rev()) {
                in_start -= precode.n_right();
                let (r_start_node, r_end_node) = split_range(postcode.n_right(), tid, num_threads);

                unsafe {
                    let in_slice = std::slice::from_raw_parts(
                        slice_buf.add(in_start * h),
                        (out_start - in_start) * h,
                    );
                    let out_slice = std::slice::from_raw_parts_mut(
                        slice_buf.add((out_start + r_start_node) * h),
                        (r_end_node - r_start_node) * h,
                    );
                    encode_column_block_range_shard::<F, BLOCK_FACTOR>(
                        postcode,
                        in_slice,
                        out_slice,
                        h,
                        r_start_node,
                        r_end_node,
                        &mut scratch,
                    );
                }

                barrier.wait();
                out_start += postcode.n_right();
            }

            // 3. Collaborative Transpose of redundancy from slice_buf into comm
            let (row_start_t, row_end_t) = split_range(h, tid, num_threads);
            for r in row_start_t..row_end_t {
                for c in self.n_per_row..self.n_cols {
                    unsafe {
                        let val = *slice_buf.add(c * h + r);
                        *comm.add((r_start + r) * self.n_cols + c) = val;
                    }
                }
            }
            barrier.wait();
        }
    }
}

/// Encode a batch of `n_rows` rows stored row-major in `comm` using multithreaded expander-level parallelism (Plan 04 §3).
///
/// Workers in `pool` collaboratively encode each slice, synchronizing between expander levels via a barrier.
pub fn encode_row_major_slices_mt<F: LazyField>(
    comm: &mut [F],
    n_rows: usize,
    code: &ExpanderCode<F>,
    slices: usize,
    pool: &ThreadPool,
) {
    if n_rows == 0 {
        return;
    }
    if pool.num_threads() <= 1 {
        encode_row_major_slices(comm, n_rows, code, slices);
        return;
    }

    let n_cols = code.codeword_len();
    let n_per_row = code.n_per_row();
    assert_eq!(comm.len(), n_rows * n_cols);

    let effective_s = if slices == 0 {
        crate::cache::adaptive_slice_count(n_per_row, n_rows, std::mem::size_of::<F>())
    } else {
        slices
    };

    let s = effective_s.clamp(1, n_rows);
    let base_r = n_rows / s;
    let max_h = if s == 1 {
        n_rows
    } else {
        base_r.max(n_rows - (s - 1) * base_r)
    };

    let mut buf = vec![F::ZERO; n_cols * max_h];
    let last_precode_right = code.precodes().last().map(|p| p.n_right()).unwrap_or(0);
    let mut temp_rs = if code.base_case() == BaseCase::ReedSolomon {
        vec![F::ZERO; last_precode_right * max_h]
    } else {
        Vec::new()
    };

    let job = RowMajorSliceJob {
        comm: comm.as_mut_ptr(),
        buf: buf.as_mut_ptr(),
        n_rows,
        n_cols,
        n_per_row,
        code,
        slices: s,
        base_r,
        max_h,
        temp_rs: temp_rs.as_mut_ptr(),
    };

    pool.execute(&job);
}

/// Encode a batch of `n_rows` vectors laid out in slice-column-major format (Plan 04 §1.2).
///
/// In slice-column-major layout, each horizontal slice $i \in [0, s)$ is contiguous and stored
/// column-major with $R_s$ rows per column. Slices are evaluated sequentially to ensure LLC residence.
pub fn encode_col_major_sliced<F: LazyField>(
    buf: &mut [F],
    n_rows: usize,
    code: &ExpanderCode<F>,
    slices: usize,
) {
    if n_rows == 0 {
        return;
    }
    let n_cols = code.codeword_len();
    assert_eq!(buf.len(), n_cols * n_rows);

    let effective_s = if slices == 0 {
        crate::cache::adaptive_slice_count(code.n_per_row(), n_rows, std::mem::size_of::<F>())
    } else {
        slices
    };

    let s = effective_s.clamp(1, n_rows);
    let base_r = n_rows / s;

    let mut offset = 0;
    for slice_idx in 0..s {
        let r_start = slice_idx * base_r;
        let r_end = if slice_idx == s - 1 {
            n_rows
        } else {
            (slice_idx + 1) * base_r
        };
        let h = r_end - r_start;
        if h == 0 {
            continue;
        }
        let slice_len = n_cols * h;
        encode_col_major(&mut buf[offset..offset + slice_len], h, code);
        offset += slice_len;
    }
}

struct ColMajorSlicedJob<'a, F: LazyField> {
    buf: *mut F,
    n_rows: usize,
    n_cols: usize,
    code: &'a ExpanderCode<F>,
    slices: usize,
    base_r: usize,
    max_h: usize,
    temp_rs: *mut F,
}

// Safety: the raw pointers refer to buffers exclusively borrowed by the
// coordinator for the duration of `ThreadPool::execute`. Workers partition
// output graph nodes into disjoint ranges, and barriers separate graph levels
// and slices. The coordinator waits for every worker before the borrows end.
unsafe impl<'a, F: LazyField> Send for ColMajorSlicedJob<'a, F> {}
// Safety: see the partitioning and lifetime invariant above.
unsafe impl<'a, F: LazyField> Sync for ColMajorSlicedJob<'a, F> {}

impl<'a, F: LazyField> WorkerJob for ColMajorSlicedJob<'a, F> {
    fn run(&self, tid: usize, num_threads: usize, barrier: &SenseReversingBarrier) {
        let precodes = self.code.precodes();
        let postcodes = self.code.postcodes();
        let levels = precodes.len();
        let n_inplace = match self.code.base_case() {
            BaseCase::ReedSolomon => levels - 1,
            BaseCase::Identity => levels,
        };

        let mut scratch = vec![F::Acc::default(); self.max_h];
        let mut offset = 0usize;

        for slice_idx in 0..self.slices {
            let r_start = slice_idx * self.base_r;
            let r_end = if slice_idx == self.slices - 1 {
                self.n_rows
            } else {
                (slice_idx + 1) * self.base_r
            };
            let h = r_end - r_start;
            if h == 0 {
                continue;
            }

            let slice_len = self.n_cols * h;
            let slice_buf = unsafe { self.buf.add(offset) };

            // Precodes
            let mut in_start = 0usize;
            for precode in &precodes[..n_inplace] {
                let in_end = in_start + precode.n_left();
                let out_start = in_end;
                let (r_start_node, r_end_node) = split_range(precode.n_right(), tid, num_threads);

                unsafe {
                    let in_slice = std::slice::from_raw_parts(
                        slice_buf.add(in_start * h),
                        precode.n_left() * h,
                    );
                    let out_slice = std::slice::from_raw_parts_mut(
                        slice_buf.add((out_start + r_start_node) * h),
                        (r_end_node - r_start_node) * h,
                    );
                    encode_column_block_range_shard::<F, BLOCK_FACTOR>(
                        precode,
                        in_slice,
                        out_slice,
                        h,
                        r_start_node,
                        r_end_node,
                        &mut scratch,
                    );
                }

                barrier.wait();
                in_start = in_end;
            }

            // Base case
            let (mut in_start, mut out_start) = match self.code.base_case() {
                BaseCase::ReedSolomon => {
                    let precode = &precodes[levels - 1];
                    let in_end = in_start + precode.n_left();
                    let (r_start_node, r_end_node) =
                        split_range(precode.n_right(), tid, num_threads);

                    unsafe {
                        let in_slice = std::slice::from_raw_parts(
                            slice_buf.add(in_start * h),
                            precode.n_left() * h,
                        );
                        let out_slice = std::slice::from_raw_parts_mut(
                            self.temp_rs.add(r_start_node * h),
                            (r_end_node - r_start_node) * h,
                        );
                        encode_column_block_range_shard::<F, BLOCK_FACTOR>(
                            precode,
                            in_slice,
                            out_slice,
                            h,
                            r_start_node,
                            r_end_node,
                            &mut scratch,
                        );
                    }
                    barrier.wait();

                    let out_end = in_end + postcodes[levels - 1].n_left();
                    let n_rs_out = postcodes[levels - 1].n_left();
                    let (c_start_rs, c_end_rs) = split_range(n_rs_out, tid, num_threads);

                    unsafe {
                        let in_rs = std::slice::from_raw_parts(self.temp_rs, precode.n_right() * h);
                        let out_rs = std::slice::from_raw_parts_mut(
                            slice_buf.add((in_end + c_start_rs) * h),
                            (c_end_rs - c_start_rs) * h,
                        );
                        reed_solomon_column_block_range_shard(
                            in_rs, out_rs, h, c_start_rs, c_end_rs,
                        );
                    }
                    barrier.wait();

                    (in_end + precode.n_right(), out_end)
                }
                BaseCase::Identity => {
                    let end = in_start + precodes[levels - 1].n_right();
                    (end, end)
                }
            };

            // Postcodes
            for (precode, postcode) in precodes.iter().rev().zip(postcodes.iter().rev()) {
                in_start -= precode.n_right();
                let (r_start_node, r_end_node) = split_range(postcode.n_right(), tid, num_threads);

                unsafe {
                    let in_slice = std::slice::from_raw_parts(
                        slice_buf.add(in_start * h),
                        (out_start - in_start) * h,
                    );
                    let out_slice = std::slice::from_raw_parts_mut(
                        slice_buf.add((out_start + r_start_node) * h),
                        (r_end_node - r_start_node) * h,
                    );
                    encode_column_block_range_shard::<F, BLOCK_FACTOR>(
                        postcode,
                        in_slice,
                        out_slice,
                        h,
                        r_start_node,
                        r_end_node,
                        &mut scratch,
                    );
                }

                barrier.wait();
                out_start += postcode.n_right();
            }

            offset += slice_len;
        }
    }
}

/// Encode a batch of `n_rows` vectors laid out in slice-column-major format using multithreading.
pub fn encode_col_major_sliced_mt<F: LazyField>(
    buf: &mut [F],
    n_rows: usize,
    code: &ExpanderCode<F>,
    slices: usize,
    pool: &ThreadPool,
) {
    if n_rows == 0 {
        return;
    }
    if pool.num_threads() <= 1 {
        encode_col_major_sliced(buf, n_rows, code, slices);
        return;
    }

    let n_cols = code.codeword_len();
    assert_eq!(buf.len(), n_cols * n_rows);

    let effective_s = if slices == 0 {
        crate::cache::adaptive_slice_count(code.n_per_row(), n_rows, std::mem::size_of::<F>())
    } else {
        slices
    };

    let s = effective_s.clamp(1, n_rows);
    let base_r = n_rows / s;
    let max_h = if s == 1 {
        n_rows
    } else {
        base_r.max(n_rows - (s - 1) * base_r)
    };

    let last_precode_right = code.precodes().last().map(|p| p.n_right()).unwrap_or(0);
    let mut temp_rs = if code.base_case() == BaseCase::ReedSolomon {
        vec![F::ZERO; last_precode_right * max_h]
    } else {
        Vec::new()
    };

    let job = ColMajorSlicedJob {
        buf: buf.as_mut_ptr(),
        n_rows,
        n_cols,
        code,
        slices: s,
        base_r,
        max_h,
        temp_rs: temp_rs.as_mut_ptr(),
    };

    pool.execute(&job);
}

/// Auto-tune the optimal slice count empirically by timing candidate slice counts around $s^*$ (Plan 04 §2.2 Step 3).
///
/// Evaluates candidate slice counts in $\{s^*/2, s^*, 2s^*, 4s^*\}$ filtered to valid slice counts
/// between 1 and `n_rows`. Performs 1 warm-up iteration and 2 timed iterations per candidate,
/// locking in the slice count with the lowest wall-clock latency.
pub fn autotune_slices<F: LazyField>(code: &ExpanderCode<F>, n_rows: usize) -> usize {
    autotune_slices_with_pool(code, n_rows, None)
}

/// Auto-tune the optimal slice count empirically, optionally using worker `pool` if provided.
pub fn autotune_slices_with_pool<F: LazyField>(
    code: &ExpanderCode<F>,
    n_rows: usize,
    pool: Option<&ThreadPool>,
) -> usize {
    if n_rows <= 1 {
        return 1;
    }
    let s_star =
        crate::cache::adaptive_slice_count(code.n_per_row(), n_rows, std::mem::size_of::<F>());

    let mut candidates = Vec::with_capacity(4);
    if s_star >= 2 {
        candidates.push(s_star / 2);
    }
    candidates.push(s_star);
    if s_star * 2 <= n_rows {
        candidates.push(s_star * 2);
    }
    if s_star * 4 <= n_rows {
        candidates.push(s_star * 4);
    }
    candidates.sort_unstable();
    candidates.dedup();

    if candidates.len() <= 1 {
        return candidates[0];
    }

    let n_cols = code.codeword_len();
    let n_per_row = code.n_per_row();
    let mut matrix = vec![F::ZERO; n_cols * n_rows];
    // Seed initial message with a non-zero pattern
    for r in 0..n_rows {
        for c in 0..n_per_row {
            matrix[r * n_cols + c] = F::ONE;
        }
    }
    let initial = matrix.clone();

    let mut best_s = s_star;
    let mut min_time = std::time::Duration::MAX;

    let run_enc = |mat: &mut [F], s: usize| {
        if let Some(p) = pool {
            encode_row_major_slices_mt(mat, n_rows, code, s, p);
        } else {
            encode_row_major_slices(mat, n_rows, code, s);
        }
    };

    for &s in &candidates {
        // 1 warm-up iteration
        matrix.copy_from_slice(&initial);
        run_enc(&mut matrix, s);

        // 2 timed iterations
        let start = std::time::Instant::now();
        for _ in 0..2 {
            matrix.copy_from_slice(&initial);
            run_enc(&mut matrix, s);
        }
        let elapsed = start.elapsed();

        if elapsed < min_time {
            min_time = elapsed;
            best_s = s;
        }
    }

    best_s
}

/// Evaluate one expander graph on a column block, right to left (gather) with
/// unreduced lazy accumulation and multi-edge register blocking (Plan 01 §3.4-§3.5, Plan 03).
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
    encode_column_block_blocked::<F, BLOCK_FACTOR>(graph, input, output, n_rows);
}

#[inline(always)]
fn encode_column_block_blocked<F: LazyField, const B: usize>(
    graph: &R2lGraph<F>,
    input: &[F],
    output: &mut [F],
    n_rows: usize,
) {
    let mut scratch = vec![F::Acc::default(); n_rows];
    encode_column_block_range_blocked::<F, B>(
        graph,
        input,
        output,
        n_rows,
        0,
        graph.n_right(),
        &mut scratch,
    );
}

/// Evaluate incoming edges for a contiguous range of right nodes `[r_start, r_end)`.
///
/// Output column `r` is written to `output[r * n_rows .. (r + 1) * n_rows]`.
/// Used by single-threaded execution (r_start=0, r_end=n_right) and multithreaded
/// worker partitions.
#[inline(always)]
pub fn encode_column_block_range_blocked<F: LazyField, const B: usize>(
    graph: &R2lGraph<F>,
    input: &[F],
    output: &mut [F],
    n_rows: usize,
    r_start: usize,
    r_end: usize,
    scratch: &mut [F::Acc],
) {
    assert!(n_rows > 0);
    assert_eq!(input.len(), graph.n_left() * n_rows);
    assert_eq!(output.len(), graph.n_right() * n_rows);
    assert!(r_start <= r_end && r_end <= graph.n_right());

    let output_range = &mut output[r_start * n_rows..r_end * n_rows];
    encode_column_block_range_shard::<F, B>(
        graph,
        input,
        output_range,
        n_rows,
        r_start,
        r_end,
        scratch,
    );
}

/// Evaluate a contiguous right-node range into a slice containing only that
/// range. The output slice is therefore disjoint across workers.
#[inline(always)]
fn encode_column_block_range_shard<F: LazyField, const B: usize>(
    graph: &R2lGraph<F>,
    input: &[F],
    output: &mut [F],
    n_rows: usize,
    r_start: usize,
    r_end: usize,
    scratch: &mut [F::Acc],
) {
    assert!(n_rows > 0);
    assert_eq!(input.len(), graph.n_left() * n_rows);
    assert!(r_start <= r_end && r_end <= graph.n_right());
    assert_eq!(output.len(), (r_end - r_start) * n_rows);
    assert!(scratch.len() >= n_rows);

    let scratch_slice = &mut scratch[..n_rows];

    for (r, dst) in (r_start..r_end).zip(output.chunks_exact_mut(n_rows)) {
        let (lefts, weights) = graph.incoming(r);
        let num_edges = lefts.len();
        if num_edges == 0 {
            dst.fill(F::ZERO);
            continue;
        }

        let mut since_fold = 0;

        // A blocked step of B edges after a fold (which leaves `since_fold == 1`)
        // needs `MAX_ACCUMULATIONS >= B + 1`. Fields with less headroom use the
        // one-edge loop below; the condition is a compile-time constant.
        if B == 4 && F::MAX_ACCUMULATIONS > 4 {
            let left_chunks = lefts.chunks_exact(4);
            let weight_chunks = weights.chunks_exact(4);

            for (l_chunk, w_chunk) in left_chunks.zip(weight_chunks) {
                if since_fold + 4 > F::MAX_ACCUMULATIONS {
                    for acc in scratch_slice.iter_mut() {
                        F::fold(acc);
                    }
                    since_fold = 1;
                }

                let &[l0_idx, l1_idx, l2_idx, l3_idx] = l_chunk else {
                    unreachable!()
                };
                let &[ref w0_raw, ref w1_raw, ref w2_raw, ref w3_raw] = w_chunk else {
                    unreachable!()
                };

                let w0 = F::prepare_weight(w0_raw);
                let w1 = F::prepare_weight(w1_raw);
                let w2 = F::prepare_weight(w2_raw);
                let w3 = F::prepare_weight(w3_raw);

                let l0 = l0_idx as usize * n_rows;
                let l1 = l1_idx as usize * n_rows;
                let l2 = l2_idx as usize * n_rows;
                let l3 = l3_idx as usize * n_rows;

                let src0 = &input[l0..l0 + n_rows];
                let src1 = &input[l1..l1 + n_rows];
                let src2 = &input[l2..l2 + n_rows];
                let src3 = &input[l3..l3 + n_rows];

                for row in 0..n_rows {
                    let mut acc = scratch_slice[row];
                    F::mac(&mut acc, &w0, &src0[row]);
                    F::mac(&mut acc, &w1, &src1[row]);
                    F::mac(&mut acc, &w2, &src2[row]);
                    F::mac(&mut acc, &w3, &src3[row]);
                    scratch_slice[row] = acc;
                }

                since_fold += 4;
            }

            let left_rem = lefts.chunks_exact(4).remainder();
            let weight_rem = weights.chunks_exact(4).remainder();
            for (&l_idx, w_raw) in left_rem.iter().zip(weight_rem.iter()) {
                if since_fold >= F::MAX_ACCUMULATIONS {
                    for acc in scratch_slice.iter_mut() {
                        F::fold(acc);
                    }
                    since_fold = 1;
                }

                let w = F::prepare_weight(w_raw);
                let l = l_idx as usize * n_rows;
                let src = &input[l..l + n_rows];

                for row in 0..n_rows {
                    let mut acc = scratch_slice[row];
                    F::mac(&mut acc, &w, &src[row]);
                    scratch_slice[row] = acc;
                }

                since_fold += 1;
            }
        } else if B == 2 && F::MAX_ACCUMULATIONS > 2 {
            let left_chunks = lefts.chunks_exact(2);
            let weight_chunks = weights.chunks_exact(2);

            for (l_chunk, w_chunk) in left_chunks.zip(weight_chunks) {
                if since_fold + 2 > F::MAX_ACCUMULATIONS {
                    for acc in scratch_slice.iter_mut() {
                        F::fold(acc);
                    }
                    since_fold = 1;
                }

                let &[l0_idx, l1_idx] = l_chunk else {
                    unreachable!()
                };
                let &[ref w0_raw, ref w1_raw] = w_chunk else {
                    unreachable!()
                };

                let w0 = F::prepare_weight(w0_raw);
                let w1 = F::prepare_weight(w1_raw);

                let l0 = l0_idx as usize * n_rows;
                let l1 = l1_idx as usize * n_rows;

                let src0 = &input[l0..l0 + n_rows];
                let src1 = &input[l1..l1 + n_rows];

                for row in 0..n_rows {
                    let mut acc = scratch_slice[row];
                    F::mac(&mut acc, &w0, &src0[row]);
                    F::mac(&mut acc, &w1, &src1[row]);
                    scratch_slice[row] = acc;
                }

                since_fold += 2;
            }

            let left_rem = lefts.chunks_exact(2).remainder();
            let weight_rem = weights.chunks_exact(2).remainder();
            for (&l_idx, w_raw) in left_rem.iter().zip(weight_rem.iter()) {
                if since_fold >= F::MAX_ACCUMULATIONS {
                    for acc in scratch_slice.iter_mut() {
                        F::fold(acc);
                    }
                    since_fold = 1;
                }

                let w = F::prepare_weight(w_raw);
                let l = l_idx as usize * n_rows;
                let src = &input[l..l + n_rows];

                for row in 0..n_rows {
                    let mut acc = scratch_slice[row];
                    F::mac(&mut acc, &w, &src[row]);
                    scratch_slice[row] = acc;
                }

                since_fold += 1;
            }
        } else {
            // Unblocked loop (B == 1, or a field with too little headroom for B)
            for (&l_idx, w_raw) in lefts.iter().zip(weights.iter()) {
                if since_fold >= F::MAX_ACCUMULATIONS {
                    for acc in scratch_slice.iter_mut() {
                        F::fold(acc);
                    }
                    since_fold = 1;
                }

                let w = F::prepare_weight(w_raw);
                let l = l_idx as usize * n_rows;
                let src = &input[l..l + n_rows];

                for row in 0..n_rows {
                    let mut acc = scratch_slice[row];
                    F::mac(&mut acc, &w, &src[row]);
                    scratch_slice[row] = acc;
                }

                since_fold += 1;
            }
        }

        for (d, acc) in dst.iter_mut().zip(scratch_slice.iter_mut()) {
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
    assert!(n_rows > 0);
    let total_cols = xo.len() / n_rows;
    reed_solomon_column_block_range(xi, xo, n_rows, 0, total_cols);
}

/// Reed-Solomon encoding for a contiguous range of output columns `[c_start, c_end)`.
pub fn reed_solomon_column_block_range<F>(
    xi: &[F],
    xo: &mut [F],
    n_rows: usize,
    c_start: usize,
    c_end: usize,
) where
    F: Field,
{
    assert!(n_rows > 0);
    let total_cols = xo.len() / n_rows;
    assert!(c_start <= c_end && c_end <= total_cols);
    let output_range = &mut xo[c_start * n_rows..c_end * n_rows];
    reed_solomon_column_block_range_shard(xi, output_range, n_rows, c_start, c_end);
}

/// Reed-Solomon encoding into a slice containing only columns `[c_start, c_end)`.
#[inline(always)]
fn reed_solomon_column_block_range_shard<F: Field>(
    xi: &[F],
    xo: &mut [F],
    n_rows: usize,
    c_start: usize,
    c_end: usize,
) {
    assert!(n_rows > 0);
    assert_eq!(xi.len() % n_rows, 0);
    assert!(c_start <= c_end);
    assert_eq!(xo.len(), (c_end - c_start) * n_rows);

    let mut x = F::ONE;
    for _ in 0..c_start {
        x += F::ONE;
    }

    for r in xo.chunks_exact_mut(n_rows) {
        r.fill(F::ZERO);
        for coeff in xi.chunks_exact(n_rows).rev() {
            for (o, c) in r.iter_mut().zip(coeff.iter()) {
                *o *= x;
                *o += c;
            }
        }
        x += F::ONE;
    }
}

#[cfg(test)]
mod range_kernel_tests {
    use super::{
        encode_column_block, encode_column_block_range_shard, reed_solomon_column_block,
        reed_solomon_column_block_range_shard, BLOCK_FACTOR,
    };
    use crate::graph::R2lGraph;
    use ff::Field;
    use lcpc_fields::{LazyField, Mont127};

    #[test]
    fn disjoint_expander_and_reed_solomon_ranges_match_full_outputs() {
        let graph = R2lGraph::from_left_edges_fixed_degree(
            3,
            5,
            2,
            &[0u32, 2, 4, 1, 2, 3],
            &[
                Mont127::from(2),
                Mont127::from(3),
                Mont127::from(5),
                Mont127::from(7),
                Mont127::from(11),
                Mont127::from(13),
            ],
        );
        let n_rows = 3;
        let input: Vec<_> = (1..=graph.n_left() * n_rows)
            .map(|v| Mont127::from(v as u64))
            .collect();
        let mut expected = vec![Mont127::ZERO; graph.n_right() * n_rows];
        encode_column_block(&graph, &input, &mut expected, n_rows);

        let mut actual = vec![Mont127::ZERO; graph.n_right() * n_rows];
        let mut scratch = vec![<Mont127 as LazyField>::Acc::default(); n_rows];
        let ranges = [(0, 2), (2, 2), (2, 5)];
        for (start, end) in ranges {
            encode_column_block_range_shard::<Mont127, BLOCK_FACTOR>(
                &graph,
                &input,
                &mut actual[start * n_rows..end * n_rows],
                n_rows,
                start,
                end,
                &mut scratch,
            );
        }
        assert_eq!(actual, expected);

        let rs_input: Vec<_> = (1..=4 * n_rows).map(|v| Mont127::from(v as u64)).collect();
        let n_rs_cols = 7;
        let mut rs_expected = vec![Mont127::ZERO; n_rs_cols * n_rows];
        reed_solomon_column_block(&rs_input, &mut rs_expected, n_rows);

        let mut rs_actual = vec![Mont127::ZERO; n_rs_cols * n_rows];
        for (start, end) in [(0, 3), (3, 3), (3, n_rs_cols)] {
            reed_solomon_column_block_range_shard(
                &rs_input,
                &mut rs_actual[start * n_rows..end * n_rows],
                n_rows,
                start,
                end,
            );
        }
        assert_eq!(rs_actual, rs_expected);
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
