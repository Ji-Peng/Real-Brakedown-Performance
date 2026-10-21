// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

use super::{BrakedownCommit, SdigEncoding, SdigEncodingS};

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
    use super::encode::{codeword_length, encode};
    use super::matgen::generate;

    let n = 256usize + ((rng.random::<u64>() as usize) % 4096);
    let (precodes, postcodes) = generate::<Ft63, TestCode>(n, 0u64);

    let xi_len = codeword_length(&precodes, &postcodes);
    let mut xi = Vec::with_capacity(xi_len);
    for _ in 0..xi_len {
        xi.push(Ft63::random(&mut rng));
    }
    encode(&mut xi, &precodes, &postcodes);
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
        + serde::de::DeserializeOwned,
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
            ("Ft127", "SdigCode6") => verify_golden_case_generic::<Ft127, SdigCode6>(case),
            ("Ft127", "SdigCode3") => verify_golden_case_generic::<Ft127, SdigCode3>(case),
            ("Ft63", "SdigCode6") => verify_golden_case_generic::<Ft63, SdigCode6>(case),
            ("Ft63", "SdigCode3") => verify_golden_case_generic::<Ft63, SdigCode3>(case),
            other => panic!("unexpected golden case (field, params) = {:?}", other),
        }
    }
}
