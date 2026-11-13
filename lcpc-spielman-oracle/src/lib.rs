// Copyright 2026 RealBrakedownPerf Authors
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

#![deny(missing_docs)]

//! Test-only SIMD-Spielman (`SIMD-Spielman-for-ZKPs`) test-vector oracle and
//! straightforward Algorithms 1–4 reference encoder.
//!
//! This crate provides:
//! 1. [`GlibcRandom`]: a portable, bit-exact implementation of GNU C Library (`glibc`)
//!    `TYPE_3` (`DEG_3 = 31, SEP_3 = 3`) nonlinear additive feedback `srandom()` / `random()`
//!    generator (including the 31-word Park-Miller LCG initialization and 310 warm-up
//!    discards), together with upstream's `rand64()` and `rand128()` composition
//!    (`SIMD-Spielman-for-ZKPs/utils/utils.h`).
//! 2. [`SpielmanTvField`]: a trait bridging [`lcpc_fields::Mont127`], [`lcpc_fields::Mers127`],
//!    and [`lcpc_fields::Fp2M61`] to upstream's `T::randomPacked()` edge-weight sampling and
//!    `tv/*.txt` hexadecimal matrix file serialization.
//! 3. [`L2rExpanderGraph`], [`R2lExpanderGraph`], and [`SpielmanOracleCode`]: upstream's
//!    depth-first recursive expander-graph sampling (Algorithm 1, `seed = 4567`, sampling
//!    right neighbors with replacement via `random() % right_nodes` interleaved with
//!    `T::randomPacked()`), plus the L2R-to-R2L transpose (`ExpanderGraph.h`, `SpielmanCode.h`).
//!    These structures are exposed publicly so the fork's own encoder is also checked
//!    against `tv/`.
//! 4. Straightforward eager Algorithms 1–4 oracle encoders ([`encode_row_l2r_eager`],
//!    [`encode_row_r2l_eager`], [`encode_matrix_l2r_eager`], [`encode_matrix_r2l_eager`])
//!    and a lazy-reduction reference ([`encode_row_r2l_lazy`], [`encode_matrix_r2l_lazy`])
//!    with zero layout tricks, generic over [`ff::Field`] and [`lcpc_fields::LazyField`].
//! 5. Loaders and verification helpers ([`locate_tv_dir`], [`load_tv_input_matrix`],
//!    [`load_tv_output_matrix`], [`verify_tv_case`]) for all 18 test-vector pairs in
//!    `SIMD-Spielman-for-ZKPs/tv/`.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use ff::{Field, PrimeField};
use lcpc_fields::{Fp2M61, LazyField, Mers127, Mont127};

/// Default PRNG seed used by all `SIMD-Spielman-for-ZKPs/spielman/tests/*_Test.cpp` binaries.
pub const SIMD_SPIELMAN_TV_SEED: u32 = 4567;

/// Environment variable overriding the path to `SIMD-Spielman-for-ZKPs/tv`.
pub const SIMD_SPIELMAN_TV_DIR_ENV: &str = "SIMD_SPIELMAN_TV_DIR";

/// Fixed code parameters from `SIMD-Spielman-for-ZKPs/spielman/SpielmanParams.h`
/// (matching Orion / Brakedown `SdigCode6` asymptotic parameters with fixed degrees).
pub struct SimdSpielmanParams;

impl SimdSpielmanParams {
    /// Target relative code distance $\delta_0 = 0.07$.
    pub const CODE_DISTANCE: f64 = 0.07;

    /// Recursion stopping threshold $\lfloor 1 / 0.07 \rfloor - 1 = 13$.
    /// When `nr_left_nodes < 13`, recursion stops and the base case is the identity map.
    pub const DISTANCE_THRESHOLD: usize = (1.0 / Self::CODE_DISTANCE) as usize - 1;

    /// Precode expansion ratio $\alpha = 0.238$.
    pub const ALPHA: f64 = 0.238;

    /// Relative distance parameter $\beta = 0.1205$.
    pub const BETA: f64 = 0.1205;

    /// Overall reciprocal rate $r = 1.72$.
    pub const R: f64 = 1.72;

    /// Fixed left degree $c_n = 10$ for all precode expander graphs ($G_1$).
    pub const CN: usize = 10;

    /// Fixed left degree $d_n = 20$ for all postcode expander graphs ($G_2$).
    pub const DN: usize = 20;
}

/// Matrix shape configuration in `SIMD-Spielman-for-ZKPs/spielman/SpielmanParams.h`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MatrixShape {
    /// Square-ish Brakedown matrix (`BRAKEDOWN = 1`, prefix `bd_spielman`):
    /// `n_rows = 1 << (log_n / 2)`, `k = (1 << log_n) / n_rows`.
    Brakedown,
    /// Fixed-height Orion matrix (`BRAKEDOWN = 0`, prefix `spielman`):
    /// `n_rows = 128`, `k = (1 << log_n) / 128`.
    Orion,
}

impl MatrixShape {
    /// File prefix used in `SIMD-Spielman-for-ZKPs/tv/`.
    #[inline]
    pub const fn file_prefix(self) -> &'static str {
        match self {
            MatrixShape::Brakedown => "bd_spielman",
            MatrixShape::Orion => "spielman",
        }
    }

    /// Computes `(n_rows, k)` for polynomial size $N = 2^{\text{log\_n}}$, where
    /// `n_rows` is `column_size` (number of matrix rows) and `k = N / n_rows` is the
    /// per-row message length.
    #[inline]
    pub const fn dims(self, log_n: u32) -> (usize, usize) {
        let n = 1usize << log_n;
        let n_rows = match self {
            MatrixShape::Brakedown => 1usize << (log_n / 2),
            MatrixShape::Orion => 128usize,
        };
        let k = n / n_rows;
        (n_rows, k)
    }
}

/// Portable, bit-exact replica of `glibc`'s default `TYPE_3` (`DEG_3 = 31`, `SEP_3 = 3`)
/// nonlinear additive feedback generator (`srandom` / `random` in `stdlib/random_r.c`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlibcRandom {
    state: [i32; Self::DEG],
    fptr: usize,
    rptr: usize,
}

impl GlibcRandom {
    const DEG: usize = 31;
    const SEP: usize = 3;

