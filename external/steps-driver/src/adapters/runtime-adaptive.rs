// Copyright 2026 RealBrakedownPerf Authors.
// Licensed under Apache-2.0.

//! Adapter for runtime code parameters with the default adaptive slicing policy.

use lcpc_2d::LcEncoding;
use lcpc_brakedown_pc::codespec::{CodeParams, SdigCode6};
use lcpc_brakedown_pc::SdigEncodingS;

pub const MODE: &str = "runtime";
pub const FIELD_NAME: &str = "adapter-field";
pub const ADAPTER_VERSION: &str = "runtime-adaptive-encode-rows-v1";
pub const SLICES_POLICY: &str = "default adaptive slice selection";

pub struct Encoder {
    inner: SdigEncodingS<F, SdigCode6>,
    n_cols: usize,
}

impl Encoder {
    pub fn new(mode: &str, n_per_row: usize, seed: u64) -> Result<Self, String> {
        let params = match mode {
            "a" => CodeParams::lcpc_sdig6(),
            "b" => CodeParams::SimdSpielman,
            _ => return Err("mode must be a or b".to_owned()),
        };
        let inner = SdigEncodingS::<F, SdigCode6>::with_params(params, n_per_row, seed);
        let n_cols = inner.get_dims(1).2;
        Ok(Self { inner, n_cols })
    }

    pub fn n_cols(&self) -> usize {
        self.n_cols
    }

    pub fn slices_used(&self, n_rows: usize) -> usize {
        self.inner.effective_slices(n_rows)
    }

    pub fn encode_matrix(&self, matrix: &mut [F], n_cols: usize) -> Result<(), String> {
        if n_cols != self.n_cols || matrix.len() % n_cols != 0 {
            return Err("matrix dimensions do not match the adapter encoding".to_owned());
        }
        self.inner
            .encode_rows(matrix, n_cols)
            .map_err(|error| error.to_string())
    }
}
