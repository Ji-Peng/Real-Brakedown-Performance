# SIMD-Spielman comparison baseline

Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0.

This directory contains the small local patch needed to reproduce the SIMD-Spielman baseline without changing the read-only upstream checkout. The upstream artifact remains at commit `7b399dbfecc406c79349015f1e21accc4def1be7` and retains its MIT license and copyright notices.

`params.patch` is applied only to an archived copy of that commit under `target-simd-spielman/`. Its SHA-256 is pinned in `reproduce/simd_spielman.sh` and written into each run manifest. The patch makes `SELECTED_CPU` overridable at compile time, makes the upstream slice tables overridable at run time, exposes CMake's polynomial-size and thread lists, and assigns worker threads from an explicit core list instead of `BASE_CORE + worker_index`. No file in `SIMD-Spielman-for-ZKPs/` is edited.

## Prerequisites

The upstream build requires CMake, a C++ compiler, Python 3, pthreads, Boost headers (`libboost-all-dev` on Debian/Ubuntu), and an x86-64 CPU with `avx512ifma`. The script checks for IFMA and exits successfully with a clear skip message when the CPU lacks it. Other missing build prerequisites are errors. The source's CMake configuration supplies the upstream flags, including `-march=native -O3 -funroll-loops` and the AVX-512 options; actual compiler versions and configuration are recorded in the manifest.

## Run

From `code/`, for example:

```sh
sh reproduce/simd_spielman.sh --threads 1 --cores 3 --logN 16 --runs 3 \
  --out target-simd-spielman/smoke/threads1
sh reproduce/simd_spielman.sh --threads 4 --cores 0,1,3,4 --logN 16 --runs 3 \
  --out target-simd-spielman/smoke/threads4
```

The script sources `reproduce/common.sh`; it accepts its `--threads`, `--logN`, `--runs`, `--cores`, `--field`, `--slices`, `--host`, and `--out` options. With no `--field`, it runs Montgomery, Mersenne, and extension-field cases for Brakedown and Orion, plus the Montgomery verifier benchmark. `--field` accepts `mont127`, `mers127`, or `fp2m61` to select one corresponding upstream field. Thread counts are `1`, `2`, `4`, `8`, or `16`; multi-threaded benchmark runs require at least that many distinct core IDs.

By default, the paper's table for the detected CPU is used. `--slices N` uses one explicit count for the Brakedown executables; Orion uses the smaller of that count and its 16 available rows. The copied upstream code also accepts `SIMD_SPIELMAN_SLICE_TABLE='16:1,18:2,20:4'`, whose entries override the selected CPU table for matching `logN` values. Each benchmark prints its effective slice count, which is collected into the manifest alongside the CPU selector, requested slice configuration, thread count, explicit core list, upstream commit, and patch checksum.

Before timing, the script builds the upstream correctness targets and runs the complete upstream `run_tests.py` suite under `taskset`. Benchmarks run under `taskset` with the same explicit affinity, and each benchmark's raw output is saved separately. The script builds both tests and benchmarks out of tree; generated source copies and build directories live below `target-simd-spielman/`.

Default output is `results/<host>/<UTC-date>-<code-git-sha>/simd_spielman/`. `--out` selects an exact output directory; relative paths are resolved from `code/`. Each output contains raw logs, a manifest, and a SIMD-Spielman-specific manifest supplement. Workstation measurements are smoke checks only and must not be reported as publishable performance results.

## License and attribution

The copied upstream sources are governed by the upstream MIT license. `params.patch`, this documentation, and the reproduction driver are RealBrakedownPerf additions; the patch records only the changes required to configure the upstream build and pin its workers safely. The SIMD-Spielman implementation remains the upstream AVX-512 implementation and is reported separately from the Rust LCPC fork.