    /// Initializes the generator state identically to `glibc`'s `srandom(seed)`.
    ///
    /// - Maps `seed == 0` to `1`.
    /// - Populates `state[1..31]` using the Park-Miller LCG
    ///   $x_i = 16807 \cdot x_{i-1} \bmod 2147483647$ with signed C division/remainder
    ///   when `state[0] < 0`.
    /// - Sets `fptr = 3`, `rptr = 0`, and discards the first `10 * 31 = 310` outputs.
    pub fn new(seed: u32) -> Self {
        let effective_seed = if seed == 0 { 1u32 } else { seed };
        let mut state = [0i32; Self::DEG];
        let mut word = effective_seed as i32;
        state[0] = word;

        for item in state.iter_mut().skip(1) {
            // Exact translation of glibc __srandom_r:
            // int32_t hi = word / 127773;
            // int32_t lo = word % 127773;
            // word = 16807 * lo - 2836 * hi;
            // if (word < 0) word += 2147483647;
            let hi = word / 127_773;
            let lo = word % 127_773;
            word = 16_807i32
                .wrapping_mul(lo)
                .wrapping_sub(2_836i32.wrapping_mul(hi));
            if word < 0 {
                word = word.wrapping_add(2_147_483_647);
            }
            *item = word;
        }

        let mut rng = Self {
            state,
            fptr: Self::SEP,
            rptr: 0,
        };

        for _ in 0..(10 * Self::DEG) {
            let _ = rng.random();
        }

        rng
    }

    /// Generates the next 31-bit non-negative random integer in `0..=0x7fffffff`,
    /// matching C `random()` on `glibc`.
    #[inline]
    pub fn random(&mut self) -> u32 {
        let val = self.state[self.fptr].wrapping_add(self.state[self.rptr]);
        self.state[self.fptr] = val;
        let res = (val as u32) >> 1;

        self.fptr += 1;
        if self.fptr >= Self::DEG {
            self.fptr = 0;
        }
        self.rptr += 1;
        if self.rptr >= Self::DEG {
            self.rptr = 0;
        }

        res
    }

    /// Reproduces `uint64_t rand64()` from `SIMD-Spielman-for-ZKPs/utils/utils.h`:
    ///
    /// ```c
    /// uint64_t rnd = (uint64_t)random();
    /// rnd = (rnd << 31) ^ random();
    /// rnd = (rnd << 31) ^ random();
    /// return rnd;
    /// ```
    #[inline]
    pub fn rand64(&mut self) -> u64 {
        let mut rnd = self.random() as u64;
        rnd = (rnd << 31) ^ (self.random() as u64);
        rnd = (rnd << 31) ^ (self.random() as u64);
        rnd
    }

    /// Reproduces `__uint128_t rand128()` from `SIMD-Spielman-for-ZKPs/utils/utils.h`:
    ///
    /// ```c
    /// __uint128_t rnd = (__uint128_t)random();
    /// rnd = (rnd << 31) ^ random();
    /// rnd = (rnd << 31) ^ random();
    /// rnd = (rnd << 31) ^ random();
    /// rnd = (rnd << 31) ^ random();
    /// return rnd;
    /// ```
    #[inline]
    pub fn rand128(&mut self) -> u128 {
        let mut rnd = self.random() as u128;
        rnd = (rnd << 31) ^ (self.random() as u128);
        rnd = (rnd << 31) ^ (self.random() as u128);
        rnd = (rnd << 31) ^ (self.random() as u128);
        rnd = (rnd << 31) ^ (self.random() as u128);
        rnd
    }
}

/// Trait mapping [`LazyField`] implementations (`Mont127`, `Mers127`, `Fp2M61`) to
/// SIMD-Spielman's `T::randomPacked()` edge-weight sampler and `tv/*.txt` word format.
pub trait SpielmanTvField: LazyField {
    /// Canonical field name in `SIMD-Spielman-for-ZKPs/tv/` filenames
    /// (`"montgomery"`, `"mersenne"`, or `"extension"`).
    const TV_FIELD_NAME: &'static str;

    /// Samples a random edge weight matching `T::randomPacked()` in
    /// `SIMD-Spielman-for-ZKPs/fields/Unpacked*.h`.
    fn sample_packed_weight(rng: &mut GlibcRandom) -> Self;

    /// Converts a pair of 64-bit hexadecimal words `(first, second)` parsed from a line
    /// `%lx %lx` of a `tv/*.txt` file into `Self`.
    fn from_tv_words(first: u64, second: u64) -> Self;

    /// Converts `Self` into the pair of 64-bit words `(first, second)` written on a line
    /// `%lx %lx` of a `tv/*.txt` file.
    fn to_tv_words(&self) -> (u64, u64);
}

impl SpielmanTvField for Mont127 {
    const TV_FIELD_NAME: &'static str = "montgomery";

    #[inline]
    fn sample_packed_weight(rng: &mut GlibcRandom) -> Self {
        // In UnpackedMontgomeryPrimeField::randomPacked():
        //   __uint128_t rnd = rand128() % PRIME;
        //   uint256_t t = (uint256_t)rnd * R_MOD_Q % PRIME; // R_avx = 2^135
        // During ExtUnpackedMontgomeryPrimeField::reduce(), the accumulated sum is
        // multiplied by R_INV = (2^135)^-1 mod PRIME, so each edge multiplies its input
        // residue by the canonical residue `rnd` modulo PRIME.
        //
        // In Mont127, converting both the input residues (via `from_tv_words`) and the
        // sampled residue `rnd` into standard Montgomery form `rnd * 2^128 mod q` (via
        // `from_repr`) means every Mont127 multiplication (eager `mul_val` or lazy
        // `prepare_weight` + `mac` + `reduce`) computes `(x * rnd) * 2^128 mod q`, and
        // `to_tv_words` (`to_repr`) recovers the exact canonical residue `x * rnd mod q`.
        let rnd = rng.rand128() % Mont127::MODULUS_U128;
        Mont127::from_repr(rnd.to_le_bytes()).expect("rnd < MODULUS_U128 is canonical")
    }

    #[inline]
    fn from_tv_words(first: u64, second: u64) -> Self {
        // SpielmanMontgomery_Test.cpp reads `fscanf(f_in, "%lx %lx\n", &input_hi[s], &input_lo[s])`
        // where `first = hi` (bits 64..126) and `second = lo` (bits 0..63).
        let val = ((first as u128) << 64) | (second as u128);
        Mont127::from_repr(val.to_le_bytes()).expect("tv input word must be < q")
    }

    #[inline]
    fn to_tv_words(&self) -> (u64, u64) {
        let repr = self.to_repr();
        let val = u128::from_le_bytes(repr);
        let hi = (val >> 64) as u64;
        let lo = val as u64;
        (hi, lo)
    }
}

impl SpielmanTvField for Mers127 {
    const TV_FIELD_NAME: &'static str = "mersenne";

    #[inline]
    fn sample_packed_weight(rng: &mut GlibcRandom) -> Self {
        // In MersennePrimeField::randomPacked():
        //   __uint128_t rnd = rand128() % MERSENNE_PRIME;
        let rnd = rng.rand128() % Mers127::MODULUS_U128;
        Mers127([rnd as u64, (rnd >> 64) as u64])
    }

    #[inline]
    fn from_tv_words(first: u64, second: u64) -> Self {
        // SpielmanMersenne_Test.cpp reads `fscanf(f_in, "%lx %lx\n", &input_hi[s], &input_lo[s])`
        // where `first = hi` and `second = lo`.
        Mers127([second, first])
    }

    #[inline]
    fn to_tv_words(&self) -> (u64, u64) {
        (self.hi(), self.lo())
    }
}

