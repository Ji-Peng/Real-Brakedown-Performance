<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# LCPC benchmark JSON schema (`lcpc-bench-v1`)

The benchmark driver (`lcpc-bench` in `bench/`, as well as `external/lcpc-orig`)
emits a JSON array containing one `lcpc-bench-v1` object per requested `logN`.
The same top-level and per-case shape is shared across the fork's benchmark
harness and the original LCPC baseline so raw results can be analyzed together
without conflating implementation names or build settings.

Each report contains:

| Field | Meaning |
| --- | --- |
| `schema_version` | Literal `lcpc-bench-v1`. |
| `implementation`, `upstream_commit`, `driver_version` | Implementation identity (`lcpc-fork` vs. `lcpc-original-upstream`) and pinned upstream commit. |
| `hash_algorithm`, `hash_crate_version`, `hash_mode` | BLAKE3 identity (`1.8.7`) and the build's backend configuration (`portable-only`). |
| `field`, `params`, `log_n`, `seed`, `threads`, `cores`, `warmup`, `runs` | Input and execution configuration. `cores` records the requested affinity; `taskset` applies it. |
| `dimensions` | `coefficient_count`, `n_rows`, `n_per_row`, and encoded `n_cols`. |
| `recursion_depth`, `level_parameters` | The degree choices at each recursion level (`n_i`, `m_i`, `c_n`, `n_i_prime`, `m_i_prime`, `d_n`). |
| `setup_ns` | One-time graph/encoding construction, deterministic coefficient and row-buffer setup, and verifier encoding construction. |
| `measurements` | `encode_matrix`, `hash_matrix_decomposition`, `commit_total`, `prove`, and `verify`. |
| `correctness` | Whether direct encoding hashes to the committed root, whether the proof verifies, and the verified evaluation in field-representation hex. |
| `hash_decomposition_includes_scratch_allocation` | `true`; the decomposition allocates its local Merkle buffers. |

Each measurement is an object with `samples_ns` (the raw timed samples) and a
`summary` object containing `min_ns`, `q1_ns`, `median_ns`, `q3_ns`, `max_ns`,
and `mean_ns`. Samples are integer nanoseconds. Summary values are numeric;
the median and mean may be fractional when the sample count is even.

`encode_matrix` excludes graph setup, allocation, and coefficient copying. It
times all row encodings of one full coefficient matrix. The standalone
`hash_matrix_decomposition` reproduces LCPC's leaf and Merkle hash operations,
including local scratch allocation; it is a checked decomposition rather than
an internal hook into upstream. `commit_total` includes all work performed by
`LcCommit::commit` and is the authoritative end-to-end commitment time. These
quantities are not additive.
