// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

use super::codespec::{CodeParams, LcpcParams};
use super::graph::R2lGraph;
use super::{
    BrakedownCommit, Fp2M61, LazyField, Mers127, Mont127, SdigEncoding, SdigEncodingS,
    SpielmanEncoding,
};

use ff::{Field, PrimeField};
use itertools::iterate;
use lcpc_2d::{FieldHash, LcCommit, LcEncoding, SizedField};
use lcpc_test_fields::{ft127::*, ft255::*, ft63::*, random_coeffs, Blake3};
use merlin::Transcript;
use ndarray::{linalg::Dot, Array};
use num_traits::Num;
use rand::{rng, RngExt, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::Deserialize;
use sprs::{CsMat, MulAcc, TriMat};
use std::iter::repeat_with;
use std::path::Path;

#[test]
fn col_opens() {
    use super::codespec::{SdigCode1, SdigCode2, SdigCode3, SdigCode4, SdigCode5, SdigCode6};

    println!("1: {}", SdigEncodingS::<Ft255, SdigCode1>::_n_col_opens());
    println!("2: {}", SdigEncodingS::<Ft255, SdigCode2>::_n_col_opens());
    println!("3: {}", SdigEncodingS::<Ft255, SdigCode3>::_n_col_opens());
    println!("4: {}", SdigEncodingS::<Ft255, SdigCode4>::_n_col_opens());
    println!("5: {}", SdigEncodingS::<Ft255, SdigCode5>::_n_col_opens());
    println!("6: {}", SdigEncodingS::<Ft255, SdigCode6>::_n_col_opens());
}

#[test]
fn sprs_playground() {
    let mut rng = rng();
    let n_rows = 65537;
    let n_cols = 32749;

    let m: CsMat<_> = {
        let mut tmp = TriMat::new((n_rows, n_cols));
        for i in 0..n_rows {
            let col1 = rng.random_range(0..n_cols);
            let col2 = {
                let mut tmp = rng.random_range(0..n_cols);
                while tmp == col1 {
                    tmp = rng.random_range(0..n_cols);
                }
                tmp
            };
            tmp.add_triplet(i, col1, Ft63::random(&mut rng));
            tmp.add_triplet(i, col2, Ft63::random(&mut rng));
        }
        // to_csr appears to be considerably faster than to_csc
        // (note that because of the transpose, we end up with csc in the end)
        tmp.to_csr().transpose_into()
    };

    let v = {
        let mut tmp = Vec::with_capacity(n_rows);
        for _ in 0..n_rows {
            tmp.push(Ft63::random(&mut rng));
        }
        Array::from(tmp)
    };

    let mut t = Ft63::ZERO;
    for i in 0..10 {
        let mv = m.dot(&v);
        t += mv[i % n_cols];
    }
    println!("{:?}", t);
}

#[test]
fn test_matgen_encode() {
    let mut rng = rng();
    use super::codespec::SdigCode3 as TestCode;
    use super::encode::encode;
    use super::matgen::generate_code;

    let n = 256usize + ((rng.random::<u64>() as usize) % 4096);
    let code = generate_code::<Ft63>(&CodeParams::Lcpc(LcpcParams::of::<TestCode>()), n, 0u64);

    let xi_len = code.codeword_len();
    let mut xi = Vec::with_capacity(xi_len);
    for _ in 0..xi_len {
        xi.push(Ft63::random(&mut rng));
    }
    encode(&mut xi, &code);
}

const N_ITERS: usize = 10;
#[test]
#[ignore]
fn prove_verify_size_bench() {
    use super::codespec::SdigCode3 as TestCode;
    use std::time::Instant;

    for lgl in (13..=29).step_by(2) {
        // commit to random poly of specified size
        let coeffs = random_coeffs(lgl);
        let enc = SdigEncodingS::<Ft255, TestCode>::new(coeffs.len(), 0);
        let comm = LcCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
        let root = comm.get_root();

        // evaluate the random polynomial we just generated at a random point x
        let x = Ft255::random(&mut rand::rng());

        // compute the outer and inner tensors for powers of x
        // NOTE: we treat coeffs as a univariate polynomial, but it doesn't
        // really matter --- the only difference from a multilinear is the
        // way we compute outer_tensor and inner_tensor from the eval point
        let inner_tensor: Vec<Ft255> = iterate(Ft255::ONE, |&v| v * x)
            .take(comm.get_n_per_row())
            .collect();
        let outer_tensor: Vec<Ft255> = {
            let xr = x * inner_tensor.last().unwrap();
            iterate(Ft255::ONE, |&v| v * xr)
                .take(comm.get_n_rows())
                .collect()
        };

        let mut xxx = 0u8;
        let now = Instant::now();
        for i in 0..N_ITERS {
            let mut tr = Transcript::new(b"test transcript");
            tr.append_message(b"polycommit", root.as_ref());
            tr.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
            let pf = comm.prove(&outer_tensor[..], &enc, &mut tr).unwrap();
            let encoded: Vec<u8> = bincode::serialize(&pf).unwrap();
            xxx ^= encoded[i];
        }
        let pf_dur = now.elapsed().as_nanos() / N_ITERS as u128;

        let mut tr = Transcript::new(b"test transcript");
        tr.append_message(b"polycommit", root.as_ref());
        tr.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
        let pf = comm.prove(&outer_tensor[..], &enc, &mut tr).unwrap();
        let encoded: Vec<u8> = bincode::serialize(&pf).unwrap();
        let len = encoded.len();

        let now = Instant::now();
        for i in 0..N_ITERS {
            let mut tr = Transcript::new(b"test transcript");
            tr.append_message(b"polycommit", root.as_ref());
            tr.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
            xxx ^= pf
                .verify(
                    root.as_ref(),
                    &outer_tensor[..],
                    &inner_tensor[..],
                    &enc,
                    &mut tr,
                )
                .unwrap()
                .to_repr()
                .as_ref()[i];
        }
        let vf_dur = now.elapsed().as_nanos() / N_ITERS as u128;

        println!("{}: {} {} {} {}", lgl, pf_dur, vf_dur, len, xxx);
    }
}

#[test]
#[ignore]
fn rough_bench() {
    use super::codespec::SdigCode3 as TestCode;
    use std::time::Instant;

    for lgl in (13..=29).step_by(2) {
        // commit to random poly of specified size
        let coeffs = random_coeffs(lgl);
        let enc = SdigEncodingS::<Ft255, TestCode>::new(coeffs.len(), 0);
        let mut xxx = 0u8;

        let now = Instant::now();
        for i in 0..N_ITERS {
            let comm = LcCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
            let root = comm.get_root();
            xxx ^= root.as_ref()[i];
        }
        let dur = now.elapsed().as_nanos() / N_ITERS as u128;
        println!("{}: {} {:?}", lgl, dur, xxx);
    }
}

#[test]
fn end_to_end_one_proof() {
    // commit to a random polynomial at a random rate
    let coeffs = get_random_coeffs();
    let enc = SdigEncoding::new(coeffs.len(), 0);
    let comm = BrakedownCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
    // this is the polynomial commitment
    let root = comm.get_root();

    // evaluate the random polynomial we just generated at a random point x
    let x = Ft63::random(&mut rand::rng());

    // compute the outer and inner tensors for powers of x
    // NOTE: we treat coeffs as a univariate polynomial, but it doesn't
    // really matter --- the only difference from a multilinear is the
    // way we compute outer_tensor and inner_tensor from the eval point
    let inner_tensor: Vec<Ft63> = iterate(Ft63::ONE, |&v| v * x)
        .take(comm.get_n_per_row())
        .collect();
    let outer_tensor: Vec<Ft63> = {
        let xr = x * inner_tensor.last().unwrap();
        iterate(Ft63::ONE, |&v| v * xr)
            .take(comm.get_n_rows())
            .collect()
    };

    // compute an evaluation proof
    let mut tr1 = Transcript::new(b"test transcript");
    tr1.append_message(b"polycommit", root.as_ref());
    tr1.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let pf = comm.prove(&outer_tensor[..], &enc, &mut tr1).unwrap();

    // verify it and finish evaluation
    let mut tr2 = Transcript::new(b"test transcript");
    tr2.append_message(b"polycommit", root.as_ref());
    tr2.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let enc2 = SdigEncoding::new_from_dims(pf.get_n_per_row(), pf.get_n_cols(), 0);
    pf.verify(
        root.as_ref(),
        &outer_tensor[..],
        &inner_tensor[..],
        &enc2,
        &mut tr2,
    )
    .unwrap();
}

#[test]
fn end_to_end_one_proof_ml() {
    let mut rng = rand::rng();

    // commit to a random polynomial at a random rate
    let lgl = 5 + ((rng.random::<u64>() as usize) % 16);
    let coeffs = random_coeffs(lgl);
    let enc = SdigEncoding::new_ml(lgl, 0);
    let comm = BrakedownCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
    // this is the polynomial commitment
    let root = comm.get_root();
    assert_eq!(1 << lgl, comm.get_n_rows() * comm.get_n_per_row());

    // evaluate the random polynomial we just generated at a random point x
    let x = Ft63::random(&mut rand::rng());

    // compute the outer and inner tensors for powers of x
    // NOTE: we treat coeffs as a univariate polynomial, but it doesn't
    // really matter --- the only difference from a multilinear is the
    // way we compute outer_tensor and inner_tensor from the eval point
    let inner_tensor: Vec<Ft63> = iterate(Ft63::ONE, |&v| v * x)
        .take(comm.get_n_per_row())
        .collect();
    let outer_tensor: Vec<Ft63> = {
        let xr = x * inner_tensor.last().unwrap();
        iterate(Ft63::ONE, |&v| v * xr)
            .take(comm.get_n_rows())
            .collect()
    };

    // compute an evaluation proof
    let mut tr1 = Transcript::new(b"test transcript");
    tr1.append_message(b"polycommit", root.as_ref());
    tr1.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let pf = comm.prove(&outer_tensor[..], &enc, &mut tr1).unwrap();

    // verify it and finish evaluation
    let mut tr2 = Transcript::new(b"test transcript");
    tr2.append_message(b"polycommit", root.as_ref());
    tr2.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let enc2 = SdigEncoding::new_from_dims(pf.get_n_per_row(), pf.get_n_cols(), 0);
    pf.verify(
        root.as_ref(),
        &outer_tensor[..],
        &inner_tensor[..],
        &enc2,
        &mut tr2,
    )
    .unwrap();
}

#[test]
fn end_to_end_two_proofs() {
    // commit to a random polynomial at a random rate
    let coeffs = get_random_coeffs();
    let enc = SdigEncoding::new(coeffs.len(), 1);
    let comm = BrakedownCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
    // this is the polynomial commitment
    let root = comm.get_root();

    // evaluate the random polynomial we just generated at a random point x
    let x = Ft63::random(&mut rand::rng());

    // compute the outer and inner tensors for powers of x
    // NOTE: we treat coeffs as a univariate polynomial, but it doesn't
    // really matter --- the only difference from a multilinear is the
    // way we compute outer_tensor and inner_tensor from the eval point
    let inner_tensor: Vec<Ft63> = iterate(Ft63::ONE, |&v| v * x)
        .take(comm.get_n_per_row())
        .collect();
    let outer_tensor: Vec<Ft63> = {
        let xr = x * inner_tensor.last().unwrap();
        iterate(Ft63::ONE, |&v| v * xr)
            .take(comm.get_n_rows())
            .collect()
    };

    // compute an evaluation proof
    let mut tr1 = Transcript::new(b"test transcript");
    tr1.append_message(b"polycommit", root.as_ref());
    tr1.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let pf = comm.prove(&outer_tensor[..], &enc, &mut tr1).unwrap();

    let challenge_after_first_proof_prover = {
        let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
        tr1.challenge_bytes(b"ligero-pc//challenge", &mut key);
        let mut deg_test_rng = ChaCha20Rng::from_seed(key);
        Ft63::random(&mut deg_test_rng)
    };

    // produce a second proof with the same transcript
    tr1.append_message(b"polycommit", root.as_ref());
    tr1.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let pf2 = comm.prove(&outer_tensor[..], &enc, &mut tr1).unwrap();

    // verify it and finish evaluation
    let mut tr2 = Transcript::new(b"test transcript");
    tr2.append_message(b"polycommit", root.as_ref());
    tr2.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let enc2 = SdigEncoding::new_from_dims(pf.get_n_per_row(), pf.get_n_cols(), 1);
    let res = pf
        .verify(
            root.as_ref(),
            &outer_tensor[..],
            &inner_tensor[..],
            &enc2,
            &mut tr2,
        )
        .unwrap();

    let challenge_after_first_proof_verifier = {
        let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
        tr2.challenge_bytes(b"ligero-pc//challenge", &mut key);
        let mut deg_test_rng = ChaCha20Rng::from_seed(key);
        Ft63::random(&mut deg_test_rng)
    };
    assert_eq!(
        challenge_after_first_proof_prover,
        challenge_after_first_proof_verifier
    );

    // second proof verification with the same transcript
    tr2.append_message(b"polycommit", root.as_ref());
    tr2.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes()[..]);
    let enc3 = SdigEncoding::new_from_dims(pf2.get_n_per_row(), pf2.get_n_cols(), 1);
    let res2 = pf2
        .verify(
            root.as_ref(),
            &outer_tensor[..],
            &inner_tensor[..],
            &enc3,
            &mut tr2,
        )
        .unwrap();

    assert_eq!(res, res2);
}

