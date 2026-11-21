# Authors' Archived Measurements (`results/`)

> **These are the authors' measurements.** They reproduce every table, figure, and derived number in the paper when processed with `reproduce/report.sh --archived`. Fresh evaluator runs write to `out/` by default and never overwrite this directory.

---

## 1. Hosts and Provenance

Exact hardware, cache topology, kernel, frequency governor, and compiler versions are recorded in machine-readable JSON files per host (summarized automatically in `tab_platforms` when running `reproduce/report.sh --archived`):

| Host label | Role | Selected sessions | Per-node hardware & toolchain records |
| :--- | :--- | :--- | :--- |
| `soc-xcnh` | Primary x86-64 host (paper `\xmach`) | `2026-11-13-96850764e1ac/{s1,s2,s3}` | `soc-xcnh/first-run-xcnh{0..3}.json`, `soc-xcnh/host-profile-xcnh{0..3}.json` |
| `soc-xcng` | Primary AArch64 host (paper `\amach`) | `2026-11-13-96850764e1ac/arm1` | `soc-xcng/first-run-xcng{0,1}.json`, `soc-xcng/host-profile-xcng{0,1}.json` |
| `gcp-c4a-highmem-16` | Archived second AArch64 host (paper `\vmach`) | `2026-10-23-375a6e18355a/arm1` | `gcp-c4a-highmem-16/first-boot-manifest*.json`, `gcp-c4a-highmem-16/host-profile*.json` |

### Sessions

- **`soc-xcnh/2026-11-13-96850764e1ac/s1` and `s2`**: Two complete independent x86-64 full-mode sessions (stages `A`, `H`, `S`, `C`, `T`, `T28`, `D`, `B`) collected on two distinct `xcnh` compute nodes to expose node-to-node variance.
- **`soc-xcnh/2026-11-13-96850764e1ac/s3`**: Supplementary x86-64 session measuring a subset of blocks (including separated PCS setup vs. input-generation timing).
- **`soc-xcng/2026-11-13-96850764e1ac/arm1`**: Complete AArch64 full-mode session (stage `ARM`, $\log_2 N \in \{18, 22, 26\}$, $T \in \{1, 8\}$; This Work and original LCPC only, as SIMD-Spielman requires AVX-512 IFMA).
- **Second Arm platform (archived, not re-measured)**: `gcp-c4a-highmem-16/2026-10-23-375a6e18355a/arm1/` holds one Arm session on a Google Cloud `c4a-highmem-16` virtual machine (Arm Neoverse-V2, 16 vCPUs, one thread per core, Debian 12, Linux 6.1.0-53-cloud-arm64, Rust 1.99.1): the four `arm_*` blocks at $\log_2 N \in \{18, 22, 26\}$, $T \in \{1, 8\}$, cores 0–7 ($T=1$ on core 1), measured at fork commit `375a6e1` with uncommitted changes in documentation and analysis scripts only (`git.code_repo_changed_paths` in the manifests). Between `375a6e1` and `9685076` the Rust sources differ only in comments and one test name, and `cargo tree -p lcpc-bench` resolves the same dependencies and features; `.cargo/config.toml` and the toolchain are unchanged. `scripts/analyze.py` reports it in T12 alongside `soc-xcng` (one row group per host) and in T0/T11; the paper prints it as `\vmach`. No further measurement on this platform is planned.

---

## 2. How the Measurements Were Collected

All published numbers except the archived second Arm platform were collected on a shared Slurm compute cluster using the exact campaign stages defined in `reproduce/stages.tsv` (driven by `reproduce/all.sh` with `--warmup 1` and a fixed `--run-date` per session). One session ran as one Slurm job on one node, executing its stages sequentially inside a job step pinned to whole physical cores with the `performance` frequency governor requested. On `soc-xcnh`, core selection picked 8 physical cores occupying a single L3 domain owned completely by the job (so no other job shared that L3 cache); on `soc-xcng`, where sysfs does not report an L3 cache, the first 8 allocated cores were selected and recorded with `cache_aware: false`.

Cluster-specific submission, sync, and fetch wrappers are kept outside this portable artifact. Evaluators reproduce the same stages on any Linux machine via `reproduce/run.sh`.

---

## 3. Session Directory Layout

