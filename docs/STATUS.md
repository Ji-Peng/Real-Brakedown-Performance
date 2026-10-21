<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Project Status: Milestones M0–M6 and the M8 Harness

Independent verification on 2026-10-21. It is measured against the exit criteria in the project plan (`agent/plan/README.md` §8.2) and the per-step plan (§8.1). I re-ran every check below myself from fresh target directories. I did not rely on earlier task reports or on `docs/REVIEW-M0-M6.md`.

**Local workstation only.** Every command ran on the development machine, an Intel Core i7-11700K under Ubuntu 26.04 LTS with Linux 7.0.0-30. Commands were pinned with `taskset` to cores `0,1,3-7`, or to core `3` for single-thread smoke benchmarks; CPU 2 was never used. Any timings mentioned here are for direction only and **must not be published**. No GCP VM was created.

Toolchain: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, `cargo 1.99.1`, GCC 13.4.0, GNU objdump 2.46, and the toolchain's `llvm-objdump` for cross-target objects.

Verified commit: `main` = `step/09-codegen` = `9f15bc2`, plus the uncommitted verification fixes listed in §3. Those fixes do not change the generated machine code for the shipped fields: the hot-kernel instruction totals are identical before and after.

---

## 1. Step history (plan §8.1)

`git log --oneline --decorate step/00-import..HEAD` shows every tag, in order, on a linear history:

| Tag | Commit | Content (spot-checked with `git diff --stat` and kernel diffs) |
| :--- | :--- | :--- |
| `step/00-import` | `2badf67` | Upstream `conroi/lcpc` `f0e2116` with full history. It differs from `f0e2116` only in `README.md` and the new `README-lcpc.md`. |
| `step/01-modernize` | `b05b33f` | Dependency and edition update, stable toolchain, stable bench harness, upstream `#[bench]` removed. Also adds the M0 tooling (`check_codegen.py`, `run_tests.py`, `reproduce/{common,lcpc_orig,simd_spielman}.sh`, golden vectors), as planned. |
| `step/02-fields` | `6ab6bb8` | `lcpc-fields` (`LazyField`, `Mont127`, `Mers127`, `Fp2M61`), wired into the eager algorithm. |
| *(untagged)* | `8ea4f00` | Test-only oracles: original LCPC as a dev-dependency, glibc `random()` sampler, Alg. 1–4 oracle. |
| `step/03-column-major` | `eae56d1` | Batched column-major encoding. |
| `step/04-r2l` | `650763a` | Transposed graph and R2L gather; runtime code-parameter modes (a) and (b). |
| *(untagged)* | `c8f9317`, `43ae713` | README only (GCP VM configuration and cost rule). |
| `step/05-lazy` | `25c2248` | Lazy reduction only (`encode_column_block` switches to `LazyField::mac`/`fold`/`reduce`). Test fields gain `LazyField` impls. |
| `step/06-blocking` | `643d38b` | Multi-edge blocking only (`BLOCK_FACTOR = 4`, `encode.rs` + `docs/BLOCKING.md`). |
| `step/07-slicing` | `8567b53` | LLC slicing, cache detection (sysfs, then `sysconf`), auto-tuner. |
| *(untagged)* | `fdaf9d4` | Benchmark and reproduction harness; GCP scripts. |
| `step/08-threads` | `285c01c` | Persistent pinned worker pool for expander encoding. |
| *(untagged)* | `cdebb9d` | Review fixes (disjoint worker slices, loud failure on missing `tv/`, script hardening) and codegen listings. |
| `step/09-codegen` | `9f15bc2` | Portable codegen tuning (`carrying_add` chains, unrolled reduction, `chunks_exact` in kernels). |

`step/05-lazy` and `step/06-blocking` were redone as two commits from `ref/combined-lazy-blocking` (`6a61714`). The code in `ref/combined-lazy-blocking..step/06-blocking` differs only in docs, test refactoring and comments (11 changed lines in `encode.rs`), so the split did not change the combined algorithm.

