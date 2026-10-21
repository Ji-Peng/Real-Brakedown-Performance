<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Correctness Oracles (`M2`)

This document describes the test-only correctness oracles used to verify the
LCPC fork across code-parameter regimes, finite fields, and optimization steps
(Plan README §3, §6.1).

---

## Section A: Original LCPC (`f0e2116`) Bit-Exact Oracle

`lcpc-oracle-tests` is a test-only workspace crate. Its renamed `lcpc-orig-*`
dependencies point directly to the unmodified LCPC crates at
`f0e2116001af5a1bbf39481535f837d6a25772e8`; no source is copied into the
fork's library crates. This direct dependency route builds alongside the
modernized workspace, so the D11 golden-vector fallback is not used for this
oracle. The old `ff 0.12` / `digest 0.9` types coexist with the fork's `ff
0.14` / `digest 0.10` types in the test crate.

For local validation in this network-restricted environment, Cargo's canonical
Git URL was temporarily rewritten through a config under the ignored
`target-oracle-orig/` directory to use the same `f0e2116` object in the fork's
full Git history. `Cargo.lock` still records the canonical Git URL and pinned
revision; normal builds fetch that revision from the upstream URL.

The pinned original `lcpc-brakedown-pc` has an unconditional
`#![feature(test)]`, although this harness only calls its stable public API.
The workspace's `tools/rustc-oracle-wrapper.sh` sets
`RUSTC_BOOTSTRAP=lcpc_brakedown_pc` only for the rustc invocation whose source
is that pinned upstream crate. It leaves the fork and every other crate on
normal stable feature gates, including when a caller has exported a broader
`RUSTC_BOOTSTRAP` value. The driver itself and the test crate use stable Rust
APIs.

`fork_fields_match_original_lcpc_mode_a` compares the original `Ft127` against
both fork fields (`Ft127` and `Mont127`) for `SdigCode3` and `SdigCode6`. It
covers `(n_rows, n_per_row)` values `(2, 32)`, `(3, 64)`, and `(4, 128)`, each
with graph seeds `0`, `0x0123456789abcdef`, and `0xfedcba9876543210`. The old
and new ChaCha streams independently sample the coefficients and evaluation
point; the test compares their canonical `PrimeField` representations before
using the values in the two implementations.

For every case, the test checks the encoded matrix element-by-element, the
commitment root, byte-for-byte serialized evaluation proofs, and successful
verification with matching returned evaluations. Both implementations use a
fresh transcript initialized with the same domain and messages. The original
digest 0.9 API is adapted to BLAKE3 1.8.7; the fork uses its portable-only
BLAKE3 wrapper. The proof comparison covers the complete serialized proof.

Run this gate from `code/` after sourcing the pinned toolchain:

```sh
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$(pwd)/target-oracles"
taskset -c 0,1,3-7 cargo test -p lcpc-oracle-tests --release
```

Run Cargo from the `code/` repository root so `.cargo/config.toml` can locate
the wrapper at `tools/rustc-oracle-wrapper.sh`.

The test sets `RAYON_NUM_THREADS=1` itself. These small inputs are correctness
cases; their runtimes are not benchmark results.

---

## Section B: SIMD-Spielman (`tv/`) Test-Vector Oracle & Alg. 1–4 Reference Encoder

Implemented in the test-only workspace crate `lcpc-spielman-oracle` (`publish = false`).

### B.1 Overview & Purpose

In `SIMD-Spielman-for-ZKPs` (commit `7b399dbfecc406c79349015f1e21accc4def1be7`),
the test suite (`spielman/tests/Spielman*_Test.cpp`) validates single- and
multi-threaded Spielman encoding against 18 reference input/output matrix pairs
(36 files total) in `SIMD-Spielman-for-ZKPs/tv/`:

- **Matrix shapes** (`MatrixShape`):
  - **Brakedown square-ish shape** (`BRAKEDOWN = 1`, prefix `bd_spielman`):
    $\text{n\_rows} = 2^{\lfloor \log_2 N / 2 \rfloor}$, $k = N / \text{n\_rows}$.
  - **Orion fixed-row shape** (`BRAKEDOWN = 0`, prefix `spielman`):
    $\text{n\_rows} = 128$, $k = N / 128$.
- **Polynomial sizes**: $\log_2 N \in \{12, 14, 16\}$ ($N \in \{4096, 16384, 65536\}$).
- **Finite fields** (`SpielmanTvField`):
  - `montgomery` ([`lcpc_fields::Mont127`], $q = \text{0x6e754097ba20e0bf7f2bd90000000001}$),
  - `mersenne` ([`lcpc_fields::Mers127`], $p = 2^{127} - 1$),
  - `extension` ([`lcpc_fields::Fp2M61`], $\mathbb{F}_{p^2} = \mathbb{F}_p[i]/(i^2+1)$ with $p = 2^{61} - 1$).

