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
// stable Rust and current dependencies; column-major batched encoding interface;
// row-collapse and column inner-product hooks on LcEncoding; batched leaf and
// Merkle hashing, merkle_root helper, repeated-opening deduplication and
// batched verifier encodings; tiled deduplicated column opening and parallel
// single-pass buffer initialisation in commit.
// The per-commit changes are in the git history since step/00-import
// (upstream conroi/lcpc f0e2116).
#![deny(missing_docs)]
#![allow(non_local_definitions)]

/*!
lcpc2d is a polynomial commitment scheme based on linear codes
*/

use digest::{Digest, Output};
use err_derive::Error;
use ff::{Field, PrimeField};
use merlin::Transcript;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::iter::repeat_with;

/// Legacy `rand 0.8`-compatible uniform `usize` sampler on 64-bit targets.
///
/// In `rand 0.8`, `Uniform<usize>` on 64-bit targets sampled a `u64` word from the
/// RNG (`rng.next_u64()`) and applied Lemire's widen-multiply rejection method with
/// `ints_to_reject = (u64::MAX - range + 1) % range` and `zone = u64::MAX - ints_to_reject`,
/// accepting `lo <= zone`.
///
/// In `rand 0.9` and `rand 0.10`, `UniformUsize` switched to 32-bit sampling whenever
/// `high <= u32::MAX as usize` and changed the rejection predicate to `lo >= thresh`.
/// Preserving `rand 0.8`'s 64-bit `Uniform<usize>` algorithm keeps expander/RS graph
/// generation (`lcpc-brakedown-pc::matgen`) and Fiat-Shamir column opening indices
/// (`lcpc-2d::{prove, verify}`) 100% bit-exact with upstream LCPC (`f0e2116`).
#[derive(Clone, Copy, Debug)]
pub struct LegacyUniformUsize {
    low: u64,
    range: u64,
    zone: u64,
}

impl LegacyUniformUsize {
    /// Construct a half-open uniform sampler over `[low, high)`.
    #[inline]
    pub fn new(low: usize, high: usize) -> Self {
        assert!(low < high, "LegacyUniformUsize::new called with `low >= high`");
        let low_u64 = low as u64;
        let range = (high as u64).wrapping_sub(low_u64);
        let ints_to_reject = if range > 0 {
            (u64::MAX - range + 1) % range
        } else {
            0
        };
        let zone = u64::MAX - ints_to_reject;
        Self {
            low: low_u64,
            range,
            zone,
        }
    }

    /// Sample a uniform `usize` in `[low, high)` using `rand 0.8`'s 64-bit Lemire algorithm.
    #[inline]
    pub fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> usize {
        loop {
            let v: u64 = rng.next_u64();
            let mul = (v as u128).wrapping_mul(self.range as u128);
            let hi = (mul >> 64) as u64;
            let lo = mul as u64;
            if lo <= self.zone {
                return self.low.wrapping_add(hi) as usize;
            }
        }
    }
}

mod macros;

#[cfg(test)]
mod tests;

/// Trait for a field element that can be hashed via [digest::Digest]
pub trait FieldHash {
    /// A representation of `Self` that can be converted to a slice of `u8`.
    type HashRepr: AsRef<[u8]>;

    /// Convert `Self` into a `HashRepr` for hashing
    fn to_hash_repr(&self) -> Self::HashRepr;

    /// Update the digest `d` with the `HashRepr` of `Self`
    fn digest_update<D: Digest>(&self, d: &mut D) {
        d.update(self.to_hash_repr())
    }

    /// Update the [merlin::Transcript] `t` with the `HashRepr` of `Self` with label `l`
    fn transcript_update(&self, t: &mut Transcript, l: &'static [u8]) {
        t.append_message(l, self.to_hash_repr().as_ref())
    }
}

impl<T: PrimeField> FieldHash for T {
    type HashRepr = T::Repr;

    fn to_hash_repr(&self) -> Self::HashRepr {
        PrimeField::to_repr(self)
    }
}

/// Trait representing bit size information for a field
pub trait SizedField {
    /// Ceil of log2(cardinality)
    const CLOG2: u32;
    /// Floor of log2(cardinality)
    const FLOG2: u32;
}

impl<T: PrimeField> SizedField for T {
    const CLOG2: u32 = <T as PrimeField>::NUM_BITS;
    const FLOG2: u32 = <T as PrimeField>::NUM_BITS - 1;
}

/// Trait for a linear encoding used by the polycommit
pub trait LcEncoding: Clone + std::fmt::Debug + Sync {
    /// Field over which coefficients are defined
    type F: Field + FieldHash + std::fmt::Debug + Clone;

    /// Domain separation label - degree test (see def_labels!())
    const LABEL_DT: &'static [u8];
    /// Domain separation label - random lin combs (see def_labels!())
    const LABEL_PR: &'static [u8];
    /// Domain separation label - eval comb (see def_labels!())
    const LABEL_PE: &'static [u8];
    /// Domain separation label - column openings (see def_labels!())
    const LABEL_CO: &'static [u8];

    /// Error type for encoding
    type Err: std::fmt::Debug + std::error::Error + Send;

    /// Encoding function
    fn encode<T: AsMut<[Self::F]>>(&self, inp: T) -> Result<(), Self::Err>;

    /// Encode every row of a row-major matrix of codewords in place.
    ///
    /// `comm` holds `comm.len() / n_cols` rows of `n_cols` elements each. On entry,
    /// the first `n_per_row` elements of each row hold that row's message (the rest
    /// of the row is ignored); on return, each row holds the same codeword that
    /// [`LcEncoding::encode`] produces for that message.
    ///
    /// The default implementation encodes the rows independently in parallel, as
    /// upstream LCPC's `commit` did. Encodings that can process many rows at once
    /// (for example, the batched column-major encoder of `lcpc-brakedown-pc`)
    /// override it; the result must be identical.
    fn encode_rows(&self, comm: &mut [Self::F], n_cols: usize) -> Result<(), Self::Err> {
        assert_eq!(comm.len() % n_cols, 0);
        comm.par_chunks_mut(n_cols).try_for_each(|r| self.encode(r))
    }

    /// Get dimensions for this encoding instance on an input vector of length `len`
    fn get_dims(&self, len: usize) -> (usize, usize, usize);

    /// Check that supplied dimensions are compatible with this encoding
    fn dims_ok(&self, n_per_row: usize, n_cols: usize) -> bool;

