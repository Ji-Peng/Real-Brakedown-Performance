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

Checks that the hot kernels contain no vector arithmetic (status `FAIL` otherwise). Vector-register data movement used only for zeroing and copying (x86-64 `vmovups`/`vxorps` zero idioms; AArch64 `movi vN.2d, #0`, `stp`/`ldp`/`str`/`ldr qN`) is counted separately as `vector_moves` and does not fail; scalar branches are scalar on every architecture. BLAKE3's SIMD modules are linked but disabled at run time. See `docs/CODEGEN.md` §4 and §4.5 for the classification and the exact claim:

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
       --runs 5 \
       --warmup 1 \
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

POSIX `sh` scripts reproduce all experiments and baseline comparisons. Each runs the relevant correctness tests first, aborts on failure, and writes raw JSON plus a manifest under `results/<host>/<UTC-date>-<gitsha>/`. Defaults are `--runs 5 --warmup 2`; the GCP campaign used `--runs 5 --warmup 1` everywhere. The examples below are local smoke runs (never publish local numbers; never use CPU 2 on the workstation).

- **`reproduce/this_work.sh`**: builds the fork HEAD, runs the tests, runs the strict codegen check, then benchmarks. `--params rs-ligero` measures the same library's Ligero Reed–Solomon encoding (E12/T10).
  ```sh
  ./reproduce/this_work.sh --threads 1 --cores 3 --logN 16,18,20 --runs 5 --warmup 1
  ```
- **`reproduce/steps.sh`**: builds, tests and benchmarks each tag `step/01-modernize` … `step/09-codegen` in its own worktree with HEAD's harness (cumulative, order-dependent). `--mode a|b|both` selects the code-parameter mode (default `both`); the full profile's two steps blocks pass `--mode a` and `--mode b`. In the campaign both steps blocks still measured both modes (`docs/STATUS.md` §7.3).
  ```sh
  ./reproduce/steps.sh --mode a --threads 1 --cores 3 --logN 16,20 --runs 5 --warmup 1
  ```
- **`reproduce/lcpc_orig.sh`**: unmodified upstream LCPC `f0e2116` through `external/lcpc-orig` (golden vectors first). `LCPC_VARIANTS` selects build variants. Every variant is built from outside `code/`, so the fork's `.cargo/config.toml` applies only to the `fork-flags-*` variants, which pass the fork's flags explicitly; each record carries `build_variant` and `effective_rustflags` (`external/lcpc-orig/README.md`).
  ```sh
  LCPC_VARIANTS=as-published-as-is,fork-flags-portable \
    ./reproduce/lcpc_orig.sh --threads 1 --cores 3 --logN 16,18,20 --runs 5 --warmup 1
  ```
- **`reproduce/simd_spielman.sh`**: upstream C++ AVX-512 IFMA baseline (`7b399db`) built out of tree with `external/simd_spielman/params.patch`; needs `avx512ifma`. `--max-orion-logn N` skips Orion-shape cases above 2^N and records them with status `omitted` (`external/simd_spielman/README.md`).
  ```sh
  ./reproduce/simd_spielman.sh --threads 1 --cores 3 --logN 16,18,20 --runs 5 --warmup 1
  ```
- **`reproduce/polycommit.sh`**: arkworks PolyCommit Brakedown encoding with `external/polycommit/polycommit.patch` (`external/polycommit/README.md`).
- **`reproduce/all.sh`**: the experiment catalogue of `agent/plan/06` as a randomized block schedule. `--profile smoke|full|arm` selects the catalogue; `--blocks REGEX`, `--logn LIST`, `--warmup N` and `--lcpc-variants LIST` restrict a run to one stage; `--stage NAME` labels it; `--session` and `--seed` name the session and the block order; `--run-date YYYY-MM-DD` fixes the date part of the run id (`run_stages.sh` passes its start date, so a session that crosses midnight UTC stays in one run directory). A failed block is recorded in `failed_blocks.tsv`, the stage continues, and `all.sh` exits 1 at the end.
  ```sh
  ./reproduce/all.sh --profile smoke --session local1
  ```
- **`reproduce/gcp/`**: `create_vm.sh` (`--machine c3d|c4a --instance NAME --zone ZONE`), `setup_vm.sh`, `run_session.sh` (start the VM, sync `code/`, run `all.sh` with the same stage options, copy the session back, stop the VM unless `--keep-running`), `run_stages.sh` (a list of stages of one session on one VM, then stop; `--preset` adds predefined stage lists), and `delete_vm.sh`.

