<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Project Status: Milestones M0–M8

> **Update 2026-10-23.** §1–§5 record the local verification of M0–M6 on 2026-10-21 and are unchanged. The GCP campaign (M7) and the M8 measurements have since run; §6 and the new §7 record what was measured, what is invalid, and what must be re-measured.

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
| **M8 harness** | Commit/Prove/Verify benchmarks with separate encoding vs. hashing; original LCPC with default and portable-only BLAKE3 | **Measured, partly invalid** (§7) | `lcpc-bench` and `bench_pcs` record `encode_matrix`, `hash_matrix_decomposition`, `commit_total`, `prove`, `verify` (V5a JSON). The campaign measured original LCPC as `as-published-as-is` and `fork-flags-portable` only. The `as-published-as-is` builds picked up the fork's `.cargo/config.toml` and must be re-measured (§7.4, I4). |

## 5. Deviations from the plan

- `step/01-modernize` also carries the M0 tooling and golden vectors, as the plan allows ("add CI/test scripts"). Test-only oracle commits and harness/docs commits sit untagged between step tags, so `steps.sh` measures the tags, not every commit.
- `step/09-codegen` holds only the codegen tuning. The verifier small-batch specialization is still to do; end-to-end PCS integration with portable BLAKE3 was already in place earlier.
- The fork's Brakedown encoding requires `F: LazyField` rather than upstream's `Field + Num + SizedField` (API note in `docs/REVIEW-M0-M6.md`).
- The `step/02`–`step/08` tags silently skip `tv/` checks when the vectors are missing. The tags are immutable history; `steps.sh` compensates (§3, item 3).
- Local SIMD-Spielman runs need Boost: the GCP setup installs `libboost-all-dev`. The local workaround (V5d) is for smoke tests only and is recorded in the manifest.

## 6. What remains

- **M7, GCP campaign.** Run on 2026-10-22/23 with the user's go-ahead, on `c3d-highmem-16` (not the planned `c3d-highmem-30`) and `c4a-highmem-16`, using a staged method. §7 is the record. Still open:
  - The re-measurement stage of §7.6 (`reproduce/gcp/run_stages.sh --preset remeasure`): original LCPC `as-published-as-is` everywhere (I4), the two other LCPC variants at 2^20/2^24 (E8), thread scaling at 2^20–2^28, s1 SIMD-Spielman Mont127 T = 1 at 2^28 (I5), and the Arm This Work blocks after the AArch64 codegen-checker fix (I7).
  - Optional: a SIMD-Spielman slice sweep at 2^24/26/28 before any 2^28 SIMD claim (§7.3).
  - Delete the four VMs after the user confirms (§7.1).
- **M8 measurements.** Measured in the campaign (T6 inputs, stages A and B). The original-LCPC "as published" PCS numbers are invalid until re-measured (I4). The M5 "no regression" evidence comes from the T = 1 / T = 8 `hash_matrix_decomposition` and commit breakdown of This Work.
- **M9.** `scripts/analyze.py` must regenerate every table and figure (T0–T12, F1–F3 of plan 06) from `code/results/`; this is unverified without results.
- **Open decisions.**
  - **D5:** *decided and executed.* The user gave the go-ahead; the campaign used 5 timed runs and 1 warm-up for every configuration (steps included) and was split into stages of about 1–3 h (§7.2).
  - **D6:** *done.* `reproduce/polycommit.sh` and `external/polycommit/` (patch for explicit square dimensions and `sec_param = 80`, commit `9385ccc`) measure arkworks PolyCommit's Brakedown encoding at T = 1 and T = 8, log2 N 16–28; the PCS stages are not measured for PolyCommit.
  - **D9:** whether to publish the fork's crates or open an upstream PR.
  - Plan §10 still lists D7 (`LazyField` signature) and D8 (B = 4) as open. In practice both are settled by the code and `docs/BLOCKING.md`; the plan text should be updated, with the §3 item 2 caveat for D8.
- **Smaller follow-ups.**
  - GCP manifest fields were exercised on the guests. First-boot records and host profiles are now written per instance (§7.4, I9); the campaign's files predate that fix.
  - Consider making worker-affinity failure fatal in benchmark runs.
  - Optionally make `lcpc_orig.sh` and `simd_spielman.sh` honour a caller-supplied build directory; they currently use the fixed `target-lcpc-orig/` and `target-simd-spielman/`.

## 7. M7 campaign record (2026-10-22/23)

