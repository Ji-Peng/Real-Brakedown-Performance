// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-2d, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.
//
// Modified 2026 by the RealBrakedownPerf authors (Apache-2.0, see NOTICE):
// tests updated for current dependencies, stable Rust, parallel row collapse,
// batched leaf hashing, merkle_root, repeated-opening verification, tiled
// column opening, and parallel single-pass commit initialisation.
// The per-commit changes are in the git history since step/00-import
// (upstream conroi/lcpc f0e2116).

use super::{def_labels, FieldHash, LcCommit, LcEncoding, LcEvalProof, LcRoot};

use digest::{Digest, Output};
use ff::Field;
use itertools::iterate;
use lcpc_test_fields::fft::{FFTError, FFTPrecomp, FieldFFT};
use lcpc_test_fields::ft127::Ft127;
use lcpc_test_fields::ft63::*;
use lcpc_test_fields::Blake3;
use merlin::Transcript;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::iter::repeat_with;

#[derive(Clone, Debug)]
struct LigeroEncoding<Ft> {
    n_per_row: usize, // number of inputs to the encoding
    n_cols: usize,    // number of outputs from the encoding
    pc: FFTPrecomp<Ft>,
}

const N_COL_OPENS: usize = 128usize; // arbitrary, not secure
impl<Ft> LigeroEncoding<Ft>
where
    Ft: FieldFFT,
{
    fn _get_dims(len: usize, rho: f64) -> Option<(usize, usize, usize)> {
        assert!(rho > 0f64);
        assert!(rho < 1f64);

        // compute #cols, which must be a power of 2 because of FFT
        let nc = (((len as f64).sqrt() / rho).ceil() as usize)
            .checked_next_power_of_two()
            .and_then(|nc| {
                if nc > (1 << <Ft as FieldFFT>::S) {
                    None
                } else {
                    Some(nc)
                }
            })?;

        // minimize nr subject to #cols and rho
        let np = ((nc as f64) * rho).floor() as usize;
        let nr = (len + np - 1) / np;
        assert!(np * nr >= len);
        assert!(np * (nr - 1) < len);

        Some((nr, np, nc))
    }

    fn _dims_ok(n_per_row: usize, n_cols: usize) -> bool {
        let sz = n_per_row < n_cols;
        let pow = n_cols.is_power_of_two();
        sz && pow
    }

    pub fn new(len: usize, rho: f64) -> Self {
        let (_, n_per_row, n_cols) = Self::_get_dims(len, rho).unwrap();
        assert!(Self::_dims_ok(n_per_row, n_cols));
        let pc = <Ft as FieldFFT>::precomp_fft(n_cols).unwrap();
        Self {
            n_per_row,
            n_cols,
            pc,
        }
    }

    pub fn new_from_dims(n_per_row: usize, n_cols: usize) -> Self {
        assert!(Self::_dims_ok(n_per_row, n_cols));
        let pc = <Ft as FieldFFT>::precomp_fft(n_cols).unwrap();
        assert_eq!(n_cols, 1 << pc.get_log_len());
        Self {
            n_per_row,
            n_cols,
            pc,
        }
    }
}

impl<Ft> LcEncoding for LigeroEncoding<Ft>
where
    Ft: FieldFFT + FieldHash,
{
    type F = Ft;
    type Err = FFTError;

    def_labels!(lcpc2d_test);

    fn encode<T: AsMut<[Ft]>>(&self, inp: T) -> Result<(), FFTError> {
        <Ft as FieldFFT>::fft_io_pc(inp, &self.pc)
    }

    fn get_dims(&self, len: usize) -> (usize, usize, usize) {
        let n_rows = (len + self.n_per_row - 1) / self.n_per_row;
        (n_rows, self.n_per_row, self.n_cols)
    }

    fn dims_ok(&self, n_per_row: usize, n_cols: usize) -> bool {
        let ok = Self::_dims_ok(n_per_row, n_cols);
        let pc = n_cols == (1 << self.pc.get_log_len());
        let np = n_per_row == self.n_per_row;
        let nc = n_cols == self.n_cols;
        ok && pc && np && nc
    }

    fn get_n_col_opens(&self) -> usize {
        N_COL_OPENS
    }

    fn get_n_degree_tests(&self) -> usize {
        2
    }
}

type LigeroCommit<D, F> = LcCommit<D, LigeroEncoding<F>>;

type LigeroEvalProof<D, F> = LcEvalProof<D, LigeroEncoding<F>>;