### Session directory layout

One session is `results/<host>/<UTC-date>-<gitsha>/<session>/`. Staged campaigns call `all.sh` several times for the same session directory; each call is one stage.

| Path in the session | Content |
| :--- | :--- |
| `<block-id>/` | The block's `--out` directory: raw JSON records and manifests. The block id equals the directory name; with `--logn` it ends in `_n<list>`, the logN values the block actually ran (for example `full_this_work_threads_t4_n20-24-26`). |
| `<block-id>.prev-<UTC time>/` | Before a block runs, `all.sh` moves an existing output directory of the same block aside under this name, so a re-run never mixes records of two stages. Analysis excludes these directories (`results/selection.json`, path glob `*.prev-*`). |
| `stages/<UTC time>_<stage>/` | One directory per stage: its `schedule.json`, `blocks.tsv`, `manifest.json`, `execution_log.json`, and copies of the VM's host profile and first-boot record. A stage never overwrites another stage's files. This is not a session; analysis never treats it as one. |
| `schedule.json` | Session-level and append-only (`rbd-session-schedule-v1`): its `stages` list accumulates every stage (stage tag and directory, the `all.sh` arguments: `--blocks` filter, logN list, warm-up, LCPC variants; seed, commit, instance, block ids with per-block status, exit status). A pre-existing campaign `schedule.json` is kept inside it as `legacy_schedule`. |
| `failed_blocks.tsv` | One line per failed block: block id, duration, exit code, stage tag. |

`all.sh` tags every JSON record in a block's own output directory, failed blocks included, with `session_id`, `session_seed`, `stage`, `block_id`, `block_status` and the frequency fields (`docs/bench-json-schema.md`).

### Campaign as run (2026-10-22/23)

The published GCP data was collected in stages, not by one `all.sh --profile full` call, and **before** the stage layout above existed: each stage wrote its blocks next to the earlier ones and overwrote the session's `schedule.json` (`docs/STATUS.md` §7.2). Per x86-64 session (`s1` on `rbd-bench-c3d-3`, `s2` on `rbd-bench-c3d-2`), every stage used `--profile full --warmup 1 --lcpc-variants as-published-as-is,fork-flags-portable`:

| Stage | `--blocks` as run | `--logn` |
| :--- | :--- | :--- |
| A | `^full_(this_work_mode\|lcpc_orig_mode\|polycommit\|ligero)` | `16,18,20,22,24,26` |
| S | `^full_simd_spielman` | `16,18,20,22,24,26` |
| C | `^full_(slice_sweep\|thread_scaling)` (block ids of that time; see below) | `20,24,26`; thread scaling ran only at 20 and 24 |
| D | `^full_steps` | `16,20,24` |
| B | `^full_(this_work_mode\|lcpc_orig_mode\|polycommit\|simd_spielman_mont127)` | `28` |
| R | `^full_(lcpc_orig_mode_a\|simd_spielman_.*_t[78])` | `16,18,20,22,24,26` |
| ARM (`--machine c4a`, profile `arm`, session `arm1`) | `^arm_` | `18,22,26` |

**Stage C and thread scaling.** The thread-scaling blocks were then defined with log2 N 20, 24, 28; intersected with stage C's `--logn 20,24,26` they ran at **2^20 and 2^24 only**, although their directories are named `..._n20-24-26`. The slice-sweep blocks ran at all three sizes.

**Renamed block ids.** The stage-C regex above matched the block ids of that time (`full_thread_scaling_{this_work,lcpc_orig}_t*`, and `full_polycommit_baseline` for PolyCommit T = 1). The block ids have since been renamed to their output directory names (`full_this_work_threads_t*`, `full_lcpc_orig_threads_t*`, `full_polycommit_t1`). To reproduce stage C with the current `all.sh`, use `--blocks '^full_(slice_sweep|this_work_threads|lcpc_orig_threads)'`; the old regex now selects only the slice sweep. The other regexes still match.

`reproduce/gcp/run_stages.sh --preset x86` runs stages A, S, C, D and B with the current block ids, and `--machine c4a --preset arm` runs the Arm stage. Which run directories and blocks count is fixed in `results/selection.json`; `docs/STATUS.md` §7 records the stage → commit → run-dir map, all deviations from the plan, and every incident that invalidated data.

### Planned re-measurement

