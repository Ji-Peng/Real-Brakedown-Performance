// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.
#![deny(missing_docs)]

/*!
sdig-pc is a polynomial commitment scheme from the SDIG expander code
*/

use codespec::{CodeParams, LcpcParams, SdigCode3 as SdigCodeDflt, SdigSpecification};
use encode::{encode, encode_col_major, transpose_into, ExpanderCode};
use matgen::generate_code;

use ff::Field;
use lcpc_2d::{
    def_labels, n_degree_tests, FieldHash, LcCommit, LcEncoding, LcEvalProof, SizedField,
};
use num_traits::Num;

pub mod codespec;
pub mod encode;
pub mod graph;
pub mod matgen;

// Re-export portable lazy-reduction fields for Brakedown PCS callers.
pub use lcpc_fields::{
    self, Fp2M61, Fp2M61Acc, LazyField, Mers127, Mers127Acc, Mont127, Mont127Acc,
};

#[cfg(test)]
mod tests;

/// Encoding definition for SDIG expander-based polycommit
///
/// The type parameter `S` selects the LCPC code specification used by the
/// upstream constructors [`new`](Self::new), [`new_ml`](Self::new_ml), and
/// [`new_from_dims`](Self::new_from_dims). Since `step/04-r2l` the encoding
/// itself is driven by runtime [`CodeParams`] (stored in the value), so the
/// same type can also hold a SIMD-Spielman (mode (b)) code; see
/// [`SpielmanEncoding`] and [`with_params`](Self::with_params).
#[derive(Clone, Debug)]
pub struct SdigEncodingS<Ft, S> {
    n_per_row: usize, // number of inputs to the encoding
    n_cols: usize,    // number of outputs from the encoding
    params: CodeParams,
    code: ExpanderCode<Ft>,
    _p: std::marker::PhantomData<S>,
}

impl<Ft, S> SdigEncodingS<Ft, S>
where
    Ft: Field + Num + SizedField,
{
    const LAMBDA: usize = 128;

    // number of column openings required for soundness, for relative distance `dist`
    fn n_col_opens_for(dist: f64) -> usize {
        let dist_ov_3 = dist / 3f64;
        let den = (1f64 - dist_ov_3).log2();
        (-(Self::LAMBDA as f64) / den).ceil() as usize
    }

    // number of degree tests required for soundness
    fn n_degree_tests_for(n_cols: usize) -> usize {
        n_degree_tests(Self::LAMBDA, n_cols, Ft::FLOG2 as usize)
    }

    // shared between new_with_params and new_ml_with_params
    fn new_from_np1(params: CodeParams, len: usize, np1: usize, seed: u64) -> Self {
        // n_per_row can't be greater than length!
        let np1 = if np1 > len { len } else { np1 };

        let n_col_opens = Self::n_col_opens_for(params.dist());
        let nr1 = (len + np1 - 1) / np1;
        let nd1 = Self::n_degree_tests_for(np1 * 2); // approximately
        assert!(np1 * nr1 >= len);
        assert!(np1 * (nr1 - 1) < len);

        let np2 = np1 / 2;
        let nr2 = (len + np2 - 1) / np2;
        let nd2 = Self::n_degree_tests_for(np2 * 2); // approximately
        assert!(np2 * nr2 >= len);
        assert!(np2 * (nr2 - 1) < len);

        let sz1 = n_col_opens * nr1 + (1 + nd1) * np1;
        let sz2 = n_col_opens * nr2 + (1 + nd2) * np2;
        let n_per_row = if sz1 < sz2 { np1 } else { np2 };

        Self::with_params(params, n_per_row, seed)
    }

    /// Create an encoding for a univariate polynomial with `len` coefficients,
    /// with code parameters `params` and a random code sampled from `seed`.
    ///
    /// This is [`new`](Self::new) with runtime code parameters.
    pub fn new_with_params(params: CodeParams, len: usize, seed: u64) -> Self {
        // compute #cols, optimizing the communication cost
        let lncf = (Self::n_col_opens_for(params.dist()) * len) as f64;
        // approximation of num_degree_tests
        let ndt = Self::n_degree_tests_for(lncf.sqrt().ceil() as usize * 2) as f64;
        let np1 = (lncf / ndt).sqrt().ceil() as usize;
        Self::new_from_np1(params, len, np1, seed)
    }

    /// Create an encoding for a multilinear polynomial with `n_vars` variables,
    /// with code parameters `params` and a random code sampled from `seed`.
    ///
    /// This is [`new_ml`](Self::new_ml) with runtime code parameters.
    pub fn new_ml_with_params(params: CodeParams, n_vars: usize, seed: u64) -> Self {
        let n_monomials = 1 << n_vars;
        let lncf = (Self::n_col_opens_for(params.dist()) * n_monomials) as f64;
        // approximation of num_degree_tests
        let ndt = Self::n_degree_tests_for(lncf.sqrt().ceil() as usize * 2) as f64;
        let np1 = ((lncf / ndt).sqrt().ceil() as usize)
            .checked_next_power_of_two()
            .unwrap();
        Self::new_from_np1(params, n_monomials, np1, seed)
    }

    /// Create an encoding with message length `n_per_row`, code parameters
    /// `params`, and a random code sampled from `seed`.
    pub fn with_params(params: CodeParams, n_per_row: usize, seed: u64) -> Self {
        let code = generate_code::<Ft>(&params, n_per_row, seed);
        assert_eq!(n_per_row, code.n_per_row());
        Self::from_code(params, code)
    }

    /// Create an encoding from an already constructed code, e.g. one assembled
    /// from externally sampled graphs with [`ExpanderCode::from_graphs`].
    ///
    /// `params` only determines the PCS soundness parameters (number of column
    /// openings); the code is used as given.
    pub fn from_code(params: CodeParams, code: ExpanderCode<Ft>) -> Self {
        Self {
            n_per_row: code.n_per_row(),
            n_cols: code.codeword_len(),
            params,
            code,
            _p: std::marker::PhantomData,
        }
    }

    /// The code-parameter mode of this encoding.
    pub fn params(&self) -> &CodeParams {
        &self.params
    }

    /// The expander code (graphs and base case) of this encoding.
    pub fn code(&self) -> &ExpanderCode<Ft> {
        &self.code
    }
}

