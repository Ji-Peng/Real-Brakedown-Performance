# SIMD-Spielman comparison baseline

Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0.

This directory contains the small local patch needed to reproduce the SIMD-Spielman baseline without changing the read-only upstream checkout. The upstream artifact remains at commit `7b399dbfecc406c79349015f1e21accc4def1be7` and retains its MIT license and copyright notices.

`params.patch` is applied only to an archived copy of that commit under `target-simd-spielman/`. Its SHA-256 is pinned in `reproduce/simd_spielman.sh` and written into each run manifest and record (`params_patch_sha256`). No file in `SIMD-Spielman-for-ZKPs/` is edited. The patch changes the following upstream files:

| Upstream file | Change |
| :--- | :--- |
| `spielman/SpielmanParams.h` | `SELECTED_CPU` overridable at compile time; the paper's slice tables kept, but overridable at run time (`SIMD_SPIELMAN_SLICES`, `SIMD_SPIELMAN_SLICE_TABLE`). |
| `utils/utils.h` | `pinThread(i)` pins to the `i`-th entry of the explicit core list `SIMD_SPIELMAN_CORES` instead of `BASE_CORE + i`. |
| `spielman/SpielmanCodeMT.h` | Each worker pins itself through `pinThread(worker index)` instead of a `pthread` affinity of `base_core + i` (no `base_core` argument). |
| `CMakeLists.txt` | Exposes the polynomial-size (`logN`) and thread lists. |
| `bench/*_Bench.cpp` (all seven) | **Benchmark method.** Each executable prints `GraphGen Done ... slices=<s>` and one `Encode sample <j> : <us>` line per encoding, so a single process yields every sample (upstream prints only the total over all repetitions). The first `--warmup` samples are discarded by the script. MT benchmarks no longer take a base-core argument. |
| `bench/*MT_Bench.cpp` (three) | Since patch `d9cf97f` (commit `dd9009a`): the spin-waiting coordinator (main) thread is pinned with `pinThread(THREADS)`, i.e. to the core after the `THREADS` workers. |
| `run_throughput_epyc.py` | Takes explicit core lists from the environment (not used by the campaign). |

The upstream encoding kernels, graph generation, fields and timed region are unchanged. The timed region is upstream's: it covers `r2l_runEncodeMatrixEfficient[MT]` only. Upstream generates the input directly in its reordered (column-packed) layout before the timer starts, so SIMD-Spielman's numbers include no layout conversion, whereas This Work's timed `encode_matrix` includes its row-major to column-major transpose (`transpose_into`). This difference favours SIMD-Spielman and is stated with the results.

### Patch versions in the 2026-10-22/23 campaign

| Patch SHA-256 (prefix) | Fork commit | Used for |
| :--- | :--- | :--- |
| `12f29de2e552…` | `4cace86` (one process per case) | All SIMD-Spielman T = 1 and the superseded T = 8 blocks at log2 N ≤ 26 (run dirs `2026-10-22-2557b5517338/s1`, `2026-10-22-18d75f808f59/s2`). |
| `d9cf97f0347f…` | `dd9009a` (coordinator pinned) | T = 7 and T = 8 at log2 N ≤ 26 (stage R) and every log2 N = 28 block (stage B). |

The only difference between the two patch versions is the `pinThread(THREADS)` call in the three MT benchmarks. Single-threaded executables never call it, so the T = 1 data from `12f29de` is unaffected. Before `dd9009a` the coordinator floated over the 8 worker cores and spin-waited (9 runnable threads on 8 cores); those T = 8 blocks are superseded (listed in `results/selection.json`). T = 8 still has 8 workers plus the coordinator on 8 cores (`coordinator_shares_worker_core: true`); the 8-core comparison point is **T = 7**: 7 workers plus the pinned coordinator.

## Prerequisites

The upstream build requires CMake, a C++ compiler, Python 3, pthreads, Boost headers (`libboost-all-dev` on Debian/Ubuntu), and an x86-64 CPU with `avx512ifma`. The script checks for IFMA and exits successfully with a clear skip message when the CPU lacks it. Other missing build prerequisites are errors. The source's CMake configuration supplies the upstream flags, including `-march=native -O3 -funroll-loops` and the AVX-512 options; actual compiler versions and configuration are recorded in the manifest.

## Run