Several cells of the campaign are invalid or missing (`docs/STATUS.md` §7.4, §7.6). `run_stages.sh --preset remeasure` re-measures them: with `--machine c3d` (run it once per x86-64 session, `s1` and `s2`) it appends the stages below; with `--machine c4a` (session `arm1`) the Arm stage. Each stage is planned to take at most about 2 h. `--print-stages` prints the expanded list without touching a VM, for checking with `all.sh --dry-run`.

| Stage | `--blocks` | log2 N | LCPC variants | Purpose |
| :--- | :--- | :--- | :--- | :--- |
| RL | `^full_lcpc_orig_mode_a_t[18]$` | 16–26 | `as-published-as-is`, `fork-flags-portable` | original LCPC as published (Cargo defaults, I4), with the fork-flags variant on the same boot |
| RL28 | same | 28 | same | the same at 2^28 |
| RH | same | 20, 24 | `as-published-portable`, `fork-flags-as-is` | the two other variants, so that T6 (E8) can separate the BLAKE3 backend from the compiler flags |
| RT | `^full_(this_work\|lcpc_orig)_threads_t[0-9]+$` | 20, 24, 26 | `as-published-as-is`, `fork-flags-portable` | thread scaling (E6), This Work and original LCPC, T = 1, 2, 4, 8 on the first T cores |
| RT28A, RT28B | the thread-scaling blocks, split in two stages | 28 | same | thread scaling at 2^28 |
| RS | `^full_simd_spielman_mont127_t1$` | 28 | — | SIMD-Spielman Mont127 T = 1, Brakedown shape (missing in s1, I5) |
| ARM (`--machine c4a`) | `^arm_` | 18, 22, 26 | `as-published-as-is`, `fork-flags-portable` | This Work on Arm after the AArch64 codegen-checker fix (I7), and original LCPC as published on Arm |

```sh
./reproduce/gcp/run_stages.sh --instance "$INSTANCE" --zone "$ZONE" --session s1 --preset remeasure
./reproduce/gcp/run_stages.sh --instance "$INSTANCE" --zone "$ZONE" --session arm1 --machine c4a --preset remeasure
```

(`GCLOUD` selects the `gcloud` binary if it is not on `PATH`.)

At most two of the 16-vCPU VMs can run at the same time (`CPUS_ALL_REGIONS` quota, "Benchmark machines").

---

## 5. Result Analysis and LaTeX Table Generation (`scripts/analyze.py`)

To aggregate the selected raw JSON results into CSVs, LaTeX tables and figures:

```sh
python3 scripts/analyze.py --results-dir results/ --selection results/selection.json \
    --out-dir analysis/ --check
```

`--selection` restricts the input to the run directories, sessions and blocks listed in `results/selection.json` and refuses records it does not cover. Without it, every JSON under `--results-dir` is read, which mixes superseded and invalid data. A session is the directory that holds a session's block directories (the parent of `stages/`); `stages/` subdirectories and `*.prev-*` block directories are never sessions (see "Session directory layout").

The strict check (`--check`) fails on

- integrity errors: duplicate or mixed configurations in one cell, oversubscribed runs (threads > pinned cores), and records not covered by the selection;
- any selected codegen report with status `FAIL` (vector arithmetic in a hot kernel), unless `results/selection.json` documents it as an omission; T0 and T11 take their PASS/FAIL from these reports;
- empty cells, `-`, NaN and `SKIPPED` values in every table.