fn get_random_coeffs() -> Vec<Ft63> {
    let mut rng = rand::rng();

    let lgl = 16 + ((rng.random::<u64>() as usize) % 8);
    let len_base = 1 << (lgl - 1);
    let len = len_base + ((rng.random::<u64>() as usize) % len_base);

    repeat_with(|| Ft63::random(&mut rng)).take(len).collect()
}

// =============================================================================
// Bit-Exact Golden Vector Regression Tests against Original LCPC (f0e2116)
// =============================================================================

#[derive(Debug, Deserialize)]
struct GoldenFile {
    schema_version: String,
    generator: String,
    upstream_commit: String,
    hash_algorithm: String,
    hash_crate_version: String,
    cases: Vec<GoldenCase>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct GoldenCase {
    case_id: String,
    params: String,
    field: String,
    seed: u64,
    evaluation_seed: u64,
    n_rows: usize,
    n_per_row: usize,
    n_cols: usize,
    coefficients_hex: Vec<String>,
    encoded_rows_hex: Vec<Vec<String>>,
    commitment_root_hex: String,
    proof_bincode_blake3_hex: String,
    proof_bincode_bytes: usize,
    proof_verified: bool,
    verified_evaluation_hex: String,
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

fn ceil_muldiv(n: usize, numerator: usize, denominator: usize) -> usize {
    (n * numerator + denominator - 1) / denominator
}

fn codeword_length_spec<S: super::codespec::SdigSpecification>(n_per_row: usize) -> usize {
    let mut levels = Vec::new();
    let mut n = n_per_row;
    while n > S::baselen() {
        let next = ceil_muldiv(n, S::alpha_num(), S::alpha_den());
        levels.push((n, next));
        n = next;
    }
    assert!(!levels.is_empty(), "input dimension must exceed baselen");

    let mut post_dims = Vec::with_capacity(levels.len());
    for &(ni, mi) in &levels {
        let nip = ceil_muldiv(mi, S::r_num(), S::r_den());
        let mip = ceil_muldiv(ni, S::r_num(), S::r_den()) - ni - nip;
        post_dims.push((nip, mip));
    }

    n_per_row
        + post_dims.last().unwrap().0
        + levels
            .iter()
            .take(levels.len() - 1)
            .map(|(_, mi)| *mi)
            .sum::<usize>()
        + post_dims.iter().map(|(_, mip)| *mip).sum::<usize>()
}

fn verify_golden_case_generic<F, S>(case: &GoldenCase)
where
    F: PrimeField
        + FieldHash
        + MulAcc
        + Num
        + SizedField
        + serde::Serialize
        + serde::de::DeserializeOwned
        + LazyField,
    S: super::codespec::SdigSpecification + std::fmt::Debug + Clone + Sync,
{
    let n_per_row = case.n_per_row;
    let n_rows = case.n_rows;
    let n_cols = codeword_length_spec::<S>(n_per_row);
    assert_eq!(n_cols, case.n_cols, "n_cols mismatch for {}", case.case_id);

    let enc = SdigEncodingS::<F, S>::new_from_dims(n_per_row, n_cols, case.seed);
    let coefficient_count = n_per_row * n_rows;
    let mut rng = ChaCha20Rng::seed_from_u64(case.seed);
    let coefficients: Vec<F> = (0..coefficient_count)
        .map(|_| F::random(&mut rng))
        .collect();

    let coeffs_hex: Vec<String> = coefficients
        .iter()
        .map(|c| to_hex(c.to_repr().as_ref()))
        .collect();
    assert_eq!(
        coeffs_hex, case.coefficients_hex,
        "coefficients_hex mismatch for {}",
        case.case_id
    );

    // 1. Standalone row-by-row encoding check
    let mut matrix = vec![F::ZERO; n_rows * n_cols];
    for row in 0..n_rows {
        matrix[row * n_cols..row * n_cols + n_per_row]
            .copy_from_slice(&coefficients[row * n_per_row..(row + 1) * n_per_row]);
    }
    for row in matrix.chunks_mut(n_cols) {
        enc.encode(row).unwrap();
    }
    let encoded_rows_hex: Vec<Vec<String>> = matrix
        .chunks(n_cols)
        .map(|row| row.iter().map(|c| to_hex(c.to_repr().as_ref())).collect())
        .collect();
    assert_eq!(
        encoded_rows_hex, case.encoded_rows_hex,
        "encoded_rows_hex mismatch for {}",
        case.case_id
    );

    // 2. Full 2D commitment root check
    let comm = LcCommit::<Blake3, _>::commit(&coeffs_from_slice(&coefficients), &enc).unwrap();
    let root = comm.get_root();
    let commitment_root_hex = to_hex(root.as_ref().as_slice());
    assert_eq!(
        commitment_root_hex, case.commitment_root_hex,
        "commitment_root_hex mismatch for {}",
        case.case_id
    );

    // 3. Prover & bincode proof bytes check
    let mut point_rng = ChaCha20Rng::seed_from_u64(case.evaluation_seed);
    let point = F::random(&mut point_rng);
    let inner_tensor: Vec<F> = iterate(F::ONE, |&v| v * point).take(n_per_row).collect();
    let outer_tensor: Vec<F> = {
        let xr = point * inner_tensor.last().unwrap();
        iterate(F::ONE, |&v| v * xr).take(n_rows).collect()
    };

    let mut tr_prv = Transcript::new(b"LCPC original baseline");
    tr_prv.append_message(b"polycommit", root.as_ref().as_slice());
    tr_prv.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes());
    let pf = comm.prove(&outer_tensor[..], &enc, &mut tr_prv).unwrap();
    let pf_bytes: Vec<u8> = bincode::serialize(&pf).unwrap();
    assert_eq!(
        pf_bytes.len(),
        case.proof_bincode_bytes,
        "proof_bincode_bytes mismatch for {}",
        case.case_id
    );
    let pf_hash = to_hex(blake3::hash(&pf_bytes).as_bytes());
    assert_eq!(
        pf_hash, case.proof_bincode_blake3_hex,
        "proof_bincode_blake3_hex mismatch for {}",
        case.case_id
    );

    // 4. Verifier & evaluation check
    let mut tr_ver = Transcript::new(b"LCPC original baseline");
    tr_ver.append_message(b"polycommit", root.as_ref().as_slice());
    tr_ver.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes());
    let enc2 = SdigEncodingS::<F, S>::new_from_dims(n_per_row, n_cols, case.seed);
    let verified_eval = pf
        .verify(
            root.as_ref(),
            &outer_tensor[..],
            &inner_tensor[..],
            &enc2,
            &mut tr_ver,
        )
        .unwrap();
    let eval_hex = to_hex(verified_eval.to_repr().as_ref());
    assert!(case.proof_verified);
    assert_eq!(
        eval_hex, case.verified_evaluation_hex,
        "verified_evaluation_hex mismatch for {}",
        case.case_id
    );
}