impl<Ft, S> SdigEncodingS<Ft, S>
where
    Ft: Field + Num + SizedField,
    S: SdigSpecification,
{
    // number of column openings required for soundness
    fn _n_col_opens() -> usize {
        Self::n_col_opens_for(S::dist())
    }

    fn lcpc_params() -> CodeParams {
        CodeParams::Lcpc(LcpcParams::of::<S>())
    }

    /// Create a new SdigEncoding for a univariate polynomial with `len` coefficients
    /// using a random expander code generated with seed `seed`.
    pub fn new(len: usize, seed: u64) -> Self {
        Self::new_with_params(Self::lcpc_params(), len, seed)
    }

    /// Create a new SdigEncoding for a multilinear polynomial with `n_vars` variables
    /// (i.e., 2^`n_vars` monomials) using a random expander code generated with seed `seed`.
    pub fn new_ml(n_vars: usize, seed: u64) -> Self {
        Self::new_ml_with_params(Self::lcpc_params(), n_vars, seed)
    }

    /// Create a new SdigEncoding for a commitment with dimensions `n_per_row` and `n_cols`
    pub fn new_from_dims(n_per_row: usize, n_cols: usize, seed: u64) -> Self {
        let enc = Self::with_params(Self::lcpc_params(), n_per_row, seed);
        assert_eq!(n_cols, enc.n_cols);
        enc
    }
}

impl<Ft, S> LcEncoding for SdigEncodingS<Ft, S>
where
    Ft: Field + FieldHash + Num + SizedField,
    S: std::fmt::Debug + std::clone::Clone + std::marker::Sync,
{
    type F = Ft;
    type Err = std::io::Error;

    def_labels!(sdig_pc);

    fn encode<T: AsMut<[Ft]>>(&self, inp: T) -> Result<(), Self::Err> {
        encode(inp, &self.code);
        Ok(())
    }

    /// Encode all rows in one batched pass over a column-major copy of the matrix.
    ///
    /// The messages are transposed into a column-major (position-major, row-minor)
    /// buffer, all rows are encoded together by [`encode_col_major`], and the
    /// redundancy columns are transposed back into the row-major `comm` (the message
    /// columns are unchanged because the code is systematic). The row-major layout is
    /// kept at this boundary so that `LcCommit`, its serialization, column hashing, and
    /// column openings are unchanged. The buffer costs one extra copy of the matrix.
    fn encode_rows(&self, comm: &mut [Ft], n_cols: usize) -> Result<(), Self::Err> {
        assert_eq!(n_cols, self.n_cols);
        assert_eq!(comm.len() % n_cols, 0);
        let n_rows = comm.len() / n_cols;
        if n_rows == 0 {
            return Ok(());
        }
        let n_per_row = self.n_per_row;

        let mut buf = vec![<Ft as Field>::ZERO; n_cols * n_rows];
        transpose_into(comm, n_cols, &mut buf, n_rows, n_rows, n_per_row);
        encode_col_major(&mut buf, n_rows, &self.code);
        transpose_into(
            &buf[n_per_row * n_rows..],
            n_rows,
            &mut comm[n_per_row..],
            n_cols,
            n_cols - n_per_row,
            n_rows,
        );
        Ok(())
    }

    fn get_dims(&self, len: usize) -> (usize, usize, usize) {
        let n_rows = (len + self.n_per_row - 1) / self.n_per_row;
        (n_rows, self.n_per_row, self.n_cols)
    }

    fn dims_ok(&self, n_per_row: usize, n_cols: usize) -> bool {
        let ok = n_per_row < n_cols;
        let np1 = n_per_row == self.n_per_row;
        let np2 = n_per_row == self.code.n_per_row();
        let nc1 = n_cols == self.n_cols;
        let nc2 = n_cols == self.code.codeword_len();
        ok && np1 && np2 && nc1 && nc2
    }

    fn get_n_col_opens(&self) -> usize {
        Self::n_col_opens_for(self.params.dist())
    }

    fn get_n_degree_tests(&self) -> usize {
        Self::n_degree_tests_for(self.n_cols)
    }
}

/// Marker for [`SdigEncodingS`] values whose code parameters are chosen at
/// runtime (no compile-time `SdigSpecification`).
#[derive(Clone, Copy, Debug)]
pub struct RuntimeParams;

/// SDIG / Spielman encoding with runtime code parameters ([`CodeParams`]).
///
/// Construct with [`SdigEncodingS::with_params`], [`SdigEncodingS::new_with_params`],
/// [`SdigEncodingS::new_ml_with_params`], or [`SdigEncodingS::from_code`]. In mode (a)
/// it computes exactly the same code as `SdigEncodingS<F, S>` for the matching `S`.
pub type SpielmanEncoding<F> = SdigEncodingS<F, RuntimeParams>;

/// default encoding
pub type SdigEncoding<F> = SdigEncodingS<F, SdigCodeDflt>;

/// SDIG expander-based polynomial commitment
pub type BrakedownCommit<D, F> = LcCommit<D, SdigEncoding<F>>;

/// An evaluation proof for SDIG expander-based polynomial commitment
pub type BrakedownEvalProof<D, F> = LcEvalProof<D, SdigEncoding<F>>;
