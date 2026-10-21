// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! The fork's own encoder (since `step/04-r2l`) against SIMD-Spielman's `tv/` test vectors.
//!
//! Mode (b) (`CodeParams::SimdSpielman`) uses SIMD-Spielman's code parameters. Its graphs
//! are reproduced exactly by the test-only glibc `random()` sampler of
//! `lcpc-spielman-oracle` (seed 4567, upstream's call order) and fed to the fork through
//! its external-graph entry points: the fork's own counting-sort transpose
//! (`R2lGraph::from_left_edges_fixed_degree`), `ExpanderCode::from_graphs`, and the
//! batched column-major R2L encoder (`encode_col_major`, `LcEncoding::encode_rows`,
//! `LcEncoding::encode`). The fork's own mode-(b) dimension recurrence
//! (`matgen::simd_spielman_dims`) must also agree with upstream's sampled graphs.
//!
//! Upstream's output files hold `2 * k` columns per row; positions past the codeword
//! length are zero (upstream allocates and never writes them).

use lcpc_2d::LcEncoding;
use lcpc_brakedown_pc::codespec::CodeParams;
use lcpc_brakedown_pc::encode::{encode_col_major, BaseCase, ExpanderCode};
use lcpc_brakedown_pc::graph::R2lGraph;
use lcpc_brakedown_pc::matgen::simd_spielman_dims;
use lcpc_brakedown_pc::SpielmanEncoding;
use lcpc_fields::{Fp2M61, Mers127, Mont127};
use lcpc_spielman_oracle::{
    load_tv_matrix, locate_tv_dir, tv_filenames, L2rExpanderGraph, MatrixShape, SpielmanOracleCode,
    SpielmanTvField, SIMD_SPIELMAN_TV_DIR_ENV, SIMD_SPIELMAN_TV_SEED,
};

// The fork's transpose of an upstream (L2R, fixed-degree) graph.
fn fork_graph<F: Copy>(g: &L2rExpanderGraph<F>) -> R2lGraph<F> {
    R2lGraph::from_left_edges_fixed_degree(
        g.n_left,
        g.n_right,
        g.left_degree,
        &g.connections,
        &g.weights,
    )
}