fn coeffs_from_slice<F: Clone>(s: &[F]) -> Vec<F> {
    s.to_vec()
}

#[test]
fn golden_vectors_bit_exact_with_lcpc_orig() {
    use super::codespec::{SdigCode3, SdigCode6};

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let golden_path = Path::new(manifest_dir)
        .parent()
        .unwrap()
        .join("tests/golden/lcpc-orig/golden-v1.json");
    let content = std::fs::read_to_string(&golden_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", golden_path.display(), e));
    let golden: GoldenFile = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse {}: {}", golden_path.display(), e));

    assert_eq!(golden.schema_version, "lcpc-golden-v1");
    assert_eq!(golden.generator, "external/lcpc-orig");
    assert_eq!(
        golden.upstream_commit,
        "f0e2116001af5a1bbf39481535f837d6a25772e8"
    );
    assert_eq!(golden.hash_algorithm, "BLAKE3-256");
    assert_eq!(golden.hash_crate_version, "1.8.7");
    assert_eq!(golden.cases.len(), 12);

    for case in &golden.cases {
        match (case.field.as_str(), case.params.as_str()) {
            ("Ft127", "SdigCode6") => {
                verify_golden_case_generic::<Ft127, SdigCode6>(case);
                verify_golden_case_generic::<Mont127, SdigCode6>(case);
            }
            ("Ft127", "SdigCode3") => {
                verify_golden_case_generic::<Ft127, SdigCode3>(case);
                verify_golden_case_generic::<Mont127, SdigCode3>(case);
            }
            ("Ft63", "SdigCode6") => verify_golden_case_generic::<Ft63, SdigCode6>(case),
            ("Ft63", "SdigCode3") => verify_golden_case_generic::<Ft63, SdigCode3>(case),
            other => panic!("unexpected golden case (field, params) = {:?}", other),
        }
    }
}

#[test]
fn test_mont127_vs_ft127_matgen_encode_and_pcs_bit_exact() {
    use super::codespec::{SdigCode3, SdigCode6};
    use super::encode::{encode, BaseCase, ExpanderCode};
    use super::matgen::generate;

    fn check_spec<S: super::codespec::SdigSpecification + std::fmt::Debug + Clone + Sync>(
        n_per_row: usize,
        n_rows: usize,
        seed: u64,
    ) {
        let (pre_ft, post_ft) = generate::<Ft127, S>(n_per_row, seed);
        let (pre_mt, post_mt) = generate::<Mont127, S>(n_per_row, seed);
        assert_eq!(pre_ft.len(), pre_mt.len());
        assert_eq!(post_ft.len(), post_mt.len());

        for (g_ft, g_mt) in pre_ft
            .iter()
            .zip(pre_mt.iter())
            .chain(post_ft.iter().zip(post_mt.iter()))
        {
            assert_eq!(
                (g_ft.n_left(), g_ft.n_right()),
                (g_mt.n_left(), g_mt.n_right())
            );
            assert_eq!(g_ft.row_ptr(), g_mt.row_ptr());
            assert_eq!(g_ft.col_idx(), g_mt.col_idx());
            assert_eq!(g_ft.weights().len(), g_mt.weights().len());
            for (v_ft, v_mt) in g_ft.weights().iter().zip(g_mt.weights().iter()) {
                assert_eq!(v_ft.to_repr().as_ref(), v_mt.to_repr().as_ref());
                assert_eq!(
                    bincode::serialize(v_ft).unwrap(),
                    bincode::serialize(v_mt).unwrap()
                );
            }
        }

        let code_ft = ExpanderCode::from_graphs(BaseCase::ReedSolomon, pre_ft, post_ft);
        let code_mt = ExpanderCode::from_graphs(BaseCase::ReedSolomon, pre_mt, post_mt);
        let n_cols = code_ft.codeword_len();
        assert_eq!(n_cols, code_mt.codeword_len());

        let mut rng_ft = ChaCha20Rng::seed_from_u64(seed ^ 0x1234_5678_9abc_def0);
        let mut rng_mt = ChaCha20Rng::seed_from_u64(seed ^ 0x1234_5678_9abc_def0);
        let coeffs_ft: Vec<Ft127> = (0..n_rows * n_per_row)
            .map(|_| Ft127::random(&mut rng_ft))
            .collect();
        let coeffs_mt: Vec<Mont127> = (0..n_rows * n_per_row)
            .map(|_| Mont127::random(&mut rng_mt))
            .collect();

        for (a, b) in coeffs_ft.iter().zip(coeffs_mt.iter()) {
            assert_eq!(a.to_repr().as_ref(), b.to_repr().as_ref());
        }

        // Check standalone row encoding bit-exactness
        let mut row_ft = vec![Ft127::ZERO; n_cols];
        let mut row_mt = vec![Mont127::ZERO; n_cols];
        row_ft[..n_per_row].copy_from_slice(&coeffs_ft[..n_per_row]);
        row_mt[..n_per_row].copy_from_slice(&coeffs_mt[..n_per_row]);
        encode(&mut row_ft, &code_ft);
        encode(&mut row_mt, &code_mt);
        for (a, b) in row_ft.iter().zip(row_mt.iter()) {
            assert_eq!(a.to_repr().as_ref(), b.to_repr().as_ref());
            assert_eq!(
                bincode::serialize(a).unwrap(),
                bincode::serialize(b).unwrap()
            );
        }

        // Check full 2D PCS commit, prove, and verify bit-exactness
        let enc_ft = SdigEncodingS::<Ft127, S>::new_from_dims(n_per_row, n_cols, seed);
        let enc_mt = SdigEncodingS::<Mont127, S>::new_from_dims(n_per_row, n_cols, seed);
        let comm_ft = LcCommit::<Blake3, _>::commit(&coeffs_ft, &enc_ft).unwrap();
        let comm_mt = LcCommit::<Blake3, _>::commit(&coeffs_mt, &enc_mt).unwrap();
        assert_eq!(
            comm_ft.get_root().as_ref().as_slice(),
            comm_mt.get_root().as_ref().as_slice()
        );

        let pt_ft = Ft127::random(&mut rng_ft);
        let pt_mt = Mont127::random(&mut rng_mt);
        assert_eq!(pt_ft.to_repr().as_ref(), pt_mt.to_repr().as_ref());

        let inner_ft: Vec<Ft127> = iterate(Ft127::ONE, |&v| v * pt_ft)
            .take(n_per_row)
            .collect();
        let outer_ft: Vec<Ft127> = {
            let xr = pt_ft * inner_ft.last().unwrap();
            iterate(Ft127::ONE, |&v| v * xr).take(n_rows).collect()
        };
        let inner_mt: Vec<Mont127> = iterate(Mont127::ONE, |&v| v * pt_mt)
            .take(n_per_row)
            .collect();
        let outer_mt: Vec<Mont127> = {
            let xr = pt_mt * inner_mt.last().unwrap();
            iterate(Mont127::ONE, |&v| v * xr).take(n_rows).collect()
        };

        let root = comm_ft.get_root();
        let mut tr_prv_ft = Transcript::new(b"mont127 vs ft127");
        tr_prv_ft.append_message(b"polycommit", root.as_ref());
        tr_prv_ft.append_message(b"ncols", &(enc_ft.get_n_col_opens() as u64).to_be_bytes());
        let pf_ft = comm_ft.prove(&outer_ft, &enc_ft, &mut tr_prv_ft).unwrap();

        let mut tr_prv_mt = Transcript::new(b"mont127 vs ft127");
        tr_prv_mt.append_message(b"polycommit", root.as_ref());
        tr_prv_mt.append_message(b"ncols", &(enc_mt.get_n_col_opens() as u64).to_be_bytes());
        let pf_mt = comm_mt.prove(&outer_mt, &enc_mt, &mut tr_prv_mt).unwrap();

        let pf_ft_bytes = bincode::serialize(&pf_ft).unwrap();
        let pf_mt_bytes = bincode::serialize(&pf_mt).unwrap();
        assert_eq!(pf_ft_bytes, pf_mt_bytes);

        let mut tr_ver_ft = Transcript::new(b"mont127 vs ft127");
        tr_ver_ft.append_message(b"polycommit", root.as_ref());
        tr_ver_ft.append_message(b"ncols", &(enc_ft.get_n_col_opens() as u64).to_be_bytes());
        let eval_ft = pf_ft
            .verify(root.as_ref(), &outer_ft, &inner_ft, &enc_ft, &mut tr_ver_ft)
            .unwrap();

        let mut tr_ver_mt = Transcript::new(b"mont127 vs ft127");
        tr_ver_mt.append_message(b"polycommit", root.as_ref());
        tr_ver_mt.append_message(b"ncols", &(enc_mt.get_n_col_opens() as u64).to_be_bytes());
        let eval_mt = pf_mt
            .verify(root.as_ref(), &outer_mt, &inner_mt, &enc_mt, &mut tr_ver_mt)
            .unwrap();

        assert_eq!(eval_ft.to_repr().as_ref(), eval_mt.to_repr().as_ref());
    }

    for &(n_per_row, n_rows, seed) in &[
        (32, 2, 0x1111_2222_3333_4444),
        (64, 4, 0x5555_6666_7777_8888),
        (256, 3, 0x9999_aaaa_bbbb_cccc),
    ] {
        check_spec::<SdigCode6>(n_per_row, n_rows, seed);
        check_spec::<SdigCode3>(n_per_row, n_rows, seed ^ 1);
    }
}

#[test]
fn test_end_to_end_lazy_fields() {
    fn run_e2e<F>(seed: u64)
    where
        F: Field
            + FieldHash
            + MulAcc
            + Num
            + SizedField
            + serde::Serialize
            + serde::de::DeserializeOwned
            + LazyField,
    {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let lgl = 10usize;
        let coeffs: Vec<F> = (0..(1 << lgl)).map(|_| F::random(&mut rng)).collect();
        let enc = SdigEncoding::<F>::new_ml(lgl, seed);
        let comm = BrakedownCommit::<Blake3, F>::commit(&coeffs, &enc).unwrap();
        let root = comm.get_root();

        let x = F::random(&mut rng);
        let inner_tensor: Vec<F> = iterate(F::ONE, |&v| v * x)
            .take(comm.get_n_per_row())
            .collect();
        let outer_tensor: Vec<F> = {
            let xr = x * inner_tensor.last().unwrap();
            iterate(F::ONE, |&v| v * xr)
                .take(comm.get_n_rows())
                .collect()
        };

        let expected: F = coeffs.chunks(inner_tensor.len()).zip(&outer_tensor).fold(
            F::ZERO,
            |acc, (row, &ow)| {
                let row_val = row
                    .iter()
                    .zip(&inner_tensor)
                    .fold(F::ZERO, |racc, (&c, &iw)| racc + c * iw);
                acc + row_val * ow
            },
        );

        let mut tr1 = Transcript::new(b"e2e lazy fields");
        tr1.append_message(b"polycommit", root.as_ref());
        tr1.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes());
        let pf = comm.prove(&outer_tensor, &enc, &mut tr1).unwrap();

        let mut tr2 = Transcript::new(b"e2e lazy fields");
        tr2.append_message(b"polycommit", root.as_ref());
        tr2.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes());
        let enc2 = SdigEncoding::<F>::new_from_dims(pf.get_n_per_row(), pf.get_n_cols(), seed);
        let verified = pf
            .verify(root.as_ref(), &outer_tensor, &inner_tensor, &enc2, &mut tr2)
            .unwrap();
        assert_eq!(verified, expected);
    }

    run_e2e::<Mont127>(0x2026_0127);
    run_e2e::<Mers127>(0x2026_0128);
    run_e2e::<Fp2M61>(0x2026_0129);
}