#[test]
fn log2() {
    use super::log2;

    for idx in 0..31 {
        assert_eq!(log2(1usize << idx), idx);
    }
}

#[test]
fn merkleize() {
    use super::{merkleize, merkleize_ser};

    let mut test_comm = random_comm();
    let mut test_comm_2 = test_comm.clone();

    merkleize(&mut test_comm);
    merkleize_ser(&mut test_comm_2);

    assert_eq!(&test_comm.comm, &test_comm_2.comm);
    assert_eq!(&test_comm.coeffs, &test_comm_2.coeffs);
    assert_eq!(&test_comm.hashes, &test_comm_2.hashes);
}

#[test]
fn eval_outer() {
    use super::{eval_outer, eval_outer_ser};

    let test_comm = random_comm();
    let mut rng = rand::rng();
    let tensor: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
        .take(test_comm.n_rows)
        .collect();

    let res1 = eval_outer(&test_comm, &tensor[..]).unwrap();
    let res2 = eval_outer_ser(&test_comm, &tensor[..]).unwrap();

    assert_eq!(&res1[..], &res2[..]);
}

#[test]
fn collapse_par_matches_eval_outer_ser() {
    use super::{collapse_par, eval_outer_ser};

    let mut rng = ChaCha20Rng::seed_from_u64(0x2d_c011_a95e);

    // Random dimensions plus explicit small / non-power-of-two row counts
    let mut comms = vec![random_comm(), random_comm()];
    for &(n_rows, n_per_row, n_cols) in &[(1usize, 5usize, 16usize), (3, 7, 16), (17, 19, 32)] {
        let coeffs: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
            .take(n_rows * n_per_row)
            .collect();
        let comm: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
            .take(n_rows * n_cols)
            .collect();
        comms.push(LigeroCommit::<Blake3, Ft63> {
            comm,
            coeffs,
            n_rows,
            n_cols,
            n_per_row,
            hashes: vec![<Output<Blake3> as Default>::default(); 2 * n_cols - 1],
        });
    }

    for test_comm in &comms {
        let enc = LigeroEncoding::<Ft63>::new_from_dims(test_comm.n_per_row, test_comm.n_cols);
        let t0: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
            .take(test_comm.n_rows)
            .collect();
        let t1: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
            .take(test_comm.n_rows)
            .collect();
        let t2: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
            .take(test_comm.n_rows)
            .collect();

        let exp0 = eval_outer_ser(test_comm, &t0).unwrap();
        let exp1 = eval_outer_ser(test_comm, &t1).unwrap();
        let exp2 = eval_outer_ser(test_comm, &t2).unwrap();

        let got1 = collapse_par(&enc, &test_comm.coeffs, test_comm.n_per_row, &[&t0]);
        assert_eq!(got1, vec![exp0.clone()]);

        let got2 = collapse_par(&enc, &test_comm.coeffs, test_comm.n_per_row, &[&t0, &t1]);
        assert_eq!(got2, vec![exp0.clone(), exp1.clone()]);

        let got3 = collapse_par(
            &enc,
            &test_comm.coeffs,
            test_comm.n_per_row,
            &[&t0, &t1, &t2],
        );
        assert_eq!(got3, vec![exp0, exp1, exp2]);
    }
}

#[test]
fn open_column() {
    use super::{merkleize, open_column, verify_column};

    let mut rng = rand::rng();

    let test_comm = {
        let mut tmp = random_comm();
        merkleize(&mut tmp);
        tmp
    };

    let root = test_comm.get_root();
    for _ in 0..64 {
        let col_num = rng.random::<u64>() as usize % test_comm.n_cols;
        let column = open_column(&test_comm, col_num).unwrap();
        assert!(verify_column::<Blake3, _>(
            &column,
            col_num,
            root.as_ref(),
            &[],
            &Ft63::ZERO,
        ));
    }
}