    /// Get the number of column openings required for this encoding
    fn get_n_col_opens(&self) -> usize;

    /// Get the number of degree tests required for this encoding
    fn get_n_degree_tests(&self) -> usize;

    /// Add row-wise linear combinations of `rows` into `polys`.
    ///
    /// `rows` is a row-major matrix whose length is a multiple of `n_per_row`.
    /// Each `tensors[t]` supplies one weight per row of `rows`, and each
    /// `polys[t]` has length `n_per_row`; on return,
    /// `polys[t][j] += sum_i tensors[t][i] * rows[i * n_per_row + j]`.
    fn collapse_rows(
        &self,
        rows: &[Self::F],
        n_per_row: usize,
        tensors: &[&[Self::F]],
        polys: &mut [Vec<Self::F>],
    ) {
        for (tensor, poly) in tensors.iter().zip(polys.iter_mut()) {
            for (row, w) in rows.chunks_exact(n_per_row).zip(tensor.iter()) {
                for (p, c) in poly.iter_mut().zip(row) {
                    *p += *c * w;
                }
            }
        }
    }

    /// Compute inner products of `col` with each tensor in `tensors`.
    ///
    /// Sets `out[t]` to the dot product over the common prefix of `tensors[t]`
    /// and `col`. Equal lengths are not assumed because a malformed proof may
    /// carry columns of a different length.
    fn column_dots(&self, col: &[Self::F], tensors: &[&[Self::F]], out: &mut [Self::F]) {
        for (tensor, o) in tensors.iter().zip(out.iter_mut()) {
            *o = tensor
                .iter()
                .zip(col)
                .fold(Self::F::ZERO, |a, (t, e)| a + *t * e);
        }
    }
}

// local accessors for enclosed types
type FldT<E> = <E as LcEncoding>::F;
type ErrT<E> = <E as LcEncoding>::Err;

/// Err variant for prover operations
#[allow(non_local_definitions)]
#[derive(Debug, Error)]
pub enum ProverError<ErrT>
where
    ErrT: std::fmt::Debug + std::error::Error + 'static,
{
    /// size too big
    #[error(display = "n_cols is too large for this encoding")]
    TooBig,
    /// error encoding a vector
    #[error(display = "encoding error: {:?}", _0)]
    Encode(#[source] ErrT),
    /// inconsistent LcCommit fields
    #[error(display = "inconsistent commitment fields")]
    Commit,
    /// bad column number
    #[error(display = "bad column number")]
    ColumnNumber,
    /// bad outer tensor
    #[error(display = "outer tensor: wrong size")]
    OuterTensor,
}

/// result of a prover operation
pub type ProverResult<T, ErrT> = Result<T, ProverError<ErrT>>;

/// Err variant for verifier operations
#[allow(non_local_definitions)]
#[derive(Debug, Error)]
pub enum VerifierError<ErrT>
where
    ErrT: std::fmt::Debug + std::error::Error + 'static,
{
    /// wrong number of column openings in proof
    #[error(display = "wrong number of column openings in proof")]
    NumColOpens,
    /// failed to verify column merkle path
    #[error(display = "column verification: merkle path failed")]
    ColumnPath,
    /// failed to verify column dot product for poly eval
    #[error(display = "column verification: eval dot product failed")]
    ColumnEval,
    /// failed to verify column dot product for degree test
    #[error(display = "column verification: degree test dot product failed")]
    ColumnDegree,
    /// bad outer tensor
    #[error(display = "outer tensor: wrong size")]
    OuterTensor,
    /// bad inner tensor
    #[error(display = "inner tensor: wrong size")]
    InnerTensor,
    /// encoding dimensions do not match proof
    #[error(display = "encoding dimension mismatch")]
    EncodingDims,
    /// error encoding a vector
    #[error(display = "encoding error: {:?}", _0)]
    Encode(#[source] ErrT),
}

/// result of a verifier operation
pub type VerifierResult<T, ErrT> = Result<T, VerifierError<ErrT>>;

/// a commitment
#[derive(Debug, Clone)]
pub struct LcCommit<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    comm: Vec<FldT<E>>,
    coeffs: Vec<FldT<E>>,
    n_rows: usize,
    n_cols: usize,
    n_per_row: usize,
    hashes: Vec<Output<D>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WrappedLcCommit<F>
where
    F: Serialize,
{
    comm: Vec<F>,
    coeffs: Vec<F>,
    n_rows: usize,
    n_cols: usize,
    n_per_row: usize,
    hashes: Vec<WrappedOutput>,
}

impl<F> WrappedLcCommit<F>
where
    F: Serialize,
{
    /// turn a WrappedLcCommit into an LcCommit
    fn unwrap<D, E>(self) -> LcCommit<D, E>
    where
        D: Digest,
        E: LcEncoding<F = F>,
    {
        let hashes = self.hashes.into_iter().map(|c| c.unwrap::<D, E>().root).collect();

        LcCommit {
            comm: self.comm,
            coeffs: self.coeffs,
            n_rows: self.n_rows,
            n_cols: self.n_cols,
            n_per_row: self.n_per_row,
            hashes,
        }
    }
}

impl<D, E> LcCommit<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize,
{
    fn wrapped(&self) -> WrappedLcCommit<FldT<E>> {
        let hashes_wrapped = self.hashes.iter().map(|h| WrappedOutput { bytes: h.to_vec() }).collect();

        WrappedLcCommit {
            comm: self.comm.clone(),
            coeffs: self.coeffs.clone(),
            n_rows: self.n_rows,
            n_cols: self.n_cols,
            n_per_row: self.n_per_row,
            hashes: hashes_wrapped,
        }
    }
}

impl<D, E> Serialize for LcCommit<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.wrapped().serialize(serializer)
    }
}

impl<'de, D, E> Deserialize<'de> for LcCommit<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize + Deserialize<'de>,
{
    fn deserialize<De>(deserializer: De) -> Result<Self, De::Error>
    where
        De: Deserializer<'de>,
    {
        Ok(WrappedLcCommit::<FldT<E>>::deserialize(deserializer)?.unwrap())
    }
}