## 2. Verification performed

| # | Check | Command | Result |
| :--- | :--- | :--- | :--- |
| V1 | Workspace tests at HEAD, clean target | `CARGO_TARGET_DIR=target-verify/ws taskset -c 0,1,3-7 cargo test --workspace --release --locked` | **PASS**: 102 passed, 0 failed, 6 ignored (5 `#[ignore]` bench tests, 1 doc test) |
| V2 | Test runner plus portability build without `target-cpu=native` | `taskset -c 0,1,3-7 python3 scripts/run_tests.py --release --portability --target-dir target-verify/rt --portability-target-dir target-verify/portability` | **PASS**: 204 passed (102 per configuration), 0 failed |
| V3 | Every step tag on its own | one temporary detached worktree per tag, `cargo test --workspace --release --locked` under `taskset -c 0,1,3-7`, own target dir, worktree removed afterwards; `SIMD_SPIELMAN_TV_DIR` set and the `../../SIMD-Spielman-for-ZKPs` path provided so historical tags find the vectors (§3, fix 3) | **PASS** for all nine: `01` 22, `02` 39, `03` 64, `04` 90, `05` 91, `06` 91, `07` 95, `08` 101, `09` 102 passed; 0 failed, 6 ignored each |
| V4 | Scalar hot kernels | `scripts/check_codegen.py <lcpc-bench> --strict` on the native and portability x86-64 binaries, and on `lcpc-bench` objects for `aarch64-unknown-linux-gnu` and `riscv64gc-unknown-linux-gnu` | **PASS** on all four: 16 hot kernels each, **0** vector-arithmetic instructions. In the x86-64 hot kernels, the only vector-register instructions are `vmovups`/`vmovaps` (58 + 13) and the zeroing idiom `vxorps` (23); this was checked independently with `objdump -d` and `awk`. `--self-test` passes, including the 17 new instruction-classification cases. |
| V5a | `reproduce/this_work.sh` | `--threads 1 --cores 3 --logN 16 --runs 3 --warmup 1 --test-only --out target-verify/smoke` | **PASS**: build, then 102 tests, then strict codegen check, then benchmarks; raw JSON, codegen report and `manifest.json` written |
| V5b | `reproduce/steps.sh` | `--threads 1 --cores 3 --logN 12 --runs 1 --warmup 0 --test-only` | **PASS** (29 min, pinned to one core). For each of the 9 tags, `steps.sh` creates a worktree, builds, runs the tag's tests (695 passed in total, 0 failed), and runs the codegen check: strict from `step/04` on, and PASS for every tag. It then benchmarks and writes a per-step `manifest.json` with `step_info` (tag, commit) and that tag's `Cargo.lock` hash. All worktrees were removed afterwards. |
| V5c | `reproduce/lcpc_orig.sh` | `--threads 1 --cores 3 --logN 16 --runs 3 --warmup 1 --test-only` | **PASS**: all four variants (as-published / fork flags × default / portable-only BLAKE3) verify the golden vectors before benchmarking; four raw JSON files and `manifest.json` written. Its build dir is the fixed `target-lcpc-orig/`, so cached builds were reused. |
| V5d | `reproduce/simd_spielman.sh` | `--threads 1 --cores 3 --logN 16 --runs 3 --test-only` | **PASS, after two fixes.** (1) Boost is not installed on this workstation, so the first attempt stopped at the prerequisite check, with `status: failed` in the manifest. For the smoke run I used header-only Boost.Multiprecision plus Boost.Config 1.86.0 under `target-verify/deps/` through `CPLUS_INCLUDE_PATH`. I also used a `CXX` wrapper that adds `-include atomic`, because upstream `utils.h` relies on full Boost to include `<atomic>`. (2) A script bug then appeared: the upstream tests could never run (§3, item 9). After the fix, the run builds the tests, passes 100 upstream test programs ("All tests successful!"), then runs 7 benchmarks (Brakedown/Orion × 3 fields plus the verifier) and writes `simd_spielman.json` (`status: complete`) and `manifest.json`. The wrapper and include path are recorded in the manifest (`cxx`, `cxx_include_env`). |
| V6 | Final gate after all fixes | `CARGO_TARGET_DIR=target-verify/final taskset -c 0,1,3-7 cargo test --workspace --release --locked` | **PASS**: 102 passed, 0 failed, 6 ignored |

