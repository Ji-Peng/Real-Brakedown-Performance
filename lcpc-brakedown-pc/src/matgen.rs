// Copyright 2021 Riad S. Wahby <rsw@cs.stanford.edu>
//
// This file is part of lcpc-brakedown-pc, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.
//
// Modified 2026 by the RealBrakedownPerf authors (Apache-2.0, see NOTICE):
// transposed (R2L) graph storage and runtime code parameters.
// The per-commit changes are in the git history since step/00-import
// (upstream conroi/lcpc f0e2116).

/*!
generate a random code for a given n

Graphs are sampled left to right, one left node at a time, exactly as the code
specification prescribes, and then transposed into [`R2lGraph`] form by a
linear-time counting sort (`step/04-r2l`):

- mode (a), [`CodeParams::Lcpc`]: upstream LCPC's sampler, unchanged. Level `i`
  uses its own `ChaCha20Rng` stream `(seed, i)` and samples the precode and
  then the postcode; each left node gets `d` *distinct* right neighbors
  (rejection), sorted, followed by `d` nonzero weights. The graphs are
  bit-exact with `f0e2116`.
- mode (b), [`CodeParams::SimdSpielman`]: SIMD-Spielman's dimensions and
  sampling structure (depth-first order `G1[0], ..., G1[L-1], G2[L-1], ...,
  G2[0]`; per left node, `d` right neighbors *with replacement*, each followed
  by its weight). The random source here is one `ChaCha20Rng` stream seeded with
  `seed` (right node uniform via LCPC's `LegacyUniformUsize`, weight nonzero via
  `F::random`), not glibc `random()`. Upstream's exact graphs (for its `tv/`
  test vectors) are reproduced by the test-only sampler in
  `lcpc-spielman-oracle` and passed in through [`ExpanderCode::from_graphs`].
*/

use crate::codespec::{CodeParams, LcpcParams, SdigSpecification, SimdSpielmanParams};
use crate::encode::{BaseCase, ExpanderCode};
use crate::graph::R2lGraph;

use ff::Field;
use itertools::iterate;
use lcpc_2d::{LegacyUniformUsize, SizedField};
use lcpc_fields::LazyField;
use num_traits::Num;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rayon::prelude::*;
use std::iter::repeat_with;

const fn ceil_muldiv(n: usize, num: usize, den: usize) -> usize {
    (n * num + den - 1) / den
}

/// Dimensions of one recursion level `i` of the code.
///
/// The precode graph maps `n` left nodes to `m` right nodes with left degree
/// `c`; the postcode graph maps `n_prime` left nodes (the encoding of the
/// precode output) to `m_prime` right nodes with left degree `d`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelDims {
    /// precode inputs `n_i`
    pub n: usize,
    /// precode outputs `m_i`
    pub m: usize,
    /// precode left degree `c_n`
    pub c: usize,
    /// postcode inputs `n'_i`
    pub n_prime: usize,
    /// postcode outputs `m'_i`
    pub m_prime: usize,
    /// postcode left degree `d_n`
    pub d: usize,
}

/// Generate a random code from a given seed (mode (a) with specification `S`)
///
/// Returns the precode and postcode graphs of every level, as upstream did
/// (upstream returned `sprs::CsMat`s; the graphs are the same).
pub fn generate<F, S>(n: usize, seed: u64) -> (Vec<R2lGraph<F>>, Vec<R2lGraph<F>>)
where
    F: Field + Num + SizedField,
    S: SdigSpecification,
{
    generate_lcpc(n, seed, &LcpcParams::of::<S>())
}

/// Sample the expander code for `params` and message length `n` from `seed`.
pub fn generate_code<F>(params: &CodeParams, n: usize, seed: u64) -> ExpanderCode<F>
where
    F: LazyField,
{
    match params {
        CodeParams::Lcpc(p) => {
            let (precodes, postcodes) = generate_lcpc(n, seed, p);
            ExpanderCode::from_graphs(BaseCase::ReedSolomon, precodes, postcodes)
        }
        CodeParams::SimdSpielman => {
            let (precodes, postcodes) = generate_simd_spielman(n, seed);
            ExpanderCode::from_graphs(BaseCase::Identity, precodes, postcodes)
        }
    }
}

/// Dimensions of every level of the code for `params`, message length `n`, and
/// a field of `log2p` bits (`SizedField::FLOG2`, used by mode (a) only).
pub fn level_dims(params: &CodeParams, n: usize, log2p: f64) -> Vec<LevelDims> {
    match params {
        CodeParams::Lcpc(p) => {
            let (pre_dims, post_dims) = get_dims(n, log2p, p);
            pre_dims
                .iter()
                .zip(&post_dims)
                .map(|(&(n, m, c), &(n_prime, m_prime, d))| LevelDims {
                    n,
                    m,
                    c,
                    n_prime,
                    m_prime,
                    d,
                })
                .collect()
        }
        CodeParams::SimdSpielman => simd_spielman_dims(n),
    }
}