fn check_tv<F: SpielmanTvField>(shape: MatrixShape, log_n: u32) {
    let tv_dir = locate_tv_dir().unwrap_or_else(|| {
        panic!(
            "required SIMD-Spielman tv/ directory not found; set {SIMD_SPIELMAN_TV_DIR_ENV}"
        )
    });
    let (n_rows, k) = shape.dims(log_n);
    let (in_name, out_name) = tv_filenames::<F>(shape, log_n);
    let input = load_tv_matrix::<F>(&tv_dir.join(&in_name), n_rows, k)
        .unwrap_or_else(|e| panic!("cannot read {in_name}: {e}"));
    let expected = load_tv_matrix::<F>(&tv_dir.join(&out_name), n_rows, 2 * k)
        .unwrap_or_else(|e| panic!("cannot read {out_name}: {e}"));

    // upstream's graphs, sampled by the test-only glibc sampler
    let oracle = SpielmanOracleCode::<F>::sample_tv(k, SIMD_SPIELMAN_TV_SEED);

    // the fork's mode-(b) dimensions are upstream's
    let dims = simd_spielman_dims(k);
    assert_eq!(
        dims.len(),
        oracle.num_levels(),
        "{out_name}: recursion depth"
    );
    for (i, lvl) in dims.iter().enumerate() {
        let (g1, g2) = (&oracle.graphs1_l2r[i], &oracle.graphs2_l2r[i]);
        assert_eq!(
            (lvl.n, lvl.m, lvl.c),
            (g1.n_left, g1.n_right, g1.left_degree),
            "{out_name}: precode {i}"
        );
        assert_eq!(
            (lvl.n_prime, lvl.m_prime, lvl.d),
            (g2.n_left, g2.n_right, g2.left_degree),
            "{out_name}: postcode {i}"
        );
    }

    // the fork's O(R + E) transpose agrees with the oracle's R2L graphs
    let precodes: Vec<_> = oracle.graphs1_l2r.iter().map(fork_graph).collect();
    let postcodes: Vec<_> = oracle.graphs2_l2r.iter().map(fork_graph).collect();
    for (fork, orc) in precodes
        .iter()
        .chain(&postcodes)
        .zip(oracle.graphs1_r2l.iter().chain(&oracle.graphs2_r2l))
    {
        assert_eq!(fork.row_ptr(), &orc.row_ptr[..], "{out_name}: row_ptr");
        assert_eq!(fork.col_idx(), &orc.col_idx[..], "{out_name}: col_idx");
        assert_eq!(fork.weights(), &orc.weights[..], "{out_name}: weights");
    }

    let code = ExpanderCode::from_graphs(BaseCase::Identity, precodes, postcodes);
    let n_cols = code.codeword_len();
    assert_eq!(n_cols, oracle.codeword_len, "{out_name}: codeword length");
    assert!(n_cols <= 2 * k);

    let check_rows = |got: &[F], what: &str| {
        for row in 0..n_rows {
            let exp = &expected[row * 2 * k..(row + 1) * 2 * k];
            let got = &got[row * n_cols..(row + 1) * n_cols];
            if let Some(col) = (0..n_cols).find(|&c| got[c] != exp[c]) {
                let (e0, e1) = exp[col].to_tv_words();
                let (g0, g1) = got[col].to_tv_words();
                panic!(
                    "{out_name} ({what}): row {row} col {col}: expected {e0:016x} {e1:016x}, \
                     got {g0:016x} {g1:016x}"
                );
            }
            assert!(
                exp[n_cols..].iter().all(|v| *v == F::ZERO),
                "{out_name}: nonzero padding past the codeword in row {row}"
            );
        }
    };

    // row-major matrix with the messages; the rest is garbage that must be overwritten
    let mut rows = vec![F::ONE; n_rows * n_cols];
    for row in 0..n_rows {
        rows[row * n_cols..row * n_cols + k].copy_from_slice(&input[row * k..(row + 1) * k]);
    }

    // 1. the direct column-major kernel
    let mut buf = vec![F::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        for col in 0..n_cols {
            buf[col * n_rows + row] = rows[row * n_cols + col];
        }
    }
    encode_col_major(&mut buf, n_rows, &code);
    let mut from_buf = vec![F::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        for col in 0..n_cols {
            from_buf[row * n_cols + col] = buf[col * n_rows + row];
        }
    }
    check_rows(&from_buf, "encode_col_major");

    // 2. the PCS entry points: batched encode_rows (commit) and single-row encode (verify)
    let enc = SpielmanEncoding::<F>::from_code(CodeParams::SimdSpielman, code);
    assert!(enc.dims_ok(k, n_cols));
    let mut single = rows.clone();
    enc.encode_rows(&mut rows, n_cols).expect("encode_rows");
    check_rows(&rows, "encode_rows");
    for row in single.chunks_mut(n_cols) {
        enc.encode(row).expect("encode");
    }
    check_rows(&single, "encode");
}

macro_rules! tv_tests {
    ($($name:ident: $field:ty, $shape:ident, $log_n:literal;)*) => {
        $(
            #[test]
            fn $name() {
                check_tv::<$field>(MatrixShape::$shape, $log_n);
            }
        )*
    };
}

tv_tests! {
    fork_tv_brakedown_montgomery_n12: Mont127, Brakedown, 12;
    fork_tv_brakedown_montgomery_n14: Mont127, Brakedown, 14;
    fork_tv_brakedown_montgomery_n16: Mont127, Brakedown, 16;
    fork_tv_brakedown_mersenne_n12: Mers127, Brakedown, 12;
    fork_tv_brakedown_mersenne_n14: Mers127, Brakedown, 14;
    fork_tv_brakedown_mersenne_n16: Mers127, Brakedown, 16;
    fork_tv_brakedown_extension_n12: Fp2M61, Brakedown, 12;
    fork_tv_brakedown_extension_n14: Fp2M61, Brakedown, 14;
    fork_tv_brakedown_extension_n16: Fp2M61, Brakedown, 16;
    fork_tv_orion_montgomery_n12: Mont127, Orion, 12;
    fork_tv_orion_montgomery_n14: Mont127, Orion, 14;
    fork_tv_orion_montgomery_n16: Mont127, Orion, 16;
    fork_tv_orion_mersenne_n12: Mers127, Orion, 12;
    fork_tv_orion_mersenne_n14: Mers127, Orion, 14;
    fork_tv_orion_mersenne_n16: Mers127, Orion, 16;
    fork_tv_orion_extension_n12: Fp2M61, Orion, 12;
    fork_tv_orion_extension_n14: Fp2M61, Orion, 14;
    fork_tv_orion_extension_n16: Fp2M61, Orion, 16;
}
