<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 01: Workspace Modernization (`step/01-modernize`)

This document records the toolchain, compiler profile, Cargo workspace, and
dependency updates applied in `step/01-modernize`, together with the RNG
compatibility layer that preserves **bit-exact** outputs (expander/Reed-Solomon
code matrices, codewords, Merkle roots, Fiat-Shamir column openings, and
`bincode`-serialized evaluation proofs) relative to original upstream LCPC
(`f0e2116001af5a1bbf39481535f837d6a25772e8`).

No algorithmic changes to encoding, commitment, proving, or verification are
made in this step.

---

## 1. Toolchain, Edition, and Scalar Codegen Configuration

1. **`rust-toolchain.toml`**: Pins stable Rust `1.99.1` (`components = ["rustfmt", "clippy"]`).
2. **Rust Edition**: All workspace crates (`lcpc-2d`, `lcpc-brakedown-pc`,
   `lcpc-ligero-pc`, `lcpc-test-fields`, and the new `bench` crate `lcpc-bench`)
   are updated from `edition = "2018"` to `edition = "2021"` with workspace
   `resolver = "2"`.
3. **`.cargo/config.toml` & `[profile.release]`**:
   - Root `Cargo.toml` configures `[profile.release]` with `opt-level = 3`,
     `codegen-units = 1`, and `lto = "fat"`.
   - `.cargo/config.toml` sets `["-C", "target-cpu=native", "-C", "no-vectorize-loops", "-C", "no-vectorize-slp"]`
     under `[target.x86_64-unknown-linux-gnu]` and `["-C", "no-vectorize-loops", "-C", "no-vectorize-slp"]`
     under `[build]`, disabling LLVM loop and SLP auto-vectorization while
     allowing scalar ISA extensions (such as BMI2 `mulx` and ADX `adcx`/`adox`
     on x86-64).
4. **Tracked `Cargo.lock`**: Removed `Cargo.lock` from `.gitignore` so exact
   dependency versions are locked and reproducible across machines.
5. **Removal of Nightly `#![feature(test)]`**:
   - Removed `#![feature(test)]`, `extern crate test;`, `src/bench.rs`, and the
     `[features]` sections from `lcpc-brakedown-pc` and `lcpc-ligero-pc` (as
     well as `def_bench!` and `paste` from `lcpc-test-fields`).
   - Added the stable Rust workspace member `bench/` (`lcpc-bench`), whose CLI
     flags (`--params`, `--logN`, `--runs`, `--warmup`, `--threads`, `--cores`,
     `--output`, `--generate-golden`, `--check-golden`) and `lcpc-bench-v1` JSON
     output schema match `external/lcpc-orig` (see `docs/bench-json-schema.md`).

---

## 2. Dependency Version Matrix

| Dependency | Upstream (`f0e2116`) | Modernized (`step/01-modernize`) | Notes |
| :--- | :--- | :--- | :--- |
| `ff` | `0.12.1` | `0.14.0` | Associated constants `Field::ZERO`, `Field::ONE`, `PrimeField::ROOT_OF_UNITY`; uses `rand_core 0.10`. |
| `digest` | `0.9.0` | `0.10.7` | Updated `Digest`, `Update`, `FixedOutput`, `FixedOutputReset`, `OutputSizeUser`, `HashMarker`. |
| `merlin` | `2.0.1` | `3.0.0` | Fiat-Shamir STROBE-128 transcript (`Transcript::new`, `append_message`, `challenge_bytes` byte-identical to 2.0). |
| `rand` | `0.8.5` | `0.10.3` | Shares `rand_core 0.10.0` with `ff 0.14.0` and `rand_chacha 0.10.0`. |
| `rand_chacha` | `0.3.1` | `0.10.0` | Deterministic DJB ChaCha20 stream (`seed_from_u64`, `from_seed`, `set_stream`). |
| `ppv-lite86` | `0.2` (`simd`) | `0.2.21` (`no_simd`) | `default-features = false, features = ["no_simd"]` forces pure scalar `generic` backend in `rand_chacha`. |
| `rayon` | `1.5` | `1.11.0` | Parallel iterators and `rayon::join`. |
| `sprs` | `0.10.0` | `0.11.6` | CSC sparse matrix representation (`CsMat`, `MulAcc`, `TriMat`). |
| `ndarray` | `0.14.0` | `0.17.2` | Dense array views (`ArrayView`, `Dot`, `into_raw_vec_and_offset`). |
| `typenum` | `1.13` | `1.19.0` | Compile-time rational code parameters (`SdigSpecification`, `LigeroEncodingRho`). |
| `itertools` | `0.10` | `0.14.0` | `iterate` helper in `matgen`, `fft`, and tests. |
| `num-traits` | `0.2` | `0.2.19` | `Num`, `Zero`, `One`, `MulAdd` bounds. |
| `err-derive` | `0.2` | `0.3.1` | Error derive macro in `lcpc-2d` and `lcpc-test-fields::fft`. |
| `serde` | `1.0` | `1.0.228` | Proof and commitment serialization. |
| `serde_bytes` | `0.11` | `0.11.19` | Byte-vector serialization in `lcpc-2d`. |
| `bincode` | `1.3` | `1.3.3` | Binary proof serialization in tests and benchmarks. |
| `blake3` | `1.x` (`traits-preview`) | `1.8.7` (portable-only) | `default-features = false, features = ["std", "pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]`. |
| `ff-derive-num` | `0.2.0` | Replaced (`impl_num!`) | Upstream `ff-derive-num 0.2.0` (2021) is unmaintained and calls removed `Field::zero()` / `Field::one()`; replaced by `lcpc_test_fields::impl_num!`. |
| `fffft` | `0.4.2` | Ported in-tree (`lcpc_test_fields::fft`) | Upstream `fffft 0.4.2` (2022) is unmaintained and locked to `ff = "0.12"`; ported to `ff 0.14` in `lcpc-test-fields/src/fft.rs` and re-exported by `lcpc-ligero-pc`. |