### Rule audit

- **No SIMD, assembly or nightly features.** `git ls-files` sources contain no `std::arch`, `core::arch`, `asm!`, `global_asm!`, `core::simd`/`std::simd`, `#![feature`, `target_feature`, `is_x86_feature_detected` or `cfg(target_arch)`. The only matches are comments that describe the rule.
- **No optimization toggles.** No workspace crate has a `[features]` section. `external/lcpc-orig`'s `portable-only` feature selects the baseline's hash build, not a fork tier. The benchmark CLIs take only runtime parameters (`--field`, `--params`, `--logN`, `--threads`, `--cores`, `--slices`, `--autotune`, `--runs`, `--warmup`, `--output`). The only environment variable read by library or test code is `SIMD_SPIELMAN_TV_DIR`.
- **BLAKE3 is portable-only.** `cargo tree -e features -i blake3` shows one `blake3 v1.8.7` with exactly `std`, `pure`, `no_sse2`, `no_sse41`, `no_avx2` and `no_avx512`, including with the dev-dependencies of the whole workspace. Even so, `blake3`'s build script always compiles its Rust SSE2/SSE4.1 modules on x86-64; they show up as 316 and 307 vector instructions in the binary. Under `no_sse2`/`no_sse41`, `Platform::detect()` never selects them, so they are dead code. This is documented in `docs/CODEGEN.md` §4.4.
- **CPU 2.** The scripts (`run_tests.py`, `run_benchmarks.py`, `reproduce/common.sh` with `hosts/local-i7-11700k.sh`) reject CPU 2 on this host. The worker pool takes an explicit core list or the process affinity mask. SIMD-Spielman's thread pinning is redirected through `SIMD_SPIELMAN_CORES` by `params.patch`. Every command in this verification excluded CPU 2.
- **Silent skips.** At HEAD, missing SIMD-Spielman vectors fail the tests with an actionable message (`docs/REVIEW-M0-M6.md`, M2). The historical tags `step/02-fields` through `step/08-threads` still skip their `tv/` checks with only an `eprintln!`, which Cargo's output capture hides. `steps.sh` now supplies the vectors and fails if they are missing (fix 3). `simd_spielman.sh` fails loudly, and writes `status: failed` into its manifest, when a prerequisite such as Boost is missing.

### `docs/REVIEW-M0-M6.md` resolution

All items marked *Fixed* in the review are present at HEAD, and I confirmed them:

- `--locked` everywhere.
- `RUSTFLAGS` is rejected.
- Per-tag `Cargo.lock` hashes.
- Strict codegen gates in `this_work.sh` and `steps.sh`.
- Missing `tv/` fails the tests.
- Workers get disjoint `&mut` slices; regression test `disjoint_expander_and_reed_solomon_ranges_match_full_outputs` passes.

Residuals that remain open:

1. Pre-R2L step tags (`01`–`03`) have no named hot kernels, so their codegen reports are informational only.
2. GCP manifest fields have not been exercised on a GCP guest.
3. Failure of worker affinity is logged, not fatal, and that path is not exercised.
4. Constructing the Brakedown encoding requires `F: LazyField`, an API-compatibility limitation.
5. AArch64 and RV64 codegen was checked from objects only; nothing was executed on those targets.

## 3. Changes made during this verification (uncommitted)