#[test]
fn test_end_to_end_fp2m61_with_proof_serde() {
    use super::codespec::{SdigCode3, SdigCode6};

    fn run_fp2_test<
        S: super::codespec::SdigSpecification + 'static + Clone + std::fmt::Debug + Sync + Send,
    >(
        lgl: usize,
        seed: u64,
    ) {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let coeffs: Vec<Fp2M61> = (0..(1 << lgl)).map(|_| Fp2M61::random(&mut rng)).collect();
        let enc = SdigEncodingS::<Fp2M61, S>::new_ml(lgl, seed);
        let comm = LcCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
        let root = comm.get_root();

        let x = Fp2M61::random(&mut rng);
        let inner_tensor: Vec<Fp2M61> = iterate(Fp2M61::ONE, |&v| v * x)
            .take(comm.get_n_per_row())
            .collect();
        let outer_tensor: Vec<Fp2M61> = {
            let xr = x * inner_tensor.last().unwrap();
            iterate(Fp2M61::ONE, |&v| v * xr)
                .take(comm.get_n_rows())
                .collect()
        };

        let expected: Fp2M61 = coeffs.chunks(inner_tensor.len()).zip(&outer_tensor).fold(
            Fp2M61::ZERO,
            |acc, (row, &ow)| {
                let row_val = row
                    .iter()
                    .zip(&inner_tensor)
                    .fold(Fp2M61::ZERO, |racc, (&c, &iw)| racc + c * iw);
                acc + row_val * ow
            },
        );

        let mut tr1 = Transcript::new(b"e2e fp2m61 serde");
        tr1.append_message(b"polycommit", root.as_ref());
        tr1.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes());
        let pf = comm.prove(&outer_tensor, &enc, &mut tr1).unwrap();

        // Round-trip proof serialization
        let pf_bytes = bincode::serialize(&pf).unwrap();
        let pf_deser: super::LcEvalProof<Blake3, SdigEncodingS<Fp2M61, S>> =
            bincode::deserialize(&pf_bytes).unwrap();

        let mut tr2 = Transcript::new(b"e2e fp2m61 serde");
        tr2.append_message(b"polycommit", root.as_ref());
        tr2.append_message(b"ncols", &(enc.get_n_col_opens() as u64).to_be_bytes());
        let enc2 = SdigEncodingS::<Fp2M61, S>::new_from_dims(
            pf_deser.get_n_per_row(),
            pf_deser.get_n_cols(),
            seed,
        );
        let verified = pf_deser
            .verify(root.as_ref(), &outer_tensor, &inner_tensor, &enc2, &mut tr2)
            .unwrap();
        assert_eq!(verified, expected);
    }

    run_fp2_test::<SdigCode3>(8, 0x1122_3344);
    run_fp2_test::<SdigCode6>(10, 0x5566_7788);
}