impl SpielmanTvField for Fp2M61 {
    const TV_FIELD_NAME: &'static str = "extension";

    #[inline]
    fn sample_packed_weight(rng: &mut GlibcRandom) -> Self {
        // In ExtensionField::randomPacked():
        //   uint64_t re = rand64() % ORION_PRIME;
        //   uint64_t im = rand64() % ORION_PRIME;
        let real = rng.rand64() % Fp2M61::MODULUS_P;
        let img = rng.rand64() % Fp2M61::MODULUS_P;
        Fp2M61::from_parts(real, img)
    }

    #[inline]
    fn from_tv_words(first: u64, second: u64) -> Self {
        // SpielmanExtension_Test.cpp reads `fscanf(f_in, "%lx %lx\n", &input_re[s], &input_im[s])`
        // where `first = real` and `second = img`.
        Fp2M61::from_parts(first, second)
    }

    #[inline]
    fn to_tv_words(&self) -> (u64, u64) {
        (self.real, self.img)
    }
}

/// Left-to-right (L2R) bipartite expander graph sampled by Algorithm 1 in
/// `SIMD-Spielman-for-ZKPs/spielman/ExpanderGraph.h`.
///
/// For each left node `l in 0..n_left` and edge slot `d in 0..left_degree`:
/// - `connections[l * left_degree + d]` holds the right-node index `r in 0..n_right`
///   sampled *with replacement* (`random() % right_nodes`).
/// - `weights[l * left_degree + d]` holds the corresponding edge weight $w_{l, d}$.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct L2rExpanderGraph<F> {
    /// Number of left (input) nodes $L$.
    pub n_left: usize,
    /// Number of right (output) nodes $R$.
    pub n_right: usize,
    /// Fixed number of outgoing edges per left node ($c_n$ or $d_n$).
    pub left_degree: usize,
    /// Flat array of length `n_left * left_degree` storing right-node indices in `[0, n_right)`.
    pub connections: Vec<u32>,
    /// Flat array of length `n_left * left_degree` storing edge weights.
    pub weights: Vec<F>,
}

impl<F: Copy> L2rExpanderGraph<F> {
    /// Samples an L2R expander graph in the exact call order of `ExpanderGraph::ExpanderGraph`
    /// (`SIMD-Spielman-for-ZKPs/spielman/ExpanderGraph.h`, lines 60–69):
    ///
    /// ```c
    /// for (uint32_t l = 0; l < left_nodes; l++) {
    ///   for (uint32_t d = 0; d < left_degree; d++) {
    ///     auto rnode = random() % right_nodes;
    ///     l2r_node_connections[l * left_degree + d] = rnode;
    ///     l2r_node_weights[l * left_degree + d] = T::randomPacked();
    ///   }
    /// }
    /// ```
    pub fn sample_with<W>(
        n_left: usize,
        n_right: usize,
        left_degree: usize,
        rng: &mut GlibcRandom,
        mut sample_weight: W,
    ) -> Self
    where
        W: FnMut(&mut GlibcRandom) -> F,
    {
        assert!(n_right > 0, "n_right must be positive");
        let num_edges = n_left * left_degree;
        let mut connections = Vec::with_capacity(num_edges);
        let mut weights = Vec::with_capacity(num_edges);
        let right_nodes_u32 = n_right as u32;

        for _l in 0..n_left {
            for _d in 0..left_degree {
                let rnode = rng.random() % right_nodes_u32;
                let weight = sample_weight(rng);
                connections.push(rnode);
                weights.push(weight);
            }
        }

        Self {
            n_left,
            n_right,
            left_degree,
            connections,
            weights,
        }
    }

    /// Transposes this L2R expander graph into a right-to-left (R2L) CSR representation
    /// using a linear-time $\mathcal{O}(R + E)$ stable counting sort.
    ///
    /// Because the counting sort is stable, incoming edges for each right node `r` appear
    /// in increasing `edge_index = l * left_degree + d` order—identical to the
    /// $\mathcal{O}(R \cdot E)$ scan in `ExpanderGraph.h` (lines 73–86).
    pub fn transpose(&self) -> R2lExpanderGraph<F> {
        let num_edges = self.n_left * self.left_degree;
        assert_eq!(self.connections.len(), num_edges);
        assert_eq!(self.weights.len(), num_edges);

        let mut rnode_degrees = vec![0u32; self.n_right];
        for &r in &self.connections {
            rnode_degrees[r as usize] += 1;
        }

        let mut row_ptr = Vec::with_capacity(self.n_right + 1);
        row_ptr.push(0u32);
        let mut acc = 0u32;
        for &deg in &rnode_degrees {
            acc += deg;
            row_ptr.push(acc);
        }
        assert_eq!(acc as usize, num_edges);

        let mut cursors = row_ptr[..self.n_right].to_vec();
        let mut col_idx = vec![0u32; num_edges];
        let mut weights = Vec::with_capacity(num_edges);
        if num_edges > 0 {
            weights.resize(num_edges, self.weights[0]);
        }

        for l in 0..self.n_left {
            let base = l * self.left_degree;
            for d in 0..self.left_degree {
                let edge_idx = base + d;
                let r = self.connections[edge_idx] as usize;
                let pos = cursors[r] as usize;
                col_idx[pos] = l as u32;
                weights[pos] = self.weights[edge_idx];
                cursors[r] += 1;
            }
        }

        R2lExpanderGraph {
            n_left: self.n_left,
            n_right: self.n_right,
            left_degree: self.left_degree,
            row_ptr,
            rnode_degrees,
            col_idx,
            weights,
        }
    }
}

/// Right-to-left (R2L) Compressed Sparse Row (CSR) transposed expander graph
/// matching `ExpanderGraph.h` and `lcpc_brakedown_pc::graph::R2lGraph<F>`.
///
/// For right node `r in 0..n_right`, its `rnode_degrees[r]` incoming edges live at
/// indices `e in row_ptr[r] as usize .. row_ptr[r + 1] as usize`, connecting from
/// left node `col_idx[e]` with weight `weights[e]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct R2lExpanderGraph<F> {
    /// Number of left (input) nodes $L$.
    pub n_left: usize,
    /// Number of right (output) nodes $R$.
    pub n_right: usize,
    /// Left out-degree used to generate the graph ($c_n$ or $d_n$).
    pub left_degree: usize,
    /// CSR row pointer offsets of length `n_right + 1`.
    pub row_ptr: Vec<u32>,
    /// In-degree of each right node (`r2l_rnode_degrees` in `ExpanderGraph.h`), length `n_right`.
    pub rnode_degrees: Vec<u32>,
    /// Left-node index `l` for each incoming edge (`r2l_node_connections` in `ExpanderGraph.h`).
    pub col_idx: Vec<u32>,
    /// Edge weight $w_{l, r}$ for each incoming edge (`r2l_node_weights` in `ExpanderGraph.h`).
    pub weights: Vec<F>,
}