This section records the campaign **as it was actually run**. It overrides the plan (`agent/plan/05`, `06`) wherever they differ; plan 06 §9 lists the same deviations. Which run directories and blocks are valid is machine-readable in `results/selection.json`; `scripts/analyze.py --selection results/selection.json` uses only those. No number in this section is a result.

### 7.1 Machines

| Instance | Machine type | Zone | Role | CPU (guest `lscpu`) |
| :--- | :--- | :--- | :--- | :--- |
| `rbd-bench-c3d` | `c3d-highmem-16` | `us-central1-c` | Created and set up (first-boot checks passed). Ran the early monolithic s1 attempts (code `2b23858`, `454cb24`, `80ae955`), whose data was later invalidated or left incomplete (I1, I10, I12); used for no valid data. After repeated stockouts in `us-central1-c`, s1 moved to `rbd-bench-c3d-3`. | AMD EPYC 9B14 |
| `rbd-bench-c3d-3` | `c3d-highmem-16` | `us-east4-c` | Session **s1** | AMD EPYC 9B14, 8 cores, 1 thread/core, one 32 MB L3 domain (CPUs 0–7) |
| `rbd-bench-c3d-2` | `c3d-highmem-16` | `us-east1-d` | Session **s2** | AMD EPYC 9B14 (same topology) |
| `rbd-bench-c4a` | `c4a-highmem-16` | `us-central1-a` | Arm session **arm1** | Neoverse-V2 (Google Axion) |