// Convert an R2L graph back into the CSC `sprs` matrix upstream LCPC built for it
// (`n_right` rows, `n_left` columns, each column's row indices increasing).
fn to_sprs<F: Clone>(g: &R2lGraph<F>) -> CsMat<F> {
    let mut per_left: Vec<Vec<(usize, F)>> = vec![Vec::new(); g.n_left()];
    for r in 0..g.n_right() {
        let (lefts, weights) = g.incoming(r);
        for (&l, w) in lefts.iter().zip(weights) {
            per_left[l as usize].push((r, w.clone()));
        }
    }
    let mut ptrs = vec![0usize];
    let mut idxs = Vec::with_capacity(g.n_edges());
    let mut data = Vec::with_capacity(g.n_edges());
    for edges in per_left {
        for (r, w) in edges {
            idxs.push(r);
            data.push(w);
        }
        ptrs.push(idxs.len());
    }
    CsMat::new_csc((g.n_right(), g.n_left()), ptrs, idxs, data)
}

// Upstream LCPC's per-row encoder (`f0e2116`, `encode.rs`), kept verbatim as a test-only
// reference for the batched column-major encoder introduced in `step/03-column-major`:
// one `sprs` CSC matrix-vector product per precode/postcode, on a single row.
fn encode_row_sprs_reference<F>(xi: &mut [F], precodes: &[CsMat<F>], postcodes: &[CsMat<F>])
where
    F: Field + Num + MulAcc,
{
    use ndarray::ArrayView;

    let mut in_start = 0usize;
    for precode in precodes.iter().take(precodes.len() - 1) {
        let in_end = in_start + precode.cols();
        let (in_arr, out_arr) = xi.split_at_mut(in_end);
        out_arr[..precode.rows()].copy_from_slice(
            precode
                .dot(&ArrayView::from(&in_arr[in_start..]))
                .as_slice()
                .unwrap(),
        );
        in_start = in_end;
    }

    let (mut in_start, mut out_start) = {
        let precode = precodes.last().unwrap();
        let in_end = in_start + precode.cols();
        let (in_arr, _) = precode
            .dot(&ArrayView::from(&xi[in_start..in_end]))
            .into_raw_vec_and_offset();
        let out_end = in_end + postcodes.last().unwrap().cols();
        let xo = &mut xi[in_end..out_end];
        let mut x = F::ONE;
        for r in xo.iter_mut() {
            *r = F::ZERO;
            for j in (0..in_arr.len()).rev() {
                *r *= x;
                *r += in_arr[j];
            }
            x += F::ONE;
        }
        (in_end + precode.rows(), out_end)
    };

    for (precode, postcode) in precodes.iter().rev().zip(postcodes.iter().rev()) {
        in_start -= precode.rows();
        let (in_arr, out_arr) = xi.split_at_mut(out_start);
        out_arr[..postcode.rows()].copy_from_slice(
            postcode
                .dot(&ArrayView::from(&in_arr[in_start..]))
                .as_slice()
                .unwrap(),
        );
        out_start += postcode.rows();
    }
    assert_eq!(in_start, precodes[0].cols());
    assert_eq!(out_start, xi.len());
}