The `omissions` of `results/selection.json` (`kind` `pending` or `omitted`, with a reason) apply only to missing or N/A cells, such as Orion-shape SIMD-Spielman at 2^28 ("omitted (documented)") or cells pending re-measurement; they never excuse an integrity error, and an omission that matches no cell produces a warning. Repeatable `--allow 'TABLE|ROW|COLUMN'` globs select further documented exceptions; smoke runs pass the known local limits in
`reproduce/all.sh` (Arm is not run locally, and the smoke subset omits the campaign's larger field measurements).
Run `python3 scripts/analyze.py --self-test` to check the missing-cell rules.

Outputs (under `--out-dir`):
- **CSV** (`csv/`) and **LaTeX** (`latex/`): `table_T0_correctness`, `table_T1_headline_encoding`, `table_T2_simd_spielman`, `table_T3_steps_breakdown`, `table_T4_slice_sweep`, `table_T5_field_comparison`, `table_T6_pcs_breakdown`, `table_T7_verifier_encoding`, `table_T8_setup_memory`, `table_T9_orion_shape`, `table_T10_brakedown_vs_rs`, `table_T11_codegen_evidence`, `table_T12_arm_portability` (`.csv` / `.tex`).
- **Figures** (`plots/`): `figure_F1_steps_breakdown`, `figure_F2_llc_scaling`, `figure_F3_thread_scaling` (`.pdf` and `.png`).

The paper's tables are to be copied from these outputs into `paper/` (for example `paper/generated/`, not yet created); no number is entered by hand.

## Benchmark machines (Google Cloud)

All published numbers come from Google Cloud VMs created with the `gcloud` CLI. A
local workstation is used only for building and testing; no published number comes
from it. The scripts in `reproduce/gcp/` wrap exactly the commands below; if the two
ever disagree, the commands in this section are the reference configuration.

### Configuration

| Setting | Primary x86-64 machines | Arm portability machine |
| :--- | :--- | :--- |
| Machine type | `c3d-highmem-16` (planned `c3d-highmem-30`; see note below) | `c4a-highmem-16` |
| CPU | AMD EPYC 9004 "Genoa" (Zen 4; guest reports EPYC 9B14), AVX-512 incl. IFMA | Google Axion (Arm Neoverse V2) |
| vCPUs / cores | 16 vCPUs → 8 physical cores with `--threads-per-core=1` (SMT off); one 32 MB L3 domain (CPUs 0–7) | 16 vCPUs = 16 cores (no SMT) |
| Memory | 128 GB | 128 GB |
| Image | `debian-12` from `debian-cloud` (x86-64) | `debian-12-arm64` from `debian-cloud` |
| Boot disk | 100 GB `pd-balanced` | 100 GB `hyperdisk-balanced` (C4A uses Hyperdisk) |
| Provisioning | on-demand (`STANDARD`), `--maintenance-policy=TERMINATE` (no live migration) | same |
| Used for | All implementations (This Work, original LCPC, PolyCommit, SIMD-Spielman), 2 independent sessions on 2 VMs | This Work and original LCPC only (SIMD-Spielman needs AVX-512), 1 short session, log2 N in {18, 22, 26} |

Instances of the 2026-10-22/23 campaign (all four still exist until the user deletes them):

| Instance | Type | Zone | Role |
| :--- | :--- | :--- | :--- |
| `rbd-bench-c3d` | `c3d-highmem-16` | `us-central1-c` | First x86-64 VM: first-boot checks and the early monolithic `s1` attempts (code `2b23858`, `454cb24`, `80ae955`). It was used only for runs whose data was later invalidated or left incomplete (`docs/STATUS.md` §7.4, I1 and I10); after repeated stockouts in `us-central1-c`, `s1` moved to `rbd-bench-c3d-3`. |
| `rbd-bench-c3d-3` | `c3d-highmem-16` | `us-east4-c` | Session `s1` (all valid `s1` data) |
| `rbd-bench-c3d-2` | `c3d-highmem-16` | `us-east1-d` | Session `s2` |
| `rbd-bench-c4a` | `c4a-highmem-16` | `us-central1-a` | Arm session `arm1` |

Planning check (2026-10-21, Google Cloud SDK 590.0.0): `c3d-highmem-30` (30 vCPUs / 240 GB) and
`c4a-highmem-16` (16 vCPUs / 128 GB) are listed in `us-central1-a`. The images resolved to
`debian-12-bookworm-v20261014` (X86_64) and `debian-12-bookworm-arm64-v20261014` (ARM64). The
manifest of every run records the image, instance, zone, CPU model, and guest cache topology
that were actually used.

Fallback if `c3d` is unavailable: `c4-highmem-32` (Intel Xeon, 16 physical cores with
`--threads-per-core=1`, `hyperdisk-balanced` boot disk). One machine family is used for
a whole campaign; results from different families are never mixed in one table.

**Why `c3d-highmem-16` (2026-10-22).** The planned `c3d-highmem-30` could not be created: this
project's `CPUS_PER_VM_FAMILY` quota for `C3D` is 24 vCPUs per region, and all `us-central1`
zones also reported a `c3d-highmem-30` stockout. `c3d-highmem-16` (same AMD EPYC Genoa / Zen 4
CPU with AVX-512 IFMA) was used instead. Consequences: 8-thread runs occupy all 8 physical
cores (none is left for the OS and harness), thread scaling stops at 8 threads, and the guest
PMU is unavailable (`--performance-monitoring-unit` is rejected for `c3d-highmem-*`). Repeated
stockouts in `us-central1-c` and `us-east1-d` moved session `s1` to a third VM in `us-east4-c`.
The project's global quota (`CPUS_ALL_REGIONS` = 32) allows at most two 16-vCPU VMs at the same
time; the two x86-64 sessions ran concurrently on separate VMs, and the Arm VM ran while one
x86-64 VM was still busy.

### Create the VMs

Authenticate and select the project first (`gcloud auth login`,
`gcloud config set project <PROJECT>`). Creating a VM starts billing.
`reproduce/gcp/create_vm.sh --machine c3d|c4a --instance NAME --zone ZONE` prints the same
command and runs it with `--yes`.

```sh
# x86-64 benchmark machine: one instance per session; INSTANCE and ZONE as in the table above
gcloud compute instances create "$INSTANCE" \
  --zone="$ZONE" \
  --machine-type=c3d-highmem-16 \
  --threads-per-core=1 \
  --image-family=debian-12 --image-project=debian-cloud \
  --boot-disk-size=100GB --boot-disk-type=pd-balanced \
  --provisioning-model=STANDARD --maintenance-policy=TERMINATE

# Arm portability machine (campaign: INSTANCE=rbd-bench-c4a, ZONE=us-central1-a)
gcloud compute instances create "$INSTANCE" \
  --zone="$ZONE" \
  --machine-type=c4a-highmem-16 \
  --image-family=debian-12-arm64 --image-project=debian-cloud \
  --boot-disk-size=100GB --boot-disk-type=hyperdisk-balanced \
  --provisioning-model=STANDARD --maintenance-policy=TERMINATE
```

**Quota.** Each of these VMs has 16 vCPUs, and the project's `CPUS_ALL_REGIONS` quota is 32
vCPUs, so at most **two** of them can run at the same time (stopped VMs do not count); creating
or starting a third fails. Create or start a VM only after another one has been stopped. To
reproduce, one x86-64 VM per session is enough (two sessions can run one after the other
on the same VM, on separate boots); the campaign used two x86-64 instances concurrently, in
the zones above, because of stockouts.

Optional hardware performance counters: if the machine type supports a guest PMU, add
`--performance-monitoring-unit=enhanced` (core/L2 and LLC events) to collect `perf stat`
counters. If creation fails with that flag, create the VM without it and record that
counters were unavailable.

### First-boot checks

Run these on each VM; `reproduce/gcp/setup_vm.sh` saves them in the run manifest.

```sh
lscpu                                    # model; c3d-highmem-16: 8 cores, 1 thread per core
grep -o -m1 avx512ifma /proc/cpuinfo     # c3d only: required by SIMD-Spielman
cat /sys/devices/system/cpu/cpu*/cache/index3/shared_cpu_list | sort -u   # L3 domains
nproc; free -g; uname -a; cat /etc/os-release
```

1-thread runs use one fixed core (CPU 1 on `c3d`, avoiding CPU 0). T-thread runs use the first T
cores of the 8-core set, in one reported L3 domain, for every implementation (SIMD-Spielman gets
T + 1 cores for its coordinator thread, at most 8); on `c3d-highmem-16`, T = 8 uses all 8 cores
(0–7), so the OS and harness share them. The first-boot record (`setup_vm.sh`) and the host profile
(`reproduce/hosts/<host>.sh`) are now written per instance
(`results/<host>/first-boot-manifest-<instance>.json`, `results/<host>/host-profile-<instance>.json`),
and `all.sh` copies them into each stage directory. In the
campaign it wrote one path per machine family (`results/gcp-c3d-highmem-16/first-boot-manifest.json`),
so that file holds only the last VM's record (`rbd-bench-c3d-3`); `s2`'s machine type, zone, CPU
model and cache topology are in its session and block manifests.

### Stop or delete the VMs

Cost rule: a VM runs only while a benchmark task is running on it. Stop it between sessions, while results are reviewed, and after any failure. `reproduce/gcp/run_session.sh` stops it on exit unless `--keep-running` is given (then `run_stages.sh` stops it after its last stage). Delete all VMs once all benchmark sessions are finished.

```sh
gcloud compute instances stop   "$INSTANCE" --zone="$ZONE"   # keeps the disk
gcloud compute instances delete "$INSTANCE" --zone="$ZONE"   # removes VM and boot disk
```

`reproduce/gcp/delete_vm.sh` wraps both. For the campaign the pairs are
`rbd-bench-c3d` / `us-central1-c`, `rbd-bench-c3d-2` / `us-east1-d`,
`rbd-bench-c3d-3` / `us-east4-c` and `rbd-bench-c4a` / `us-central1-a`; delete them only after
the user confirms.

A stopped VM no longer bills for vCPUs and memory, but its disk is still billed.