All x86-64 VMs were created with the same flags (`--threads-per-core=1`, Debian 12, 100 GB `pd-balanced`, on-demand, `--maintenance-policy=TERMINATE`); `code/README.md` ("Benchmark machines") has the commands. s1 and s2 ran **concurrently** on two VMs in different zones (the project's 32-vCPU `CPUS_ALL_REGIONS` quota allows two 16-vCPU VMs), not on separate days on one VM; the Arm VM ran while s2's stage R was still running. Every VM was stopped when its stages finished. The four VMs still exist (disks are billed) and must be deleted once the user confirms.

### 7.2 Method: staged sessions

The monolithic `all.sh --profile full` session (~15 h per VM) was replaced on 2026-10-22 by stages of about 1–3 h, each a `reproduce/gcp/run_session.sh --keep-running` call with a block regex, a log2 N list, `--warmup 1` and `--lcpc-variants as-published-as-is,fork-flags-portable`. The driver used during the campaign lived outside the repository; its in-repo equivalent is `reproduce/gcp/run_stages.sh`. Each stage synced the fork's current HEAD to the VM, so stages ran at different commits and wrote different run directories (`results/<host>/<UTC-date>-<sha>/<session>/`):

| Stage | Blocks (regex) | log2 N | s1 run dir | s2 run dir |
| :--- | :--- | :--- | :--- | :--- |
| A | `^full_(this_work_mode\|lcpc_orig_mode\|polycommit\|ligero)` | 16–26 (Ligero 20, 22, 24) | `2026-10-22-2557b5517338` | `2026-10-22-2557b5517338` |
| S | `^full_simd_spielman` | 16–26 | `2026-10-22-2557b5517338` | `2026-10-22-18d75f808f59` |
| C | `^full_(slice_sweep\|thread_scaling)` (block ids of that time; now `^full_(slice_sweep\|this_work_threads\|lcpc_orig_threads)`) | slice sweep 20, 24, 26; thread scaling 20, 24 (see §7.3) | `2026-10-22-18d75f808f59` | `2026-10-22-18d75f808f59` |
| D | `^full_steps` | 16, 20, 24 | `2026-10-22-8dec85493004` | `2026-10-22-8dec85493004` |
| B | `^full_(this_work_mode\|lcpc_orig_mode\|polycommit\|simd_spielman_mont127)` | 28 | `2026-10-22-8dec85493004` | `2026-10-22-8dec85493004` |
| R | `^full_(lcpc_orig_mode_a\|simd_spielman_.*_t[78])` | 16–26 | `2026-10-23-8dec85493004` | `2026-10-23-acfac2349c11` (running at the time of writing) |
| ARM | `^arm_` (profile `arm`) | 18, 22, 26 | — | — (arm1: `results/gcp-c4a-highmem-16/2026-10-23-acfac2349c11`) |

Commits `2557b55`, `18d75f8`, `8dec854` and `acfac23` differ from each other only in harness scripts, the original-LCPC driver's row loop (`18d75f8`), the SIMD-Spielman patch (`dd9009a`), and `analyze.py`; This Work's library and benchmark code (`lcpc-*`, `bench/`) are identical in all four. Block order is randomized only within each stage. Stage R re-measured, on a later boot, the cells invalidated by I2 and I3 below, so original LCPC ≤ 2^26 and SIMD-Spielman T7/T8 ≤ 2^26 come from a different boot than This Work ≤ 2^26 (stage A). Generated tables must name the stage/run dir of every column, and the paper must list this cross-boot comparison as a threat to validity.

Every configuration used **5 timed runs and 1 warm-up** (the plan said 2 warm-ups below 2^26). The session `manifest.json` files show the script defaults (warm-up 2, log2 N 16–26), not the stage arguments; the per-record `warmup` field and the `schedule.json` commands are authoritative. Each stage overwrote the session's `schedule.json`, so earlier stages' seeds survive only in the records' `session_seed` fields.

**Layout since the audit fixes.** `all.sh` now keeps each stage's `schedule.json`, `blocks.tsv`, `manifest.json` and execution log under `<session>/stages/<UTC time>_<stage>/`, and maintains an append-only session-level `<session>/schedule.json` whose `stages` list accumulates every stage (tag, `--blocks` filter, logN list, warm-up, LCPC variants, seed, block ids, exit status). Before a block runs, an existing output directory of the same block is moved to `<block>.prev-<UTC time>` inside the session, so a re-run never mixes stages; `results/selection.json` excludes `*.prev-*`. `code/README.md` ("Session directory layout") describes the layout. The campaign's run directories predate it.

### 7.3 Scope actually measured, and deviations from plan 06

- **Original LCPC variants:** only `as-published-as-is` and `fork-flags-portable` (plan: all four). The `as-published-as-is` data is invalid (I4).
- **SIMD-Spielman:** one process per case with in-process warm-up and per-encoding samples (`4cace86`; `external/simd_spielman/README.md`). Thread counts 1, 7 and 8; **T7** (7 workers + the coordinator pinned to the 8th core, `dd9009a`) is the 8-core comparison point, T8 has 9 threads on 8 cores. Slice counts come from the upstream EPYC 9754 table (`slice_mode: table`, e.g. 8 slices at 2^28); **no SIMD-Spielman slice sweep and no auto-tuned This Work run** were made (plan E2/E4 asked for "tuned"). Any 2^28 SIMD comparison must be labelled "upstream table, untuned on this host".
- **2^28:** This Work (modes a and b, all fields), original LCPC, PolyCommit at T = 1 and 8; SIMD-Spielman **Mont127 only** (T1, T7, T8, Brakedown shape). **Orion-shape SIMD-Spielman at 2^28 is omitted**: upstream's Orion graph generation took more than 90 min per process. In the campaign these cases were stopped by an external watcher and recorded `status: failed`; the harness now skips them explicitly (`simd_spielman.sh --max-orion-logn 26`, set by `all.sh --profile full`), and analysis reports them as "omitted (documented)".
- **Slice sweep (E5):** s ∈ {1, 2, …, 256}, T ∈ {1, 8}, Mont127, log2 N ∈ {20, 24, 26} (plan: 20–28).
- **Thread scaling (E6):** T ∈ {1, 2, 4, 8} at log2 N ∈ {20, 24} only. The blocks are named `..._n20-24-26`, but the block's own log2 N list (20, 24, 28) intersected with stage C's (20, 24, 26) left 20 and 24. No T = 15 (8-core VM). Thread scaling at 2^20–2^28 is part of the re-measurement (§7.6). For T = 2 and 4, original LCPC's Rayon threads floated over all 8 pinned cores while This Work pinned T workers to the first T cores; `all.sh` now gives every implementation the first T cores.
- **Steps (E3):** log2 N ∈ {16, 20, 24} (plan: up to 28). `full_steps_mode_a` and `full_steps_mode_b` ran the identical command and each measured both modes, so every configuration exists twice per session; analysis uses one. `step/01-modernize` measures `Ft127` (later steps `Mont127`), and its codegen check matched no hot kernels.
- **Arm (E14):** original LCPC **`fork-flags-portable` only**: This Work has no Arm data (I7), and the Arm `as-published-as-is` records are excluded like the x86-64 ones (I4). T = 1 and 8, log2 N ∈ {18, 22, 26}. The Arm build is generic `aarch64` (no `target-cpu=native`), unlike x86-64.
- **Frequency:** `cpu_mhz_*` is the nominal 2600 MHz from `/proc/cpuinfo` on the `c3d` guests (no cpufreq, no PMU), so `cycles_per_mac` is nominal, not measured.
- **SIMD-Spielman setup time and peak RSS** were not recorded in the campaign (T8 has no SIMD-Spielman columns). `simd_spielman.sh` now records `peak_rss_bytes` and an approximate `setup_ns` (process wall time minus the encode samples, from `/usr/bin/time -v`, 10 ms resolution) for new runs.

### 7.4 Incidents that invalidated data, and fixes

| # | Incident | Data affected | Fix | Status of the data |
| :--- | :--- | :--- | :--- | :--- |
| I1 | **Core pinning bug.** `all.sh` passed the single T = 1 core to every block, so all T > 1 runs were oversubscribed on one core. | The monolithic sessions on code `454cb24` (s1, `rbd-bench-c3d`), `d0726a8` (s2, failed at the PolyCommit block) and `f0a2825` (s2, stopped at block 11 of 41 when the bug was found). | `80ae955`: per-block pinning by thread count; `analyze.py --check` rejects threads > pinned cores. | Invalid, excluded. Their session data is not kept in `results/`; the run dirs `2026-10-22-{454cb24d94f9,d0726a86d61b,f0a2825452f6}` in the repository hold only the `probe/` core-probe dry run (`schedule.json`, no measurements). |
| I2 | **Serial original-LCPC driver.** `encode_matrix` looped over rows serially, while upstream `commit` uses `par_chunks_mut`; T8 equalled T1. | `full_lcpc_orig_mode_a_t{1,8}_n16-18-20-22-24-26` in `2026-10-22-2557b5517338` (s1, s2). | `18d75f8`: parallel row loop like upstream (`external/lcpc-orig/README.md`). | Superseded; re-measured in stage R. |
| I3 | **SIMD-Spielman coordinator oversubscription.** Upstream's MT main thread spin-waits; unpinned, it competed with 8 pinned workers on 8 cores. | `full_simd_spielman_*_t8_n16-18-20-22-24-26` in `2026-10-22-2557b5517338/s1` and `2026-10-22-18d75f808f59/s2`. | `dd9009a`: coordinator pinned after the workers; T7 blocks added. | Superseded; re-measured in stage R (T7, T8). |
| I4 | **"As-published" LCPC built with the fork's flags.** Cargo read `code/.cargo/config.toml` from the working directory, so the as-published builds used `target-cpu=native`, `no-vectorize-loops`, `no-vectorize-slp` (Arm: the two no-vectorize flags), while their manifests claimed Cargo defaults. | Every `as-published-*` record in every valid run dir, x86-64 and Arm. | `lcpc_orig.sh` builds the as-published variants outside `code/` and records `build_variant` and `effective_rustflags`. | Excluded ("as-published built with fork .cargo/config.toml"); **re-measurement on GCP required**. `fork-flags-portable` is valid. |
| I5 | **Orion 2^28 watcher.** An external process killer for the omitted Orion 2^28 cases also matched the `g++` build of those targets. | s1 `full_simd_spielman_mont127_t1_n28`: build terminated, no data. | Explicit `--max-orion-logn` in the harness instead of a killer. | **Re-run required** (s2 has data). |
| I6 | **Failed blocks mis-tagged.** `tag_json.py` stamped the JSON of failed 2^28 SIMD blocks with the next block's `block_id`. | `full_simd_spielman_mont127_t{1,7,8}_n28` records (valid Brakedown samples). | Analysis takes the block from the directory path. `all.sh` now tags only the block's own output directory, re-tags its records on every run, and moves an earlier output directory of the block aside (`*.prev-*`) before a re-run. | Valid after correction. |
| I7 | **Arm codegen false positive.** `check_codegen.py --strict` read AArch64 branch-target addresses in GNU `objdump` output (e.g. `b3334`) as `b` registers and failed with 56 "vector" instructions, all scalar branches (`docs/CODEGEN.md` §4.5). | `arm_this_work_t{1,8}_n18-22-26`: no timing data. | AArch64 classifier fix: branches (`b.cond`, `cbz`, `cbnz`, `tbz`, `tbnz`, `b`, `bl`, `ret`, …) are scalar; zeroing/copy data movement is counted as `vector_moves` and does not fail; only vector arithmetic fails (`docs/CODEGEN.md` §4). | **Re-run required** (§7.6). |
| I8 | **Stale session manifest.** Before `8dec854`, VM syncs shipped local `results/`, so s2's `2026-10-22-18d75f808f59/manifest.json` was a copy from s1. | Session metadata only. | Restored from the s2 VM; `8dec854` excludes `results/` from the sync. | Fixed (record JSONs identical). |
| I9 | **First-boot record overwritten.** `setup_vm.sh` wrote `results/gcp-c3d-highmem-16/first-boot-manifest.json` per machine family, so s2's VM record was overwritten by s1's (`rbd-bench-c3d-3`). | s2 first-boot `lscpu`/IFMA/L3 record. | `setup_vm.sh` and the host profiles now write `first-boot-manifest-<instance>.json` and `host-profile-<instance>.json`; `all.sh` copies them into each stage directory. | s2's machine type, zone, CPU model and cache topology survive in its session and block manifests. |
| I10 | **Monolithic sessions stopped for the staged method.** After the pinning fix, s1 and s2 were restarted as full monolithic sessions (about 15 h each) on `80ae955`; they were killed when the campaign switched to short stages ("small steps", §7.2). | Run dir `2026-10-22-80ae955f2878` (four of s2's blocks were copied back; s1's output was not). | Staged sessions (`56e3fde`: `all.sh --blocks/--logn/--warmup/--lcpc-variants`, `run_session.sh --keep-running`). | Incomplete, excluded. No measurement bug is known in it; it is simply not a complete session. |
| I11 | **First staged stage A too slow.** On `56e3fde`, SIMD-Spielman still generated its expander graph once per timed sample (one process per sample, about 10 min per process at 2^26) and ran the upstream test suite (about 20 min) in every block, so stage A was still on its first block after 1.5 h. Both stage-A runs were killed (an earlier launch of the same stage had already ended when its controlling session was closed). | Run dir `2026-10-22-56e3fde099f6` (s1, s2). | `4cace86`: one process per case with in-process warm-up and per-encoding samples; `2557b55`: upstream tests once per host/commit/patch/compiler. Stage A re-ran on `2557b55`. | Incomplete, excluded. |
| I12 | **Earlier failed attempts.** Code `6262d22` (first session launch and core probe after VM setup; the host profile was not passed to `all.sh`, so the schedule named cores 1–8; fixed in `6017495`) and `2b23858` (s1: the VM lacked SIMD-Spielman's `tv/` test vectors, so the correctness tests failed as designed; fixed in `454cb24`). | No measurements (schedules and at most one aborted block). | See commits. | Not kept in `results/`. |

Other failures that produced no data: the first staged launch (local `/tmp` quota), s1's first stages D and B (sync archive changed under the copy-back; fixed in `8dec854`), and several stockout retries.

### 7.5 Patch and baseline versions

- SIMD-Spielman `7b399db` + `params.patch` `12f29de2e552…` (T1 ≤ 2^26) and `d9cf97f0347f…` (T7/T8 ≤ 2^26, all 2^28); `external/simd_spielman/README.md` describes both.
- PolyCommit arkworks `1eaec50` + `polycommit.patch` (`9385ccc`), `sec_param = 80`.
- Original LCPC `f0e2116` through `external/lcpc-orig` (parallel row loop since `18d75f8`).
- Rust 1.99.1 on every VM; x86-64 with `target-cpu=native`, Arm generic.

### 7.6 Planned re-measurement

Not yet run. `reproduce/gcp/run_stages.sh --preset remeasure` runs it on both x86-64 sessions (s1 on `rbd-bench-c3d-3`, s2 on `rbd-bench-c3d-2`) and on the Arm VM (`--machine c4a`, session `arm1`), with at most two VMs running at a time (32-vCPU `CPUS_ALL_REGIONS` quota). The stages write new run directories in the stage layout of §7.2 (`code/README.md`, "Planned re-measurement", lists the stage arguments):

- **RL, RL28 — original LCPC as published** (`as-published-as-is`, Cargo defaults, built outside `code/`; I4), T = 1 and 8, 2^16–2^28, together with `fork-flags-portable` on the same boot.
- **RH — E8 extra variants:** `as-published-portable` and `fork-flags-as-is` at 2^20 and 2^24, so that T6 can separate the BLAKE3 backend from the compiler flags.
- **RT, RT28A, RT28B — thread scaling (E6):** This Work and original LCPC, T ∈ {1, 2, 4, 8}, at 2^20, 2^24, 2^26 and 2^28, with every implementation on the first T cores.
- **RS — SIMD-Spielman Mont127 T = 1 at 2^28** (I5; Brakedown shape), on both sessions.
- **ARM — This Work on Arm** (T = 1 and 8, 2^18/2^22/2^26) after the AArch64 codegen-checker fix (I7), plus original LCPC `as-published-as-is` and `fork-flags-portable` on Arm.

Until these cells exist, the tables mark them as pending through `results/selection.json` omissions.