`lcpc-spielman-oracle` reproduces upstream's exact graph generation and encodes
every `tv/*_in.txt` matrix using `lcpc-fields`'s `Mont127`, `Mers127`, and
`Fp2M61`, asserting **100% bit-exact equality** with `tv/*_out.txt` across all
18 test-vector pairs with **zero deviations**.

### B.2 `glibc` `TYPE_3` `srandom()` / `random()` & `rand64` / `rand128` (`GlibcRandom`)

Upstream `SIMD-Spielman-for-ZKPs` seeds the C library generator via `srand(4567)`
in `SpielmanCode.h` and draws graph edges and weights using `random()` in
`ExpanderGraph.h` and `utils/utils.h`. On Linux with `glibc`, `srand(seed)` is an
alias for `srandom(seed)`, which initializes the default `TYPE_3` nonlinear
additive feedback generator (`DEG_3 = 31`, `SEP_3 = 3` in `stdlib/random_r.c`):

1. **Seeding (`GlibcRandom::new(seed)`)**:
   - Maps `seed == 0` to `1`.
   - Sets `state[0] = seed as i32` and computes `state[1..31]` via the Park-Miller
     LCG (`hi = word / 127773`, `lo = word % 127773`,
     `word = 16807 * lo - 2836 * hi`, adding `2147483647` if `word < 0`).
   - Initializes `fptr = 3`, `rptr = 0`, and discards the first $10 \times 31 = 310$
     outputs.
2. **Step (`GlibcRandom::random()`)**:
   - Computes `val = state[fptr].wrapping_add(state[rptr])`, stores `state[fptr] = val`,
     advances `fptr` and `rptr` modulo 31, and returns `(val as u32) >> 1` in
     $[0, 2^{31} - 1]$.
3. **64-bit and 128-bit compositions (`utils/utils.h`)**:
   - `rand64()`: draws 3 consecutive `random()` values $r_0, r_1, r_2$ and returns
     $(((r_0 \ll 31) \oplus r_1) \ll 31) \oplus r_2$.
   - `rand128()`: draws 5 consecutive `random()` values $r_0, \dots, r_4$ and folds
     each via `rnd = (rnd << 31) ^ random()`.
4. **Verification against system `glibc`**:
   - `tests::test_glibc_random_matches_libc` checks `GlibcRandom::new`, `random()`,
     `rand64()`, and `rand128()` directly against `libc` `srandom` / `srand` and
     `random` across 7 seeds (`4567`, `0`, `1`, `42`, `0xdeadbeef`, `0x80000000`,
     `u32::MAX`) and 18,000 `random()` calls per seed on `*-linux-gnu` targets
     (and skips cleanly on non-glibc targets).

### B.3 Algorithm 1 Graph Sampling Call Order & Code Parameters (`SpielmanOracleCode`)

In `SIMD-Spielman-for-ZKPs/spielman/{SpielmanParams,SpielmanCode,ExpanderGraph}.h`:

1. **Parameters (`SimdSpielmanParams`)**:
   - $\alpha = 0.238$, $\beta = 0.1205$, $r = 1.72$, $\delta_0 = 0.07$.
   - Recursion stopping threshold: `distance_threshold = (int)(1.0 / 0.07) - 1 = 13`.
   - Fixed left degrees: $c_n = 10$ (precodes $G_1$) and $d_n = 20$ (postcodes $G_2$).
2. **Depth-First Recursive Graph Sampling (`SpielmanCode::initExpanders`)**:
   - When `nr_left_nodes < 13`, recursion stops without sampling any graph and returns
     `nr_left_nodes` (the base case is the identity map).
   - Otherwise, at level `rec`:
     1. Samples `graphs1[rec]` with `n_left = nr_left_nodes`,
        `n_right = (0.238 * nr_left_nodes as f64) as u32`, and `left_degree = 10`.
     2. Recursively calls `initExpanders(n_right, rec + 1)` to obtain `nr_nodes`.
     3. Samples `graphs2[rec]` with `n_left = nr_nodes`,
        `n_right = (nr_left_nodes as f64 * (1.72 - 1.0) - nr_nodes as f64) as u32`,
        and `left_degree = 20`.
     4. Returns `nr_left_nodes + nr_nodes + graphs2[rec].n_right`.
   - Consequently, `graphs1[0..L]` are sampled top-down ($0, 1, \dots, L-1$), and
     `graphs2[0..L]` are sampled bottom-up ($L-1, \dots, 1, 0$) from the single shared
     `GlibcRandom` stream.