#[test]
fn commit() {
    use super::{commit, eval_outer, eval_outer_fft};

    let (coeffs, rho) = random_coeffs_rho();
    let enc = LigeroEncoding::<Ft63>::new(coeffs.len(), rho);
    let comm = commit::<Blake3, LigeroEncoding<_>>(&coeffs, &enc).unwrap();

    let x = Ft63::random(&mut rand::rng());

    let eval = comm
        .coeffs
        .iter()
        .zip(iterate(Ft63::ONE, |&v| v * x).take(coeffs.len()))
        .fold(Ft63::ZERO, |acc, (c, r)| acc + *c * r);

    let roots_lo: Vec<Ft63> = iterate(Ft63::ONE, |&v| v * x)
        .take(comm.n_per_row)
        .collect();
    let roots_hi: Vec<Ft63> = {
        let xr = x * roots_lo.last().unwrap();
        iterate(Ft63::ONE, |&v| v * xr)
            .take(comm.n_rows)
            .collect()
    };
    let coeffs_flattened = eval_outer(&comm, &roots_hi[..]).unwrap();
    let eval2 = coeffs_flattened
        .iter()
        .zip(roots_lo.iter())
        .fold(Ft63::ZERO, |acc, (c, r)| acc + *c * r);
    assert_eq!(eval, eval2);

    let mut poly_fft = eval_outer_fft(&comm, &roots_hi[..]).unwrap();
    <Ft63 as FieldFFT>::ifft_oi(&mut poly_fft).unwrap();
    assert!(poly_fft
        .iter()
        .skip(comm.n_per_row)
        .all(|&v| v == Ft63::ZERO));
    let eval3 = poly_fft
        .iter()
        .zip(roots_lo.iter())
        .fold(Ft63::ZERO, |acc, (c, r)| acc + *c * r);
    assert_eq!(eval2, eval3);
}

#[test]
fn end_to_end() {
    use super::{commit, prove, verify};

    // commit to a random polynomial at a random rate
    let (coeffs, rho) = random_coeffs_rho();
    let enc = LigeroEncoding::<Ft63>::new(coeffs.len(), rho);
    let comm = commit::<Blake3, LigeroEncoding<_>>(&coeffs, &enc).unwrap();
    // this is the polynomial commitment
    let root = comm.get_root();

    // evaluate the random polynomial we just generated at a random point x
    let x = Ft63::random(&mut rand::rng());
    let eval = comm
        .coeffs
        .iter()
        .zip(iterate(Ft63::ONE, |&v| v * x).take(coeffs.len()))
        .fold(Ft63::ZERO, |acc, (c, r)| acc + *c * r);

    // compute the outer and inner tensors for powers of x
    // NOTE: we treat coeffs as a univariate polynomial, but it doesn't
    // really matter --- the only difference from a multilinear is the
    // way we compute outer_tensor and inner_tensor from the eval point
    let inner_tensor: Vec<Ft63> = iterate(Ft63::ONE, |&v| v * x)
        .take(comm.n_per_row)
        .collect();
    let outer_tensor: Vec<Ft63> = {
        let xr = x * inner_tensor.last().unwrap();
        iterate(Ft63::ONE, |&v| v * xr)
            .take(comm.n_rows)
            .collect()
    };

    // compute an evaluation proof
    let mut tr1 = Transcript::new(b"test transcript");
    tr1.append_message(b"polycommit", root.as_ref());
    tr1.append_message(b"rate", &rho.to_be_bytes()[..]);
    tr1.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
    let pf: LigeroEvalProof<Blake3, Ft63> =
        prove(&comm, &outer_tensor[..], &enc, &mut tr1).unwrap();
    let encoded: Vec<u8> = bincode::serialize(&pf).unwrap();
    let encroot: Vec<u8> = bincode::serialize(&LcRoot::<Blake3, LigeroEncoding<Ft63>> {
        root: *root.as_ref(),
        _p: Default::default(),
    })
    .unwrap();

    // verify it and finish evaluation
    let mut tr2 = Transcript::new(b"test transcript");
    tr2.append_message(b"polycommit", root.as_ref());
    tr2.append_message(b"rate", &rho.to_be_bytes()[..]);
    tr2.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
    let enc2 = LigeroEncoding::<Ft63>::new_from_dims(pf.get_n_per_row(), pf.get_n_cols());
    let res = verify(
        root.as_ref(),
        &outer_tensor[..],
        &inner_tensor[..],
        &pf,
        &enc2,
        &mut tr2,
    )
    .unwrap();

    let root2 =
        bincode::deserialize::<LcRoot<Blake3, LigeroEncoding<Ft63>>>(&encroot[..]).unwrap();
    let pf2: LigeroEvalProof<Blake3, Ft63> = bincode::deserialize(&encoded[..]).unwrap();
    let mut tr3 = Transcript::new(b"test transcript");
    tr3.append_message(b"polycommit", root.as_ref());
    tr3.append_message(b"rate", &rho.to_be_bytes()[..]);
    tr3.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
    let enc3 = LigeroEncoding::<Ft63>::new_from_dims(pf2.get_n_per_row(), pf2.get_n_cols());
    let res2 = verify(
        root2.as_ref(),
        &outer_tensor[..],
        &inner_tensor[..],
        &pf2,
        &enc3,
        &mut tr3,
    )
    .unwrap();

    assert_eq!(res, eval);
    assert_eq!(res, res2);
}