/// Mode (a): sample LCPC's graphs for message length `n`.
pub fn generate_lcpc<F>(
    n: usize,
    seed: u64,
    params: &LcpcParams,
) -> (Vec<R2lGraph<F>>, Vec<R2lGraph<F>>)
where
    F: Field + Num + SizedField,
{
    let (pre_dims, post_dims) = get_dims(n, F::FLOG2 as f64, params);
    assert!(!pre_dims.is_empty());

    let mut precodes = Vec::with_capacity(pre_dims.len());
    let mut postcodes = Vec::with_capacity(pre_dims.len());
    pre_dims[..]
        .par_iter()
        .zip(&post_dims[..])
        .enumerate()
        .map(|(i, (&(ni, mi, cn), &(nip, mip, dn)))| {
            let mut rng = ChaCha20Rng::seed_from_u64(seed);
            rng.set_stream(i as u64);
            let precode = gen_code(ni, mi, cn, &mut rng);
            let postcode = gen_code(nip, mip, dn, &mut rng);
            (precode, postcode)
        })
        .unzip_into_vecs(&mut precodes, &mut postcodes);

    (precodes, postcodes)
}

// compute dimensions for all of the matrices used by this code
#[allow(clippy::type_complexity)]
fn get_dims(
    n: usize,
    log2p: f64,
    params: &LcpcParams,
) -> (Vec<(usize, usize, usize)>, Vec<(usize, usize, usize)>) {
    use std::cmp::{max, min};
    let baselen = params.baselen();
    assert!(n > baselen);

    // figure out dimensions for the precode and postcode matrices
    let pre_dims = {
        let mut tmp: Vec<_> = iterate(n, |&ni| {
            ceil_muldiv(ni, params.alpha_num(), params.alpha_den())
        })
        .take_while(|&ni| ni > baselen)
        .collect();
        if let Some(&ni) = tmp.last() {
            let last = ceil_muldiv(ni, params.alpha_num(), params.alpha_den());
            assert!(last <= baselen);
            tmp.push(last);
        }
        assert!(tmp.len() > 1);
        tmp[..]
            .windows(2)
            .map(|nm| {
                let ni = nm[0];
                let mi = nm[1];
                let cn = min(
                    max(
                        ceil_muldiv(ni, 32 * params.beta_num(), 25 * params.beta_den()),
                        4 + ceil_muldiv(ni, params.beta_num(), params.beta_den()),
                    ),
                    ((110f64 / (ni as f64) + params.cnst_cn_1()) / params.cnst_cn_2()).ceil()
                        as usize,
                );
                let cn = min(cn, mi); // can't generate more nonzero entries than there are columns
                (ni, mi, cn)
            })
            .collect::<Vec<_>>()
    };

    let post_dims = pre_dims
        .iter()
        .map(|&(ni, mi, _)| {
            let niprime = ceil_muldiv(mi, params.r_num(), params.r_den());
            let miprime = ceil_muldiv(ni, params.r_num(), params.r_den()) - ni - niprime;
            let tmp1 = ceil_muldiv(ni, 2 * params.beta_num(), params.beta_den()); // 2 * beta * ni
            let tmp2 = ceil_muldiv(ni, params.r_num(), params.r_den()) - ni + 110; // ni * (r - 1 + 110/ni)
            let dn = min(
                tmp1 + (tmp2 as f64 / log2p).ceil() as usize,
                ((110f64 / (ni as f64) + params.cnst_dn_1()) / params.cnst_dn_2()).ceil() as usize,
            );
            let dn = min(dn, miprime); // can't generate more nonzero entries than there are columns
            (niprime, miprime, dn)
        })
        .collect::<Vec<_>>();

    assert_eq!(pre_dims.len(), post_dims.len());
    (pre_dims, post_dims)
}

// generate a code matrix of a given size with specified row density
//
// The graph is sampled exactly as upstream did (into what upstream passed to
// `CsMat::new_csc`) and then transposed into R2L form.
fn gen_code<F, R>(n: usize, m: usize, d: usize, mut rng: R) -> R2lGraph<F>
where
    F: Field + Num,
    R: Rng,
{
    let dist = LegacyUniformUsize::new(0, m);
    let mut data = Vec::<F>::with_capacity(d * n);
    let mut idxs = Vec::<usize>::with_capacity(d * n);
    let mut ptrs = Vec::<usize>::with_capacity(1 + n);
    ptrs.push(0); // ptrs always starts with 0

    for _ in 0..n {
        // for each row, sample d random nonzero columns (without replacement)
        let cols = {
            // for small d, the quadratic approach is almost certainly faster
            let mut tmp = Vec::with_capacity(d);
            assert_eq!(
                d,
                repeat_with(|| dist.sample(&mut rng))
                    .filter(|&x| {
                        if tmp.contains(&x) {
                            false
                        } else {
                            tmp.push(x);
                            true
                        }
                    })
                    .take(d)
                    .count()
            );
            tmp.sort_unstable(); // upstream sorted to supply to new_csc
            tmp
        };
        assert_eq!(d, cols.len());

        // sample random elements for each column
        let mut last = m + 1;
        for &col in &cols[..] {
            // detect and skip repeats
            if col == last {
                continue;
            }
            last = col;

            idxs.push(col);
            data.push(random_nonzero(&mut rng));
        }
        ptrs.push(data.len());
    }

    R2lGraph::from_left_edges(n, m, &ptrs, &idxs, &data)
}

