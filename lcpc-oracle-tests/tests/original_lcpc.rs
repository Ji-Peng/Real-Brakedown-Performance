// Copyright 2026 RealBrakedownPerf Authors
// Licensed under Apache-2.0.

//! Differential checks against the unmodified LCPC `f0e2116` crates.

use std::fmt::Debug;

use digest_09::generic_array::{typenum::U32, GenericArray};
use digest_09::{FixedOutputDirty, Reset, Update};
use ff::{Field, PrimeField};
use lcpc_2d::{LcCommit, LcEncoding, SizedField};
use lcpc_brakedown_pc::codespec::CodeParams;
use lcpc_brakedown_pc::{codespec, matgen, SdigEncodingS, SpielmanEncoding};
use lcpc_fields::Mont127;
use lcpc_orig_2d::LcCommit as OriginalCommit;
use lcpc_orig_2d::LcEncoding as OriginalLcEncoding;
use lcpc_orig_brakedown_pc::{
    codespec as original_codespec, SdigEncodingS as OriginalSdigEncodingS,
};
use lcpc_orig_test_fields::ft127::Ft127 as OriginalFt127;
use lcpc_test_fields::{ft127::Ft127, Blake3};
use num_traits::Num;
use rand_chacha_new::ChaCha20Rng as ForkChaCha20Rng;
use rand_chacha_orig::ChaCha20Rng as OriginalChaCha20Rng;
use rand_new::SeedableRng as ForkSeedableRng;
use rand_orig::SeedableRng as OriginalSeedableRng;
use serde::Serialize;

/// Digest 0.9 adapter for the pinned upstream LCPC API.
#[derive(Clone)]
struct OriginalBlake3(blake3::Hasher);

impl Default for OriginalBlake3 {
    fn default() -> Self {
        Self(blake3::Hasher::new())
    }
}

impl Update for OriginalBlake3 {
    fn update(&mut self, data: impl AsRef<[u8]>) {
        self.0.update(data.as_ref());
    }
}

impl Reset for OriginalBlake3 {
    fn reset(&mut self) {
        self.0 = blake3::Hasher::new();
    }
}

impl FixedOutputDirty for OriginalBlake3 {
    type OutputSize = U32;

    fn finalize_into_dirty(&mut self, out: &mut GenericArray<u8, Self::OutputSize>) {
        out.copy_from_slice(self.0.finalize().as_bytes());
    }
}

const INPUT_SIZES: &[(usize, usize)] = &[(2, 32), (3, 64), (4, 128)];
const GRAPH_SEEDS: &[u64] = &[0, 0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210];
const EVALUATION_SEED_SALT: u64 = 0xa5a5_5a5a_c3c3_3c3c;

#[test]
fn fork_fields_match_original_lcpc_mode_a() {
    std::env::set_var("RAYON_NUM_THREADS", "1");

    for &(n_rows, n_per_row) in INPUT_SIZES {
        for &seed in GRAPH_SEEDS {
            check_case::<Ft127, original_codespec::SdigCode3, codespec::SdigCode3>(
                "Ft127/SdigCode3",
                n_rows,
                n_per_row,
                seed,
            );
            check_case::<Ft127, original_codespec::SdigCode6, codespec::SdigCode6>(
                "Ft127/SdigCode6",
                n_rows,
                n_per_row,
                seed,
            );
            check_case::<Mont127, original_codespec::SdigCode3, codespec::SdigCode3>(
                "Mont127/SdigCode3",
                n_rows,
                n_per_row,
                seed,
            );
            check_case::<Mont127, original_codespec::SdigCode6, codespec::SdigCode6>(
                "Mont127/SdigCode6",
                n_rows,
                n_per_row,
                seed,
            );
        }
    }
}

// The fork stores each graph transposed (right to left). The graphs
// sampled by the fork must be exactly upstream's CSC matrices, transposed: same
// dimensions, the same incoming edges per right node, the same weights.
#[test]
fn fork_r2l_graphs_are_transposed_original_graphs() {
    for &n in &[32usize, 100, 1000, 4096] {
        for &seed in GRAPH_SEEDS {
            check_graphs::<original_codespec::SdigCode3, codespec::SdigCode3>(n, seed);
            check_graphs::<original_codespec::SdigCode6, codespec::SdigCode6>(n, seed);
        }
    }
}