#[test]
fn end_to_end_two_proofs() {
    use super::{commit, prove, verify};

    // commit to a random polynomial at a random rate
    let (coeffs, rho) = random_coeffs_rho();
    let enc = LigeroEncoding::<Ft63>::new(coeffs.len(), rho);
    let comm = commit::<Blake3, LigeroEncoding<_>>(&coeffs, &enc).unwrap();
    // this is the polynomial commitment
    let root = comm.get_root();

    // evaluate the random polynomial we just generated at a random point x
    let x = Ft63::random(&mut rand::rng());
    let eval = comm
        .coeffs
        .iter()
        .zip(iterate(Ft63::ONE, |&v| v * x).take(coeffs.len()))
        .fold(Ft63::ZERO, |acc, (c, r)| acc + *c * r);

    // compute the outer and inner tensors for powers of x
    // NOTE: we treat coeffs as a univariate polynomial, but it doesn't
    // really matter --- the only difference from a multilinear is the
    // way we compute outer_tensor and inner_tensor from the eval point
    let inner_tensor: Vec<Ft63> = iterate(Ft63::ONE, |&v| v * x)
        .take(comm.n_per_row)
        .collect();
    let outer_tensor: Vec<Ft63> = {
        let xr = x * inner_tensor.last().unwrap();
        iterate(Ft63::ONE, |&v| v * xr)
            .take(comm.n_rows)
            .collect()
    };

    // compute an evaluation proof
    let mut tr1 = Transcript::new(b"test transcript");
    tr1.append_message(b"polycommit", root.as_ref());
    tr1.append_message(b"rate", &rho.to_be_bytes()[..]);
    tr1.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
    let pf = prove::<Blake3, _>(&comm, &outer_tensor[..], &enc, &mut tr1).unwrap();

    let challenge_after_first_proof_prover = {
        let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
        tr1.challenge_bytes(b"ligero-pc//challenge", &mut key);
        let mut deg_test_rng = ChaCha20Rng::from_seed(key);
        Ft63::random(&mut deg_test_rng)
    };

    // produce a second proof with the same transcript
    tr1.append_message(b"polycommit", root.as_ref());
    tr1.append_message(b"rate", &rho.to_be_bytes()[..]);
    tr1.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
    let pf2 = prove::<Blake3, _>(&comm, &outer_tensor[..], &enc, &mut tr1).unwrap();

    // verify it and finish evaluation
    let mut tr2 = Transcript::new(b"test transcript");
    tr2.append_message(b"polycommit", root.as_ref());
    tr2.append_message(b"rate", &rho.to_be_bytes()[..]);
    tr2.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
    let enc2 = LigeroEncoding::<Ft63>::new_from_dims(pf.get_n_per_row(), pf.get_n_cols());
    let res = verify(
        root.as_ref(),
        &outer_tensor[..],
        &inner_tensor[..],
        &pf,
        &enc2,
        &mut tr2,
    )
    .unwrap();

    assert_eq!(res, eval);
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
    tr2.append_message(b"rate", &rho.to_be_bytes()[..]);
    tr2.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
    let enc3 = LigeroEncoding::<Ft63>::new_from_dims(pf2.get_n_per_row(), pf2.get_n_cols());
    let res2 = verify(
        root.as_ref(),
        &outer_tensor[..],
        &inner_tensor[..],
        &pf2,
        &enc3,
        &mut tr2,
    )
    .unwrap();

    assert_eq!(res2, eval);
}

fn check_leaf_hasher_field<F: Field + FieldHash>(seed: u64) {
    use super::LeafHasher;

    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    for &len in &[0usize, 1, 61, 62, 63, 64, 65, 123, 124, 125, 1000] {
        let col: Vec<F> = repeat_with(|| F::random(&mut rng)).take(len).collect();

        let mut reference = Blake3::new();
        reference.update(<Output<Blake3> as Default>::default());
        for e in &col {
            e.digest_update(&mut reference);
        }
        let expected = reference.finalize();

        let mut batched = LeafHasher::<Blake3>::new();
        for e in &col {
            batched.push(e);
        }
        let got = batched.finalize();

        assert_eq!(got, expected, "LeafHasher mismatch for column length {len}");
    }
}

