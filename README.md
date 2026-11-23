# Real Brakedown Performance (`RBP-LCPC`)

This repository is a fork of [LCPC](https://github.com/conroi/lcpc) by Riad S. Wahby
(Apache-2.0, see [`LICENSE`](LICENSE)), imported with its full git history at upstream commit
`f0e2116001af5a1bbf39481535f837d6a25772e8`. The original upstream README is preserved unchanged in
[`README-lcpc.md`](README-lcpc.md).

The fork integrates the scalar and software optimizations analysed in *High-Performance SIMD Software
for Spielman Codes in Zero-Knowledge Proofs* (Krieger, Dobrouschek, Hirner, Sinha Roy; TCHES 2026)
into LCPC's Brakedown (SDIG) encoder using **portable stable Rust only** (compiler-generated scalar
code, auto-vectorization disabled). Every optimization technique is recorded as one tagged commit
(`step/01-modernize` through `step/09-codegen`) on top of the import merge (`step/00-import`), so
the git history documents each step relative to upstream.

---

## 1. Kick the Tires (≈30 min)

### Option A: Reproduce every paper table from the authors' archived data (≈1–2 min)

Generates all analysis CSVs, all paper-format tables and figures, `claims.csv`, and `tables.pdf`
(or `tables.tex` if TeX is absent) directly from `results/`:

```sh
reproduce/report.sh --archived
```

Outputs land in `out/report-archived/` (`tables.pdf`, `claims.csv`, `paper/tab_*.tex`, `analysis/`).

### Option B: Quick functional run on your machine (native)

Preflight check, pinned dependency fetch, full C0 correctness & oracle gate, C1 scalar codegen
audit, and C2 headline encoding benchmarks at reduced quick-mode sizes, followed by report build:

```sh
reproduce/run.sh --mode quick --claims C0,C1,C2
```

### Option C: Quick functional run inside Docker (offline after image build)

Build from a full git clone (not a linked worktree where `.git` is a file; `.git` is required for
`step/*` tags). No Rust binaries are prebuilt in the image so `-C target-cpu=native` compiles for
your CPU at run time:

```sh
docker build -t rbd-artifact .
docker run --rm --network none --cpuset-cpus 0-7 -v "$PWD/out:/rbd/out" \
    rbd-artifact reproduce/run.sh --mode quick --claims C0,C1,C2
```

> **Container CPU pinning:** Always pin physical cores with `--cpuset-cpus` rather than limiting
> CPU quota with `--cpus`. Timing comparisons against SIMD-Spielman and original LCPC match native
> runs only when the container is not throttled by CFS CPU quotas.

---

## 2. Requirements

- **Architecture & hardware** (summarized from `reproduce/claims.tsv`; run `reproduce/run.sh --list-claims` to inspect your host):
  - **C0, C1**: any Linux x86-64 or AArch64 host.
  - **C2, C4–C10**: Linux x86-64; **≥ 8 physical cores** (preferably inside one L3 cache domain) for the $T=8$ points in full mode (quick mode caps $T$ to available physical cores).
  - **C3 (SIMD-Spielman)**: x86-64 with **AVX-512 IFMA** (`avx512ifma`), `g++`, `cmake`, and Boost headers.
  - **C11 (Arm portability)**: Linux **AArch64**.
  - **RAM**: quick mode ≥ 8 GB; full mode ≥ 64 GB (the $N=2^{28}$ stages `B` and `T28` peak around 34 GiB RSS and are dropped automatically with a documented `not-run` reason on hosts under 64 GB).
  - **Disk**: about 2 GB for quick mode (1.9 GB Cargo target directory, 95 MB outputs), plus the Rust toolchain.
- **OS**: Linux with `/sys` cache topology (`sysfs`) and `taskset` (`util-linux`).
- **Toolchain**: `rustup` and Rust `1.99.1` (installed automatically via `rust-toolchain.toml`),
  Python ≥ 3.11 with `matplotlib`, `git`, GNU `time`, `objdump` (`binutils`), and `g++`/`cmake`/Boost
  headers for C3. TeX (`latexmk` or `pdflatex` with `tabularray`, `threeparttable`, `booktabs`,
  `siunitx`, `pdflscape`, `cleveref`) is optional; without TeX, `reproduce/report.sh` writes
  `tables.tex` and skips PDF compilation cleanly.
