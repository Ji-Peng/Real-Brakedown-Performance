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
reproduce/fetch_deps.sh
reproduce/test.sh
```

Or run a quick end-to-end evaluation (preflight, dependency fetch, C0 tests, selected benchmarks, and analysis):

```sh
reproduce/run.sh --list-claims
reproduce/run.sh --mode quick --claims C0,C2
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

POSIX `sh` scripts reproduce all experiments and baseline comparisons. Each runs the relevant correctness tests first, aborts on failure, and writes raw JSON plus a manifest under `out/<host>/<UTC-date>-<gitsha>/` (or `--out`). Benchmark cores are autodetected from the process CPU affinity mask and sysfs L3 topology (`reproduce/lib/cores.py`), so external pinning (`taskset`, container `--cpuset-cpus`) composes automatically.

- **`reproduce/run.sh`**: single portable entry point (`--mode quick|full`, `--claims all|C0,C2,...`, `--out DIR`, `--cores LIST`, `--max-threads N`, `--host-label NAME`, `--dry-run`, `--list-claims [--markdown]`). Uses `reproduce/claims.tsv` and `reproduce/stages.tsv` to run preflight, dependency fetch, C0 tests, selected stages, `scripts/make_selection.py`, and analysis.
- **`reproduce/preflight.sh`**: inspects architecture, AVX-512 IFMA, core/L3 topology, RAM, disk, tools, and `step/*` git tags, writing `preflight.json`.
- **`reproduce/fetch_deps.sh`**: idempotently fetches pinned `SIMD-Spielman-for-ZKPs`, `arkworks poly-commit`, Boost 1.86.0 headers (if missing on x86-64), and Cargo crates into `deps/`, writing `deps/env.sh`.
- **`reproduce/test.sh`**: runs the C0 correctness gate (`cargo test --workspace --release --locked` and the original-LCPC golden check).
- **`reproduce/this_work.sh`**: builds the fork HEAD, verifies correctness tests, runs the strict codegen check, then benchmarks. `--params rs-ligero` measures the same library's Ligero Reed–Solomon encoding (T10).
- **`reproduce/steps.sh`**: builds, tests and benchmarks each tag `step/01-modernize` … `step/09-codegen` in its own worktree with HEAD's harness.
- **`reproduce/lcpc_orig.sh`**: unmodified upstream LCPC `f0e2116` through `external/lcpc-orig` (golden vectors first). `LCPC_VARIANTS` selects build variants.
- **`reproduce/simd_spielman.sh`**: upstream C++ AVX-512 IFMA baseline (`7b399db`) built out of tree with `external/simd_spielman/params.patch`; needs `avx512ifma`.
- **`reproduce/polycommit.sh`**: arkworks PolyCommit Brakedown encoding with `external/polycommit/polycommit.patch`.
- **`reproduce/all.sh`**: randomized stage/block orchestrator (`--profile quick|full|arm`, `--blocks REGEX`, `--logn LIST`, `--warmup N`, `--lcpc-variants LIST`, `--stage NAME`, `--session ID`, `--seed N`, `--run-date YYYY-MM-DD`).

### Session directory layout

One session is `<out>/<host>/<UTC-date>-<gitsha>/<session>/`. Staged campaigns call `all.sh` several times for the same session directory; each call is one stage.

| Path in the session | Content |
| :--- | :--- |
| `<block-id>/` | The block's `--out` directory: raw JSON records and manifests. The block id equals the directory name; with `--logn` it ends in `_n<list>`, the logN values the block actually ran (for example `full_this_work_threads_t4_n20-24-26`). |
| `<block-id>.prev-<UTC time>/` | Before a block runs, `all.sh` moves an existing output directory of the same block aside under this name, so a re-run never mixes records of two stages. Analysis excludes these directories (`*.prev-*`). |
| `stages/<UTC time>_<stage>/` | One directory per stage: its `schedule.json`, `blocks.tsv`, `manifest.json`, `host-profile-<node>.json`, and `execution_log.json`. A stage never overwrites another stage's files. This is not a session; analysis never treats it as one. |
| `schedule.json` | Session-level and append-only (`rbd-session-schedule-v1`): its `stages` list accumulates every stage. |
| `failed_blocks.tsv` | One line per failed block: block id, duration, exit code, stage tag. |

