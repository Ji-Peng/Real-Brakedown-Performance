# Adversarial Review: Milestones M0–M6

Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0.

Review date: 2026-10-21  
Scope: `git diff step/00-import..HEAD`, field arithmetic, encoder memory safety, thread-pool synchronization, test oracles, portable-code rules, API compatibility, and reproduction scripts. No benchmark campaign or GCP VM was started.

## Findings

### M0 — Build and reproduction harness

**Fixed — reproduction could accept unpinned or changed builds.** The workspace build and test commands in `this_work.sh` and `steps.sh` now use `--locked`. `common.sh` rejects `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS`, and `run_tests.py` refuses to run if `taskset` is unavailable. The campaign defaults now match the plan: two warm-ups and five timed runs.

**Fixed — per-step manifests could identify the wrong source.** `steps.sh` now records each tag's commit and hashes the `Cargo.lock` in that tag's worktree rather than the HEAD lockfile. The common manifest now records `lscpu`, deduplicated cache instances across CPUs, GCP machine type and zone when available, effective Rust flags and their source, the lockfile's BLAKE3 version, and BLAKE3 declarations from workspace manifests.

**Fixed — campaign scripts could report incomplete work as successful.** `this_work.sh` now treats a failed or empty strict codegen check as a benchmark blocker. `steps.sh` runs the checker before each step benchmark and requires a matched hot kernel from `step/04-r2l` onward. `all.sh` now exits when a requested SIMD-Spielman run or analysis generation fails.

**Residual.** `step/01-modernize` through `step/03-column-major` have no named R2L hot kernels for the strict checker, so their codegen reports are informational and do not establish scalar hot-kernel codegen. GCP metadata fields were not exercised on a GCP guest; they are null on the local workstation.

### M1 — Field arithmetic

**No arithmetic defect confirmed.** The implementation uses stable integer arithmetic and has no architecture-specific field path. The existing tests compare all three fields against big-integer oracles, check `Mont127` against upstream `Ft127`, and construct accumulator boundary values. The Montgomery boundary test checks a `2^64`-product bound and a value immediately below `q·2^192`; the Mersenne test checks a `2^66`-product bound and a full 320-bit accumulator; the extension-field test covers 32 maximal products and folding across a second 32-edge batch.

### M2 — Correctness oracles

**Fixed — missing SIMD-Spielman vectors could pass as successful tests.** The field test, oracle unit tests, and fork integration tests now fail with an actionable message if their required `tv/` files are missing. The 18 fork integration cases, the original-LCPC differential cases, and field oracles all ran against the available fixtures in this workspace.

### M3 — Single-thread encoding

**No defect confirmed.** The Alg. 1–4 oracle, original LCPC graph/encoding/PCS comparisons, and SIMD-Spielman vectors exercise the direct and batched encoder paths. Edge blocking and algebraic behavior passed the workspace tests.

### M4 — Slicing

**No defect confirmed.** Slice-invariance and the empirical tuner tests passed for the available fields and code modes.

### M5 — Multithreading and unsafe code

**Fixed — workers created overlapping mutable references.** Both `RowMajorSliceJob` and `ColMajorSlicedJob` previously created a full-length `&mut` slice over shared output buffers in every worker. This was undefined behavior even though the kernel subsequently wrote disjoint graph-node ranges. The workers now create slices limited to their assigned output nodes or Reed–Solomon columns, including the temporary Reed–Solomon buffer. The new `disjoint_expander_and_reed_solomon_ranges_match_full_outputs` regression test checks split and empty shards against the full-output kernels. Existing thread-invariance and repeated-pool tests exercise these shards concurrently.

**No additional race or pointer-lifetime defect confirmed.** The pool serializes `execute` calls through its completion mutex; the job pointer remains borrowed until all workers have passed the completion barrier. Worker partitions are separated by barriers between phases.

**Residual.** Worker affinity failures are logged and execution continues unpinned, as the API currently documents. On the benchmark host, process-level `taskset` still limits the workers to the configured CPU set, but an affinity failure could invalidate the intended one-worker-per-core layout. The local pinning tests passed; failure behavior was not exercised.

### M6 — Portable scalar code generation

**No source rule violation found.** A source scan found no `std::arch`, `core::arch`, `asm!`, portable-SIMD use, or `#![feature]`. The fork's BLAKE3 declarations select `pure` and disable SSE2, SSE4.1, AVX2, and AVX-512; `ppv-lite86` uses `no_simd`. The only `portable-only` Cargo feature is in the external original-LCPC driver and selects the baseline hash build, not a fork optimization tier.

**Verified on local x86-64.** `scripts/check_codegen.py target-review/release/lcpc-bench --strict` passed with 16 hot kernels checked and zero vector-arithmetic violations. The full binary's report and listing were written under `/tmp` for review.

**Not verified.** No AArch64 or RV64 cross-build/disassembly was run in this review. The local x86-64 result does not establish M6 portability codegen on those targets.

## API compatibility

The upstream `SdigEncodingS::{new,new_ml,new_from_dims}` constructor names remain available. `LcEncoding::encode_rows` is a default method, so existing implementations do not need to add an implementation. The original LCPC differential tests passed.

There is one generic-bound compatibility limitation: constructing the fork's Brakedown encoding now requires `F: LazyField`, which adds accumulator and thread-safety requirements beyond upstream's `Field + Num + SizedField` bound. This is required by the fork's lazy encoder; custom downstream fields must implement `LazyField`. This review did not add an eager compatibility path.

## Rule and script audit

- Local build, codegen, and test commands in this review used `taskset -c 0,1,3-7`; CPU 2 was excluded.
- The local host profile rejects CPU 2, and no script pins workers to CPU 2 by default.
- Original LCPC's pinned commit and golden vectors are checked by the driver; the SIMD-Spielman script verifies its pinned source and patch hashes before testing.
- Reproduction scripts were reviewed statically but not run end-to-end. No published timings or GCP results were produced.
- An unrelated working-tree edit to `scripts/check_codegen.py` was present during the review and was left untouched.

## Verification

- `taskset -c 0,1,3-7 cargo build --workspace --release --locked` — passed.
- `taskset -c 0,1,3-7 python3 scripts/check_codegen.py target-review/release/lcpc-bench --strict` — passed; 16 kernels, zero violations.
- `taskset -c 0,1,3-7 cargo test --workspace --release` — passed after the fixes: 102 passed, 5 ignored benchmark tests, and 1 ignored doc test.
- Missing-vector negative check — the selected SIMD-Spielman oracle test failed with the required fixture message when pointed at a nonexistent `SIMD_SPIELMAN_TV_DIR`.
- `sh -n` on modified reproduction scripts and targeted `rustfmt --check` on the formatted encoder — passed.
- `cargo fmt --all -- --check` — reports pre-existing formatting differences in other workspace files; those files were not reformatted by this review.
