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

POSIX `sh` scripts reproduce all experiments and baseline comparisons. Each runs the relevant correctness tests first, aborts on failure, and writes raw JSON plus a manifest under `results/<host>/<UTC-date>-<gitsha>/`. Defaults are `--runs 5 --warmup 2`; the cluster campaign (`reproduce/soc/run_stages.sh`) passes `--warmup 1` to every block. The examples below are local smoke runs (never publish local numbers; never use CPU 2 on the workstation).

- **`reproduce/this_work.sh`**: builds the fork HEAD, runs the tests, runs the strict codegen check, then benchmarks. `--params rs-ligero` measures the same library's Ligero Reed–Solomon encoding (E12/T10).
  ```sh
  ./reproduce/this_work.sh --threads 1 --cores 3 --logN 16,18,20 --runs 5 --warmup 1
  ```
- **`reproduce/steps.sh`**: builds, tests and benchmarks each tag `step/01-modernize` … `step/09-codegen` in its own worktree with HEAD's harness (cumulative, order-dependent). `--mode a|b|both` selects the code-parameter mode (default `both`); the full profile's two steps blocks pass `--mode a` and `--mode b`.
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
- **`reproduce/all.sh`**: the experiment catalogue as a randomized block schedule. `--profile smoke|full|arm` selects the catalogue; `--blocks REGEX`, `--logn LIST`, `--warmup N` and `--lcpc-variants LIST` restrict a run to one stage; `--stage NAME` labels it; `--session` and `--seed` name the session and the block order; `--run-date YYYY-MM-DD` fixes the date part of the run id (`reproduce/soc/run_stages.sh` passes its submission date, so a session that crosses midnight UTC stays in one run directory). A failed block is recorded in `failed_blocks.tsv`, the stage continues, and `all.sh` exits 1 at the end.
  ```sh
  ./reproduce/all.sh --profile smoke --session local1
  ```
- **`reproduce/soc/`**: the SoC Slurm cluster campaign (`setup.sh`, `sync.sh`, `run_stages.sh` with `job.sh`, `fetch.sh`); see "Campaign on the SoC cluster" below.

### Session directory layout

One session is `results/<host>/<UTC-date>-<gitsha>/<session>/`. Staged campaigns call `all.sh` several times for the same session directory; each call is one stage.

| Path in the session | Content |
| :--- | :--- |
| `<block-id>/` | The block's `--out` directory: raw JSON records and manifests. The block id equals the directory name; with `--logn` it ends in `_n<list>`, the logN values the block actually ran (for example `full_this_work_threads_t4_n20-24-26`). |
| `<block-id>.prev-<UTC time>/` | Before a block runs, `all.sh` moves an existing output directory of the same block aside under this name, so a re-run never mixes records of two stages. Analysis excludes these directories (`results/selection.json`, path glob `*.prev-*`). |
| `stages/<UTC time>_<stage>/` | One directory per stage: its `schedule.json`, `blocks.tsv`, `manifest.json`, `execution_log.json` (per block: duration, exit code, clock of the first pinned core and, on the cluster, a noise snapshot before and after), and copies of the node's host profile and first-run record. A stage never overwrites another stage's files. This is not a session; analysis never treats it as one. |
| `schedule.json` | Session-level and append-only (`rbd-session-schedule-v1`): its `stages` list accumulates every stage (stage tag and directory, the `all.sh` arguments: `--blocks` filter, logN list, warm-up, LCPC variants; seed, commit, host name, block ids with per-block status, exit status). |
| `failed_blocks.tsv` | One line per failed block: block id, duration, exit code, stage tag. |

`all.sh` tags every JSON record in a block's own output directory, failed blocks included, with `session_id`, `session_seed`, `stage`, `block_id`, `block_status` and the frequency fields (`docs/bench-json-schema.md`).

### Campaign on the SoC cluster

All published numbers come from the NUS SoC Slurm cluster (machines in "Benchmark machines" below). One session is one Slurm job on one node; its stages run one after another inside that job. The x86-64 campaign has two sessions, `s1` and `s2`, on two different `xcnh` nodes; the Arm portability campaign has one session, `arm1`, on an `xcng` node.