/// Complete multi-level Spielman expander code sampled with SIMD-Spielman's parameters
/// and depth-first RNG call order (`SIMD-Spielman-for-ZKPs/spielman/SpielmanCode.h`).
#[derive(Clone, Debug)]
pub struct SpielmanOracleCode<F> {
    /// Message length $k$ (number of left nodes at recursion level 0).
    pub k: usize,
    /// Active codeword length returned by `initExpanders(k)`.
    pub codeword_len: usize,
    /// Total padded codeword buffer length `2 * k` used by SIMD-Spielman's matrix arrays
    /// (positions `codeword_len .. 2 * k` remain zero).
    pub padded_codeword_len: usize,
    /// Precode expander graphs $G_1^{(0)}, \dots, G_1^{(L-1)}$ in L2R format.
    pub graphs1_l2r: Vec<L2rExpanderGraph<F>>,
    /// Postcode expander graphs $G_2^{(0)}, \dots, G_2^{(L-1)}$ in L2R format.
    pub graphs2_l2r: Vec<L2rExpanderGraph<F>>,
    /// Precode expander graphs $G_1^{(0)}, \dots, G_1^{(L-1)}$ in transposed R2L CSR format.
    pub graphs1_r2l: Vec<R2lExpanderGraph<F>>,
    /// Postcode expander graphs $G_2^{(0)}, \dots, G_2^{(L-1)}$ in transposed R2L CSR format.
    pub graphs2_r2l: Vec<R2lExpanderGraph<F>>,
}

impl<F: Copy> SpielmanOracleCode<F> {
    /// Samples the recursive expander code for message length `k` using `rng` and custom
    /// weight sampler `sample_weight`, following the exact depth-first recursion order of
    /// `SpielmanCode::initExpanders` (`SIMD-Spielman-for-ZKPs/spielman/SpielmanCode.h`, lines 94–104):
    ///
    /// 1. If `nr_left_nodes < 13`, returns `nr_left_nodes` without sampling any graph.
    /// 2. Samples `graphs1[level]` with `left_nodes = nr_left_nodes`,
    ///    `right_nodes = (alpha * nr_left_nodes) as u32`, `left_degree = cn = 10`.
    /// 3. Recursively calls `initExpanders(right_nodes, level + 1)` to obtain `nr_nodes`.
    /// 4. Samples `graphs2[level]` with `left_nodes = nr_nodes`,
    ///    `right_nodes = (nr_left_nodes * (r - 1.0) - nr_nodes) as u32`, `left_degree = dn = 20`.
    /// 5. Returns `nr_left_nodes + nr_nodes + graphs2[level].right_nodes`.
    pub fn sample_with_custom_weight<W>(
        k: usize,
        rng: &mut GlibcRandom,
        mut sample_weight: W,
    ) -> Self
    where
        W: FnMut(&mut GlibcRandom) -> F,
    {
        let mut graphs1_l2r = Vec::new();
        let mut graphs2_l2r = Vec::new();

        fn init_expanders<F: Copy, W: FnMut(&mut GlibcRandom) -> F>(
            nr_left_nodes: u32,
            rng: &mut GlibcRandom,
            sample_weight: &mut W,
            graphs1: &mut Vec<L2rExpanderGraph<F>>,
            graphs2: &mut Vec<Option<L2rExpanderGraph<F>>>,
        ) -> u32 {
            if (nr_left_nodes as usize) < SimdSpielmanParams::DISTANCE_THRESHOLD {
                return nr_left_nodes;
            }
            let idx = graphs1.len();
            let r1 = (SimdSpielmanParams::ALPHA * (nr_left_nodes as f64)) as u32;
            let g1 = L2rExpanderGraph::sample_with(
                nr_left_nodes as usize,
                r1 as usize,
                SimdSpielmanParams::CN,
                rng,
                &mut *sample_weight,
            );
            graphs1.push(g1);
            graphs2.push(None);

            let nr_nodes = init_expanders(r1, rng, sample_weight, graphs1, graphs2);

            let r2 = ((nr_left_nodes as f64) * (SimdSpielmanParams::R - 1.0)
                - (nr_nodes as f64)) as u32;
            let g2 = L2rExpanderGraph::sample_with(
                nr_nodes as usize,
                r2 as usize,
                SimdSpielmanParams::DN,
                rng,
                &mut *sample_weight,
            );
            graphs2[idx] = Some(g2);

            nr_left_nodes + nr_nodes + r2
        }

        let mut graphs2_opt = Vec::new();
        let codeword_len = init_expanders(
            k as u32,
            rng,
            &mut sample_weight,
            &mut graphs1_l2r,
            &mut graphs2_opt,
        ) as usize;

        for g2 in graphs2_opt {
            graphs2_l2r.push(g2.expect("all recursion levels populate graphs2"));
        }

        let graphs1_r2l = graphs1_l2r.iter().map(L2rExpanderGraph::transpose).collect();
        let graphs2_r2l = graphs2_l2r.iter().map(L2rExpanderGraph::transpose).collect();

        Self {
            k,
            codeword_len,
            padded_codeword_len: 2 * k,
            graphs1_l2r,
            graphs2_l2r,
            graphs1_r2l,
            graphs2_r2l,
        }
    }

    /// Number of recursive expander levels (`num_graphs` in `SpielmanCode.h`).
    #[inline]
    pub fn num_levels(&self) -> usize {
        self.graphs1_l2r.len()
    }
}

impl<F: SpielmanTvField> SpielmanOracleCode<F> {
    /// Samples the Spielman code for message length `k` using `GlibcRandom::new(seed)` and
    /// `F::sample_packed_weight` (`seed = 4567` for all `SIMD-Spielman-for-ZKPs/tv/` vectors).
    pub fn sample_tv(k: usize, seed: u32) -> Self {
        let mut rng = GlibcRandom::new(seed);
        Self::sample_with_custom_weight(k, &mut rng, F::sample_packed_weight)
    }
}

/// Algorithm 2 (Left-to-Right / scatter) single-expander evaluation with eager field
/// arithmetic (`output[r] += input[l] * weight`).
///
/// Overwrites `output[0..graph.n_right]` with the bipartite graph product.
pub fn eval_expander_l2r_eager<F: Field>(
    graph: &L2rExpanderGraph<F>,
    input: &[F],
    output: &mut [F],
) {
    assert_eq!(input.len(), graph.n_left);
    assert_eq!(output.len(), graph.n_right);

    output.fill(F::ZERO);
    for l in 0..graph.n_left {
        let x_l = input[l];
        let base = l * graph.left_degree;
        for d in 0..graph.left_degree {
            let r = graph.connections[base + d] as usize;
            let w = graph.weights[base + d];
            output[r] += x_l * w;
        }
    }
}