One session is `results/<host>/<UTC-date>-<gitsha>/<session>/`. Staged campaigns invoke `reproduce/all.sh` once per stage in the same session directory.

| Path in the session | Content |
| :--- | :--- |
| `<block-id>/` | Raw JSON records and block manifest (`manifest.json`). With `--logn`, the directory name ends in `_n<list>` (e.g., `full_this_work_threads_t4_n20-24-26`). |
| `<block-id>.prev-<UTC time>/` | Previous output of a re-run block moved aside before starting the new run, preventing mixed records across stages. Analysis excludes `*.prev-*`. |
| `stages/<UTC time>_<stage>/` | Per-stage provenance: `schedule.json`, `blocks.tsv`, `manifest.json`, `execution_log.json` (duration, exit code, core clock, and pre/post node noise snapshots), plus copies of the node's host profile and first-run record. Never treated as a session. |
| `schedule.json` | Session-level append-only schedule (`rbd-session-schedule-v1`) listing all stages, arguments, seeds, commits, and per-block statuses. |
| `failed_blocks.tsv` | One line per failed block (if any): block id, duration, exit code, stage tag. |

Every JSON record in a block directory is tagged by `reproduce/all.sh` with `session_id`, `session_seed`, `stage`, `block_id`, `block_status`, and CPU frequency fields (`docs/bench-json-schema.md`).

---

## 4. `selection.json` Semantics

`results/selection.json` (`rbd-selection-v1`) restricts `scripts/analyze.py` to the curated runs and sessions that belong in the published tables, preventing stale, duplicate, or invalid data from silently mixing in:

- **`machines.<host>.runs`**: Lists allowed `run_dir` globs and `sessions` per host. Records outside the selection cause `analyze.py --check` to fail.
- **`exclude`**: Path globs (with reason and optional field `match`) dropped before aggregation, such as `*.prev-*` directories from re-run blocks.
- **`supersede`** (optional): Allows newer runs to replace older cells per cell key (unused in the archived selection).
- **`expected_rows`**: Specifies the required key combinations per table (Cartesian product of key columns, or a list of specs whose union is required, as in T6). Missing required rows fail `--check`.
- **`omissions`**: Documents intentional gaps (`kind`: `omitted`, `pending`, or `not-run`, with a reason), such as Orion-shape SIMD-Spielman at $N=2^{28}$ (`--max-orion-logn 26`) and T8 SIMD-Spielman setup/memory rows. Omissions never excuse integrity errors (duplicate cells, oversubscribed threads, or interrupted blocks), and unused omission rules produce a warning.

---

## 5. Limits and Noise Recording

- **Shared nodes**: Whole physical cores were allocated to each job (and an entire L3 domain on `soc-xcnh`), so no competing job ran on benchmark cores or within their L3 cache. Exclusive node access was unavailable, so neighbouring jobs on other cores could still share memory bandwidth, other L3 domains, and the socket power/boost budget.
- **Boost clocks**: The `performance` governor was requested for each job step, while turbo/boost remained a node-level setting; effective clocks could therefore vary with neighbouring-node load.
- **PMU counters on `soc-xcnh`**: `kernel.perf_event_paranoid` was set to 4 on `soc-xcnh`, disabling unprivileged `perf stat`. The campaign therefore did not pass `--perf`; per-node host profiles record PMU availability explicitly.
- **Noise records and randomization**: Before and after every block, `reproduce/all.sh` recorded node load average, allocated/total CPUs, running job count, and first-core clock in `stages/*/execution_log.json` (`noise_before`, `noise_after`). Two independent x86-64 sessions (`s1`, `s2`) on different nodes quantify node-to-node spread, and seeded random block ordering prevents transient load spikes from systematically biasing one implementation. Cells where session medians diverge by more than 5% are flagged in `session_divergence.csv` and marked with a dagger in the paper tables.

---

## 6. Compressed Codegen Reports (`*.json.xz`)

Uncompressed `codegen_report.json` files are roughly 18 MB each (dominated by annotated disassembly listings). To keep the repository compact, archived reports under `results/` are stored xz-compressed as `codegen_report.json.xz` (`xz -9`). `scripts/analyze.py` reads `*.json.xz` directly without manual extraction; running `xz -dk <file>` restores the exact original JSON byte for byte.