impl<D, E> LcCommit<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    /// returns the Merkle root of this polynomial commitment (which is the commitment itself)
    pub fn get_root(&self) -> LcRoot<D, E> {
        LcRoot {
            root: self.hashes.last().cloned().unwrap(),
            _p: Default::default(),
        }
    }

    /// return the number of coefficients encoded in each matrix row
    pub fn get_n_per_row(&self) -> usize {
        self.n_per_row
    }

    /// return the number of columns in the encoded matrix
    pub fn get_n_cols(&self) -> usize {
        self.n_cols
    }

    /// return the number of rows in the encoded matrix
    pub fn get_n_rows(&self) -> usize {
        self.n_rows
    }

    /// generate a commitment to a polynomial
    pub fn commit(coeffs: &[FldT<E>], enc: &E) -> ProverResult<Self, ErrT<E>> {
        commit(coeffs, enc)
    }

    /// Generate an evaluation of a committed polynomial
    pub fn prove(
        &self,
        outer_tensor: &[FldT<E>],
        enc: &E,
        tr: &mut Transcript,
    ) -> ProverResult<LcEvalProof<D, E>, ErrT<E>> {
        prove(self, outer_tensor, enc, tr)
    }
}

/// A Merkle root corresponding to a committed polynomial
#[derive(Debug, Clone)]
pub struct LcRoot<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    root: Output<D>,
    _p: std::marker::PhantomData<E>,
}

impl<D, E> LcRoot<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    fn wrapped(&self) -> WrappedOutput {
        WrappedOutput {
            bytes: self.root.to_vec(),
        }
    }

    /// Convert this value into a raw Output<D>
    pub fn into_raw(self) -> Output<D> {
        self.root
    }
}

impl<D, E> AsRef<Output<D>> for LcRoot<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    fn as_ref(&self) -> &Output<D> {
        &self.root
    }
}

// support impl for serializing and deserializing proofs
#[derive(Debug, Clone, Deserialize, Serialize)]
struct WrappedOutput {
    /// wrapped output
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

impl WrappedOutput {
    fn unwrap<D, E>(self) -> LcRoot<D, E>
    where
        D: Digest,
        E: LcEncoding,
    {
        LcRoot {
            root: self.bytes.into_iter().collect::<Output<D>>(),
            _p: Default::default(),
        }
    }
}

impl<D, E> Serialize for LcRoot<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.wrapped().serialize(serializer)
    }
}

impl<'de, D, E> Deserialize<'de> for LcRoot<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    fn deserialize<De>(deserializer: De) -> Result<Self, De::Error>
    where
        De: Deserializer<'de>,
    {
        Ok(WrappedOutput::deserialize(deserializer)?.unwrap())
    }
}

/// A column opening and the corresponding Merkle path.
#[derive(Debug, Clone)]
pub struct LcColumn<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    col: Vec<FldT<E>>,
    path: Vec<Output<D>>,
}

impl<D, E> LcColumn<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize,
{
    fn wrapped(&self) -> WrappedLcColumn<FldT<E>> {
        let path_wrapped = (0..self.path.len())
            .map(|i| WrappedOutput {
                bytes: self.path[i].to_vec(),
            })
            .collect();

        WrappedLcColumn {
            col: self.col.clone(),
            path: path_wrapped,
        }
    }
}

// A column opening and the corresponding Merkle path.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct WrappedLcColumn<F>
where
    F: Serialize,
{
    col: Vec<F>,
    path: Vec<WrappedOutput>,
}

impl<F> WrappedLcColumn<F>
where
    F: Serialize,
{
    /// turn WrappedLcColumn into LcColumn
    fn unwrap<D, E>(self) -> LcColumn<D, E>
    where
        D: Digest,
        E: LcEncoding<F = F>,
    {
        let col = self.col;
        let path = self
            .path
            .into_iter()
            .map(|v| v.bytes.into_iter().collect::<Output<D>>())
            .collect();

        LcColumn { col, path }
    }
}

impl<D, E> Serialize for LcColumn<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.wrapped().serialize(serializer)
    }
}

impl<'de, D, E> Deserialize<'de> for LcColumn<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize + Deserialize<'de>,
{
    fn deserialize<De>(deserializer: De) -> Result<Self, De::Error>
    where
        De: Deserializer<'de>,
    {
        Ok(WrappedLcColumn::<FldT<E>>::deserialize(deserializer)?.unwrap())
    }
}

/// An evaluation and proof of its correctness and of the low-degreeness of the commitment.
#[derive(Debug, Clone)]
pub struct LcEvalProof<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    n_cols: usize,
    p_eval: Vec<FldT<E>>,
    p_random_vec: Vec<Vec<FldT<E>>>,
    columns: Vec<LcColumn<D, E>>,
}

impl<D, E> LcEvalProof<D, E>
where
    D: Digest,
    E: LcEncoding,
{
    /// Get the number of elements in an encoded vector
    pub fn get_n_cols(&self) -> usize {
        self.n_cols
    }

    /// Get the number of elements in an unencoded vector
    pub fn get_n_per_row(&self) -> usize {
        self.p_eval.len()
    }

    /// Verify an evaluation proof and return the resulting evaluation
    pub fn verify(
        &self,
        root: &Output<D>,
        outer_tensor: &[FldT<E>],
        inner_tensor: &[FldT<E>],
        enc: &E,
        tr: &mut Transcript,
    ) -> VerifierResult<FldT<E>, ErrT<E>> {
        verify(root, outer_tensor, inner_tensor, self, enc, tr)
    }
}

impl<D, E> LcEvalProof<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize,
{
    fn wrapped(&self) -> WrappedLcEvalProof<FldT<E>> {
        let columns_wrapped = (0..self.columns.len())
            .map(|i| self.columns[i].wrapped())
            .collect();

        WrappedLcEvalProof {
            n_cols: self.n_cols,
            p_eval: self.p_eval.clone(),
            p_random_vec: self.p_random_vec.clone(),
            columns: columns_wrapped,
        }
    }
}

/// An evaluation and proof of its correctness and of the low-degreeness of the commitment.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WrappedLcEvalProof<F>
where
    F: Serialize,
{
    n_cols: usize,
    p_eval: Vec<F>,
    p_random_vec: Vec<Vec<F>>,
    columns: Vec<WrappedLcColumn<F>>,
}

impl<F> WrappedLcEvalProof<F>
where
    F: Serialize,
{
    /// turn a WrappedLcEvalProof into an LcEvalProof
    fn unwrap<D, E>(self) -> LcEvalProof<D, E>
    where
        D: Digest,
        E: LcEncoding<F = F>,
    {
        let columns = self.columns.into_iter().map(|c| c.unwrap()).collect();

        LcEvalProof {
            n_cols: self.n_cols,
            p_eval: self.p_eval,
            p_random_vec: self.p_random_vec,
            columns,
        }
    }
}