fn check_graphs<OriginalSpec, ForkSpec>(n: usize, seed: u64)
where
    OriginalSpec: lcpc_orig_brakedown_pc::codespec::SdigSpecification,
    ForkSpec: codespec::SdigSpecification,
{
    let (orig_pre, orig_post) =
        lcpc_orig_brakedown_pc::matgen::generate::<OriginalFt127, OriginalSpec>(n, seed);
    let (fork_pre, fork_post) = matgen::generate::<Mont127, ForkSpec>(n, seed);
    assert_eq!(orig_pre.len(), fork_pre.len());
    assert_eq!(orig_post.len(), fork_post.len());
    for (orig, fork) in orig_pre
        .iter()
        .zip(&fork_pre)
        .chain(orig_post.iter().zip(&fork_post))
    {
        assert!(orig.is_csc());
        assert_eq!((orig.rows(), orig.cols()), (fork.n_right(), fork.n_left()));
        // transpose upstream's CSC matrix naively: incoming edges per right node,
        // in increasing left-node order
        let mut incoming: Vec<Vec<(u32, Vec<u8>)>> = vec![Vec::new(); orig.rows()];
        for (l, column) in orig.outer_iterator().enumerate() {
            for (r, w) in column.iter() {
                incoming[r].push((l as u32, ff_orig::PrimeField::to_repr(w).as_ref().to_vec()));
            }
        }
        for (r, expect) in incoming.iter().enumerate() {
            let (lefts, weights) = fork.incoming(r);
            let got: Vec<(u32, Vec<u8>)> = lefts
                .iter()
                .zip(weights)
                .map(|(&l, w)| (l, PrimeField::to_repr(w).as_ref().to_vec()))
                .collect();
            assert_eq!(&got, expect, "n={n}, seed={seed:#x}, right node {r}");
        }
    }
}

// Larger shapes, encoded through both the compile-time specification and the
// runtime mode (a) parameters (`SpielmanEncoding` with `CodeParams::Lcpc`).
#[test]
fn fork_runtime_params_match_original_lcpc_mode_a() {
    std::env::set_var("RAYON_NUM_THREADS", "1");
    for &(n_rows, n_per_row, seed) in &[(16usize, 1024usize, 0x5eed_u64), (7, 777, 0x1234)] {
        check_case::<Mont127, original_codespec::SdigCode6, codespec::SdigCode6>(
            "Mont127/SdigCode6",
            n_rows,
            n_per_row,
            seed,
        );
        check_case::<Mont127, original_codespec::SdigCode3, codespec::SdigCode3>(
            "Mont127/SdigCode3",
            n_rows,
            n_per_row,
            seed,
        );
        check_runtime_encoding::<original_codespec::SdigCode6>(
            CodeParams::lcpc_sdig6(),
            n_rows,
            n_per_row,
            seed,
        );
        check_runtime_encoding::<original_codespec::SdigCode3>(
            CodeParams::lcpc_sdig3(),
            n_rows,
            n_per_row,
            seed,
        );
    }
}

fn check_runtime_encoding<OriginalSpec>(
    params: CodeParams,
    n_rows: usize,
    n_per_row: usize,
    seed: u64,
) where
    OriginalSpec: lcpc_orig_brakedown_pc::codespec::SdigSpecification + Debug + Clone + Sync,
    OriginalSdigEncodingS<OriginalFt127, OriginalSpec>: lcpc_orig_2d::LcEncoding<F = OriginalFt127>,
{
    let n_cols = original_output_length::<OriginalSpec>(n_per_row);
    let original_enc = OriginalSdigEncodingS::<OriginalFt127, OriginalSpec>::new_from_dims(
        n_per_row, n_cols, seed,
    );
    let fork_enc = SpielmanEncoding::<Mont127>::with_params(params, n_per_row, seed);
    assert_eq!(fork_enc.get_dims(n_per_row), (1, n_per_row, n_cols));
    assert_eq!(fork_enc.get_n_col_opens(), original_enc.get_n_col_opens());
    assert_eq!(
        fork_enc.get_n_degree_tests(),
        original_enc.get_n_degree_tests()
    );

    let mut rng = OriginalChaCha20Rng::seed_from_u64(seed);
    let mut fork_rng = ForkChaCha20Rng::seed_from_u64(seed);
    let mut original_matrix = vec![<OriginalFt127 as ff_orig::Field>::zero(); n_rows * n_cols];
    let mut fork_matrix = vec![Mont127::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        for col in 0..n_per_row {
            original_matrix[row * n_cols + col] =
                <OriginalFt127 as ff_orig::Field>::random(&mut rng);
            fork_matrix[row * n_cols + col] = <Mont127 as Field>::random(&mut fork_rng);
        }
    }
    for row in original_matrix.chunks_mut(n_cols) {
        original_enc.encode(row).expect("original encode");
    }
    fork_enc
        .encode_rows(&mut fork_matrix, n_cols)
        .expect("fork encode_rows");
    assert_eq!(
        reprs_original(&original_matrix),
        reprs(&fork_matrix),
        "{}: runtime-parameter encoding, rows={n_rows}, n_per_row={n_per_row}",
        params.name()
    );
}