#[test]
fn leaf_hasher_matches_reference() {
    use super::LeafHasher;

    check_leaf_hasher_field::<Ft63>(0x1eaf_0063);
    check_leaf_hasher_field::<Ft127>(0x1eaf_0127);

    // Exercise the oversized-representation direct-update path (> 1024 bytes).
    struct BigElem(Vec<u8>);
    impl FieldHash for BigElem {
        type HashRepr = Vec<u8>;
        fn to_hash_repr(&self) -> Self::HashRepr {
            self.0.clone()
        }
    }
    let items = [
        BigElem(vec![0x11; 16]),
        BigElem(vec![0x22; 1500]),
        BigElem(vec![0x33; 20]),
        BigElem(vec![0x44; 2048]),
    ];
    let mut reference = Blake3::new();
    reference.update(<Output<Blake3> as Default>::default());
    for item in &items {
        item.digest_update(&mut reference);
    }
    let mut batched = LeafHasher::<Blake3>::new();
    for item in &items {
        batched.push(item);
    }
    assert_eq!(batched.finalize(), reference.finalize());
}

#[test]
fn merkle_root_matches_commit_and_reference() {
    use super::{commit, merkle_root};

    // Compare against LcCommit::commit root.
    let (coeffs, rho) = random_coeffs_rho();
    let enc = LigeroEncoding::<Ft63>::new(coeffs.len(), rho);
    let comm = commit::<Blake3, LigeroEncoding<_>>(&coeffs, &enc).unwrap();
    let root = merkle_root::<Blake3, Ft63>(&comm.comm, comm.n_rows, comm.n_cols);
    assert_eq!(root, *comm.get_root().as_ref());

    // Compare against a per-element reference across power-of-two and non-power-of-two column counts.
    let mut rng = ChaCha20Rng::seed_from_u64(0x9007_1270);
    for &(n_rows, n_cols) in &[
        (1usize, 1usize),
        (3, 2),
        (5, 3),
        (65, 15),
        (65, 16),
        (65, 17),
        (10, 31),
        (10, 32),
        (10, 33),
        (70, 65),
    ] {
        let matrix: Vec<Ft127> = repeat_with(|| Ft127::random(&mut rng))
            .take(n_rows * n_cols)
            .collect();

        let leaf_count = n_cols.next_power_of_two();
        let mut level: Vec<Output<Blake3>> = (0..leaf_count)
            .map(|col| {
                if col >= n_cols {
                    return <Output<Blake3> as Default>::default();
                }
                let mut d = Blake3::new();
                d.update(<Output<Blake3> as Default>::default());
                for row in 0..n_rows {
                    matrix[row * n_cols + col].digest_update(&mut d);
                }
                d.finalize()
            })
            .collect();
        while level.len() > 1 {
            level = level
                .chunks_exact(2)
                .map(|pair| {
                    let mut d = Blake3::new();
                    d.update(pair[0].as_slice());
                    d.update(pair[1].as_slice());
                    d.finalize()
                })
                .collect();
        }
        let expected = level.pop().unwrap();
        let got = merkle_root::<Blake3, Ft127>(&matrix, n_rows, n_cols);
        assert_eq!(got, expected, "merkle_root mismatch for {n_rows}x{n_cols}");
    }
}

