// Copyright 2026 RealBrakedownPerf Authors
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Lazy linear-combination kernels over [`LazyField`] for PCS row collapse and
//! column inner products.

use lcpc_fields::LazyField;

/// Column tile width used by [`collapse_rows_lazy`].
///
/// With 16-byte field elements and 32-40 byte accumulators, a 256-column tile
/// reads `4 * 4 KiB = 16 KiB` of row inputs across a 4-row group and updates
/// at most `10 KiB` of accumulators per tensor, staying comfortably in L1d.
const COL_TILE: usize = 256;

/// Add row-wise linear combinations of a row-major `rows` matrix into `polys`
/// using unreduced [`LazyField`] accumulation and 4-row blocking.
///
/// `rows` has length `n_rows * k` (a multiple of `k`). Each `tensors[t]` holds
/// `n_rows` weights and each `polys[t]` has length `k`; on return,
/// `polys[t][j] += sum_i tensors[t][i] * rows[i * k + j]`.
///
/// Kept `#[inline(never)]` so `scripts/check_codegen.py` can inspect the
/// monomorphized scalar assembly per field.
#[inline(never)]
pub fn collapse_rows_lazy<F: LazyField>(
    rows: &[F],
    k: usize,
    tensors: &[&[F]],
    polys: &mut [Vec<F>],
) {
    assert_eq!(tensors.len(), polys.len());
    let nt = tensors.len();
    if k == 0 {
        assert!(rows.is_empty());
        for poly in polys.iter() {
            assert_eq!(poly.len(), 0);
        }
        return;
    }
    assert_eq!(rows.len() % k, 0);
    let n_rows = rows.len() / k;
    for (tensor, poly) in tensors.iter().zip(polys.iter()) {
        assert_eq!(tensor.len(), n_rows);
        assert_eq!(poly.len(), k);
    }
    if nt == 0 || n_rows == 0 {
        return;
    }

    let pw: Vec<Vec<F>> = tensors
        .iter()
        .map(|t| t.iter().map(F::prepare_weight).collect())
        .collect();
    let mut acc = vec![F::Acc::default(); nt * k];
    let mut since_fold = 0usize;

    let mut i = 0usize;
    if F::MAX_ACCUMULATIONS > 4 {
        while i + 4 <= n_rows {
            if since_fold + 4 > F::MAX_ACCUMULATIONS {
                for a in acc.iter_mut() {
                    F::fold(a);
                }
                since_fold = 1;
            }

            let x0 = &rows[i * k..][..k];
            let x1 = &rows[(i + 1) * k..][..k];
            let x2 = &rows[(i + 2) * k..][..k];
            let x3 = &rows[(i + 3) * k..][..k];

            for j0 in (0..k).step_by(COL_TILE) {
                let j1 = (j0 + COL_TILE).min(k);
                let s0 = &x0[j0..j1];
                let s1 = &x1[j0..j1];
                let s2 = &x2[j0..j1];
                let s3 = &x3[j0..j1];

                for t in 0..nt {
                    let &[w0, w1, w2, w3] = &pw[t][i..i + 4] else {
                        unreachable!()
                    };
                    let a = &mut acc[t * k + j0..t * k + j1];
                    let s0 = &s0[..a.len()];
                    let s1 = &s1[..a.len()];
                    let s2 = &s2[..a.len()];
                    let s3 = &s3[..a.len()];
                    for j in 0..a.len() {
                        let mut v = a[j];
                        F::mac(&mut v, &w0, &s0[j]);
                        F::mac(&mut v, &w1, &s1[j]);
                        F::mac(&mut v, &w2, &s2[j]);
                        F::mac(&mut v, &w3, &s3[j]);
                        a[j] = v;
                    }
                }
            }

            since_fold += 4;
            i += 4;
        }
    }

    while i < n_rows {
        if since_fold + 1 > F::MAX_ACCUMULATIONS {
            for a in acc.iter_mut() {
                F::fold(a);
            }
            since_fold = 1;
        }

        let x0 = &rows[i * k..][..k];
        for j0 in (0..k).step_by(COL_TILE) {
            let j1 = (j0 + COL_TILE).min(k);
            let s0 = &x0[j0..j1];
            for t in 0..nt {
                let w0 = pw[t][i];
                let a = &mut acc[t * k + j0..t * k + j1];
                let s0 = &s0[..a.len()];
                for j in 0..a.len() {
                    let mut v = a[j];
                    F::mac(&mut v, &w0, &s0[j]);
                    a[j] = v;
                }
            }
        }

        since_fold += 1;
        i += 1;
    }

    for (t, poly) in polys.iter_mut().enumerate() {
        let a = &acc[t * k..(t + 1) * k];
        for (p, &v) in poly.iter_mut().zip(a.iter()) {
            *p += F::reduce(v);
        }
    }
}

/// Compute the inner product of `w` and `x` over their common prefix using
/// unreduced [`LazyField`] accumulation.
///
/// Accumulates unprepared weights `w[i]` directly via [`LazyField::mac`] (folding
/// between chunks of `MAX_ACCUMULATIONS - 1` elements when finite) and applies a
/// single [`LazyField::prepare_weight`] after [`LazyField::reduce`] to cancel the
/// field-constant factor.
///
/// Kept `#[inline(never)]` so `scripts/check_codegen.py` can inspect the
/// monomorphized scalar assembly per field.
#[inline(never)]
pub fn dot_lazy<F: LazyField>(w: &[F], x: &[F]) -> F {
    let n = w.len().min(x.len());
    let w = &w[..n];
    let x = &x[..n];
    let mut acc = F::Acc::default();

    if F::MAX_ACCUMULATIONS == usize::MAX {
        for (wi, xi) in w.iter().zip(x.iter()) {
            F::mac(&mut acc, wi, xi);
        }
    } else {
        let chunk_len = F::MAX_ACCUMULATIONS - 1;
        assert!(chunk_len >= 1);
        for (ci, (wc, xc)) in w.chunks(chunk_len).zip(x.chunks(chunk_len)).enumerate() {
            if ci > 0 {
                F::fold(&mut acc);
            }
            for (wi, xi) in wc.iter().zip(xc.iter()) {
                F::mac(&mut acc, wi, xi);
            }
        }
    }

    F::prepare_weight(&F::reduce(acc))
}