1. **`scripts/check_codegen.py`: classifier bugs (correctness of the codegen evidence).**
   - On x86-64, every AT&T `movq`/`movd` counted as a *vector move*, even between general-purpose registers. On AArch64, every `ldr`/`str` did the same. This inflated the vector-move counts (x86-64 native 1,711, now 94) and suppressed stack-access detection, so x86-64 reported "0 spills".
   - Instructions on vector registers that were in neither the move list nor the arithmetic list (for example `vxorps`, `vinserti128`, AArch64 `ushr`) were classified as *scalar*. On RISC-V, unknown `v*` instructions counted as moves.
   - The classifier is now conservative. A move counts as a vector move only if it involves a vector register. The register-zeroing idiom counts as a move. Any other instruction on a vector register counts as vector arithmetic.
   - `--self-test` gained 17 classification cases.
   - The hot-kernel verdict is unchanged: 0 vector arithmetic on all four targets. The listings in `docs/codegen/` were regenerated, and `docs/CODEGEN.md` (table, §3.2, §4.2, §4.4, §5.2) and `docs/codegen/README.md` were corrected. The AArch64 JSON report now keeps only the hot-kernel symbols; the full report of the unlinked object is about 225 MB.
2. **Correction to the M6 claims.** `docs/CODEGEN.md` §5.2 stated that the `B = 4` inner loop runs without spills on x86-64. It does not. In `encode_column_block::<Mont127>`, 22 of the 113 inner-loop instructions access `(%rsp)`; `Mers127` has 20 and `Fp2M61` 34. These are reloads of prepared weights and source pointers, and spill/reload of a partial accumulator. The choice of `B = 4` rests on the local timings in `docs/BLOCKING.md`.
3. **`reproduce/steps.sh`: silent `tv/` skips at historical tags.** Its worktrees live under the target dir, where tags `02`–`08` could not find `SIMD-Spielman-for-ZKPs/tv` and passed while skipping those checks. The script now exports `SIMD_SPIELMAN_TV_DIR` (defaulting to the sibling upstream checkout), links `$TARGET_BASE/SIMD-Spielman-for-ZKPs` for the old `../../` lookup, and fails early if the vectors are absent.
4. **`lcpc-fields/tests/fields_test.rs`.** `fp2m61_simd_spielman_test_vectors_and_encoding` now honours `SIMD_SPIELMAN_TV_DIR`, as the oracle crate does, so it works outside the default layout (worktrees, standalone clones).
5. **`lcpc-brakedown-pc/src/encode.rs`: latent accumulator-bound bug.** After a fold, `since_fold = 1`, and the blocked `B`-edge step then performs `B` more MACs, so it needs `MAX_ACCUMULATIONS >= B + 1`. A `LazyField` with `MAX_ACCUMULATIONS` in 2..=4 would have overflowed its accumulator. The blocked loops are now taken only when `F::MAX_ACCUMULATIONS > B`, a compile-time constant; otherwise the one-edge loop runs. The shipped fields are unaffected (`Fp2M61` = 32, the others `usize::MAX`), and so is their codegen. No regression test was added, because it would need a test field whose accumulator detects overflow.
6. **`reproduce/common.sh`.**
   - Manifests now record `git.code_repo_dirty` and `git.code_repo_changed_paths`. Before, a run from a modified tree recorded only the HEAD SHA.
   - The usage text showed stale defaults (30 runs / 5 warm-ups); the actual defaults are 5 and 2.
7. **`reproduce/simd_spielman.sh`.** The manifest records `CPATH`/`CPLUS_INCLUDE_PATH` when they are set (`cxx_include_env`).
8. **`scripts/run_tests.py`.** New `--portability-target-dir` option; the portability build no longer always reuses `target-portability/`.
9. **`reproduce/simd_spielman.sh`: the upstream tests never ran.** The script built the tests in `$RUN_ROOT/build-tests`, but upstream `run_tests.py` runs the programs from `./build/` in the source tree, and they read `../tv/`. Every run therefore failed with `FileNotFoundError: './build/'` right after the test build. So the M0 criterion "baseline script runs end-to-end" was not met before this fix. The test build now lives in `$PATCHED_SOURCE/build`.
10. **`reproduce/steps.sh`: per-step manifests.** With fix 6, the manifest would have attributed the main checkout's dirty state to the benchmarked tag. Step manifests now record the tag worktree's own state (`code_repo_dirty`, always clean for a detached tag checkout) and, separately, the state of the checkout that holds the harness scripts (`harness_repo_dirty`, `harness_repo_changed_paths`). This change came after the V5b smoke run. I tested it by running the manifest-patching code on its own, against a V5b manifest; `sh -n` passes.