- **Network**: needed once on a native host (`reproduce/fetch_deps.sh`) to fetch pinned baselines
  and Cargo crates; Docker builds prefetch everything so container runs work with `--network none`.

---

## 3. Claims to Commands Map

Generated from `reproduce/claims.tsv`, `scripts/claim_times.py`, and
`scripts/paper_reference/claims.csv` via `reproduce/run.sh --list-claims --markdown`. Full-mode
wall times are measured from the authors' archived `execution_log.json` files (full non-overlapping
session totals: `soc-xcnh/s1` 9.0 h, `soc-xcnh/s2` 8.8 h, `soc-xcng/arm1` 54 min); quick-mode times
were measured with one cold `reproduce/run.sh --mode quick` on an AMD EPYC 9755 host with 8 cores
(28.5 min for all x86-64 claims; C4 and C11 not measured there).

<!-- BEGIN CLAIMS TABLE -->
| ID | Paper location | Command | Output (CSV / paper table) | Full-mode time | Quick-mode time | Requirements | Expected result and tolerance |
| :--- | :--- | :--- | :--- | ---: | ---: | :--- | :--- |
| **C0** | §Correctness | `reproduce/run.sh --mode quick --claims C0` | T0 / tab_correctness | ~2 min (tests) | 3 min | any Linux | all PASS (hard: every row must PASS) |
| **C1** | §Codegen | `reproduce/run.sh --mode quick --claims C0,C1` | T11 / tab_codegen | included in C2/C11 | 3 min | any Linux | 0 vec arith, PASS (hard: 0 vector arithmetic and PASS) |
| **C2** | §Headline | `reproduce/run.sh --mode quick --claims C0,C2` | T1 / tab_headline | 3.0 h | 6 min | x86_64 | >1x (ref T=1 pub 8.14-9.06x, ff 6.89-7.63x, pc 3.95-5.80x) (directional: all measured speedups > 1 (strict: all faster)) |
| **C3** | §SIMD | `reproduce/run.sh --mode quick --claims C0,C3` | T2,T9 / tab_simd | 5.1 h | 24 min | x86_64,avx512ifma,boost,cmake | >1x (ref 1.59-4.44x); Brakedown MT not slower than T=1 (directional: SIMD faster at T=1 (N<=2^26) and MT <= T=1) |
| **C4** | §Steps | `reproduce/run.sh --mode quick --claims C0,C4` | T3,F1 / tab_steps | 1.0 h | not measured | x86_64,git-tags | >1x (ref a=6.21x, b=2.12x) (directional: cumulative speedup > 1 at HEAD) |
| **C5** | §Slicing | `reproduce/run.sh --mode quick --claims C0,C5` | T4,F2 / tab_slices,fig_llc | 21 min | 4 min | x86_64 | >=1x (ref T=1 1.02-1.04x, T=8 1.07-1.43x) (directional: s=1/best >= 1 and adaptive close to best) |
| **C6** | §Threads | `reproduce/run.sh --mode quick --claims C0,C6` | F3 / fig_threads | 2.3 h | 5 min | x86_64,cores>=8(full) | >1x (ref T=8 7.09-7.57x, eff 0.89-0.95) (directional: S(Tmt) > 1) |
| **C7** | §Fields | `reproduce/run.sh --mode quick --claims C0,C7` | T5 / tab_fields | 47 min | 6 min | x86_64 | >1x (ref 2^20 1.25x) (directional: Mont127 / Fp2M61 > 1) |
| **C8** | §PCS | `reproduce/run.sh --mode quick --claims C0,C8` | T6,T7 / tab_pcs | 3.1 h | 7 min | x86_64 | >1x (ref T=1 5.21-5.95x, T=8 4.63-5.09x) (directional: commit speedup > 1 and hash share rises) |
| **C9** | §Setup | `reproduce/run.sh --mode quick --claims C0,C9` | T8 / tab_setup | 5.1 h | 6 min (24 min with IFMA) | x86_64 | PCS setup < Input gen. (ref PCS/enc 0.00-0.03x, ratio 0.69-0.76x) (directional: PCS setup < Input gen.) |
| **C10** | §Ligero | `reproduce/run.sh --mode quick --claims C0,C10` | T10 (artifact CSV only) | 47 min | 6 min | x86_64 | >1x (directional: RS / Brakedown > 1) |
| **C11** | §Arm | `reproduce/run.sh --mode quick --claims C0,C11` | T12 / tab_arm | 54 min | not measured | aarch64 | >1x (ref T=1 pub 3.14-3.24x, ff 2.84-2.93x) (directional: Arm speedup > 1) |
<!-- END CLAIMS TABLE -->