/// Algorithm 3 (Right-to-Left / gather) single-expander evaluation with eager field
/// arithmetic (`acc += input[col_idx[e]] * weights[e]`).
///
/// Overwrites `output[0..graph.n_right]` with the bipartite graph product.
pub fn eval_expander_r2l_eager<F: Field>(
    graph: &R2lExpanderGraph<F>,
    input: &[F],
    output: &mut [F],
) {
    assert_eq!(input.len(), graph.n_left);
    assert_eq!(output.len(), graph.n_right);

    for (r, out_r) in output.iter_mut().enumerate() {
        let start = graph.row_ptr[r] as usize;
        let end = graph.row_ptr[r + 1] as usize;
        let mut acc = F::ZERO;
        for e in start..end {
            let l = graph.col_idx[e] as usize;
            let w = graph.weights[e];
            acc += input[l] * w;
        }
        *out_r = acc;
    }
}

/// Algorithm 3 (Right-to-Left / gather) single-expander evaluation using [`LazyField`]
/// unreduced accumulation (`prepare_weight`, `mac`, `fold`, `reduce`).
pub fn eval_expander_r2l_lazy<F: LazyField>(
    graph: &R2lExpanderGraph<F>,
    input: &[F],
    output: &mut [F],
) {
    assert_eq!(input.len(), graph.n_left);
    assert_eq!(output.len(), graph.n_right);

    for (r, out_r) in output.iter_mut().enumerate() {
        let start = graph.row_ptr[r] as usize;
        let end = graph.row_ptr[r + 1] as usize;
        let mut acc = F::Acc::default();
        let mut since_fold = 0usize;
        for e in start..end {
            if since_fold == F::MAX_ACCUMULATIONS {
                F::fold(&mut acc);
                since_fold = 1;
            } else {
                since_fold += 1;
            }
            let l = graph.col_idx[e] as usize;
            let w = F::prepare_weight(&graph.weights[e]);
            F::mac(&mut acc, &w, &input[l]);
        }
        *out_r = F::reduce(acc);
    }
}

/// Straightforward Algorithm 4 recursive Spielman encoding of a single row using
/// eager Left-to-Right (Algorithm 2) expander evaluation.
///
/// `row` must have length at least `code.codeword_len` (typically `code.padded_codeword_len = 2 * k`),
/// with the message in `row[0..code.k]`. Positions `code.codeword_len..row.len()` are zeroed.
pub fn encode_row_l2r_eager<F: Field>(code: &SpielmanOracleCode<F>, row: &mut [F]) {
    assert!(row.len() >= code.codeword_len);
    for elem in &mut row[code.k..] {
        *elem = F::ZERO;
    }

    fn recurse<F: Field>(code: &SpielmanOracleCode<F>, buf: &mut [F], level: usize) {
        if level >= code.num_levels() {
            // Base case: nr_left_nodes < 13 => identity no-op.
            return;
        }
        let g1 = &code.graphs1_l2r[level];
        let g2 = &code.graphs2_l2r[level];

        {
            let (left_slice, rest) = buf.split_at_mut(g1.n_left);
            eval_expander_l2r_eager(g1, left_slice, &mut rest[..g1.n_right]);
        }

        recurse(code, &mut buf[g1.n_left..], level + 1);

        {
            let sub = &mut buf[g1.n_left..];
            let (left_slice, rest) = sub.split_at_mut(g2.n_left);
            eval_expander_l2r_eager(g2, left_slice, &mut rest[..g2.n_right]);
        }
    }

    recurse(code, row, 0);
}

/// Straightforward Algorithm 4 recursive Spielman encoding of a single row using
/// eager Right-to-Left (Algorithm 3) expander evaluation.
///
/// `row` must have length at least `code.codeword_len` (typically `code.padded_codeword_len = 2 * k`),
/// with the message in `row[0..code.k]`. Positions `code.codeword_len..row.len()` are zeroed.
pub fn encode_row_r2l_eager<F: Field>(code: &SpielmanOracleCode<F>, row: &mut [F]) {
    assert!(row.len() >= code.codeword_len);
    for elem in &mut row[code.k..] {
        *elem = F::ZERO;
    }

    fn recurse<F: Field>(code: &SpielmanOracleCode<F>, buf: &mut [F], level: usize) {
        if level >= code.num_levels() {
            return;
        }
        let g1 = &code.graphs1_r2l[level];
        let g2 = &code.graphs2_r2l[level];

        {
            let (left_slice, rest) = buf.split_at_mut(g1.n_left);
            eval_expander_r2l_eager(g1, left_slice, &mut rest[..g1.n_right]);
        }

        recurse(code, &mut buf[g1.n_left..], level + 1);

        {
            let sub = &mut buf[g1.n_left..];
            let (left_slice, rest) = sub.split_at_mut(g2.n_left);
            eval_expander_r2l_eager(g2, left_slice, &mut rest[..g2.n_right]);
        }
    }

    recurse(code, row, 0);
}

/// Reference Algorithm 4 recursive Spielman encoding of a single row using
/// Right-to-Left (Algorithm 3) expander evaluation and [`LazyField`] lazy reduction.
pub fn encode_row_r2l_lazy<F: LazyField>(code: &SpielmanOracleCode<F>, row: &mut [F]) {
    assert!(row.len() >= code.codeword_len);
    for elem in &mut row[code.k..] {
        *elem = F::ZERO;
    }

    fn recurse<F: LazyField>(code: &SpielmanOracleCode<F>, buf: &mut [F], level: usize) {
        if level >= code.num_levels() {
            return;
        }
        let g1 = &code.graphs1_r2l[level];
        let g2 = &code.graphs2_r2l[level];

        {
            let (left_slice, rest) = buf.split_at_mut(g1.n_left);
            eval_expander_r2l_lazy(g1, left_slice, &mut rest[..g1.n_right]);
        }

        recurse(code, &mut buf[g1.n_left..], level + 1);

        {
            let sub = &mut buf[g1.n_left..];
            let (left_slice, rest) = sub.split_at_mut(g2.n_left);
            eval_expander_r2l_lazy(g2, left_slice, &mut rest[..g2.n_right]);
        }
    }

    recurse(code, row, 0);
}

/// Encodes an entire `n_rows x k` row-major input matrix into an `n_rows x (2 * k)`
/// row-major output matrix using eager L2R evaluation ([`encode_row_l2r_eager`]).
pub fn encode_matrix_l2r_eager<F: Field>(
    code: &SpielmanOracleCode<F>,
    input_rows: &[F],
    n_rows: usize,
) -> Vec<F> {
    assert_eq!(input_rows.len(), n_rows * code.k);
    let out_cols = code.padded_codeword_len;
    let mut out = vec![F::ZERO; n_rows * out_cols];
    for r in 0..n_rows {
        let src = &input_rows[r * code.k..(r + 1) * code.k];
        let dst = &mut out[r * out_cols..(r + 1) * out_cols];
        dst[..code.k].copy_from_slice(src);
        encode_row_l2r_eager(code, dst);
    }
    out
}