From `code/`, for example:

```sh
sh reproduce/simd_spielman.sh --threads 1 --cores 3 --logN 16 --runs 3 --warmup 1 \
  --out target-simd-spielman/smoke/threads1
sh reproduce/simd_spielman.sh --threads 4 --cores 0,1,3,4 --logN 16 --runs 3 --warmup 1 \
  --out target-simd-spielman/smoke/threads4
```

The script sources `reproduce/common.sh`; it accepts its `--threads`, `--logN`, `--runs`, `--warmup`, `--cores`, `--field`, `--shape`, `--slices`, `--host`, and `--out` options (defaults: 5 runs, 2 warm-ups; the campaign used `--runs 5 --warmup 1`). With no `--field`, it runs Montgomery, Mersenne, and extension-field cases for Brakedown and Orion, plus the Montgomery verifier benchmark. `--field` accepts `mont127`, `mers127`, or `fp2m61`. `--threads` accepts any T from 1 to 16 (the campaign used 1, 7 and 8); multi-threaded runs need an explicit `--cores` list; the coordinator is pinned to list entry T + 1, or wraps to the first listed core when the list has only T entries (as for T = 8 on the 8-core VM). `--max-orion-logn N` (set to 26 by `all.sh --profile full`) skips Orion-shape cases above 2^N and records them with status `omitted` and an `omission_reason`: upstream's Orion graph generation at 2^28 took more than 90 minutes per process on the campaign VM. (In the campaign these cases were instead stopped by an external watcher and have status `failed`.)

**Setup time and memory (new runs only).** Each benchmark process runs under `/usr/bin/time -v`; records get `peak_rss_bytes` and an approximate `setup_ns` = process wall time minus the sum of all encode samples. The wall time has 10 ms resolution and includes process start, input generation and output, so `setup_ns` is only meaningful where graph generation dominates (large N). The campaign's SIMD-Spielman records have neither field.

**Measurement method (since `4cace86`).** One process per (field, shape, logN) case: the graph is generated once, then the process runs `warmup + runs` encodings, each on fresh random input, each timed separately. Upstream's own harness instead builds one executable per size and reports the mean over its repetitions; per-sample timing was added so that the median and range can be reported like the other implementations.

**Slices.** By default the paper's table for `SELECTED_CPU` is used. On `c3d` that is the EPYC 9754 (Bergamo) table, for example 128 slices at 2^26 and 8 at 2^28. It was tuned on a CPU with 16 MB L3 per CCX, while the campaign's EPYC 9B14 guest reports 32 MB L3 per domain. **No SIMD-Spielman slice sweep was run in the campaign** (all records have `slice_mode: table`), whereas This Work used its adaptive slice choice. Results must label SIMD-Spielman's slice count as "upstream table, untuned on this host", especially at 2^28. `--slices N` uses one explicit count for the Brakedown executables; Orion uses the smaller of that count and its 16 available rows. `SIMD_SPIELMAN_SLICE_TABLE='16:1,18:2,20:4'` overrides individual table entries. The effective slice count of each case is recorded in the manifest (`effective_slice_counts_by_benchmark`).

**Upstream tests.** Before timing, the script builds the upstream correctness targets and runs the complete upstream `run_tests.py` suite under `taskset`. Since `2557b55` it does this once per host, upstream commit, patch SHA-256 and compiler, recorded by a marker file under `target-simd-spielman/`; later runs with the same key reuse the marker and copy its log. Benchmarks run under `taskset` with the same explicit affinity, and each case's raw output is saved separately. Generated source copies and build directories live below `target-simd-spielman/`.

Default output is `results/<host>/<UTC-date>-<code-git-sha>/simd_spielman/`. `--out` selects an exact output directory; relative paths are resolved from `code/`. Each output contains raw logs, a manifest, and a SIMD-Spielman-specific manifest supplement. Workstation measurements are smoke checks only and must not be reported as publishable performance results.

## License and attribution

The copied upstream sources are governed by the upstream MIT license. `params.patch`, this documentation, and the reproduction driver are RealBrakedownPerf additions; the patch configures the upstream build, pins its workers and coordinator to explicit cores, and changes only the benchmark executables' sample reporting (see the table above). The SIMD-Spielman implementation remains the upstream AVX-512 implementation and is reported separately from the Rust LCPC fork.