impl<D, E> Serialize for LcEvalProof<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.wrapped().serialize(serializer)
    }
}

impl<'de, D, E> Deserialize<'de> for LcEvalProof<D, E>
where
    D: Digest,
    E: LcEncoding,
    E::F: Serialize + Deserialize<'de>,
{
    fn deserialize<De>(deserializer: De) -> Result<Self, De::Error>
    where
        De: Deserializer<'de>,
    {
        Ok(WrappedLcEvalProof::<FldT<E>>::deserialize(deserializer)?.unwrap())
    }
}

/// Compute number of degree tests required for `lambda`-bit security
/// for a code with `len`-length codewords over `flog2`-bit field
pub fn n_degree_tests(lambda: usize, len: usize, flog2: usize) -> usize {
    let den = flog2 - log2(len);
    (lambda + den - 1) / den
}

// parallelization limit when working on columns
const LOG_MIN_NCOLS: usize = 5;

// base-case column block width for batched leaf hashing
const HASH_COLUMNS_BLOCK: usize = 32;

// stack buffer size for batched leaf hashing (1 KiB = one BLAKE3 chunk)
const LEAF_HASH_BUF_LEN: usize = 1024;

// tile dimensions for deduplicated column opening
const OPEN_TILE_ROWS: usize = 8;
const OPEN_TILE_COLS: usize = 64;

struct LeafHasher<D: Digest> {
    digest: D,
    buf: [u8; LEAF_HASH_BUF_LEN],
    len: usize,
}

impl<D: Digest> LeafHasher<D> {
    #[inline]
    fn new() -> Self {
        let mut digest = D::new();
        // column hashes start with a block of 0's of digest output length
        let prefix = <Output<D> as Default>::default();
        let prefix_bytes = prefix.as_ref();
        let mut buf = [0u8; LEAF_HASH_BUF_LEN];
        let len = if prefix_bytes.len() <= LEAF_HASH_BUF_LEN {
            buf[..prefix_bytes.len()].copy_from_slice(prefix_bytes);
            prefix_bytes.len()
        } else {
            digest.update(prefix_bytes);
            0
        };
        Self { digest, buf, len }
    }

    #[inline]
    fn push<F: FieldHash>(&mut self, e: &F) {
        let repr = e.to_hash_repr();
        let bytes = repr.as_ref();
        if bytes.len() > LEAF_HASH_BUF_LEN - self.len {
            if self.len > 0 {
                self.digest.update(&self.buf[..self.len]);
                self.len = 0;
            }
            if bytes.len() > LEAF_HASH_BUF_LEN {
                self.digest.update(bytes);
                return;
            }
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    #[inline]
    fn finalize(mut self) -> Output<D> {
        if self.len > 0 {
            self.digest.update(&self.buf[..self.len]);
        }
        self.digest.finalize()
    }
}

#[inline]
fn hash_merkle_node<D: Digest>(left: &Output<D>, right: &Output<D>) -> Output<D> {
    let mut digest = D::new();
    let l = left.as_ref();
    let r = right.as_ref();
    let n = l.len();
    if 2 * n <= 128 {
        let mut buf = [0u8; 128];
        buf[..n].copy_from_slice(l);
        buf[n..2 * n].copy_from_slice(r);
        digest.update(&buf[..2 * n]);
    } else {
        digest.update(l);
        digest.update(r);
    }
    digest.finalize()
}

/// Commit to a univariate polynomial whose coefficients are `coeffs` using encoding `enc`
fn commit<D, E>(coeffs_in: &[FldT<E>], enc: &E) -> ProverResult<LcCommit<D, E>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    let (n_rows, n_per_row, n_cols) = enc.get_dims(coeffs_in.len());

    // check that parameters are ok
    assert!(n_rows * n_per_row >= coeffs_in.len());
    assert!((n_rows - 1) * n_per_row < coeffs_in.len());
    assert!(enc.dims_ok(n_per_row, n_cols));
    assert!(n_per_row > 0);
    assert!(n_per_row <= n_cols);
    assert_eq!(coeffs_in.len().div_ceil(n_per_row), n_rows);

    // matrix (encoded as a vector), each buffer initialised in one parallel pass
    // XXX(zk) pad coeffs
    let coeffs_len = n_rows.checked_mul(n_per_row).expect("coeffs size overflow");
    let mut coeffs: Vec<FldT<E>> = Vec::with_capacity(coeffs_len);
    coeffs.spare_capacity_mut()[..coeffs_len]
        .par_chunks_mut(n_per_row)
        .zip(coeffs_in.par_chunks(n_per_row))
        .for_each(|(dst, src)| {
            let (head, tail) = dst.split_at_mut(src.len());
            for (d, s) in head.iter_mut().zip(src) {
                d.write(*s);
            }
            for d in tail {
                d.write(FldT::<E>::ZERO);
            }
        });
    // SAFETY: `coeffs_in.len().div_ceil(n_per_row) == n_rows`, so both sides have
    // `n_rows` chunks with `dst.len() == n_per_row >= src.len()`, and each of the
    // `coeffs_len` elements was written exactly once.
    unsafe { coeffs.set_len(coeffs_len) };

    let comm_len = n_rows.checked_mul(n_cols).expect("comm size overflow");
    let mut comm: Vec<FldT<E>> = Vec::with_capacity(comm_len);
    comm.spare_capacity_mut()[..comm_len]
        .par_chunks_mut(n_cols)
        .zip(coeffs_in.par_chunks(n_per_row))
        .for_each(|(dst, src)| {
            let (head, tail) = dst.split_at_mut(src.len());
            for (d, s) in head.iter_mut().zip(src) {
                d.write(*s);
            }
            for d in tail {
                d.write(FldT::<E>::ZERO);
            }
        });
    // SAFETY: `coeffs_in.len().div_ceil(n_per_row) == n_rows` and `n_per_row <= n_cols`,
    // so both sides have `n_rows` chunks with `dst.len() == n_cols >= src.len()`, and
    // each of the `comm_len` elements was written exactly once.
    unsafe { comm.set_len(comm_len) };

    // now encode all rows (batched for encodings that support it)
    enc.encode_rows(&mut comm, n_cols)?;

    // compute Merkle tree
    let n_cols_np2 = n_cols
        .checked_next_power_of_two()
        .ok_or(ProverError::TooBig)?;
    let mut ret = LcCommit {
        comm,
        coeffs,
        n_rows,
        n_cols,
        n_per_row,
        hashes: vec![<Output<D> as Default>::default(); 2 * n_cols_np2 - 1],
    };
    check_comm(&ret, enc)?;
    merkleize(&mut ret);

    Ok(ret)
}

fn check_comm<D, E>(comm: &LcCommit<D, E>, enc: &E) -> ProverResult<(), ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    let comm_sz = comm.comm.len() != comm.n_rows * comm.n_cols;
    let coeff_sz = comm.coeffs.len() != comm.n_rows * comm.n_per_row;
    let hashlen = comm.hashes.len() != 2 * comm.n_cols.next_power_of_two() - 1;
    let dims = !enc.dims_ok(comm.n_per_row, comm.n_cols);