The workstation reaches the login node through `ssh`; `RBD_LOGIN` (default `xlogin.comp.nus.edu.sg`, e.g. `user@xlogin.comp.nus.edu.sg`) and `RBD_SSH` (default `ssh`; for password login e.g. `sshpass -f ~/.soc.pass ssh -o PubkeyAuthentication=no`) select it, and `RBD_REMOTE_ROOT` (default `rbd`) is the working directory below the cluster home. The cluster home is shared by all nodes, x86-64 and aarch64, so everything lives under `~/rbd/`: the clone `code/`, the pinned upstream checkout `SIMD-Spielman-for-ZKPs/` (the fork's tests read its test vectors), one rustup/Cargo home per architecture in `toolchain/<arch>/`, the Boost headers in `deps/`, one Cargo target directory per session in `target/<class>-<session>/`, and the job files and logs in `jobs/`.

| Script | Runs on | What it does |
| :--- | :--- | :--- |
| `reproduce/soc/sync.sh` | workstation | Sends HEAD and all tags as a git bundle and checks out the same commit, detached, in `~/rbd/code`. Only committed changes are copied; run directories are named after this commit. Results already in the clone are kept. It refuses to move the clone while an `rbd-*` job is queued or running (`--force` overrides). |
| `reproduce/soc/setup.sh --node-class xcnh\|xcng` | login node | Re-runs itself through `srun` on a node of that class (the login node never builds). Installs rustup and the toolchain of `rust-toolchain.toml` into `toolchain/<arch>/`, checks out SIMD-Spielman `7b399db`, unpacks Boost 1.86.0 headers (SHA-256 pinned) on `xcnh` when the system has none and points `SIMD_SPIELMAN_EXTRA_INCLUDE` at them (`deps/simd-spielman.env`), and writes `results/soc-<class>/first-run-<node>.json`: CPU, caches and L3 domains, governor, boost, PMU access, OS, kernel, glibc, Python, Rust, g++, CMake, objdump, Boost and a list of checks; it exits 1 if a check fails. |
| `reproduce/soc/run_stages.sh` | login node | Submits one session as one Slurm job (`sbatch`): `--partition long`, `--constraint=<class>`, `-c 16 --threads-per-core=1`, `--mem=160G`, `--cpu-freq=Performance`, `--no-requeue`, time limit 2 days on `xcnh` and 12 h on `xcng`, and `--nodelist` when given. Stage specs `NAME\|REGEX\|LOGN[\|lcpc=LIST]` select `all.sh --blocks`, `--logn` and `--lcpc-variants` (an empty regex or logN list means no filter); `--preset x86\|arm` appends the campaign stages below; `--print-stages` and `--dry-run` show the stage list and the `sbatch` command without submitting. Every stage gets `--warmup 1` and the submission date as `--run-date`. |
| `reproduce/soc/job.sh` | compute node | The batch script. It starts the session as a job step (`srun --cpu-freq=Performance`; Slurm applies the frequency request to job steps only), checks that the clone is still at the submitted commit, writes the node's first-run record (`setup.sh --record-only`) and runs each stage as `all.sh --host soc-<class>`. The log `~/rbd/jobs/rbd-<session>-<job id>.out` ends every stage with `STAGE_<NAME>_EXIT=<code>`. |
| `reproduce/soc/fetch.sh` | workstation | Copies whole session directories (`--host`, `--run`, `--session`; globs allowed) and the host's per-node records into `results/` (or `--dest`); `--list` lists the sessions on the cluster. The codegen disassembly listings stay on the cluster unless `--with-listings`; every `codegen_report.json` is stored as `codegen_report.json.xz` unless `--no-xz`. |

The x86-64 sessions run the `full` profile in these stages (`run_stages.sh --preset x86`; LCPC variants `as-published-as-is,fork-flags-portable` unless stated):

| Stage | `--blocks` | log2 N | Content |
| :--- | :--- | :--- | :--- |
| A | `^full_(this_work_mode\|this_work_ft127\|lcpc_orig_mode\|polycommit\|ligero)` | 16–26 | This Work modes (a) and (b) (three fields, both shapes), This Work over Ft127 (2^20, 2^24), original LCPC, PolyCommit, Ligero |
| H | `^full_lcpc_orig_mode_a_t[18]$` | 20, 24 | original LCPC `as-published-portable` and `fork-flags-as-is`, which separate the BLAKE3 backend from the compiler flags in T6 |
| S | `^full_simd_spielman` | 16–26 | SIMD-Spielman, three fields, T = 1, 7 (+ coordinator), 8, Brakedown and Orion shapes |
| C | `^full_slice_sweep` | 20, 24, 26 | slice sweep s = 1 … 256, T = 1 and 8 |
| T | `^full_(this_work\|lcpc_orig)_threads_t[0-9]+$` | 20, 24, 26 | thread scaling T = 1, 2, 4, 8, This Work and original LCPC in the same job |
| T28 | same | 28 | the same at 2^28 |
| D | `^full_steps` | 16, 20, 24 | per-technique breakdown, modes (a) and (b) |
| B | `^full_(this_work_mode\|lcpc_orig_mode\|polycommit\|simd_spielman)` | 28 | the headline, SIMD-Spielman (Brakedown shape only; Orion above 2^26 is omitted by `simd_spielman.sh --max-orion-logn 26`) and PolyCommit at 2^28 |

The Arm session runs the `arm` profile in one stage, `ARM` (`^arm_`, log2 N 18, 22, 26: This Work and original LCPC, T = 1 and 8). SIMD-Spielman needs AVX-512 IFMA and therefore runs on `xcnh` only.

`python3 scripts/estimate_campaign.py` estimates each stage from block durations measured on an 8-core AMD Zen 4 server and an Arm Neoverse-V2 server, scaled by the speed of the cluster nodes measured in smoke runs (`xcnh` about 1.3 times the Zen 4 server, `xcng` about 0.3 times the Neoverse-V2 server): about 10 h for an x86-64 session (most of it the SIMD-Spielman Orion graph generation in stage S and the 2^28 stages T28 and B) and about 1.2 h for `arm1`. The time limits (2 days, 12 h) leave a wide margin.

Commands, in order (`sync.sh` and `fetch.sh` on the workstation, the rest on the login node in `~/rbd/code`):

```sh
# Workstation: copy the committed HEAD to the cluster
./reproduce/soc/sync.sh

# Login node, once per node class (and after a toolchain change)
cd ~/rbd/code
sh reproduce/soc/setup.sh --node-class xcnh
sh reproduce/soc/setup.sh --node-class xcng

# Login node: one job per session; s1 and s2 on two different idle xcnh nodes
sinfo -N -p long -n xcnh1,xcnh2,xcnh3 -o '%N %t %C'
sh reproduce/soc/run_stages.sh --session s1 --preset x86 --nodelist xcnh1
sh reproduce/soc/run_stages.sh --session s2 --preset x86 --nodelist xcnh2
sh reproduce/soc/run_stages.sh --session arm1 --preset arm
squeue -u "$USER"; tail -f ~/rbd/jobs/rbd-s1-<job id>.out

# Workstation, after the jobs have ended
./reproduce/soc/fetch.sh --host soc-xcnh --session 's[12]'
./reproduce/soc/fetch.sh --host soc-xcng --session arm1
python3 scripts/analyze.py --results-dir results/ --selection results/selection.json --out-dir analysis/ --check
```

Before submitting, `run_stages.sh --preset x86 --print-stages` lists the stages, and `all.sh --dry-run` inside a short job prints the block schedule with the derived cores (give it its own `--out` directory outside `results/`, because `analyze.py --selection … --check` refuses records that the selection does not cover). A stage that failed can be repeated alone by submitting the same session on the same node (`--nodelist`) with that stage spec and the original `--run-date`: `all.sh` moves the earlier output of each re-run block aside to `<block>.prev-<UTC>`, which analysis skips.

`results/selection.json` selects sessions `s1`, `s2` on `soc-xcnh` and `arm1` on `soc-xcng`, each through the run-directory glob `20[0-9][0-9]-[0-9][0-9]-[0-9][0-9]-*`, so the new run directories count as soon as they are fetched. If more than one run directory of a host is fetched, the same cells appear twice and `analyze.py --check` reports duplicates; keep one run per host, or narrow the glob.

---

## 5. Result Analysis and LaTeX Table Generation (`scripts/analyze.py`)

To aggregate the selected raw JSON results into CSVs, LaTeX tables and figures:

```sh
python3 scripts/analyze.py --results-dir results/ --selection results/selection.json \
    --out-dir analysis/ --check
```

`--selection` restricts the input to the run directories, sessions and blocks listed in `results/selection.json` and refuses records it does not cover. Without it, every JSON under `--results-dir` is read, which mixes superseded and invalid data. A session is the directory that holds a session's block directories (the parent of `stages/`); `stages/` subdirectories and `*.prev-*` block directories are never sessions (see "Session directory layout").

The strict check (`--check`) fails on

- integrity errors: duplicate or mixed configurations in one cell, oversubscribed runs (threads > pinned cores), records not covered by the selection, and records of an **interrupted block** (untagged partial output of a block whose latest run never completed: its earlier output was moved aside, or its stage was killed before `all.sh` finalized it); re-run such a block or exclude it. Records of a block that completed with a failure only produce a warning;
- any selected codegen report with status `FAIL` (vector arithmetic in a hot kernel), unless `results/selection.json` documents it as an omission with the selector `T11|<arch> @ <fork>,hot_kernels (aggregate)|Scalar Status` (campaign reports) or `T3|codegen,<tag>|Scalar Status` (the per-step reports, `<tag>` = `step/01-modernize` … `HEAD`); T0, T3 and T11 take their PASS/FAIL from these reports;
- rows that `results/selection.json` `expected_rows` requires but a table lacks (`{TABLE: {key column header: [values]}}`, the cartesian product of the listed values, e.g. T6 `Implementation` × `T` × `log2(N)`; a table's entry may also be a list of such specs, whose required rows are the union of their products, as T6 uses for the stage-RH variants), reported as `TABLE|v1,v2,…|(row)`;
- empty cells, `-`, NaN and `SKIPPED` values in every table;
- generated LaTeX tables that do not compile: with `--check`, each table under `latex/` is compiled standalone with `pdflatex` (booktabs, threeparttable) in `--latex-build-dir` (default `<out-dir>/latex_build`). If `pdflatex` is not installed, the compile step is skipped with a warning; `--no-latex-compile` skips it on purpose.

`results/selection.json` also defines which records count: `machines.<host>.runs` (a `run_dir` may be a glob, inert until a matching directory exists), `exclude` (path globs with a reason and an optional `match` on record fields), and optionally `supersede` (newer runs replace older records of the cells they re-measured, per cell; the current selection has none). The `omissions` (`kind` `pending` or `omitted`, with a reason) apply only to missing or N/A cells (and to absent `expected_rows` rows and documented `FAIL` codegen reports, through the selectors above), such as Orion-shape SIMD-Spielman at 2^28 ("omitted (documented)"); they never excuse an integrity error, and an omission that matches no cell produces a warning. Repeatable `--allow 'TABLE|ROW|COLUMN'` globs select further documented exceptions; smoke runs pass the known local limits in
`reproduce/all.sh` (Arm is not run locally, and the smoke subset omits the campaign's larger field measurements).
Run `python3 scripts/analyze.py --self-test` to check the missing-cell rules.

Runs write plain `codegen_report.json` (about 18 MB each, mostly disassembly listings); `reproduce/soc/fetch.sh` stores each one xz-compressed as `codegen_report.json.xz` (`xz -9`) under `results/`. `analyze.py` reads `*.json.xz` like `*.json`, and `xz -dk <file>` restores the original byte for byte.

Outputs (under `--out-dir`):
- **CSV** (`csv/`) and **LaTeX** (`latex/`): `table_T0_correctness`, `table_T1_headline_encoding`, `table_T2_simd_spielman`, `table_T3_steps_breakdown`, `table_T4_slice_sweep`, `table_T5_field_comparison`, `table_T6_pcs_breakdown`, `table_T7_verifier_encoding`, `table_T8_setup_memory`, `table_T9_orion_shape`, `table_T10_brakedown_vs_rs`, `table_T11_codegen_evidence`, `table_T12_arm_portability` (`.csv` / `.tex`).
- **Figures** (`plots/`): `figure_F1_steps_breakdown`, `figure_F2_llc_scaling`, `figure_F3_thread_scaling` (`.pdf` and `.png`).

The paper's tables and figures are copied from these outputs into the paper repository's `paper/generated/` (`csv/`, `latex/`, `plots/`); no number is entered by hand.

## Benchmark machines (SoC cluster)

All published numbers come from two node classes of the NUS School of Computing Slurm cluster. The local workstation is used only for building and testing; no published number comes from it. The scripts in `reproduce/soc/` run the commands of "Campaign on the SoC cluster" (§4); each run records the node it actually used.

| Setting | x86-64 sessions `s1`, `s2` | Arm session `arm1` |
| :--- | :--- | :--- |
| Node class (`--constraint`) | `xcnh` (four nodes, `xcnh1`–`xcnh3` in partition `long`) | `xcng` (two nodes, `xcng1` in partition `long`) |
| Host profile, results directory | `soc-xcnh` | `soc-xcng` |
| CPU | 2 × AMD EPYC 9755 (Zen 5), 128 cores per socket, SMT 2; AVX-512 including IFMA | Ampere, Arm Neoverse-N1, 128 cores, 1 socket, no SMT, up to 3.0 GHz |
| Cache topology | L3 of 32 MB shared by 8 physical cores (e.g. CPUs 0–7 and their siblings 256–263) | per-core L1 and L2; sysfs exposes no level-3 cache |
| Memory | 1 TB per node; the job requests 160 GB | 1 TB per node; the job requests 160 GB |
| Job shape | 16 physical cores (`-c 16 --threads-per-core=1`), i.e. two whole L3 domains | 16 cores (`-c 16`) |
| Benchmark cores | the 8 physical cores of one L3 domain that the job owns entirely, one hardware thread each; T = 1 on the first of them that is not CPU 0 | the first 8 cores of the job |
| Frequency policy | `amd-pstate-epp`, governor `performance` (node default; `--cpu-freq=Performance` requests it); boost on | `cppc_cpufreq`, governor `ondemand` by default, set to `performance` for the job step by `--cpu-freq=Performance`; boost reported off |
| PMU counters | unavailable (`kernel.perf_event_paranoid` = 4) | available (`kernel.perf_event_paranoid` = -1) |
| OS, kernel | Ubuntu 26.04 LTS, Linux 7.0.0-28-generic | Ubuntu 24.04.5 LTS, Linux 6.8.0-139-generic |
| Compilers | Rust 1.99.1 (`rust-toolchain.toml`), g++ 15.2.0, CMake 4.2.3, Boost 1.86.0 headers (`setup.sh`), GNU objdump | Rust 1.99.1, g++ 13.3.0, CMake 3.28 |
| Used for | All implementations (This Work, original LCPC, PolyCommit, SIMD-Spielman), two sessions on two different nodes | This Work and original LCPC only (SIMD-Spielman needs AVX-512), one session, log2 N in {18, 22, 26} |

The table lists the state of the cluster when the tooling was set up; the first-run record of each node (`results/<host>/first-run-<node>.json`, also copied into every stage directory) records the versions a run actually used.

### Core selection

The host profiles (`reproduce/hosts/soc-xcnh.sh`, `soc-xcng.sh`, both calling `reproduce/soc/host_profile.py`) derive the benchmark cores from what the job may use, never from all online CPUs of the shared node: the usable CPUs are the job step's CPU affinity, and the owned CPUs are its cgroup cpuset, which also lists the SMT siblings of the allocated cores. On `xcnh` the profile selects the first L3 domain whose CPUs, siblings included, all belong to the job, so no other job runs inside that L3; it stops with an error if the job owns no such domain (on a busy node, pick another one with `--nodelist`). On `xcng`, which reports no L3 in sysfs, it takes the first 8 usable cores and records that the choice is not cache-aware. T-thread blocks use the first T benchmark cores for every implementation (SIMD-Spielman gets T + 1 for its coordinator thread, at most 8). The other 8 cores of the job run no benchmark thread (on `xcnh` they are the job's second L3 domain); SIMD-Spielman's upstream test programs use all 16.

Each profile load writes `results/<host>/host-profile-<node>.json`: host name, Slurm job and step, partition, CPU model, sockets, cores and threads per core, the usable and owned CPU lists, all L3 domains with the selected one, the benchmark cores with their NUMA node, governor, driver and current clock of each benchmark core, the boost flag, the node's allocated and total CPUs and CPU load from `scontrol show node`, the number of running jobs on the node, the load average, and whether `perf stat` is available. `all.sh` copies it into each stage directory. The run manifests record the Slurm job (`machine_identity`) and the governor and clock of the pinned cores.

### Limits and how noise is recorded

- **Shared nodes.** Slurm allocates whole cores (`CR_CORE_MEMORY`), so no other job runs on the benchmark cores or, on `xcnh`, inside their L3 domain. Exclusive node access is refused by the cluster (`--exclusive`), so other jobs can still run on the other cores of the node and share memory bandwidth, the other L3 domains, and the socket's power and boost budget.
- **Boost cannot be disabled.** The job sets the governor of its cores (`--cpu-freq=Performance`, applied to the job step); turbo/boost is a node setting. Clocks therefore vary with the load of neighbouring jobs.
- **No PMU counters on `xcnh`.** `kernel.perf_event_paranoid` is 4, so unprivileged `perf stat` fails. The campaign does not use the benchmark binaries' optional `--perf` flag; the host profile and the first-run record state `perf: unavailable` with the reason.
- **Per-user limits.** The cluster caps one user at 256 CPUs and 16 running jobs; the three sessions use 48 CPUs.
- **Noise records.** Before and after every block, `all.sh` stores a snapshot in the stage's `execution_log.json` (`noise_before`, `noise_after`: load average, allocated and total CPUs of the node, CPUs held by this job, `scontrol` CPU load, running jobs on the node), next to the clock of the block's first pinned core. Two sessions on two different nodes give the node-to-node spread; within a session, blocks run in a seeded random order, so slow periods do not fall on one implementation only.
