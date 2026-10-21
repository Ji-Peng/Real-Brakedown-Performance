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
| `status` | Run status: `"ok"`, `"timeout"`, or `"oom"`. |
| `error` | Error message string if `status != "ok"`, else `null`. |
| `hash_algorithm`, `hash_crate_version`, `hash_mode` | BLAKE3 identity (`1.8.7`) and the build's backend configuration (`portable-only`). |
| `field`, `params`, `log_n`, `seed`, `threads`, `cores`, `slices`, `slice_mode`, `warmup`, `runs` | Input and execution configuration. `slices` is the effective horizontal slice count; `slice_mode` is `adaptive`, `explicit`, `autotuned`, or `table`. `params` is the code-parameter mode (`lcpc-sdig6`, `lcpc-sdig3`, or `simd-spielman`). `cores` records the requested affinity. Defaults are `--warmup 2 --runs 5` (`--warmup 1` for $\log_2 N \ge 26$). |
| `adaptive_slices` | The slice count recommended by the adaptive cache model. |
| `cpu_mhz_before`, `cpu_mhz_after` | Observed CPU core frequency in MHz before and after the timed execution (from `/proc/cpuinfo`). |
| `dimensions` | `coefficient_count`, `n_rows`, `n_per_row`, and encoded `n_cols`. |
| `recursion_depth`, `level_parameters` | The dimensions and degree choices at each recursion level (`n_i`, `m_i`, `c_n`, `n_i_prime`, `m_i_prime`, `d_n`), along with per-level graph edge counts: `precode_edges`, `postcode_edges`, and `total_edges`. |
| `total_graph_edges` | Sum of sampled graph edges across all recursion levels. |
| `total_edge_row_macs` | Total edge-row multiply-accumulate operations in the full matrix encoding ($\sum_{\text{levels}} \text{edges} \times \text{rows}$). |
| `ns_per_mac` | Normalized latency: nanoseconds per (edge $\times$ row) MAC = `latency_ns / total_edge_row_macs`. |
| `cycles_per_mac` | Normalized CPU cycles per (edge $\times$ row) MAC derived from observed CPU MHz. |
| `setup_ns` | One-time graph/encoding construction, deterministic coefficient and row-buffer setup, and verifier encoding construction. |
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