    if comm_sz || coeff_sz || hashlen || dims {
        Err(ProverError::Commit)
    } else {
        Ok(())
    }
}

/// Compute the column-wise Merkle root of a row-major `n_rows` x `n_cols` matrix.
///
/// Each column is hashed in row order after a leading zero block of `D`'s output
/// size, padded to the next power of two with default digests, and reduced into
/// a binary Merkle tree. The resulting root matches [`LcCommit::commit`].
pub fn merkle_root<D, F>(matrix: &[F], n_rows: usize, n_cols: usize) -> Output<D>
where
    D: Digest,
    F: FieldHash + Sync,
{
    assert_eq!(matrix.len(), n_rows * n_cols);
    let n_cols_np2 = n_cols.next_power_of_two();
    let mut hashes = vec![<Output<D> as Default>::default(); 2 * n_cols_np2 - 1];
    hash_columns::<D, F>(matrix, &mut hashes[..n_cols], n_rows, n_cols, 0);
    let (hin, hout) = hashes.split_at_mut(n_cols_np2);
    if !hout.is_empty() {
        merkle_tree::<D>(hin, hout);
    }
    hashes.pop().unwrap()
}

fn merkleize<D, E>(comm: &mut LcCommit<D, E>)
where
    D: Digest,
    E: LcEncoding,
{
    // step 1: hash each column of the commitment (we always reveal a full column)
    let hashes = &mut comm.hashes[..comm.n_cols];
    hash_columns::<D, FldT<E>>(&comm.comm, hashes, comm.n_rows, comm.n_cols, 0);

    // step 2: compute rest of Merkle tree
    let len_plus_one = comm.hashes.len() + 1;
    assert!(len_plus_one.is_power_of_two());
    let (hin, hout) = comm.hashes.split_at_mut(len_plus_one / 2);
    if !hout.is_empty() {
        merkle_tree::<D>(hin, hout);
    }
}

fn hash_columns<D, F>(
    comm: &[F],
    hashes: &mut [Output<D>],
    n_rows: usize,
    n_cols: usize,
    offset: usize,
) where
    D: Digest,
    F: FieldHash + Sync,
{
    hashes
        .par_chunks_mut(HASH_COLUMNS_BLOCK)
        .enumerate()
        .for_each(|(block_idx, block_hashes)| {
            let col_offset = offset + block_idx * HASH_COLUMNS_BLOCK;
            let width = block_hashes.len();
            let mut hashers: Vec<LeafHasher<D>> =
                (0..width).map(|_| LeafHasher::new()).collect();
            for row in 0..n_rows {
                let row_start = row * n_cols + col_offset;
                let row_slice = &comm[row_start..row_start + width];
                for (elem, hasher) in row_slice.iter().zip(hashers.iter_mut()) {
                    hasher.push(elem);
                }
            }
            for (dst, hasher) in block_hashes.iter_mut().zip(hashers) {
                *dst = hasher.finalize();
            }
        });
}

fn merkle_tree<D>(ins: &[Output<D>], outs: &mut [Output<D>])
where
    D: Digest,
{
    // array should always be of length 2^k - 1
    assert_eq!(ins.len(), outs.len() + 1);

    let (outs, rems) = outs.split_at_mut((outs.len() + 1) / 2);
    merkle_layer::<D>(ins, outs);

    if !rems.is_empty() {
        merkle_tree::<D>(outs, rems)
    }
}

fn merkle_layer<D>(ins: &[Output<D>], outs: &mut [Output<D>])
where
    D: Digest,
{
    assert_eq!(ins.len(), 2 * outs.len());

    if ins.len() <= (1 << LOG_MIN_NCOLS) {
        // base case: just compute all of the hashes
        for idx in 0..outs.len() {
            outs[idx] = hash_merkle_node::<D>(&ins[2 * idx], &ins[2 * idx + 1]);
        }
    } else {
        // recursive case: split and compute
        let (inl, inr) = ins.split_at(ins.len() / 2);
        let (outl, outr) = outs.split_at_mut(outs.len() / 2);
        rayon::join(
            || merkle_layer::<D>(inl, outl),
            || merkle_layer::<D>(inr, outr),
        );
    }
}

// Extract the Merkle authentication path for one column.
fn merkle_path<D: Digest>(
    hashes: &[Output<D>],
    n_cols: usize,
    mut column: usize,
) -> Vec<Output<D>> {
    let mut hashes = hashes;
    let path_len = log2(n_cols);
    let mut path = Vec::with_capacity(path_len);
    for _ in 0..path_len {
        let other = (column & !1) | (!column & 1);
        assert_eq!(other ^ column, 1);
        path.push(hashes[other].clone());
        let (_, hashes_new) = hashes.split_at((hashes.len() + 1) / 2);
        hashes = hashes_new;
        column >>= 1;
    }
    assert_eq!(column, 0);
    path
}

// Open the commitment to one column
#[cfg(test)]
fn open_column<D, E>(comm: &LcCommit<D, E>, column: usize) -> ProverResult<LcColumn<D, E>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    // make sure arguments are well formed
    if column >= comm.n_cols {
        return Err(ProverError::ColumnNumber);
    }

    // column of values
    let col = comm
        .comm
        .iter()
        .skip(column)
        .step_by(comm.n_cols)
        .cloned()
        .collect();

    // Merkle path
    let path = merkle_path::<D>(&comm.hashes, comm.n_cols, column);

    Ok(LcColumn { col, path })
}