`all.sh` tags every JSON record in a block's own output directory, failed blocks included, with `session_id`, `session_seed`, `stage`, `block_id`, `block_status` and the frequency fields (`docs/bench-json-schema.md`).

### Campaign stages (`reproduce/stages.tsv`)

The x86-64 full campaign runs stages `A`, `H`, `S`, `C`, `T`, `T28`, `D`, and `B` (`reproduce/run.sh --mode full --dry-run` prints the full stage list); AArch64 runs stage `ARM`. Quick mode runs `QA`, `QH`, `QS`, `QC`, `QT`, `QD` (or `QARM` on AArch64) with reduced sizes and repetitions.

---

## 5. Result Analysis and LaTeX Table Generation (`scripts/analyze.py`)

To re-analyze the authors' archived measurements in `results/` into CSVs, LaTeX tables and figures:

```sh
python3 scripts/analyze.py --results-dir results/ --selection results/selection.json \
    --out-dir analysis/ --check
```

`--selection` restricts the input to the run directories, sessions and blocks listed in `results/selection.json` (or a fresh run's `out/selection.json` generated by `scripts/make_selection.py`) and refuses records it does not cover. Without it, `--check` fails. A session is the directory that holds a session's block directories (the parent of `stages/`); `stages/` subdirectories and `*.prev-*` block directories are never sessions.

The strict check (`--check`) fails on:

- integrity errors: duplicate or mixed configurations in one cell, oversubscribed runs (threads > pinned cores), records not covered by the selection, and records of an **interrupted block**;
- any selected codegen report with status `FAIL` (vector arithmetic in a hot kernel), unless `selection.json` documents it as an omission;
- rows that `selection.json` `expected_rows` requires but a table lacks;
- empty cells, `-`, NaN and `SKIPPED` values in every table (`not run (<reason>)` from `not-run` omissions is accepted and logged in `todo_cells.csv`);
- generated LaTeX tables that do not compile with `pdflatex` (skipped with a warning if `pdflatex` is not installed).

Large `codegen_report.json` files in `results/` are stored xz-compressed as `codegen_report.json.xz` (`xz -9`); `analyze.py` reads `*.json.xz` directly, and `xz -dk <file>` restores the original byte for byte.

Outputs (under `--out-dir`):
- **CSV** (`csv/`) and **LaTeX** (`latex/`): `table_T0_correctness` through `table_T12_arm_portability` (`.csv` / `.tex`).
- **Figures** (`plots/`): `figure_F1_steps_breakdown`, `figure_F2_llc_scaling`, `figure_F3_thread_scaling` (`.pdf` and `.png`).

---

## 6. Archived Authors' Measurements (`results/`)

The authors' archived measurements in `results/` come from:

- **`results/soc-xcnh/`** (`s1`, `s2`, `s3`): x86-64 AMD EPYC 9755 (Zen 5, 32 MB L3 per 8-core domain, AVX-512 IFMA), 8 physical cores of one L3 domain, Ubuntu 26.04 LTS, Rust 1.99.1, g++ 15.2.0, CMake 4.2.3, Boost 1.86.0 headers.
- **`results/soc-xcng/`** (`arm1`): Ampere Arm Neoverse-N1 (128 cores, no SMT), 8 physical cores, Ubuntu 24.04.5 LTS, Rust 1.99.1.
- **`results/gcp-c4a-highmem-16/`** (`arm1`, archived at fork commit `375a6e1`): Google Cloud `c4a-highmem-16` virtual machine (Arm Neoverse-V2, 16 vCPUs, Debian 12, Rust 1.99.1), cores 0–7.

Each archived host directory includes per-node `host-profile-*.json` and `first-run-*.json` records documenting exact hardware, kernel, toolchain, frequency governor, boost state, PMU availability, and shared-node load snapshots.