---

## 3. Portable-Only BLAKE3 + `digest 0.10` Integration

To satisfy the project's portable-only scalar policy:

- `blake3 1.8.7` is compiled across the workspace with `default-features = false`
  and `features = ["std", "pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]`.
- Because `blake3 1.8.7`'s optional `traits-preview` feature targets `digest 0.11`
  whereas the workspace standardizes on `digest 0.10`, `lcpc_test_fields::Blake3`
  provides a thin wrapper around `blake3::Hasher` implementing `digest 0.10`'s
  `HashMarker`, `OutputSizeUser<OutputSize = U32>`, `Update`, `Reset`,
  `FixedOutput`, and `FixedOutputReset` traits (which blanket-implement
  `digest::Digest`).

---

## 4. Bit-Exact RNG & `Uniform<usize>` Compatibility (`LegacyUniformUsize`)

Upgrading `ff` (`0.12` -> `0.14`), `rand_chacha` (`0.3` -> `0.10`), and `merlin`
(`2.0` -> `3.0`) preserves:

1. **ChaCha20 word stream**: `ChaCha20Rng::seed_from_u64`, `ChaCha20Rng::from_seed`,
   and `ChaCha20Rng::set_stream` generate the exact same sequence of 64-bit words
   (`next_u64()`).
2. **`PrimeField` random sampling (`ff_derive`)**: `ff_derive 0.14.0` generates
   the exact same limb-by-limb `try_next_u64()` + `REPR_SHAVE_BITS` bitmask +
   `< MODULUS` rejection loop as `ff_derive 0.12.1`, using the accepted limbs
   directly as the Montgomery representation.
3. **Merlin transcript challenges**: `merlin 3.0.0` produces byte-for-byte
   identical `challenge_bytes` outputs to `merlin 2.0.1`.

However, **`rand`'s `Uniform<usize>` algorithm changed in `rand 0.9` / `0.10`**:

- In `rand 0.8` (`uniform_int_impl! { usize, usize, usize }` on 64-bit targets),
  `Uniform::new(0usize, high)` computed:
  - `range = high as u64`
  - `ints_to_reject = (u64::MAX - range + 1) % range`
  - `zone = u64::MAX - ints_to_reject`
  and in each iteration drew a **64-bit** word `v = rng.next_u64()`, computed the
  128-bit widening product `mul = (v as u128) * (range as u128)`, and accepted
  `hi = (mul >> 64) as usize` when `(mul as u64) <= zone`.
- In `rand 0.9` and `rand 0.10`, `UniformUsize` inspects `high <= u32::MAX as usize`
  and switches to **32-bit** sampling (`next_u32()`), while also changing the
  rejection predicate from `lo <= zone` to `lo >= thresh`.

Because both `lcpc-brakedown-pc::matgen::gen_code` (which samples `d` distinct
column indices per row in `0..m`) and `lcpc-2d::{prove, verify}` (which sample
`n_col_opens` column indices in `0..n_cols`) use `Uniform<usize>`, using
`rand 0.9`/`0.10`'s `Uniform<usize>` directly would alter every generated code
matrix and every opened column index.

To preserve **100% bit-exactness** with original LCPC (`f0e2116`), `lcpc-2d`
provides `LegacyUniformUsize` implementing `rand 0.8`'s exact 64-bit Lemire
rejection sampler, and both `lcpc-2d` (`prove` and `verify`) and
`lcpc-brakedown-pc::matgen` (`gen_code`) use `LegacyUniformUsize`.

---

## 5. Golden-Vector Verification

Bit-exactness against original LCPC (`f0e2116`) is verified in two places:

1. **Unit/Integration Test (`lcpc-brakedown-pc/src/tests.rs`)**:
   `golden_vectors_bit_exact_with_lcpc_orig` loads
   `tests/golden/lcpc-orig/golden-v1.json` (generated by `external/lcpc-orig`
   compiled directly against pinned upstream `f0e2116`) and checks all 12 golden
   cases (`Ft127` and `Ft63`, `SdigCode6` and `SdigCode3`, across 3 matrix
   shapes):
   - All sampled polynomial coefficients (`coefficients_hex`)
   - All encoded matrix rows and columns (`encoded_rows_hex`)
   - The 2D Merkle commitment root (`commitment_root_hex`)
   - The `bincode`-serialized evaluation proof byte length (`proof_bincode_bytes`)
     and BLAKE3 digest (`proof_bincode_blake3_hex`)
   - Successful proof verification and verified evaluation (`verified_evaluation_hex`)
2. **Benchmark Driver CLI (`lcpc-bench --check-golden`)**:
   Running `lcpc-bench --check-golden tests/golden/lcpc-orig/golden-v1.json`
   regenerates the complete `lcpc-golden-v1` JSON document and verifies that it
   matches `tests/golden/lcpc-orig/golden-v1.json` character-for-character.