/// Encodes an entire `n_rows x k` row-major input matrix into an `n_rows x (2 * k)`
/// row-major output matrix using eager R2L evaluation ([`encode_row_r2l_eager`]).
pub fn encode_matrix_r2l_eager<F: Field>(
    code: &SpielmanOracleCode<F>,
    input_rows: &[F],
    n_rows: usize,
) -> Vec<F> {
    assert_eq!(input_rows.len(), n_rows * code.k);
    let out_cols = code.padded_codeword_len;
    let mut out = vec![F::ZERO; n_rows * out_cols];
    for r in 0..n_rows {
        let src = &input_rows[r * code.k..(r + 1) * code.k];
        let dst = &mut out[r * out_cols..(r + 1) * out_cols];
        dst[..code.k].copy_from_slice(src);
        encode_row_r2l_eager(code, dst);
    }
    out
}

/// Encodes an entire `n_rows x k` row-major input matrix into an `n_rows x (2 * k)`
/// row-major output matrix using lazy R2L evaluation ([`encode_row_r2l_lazy`]).
pub fn encode_matrix_r2l_lazy<F: LazyField>(
    code: &SpielmanOracleCode<F>,
    input_rows: &[F],
    n_rows: usize,
) -> Vec<F> {
    assert_eq!(input_rows.len(), n_rows * code.k);
    let out_cols = code.padded_codeword_len;
    let mut out = vec![F::ZERO; n_rows * out_cols];
    for r in 0..n_rows {
        let src = &input_rows[r * code.k..(r + 1) * code.k];
        let dst = &mut out[r * out_cols..(r + 1) * out_cols];
        dst[..code.k].copy_from_slice(src);
        encode_row_r2l_lazy(code, dst);
    }
    out
}

/// Resolves the path to `SIMD-Spielman-for-ZKPs/tv/`, using `override_dir` if provided,
/// or checking `SIMD_SPIELMAN_TV_DIR` and default relative paths otherwise.
pub fn locate_tv_dir_with_override(override_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = override_dir {
        if p.is_dir() {
            return Some(p.to_path_buf());
        }
        return None;
    }

    if let Ok(env_val) = std::env::var(SIMD_SPIELMAN_TV_DIR_ENV) {
        let p = PathBuf::from(env_val);
        if p.is_dir() {
            return Some(p);
        }
        return None;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest_dir.join("../../SIMD-Spielman-for-ZKPs/tv"),
        manifest_dir.join("../SIMD-Spielman-for-ZKPs/tv"),
        PathBuf::from("../SIMD-Spielman-for-ZKPs/tv"),
        PathBuf::from("SIMD-Spielman-for-ZKPs/tv"),
    ];
    for cand in candidates {
        if cand.is_dir() {
            return Some(cand);
        }
    }
    None
}

/// Resolves the path to `SIMD-Spielman-for-ZKPs/tv/`:
/// 1. Checks `SIMD_SPIELMAN_TV_DIR` if set.
/// 2. Falls back to `../../SIMD-Spielman-for-ZKPs/tv` relative to `CARGO_MANIFEST_DIR`.
/// 3. Falls back to `../SIMD-Spielman-for-ZKPs/tv` relative to current working directory.
///
/// Returns `Some(path)` if the directory exists, or `None` otherwise.
pub fn locate_tv_dir() -> Option<PathBuf> {
    locate_tv_dir_with_override(None)
}

/// Returns the `(in_filename, out_filename)` pair inside `tv/` for `(shape, F, log_n)`.
pub fn tv_filenames<F: SpielmanTvField>(shape: MatrixShape, log_n: u32) -> (String, String) {
    let prefix = shape.file_prefix();
    let field = F::TV_FIELD_NAME;
    (
        format!("{prefix}_{field}_N{log_n}_in.txt"),
        format!("{prefix}_{field}_N{log_n}_out.txt"),
    )
}

/// Parses a single line `"<hex_word_0> <hex_word_1>"` from a `tv/*.txt` file.
fn parse_hex_pair(line: &str, line_no: usize, path: &Path) -> io::Result<(u64, u64)> {
    let mut parts = line.split_whitespace();
    let w0_str = parts.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}:{}: missing first hex word", path.display(), line_no),
        )
    })?;
    let w1_str = parts.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}:{}: missing second hex word", path.display(), line_no),
        )
    })?;
    let w0 = u64::from_str_radix(w0_str, 16).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}:{}: invalid hex '{}': {}", path.display(), line_no, w0_str, e),
        )
    })?;
    let w1 = u64::from_str_radix(w1_str, 16).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}:{}: invalid hex '{}': {}", path.display(), line_no, w1_str, e),
        )
    })?;
    Ok((w0, w1))
}

/// Reads an `n_rows x n_cols` matrix from a SIMD-Spielman `tv/*.txt` file and returns
/// it in standard **row-major** order (`matrix[row * n_cols + col]`).
///
/// In `SIMD-Spielman-for-ZKPs/spielman/tests/*_Test.cpp` (lines 46–59 and 66–77),
/// the file order is:
/// - outer loop `r_blk in 0..(n_rows / 8)`
/// - middle loop `col in 0..n_cols` (`k` for `*_in.txt`, `2 * k` for `*_out.txt`)
/// - inner loop `s in 0..8` (AVX-512 lane, corresponding to `row = r_blk * 8 + s`).
pub fn load_tv_matrix<F: SpielmanTvField>(
    path: &Path,
    n_rows: usize,
    n_cols: usize,
) -> io::Result<Vec<F>> {
    assert_eq!(n_rows % 8, 0, "n_rows must be a multiple of 8 AVX-512 lanes");
    let col_elements = n_rows / 8;
    let expected_lines = n_rows * n_cols;

    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines().enumerate();

    let mut row_major = vec![F::ZERO; expected_lines];

    for r_blk in 0..col_elements {
        for col in 0..n_cols {
            for s in 0..8 {
                let (line_idx, line_res) = lines.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "{}: expected {} lines, got EOF early",
                            path.display(),
                            expected_lines
                        ),
                    )
                })?;
                let line = line_res?;
                let (w0, w1) = parse_hex_pair(&line, line_idx + 1, path)?;
                let row = r_blk * 8 + s;
                row_major[row * n_cols + col] = F::from_tv_words(w0, w1);
            }
        }
    }

    Ok(row_major)
}

/// Loads the `n_rows x k` input matrix from `tv/{prefix}_{field}_N{log_n}_in.txt`
/// in row-major order (`matrix[row * k + col]`).
pub fn load_tv_input_matrix<F: SpielmanTvField>(
    tv_dir: &Path,
    shape: MatrixShape,
    log_n: u32,
) -> io::Result<Vec<F>> {
    let (n_rows, k) = shape.dims(log_n);
    let (in_name, _) = tv_filenames::<F>(shape, log_n);
    load_tv_matrix(&tv_dir.join(in_name), n_rows, k)
}