---

## 4. Running Experiments

### Single entry point (`reproduce/run.sh`)

```sh
reproduce/run.sh [--mode quick|full] [--claims all|C0,C2,...] [--out DIR] \
                 [--cores LIST] [--max-threads N] [--host-label NAME] \
                 [--session ID] [--seed N] [--no-report] [--dry-run] \
                 [--list-claims [--markdown]] [--keep-going]
```

Execution sequence: `preflight.sh` → `fetch_deps.sh` → `test.sh` (C0 gate; aborts with exit 3 on
failure) → selected stages via `all.sh` → `scripts/make_selection.py` → `reproduce/report.sh`.

- **Modes**: `--mode quick` (default) runs stages `QA`–`QD` / `QARM` from `reproduce/stages.tsv` at
  reduced sizes ($\log_2 N \le 22$, 2–3 runs); `--mode full` reproduces the paper's full stage set
  (`A,H,S,C,T,T28,D,B` on x86-64; `ARM` on AArch64). Use `--dry-run` to preview the resolved plan.
- **Output layout** (under `--out`, default `./out`):
  - `out/<label>/<UTC-date>-<gitsha>/<session>/<block>/…` — raw JSON records and manifests;
  - `out/selection.json` — generated selection file documenting run scope and `not-run` omissions;
  - `out/report/{analysis/,paper/,claims.csv,tables.tex,tables.pdf}` — CSVs, paper tables, verdict,
    and compiled PDF.
- **Rerunning a single stage**: call `reproduce/all.sh` with the same `--out`, `--host-label`,
  `--session`, and `--run-date` plus `--stage <NAME> --blocks <REGEX> --logn <LIST>`. Existing
  block directories are moved aside to `<block>.prev-<UTC>` and excluded from analysis.
- **Shared machines**: pass `--cores <list>` or restrict process affinity externally (`taskset -c ...`
  or Docker `--cpuset-cpus`). `reproduce/lib/cores.py` autodetects physical cores and L3 domains
  strictly inside the process CPU affinity mask and never uses SMT siblings.
- **Custom host profiles**: pass `--host-profile <file>` to block scripts or set `RBD_HOST_PROFILE`
  to supply a machine-specific shell snippet defining `BENCH_CORE_SINGLE`, `BENCH_CORES_MULTI`, and
  `HOST_LABEL`.

---

## 5. Expected Output and Tolerances

- **Hard gates (C0 and C1)**: Every correctness check (T0) must `PASS`, and every scalar codegen
  audit (T11) must report `0` vector arithmetic instructions and `PASS`. Any failure in C0 or C1
  causes `reproduce/report.sh` to exit `1`.
- **Directional performance claims (C2–C11)**: Evaluated directionally against each claim's
  `tolerance_rule` in `claims.csv` (speedup $> 1\times$ where the paper claims a speedup;
  SIMD-Spielman faster at $T=1$ for $N \le 2^{26}$ and not slower multithreaded on Brakedown; PCS
  setup smaller than input generation). Claims report `PASS`, `DIFFERS`, or `NOT-RUN` with reasons.
- **Expected magnitudes**: Exact timings and speedups vary with CPU microarchitecture, LLC capacity,
  and memory bandwidth. On an x86-64 host similar to the authors' primary machine (Zen 4/5 EPYC
  with 32 MB L3 per 8 cores), observed ranges should stay close to the archived ranges. Both
  `claims.csv` and `tables.pdf` print your observed range alongside the archived paper range read
  directly from `scripts/paper_reference/derived.csv`.
- **Quick mode**: Quick-mode cells use smaller sizes and fewer repetitions than the paper campaign
  and are intended for functional validation rather than one-to-one numerical comparison.

---

## 6. Authors' Archived Measurements