// Gather a sorted slice of unique columns from a row-major matrix in row-by-column tiles.
fn gather_columns<F: Copy>(comm: &[F], n_rows: usize, n_cols: usize, cols: &[usize]) -> Vec<Vec<F>> {
    assert_eq!(comm.len(), n_rows * n_cols);
    let mut out: Vec<Vec<F>> = cols.iter().map(|_| Vec::with_capacity(n_rows)).collect();
    for rows in comm.chunks(OPEN_TILE_ROWS * n_cols) {
        for (ct, ot) in cols
            .chunks(OPEN_TILE_COLS)
            .zip(out.chunks_mut(OPEN_TILE_COLS))
        {
            for (&c, o) in ct.iter().zip(ot.iter_mut()) {
                o.extend(rows.chunks_exact(n_cols).map(|row| row[c]));
            }
        }
    }
    out
}

// Open the commitment to a list of columns, gathering each unique column once in
// tiles and cloning repeated openings while each chunk is still warm in cache.
fn open_columns<D, E>(
    comm: &LcCommit<D, E>,
    cols_to_open: &[usize],
) -> ProverResult<Vec<LcColumn<D, E>>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    if cols_to_open.iter().any(|&c| c >= comm.n_cols) {
        return Err(ProverError::ColumnNumber);
    }
    if cols_to_open.is_empty() {
        return Ok(Vec::new());
    }

    let mut uniq = cols_to_open.to_vec();
    uniq.sort_unstable();
    uniq.dedup();

    let mut slot = vec![0usize; comm.n_cols];
    for (s, &c) in uniq.iter().enumerate() {
        slot[c] = s;
    }

    let mut counts = vec![0usize; uniq.len()];
    for &c in cols_to_open {
        counts[slot[c]] += 1;
    }

    let n_threads = rayon::current_num_threads().max(1);
    let chunk_size = uniq.len().div_ceil(n_threads).clamp(16, OPEN_TILE_COLS);

    let chunks: Vec<Vec<Vec<LcColumn<D, E>>>> = uniq
        .par_chunks(chunk_size)
        .zip(counts.par_chunks(chunk_size))
        .map(|(cols, cnts)| {
            let cols_data = gather_columns(&comm.comm, comm.n_rows, comm.n_cols, cols);
            cols_data
                .into_iter()
                .zip(cols.iter().zip(cnts.iter()))
                .map(|(col, (&c, &cnt))| {
                    let path = merkle_path::<D>(&comm.hashes, comm.n_cols, c);
                    let mut copies = Vec::with_capacity(cnt);
                    for _ in 1..cnt {
                        copies.push(LcColumn {
                            col: col.clone(),
                            path: path.clone(),
                        });
                    }
                    copies.push(LcColumn { col, path });
                    copies
                })
                .collect()
        })
        .collect();

    let mut buckets: Vec<Vec<LcColumn<D, E>>> = Vec::with_capacity(uniq.len());
    for chunk in chunks {
        buckets.extend(chunk);
    }

    let columns = cols_to_open
        .iter()
        .map(|&c| buckets[slot[c]].pop().unwrap())
        .collect();

    Ok(columns)
}

const fn log2(v: usize) -> usize {
    (63 - (v.next_power_of_two() as u64).leading_zeros()) as usize
}

/// Verify the evaluation of a committed polynomial and return the result
fn verify<D, E>(
    root: &Output<D>,
    outer_tensor: &[FldT<E>],
    inner_tensor: &[FldT<E>],
    proof: &LcEvalProof<D, E>,
    enc: &E,
    tr: &mut Transcript,
) -> VerifierResult<FldT<E>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    // make sure arguments are well formed
    let n_col_opens = enc.get_n_col_opens();
    if n_col_opens != proof.columns.len() || n_col_opens == 0 {
        return Err(VerifierError::NumColOpens);
    }
    let n_rows = proof.columns[0].col.len();
    let n_cols = proof.get_n_cols();
    let n_per_row = proof.get_n_per_row();
    if inner_tensor.len() != n_per_row {
        return Err(VerifierError::InnerTensor);
    }
    if outer_tensor.len() != n_rows {
        return Err(VerifierError::OuterTensor);
    }
    if !enc.dims_ok(n_per_row, n_cols) {
        return Err(VerifierError::EncodingDims);
    }

    // encode all degree-test polynomials and p_eval together in one batched call
    let n_degree_tests = enc.get_n_degree_tests();
    let mut encoded_polys = vec![FldT::<E>::ZERO; (n_degree_tests + 1) * n_cols];
    for i in 0..n_degree_tests {
        let src = &proof.p_random_vec[i];
        let copy_len = src.len().min(n_cols);
        encoded_polys[i * n_cols..i * n_cols + copy_len].copy_from_slice(&src[..copy_len]);
    }
    let eval_copy_len = proof.p_eval.len().min(n_cols);
    encoded_polys[n_degree_tests * n_cols..n_degree_tests * n_cols + eval_copy_len]
        .copy_from_slice(&proof.p_eval[..eval_copy_len]);
    enc.encode_rows(&mut encoded_polys, n_cols)?;

    // step 1: random tensor for degree test and random columns to test
    // step 1a: extract random tensor from transcript
    // we run multiple instances of this to boost soundness
    let mut rand_tensor_vec: Vec<Vec<FldT<E>>> = Vec::with_capacity(n_degree_tests);
    for i in 0..n_degree_tests {
        let rand_tensor: Vec<FldT<E>> = {
            let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
            tr.challenge_bytes(E::LABEL_DT, &mut key);
            let mut deg_test_rng = ChaCha20Rng::from_seed(key);
            // XXX(optimization) could expand seed in parallel instead of in series
            repeat_with(|| FldT::<E>::random(&mut deg_test_rng))
                .take(n_rows)
                .collect()
        };

        rand_tensor_vec.push(rand_tensor);

        // step 1b: push p_random into transcript
        proof.p_random_vec[i]
            .iter()
            .for_each(|coeff| coeff.transcript_update(tr, E::LABEL_PR));
    }

    proof
        .p_eval
        .iter()
        .for_each(|coeff| coeff.transcript_update(tr, E::LABEL_PE));

    // step 1c: extract columns to open
    let cols_to_open: Vec<usize> = {
        let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
        tr.challenge_bytes(E::LABEL_CO, &mut key);
        let mut cols_rng = ChaCha20Rng::from_seed(key);
        // XXX(optimization) could expand seed in parallel instead of in series
        let col_range = LegacyUniformUsize::new(0usize, n_cols);
        repeat_with(|| col_range.sample(&mut cols_rng))
            .take(n_col_opens)
            .collect()
    };

    // record the first opening index for each column so identical repeats can be skipped;
    // the first opening always runs the full check, so accept/reject and error kinds are unchanged.
    assert!(cols_to_open.len() <= u32::MAX as usize);
    let mut first = vec![u32::MAX; n_cols];
    for (k, &c) in cols_to_open.iter().enumerate() {
        if first[c] == u32::MAX {
            first[c] = k as u32;
        }
    }

    // step 2: check p_random, p_eval, and col paths
    let p_eval_fft = &encoded_polys[n_degree_tests * n_cols..];
    let mut tensors: Vec<&[FldT<E>]> = Vec::with_capacity(n_degree_tests + 1);
    for t in &rand_tensor_vec {
        tensors.push(t.as_slice());
    }
    tensors.push(outer_tensor);

    cols_to_open
        .par_iter()
        .zip(&proof.columns[..])
        .enumerate()
        .try_for_each_init(
            || vec![FldT::<E>::ZERO; n_degree_tests + 1],
            |dots, (k, (&col_num, column))| {
                let f = first[col_num] as usize;
                if f != k
                    && column.col == proof.columns[f].col
                    && column.path == proof.columns[f].path
                {
                    return Ok(());
                }

                enc.column_dots(&column.col, &tensors, dots);
                let mut rand = true;
                for i in 0..n_degree_tests {
                    rand &= dots[i] == encoded_polys[i * n_cols + col_num];
                }

                let eval = dots[n_degree_tests] == p_eval_fft[col_num];
                let path = verify_column_path(column, col_num, root);
                match (rand, eval, path) {
                    (false, _, _) => Err(VerifierError::ColumnDegree),
                    (_, false, _) => Err(VerifierError::ColumnEval),
                    (_, _, false) => Err(VerifierError::ColumnPath),
                    _ => Ok(()),
                }
            },
        )?;

    // step 3: evaluate and return
    let mut out = [FldT::<E>::ZERO; 1];
    enc.column_dots(&proof.p_eval, &[inner_tensor], &mut out);
    Ok(out[0])
}