#[test]
fn verify_rejects_tampered_repeated_opening() {
    use super::{
        commit, prove, verify, verify_column_path, verify_column_value, LegacyUniformUsize,
        VerifierError,
    };

    let mut rng = ChaCha20Rng::seed_from_u64(0xded0_0001);
    let n_per_row = 16usize;
    let n_cols = 32usize;
    let n_rows = 70usize;
    let coeffs: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
        .take(n_rows * n_per_row)
        .collect();
    let enc = LigeroEncoding::<Ft63>::new_from_dims(n_per_row, n_cols);
    let comm = commit::<Blake3, _>(&coeffs, &enc).unwrap();
    let root = comm.get_root();

    let x = Ft63::random(&mut rng);
    let inner_tensor: Vec<Ft63> = iterate(Ft63::ONE, |&v| v * x).take(n_per_row).collect();
    let outer_tensor: Vec<Ft63> = {
        let xr = x * inner_tensor.last().unwrap();
        iterate(Ft63::ONE, |&v| v * xr).take(n_rows).collect()
    };

    let make_tr = || {
        let mut tr = Transcript::new(b"repeated opening test");
        tr.append_message(b"polycommit", root.as_ref());
        tr.append_message(b"ncols", &(N_COL_OPENS as u64).to_be_bytes()[..]);
        tr
    };

    let mut tr_prove = make_tr();
    let pf = prove(&comm, &outer_tensor, &enc, &mut tr_prove).unwrap();
    let mut tr_ok = make_tr();
    verify(
        root.as_ref(),
        &outer_tensor,
        &inner_tensor,
        &pf,
        &enc,
        &mut tr_ok,
    )
    .unwrap();

    // Replay the Fiat-Shamir challenges to locate a repeated opening and build
    // the full-check reference expectation for any tampered column.
    let mut tr_replay = make_tr();
    let n_degree_tests = enc.get_n_degree_tests();
    let mut rand_tensor_vec = Vec::with_capacity(n_degree_tests);
    let mut p_random_fft = Vec::with_capacity(n_degree_tests);
    for i in 0..n_degree_tests {
        let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
        tr_replay.challenge_bytes(<LigeroEncoding<Ft63> as LcEncoding>::LABEL_DT, &mut key);
        let mut deg_rng = ChaCha20Rng::from_seed(key);
        let rand_tensor: Vec<Ft63> = repeat_with(|| Ft63::random(&mut deg_rng))
            .take(n_rows)
            .collect();
        rand_tensor_vec.push(rand_tensor);

        let mut tmp = pf.p_random_vec[i].clone();
        tmp.resize(n_cols, Ft63::ZERO);
        enc.encode(&mut tmp).unwrap();
        p_random_fft.push(tmp);

        pf.p_random_vec[i]
            .iter()
            .for_each(|c| c.transcript_update(&mut tr_replay, <LigeroEncoding<Ft63> as LcEncoding>::LABEL_PR));
    }
    pf.p_eval
        .iter()
        .for_each(|c| c.transcript_update(&mut tr_replay, <LigeroEncoding<Ft63> as LcEncoding>::LABEL_PE));

    let mut col_key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
    tr_replay.challenge_bytes(<LigeroEncoding<Ft63> as LcEncoding>::LABEL_CO, &mut col_key);
    let mut cols_rng = ChaCha20Rng::from_seed(col_key);
    let col_range = LegacyUniformUsize::new(0, n_cols);
    let cols_to_open: Vec<usize> = repeat_with(|| col_range.sample(&mut cols_rng))
        .take(N_COL_OPENS)
        .collect();

    let mut p_eval_fft = pf.p_eval.clone();
    p_eval_fft.resize(n_cols, Ft63::ZERO);
    enc.encode(&mut p_eval_fft).unwrap();

    // Find the first occurrence `f` and second occurrence `k` of a repeated column.
    let mut seen = vec![None; n_cols];
    let mut repeated = None;
    for (idx, &c) in cols_to_open.iter().enumerate() {
        if let Some(first_idx) = seen[c] {
            repeated = Some((c, first_idx, idx));
            break;
        }
        seen[c] = Some(idx);
    }
    let (col_num, first_idx, second_idx) =
        repeated.expect("N_COL_OPENS > n_cols guarantees a repeated column opening");
    assert_eq!(pf.columns[first_idx].col, pf.columns[second_idx].col);
    assert_eq!(pf.columns[first_idx].path, pf.columns[second_idx].path);

    let full_check = |column: &super::LcColumn<Blake3, LigeroEncoding<Ft63>>| {
        let mut rand = true;
        for i in 0..n_degree_tests {
            rand &= verify_column_value(column, &rand_tensor_vec[i], &p_random_fft[i][col_num]);
        }
        let eval = verify_column_value(column, &outer_tensor, &p_eval_fft[col_num]);
        let path = verify_column_path(column, col_num, root.as_ref());
        match (rand, eval, path) {
            (false, _, _) => Err(VerifierError::ColumnDegree),
            (_, false, _) => Err(VerifierError::ColumnEval),
            (_, _, false) => Err(VerifierError::ColumnPath),
            _ => Ok(()),
        }
    };

    // Case 1: change one element in the second copy of the repeated opening.
    let mut pf_bad_elem = pf.clone();
    pf_bad_elem.columns[second_idx].col[0] += Ft63::ONE;
    let expected_elem_err = full_check(&pf_bad_elem.columns[second_idx]).unwrap_err();
    let mut tr_elem = make_tr();
    let got_elem_err = verify(
        root.as_ref(),
        &outer_tensor,
        &inner_tensor,
        &pf_bad_elem,
        &enc,
        &mut tr_elem,
    )
    .unwrap_err();
    assert_eq!(
        std::mem::discriminant(&got_elem_err),
        std::mem::discriminant(&expected_elem_err),
        "expected {expected_elem_err:?}, got {got_elem_err:?}"
    );

    // Case 2: change one Merkle path entry in the second copy of the repeated opening.
    let mut pf_bad_path = pf.clone();
    pf_bad_path.columns[second_idx].path[0][0] ^= 0x01;
    let expected_path_err = full_check(&pf_bad_path.columns[second_idx]).unwrap_err();
    assert!(matches!(expected_path_err, VerifierError::ColumnPath));
    let mut tr_path = make_tr();
    let got_path_err = verify(
        root.as_ref(),
        &outer_tensor,
        &inner_tensor,
        &pf_bad_path,
        &enc,
        &mut tr_path,
    )
    .unwrap_err();
    assert_eq!(
        std::mem::discriminant(&got_path_err),
        std::mem::discriminant(&expected_path_err),
        "expected {expected_path_err:?}, got {got_path_err:?}"
    );

    // Case 3: tamper every copy of `col_num` identically so all repeats match `first_idx`;
    // `first_idx` must still run the full check and reject with ColumnPath.
    let mut pf_bad_all = pf.clone();
    for (idx, &c) in cols_to_open.iter().enumerate() {
        if c == col_num {
            pf_bad_all.columns[idx].path[0][0] ^= 0x01;
        }
    }
    let mut tr_all = make_tr();
    let got_all_err = verify(
        root.as_ref(),
        &outer_tensor,
        &inner_tensor,
        &pf_bad_all,
        &enc,
        &mut tr_all,
    )
    .unwrap_err();
    assert!(
        matches!(got_all_err, VerifierError::ColumnPath),
        "expected ColumnPath, got {got_all_err:?}"
    );
}