## 4. Milestones vs. exit criteria (plan §8.2)

| # | Exit criterion | Status | Evidence |
| :--- | :--- | :--- | :--- |
| **M0** | Workspace builds and tests pass on stable Rust | **Met** | V1, V2, V6 (stable 1.99.1, no `#![feature]`; the upstream oracle's `#![feature(test)]` is bootstrapped for that crate only by `tools/rustc-oracle-wrapper.sh`) |
| | `step/01-modernize` bit-exact with `f0e2116` | **Met** | `tests::golden_vectors_bit_exact_with_lcpc_orig` passes at `step/01-modernize` (V3). At HEAD, `lcpc-oracle-tests/tests/original_lcpc.rs` (3 tests) compares against the pinned upstream crates directly. |
| | Both baseline scripts run end-to-end locally on `0,1,3-7` and write manifests | **Met after fixes**: `lcpc_orig.sh` passes unchanged. `simd_spielman.sh` only passes with the §3 item 9 fix, and locally only with the Boost workaround (V5d). | V5c, V5d |
| **M1** | Field tests vs. big-int oracle; `Mont127` ≡ `Ft127` representation and `F::random`; scalar field kernels | **Met** | `lcpc-fields/tests/fields_test.rs` (14 tests, including `mont127_differential_vs_ft127_plan02_sec4_4`, `*_differential_vs_bigint`, accumulator-boundary tests); `test_mont127_vs_ft127_matgen_encode_and_pcs_bit_exact`; V4 (`mac_column_*`, `reduce_column_*` scalar). |
| **M2** | Fork bit-exact with `lcpc-orig` in mode (a); Alg. 1–4 oracle plus glibc sampler reproduce all `tv/` files; fork encoder matches `tv/` from `step/04` on | **Met** | `fork_fields_match_original_lcpc_mode_a`, `fork_runtime_params_match_original_lcpc_mode_a`, `fork_r2l_graphs_are_transposed_original_graphs`. Oracle `tv_*` tests cover 3 fields × N ∈ {2^12, 2^14, 2^16} × {Brakedown, Orion}, and `fork_tv_*` covers the same 18 cases. All pass at HEAD and at tags `04`–`09` (V3). |
| **M3** | Each of `step/03`–`06` passes all oracle, `tv/` and codegen checks; local sanity timings show progression | **Met, with a caveat** | V3 for tests. The strict codegen check applies from `step/04` (V5b runs it per tag). Direction-only local timings are in `docs/BLOCKING.md` and `docs/LAZY.md`. The tiny smoke in V5b (logN 12, 1 run) is not a timing comparison. |
| **M4** | Slice invariance for s ∈ {1, 2, 4, 8, 16}; sysfs/`sysconf` detection; tuner tested | **Met** | `test_slice_invariance_all_fields_and_modes` (s ∈ {adaptive, 2, 4, 8, 16} vs. s = 1, all fields and modes), `test_cache_detection_sanity`, `test_autotune_slices_empirical` |
| **M5** | Thread invariance across T; PCS stages outside encoding do not regress | **Partly met** | `test_thread_invariance_all_fields_and_modes` (T ∈ {1, 2, 4, 7, 8} × s ∈ {1, 4}), `test_threading_flakiness_50x_loop`, `test_pcs_commit_with_threads`. "No regression" of hashing and Merkle stages is asserted in `docs/THREADS.md` but has no recorded before/after measurement. It must be shown with the GCP T = 1 / T = 8 `hash_matrix_decomposition` and commit breakdown. |
| **M6** | Disassembly inspected via `check_codegen.py` on all three targets; avoidable spills/branches removed; listings saved | **Mostly met** | V4 and the regenerated `docs/codegen/*.listing.txt`. AArch64 and RV64 were checked from cross-compiled objects only (no cross linker or emulator). Spills remain in the x86-64 `B = 4` inner loop (§3, item 2); whether `B = 2` or another formulation would avoid them was not measured. The verifier small-batch specialization listed under `step/09+` in §8.1 (01 §3.8) is not implemented. |
| **M8 harness** | Commit/Prove/Verify benchmarks with separate encoding vs. hashing; original LCPC with default and portable-only BLAKE3 | **Harness ready; measurements pending** | `lcpc-bench` and `bench_pcs` record `encode_matrix`, `hash_matrix_decomposition`, `commit_total`, `prove`, `verify` (V5a JSON). `lcpc_orig.sh` builds and benchmarks four variants: as-published and fork flags, each with default and portable-only BLAKE3 (V5c). Measurements still have to be made on GCP. |

## 5. Deviations from the plan

- `step/01-modernize` also carries the M0 tooling and golden vectors, as the plan allows ("add CI/test scripts"). Test-only oracle commits and harness/docs commits sit untagged between step tags, so `steps.sh` measures the tags, not every commit.
- `step/09-codegen` holds only the codegen tuning. The verifier small-batch specialization is still to do; end-to-end PCS integration with portable BLAKE3 was already in place earlier.
- The fork's Brakedown encoding requires `F: LazyField` rather than upstream's `Field + Num + SizedField` (API note in `docs/REVIEW-M0-M6.md`).
- The `step/02`–`step/08` tags silently skip `tv/` checks when the vectors are missing. The tags are immutable history; `steps.sh` compensates (§3, item 3).
- Local SIMD-Spielman runs need Boost: the GCP setup installs `libboost-all-dev`. The local workaround (V5d) is for smoke tests only and is recorded in the manifest.

## 6. What remains

- **M7, GCP campaign.** Needs the user's explicit go-ahead because the VM costs money (D5). The configuration is decided: `c3d-highmem-30`, `--threads-per-core=1`, Debian 12, created with `reproduce/gcp/create_vm.sh`. Run `this_work.sh`, `steps.sh`, `lcpc_orig.sh` and `simd_spielman.sh` at T = 1 and T = 8, keep manifests and codegen reports in `code/results/`, then stop or delete the VM.
  - The VM needs `libboost-all-dev`, which `reproduce/gcp/setup_vm.sh` installs.
  - The scripts write manifests that record whether the tree was dirty; publish only from a clean, committed tree.
- **M8 measurements.** End-to-end Commit/Prove/Verify on GCP, including original LCPC with default and portable-only BLAKE3, with encoding and hashing reported separately. This also supplies the missing M5 "no regression" evidence.
- **M9.** `scripts/analyze.py` must regenerate every table and figure (T0–T12, F1–F3 of plan 06) from `code/results/`; this is unverified without results.
- **Open decisions.**
  - **D5:** campaign length, VM budget, and repetitions for `step/NN` vs. HEAD; also the go-ahead itself.
  - **D6:** recorded in the plan as *decided: include PolyCommit*, but there is no PolyCommit reproduction script yet, so implementation and scope are still open.
  - **D9:** whether to publish the fork's crates or open an upstream PR.
  - Plan §10 still lists D7 (`LazyField` signature) and D8 (B = 4) as open. In practice both are settled by the code and `docs/BLOCKING.md`; the plan text should be updated, with the §3 item 2 caveat for D8.
- **Smaller follow-ups.**
  - Exercise the GCP manifest fields on a real guest.
  - Consider making worker-affinity failure fatal in benchmark runs.
  - Optionally make `lcpc_orig.sh` and `simd_spielman.sh` honour a caller-supplied build directory; they currently use the fixed `target-lcpc-orig/` and `target-simd-spielman/`.