fn check_case<F, OriginalSpec, ForkSpec>(case: &str, n_rows: usize, n_per_row: usize, seed: u64)
where
    F: Field
        + PrimeField
        + lcpc_2d::FieldHash
        + Num
        + SizedField
        + Serialize
        + Debug
        + Clone
        + Send
        + Sync
        + lcpc_fields::LazyField
        + 'static,
    OriginalSpec: lcpc_orig_brakedown_pc::codespec::SdigSpecification + Debug + Clone + Sync,
    ForkSpec: codespec::SdigSpecification + Debug + Clone + Sync,
    OriginalSdigEncodingS<OriginalFt127, OriginalSpec>: lcpc_orig_2d::LcEncoding<F = OriginalFt127>,
    <OriginalSdigEncodingS<OriginalFt127, OriginalSpec> as OriginalLcEncoding>::Err: 'static,
    SdigEncodingS<F, ForkSpec>: LcEncoding<F = F>,
    <SdigEncodingS<F, ForkSpec> as LcEncoding>::Err: 'static,
{
    let context = format!("{case}, rows={n_rows}, n_per_row={n_per_row}, seed={seed:#x}");
    let original_n_cols = original_output_length::<OriginalSpec>(n_per_row);
    let fork_n_cols = fork_output_length::<ForkSpec>(n_per_row);
    assert_eq!(original_n_cols, fork_n_cols, "{context}: code dimensions");
    let n_cols = original_n_cols;

    let original_enc = OriginalSdigEncodingS::<OriginalFt127, OriginalSpec>::new_from_dims(
        n_per_row, n_cols, seed,
    );
    let fork_enc = SdigEncodingS::<F, ForkSpec>::new_from_dims(n_per_row, n_cols, seed);
    assert_eq!(
        original_enc.get_dims(n_per_row),
        fork_enc.get_dims(n_per_row),
        "{context}: encoding dimensions"
    );

    let coefficient_count = n_rows * n_per_row;
    let mut rng = OriginalChaCha20Rng::seed_from_u64(seed);
    let original_coefficients: Vec<OriginalFt127> = (0..coefficient_count)
        .map(|_| <OriginalFt127 as ff_orig::Field>::random(&mut rng))
        .collect();
    let mut fork_rng = ForkChaCha20Rng::seed_from_u64(seed);
    let fork_coefficients: Vec<F> = (0..coefficient_count)
        .map(|_| <F as Field>::random(&mut fork_rng))
        .collect();
    assert_eq!(
        reprs_original(&original_coefficients),
        reprs(&fork_coefficients),
        "{context}: sampled coefficients"
    );

    let mut original_matrix = vec![<OriginalFt127 as ff_orig::Field>::zero(); n_rows * n_cols];
    let mut fork_matrix = vec![F::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        let source_start = row * n_per_row;
        let dest_start = row * n_cols;
        original_matrix[dest_start..dest_start + n_per_row]
            .copy_from_slice(&original_coefficients[source_start..source_start + n_per_row]);
        fork_matrix[dest_start..dest_start + n_per_row]
            .copy_from_slice(&fork_coefficients[source_start..source_start + n_per_row]);
    }
    for row in 0..n_rows {
        original_enc
            .encode(&mut original_matrix[row * n_cols..(row + 1) * n_cols])
            .unwrap_or_else(|error| panic!("{context}: original encode failed: {error}"));
        fork_enc
            .encode(&mut fork_matrix[row * n_cols..(row + 1) * n_cols])
            .unwrap_or_else(|error| panic!("{context}: fork encode failed: {error}"));
    }
    assert_eq!(
        reprs_original(&original_matrix),
        reprs(&fork_matrix),
        "{context}: encoded matrix"
    );

    let original_commit =
        OriginalCommit::<OriginalBlake3, _>::commit(&original_coefficients, &original_enc)
            .unwrap_or_else(|error| panic!("{context}: original commit failed: {error}"));
    let fork_commit = LcCommit::<Blake3, _>::commit(&fork_coefficients, &fork_enc)
        .unwrap_or_else(|error| panic!("{context}: fork commit failed: {error}"));
    let original_root = original_commit.get_root();
    let fork_root = fork_commit.get_root();
    assert_eq!(
        original_root.as_ref().as_slice(),
        fork_root.as_ref().as_slice(),
        "{context}: commitment root"
    );

    let evaluation_seed = seed ^ EVALUATION_SEED_SALT;
    let mut point_rng = OriginalChaCha20Rng::seed_from_u64(evaluation_seed);
    let original_point = <OriginalFt127 as ff_orig::Field>::random(&mut point_rng);
    let mut fork_point_rng = ForkChaCha20Rng::seed_from_u64(evaluation_seed);
    let fork_point = <F as Field>::random(&mut fork_point_rng);
    assert_eq!(
        reprs_original(&[original_point]),
        reprs(&[fork_point]),
        "{context}: sampled evaluation point"
    );
    let (original_inner, original_outer) = tensors_original(original_point, n_per_row, n_rows);
    let (fork_inner, fork_outer) = tensors(fork_point, n_per_row, n_rows);
    assert_eq!(
        reprs_original(&original_inner),
        reprs(&fork_inner),
        "{context}: inner tensor"
    );
    assert_eq!(
        reprs_original(&original_outer),
        reprs(&fork_outer),
        "{context}: outer tensor"
    );

    let mut original_prove_transcript = original_transcript(
        original_root.as_ref().as_slice(),
        original_enc.get_n_col_opens(),
    );
    let original_proof = original_commit
        .prove(
            &original_outer,
            &original_enc,
            &mut original_prove_transcript,
        )
        .unwrap_or_else(|error| panic!("{context}: original prove failed: {error}"));
    let original_proof_bytes =
        bincode::serialize(&original_proof).expect("serialize original proof");

    let mut fork_prove_transcript =
        fork_transcript(fork_root.as_ref().as_slice(), fork_enc.get_n_col_opens());
    let fork_proof = fork_commit
        .prove(&fork_outer, &fork_enc, &mut fork_prove_transcript)
        .unwrap_or_else(|error| panic!("{context}: fork prove failed: {error}"));
    let fork_proof_bytes = bincode::serialize(&fork_proof).expect("serialize fork proof");
    assert_eq!(
        original_proof_bytes, fork_proof_bytes,
        "{context}: proof bytes"
    );

    let original_verify_enc = OriginalSdigEncodingS::<OriginalFt127, OriginalSpec>::new_from_dims(
        n_per_row, n_cols, seed,
    );
    let fork_verify_enc = SdigEncodingS::<F, ForkSpec>::new_from_dims(n_per_row, n_cols, seed);
    let mut original_verify_transcript = original_transcript(
        original_root.as_ref().as_slice(),
        original_verify_enc.get_n_col_opens(),
    );
    let original_value = original_proof
        .verify(
            original_root.as_ref(),
            &original_outer,
            &original_inner,
            &original_verify_enc,
            &mut original_verify_transcript,
        )
        .unwrap_or_else(|error| panic!("{context}: original verification failed: {error}"));
    let mut fork_verify_transcript = fork_transcript(
        fork_root.as_ref().as_slice(),
        fork_verify_enc.get_n_col_opens(),
    );
    let fork_value = fork_proof
        .verify(
            fork_root.as_ref(),
            &fork_outer,
            &fork_inner,
            &fork_verify_enc,
            &mut fork_verify_transcript,
        )
        .unwrap_or_else(|error| panic!("{context}: fork verification failed: {error}"));
    assert_eq!(
        reprs_original(&[original_value]),
        reprs(&[fork_value]),
        "{context}: verified evaluation"
    );
}