// sample a nonzero field element by rejection (upstream LCPC's weight sampler)
fn random_nonzero<F: Field, R: Rng>(rng: &mut R) -> F {
    let mut tmp = F::random(&mut *rng);
    while <F as Field>::is_zero(&tmp).into() {
        tmp = F::random(&mut *rng);
    }
    tmp
}

/// Mode (b): dimensions of SIMD-Spielman's code for message length `k`.
///
/// This is `SpielmanCode::initExpanders` (`SpielmanCode.h`): a level with
/// `n >= 13` left nodes gets a precode with `floor(0.238 * n)` right nodes, the
/// next level is built on those, and the postcode maps the `n'` outputs of that
/// sub-encoding to `floor(n * (1.72 - 1) - n')` right nodes. The floors are the
/// same `double`-to-`uint32_t` conversions upstream performs.
///
/// # Panics
///
/// Panics if `k < 13` (no level at all) or `k > u32::MAX`.
pub fn simd_spielman_dims(k: usize) -> Vec<LevelDims> {
    fn init_expanders(nr_left_nodes: u32, levels: &mut Vec<LevelDims>) -> u32 {
        if (nr_left_nodes as usize) < SimdSpielmanParams::DISTANCE_THRESHOLD {
            return nr_left_nodes;
        }
        let r1 = (SimdSpielmanParams::ALPHA * nr_left_nodes as f64) as u32;
        let idx = levels.len();
        levels.push(LevelDims {
            n: nr_left_nodes as usize,
            m: r1 as usize,
            c: SimdSpielmanParams::CN,
            n_prime: 0,
            m_prime: 0,
            d: SimdSpielmanParams::DN,
        });
        let nr_nodes = init_expanders(r1, levels);
        let r2 = (nr_left_nodes as f64 * (SimdSpielmanParams::R - 1.0) - nr_nodes as f64) as u32;
        assert!(r2 > 0, "SIMD-Spielman postcode with no right nodes");
        levels[idx].n_prime = nr_nodes as usize;
        levels[idx].m_prime = r2 as usize;
        nr_left_nodes + nr_nodes + r2
    }

    assert!(
        k >= SimdSpielmanParams::DISTANCE_THRESHOLD,
        "SIMD-Spielman code needs k >= {}",
        SimdSpielmanParams::DISTANCE_THRESHOLD
    );
    let k = u32::try_from(k).expect("message length exceeds u32");
    let mut levels = Vec::new();
    init_expanders(k, &mut levels);
    levels
}

/// Mode (b): sample SIMD-Spielman-shaped graphs for message length `k`.
///
/// Graphs are drawn in upstream's depth-first order from a single
/// `ChaCha20Rng` stream (see the module documentation).
pub fn generate_simd_spielman<F>(k: usize, seed: u64) -> (Vec<R2lGraph<F>>, Vec<R2lGraph<F>>)
where
    F: Field,
{
    let levels = simd_spielman_dims(k);
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut sample = |n: usize, m: usize, d: usize| {
        let dist = LegacyUniformUsize::new(0, m);
        let mut right_idx = Vec::<u32>::with_capacity(n * d);
        let mut weights = Vec::<F>::with_capacity(n * d);
        for _ in 0..n * d {
            right_idx.push(dist.sample(&mut rng) as u32);
            weights.push(random_nonzero(&mut rng));
        }
        R2lGraph::from_left_edges_fixed_degree(n, m, d, &right_idx, &weights)
    };

    let precodes: Vec<_> = levels.iter().map(|l| sample(l.n, l.m, l.c)).collect();
    let mut postcodes: Vec<_> = levels
        .iter()
        .rev()
        .map(|l| sample(l.n_prime, l.m_prime, l.d))
        .collect();
    postcodes.reverse();
    (precodes, postcodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simd_spielman_dims_small() {
        // k = 64: 64 -> 15 -> 3 (< 13 stops); see lcpc-spielman-oracle
        let dims = simd_spielman_dims(64);
        assert_eq!(dims.len(), 2);
        assert_eq!(
            (dims[0].n, dims[0].m, dims[0].n_prime, dims[0].m_prime),
            (64, 15, 25, 21)
        );
        assert_eq!(
            (dims[1].n, dims[1].m, dims[1].n_prime, dims[1].m_prime),
            (15, 3, 3, 7)
        );
        assert!(dims.iter().all(|l| l.c == 10 && l.d == 20));
    }
}