// The batched column-major R2L encoder (direct kernel, single-vector wrapper, and the
// `LcEncoding::encode_rows` override used by `commit`) must reproduce the upstream
// per-row `sprs` encoder bit for bit (as `step/03-column-major` did), for every batch
// size including the verifier's 1 and 2.
#[test]
fn batched_col_major_encode_matches_per_row_reference() {
    use super::codespec::{SdigCode3, SdigCode6, SdigSpecification};
    use super::encode::{encode, encode_col_major};

    fn check<F, S>(n_per_row: usize, seed: u64)
    where
        F: Field + FieldHash + MulAcc + Num + SizedField + std::fmt::Debug + LazyField,
        S: SdigSpecification + std::fmt::Debug + Clone + Sync,
    {
        let enc = SdigEncodingS::<F, S>::new_from_dims(
            n_per_row,
            codeword_length_spec::<S>(n_per_row),
            seed,
        );
        let code = enc.code();
        let n_cols = code.codeword_len();
        let pre: Vec<CsMat<F>> = code.precodes().iter().map(to_sprs).collect();
        let post: Vec<CsMat<F>> = code.postcodes().iter().map(to_sprs).collect();
        let mut rng = ChaCha20Rng::seed_from_u64(seed ^ 0x0c01_3a70);

        for n_rows in [1usize, 2, 3, 5, 16, 33] {
            // row-major messages, followed by garbage that every encoder must overwrite
            let mut rows: Vec<F> = (0..n_rows * n_cols).map(|_| F::random(&mut rng)).collect();

            // reference: upstream per-row sprs encoding
            let mut expect = rows.clone();
            for row in expect.chunks_mut(n_cols) {
                encode_row_sprs_reference(row, &pre, &post);
            }

            // single-vector wrapper, row by row
            let mut single = rows.clone();
            for row in single.chunks_mut(n_cols) {
                encode(row, code);
            }
            assert_eq!(single, expect, "encode, n_rows={n_rows}");

            // direct column-major kernel
            let mut buf = vec![F::ZERO; n_cols * n_rows];
            for row in 0..n_rows {
                for col in 0..n_cols {
                    buf[col * n_rows + row] = rows[row * n_cols + col];
                }
            }
            encode_col_major(&mut buf, n_rows, code);
            for row in 0..n_rows {
                for col in 0..n_cols {
                    assert_eq!(
                        buf[col * n_rows + row],
                        expect[row * n_cols + col],
                        "encode_col_major, n_rows={n_rows}, row={row}, col={col}"
                    );
                }
            }

            // row-major batched entry point used by LcCommit::commit
            enc.encode_rows(&mut rows, n_cols).unwrap();
            assert_eq!(rows, expect, "encode_rows, n_rows={n_rows}");
        }
    }

    check::<Ft63, SdigCode3>(300, 1);
    check::<Ft127, SdigCode3>(257, 2);
    check::<Ft127, SdigCode6>(512, 3);
    check::<Mont127, SdigCode6>(512, 3);
    check::<Mers127, SdigCode3>(400, 4);
    check::<Fp2M61, SdigCode6>(333, 5);
}

// Runtime mode (a) parameters build exactly the code of the matching compile-time
// specification (same graphs, dimensions, and PCS parameters).
#[test]
fn runtime_lcpc_params_match_compile_time_spec() {
    use super::codespec::{SdigCode3, SdigCode6, SdigSpecification};

    fn check<S: SdigSpecification + std::fmt::Debug + Clone + Sync>(
        params: CodeParams,
        name: &str,
    ) {
        assert_eq!(params.name(), name);
        assert_eq!(CodeParams::from_name(name), Some(params));
        for (n, seed) in [(64usize, 7u64), (1000, 8)] {
            let a =
                SdigEncodingS::<Mont127, S>::new_from_dims(n, codeword_length_spec::<S>(n), seed);
            let b = SpielmanEncoding::<Mont127>::with_params(params, n, seed);
            assert_eq!(a.code().precodes(), b.code().precodes());
            assert_eq!(a.code().postcodes(), b.code().postcodes());
            assert_eq!(a.get_dims(n), b.get_dims(n));
            assert_eq!(a.get_n_col_opens(), b.get_n_col_opens());
            assert_eq!(a.get_n_degree_tests(), b.get_n_degree_tests());
        }
        let a = SdigEncodingS::<Fp2M61, S>::new_ml(12, 9);
        let b = SpielmanEncoding::<Fp2M61>::new_ml_with_params(params, 12, 9);
        assert_eq!(a.get_dims(1 << 12), b.get_dims(1 << 12));
        assert_eq!(a.code().precodes(), b.code().precodes());
    }

    check::<SdigCode6>(CodeParams::lcpc_sdig6(), "lcpc-sdig6");
    check::<SdigCode3>(CodeParams::lcpc_sdig3(), "lcpc-sdig3");
}