3. **Per-Edge Sampling with Replacement (`L2rExpanderGraph::sample_with`)**:
   - For `l in 0..n_left` and `d in 0..left_degree`:
     1. Draws `rnode = rng.random() % (n_right as u32)` (with replacement; parallel
        edges between the same $(l, r)$ pair are retained as distinct entries).
     2. Immediately draws the edge weight via `F::sample_packed_weight(rng)`
        (`T::randomPacked()`):
        - `Mont127`: draws `rnd = rng.rand128() % q` and converts to canonical
          Montgomery form $rnd \cdot 2^{128} \bmod q$ (`Mont127::from_repr`).
          (In upstream `UnpackedMontgomeryPrimeField::randomPacked()`, `rnd` is scaled
          by $R_{\text{avx}} = 2^{135} \bmod q$ and `ExtUnpackedMontgomeryPrimeField::reduce()`
          divides by $2^{135} \bmod q$, so each edge multiplies the input residue by
          `rnd` modulo $q$.)
        - `Mers127`: draws `rnd = rng.rand128() % (2^127 - 1)`.
        - `Fp2M61`: draws `real = rng.rand64() % (2^61 - 1)` followed by
          `img = rng.rand64() % (2^61 - 1)`.
4. **L2R-to-R2L CSR Transpose (`L2rExpanderGraph::transpose`)**:
   - Transposes `L2rExpanderGraph<F>` into `R2lExpanderGraph<F>` in $\mathcal{O}(R + E)$
     time using a stable two-pass counting sort. Because the sort is stable, incoming
     edges for every right node appear in increasing `edge_index = l * left_degree + d`
     order, matching upstream's $\mathcal{O}(R \cdot E)$ scan in `ExpanderGraph.h`
     (verified by `tests::test_simd_spielman_dimensions_and_transpose`).

### B.4 Straightforward Algorithms 1–4 Oracle Encoders & `tv/` Verification

`lcpc-spielman-oracle` provides three independent reference encoders over
`SpielmanOracleCode<F>`:

1. `encode_row_l2r_eager` / `encode_matrix_l2r_eager`: Algorithm 2 (L2R scatter) +
   Algorithm 4 recursive encoding using eager field multiplications and additions.
2. `encode_row_r2l_eager` / `encode_matrix_r2l_eager`: Algorithm 3 (R2L gather) +
   Algorithm 4 recursive encoding using eager field multiplications and additions.
3. `encode_row_r2l_lazy` / `encode_matrix_r2l_lazy`: Algorithm 3 (R2L gather) +
   Algorithm 4 recursive encoding using `LazyField` unreduced accumulators
   (`prepare_weight`, `mac`, `fold` every `MAX_ACCUMULATIONS` edges, and `reduce`).

All three encoders are verified against all 18 test-vector pairs in
`SIMD-Spielman-for-ZKPs/tv/`:

| Shape | Prefix | $\log_2 N = 12$ | $\log_2 N = 14$ | $\log_2 N = 16$ | `Mont127` | `Mers127` | `Fp2M61` |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| Brakedown ($n_{\text{rows}} = 2^{\log_2 N / 2}$) | `bd_spielman` | $64 \times 64 \to 64 \times 128$ | $128 \times 128 \to 128 \times 256$ | $256 \times 256 \to 256 \times 512$ | Bit-exact | Bit-exact | Bit-exact |
| Orion ($n_{\text{rows}} = 128$) | `spielman` | $128 \times 32 \to 128 \times 64$ | $128 \times 128 \to 128 \times 256$ | $128 \times 512 \to 128 \times 1024$ | Bit-exact | Bit-exact | Bit-exact |

### B.5 Public API for `step/04-r2l` Fork Integration

`lcpc-spielman-oracle` exports [`GlibcRandom`], [`SpielmanTvField`], [`MatrixShape`],
[`SimdSpielmanParams`], [`L2rExpanderGraph`], [`R2lExpanderGraph`],
[`SpielmanOracleCode`], [`locate_tv_dir`], [`load_tv_input_matrix`],
[`load_tv_output_matrix`], and [`verify_tv_case`] so that in `step/04-r2l` (M3),
`lcpc-brakedown-pc` can import `lcpc-spielman-oracle` as a dev-dependency and
verify the fork's own R2L column-major encoder directly against all 18
`SIMD-Spielman-for-ZKPs/tv/` test-vector pairs.