#[test]
fn open_columns_matches_open_column() {
    use super::{merkleize, open_column, open_columns, ProverError};

    let mut rng = ChaCha20Rng::seed_from_u64(0x09e4_c015);

    // Test both a small matrix (< 16 rows) and a larger matrix whose row count
    // is not a multiple of 16 and whose column count spans multiple column tiles.
    for &(n_rows, n_per_row, n_cols) in &[(5usize, 7usize, 16usize), (37, 45, 128)] {
        let coeffs: Vec<Ft127> = repeat_with(|| Ft127::random(&mut rng))
            .take(n_rows * n_per_row)
            .collect();
        let comm_data: Vec<Ft127> = repeat_with(|| Ft127::random(&mut rng))
            .take(n_rows * n_cols)
            .collect();
        let mut test_comm = LigeroCommit::<Blake3, Ft127> {
            comm: comm_data,
            coeffs,
            n_rows,
            n_cols,
            n_per_row,
            hashes: vec![<Output<Blake3> as Default>::default(); 2 * n_cols - 1],
        };
        merkleize(&mut test_comm);

        let all_rev: Vec<usize> = (0..n_cols).rev().collect();
        let mut all_shuffled: Vec<usize> = (0..n_cols).collect();
        for i in (1..n_cols).rev() {
            let j = (rng.random::<u64>() as usize) % (i + 1);
            all_shuffled.swap(i, j);
        }
        let random_with_repeats: Vec<usize> = (0..n_cols * 2 + 13)
            .map(|_| (rng.random::<u64>() as usize) % n_cols)
            .collect();
        let explicit_repeats = vec![
            n_cols - 1,
            3,
            n_cols - 1,
            n_cols - 1,
            0,
            3,
            n_cols / 2,
            0,
        ];

        let cases: Vec<Vec<usize>> = vec![
            vec![],
            vec![0],
            vec![n_cols - 1],
            vec![5, 5, 5, 5],
            explicit_repeats,
            all_rev,
            all_shuffled,
            random_with_repeats,
        ];

        for indices in &cases {
            let opened = open_columns(&test_comm, indices).unwrap();
            assert_eq!(opened.len(), indices.len());
            for (got, &col_idx) in opened.iter().zip(indices.iter()) {
                let expected = open_column(&test_comm, col_idx).unwrap();
                assert_eq!(got.col, expected.col);
                assert_eq!(got.path, expected.path);
            }
        }

        // Out-of-range indices must return ColumnNumber.
        for bad in [
            vec![n_cols],
            vec![0, 3, n_cols, 1],
            vec![n_cols + 17],
        ] {
            assert!(matches!(
                open_columns(&test_comm, &bad),
                Err(ProverError::ColumnNumber)
            ));
        }
    }
}