// Mode (b) (SIMD-Spielman parameters): graph shapes, systematic prefix, linearity,
// batch invariance, and an end-to-end PCS round trip. Bit-exactness with upstream's
// test vectors is checked in `lcpc-oracle-tests` (`simd_spielman_tv.rs`).
#[test]
fn simd_spielman_mode_encodes_and_commits() {
    use super::codespec::SimdSpielmanParams;
    use super::encode::{encode, encode_col_major, BaseCase};
    use super::matgen::simd_spielman_dims;

    fn check<F>(k: usize, seed: u64)
    where
        F: Field
            + FieldHash
            + Num
            + SizedField
            + serde::Serialize
            + serde::de::DeserializeOwned
            + LazyField,
    {
        let enc = SpielmanEncoding::<F>::with_params(CodeParams::SimdSpielman, k, seed);
        let code = enc.code();
        assert_eq!(code.base_case(), BaseCase::Identity);
        let dims = simd_spielman_dims(k);
        assert_eq!(code.precodes().len(), dims.len());
        for (lvl, (g1, g2)) in dims
            .iter()
            .zip(code.precodes().iter().zip(code.postcodes()))
        {
            assert_eq!((g1.n_left(), g1.n_right()), (lvl.n, lvl.m));
            assert_eq!((g2.n_left(), g2.n_right()), (lvl.n_prime, lvl.m_prime));
            assert_eq!(g1.n_edges(), lvl.n * SimdSpielmanParams::CN);
            assert_eq!(g2.n_edges(), lvl.n_prime * SimdSpielmanParams::DN);
        }
        let last = dims.last().unwrap();
        assert!(last.m < SimdSpielmanParams::DISTANCE_THRESHOLD);
        let n_cols = code.codeword_len();
        assert_eq!(
            n_cols,
            k + dims.iter().map(|l| l.m + l.m_prime).sum::<usize>()
        );

        let mut rng = ChaCha20Rng::seed_from_u64(seed ^ 0xb0b0);
        let n_rows = 5;
        let msgs: Vec<F> = (0..n_rows * k).map(|_| F::random(&mut rng)).collect();
        let mut rows: Vec<F> = (0..n_rows * n_cols).map(|_| F::random(&mut rng)).collect();
        for (row, msg) in rows.chunks_mut(n_cols).zip(msgs.chunks(k)) {
            row[..k].copy_from_slice(msg);
        }
        let mut single = rows.clone();
        for row in single.chunks_mut(n_cols) {
            encode(row, code);
        }
        let mut buf = vec![F::ZERO; n_rows * n_cols];
        for row in 0..n_rows {
            for col in 0..n_cols {
                buf[col * n_rows + row] = rows[row * n_cols + col];
            }
        }
        encode_col_major(&mut buf, n_rows, code);
        enc.encode_rows(&mut rows, n_cols).unwrap();
        assert_eq!(rows, single);
        for row in 0..n_rows {
            assert_eq!(
                &rows[row * n_cols..row * n_cols + k],
                &msgs[row * k..(row + 1) * k]
            );
            for col in 0..n_cols {
                assert_eq!(buf[col * n_rows + row], rows[row * n_cols + col]);
            }
        }

        // linearity: Enc(a x + y) = a Enc(x) + Enc(y)
        let a = F::random(&mut rng);
        let (x, y) = (&rows[..n_cols], &rows[n_cols..2 * n_cols]);
        let mut comb = vec![F::ZERO; n_cols];
        for i in 0..k {
            comb[i] = a * x[i] + y[i];
        }
        encode(&mut comb, code);
        for i in 0..n_cols {
            assert_eq!(comb[i], a * x[i] + y[i], "linearity at {i}");
        }

        // PCS round trip
        let lgl = 12;
        let coeffs: Vec<F> = (0..(1usize << lgl)).map(|_| F::random(&mut rng)).collect();
        let enc = SpielmanEncoding::<F>::new_ml_with_params(CodeParams::SimdSpielman, lgl, seed);
        let comm = LcCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
        let root = comm.get_root();
        let pt = F::random(&mut rng);
        let inner: Vec<F> = iterate(F::ONE, |&v| v * pt)
            .take(comm.get_n_per_row())
            .collect();
        let outer: Vec<F> = {
            let xr = pt * inner.last().unwrap();
            iterate(F::ONE, |&v| v * xr)
                .take(comm.get_n_rows())
                .collect()
        };
        let expected = coeffs
            .chunks(inner.len())
            .zip(&outer)
            .fold(F::ZERO, |acc, (row, &ow)| {
                acc + row
                    .iter()
                    .zip(&inner)
                    .fold(F::ZERO, |r, (&c, &iw)| r + c * iw)
                    * ow
            });
        let mut tr1 = Transcript::new(b"simd-spielman mode");
        tr1.append_message(b"polycommit", root.as_ref());
        let pf = comm.prove(&outer, &enc, &mut tr1).unwrap();
        let mut tr2 = Transcript::new(b"simd-spielman mode");
        tr2.append_message(b"polycommit", root.as_ref());
        let verified = pf
            .verify(root.as_ref(), &outer, &inner, &enc, &mut tr2)
            .unwrap();
        assert_eq!(verified, expected);
    }

    check::<Mont127>(64, 1);
    check::<Mers127>(256, 2);
    check::<Fp2M61>(300, 3);
    check::<Ft63>(13, 4);
}

#[test]
fn test_worst_case_accumulation_and_fold() {
    use super::encode::encode_column_block;

    fn run_worst_case<F: LazyField>(w_val: F, in_val: F) {
        let degree = 100usize; // > MAX_ACCUMULATIONS (32 for Fp2M61) to force multiple folds
        let n_left = degree;
        let n_right = 1;
        let right_idx = vec![0u32; degree];
        let weights = vec![w_val; degree];
        let graph = R2lGraph::from_left_edges_fixed_degree(n_left, n_right, 1, &right_idx, &weights);

        for &n_rows in &[1usize, 8usize] {
            let input = vec![in_val; n_left * n_rows];
            let mut output = vec![F::ZERO; n_right * n_rows];

            encode_column_block(&graph, &input, &mut output, n_rows);

            let mut expected = F::ZERO;
            for _ in 0..degree {
                expected += w_val * in_val;
            }

            for row in 0..n_rows {
                assert_eq!(
                    output[row], expected,
                    "row {row} worst case degree {degree} (n_rows={n_rows})"
                );
            }
        }
    }

    // Mont127: q - 1
    let q_minus_1 = -Mont127::ONE;
    run_worst_case::<Mont127>(q_minus_1, q_minus_1);

    // Mers127: p - 1
    let p_minus_1 = -Mers127::ONE;
    run_worst_case::<Mers127>(p_minus_1, p_minus_1);

    // Fp2M61: (p - 1) + (p - 1)i
    let fp2_max = Fp2M61::from_parts(Fp2M61::MODULUS_P - 1, Fp2M61::MODULUS_P - 1);
    run_worst_case::<Fp2M61>(fp2_max, fp2_max);
}

#[test]
fn test_cache_detection_sanity() {
    use super::cache::{
        adaptive_slice_count_with_params, detect_cache, parse_size, DEFAULT_LLC_HEADROOM_GAMMA,
    };

    // 1. Test parse_size on various formats
    assert_eq!(parse_size("48K"), Some(48 * 1024));
    assert_eq!(parse_size("48KiB"), Some(48 * 1024));
    assert_eq!(parse_size("512k"), Some(512 * 1024));
    assert_eq!(parse_size("512KB"), Some(512 * 1024));
    assert_eq!(parse_size("16384K"), Some(16384 * 1024));
    assert_eq!(parse_size("16M"), Some(16 * 1024 * 1024));
    assert_eq!(parse_size("32MiB"), Some(32 * 1024 * 1024));
    assert_eq!(parse_size("1048576"), Some(1048576));
    assert_eq!(parse_size("1048576 B"), Some(1048576));
    assert_eq!(parse_size("1G"), Some(1024 * 1024 * 1024));
    assert_eq!(parse_size(""), None);
    assert_eq!(parse_size("  "), None);
    assert_eq!(parse_size("unknown"), None);
    assert_eq!(parse_size("123XYZ"), None);

    // 2. Test detect_cache returns plausible values
    let cache = detect_cache();
    assert!(
        cache.l1d_bytes >= 16 * 1024,
        "L1d cache should be at least 16 KiB, got {}",
        cache.l1d_bytes
    );
    assert!(
        cache.l2_bytes >= 128 * 1024,
        "L2 cache should be at least 128 KiB, got {}",
        cache.l2_bytes
    );
    assert!(
        cache.l3_bytes >= 1024 * 1024,
        "L3 cache should be at least 1 MiB, got {}",
        cache.l3_bytes
    );

    // 3. Test analytical model against Plan 04 §2.2 Table 166
    let elem_bytes = 16;
    let gamma = DEFAULT_LLC_HEADROOM_GAMMA;

    // 16 MiB LLC (e.g. i7-11700K, EPYC 9754)
    let llc_16m = 16 * 1024 * 1024;
    let expected_16m = [
        (16, 256, 1),
        (18, 512, 1),
        (20, 1024, 2),
        (22, 2048, 8),
        (24, 4096, 32),
        (26, 8192, 128),
        (28, 16384, 512),
    ];
    for &(logn, k, expected_s) in &expected_16m {
        let s = adaptive_slice_count_with_params(k, k, elem_bytes, llc_16m, gamma);
        assert_eq!(
            s, expected_s,
            "16 MiB LLC mismatch at logN={logn}: expected {expected_s}, got {s}"
        );
    }

    // 32 MiB LLC (e.g. GCP c3d Genoa Zen 4)
    let llc_32m = 32 * 1024 * 1024;
    let expected_32m = [
        (16, 256, 1),
        (18, 512, 1),
        (20, 1024, 1),
        (22, 2048, 4),
        (24, 4096, 16),
        (26, 8192, 64),
        (28, 16384, 256),
    ];
    for &(logn, k, expected_s) in &expected_32m {
        let s = adaptive_slice_count_with_params(k, k, elem_bytes, llc_32m, gamma);
        assert_eq!(
            s, expected_s,
            "32 MiB LLC mismatch at logN={logn}: expected {expected_s}, got {s}"
        );
    }
}

