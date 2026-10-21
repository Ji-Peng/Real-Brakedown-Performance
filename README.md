# Real Brakedown Performance (LCPC fork)

This repository is a fork of [LCPC](https://github.com/conroi/lcpc) by Riad S. Wahby
(Apache-2.0, see `LICENSE`), imported with its full history at upstream commit
`f0e2116001af5a1bbf39481535f837d6a25772e8`. The original upstream README is kept
unchanged in [`README-lcpc.md`](README-lcpc.md).

The fork integrates the scalar/software optimizations analysed in *High-Performance
SIMD Software for Spielman Codes in Zero-Knowledge Proofs* (Krieger, Dobrouschek,
Hirner, Sinha Roy; TCHES 2026) into LCPC's Brakedown (SDIG) encoder, without explicit
SIMD: portable stable Rust, compiler-generated scalar code. Each technique is added as
one tagged commit (`step/NN-<name>`) on top of the import (`step/00-import`), so the
history itself documents what changed relative to upstream.

---

## 1. Build Policy and Prerequisites

- **Toolchain**: Pinned stable Rust via `rust-toolchain.toml` (Rust 1.99.1, edition 2021).
- **Auto-Vectorization Policy**: Disables LLVM loop and SLP vectorizers across workspace crates
  via `.cargo/config.toml` (`-C no-vectorize-loops -C no-vectorize-slp`).
- **Target Optimization**: `-C target-cpu=native` enables the best scalar instructions (e.g. `mulx`, `adc` on x86-64).
- **Portable Dependencies**: `blake3` is built portable-only with `features = ["pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]`.

To build the workspace in release mode:

```sh
. "$HOME/.cargo/env"
cargo build --workspace --release
```

---

## 2. Testing and Correctness Verification

The test suite runs unit tests, big-integer algebraic field axioms, bit-exactness oracles against original LCPC (`f0e2116`), and test-vector checks against upstream SIMD-Spielman:

```sh
# Run on permitted cores (CPU 2 is reserved on the local workstation):
taskset -c 0,1,3-7 cargo test --workspace --release
```

Or using the test orchestrator:

```sh
python3 scripts/run_tests.py --cores 0,1,3-7
```

### Codegen Verification (`scripts/check_codegen.py`)

Ensures hot kernels contain no vector-register arithmetic instructions:

```sh
python3 scripts/check_codegen.py target-harness/release/lcpc-bench \
    --output-json target-harness/codegen_report.json \
    --output-listing target-harness/disassembly.txt
```

---

## 3. Benchmark Binaries (`bench/`)

The workspace provides stable benchmark binaries adhering to the `lcpc-bench-v1` JSON schema (`docs/bench-json-schema.md`):

1. **`lcpc-bench`**: The complete benchmark driver evaluating prover encoding, verifier encoding, hash decomposition, commit total, prove, verify, and peak RSS:
   ```sh
   taskset -c 0 cargo run --release --bin lcpc-bench -- \
       --field mont127 \
       --params lcpc-sdig6 \
       --logN 16,18,20 \
       --threads 1 \
       --slices adaptive \
       --runs 30 \
       --warmup 5 \
       --output results.json
   ```

2. **`bench_encode`**: Dedicated prover and small-batch verifier encoding benchmark binary:
   ```sh
   taskset -c 0 cargo run --release --bin bench_encode -- \
       --field mont127 --params lcpc-sdig6 --logN 16,18,20 --slices adaptive --output encode.json
   ```

3. **`bench_pcs`**: Dedicated end-to-end PCS benchmark binary (Commit / Prove / Verify with encoding vs. portable hashing breakdown):
   ```sh
   taskset -c 0 cargo run --release --bin bench_pcs -- \
       --field mont127 --params lcpc-sdig6 --logN 16,18,20 --slices adaptive --output pcs.json
   ```

4. **`bench_fields`**: Microbenchmark binary for modular field arithmetic (`mul`, `mac`, `reduce`):
   ```sh
   taskset -c 0 cargo run --release --bin bench_fields -- \
       --field all --runs 100000 --output field_benchmarks.json
   ```

---

## 4. Reproduction Scripts (`reproduce/`)

POSIX `sh` scripts reproduce all experiments and baseline comparisons with automated manifest generation:

- **`reproduce/this_work.sh`**:
  Builds the fork HEAD, runs tests (aborting on any failure), generates the disassembly codegen report, executes the timed benchmarks, and writes results and manifest into `results/<host>/<date>-<gitsha>/this_work/`.
  ```sh
  ./reproduce/this_work.sh --threads 1 --logN 16,18,20,22,24,26 --cores 0 --runs 30
  ```

- **`reproduce/steps.sh`**:
  Evaluates the cumulative per-technique progression by checking out each tagged commit (`step/01-modernize` through `HEAD`) in isolated worktrees, running correctness tests, and measuring performance under identical parameters:
  ```sh
  ./reproduce/steps.sh --threads 1 --logN 16,18,20 --cores 0 --runs 30
  ```

- **`reproduce/lcpc_orig.sh`**:
  Builds and benchmarks the unmodified upstream LCPC implementation at commit `f0e2116` via the external stable-Rust driver (`external/lcpc-orig`):
  ```sh
  ./reproduce/lcpc_orig.sh --threads 1 --logN 16,18,20 --cores 0 --runs 30
  ```

- **`reproduce/simd_spielman.sh`**:
  Builds and benchmarks the upstream C++ AVX-512 IFMA baseline (`7b399db`) out of tree with `external/simd_spielman/params.patch`:
  ```sh
  ./reproduce/simd_spielman.sh --threads 1 --logN 16,18,20 --cores 0 --runs 30
  ```

- **`reproduce/all.sh`**:
  Orchestrates all four targets in one unified session with identical parameters and generates analysis tables and plots:
  ```sh
  ./reproduce/all.sh --threads 1 --logN 16,18,20,22,24,26 --cores 0 --runs 30
  ```

---

## 5. Result Analysis and LaTeX Table Generation (`scripts/analyze.py`)

To aggregate raw JSON results into publication-ready CSVs, LaTeX tables (`paper/`), and charts:

```sh
python3 scripts/analyze.py --results-dir results/ --out-dir analysis/
```

Outputs:
- **CSVs**: `csv/prover_encoding_comparison.csv`, `csv/pcs_breakdown.csv`, `csv/steps_ablation.csv`, `csv/field_microbenchmarks.csv`
- **LaTeX Tables**: `latex/table_encoding_comparison.tex`, `latex/table_pcs_breakdown.tex`, `latex/table_steps_ablation.tex`, `latex/table_field_microbenchmarks.tex`
- **Plots**: `plots/encoding_latency_vs_logn.png`, `plots/pcs_breakdown.png`

## Benchmark machines (Google Cloud)

All published numbers come from Google Cloud VMs created with the `gcloud` CLI. A
local workstation is used only for building and testing; no published number comes
from it. The scripts in `reproduce/gcp/` wrap exactly the commands below; if the two
ever disagree, the commands in this section are the reference configuration.

### Configuration

| Setting | Primary x86-64 machine | Arm portability machine |
| :--- | :--- | :--- |
| Instance name | `rbd-bench-c3d` | `rbd-bench-c4a` |
| Machine type | `c3d-highmem-30` | `c4a-highmem-16` |
| CPU | AMD EPYC 9004 "Genoa" (Zen 4), AVX-512 incl. IFMA | Google Axion (Arm Neoverse V2) |
| vCPUs / cores | 30 vCPUs → 15 physical cores with `--threads-per-core=1` (SMT off) | 16 vCPUs = 16 cores (no SMT) |
| Memory | 240 GB | 128 GB |
| Zone | `us-central1-a` | `us-central1-a` |
| Image | `debian-12` from `debian-cloud` (x86-64) | `debian-12-arm64` from `debian-cloud` |
| Boot disk | 100 GB `pd-balanced` | 100 GB `hyperdisk-balanced` (C4A uses Hyperdisk) |
| Provisioning | on-demand (`STANDARD`), `--maintenance-policy=TERMINATE` (no live migration) | same |
| Used for | All implementations (this work, original LCPC, PolyCommit, SIMD-Spielman), 2 independent sessions | This work and original LCPC only (SIMD-Spielman needs AVX-512), 1 short session, log2 N in {18, 22, 26} |

Checked on 2026-10-21 with Google Cloud SDK 590.0.0: both machine types are offered
in `us-central1-a` (`c3d-highmem-30`: 30 vCPUs / 240 GB; `c4a-highmem-16`: 16 vCPUs /
128 GB). The images resolved to `debian-12-bookworm-v20261014` (X86_64) and
`debian-12-bookworm-arm64-v20261014` (ARM64). The manifest of every run records the
image, zone, CPU model, and guest cache topology that were actually used.

Fallback if `c3d` is unavailable: `c4-highmem-32` (Intel Xeon, 16 physical cores with
`--threads-per-core=1`, `hyperdisk-balanced` boot disk). One machine family is used for
a whole campaign; results from different families are never mixed in one table.

### Create the VMs

Authenticate and select the project first (`gcloud auth login`,
`gcloud config set project <PROJECT>`). Creating a VM starts billing.

```sh
# Primary x86-64 benchmark machine
gcloud compute instances create rbd-bench-c3d \
  --zone=us-central1-a \
  --machine-type=c3d-highmem-30 \
  --threads-per-core=1 \
  --image-family=debian-12 --image-project=debian-cloud \
  --boot-disk-size=100GB --boot-disk-type=pd-balanced \
  --provisioning-model=STANDARD --maintenance-policy=TERMINATE

# Arm portability machine
gcloud compute instances create rbd-bench-c4a \
  --zone=us-central1-a \
  --machine-type=c4a-highmem-16 \
  --image-family=debian-12-arm64 --image-project=debian-cloud \
  --boot-disk-size=100GB --boot-disk-type=hyperdisk-balanced \
  --provisioning-model=STANDARD --maintenance-policy=TERMINATE
```

Optional hardware performance counters: if the machine type supports a guest PMU, add
`--performance-monitoring-unit=enhanced` (core/L2 and LLC events) to collect `perf stat`
counters. If creation fails with that flag, create the VM without it and record that
counters were unavailable.

### First-boot checks

Run these on each VM; `reproduce/gcp/setup_vm.sh` saves them in the run manifest.

```sh
lscpu                                    # model; c3d: 15 cores, 1 thread per core
grep -o -m1 avx512ifma /proc/cpuinfo     # c3d only: required by SIMD-Spielman
cat /sys/devices/system/cpu/cpu*/cache/index3/shared_cpu_list | sort -u   # L3 domains
nproc; free -g; uname -a; cat /etc/os-release
```

Use one fixed core for 1-thread runs, and 8 distinct physical cores in one reported L3
domain for 8-thread runs. Leave the remaining cores for the OS and the harness.

### Stop or delete the VMs

Cost rule: a VM runs only while a benchmark task is running on it. Stop it between sessions, while results are reviewed, and after any failure. `reproduce/gcp/run_session.sh` stops it automatically on exit. Delete both VMs once all benchmark sessions are finished.

```sh
gcloud compute instances stop   rbd-bench-c3d --zone=us-central1-a   # keeps the disk
gcloud compute instances delete rbd-bench-c3d --zone=us-central1-a   # removes VM and boot disk
gcloud compute instances stop   rbd-bench-c4a --zone=us-central1-a
gcloud compute instances delete rbd-bench-c4a --zone=us-central1-a
```

A stopped VM no longer bills for vCPUs and memory, but its disk is still billed.