fn reprs<F: PrimeField>(values: &[F]) -> Vec<Vec<u8>> {
    values
        .iter()
        .map(|value| <F as PrimeField>::to_repr(value).as_ref().to_vec())
        .collect()
}

fn reprs_original<F: ff_orig::PrimeField>(values: &[F]) -> Vec<Vec<u8>> {
    values
        .iter()
        .map(|value| <F as ff_orig::PrimeField>::to_repr(value).as_ref().to_vec())
        .collect()
}

fn original_output_length<S: original_codespec::SdigSpecification>(n_per_row: usize) -> usize {
    output_length_from_parameters(
        n_per_row,
        S::baselen(),
        S::alpha_num(),
        S::alpha_den(),
        S::r_num(),
        S::r_den(),
    )
}

fn fork_output_length<S: codespec::SdigSpecification>(n_per_row: usize) -> usize {
    output_length_from_parameters(
        n_per_row,
        S::baselen(),
        S::alpha_num(),
        S::alpha_den(),
        S::r_num(),
        S::r_den(),
    )
}

fn output_length_from_parameters(
    n_per_row: usize,
    baselen: usize,
    alpha_num: usize,
    alpha_den: usize,
    r_num: usize,
    r_den: usize,
) -> usize {
    let mut levels = Vec::new();
    let mut n = n_per_row;
    while n > baselen {
        let next = ceil_muldiv(n, alpha_num, alpha_den);
        levels.push((n, next));
        n = next;
    }
    let post_dims: Vec<_> = levels
        .iter()
        .map(|&(ni, mi)| {
            let nip = ceil_muldiv(mi, r_num, r_den);
            let mip = ceil_muldiv(ni, r_num, r_den) - ni - nip;
            (nip, mip)
        })
        .collect();
    n_per_row
        + post_dims.last().expect("input dimension exceeds baselen").0
        + levels
            .iter()
            .take(levels.len() - 1)
            .map(|(_, mi)| *mi)
            .sum::<usize>()
        + post_dims.iter().map(|(_, mip)| *mip).sum::<usize>()
}