All published paper numbers come from the archived raw data in `results/` (`soc-xcnh`, `soc-xcng`,
and the archived `gcp-c4a-highmem-16` session). See [`results/README.md`](results/README.md) for
host records, session structure, `selection.json` semantics, collection methodology, noise logging,
and `.json.xz` codegen compression. Run `reproduce/report.sh --archived` to regenerate every paper
table and `tables.pdf` from `results/`.

---

## 7. Repository Layout, Benchmark Binaries, and Build Policy

### Layout

| Path | Description |
| :--- | :--- |
| `lcpc-fields/`, `lcpc-2d/`, `lcpc-brakedown-pc/`, `lcpc-ligero-pc/`, `lcpc-test-fields/` | Portable scalar Rust workspace crates (modified LCPC fork) |
| `lcpc-oracle-tests/`, `lcpc-spielman-oracle/` | Bit-exactness oracles against upstream LCPC `f0e2116` and SIMD-Spielman test vectors |
| `bench/` | Stable benchmark binaries (`lcpc-bench`, `bench_encode`, `bench_pcs`, `bench_fields`) |
| `external/` | Pinned comparison harnesses: `lcpc-orig` (LCPC `f0e2116`), `simd_spielman` (`7b399db` + `params.patch`), `polycommit` (`1eaec50` + `polycommit.patch`) |
| `reproduce/` | Portable POSIX `sh` entry points (`run.sh`, `report.sh`, `preflight.sh`, `fetch_deps.sh`, `test.sh`, `all.sh`, per-baseline scripts, `claims.tsv`, `stages.tsv`, `lib/cores.py`) |
| `scripts/` | Analysis and reporting (`analyze.py`, `paper_tables.py`, `check_codegen.py`, `claim_times.py`, `make_selection.py`, `paper_reference/`, unit tests) |
| `docs/` | Detailed documentation (`bench-json-schema.md`, `CODEGEN.md`, `BLOCKING.md`, `CODEGEN-TUNING.md`, `ORACLES.md`, `STEPS-METHOD.md`, `THREADS.md`, `FIELDS.md`) |
| `results/` | Authors' archived raw measurements and `selection.json` |

### Benchmark binaries (`bench/`)

All four binaries emit `lcpc-bench-v1` JSON (`docs/bench-json-schema.md`). Prefer `reproduce/run.sh`
for pinned evaluation; for ad-hoc runs, pin to a physical core with `taskset -c <core>`:

```sh
cargo run --release --bin lcpc-bench -- \
    --field mont127 --params lcpc-sdig6 --logN 16,18,20 --threads 1 --slices adaptive --runs 5 --warmup 1 --output out/bench.json
cargo run --release --bin bench_encode -- \
    --field mont127 --params lcpc-sdig6 --logN 16,18,20 --slices adaptive --output out/encode.json
cargo run --release --bin bench_pcs -- \
    --field mont127 --params lcpc-sdig6 --logN 16,18,20 --slices adaptive --output out/pcs.json
cargo run --release --bin bench_fields -- \
    --field all --runs 100000 --output out/fields.json
```

### Build policy and codegen verification

- **Toolchain**: Pinned stable Rust `1.99.1` (`rust-toolchain.toml`, edition 2021).
- **Scalar-only policy**: `.cargo/config.toml` sets `-C no-vectorize-loops -C no-vectorize-slp`
  workspace-wide and `-C target-cpu=native` on x86-64 (enabling scalar instructions such as `mulx`
  and `adc`). `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS` must remain unset.
- **Portable hashing**: `blake3` is configured with `features = ["pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]`.
- **Disassembly check**: `scripts/check_codegen.py` disassembles the release binary and verifies
  zero vector arithmetic instructions in hot kernels (`docs/CODEGEN.md`):
  ```sh
  python3 scripts/check_codegen.py target-harness/release/lcpc-bench \
      --output-json out/codegen_report.json --output-listing out/disassembly.txt
  ```

---

## 8. License and Notices

- Licensed under the Apache License, Version 2.0 ([`LICENSE`](LICENSE)).
- Full modification inventory, upstream history (`f0e2116001af5a1bbf39481535f837d6a25772e8`),
  embedded `fffft` attribution, and third-party baseline licenses are documented in
  [`NOTICE`](NOTICE).
- The original upstream LCPC README is preserved unchanged in [`README-lcpc.md`](README-lcpc.md).
