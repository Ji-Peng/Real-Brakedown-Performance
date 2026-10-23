<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# LCPC benchmark JSON schema (`lcpc-bench-v1`)

The benchmark driver (`lcpc-bench` in `bench/`, as well as `external/lcpc-orig` and `reproduce/simd_spielman.sh`)
emits a JSON array containing one `lcpc-bench-v1` object per evaluated configuration.
The same top-level and per-case shape is shared across the fork's benchmark
harness and baseline drivers so raw results can be analyzed together
without conflating implementation names or build settings.

Each report contains:

| Field | Meaning |
| --- | --- |
| `schema_version` | Literal `lcpc-bench-v1`. |
| `implementation`, `upstream_commit`, `driver_version` | Implementation identity (`lcpc-fork`, `lcpc-original-upstream`, `simd-spielman`, `polycommit`) and pinned upstream commit. |
| `shape` | Matrix shape: `"brakedown"` (square $k \approx \sqrt{N}$) or `"orion"` (128 rows, $k = 128$). |
| `status` | Run status. Values by writer: the fork's `lcpc-bench` and the PolyCommit driver write `"complete"` for a successful case, and `lcpc-bench`'s supervisor writes `"failed"`, `"timeout"` or `"oom"` for a worker that did not finish; `reproduce/simd_spielman.sh` writes `"ok"`, `"failed"` or `"omitted"`; the original-LCPC driver (`external/lcpc-orig`) writes no `status` field (a record exists only for a finished case). `analyze.py` treats a missing status, `"ok"` and `"complete"` as measured. `failed` is any other error (worker crash, unparsable output, missing samples; the campaign's Orion-shape SIMD-Spielman 2^28 cases, stopped by an external watcher, have this status). `omitted` is a case deliberately not run, such as Orion-shape SIMD-Spielman above `simd_spielman.sh --max-orion-logn`; it carries `omission_reason`. |
| `error`, `omission_reason` | Error message string if `status` is `failed`, `timeout` or `oom`, else `null`; `omission_reason` explains an `omitted` case (SIMD-Spielman records only). |
| `hash_algorithm`, `hash_crate_version`, `hash_mode` | BLAKE3 identity (`1.8.7`) and the build's backend configuration (`portable-only`). |
| `field`, `params`, `log_n`, `seed`, `threads`, `cores`, `slices`, `slice_mode`, `warmup`, `runs` | Input and execution configuration. `slices` is the effective horizontal slice count; `slice_mode` is `adaptive`, `explicit`, `autotuned`, or `table`. `params` is the code-parameter mode (`lcpc-sdig6`, `lcpc-sdig3`, or `simd-spielman`). `cores` records the requested affinity. Script defaults are `--warmup 2 --runs 5`; the 2026-10-22/23 GCP campaign used `--warmup 1 --runs 5` for every configuration. |
| `build_variant`, `effective_rustflags` | Original LCPC only (written by `external/lcpc-orig` / `reproduce/lcpc_orig.sh`). `build_variant` is one of `as-published-as-is`, `as-published-portable`, `fork-flags-as-is`, `fork-flags-portable`; `effective_rustflags` is the rustflags string the build actually used. All records of the 2026-10-22/23 campaign (x86-64 stages A–R and Arm) lack both fields; their variant follows from the file name (`lcpc-sdig6-plain-release-blake3-as-is.json` → `as-published-as-is`, `...-plain-release-blake3-portable-only.json` → `as-published-portable`, `...-fork-release-blake3-as-is.json` → `fork-flags-as-is`, `...-fork-release-blake3-portable-only.json` → `fork-flags-portable`). The campaign's `as-published-*` records were built with the fork's flags and are excluded by `results/selection.json`. |
| `adaptive_slices` | The slice count recommended by the adaptive cache model. |
| `session_id`, `session_seed`, `stage`, `block_id`, `block_status` | Added by `reproduce/all.sh` to every record in a block's own output directory, failed blocks included: the session, its block-order seed, the stage tag (`<UTC time>_<stage>`, matching `<session>/stages/`), the block id (equal to the output directory name) and the block's exit status. The campaign's records lack `stage` and `block_status`, and some 2^28 SIMD-Spielman records carry the wrong `block_id` (`docs/STATUS.md` §7.4, I6); analysis takes the block from the directory path. |
| `freq_before_mhz`, `freq_after_mhz`, `freq_source`, `freq_is_nominal`, `watermark` | Also added by `all.sh`: the frequency of the block's first pinned core before and after the block, and where it came from. `freq_source` starts with `observed:` (cpufreq `scaling_cur_freq` of that core), `nominal:` (the core's `cpu MHz` line in `/proc/cpuinfo`, on hosts without cpufreq such as the GCP guests) or is `unavailable`; `freq_is_nominal` is true exactly when it starts with `nominal`. `watermark` marks local smoke output. The campaign's records (2026-10-22/23) carry only `freq_before_mhz` and `freq_after_mhz`; they lack `freq_source`, `freq_is_nominal`, `cycles_per_mac_is_nominal` and `cpu_mhz_source` (their frequencies are the nominal `/proc/cpuinfo` value). |
| `cpu_mhz_before`, `cpu_mhz_after`, `cpu_mhz_source` | CPU frequency in MHz read by the benchmark binary from the first `cpu MHz` entry of `/proc/cpuinfo` (CPU 0, not necessarily the pinned core), one snapshot before and one after the timed execution. On the GCP guests this is the constant nominal value (2600 MHz on `c3d`), not an observed frequency. `cpu_mhz_source` (This Work's `lcpc-bench`, `bench/src/lib.rs`) states this provenance; `all.sh` adds an equivalent legacy description to records that have `cycles_per_mac` but no `cpu_mhz_source`. |
| `cycles_per_mac_is_nominal` | Added by `lcpc-bench` and by `all.sh` to every record that has `cycles_per_mac`, on every host, and always `true`: `cycles_per_mac` is computed from the `cpu_mhz_*` snapshot above, never from a cycle counter or the block-level `freq_*` fields, so it is nominal even where cpufreq exists. Absent when `cycles_per_mac` is absent. |
| `dimensions` | `coefficient_count`, `n_rows`, `n_per_row`, and encoded `n_cols`. |
| `recursion_depth`, `level_parameters` | The dimensions and degree choices at each recursion level (`n_i`, `m_i`, `c_n`, `n_i_prime`, `m_i_prime`, `d_n`), along with per-level graph edge counts: `precode_edges`, `postcode_edges`, and `total_edges`. |
| `total_graph_edges` | Sum of sampled graph edges across all recursion levels. |
| `total_edge_row_macs` | Total edge-row multiply-accumulate operations in the full matrix encoding ($\sum_{\text{levels}} \text{edges} \times \text{rows}$). |
| `ns_per_mac` | Normalized latency: nanoseconds per (edge $\times$ row) MAC = `latency_ns / total_edge_row_macs`. |
| `cycles_per_mac` | `ns_per_mac` × MHz / 1000, using the `/proc/cpuinfo` MHz above. On GCP guests this uses the nominal frequency and is not a measured cycle count; do not report it as such. |
| `setup_ns` | One-time graph/encoding construction, deterministic coefficient and row-buffer setup, and verifier encoding construction. For SIMD-Spielman (`reproduce/simd_spielman.sh`, new runs only) it is an object `{"graph_generation_and_process_overhead_ns": …}`: GNU `time` wall time of the benchmark process minus the sum of its printed encode samples (warm-up and timed), so it includes process start, graph generation, input generation and output; `null` when GNU `time` was unavailable. |
| `process_wall_ns`, `setup_ns_resolution_ns`, `setup_ns_floor_ns`, `setup_ns_below_floor`, `setup_ns_method` | SIMD-Spielman records only (`reproduce/simd_spielman.sh`, new runs). `process_wall_ns`: GNU `time` "Elapsed (wall clock)" of the benchmark process. `setup_ns_resolution_ns`: that clock's resolution (10 ms). `setup_ns_floor_ns`: 10 × the resolution (100 ms); `setup_ns_below_floor` is true when the derived setup time is below it, i.e. too close to the resolution to be meaningful (the value is kept, but must be flagged in any table). `setup_ns_method`: a text description of the derivation. All are `null` when no setup time was derived. |
| `measurements` | Map of timed operations: `encode_matrix`, `verifier_encode_1`, `verifier_encode_2`, `verifier_encode`, `hash_matrix_decomposition`, `commit_total`, `prove`, and `verify`. |
| `proof_size_bytes` | Serialized PCS proof size in bytes. |
| `correctness` | Whether direct encoding hashes to the committed root, whether the proof verifies, and the verified evaluation in field-representation hex. |
| `peak_rss_bytes` | Peak resident set size (VmHWM) in bytes recorded from `/proc/self/status`. |
| `perf_stat` | Optional PMU hardware performance counter measurements (when `--perf` is passed), including `cycles`, `instructions`, `ipc`, `branches`, `branch_misses`, `l1d_loads`, `l1d_load_misses`, `llc_loads`, `llc_load_misses`, and `time_elapsed_secs`. |

## Measurement structure

Each measurement object contains:
- `samples_ns`: raw timed samples as integer nanoseconds.
- `summary`: statistical summary object containing `min_ns`, `q1_ns`, `median_ns`, `q3_ns`, `iqr_ns`, `max_ns`, and `mean_ns`.

Measurements:
- `encode_matrix`: Prover encoding of the full $k \times n$ matrix (setup excluded).
- `verifier_encode_1`: Encoding of 1 vector of length $k$.
- `verifier_encode_2`: Encoding of 2 vectors of length $k$ (matches SIMD-Spielman 128-bit vector lane count).
- `verifier_encode`: Alias for 2-vector verifier encoding.
- `hash_matrix_decomposition`: Leaf and Merkle hashing of the encoded matrix.
- `commit_total`: Full `commit()` operation combining encoding, hashing, and tree construction.
- `prove`: Polynomial evaluation proof generation.
- `verify`: Verifier check of the polynomial evaluation proof.

## Field microbenchmarks schema

When invoked with `--field-bench`, `lcpc-bench` (or `bench_fields`) produces a JSON
array of field microbenchmark reports:

```json
[
  {
    "field": "Mont127",
    "mul": { "op": "mul", "iterations": 100000, "total_ns": 234567, "ns_per_op": 2.345 },
    "mac": { "op": "mac", "iterations": 100000, "total_ns": 198765, "ns_per_op": 1.987 },
    "reduce": { "op": "reduce", "iterations": 100000, "total_ns": 145678, "ns_per_op": 1.456 }
  }
]
```

## Session layout

`reproduce/all.sh` writes one session as `results/<host>/<UTC-date>-<gitsha>/<session>/`: one directory per block (its records), `stages/<UTC time>_<stage>/` per `all.sh` call (stage `schedule.json`, `blocks.tsv`, `manifest.json`, execution log), an append-only session-level `schedule.json` whose `stages` list accumulates every stage, `failed_blocks.tsv`, and `<block>.prev-<UTC time>/` for a block output that a later re-run moved aside. `code/README.md` ("Session directory layout") has the details. The 2026-10-22/23 campaign predates this layout.

## Result selection (`results/selection.json`)

Raw run directories under `results/` are not all valid: some were measured
with bugs that were later fixed, and some blocks were re-measured. The file
`results/selection.json` (schema `rbd-selection-v1`, defined by `scripts/analyze.py`) lists, per
machine, the run directories and sessions that are included. Its `exclude` list
holds **path globs** relative to `results/` (`<machine>/<run>/<session>/<block>`, fnmatch,
`*` also matches `/`), each with a reason and an optional `match` on record fields (for
example `build_variant`); `*.prev-*` excludes block outputs moved aside by a re-run. Its
`omissions` list documents table cells (`TABLE|ROW|COLUMN` globs) that are pending
re-measurement or deliberately omitted; omissions apply only to missing or N/A cells, never to
integrity errors (duplicates, mixed configurations, oversubscription, uncovered records, interrupted
blocks), and an omission that matches no cell produces a warning. Omissions can also document a
row that `expected_rows` requires but a table lacks (`TABLE|v1,v2,…|(row)`) and a `FAIL` codegen
report (`T11|<arch> @ <fork>,hot_kernels (aggregate)|Scalar Status`, or
`T3|codegen,<tag>|Scalar Status` for the per-step reports). A `runs[].run_dir` may be a glob
(inert until a matching directory exists). Its `supersede` rules (`by`: machine/run globs,
`blocks`: block path globs, `reason`) exclude, among included records, every older record of a
cell that a run matching `by` re-measured; the cell is the configuration (implementation, LCPC
build variant, field, parameters, shape, slices, log2 N, threads), so superseding is per cell, not
per block, and spans all sessions; a re-measured This Work/original-LCPC thread-scaling or Arm pair
whose two sides end up in different runs is reported as `[WARN] Same-boot pair split across runs`.
Its `expected_rows` (`{TABLE: {key column header: [values]}}`, or a list of such specs whose rows
are the union of their cartesian products) lists the rows each table must contain.
`scripts/analyze.py --selection results/selection.json` refuses records that
the selection does not cover. The campaign record in `docs/STATUS.md` (§7)
explains every exclusion.