// Check a column opening
fn verify_column_path<D, E>(column: &LcColumn<D, E>, col_num: usize, root: &Output<D>) -> bool
where
    D: Digest,
    E: LcEncoding,
{
    let mut hasher = LeafHasher::<D>::new();
    for e in &column.col[..] {
        hasher.push(e);
    }

    // check Merkle path
    let mut hash = hasher.finalize();
    let mut col = col_num;
    for p in &column.path[..] {
        hash = if col % 2 == 0 {
            hash_merkle_node::<D>(&hash, p)
        } else {
            hash_merkle_node::<D>(p, &hash)
        };
        col >>= 1;
    }

    &hash == root
}

// check column value
#[cfg(test)]
fn verify_column_value<D, E>(
    column: &LcColumn<D, E>,
    tensor: &[FldT<E>],
    poly_eval: &FldT<E>,
) -> bool
where
    D: Digest,
    E: LcEncoding,
{
    let tensor_eval = tensor
        .iter()
        .zip(&column.col[..])
        .fold(FldT::<E>::ZERO, |a, (t, e)| a + *t * e);

    poly_eval == &tensor_eval
}

fn collapse_par<E>(
    enc: &E,
    coeffs: &[FldT<E>],
    n_per_row: usize,
    tensors: &[&[FldT<E>]],
) -> Vec<Vec<FldT<E>>>
where
    E: LcEncoding,
{
    if tensors.is_empty() {
        return Vec::new();
    }
    if n_per_row == 0 || coeffs.is_empty() {
        return vec![vec![FldT::<E>::ZERO; n_per_row]; tensors.len()];
    }
    assert_eq!(coeffs.len() % n_per_row, 0);
    let n_rows = coeffs.len() / n_per_row;
    for t in tensors {
        assert_eq!(t.len(), n_rows);
    }

    let n_threads = rayon::current_num_threads().max(1);
    let rows_per_task = n_rows.div_ceil(n_threads);

    let partials: Vec<Vec<Vec<FldT<E>>>> = coeffs
        .par_chunks(rows_per_task * n_per_row)
        .enumerate()
        .map(|(k, chunk)| {
            let chunk_rows = chunk.len() / n_per_row;
            let row_start = k * rows_per_task;
            let row_end = row_start + chunk_rows;
            let task_tensors: Vec<&[FldT<E>]> =
                tensors.iter().map(|t| &t[row_start..row_end]).collect();
            let mut polys = vec![vec![FldT::<E>::ZERO; n_per_row]; tensors.len()];
            enc.collapse_rows(chunk, n_per_row, &task_tensors, &mut polys);
            polys
        })
        .collect();

    let mut iter = partials.into_iter();
    let mut acc = iter.next().unwrap();
    for part in iter {
        for (dst_poly, src_poly) in acc.iter_mut().zip(part.iter()) {
            for (d, s) in dst_poly.iter_mut().zip(src_poly.iter()) {
                *d += s;
            }
        }
    }
    acc
}