#[test]
fn test_slice_invariance_all_fields_and_modes() {
    use super::codespec::{SdigCode3, SdigCode6};

    fn check_invariance<F: LazyField>(
        params: CodeParams,
        k: usize,
        n_rows: usize,
        seed: u64,
    ) {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let enc = SpielmanEncoding::<F>::with_params(params, k, seed);
        let n_cols = enc.code().codeword_len();

        let mut initial_matrix = vec![F::ZERO; n_rows * n_cols];
        for r in 0..n_rows {
            for c in 0..k {
                initial_matrix[r * n_cols + c] = F::random(&mut rng);
            }
        }

        // Base run: s = 1 (single slice, previous behaviour)
        let mut base_matrix = initial_matrix.clone();
        enc.encode_rows_with_slices(&mut base_matrix, n_cols, 1)
            .expect("encode s=1 failed");

        // Systematic prefix check
        for r in 0..n_rows {
            assert_eq!(
                &base_matrix[r * n_cols..r * n_cols + k],
                &initial_matrix[r * n_cols..r * n_cols + k],
                "systematic prefix corrupted at row {r}"
            );
        }

        // Test across slice counts s in {0 (adaptive), 2, 4, 8, 16}
        for &s in &[0, 2, 4, 8, 16] {
            let mut sliced_matrix = initial_matrix.clone();
            enc.encode_rows_with_slices(&mut sliced_matrix, n_cols, s)
                .unwrap_or_else(|e| panic!("encode s={s} failed: {e}"));

            assert_eq!(
                sliced_matrix, base_matrix,
                "slice invariance failed for field={}, params={:?}, k={k}, n_rows={n_rows}, s={s}",
                std::any::type_name::<F>(),
                params
            );
        }
    }

    let test_configs = [
        // Mode (a) LCPC SdigCode6
        CodeParams::Lcpc(LcpcParams::of::<SdigCode6>()),
        // Mode (a) LCPC SdigCode3
        CodeParams::Lcpc(LcpcParams::of::<SdigCode3>()),
        // Mode (b) SIMD-Spielman
        CodeParams::SimdSpielman,
    ];

    for params in test_configs {
        // Square configurations
        for &(k, n_rows) in &[(64, 64), (128, 128), (256, 256)] {
            check_invariance::<Mont127>(params, k, n_rows, 0x1234_5678);
            check_invariance::<Mers127>(params, k, n_rows, 0x2345_6789);
            check_invariance::<Fp2M61>(params, k, n_rows, 0x3456_789a);
        }

        // Non-square configurations with non-multiples of s to test remainder row handling
        for &(k, n_rows) in &[(64, 100), (64, 35)] {
            check_invariance::<Mont127>(params, k, n_rows, 0x5678_9abc);
            check_invariance::<Mers127>(params, k, n_rows, 0x6789_abcd);
            check_invariance::<Fp2M61>(params, k, n_rows, 0x789a_bcde);
        }
    }
}

#[test]
fn test_autotune_slices_empirical() {
    fn check_autotune<F: LazyField>(params: CodeParams, k: usize, n_rows: usize) {
        let seed = 0xbeef_cafe;
        let mut enc = SpielmanEncoding::<F>::with_params(params, k, seed);
        let n_cols = enc.code().codeword_len();

        let s_star = enc.effective_slices(n_rows);
        assert!(s_star >= 1);

        let tuned_s = enc.autotune(n_rows);
        assert!(tuned_s >= 1);
        assert!(tuned_s <= n_rows);
        assert_eq!(enc.slices(), tuned_s);

        // Verify that encoding with the tuned slice count is bit-exact with s=1
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut matrix_tuned = vec![F::ZERO; n_rows * n_cols];
        for r in 0..n_rows {
            for c in 0..k {
                matrix_tuned[r * n_cols + c] = F::random(&mut rng);
            }
        }
        let matrix_init = matrix_tuned.clone();

        let mut matrix_s1 = matrix_init.clone();
        enc.encode_rows_with_slices(&mut matrix_s1, n_cols, 1).unwrap();

        enc.encode_rows(&mut matrix_tuned, n_cols).unwrap();
        assert_eq!(
            matrix_tuned, matrix_s1,
            "autotuned s={tuned_s} failed bit-exactness check vs s=1"
        );
    }

    check_autotune::<Mont127>(CodeParams::SimdSpielman, 128, 128);
    check_autotune::<Mers127>(CodeParams::SimdSpielman, 128, 128);
    check_autotune::<Fp2M61>(CodeParams::SimdSpielman, 128, 128);
}

#[test]
fn test_pcs_commit_with_slicing() {
    fn check_pcs_slices<F>(slices: usize)
    where
        F: LazyField + FieldHash,
    {
        let lgl = 12;
        let mut rng = ChaCha20Rng::seed_from_u64(0xcafe_d00d);
        let coeffs: Vec<F> = (0..(1usize << lgl)).map(|_| F::random(&mut rng)).collect();
        let enc = SpielmanEncoding::<F>::new_ml_with_params(CodeParams::SimdSpielman, lgl, 0x1234)
            .with_slices(slices);

        let comm = LcCommit::<Blake3, _>::commit(&coeffs, &enc).unwrap();
        let root = comm.get_root();

        let pt = F::random(&mut rng);
        let inner: Vec<F> = iterate(F::ONE, |&v| v * pt)
            .take(comm.get_n_per_row())
            .collect();
        let outer: Vec<F> = {
            let xr = pt * inner.last().unwrap();
            iterate(F::ONE, |&v| v * xr)
                .take(comm.get_n_rows())
                .collect()
        };

        let expected = coeffs
            .chunks(inner.len())
            .zip(&outer)
            .fold(F::ZERO, |acc, (row, &ow)| {
                acc + row
                    .iter()
                    .zip(&inner)
                    .fold(F::ZERO, |r, (&c, &iw)| r + c * iw)
                    * ow
            });

        let mut tr1 = Transcript::new(b"sliced commit test");
        tr1.append_message(b"polycommit", root.as_ref());
        let pf = comm.prove(&outer, &enc, &mut tr1).unwrap();

        let mut tr2 = Transcript::new(b"sliced commit test");
        tr2.append_message(b"polycommit", root.as_ref());
        let verified = pf
            .verify(root.as_ref(), &outer, &inner, &enc, &mut tr2)
            .unwrap();
        assert_eq!(verified, expected);
    }

    for &s in &[0, 1, 2, 4, 8] {
        check_pcs_slices::<Mont127>(s);
        check_pcs_slices::<Fp2M61>(s);
    }
}