#[test]
fn commit_matches_serial_reference_unaligned() {
    use super::{commit, merkleize_ser};

    let mut rng = ChaCha20Rng::seed_from_u64(0xc0a1_7001);

    // Cover non-multiples of n_per_row (single partial row, 1 full + 1 elem,
    // nearly full last row, multi-row unaligned) as well as an exact multiple.
    let cases = [
        (1usize, 13usize, 32usize),
        (14, 13, 32),
        (3 * 13 + 12, 13, 32),
        (37 * 45 + 19, 45, 64),
        (17 * 30, 30, 64),
    ];

    for &(len, n_per_row, n_cols) in &cases {
        let coeffs_in: Vec<Ft127> = repeat_with(|| Ft127::random(&mut rng)).take(len).collect();
        let enc = LigeroEncoding::<Ft127>::new_from_dims(n_per_row, n_cols);
        let got = commit::<Blake3, _>(&coeffs_in, &enc).unwrap();

        let (n_rows, ref_n_per_row, ref_n_cols) = enc.get_dims(coeffs_in.len());
        assert_eq!(ref_n_per_row, n_per_row);
        assert_eq!(ref_n_cols, n_cols);

        let mut ref_coeffs = vec![Ft127::ZERO; n_rows * n_per_row];
        for (dst, src) in ref_coeffs
            .chunks_mut(n_per_row)
            .zip(coeffs_in.chunks(n_per_row))
        {
            dst[..src.len()].copy_from_slice(src);
        }

        let mut ref_comm = vec![Ft127::ZERO; n_rows * n_cols];
        for (dst, src) in ref_comm
            .chunks_mut(n_cols)
            .zip(ref_coeffs.chunks(n_per_row))
        {
            dst[..src.len()].copy_from_slice(src);
        }

        enc.encode_rows(&mut ref_comm, n_cols).unwrap();

        let n_cols_np2 = n_cols.next_power_of_two();
        let mut expected = LigeroCommit::<Blake3, Ft127> {
            comm: ref_comm,
            coeffs: ref_coeffs,
            n_rows,
            n_cols,
            n_per_row,
            hashes: vec![<Output<Blake3> as Default>::default(); 2 * n_cols_np2 - 1],
        };
        merkleize_ser(&mut expected);

        assert_eq!(got.n_rows, expected.n_rows);
        assert_eq!(got.n_per_row, expected.n_per_row);
        assert_eq!(got.n_cols, expected.n_cols);
        assert_eq!(got.coeffs, expected.coeffs, "coeffs mismatch for len={len}");
        assert_eq!(got.comm, expected.comm, "comm mismatch for len={len}");
        assert_eq!(got.hashes, expected.hashes, "hashes mismatch for len={len}");
        assert_eq!(
            got.get_root().as_ref(),
            expected.get_root().as_ref(),
            "root mismatch for len={len}"
        );
    }
}

fn random_coeffs_rho() -> (Vec<Ft63>, f64) {
    let mut rng = rand::rng();

    let lgl = 8 + (rng.random::<u64>() as usize) % 8;
    let len_base = 1 << (lgl - 1);
    let len = len_base + ((rng.random::<u64>() as usize) % len_base);

    (
        repeat_with(|| Ft63::random(&mut rng)).take(len).collect(),
        rng.random_range(0.1f64..0.9f64),
    )
}

fn random_comm() -> LigeroCommit<Blake3, Ft63> {
    let mut rng = rand::rng();

    let lgl = 8 + (rng.random::<u64>() as usize) % 8;
    let len_base = 1 << (lgl - 1);
    let len = len_base + ((rng.random::<u64>() as usize) % len_base);
    let rho = rng.random_range(0.1f64..0.9f64);
    let (n_rows, n_per_row, n_cols) = LigeroEncoding::<Ft63>::_get_dims(len, rho).unwrap();

    let coeffs_len = (n_per_row - 1) * n_rows + 1 + ((rng.random::<u64>() as usize) % n_rows);
    let coeffs = {
        let mut tmp = repeat_with(|| Ft63::random(&mut rng))
            .take(coeffs_len)
            .collect::<Vec<Ft63>>();
        tmp.resize(n_per_row * n_rows, Ft63::ZERO);
        tmp
    };

    let comm_len = n_rows * n_cols;
    let comm: Vec<Ft63> = repeat_with(|| Ft63::random(&mut rng))
        .take(comm_len)
        .collect();

    LigeroCommit::<Blake3, Ft63> {
        comm,
        coeffs,
        n_rows,
        n_cols,
        n_per_row,
        hashes: vec![<Output<Blake3> as Default>::default(); 2 * n_cols - 1],
    }
}