/// Loads the `n_rows x (2 * k)` reference output matrix from
/// `tv/{prefix}_{field}_N{log_n}_out.txt` in row-major order (`matrix[row * (2 * k) + col]`).
pub fn load_tv_output_matrix<F: SpielmanTvField>(
    tv_dir: &Path,
    shape: MatrixShape,
    log_n: u32,
) -> io::Result<Vec<F>> {
    let (n_rows, k) = shape.dims(log_n);
    let (_, out_name) = tv_filenames::<F>(shape, log_n);
    load_tv_matrix(&tv_dir.join(out_name), n_rows, 2 * k)
}

/// Verifies that the oracle encoder (`encode_matrix_r2l_eager`, `encode_matrix_l2r_eager`,
/// and `encode_matrix_r2l_lazy`) reproduces `tv/{prefix}_{field}_N{log_n}_out.txt`
/// bit-for-bit from `tv/{prefix}_{field}_N{log_n}_in.txt` with `seed = 4567`.
pub fn verify_tv_case<F: SpielmanTvField>(tv_dir: &Path, shape: MatrixShape, log_n: u32) {
    let (n_rows, k) = shape.dims(log_n);
    let (in_name, out_name) = tv_filenames::<F>(shape, log_n);
    let in_path = tv_dir.join(&in_name);
    let out_path = tv_dir.join(&out_name);

    let input_matrix = load_tv_matrix::<F>(&in_path, n_rows, k)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", in_path.display(), e));
    let expected_output = load_tv_matrix::<F>(&out_path, n_rows, 2 * k)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", out_path.display(), e));

    let code = SpielmanOracleCode::<F>::sample_tv(k, SIMD_SPIELMAN_TV_SEED);

    // 1. Eager R2L (Alg. 3 + Alg. 4)
    let got_r2l_eager = encode_matrix_r2l_eager(&code, &input_matrix, n_rows);
    let out_cols = 2 * k;
    for r in 0..n_rows {
        for c in 0..out_cols {
            let idx = r * out_cols + c;
            if got_r2l_eager[idx] != expected_output[idx] {
                let (exp_0, exp_1) = expected_output[idx].to_tv_words();
                let (got_0, got_1) = got_r2l_eager[idx].to_tv_words();
                panic!(
                    "TV mismatch in {} (eager R2L) at row {} col {}: expected {:016x}_{:016x}, got {:016x}_{:016x}",
                    out_name, r, c, exp_0, exp_1, got_0, got_1
                );
            }
        }
    }

    // 2. Eager L2R (Alg. 2 + Alg. 4) must be bit-exact with Eager R2L and TV
    let got_l2r_eager = encode_matrix_l2r_eager(&code, &input_matrix, n_rows);
    assert_eq!(
        got_l2r_eager, expected_output,
        "TV mismatch in {} (eager L2R)",
        out_name
    );

    // 3. Lazy R2L via LazyField must also be bit-exact with Eager R2L and TV
    let got_r2l_lazy = encode_matrix_r2l_lazy(&code, &input_matrix, n_rows);
    assert_eq!(
        got_r2l_lazy, expected_output,
        "TV mismatch in {} (lazy R2L)",
        out_name
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compares [`GlibcRandom`] against the system's real `glibc` `srandom` / `random`
    /// via `libc` across multiple seeds (`4567`, `0`, `1`, `0xdeadbeef`, `0x80000000`,
    /// `u32::MAX`) and 10,000 draws per seed, plus `rand64` and `rand128`.
    #[test]
    fn test_glibc_random_matches_libc() {
        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        {
            eprintln!("SKIPPED: real glibc random() check only runs on *-linux-gnu targets");
            return;
        }

        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        {
            extern "C" {
                fn srandom(seed: libc::c_uint);
                fn random() -> libc::c_long;
            }

            let seeds = [
                SIMD_SPIELMAN_TV_SEED,
                0u32,
                1u32,
                42u32,
                0xdead_beefu32,
                0x8000_0000u32,
                u32::MAX,
            ];

            for &seed in &seeds {
                let mut our_rng = GlibcRandom::new(seed);
                // SAFETY: `srandom` and `random` are standard C library functions in glibc;
                // this single-threaded test serializes all calls to the global state within
                // this loop.
                unsafe {
                    srandom(seed as libc::c_uint);
                }

                for step in 0..10_000 {
                    let expected = unsafe { random() } as u32;
                    let got = our_rng.random();
                    assert_eq!(
                        got, expected,
                        "GlibcRandom mismatch vs glibc random() at seed={seed}, step={step}"
                    );
                }

                // Also verify rand64 and rand128 compositions against glibc random()
                for step in 0..1_000 {
                    let r0 = unsafe { random() } as u64;
                    let r1 = unsafe { random() } as u64;
                    let r2 = unsafe { random() } as u64;
                    let expected_64 = (((r0 << 31) ^ r1) << 31) ^ r2;
                    assert_eq!(
                        our_rng.rand64(),
                        expected_64,
                        "rand64 mismatch at seed={seed}, step={step}"
                    );

                    let q0 = unsafe { random() } as u128;
                    let q1 = unsafe { random() } as u128;
                    let q2 = unsafe { random() } as u128;
                    let q3 = unsafe { random() } as u128;
                    let q4 = unsafe { random() } as u128;
                    let expected_128 = (((((((q0 << 31) ^ q1) << 31) ^ q2) << 31) ^ q3) << 31) ^ q4;
                    assert_eq!(
                        our_rng.rand128(),
                        expected_128,
                        "rand128 mismatch at seed={seed}, step={step}"
                    );
                }

                // Verify libc::srand matches srandom
                let mut our_rng_2 = GlibcRandom::new(seed);
                unsafe {
                    libc::srand(seed as libc::c_uint);
                }
                for step in 0..256 {
                    let expected = unsafe { random() } as u32;
                    let got = our_rng_2.random();
                    assert_eq!(
                        got, expected,
                        "GlibcRandom mismatch after libc::srand at seed={seed}, step={step}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_simd_spielman_dimensions_and_transpose() {
        let code = SpielmanOracleCode::<Mont127>::sample_tv(64, SIMD_SPIELMAN_TV_SEED);
        assert_eq!(code.k, 64);
        assert_eq!(code.num_levels(), 2);
        // Level 0: L1 = 64, R1 = floor(0.238 * 64) = 15
        assert_eq!(code.graphs1_l2r[0].n_left, 64);
        assert_eq!(code.graphs1_l2r[0].n_right, 15);
        // Level 1: L1 = 15, R1 = floor(0.238 * 15) = 3 (< 13 => base case returns 3)
        assert_eq!(code.graphs1_l2r[1].n_left, 15);
        assert_eq!(code.graphs1_l2r[1].n_right, 3);
        // Level 1 postcode: L2 = 3, R2 = floor(15 * 0.72 - 3) = 7
        assert_eq!(code.graphs2_l2r[1].n_left, 3);
        assert_eq!(code.graphs2_l2r[1].n_right, 7);
        //Level 0 postcode: L2 = 15 + 3 + 7 = 25, R2 = floor(64 * 0.72 - 25) = 21
        assert_eq!(code.graphs2_l2r[0].n_left, 25);
        assert_eq!(code.graphs2_l2r[0].n_right, 21);
        assert_eq!(code.codeword_len, 64 + 25 + 21);

        // Verify O(R + E) counting-sort transpose matches upstream's O(R * E) scan in ExpanderGraph.h
        for g_l2r in code.graphs1_l2r.iter().chain(code.graphs2_l2r.iter()) {
            let g_r2l = g_l2r.transpose();
            let num_edges = g_l2r.n_left * g_l2r.left_degree;
            let mut naive_degrees = vec![0u32; g_l2r.n_right];
            let mut naive_col_idx = Vec::with_capacity(num_edges);
            let mut naive_weights = Vec::with_capacity(num_edges);
            for r in 0..g_l2r.n_right {
                let mut d = 0u32;
                for edge_index in 0..num_edges {
                    if g_l2r.connections[edge_index] as usize == r {
                        naive_col_idx.push((edge_index / g_l2r.left_degree) as u32);
                        naive_weights.push(g_l2r.weights[edge_index]);
                        d += 1;
                    }
                }
                naive_degrees[r] = d;
            }
            assert_eq!(g_r2l.rnode_degrees, naive_degrees);
            assert_eq!(g_r2l.col_idx, naive_col_idx);
            assert_eq!(g_r2l.weights, naive_weights);
        }
    }

    #[test]
    fn test_fp2m61_high_degree_fold_exercised() {
        // For Orion N=2^16, k = 512, average right-node degree in graphs1[0] is 10 / 0.238 ~ 42.02 > 32.
        let code = SpielmanOracleCode::<Fp2M61>::sample_tv(512, SIMD_SPIELMAN_TV_SEED);
        let max_deg = *code.graphs1_r2l[0].rnode_degrees.iter().max().unwrap() as usize;
        assert!(
            max_deg > Fp2M61::MAX_ACCUMULATIONS,
            "expected max in-degree ({max_deg}) > Fp2M61::MAX_ACCUMULATIONS ({})",
            Fp2M61::MAX_ACCUMULATIONS
        );
    }

    #[test]
    fn test_missing_tv_dir_returns_none() {
        let missing = Path::new("/nonexistent/simd-spielman/tv-directory");
        assert!(locate_tv_dir_with_override(Some(missing)).is_none());
    }

    #[test]
    fn test_linearity_and_systematic_prefix() {
        let k = 64;
        let code = SpielmanOracleCode::<Mont127>::sample_tv(k, SIMD_SPIELMAN_TV_SEED);
        let mut rng = GlibcRandom::new(9999);

        let x: Vec<Mont127> = (0..k)
            .map(|_| Mont127::sample_packed_weight(&mut rng))
            .collect();
        let y: Vec<Mont127> = (0..k)
            .map(|_| Mont127::sample_packed_weight(&mut rng))
            .collect();
        let a = Mont127::sample_packed_weight(&mut rng);

        let ax_plus_y: Vec<Mont127> = x.iter().zip(y.iter()).map(|(&xi, &yi)| a * xi + yi).collect();

        let enc_x = encode_matrix_r2l_eager(&code, &x, 1);
        let enc_y = encode_matrix_r2l_eager(&code, &y, 1);
        let enc_comb = encode_matrix_r2l_eager(&code, &ax_plus_y, 1);

        // Systematic prefix check
        assert_eq!(&enc_x[..k], &x[..]);
        assert_eq!(&enc_y[..k], &y[..]);

        // Algebraic linearity: Enc(a*x + y) == a*Enc(x) + Enc(y)
        for i in 0..enc_comb.len() {
            assert_eq!(enc_comb[i], a * enc_x[i] + enc_y[i]);
        }
    }

    fn run_tv<F: SpielmanTvField>(shape: MatrixShape, log_n: u32) {
        let tv_dir = locate_tv_dir().unwrap_or_else(|| {
            panic!(
                "required SIMD-Spielman tv/ directory not found; set {} or checkout SIMD-Spielman-for-ZKPs/tv",
                SIMD_SPIELMAN_TV_DIR_ENV
            )
        });
        verify_tv_case::<F>(&tv_dir, shape, log_n);
    }

    // --- Brakedown shape (bd_spielman_*) ---

    #[test]
    fn tv_brakedown_montgomery_n12() {
        run_tv::<Mont127>(MatrixShape::Brakedown, 12);
    }

    #[test]
    fn tv_brakedown_montgomery_n14() {
        run_tv::<Mont127>(MatrixShape::Brakedown, 14);
    }

    #[test]
    fn tv_brakedown_montgomery_n16() {
        run_tv::<Mont127>(MatrixShape::Brakedown, 16);
    }

    #[test]
    fn tv_brakedown_mersenne_n12() {
        run_tv::<Mers127>(MatrixShape::Brakedown, 12);
    }

    #[test]
    fn tv_brakedown_mersenne_n14() {
        run_tv::<Mers127>(MatrixShape::Brakedown, 14);
    }

    #[test]
    fn tv_brakedown_mersenne_n16() {
        run_tv::<Mers127>(MatrixShape::Brakedown, 16);
    }

    #[test]
    fn tv_brakedown_extension_n12() {
        run_tv::<Fp2M61>(MatrixShape::Brakedown, 12);
    }

    #[test]
    fn tv_brakedown_extension_n14() {
        run_tv::<Fp2M61>(MatrixShape::Brakedown, 14);
    }

    #[test]
    fn tv_brakedown_extension_n16() {
        run_tv::<Fp2M61>(MatrixShape::Brakedown, 16);
    }

    // --- Orion shape (spielman_*) ---

    #[test]
    fn tv_orion_montgomery_n12() {
        run_tv::<Mont127>(MatrixShape::Orion, 12);
    }

    #[test]
    fn tv_orion_montgomery_n14() {
        run_tv::<Mont127>(MatrixShape::Orion, 14);
    }

    #[test]
    fn tv_orion_montgomery_n16() {
        run_tv::<Mont127>(MatrixShape::Orion, 16);
    }

    #[test]
    fn tv_orion_mersenne_n12() {
        run_tv::<Mers127>(MatrixShape::Orion, 12);
    }

    #[test]
    fn tv_orion_mersenne_n14() {
        run_tv::<Mers127>(MatrixShape::Orion, 14);
    }

    #[test]
    fn tv_orion_mersenne_n16() {
        run_tv::<Mers127>(MatrixShape::Orion, 16);
    }

    #[test]
    fn tv_orion_extension_n12() {
        run_tv::<Fp2M61>(MatrixShape::Orion, 12);
    }

    #[test]
    fn tv_orion_extension_n14() {
        run_tv::<Fp2M61>(MatrixShape::Orion, 14);
    }

    #[test]
    fn tv_orion_extension_n16() {
        run_tv::<Fp2M61>(MatrixShape::Orion, 16);
    }
}
