// Copyright 2026 RealBrakedownPerf Authors.
// Licensed under Apache-2.0.

//! Legacy adapter for tags before `LcEncoding::encode_rows` existed.

use lcpc_2d::LcEncoding;
use lcpc_brakedown_pc::codespec::{SdigCode6, SdigSpecification};
use lcpc_brakedown_pc::SdigEncodingS;

pub const MODE: &str = "a";
pub const PARAMS: &str = "lcpc-sdig6";
pub const FIELD_NAME: &str = "adapter-field";
pub const ADAPTER_VERSION: &str = "rowwise-encode-v1";
pub const SLICES_POLICY: &str = "unsliced; slicing API not present at this tag";

pub struct Encoder {
    inner: SdigEncodingS<F, SdigCode6>,
    n_cols: usize,
}

impl Encoder {
    pub fn new(mode: &str, n_per_row: usize, seed: u64) -> Result<Self, String> {
        if mode != MODE {
            return Err("this adapter only supports E3 mode (a)".to_owned());
        }
        let n_cols = codeword_length::<SdigCode6>(n_per_row);
        let inner = SdigEncodingS::<F, SdigCode6>::new_from_dims(n_per_row, n_cols, seed);
        Ok(Self { inner, n_cols })
    }

    pub fn n_cols(&self) -> usize {
        self.n_cols
    }

    pub fn slices_used(&self, _n_rows: usize) -> usize {
        1
    }

    pub fn encode_matrix(&self, matrix: &mut [F], n_cols: usize) -> Result<(), String> {
        if n_cols != self.n_cols || matrix.len() % n_cols != 0 {
            return Err("matrix dimensions do not match the adapter encoding".to_owned());
        }
        for row in matrix.chunks_mut(n_cols) {
            self.inner.encode(row).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn ceil_muldiv(n: usize, numerator: usize, denominator: usize) -> usize {
    (n * numerator + denominator - 1) / denominator
}

fn codeword_length<S: SdigSpecification>(n_per_row: usize) -> usize {
    let mut levels = Vec::new();
    let mut n = n_per_row;
    while n > S::baselen() {
        let next = ceil_muldiv(n, S::alpha_num(), S::alpha_den());
        levels.push((n, next));
        n = next;
    }
    assert!(
        !levels.is_empty(),
        "input dimension must exceed the base length"
    );

    let mut post_dims = Vec::with_capacity(levels.len());
    for &(n_i, m_i) in &levels {
        let n_i_prime = ceil_muldiv(m_i, S::r_num(), S::r_den());
        let m_i_prime = ceil_muldiv(n_i, S::r_num(), S::r_den()) - n_i - n_i_prime;
        post_dims.push((n_i_prime, m_i_prime));
    }

    n_per_row
        + post_dims.last().unwrap().0
        + levels
            .iter()
            .take(levels.len() - 1)
            .map(|(_, m_i)| *m_i)
            .sum::<usize>()
        + post_dims
            .iter()
            .map(|(_, m_i_prime)| *m_i_prime)
            .sum::<usize>()
}