fn ceil_muldiv(n: usize, numerator: usize, denominator: usize) -> usize {
    (n * numerator + denominator - 1) / denominator
}

fn tensors<F: Field>(point: F, n_per_row: usize, n_rows: usize) -> (Vec<F>, Vec<F>) {
    let mut inner = Vec::with_capacity(n_per_row);
    let mut value = F::ONE;
    for _ in 0..n_per_row {
        inner.push(value);
        value *= point;
    }
    let row_step = value;
    let mut outer = Vec::with_capacity(n_rows);
    value = F::ONE;
    for _ in 0..n_rows {
        outer.push(value);
        value *= row_step;
    }
    (inner, outer)
}

fn tensors_original<F: ff_orig::Field>(
    point: F,
    n_per_row: usize,
    n_rows: usize,
) -> (Vec<F>, Vec<F>) {
    let mut inner = Vec::with_capacity(n_per_row);
    let mut value = F::one();
    for _ in 0..n_per_row {
        inner.push(value);
        value *= point;
    }
    let row_step = value;
    let mut outer = Vec::with_capacity(n_rows);
    value = F::one();
    for _ in 0..n_rows {
        outer.push(value);
        value *= row_step;
    }
    (inner, outer)
}

fn original_transcript(root: &[u8], n_col_opens: usize) -> merlin_orig::Transcript {
    let mut transcript = merlin_orig::Transcript::new(b"LCPC original baseline");
    transcript.append_message(b"polycommit", root);
    transcript.append_message(b"ncols", &(n_col_opens as u64).to_be_bytes());
    transcript
}

fn fork_transcript(root: &[u8], n_col_opens: usize) -> merlin::Transcript {
    let mut transcript = merlin::Transcript::new(b"LCPC original baseline");
    transcript.append_message(b"polycommit", root);
    transcript.append_message(b"ncols", &(n_col_opens as u64).to_be_bytes());
    transcript
}