/// Evaluate the committed polynomial using the supplied "outer" tensor
/// and generate a proof of (1) low-degreeness and (2) correct evaluation.
fn prove<D, E>(
    comm: &LcCommit<D, E>,
    outer_tensor: &[FldT<E>],
    enc: &E,
    tr: &mut Transcript,
) -> ProverResult<LcEvalProof<D, E>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    // make sure arguments are well formed
    check_comm(comm, enc)?;
    if outer_tensor.len() != comm.n_rows {
        return Err(ProverError::OuterTensor);
    }

    // first, evaluate the polynomial on random tensors (low-degree test);
    // degree test 0 and outer_tensor are both known before p_random_0 enters
    // the transcript, so they are collapsed together in one pass.
    let n_degree_tests = enc.get_n_degree_tests();
    let mut p_random_vec: Vec<Vec<FldT<E>>> = Vec::with_capacity(n_degree_tests);
    let mut p_eval_opt: Option<Vec<FldT<E>>> = None;

    for i in 0..n_degree_tests {
        let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
        tr.challenge_bytes(E::LABEL_DT, &mut key);
        let mut deg_test_rng = ChaCha20Rng::from_seed(key);
        // XXX(optimization) could expand seed in parallel instead of in series
        let rand_tensor: Vec<FldT<E>> = repeat_with(|| FldT::<E>::random(&mut deg_test_rng))
            .take(comm.n_rows)
            .collect();

        let p_random = if i == 0 {
            let mut collapsed = collapse_par(
                enc,
                &comm.coeffs,
                comm.n_per_row,
                &[&rand_tensor, outer_tensor],
            );
            p_eval_opt = collapsed.pop();
            collapsed.pop().unwrap()
        } else {
            collapse_par(enc, &comm.coeffs, comm.n_per_row, &[&rand_tensor])
                .pop()
                .unwrap()
        };

        // add p_random to the transcript
        p_random
            .iter()
            .for_each(|coeff| coeff.transcript_update(tr, E::LABEL_PR));

        p_random_vec.push(p_random);
    }

    // next, evaluate the polynomial using the supplied tensor (computed alone only if n_degree_tests == 0)
    let p_eval = match p_eval_opt {
        Some(poly) => poly,
        None => collapse_par(enc, &comm.coeffs, comm.n_per_row, &[outer_tensor])
            .pop()
            .unwrap(),
    };
    // add p_eval to the transcript
    p_eval
        .iter()
        .for_each(|coeff| coeff.transcript_update(tr, E::LABEL_PE));

    // now extract the column numbers to open
    let n_col_opens = enc.get_n_col_opens();
    let columns: Vec<LcColumn<D, E>> = {
        let mut key: <ChaCha20Rng as SeedableRng>::Seed = Default::default();
        tr.challenge_bytes(E::LABEL_CO, &mut key);
        let mut cols_rng = ChaCha20Rng::from_seed(key);
        // XXX(optimization) could expand seed in parallel instead of in series
        let col_range = LegacyUniformUsize::new(0usize, comm.n_cols);
        let cols_to_open: Vec<usize> = repeat_with(|| col_range.sample(&mut cols_rng))
            .take(n_col_opens)
            .collect();
        open_columns(comm, &cols_to_open)?
    };

    Ok(LcEvalProof {
        n_cols: comm.n_cols,
        p_eval,
        p_random_vec,
        columns,
    })
}

#[cfg(test)]
fn collapse_columns<E>(
    coeffs: &[FldT<E>],
    tensor: &[FldT<E>],
    poly: &mut [FldT<E>],
    n_rows: usize,
    n_per_row: usize,
    offset: usize,
) where
    E: LcEncoding,
{
    if poly.len() <= (1 << LOG_MIN_NCOLS) {
        // base case: run the computation
        // row-by-row, compute elements of dot product
        for (row, tensor_val) in tensor.iter().enumerate() {
            for (col, val) in poly.iter_mut().enumerate() {
                let entry = row * n_per_row + offset + col;
                *val += coeffs[entry] * tensor_val;
            }
        }
    } else {
        // recursive case: split and execute in parallel
        let half_cols = poly.len() / 2;
        let (lo, hi) = poly.split_at_mut(half_cols);
        rayon::join(
            || collapse_columns::<E>(coeffs, tensor, lo, n_rows, n_per_row, offset),
            || collapse_columns::<E>(coeffs, tensor, hi, n_rows, n_per_row, offset + half_cols),
        );
    }
}

// TESTING ONLY //

#[cfg(test)]
fn merkleize_ser<D, E>(comm: &mut LcCommit<D, E>)
where
    D: Digest,
    E: LcEncoding,
{
    let hashes = &mut comm.hashes;

    // hash each column
    for (col, hash) in hashes.iter_mut().enumerate().take(comm.n_cols) {
        let mut digest = D::new();
        digest.update(<Output<D> as Default>::default());
        for row in 0..comm.n_rows {
            comm.comm[row * comm.n_cols + col].digest_update(&mut digest);
        }
        *hash = digest.finalize();
    }

    // compute rest of Merkle tree
    let (mut ins, mut outs) = hashes.split_at_mut(comm.n_cols);
    while !outs.is_empty() {
        for idx in 0..ins.len() / 2 {
            let mut digest = D::new();
            digest.update(ins[2 * idx].as_ref());
            digest.update(ins[2 * idx + 1].as_ref());
            outs[idx] = digest.finalize();
        }
        let (new_ins, new_outs) = outs.split_at_mut((outs.len() + 1) / 2);
        ins = new_ins;
        outs = new_outs;
    }
}

#[cfg(test)]
// Check a column opening
fn verify_column<D, E>(
    column: &LcColumn<D, E>,
    col_num: usize,
    root: &Output<D>,
    tensor: &[FldT<E>],
    poly_eval: &FldT<E>,
) -> bool
where
    D: Digest,
    E: LcEncoding,
{
    verify_column_path(column, col_num, root) && verify_column_value(column, tensor, poly_eval)
}

// Evaluate the committed polynomial using the "outer" tensor
#[cfg(test)]
fn eval_outer<D, E>(
    comm: &LcCommit<D, E>,
    tensor: &[FldT<E>],
) -> ProverResult<Vec<FldT<E>>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    if tensor.len() != comm.n_rows {
        return Err(ProverError::OuterTensor);
    }

    // allocate result and compute
    let mut poly = vec![FldT::<E>::ZERO; comm.n_per_row];
    collapse_columns::<E>(
        &comm.coeffs,
        tensor,
        &mut poly,
        comm.n_rows,
        comm.n_per_row,
        0,
    );

    Ok(poly)
}

#[cfg(test)]
fn eval_outer_ser<D, E>(
    comm: &LcCommit<D, E>,
    tensor: &[FldT<E>],
) -> ProverResult<Vec<FldT<E>>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    if tensor.len() != comm.n_rows {
        return Err(ProverError::OuterTensor);
    }

    let mut poly = vec![FldT::<E>::ZERO; comm.n_per_row];
    for (row, tensor_val) in tensor.iter().enumerate() {
        for (col, val) in poly.iter_mut().enumerate() {
            let entry = row * comm.n_per_row + col;
            *val += comm.coeffs[entry] * tensor_val;
        }
    }

    Ok(poly)
}

#[cfg(test)]
fn eval_outer_fft<D, E>(
    comm: &LcCommit<D, E>,
    tensor: &[FldT<E>],
) -> ProverResult<Vec<FldT<E>>, ErrT<E>>
where
    D: Digest,
    E: LcEncoding,
{
    if tensor.len() != comm.n_rows {
        return Err(ProverError::OuterTensor);
    }

    let mut poly_fft = vec![FldT::<E>::ZERO; comm.n_cols];
    for (coeffs, tensorval) in comm.comm.chunks(comm.n_cols).zip(tensor.iter()) {
        for (coeff, polyval) in coeffs.iter().zip(poly_fft.iter_mut()) {
            *polyval += *coeff * tensorval;
        }
    }

    Ok(poly_fft)
}
