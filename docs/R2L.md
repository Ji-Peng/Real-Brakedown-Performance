<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 04: Right-to-Left Graph Evaluation and Runtime Code Parameters (`step/04-r2l`)

This step changes how the expander graphs are stored and evaluated, and makes
the code parameters a runtime choice. Field arithmetic stays eager (one
modular multiply and add per edge and row); lazy reduction comes in
`step/05-lazy`.

## Graph storage (`lcpc-brakedown-pc/src/graph.rs`)

Upstream LCPC stored each precode/postcode as a `sprs::CsMat` in compressed
sparse column (CSC) form and evaluated it with `sprs`'s matrix-vector product,
which walks the left (input) nodes and scatters `out[r] += w * in[l]`
(left-to-right, SIMD-Spielman's Algorithm 2).

The fork now stores each graph as an `R2lGraph<F>`: compressed sparse row
(CSR) form over the right (output) nodes, `row_ptr: Vec<u32>`,
`col_idx: Vec<u32>` (left node of each incoming edge), `weights: Vec<F>`.

- Graphs are still **sampled** left to right in the specification's RNG
  order and then transposed once, by `R2lGraph::from_left_edges` (CSC input,
  variable degree) or `R2lGraph::from_left_edges_fixed_degree` (SIMD-Spielman's
  `l2r_node_connections` layout).
- The transpose is a stable two-pass counting sort: count in-degrees, prefix
  sum, scatter in left-node order. It runs in `O(n_left + n_right + E)` time,
  versus SIMD-Spielman's `O(R * E)` scan (`ExpanderGraph.h`); both give the
  same edge order per right node (increasing left node, sampling order for
  parallel edges). It is setup work, timed with graph sampling and not with
  encoding.
- Parallel edges (possible in mode (b), which samples with replacement) are
  kept as separate entries.
- `u32` indices halve index traffic compared with `usize`; construction panics
  if a graph does not fit.

`sprs` is no longer a dependency of the library or of `lcpc-bench`. It
remains a dev-dependency of `lcpc-brakedown-pc` for the verbatim upstream
per-row encoder kept as a test-only reference (and upstream's
`sprs_playground` test).

## Evaluation (`lcpc-brakedown-pc/src/encode.rs`)

`encode_column_block(graph, input, output, n_rows)` evaluates one graph on a
column-major block (SIMD-Spielman's Algorithm 3, right-to-left "gather"): for
each right node `r`, the first incoming edge initializes output column `r`
(`out[r][row] = w * in[l][row]`), the remaining edges accumulate into it, and a
node without edges gets zeros. Each output column is written in one pass over
its edges, and input columns are only read. Rows are the unit-stride inner loop,
as in `step/03-column-major`.

Eager field addition is associative and commutative, so the result is the
same field element as the left-to-right scatter: the encoder is bit-exact with
`step/03-column-major` and with upstream LCPC in mode (a).

`encode_col_major(buf, n_rows, code)` and the single-vector
`encode(xi, code)` now take an `ExpanderCode<F>`: the per-level precode and
postcode graphs plus a `BaseCase`:

- `BaseCase::ReedSolomon` (mode (a)): as upstream, the last precode output is a
  temporary, Reed-Solomon encoded (Horner, column-major) into the codeword.
- `BaseCase::Identity` (mode (b)): the last precode output is written into the
  codeword and the last postcode reads it directly (SIMD-Spielman's
  `r2l_runEncodeEfficient` returns at `rec >= num_graphs`).

The codeword length formula is the same for both base cases.

**External graphs.** `ExpanderCode::from_graphs(base, precodes, postcodes)`
builds a code from graphs sampled elsewhere and checks that their dimensions
chain correctly. `SdigEncodingS::from_code(params, code)` wraps such a code as
an `LcEncoding` for the PCS. The `tv/` tests use this path to feed upstream's
graphs (from the test-only glibc sampler in `lcpc-spielman-oracle`) to the
fork's own transpose and encoder.

**Codegen entry points.** `encode_column_block` and
`reed_solomon_column_block` are public, generic, and `#[inline(never)]`, so
each field gets its own symbol (e.g.
`lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::mont127::Mont127>`),
which `scripts/check_codegen.py` finds by pattern. This step also fixes the
checker's symbol-header regex, which stopped at the first `>` and so never
matched demangled generic symbols (the `step/03` kernel `mac_column_block`
was not inspected before this fix).

## Runtime code parameters (`codespec.rs`, `matgen.rs`)

`CodeParams` selects the code at runtime. It chooses *which code* is computed,
not how; both modes use the same encoder.

| Mode | `CodeParams` | Name | Dimensions | Degrees | Sampling | Base case |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| (a) | `Lcpc(LcpcParams::of::<SdigCode6>())` | `lcpc-sdig6` | exact rational ceilings (`get_dims`) | entropy formulas per level | `ChaCha20Rng` stream `(seed, level)`, distinct neighbors, sorted | Reed-Solomon, `baselen = 20` |
| (a) | `Lcpc(LcpcParams::of::<SdigCode3>())` | `lcpc-sdig3` | as above | as above | as above | as above |
| (b) | `SimdSpielman` | `simd-spielman` | `floor(0.238 n)`, `floor(n (1.72 - 1) - n')` (upstream's `double` to `uint32_t` casts) | `c_n = 10`, `d_n = 20` | depth-first `G1[0..L]`, then `G2[L-1..0]`; with replacement | identity, threshold 13 |

- `LcpcParams::of::<S>()` captures every value LCPC's recurrence reads from
  `S` (numerators, denominators, `baselen`, `dist`, and the four entropy
  constants) by calling `S`'s own methods, so a runtime mode-(a) code is
  identical to the compile-time one.
- `SdigEncodingS<F, S>` keeps the upstream constructors (`new`, `new_ml`,
  `new_from_dims`) for compile-time `S`. The value now stores its
  `CodeParams`, and the `LcEncoding` impl no longer requires
  `S: SdigSpecification`. `SpielmanEncoding<F> = SdigEncodingS<F, RuntimeParams>`
  is the runtime-parameter type, built with `with_params`, `new_with_params`,
  `new_ml_with_params`, or `from_code`.
- Mode (b) in the library samples with one `ChaCha20Rng` stream seeded by
  `seed`: right node uniform via LCPC's `LegacyUniformUsize`, weight
  `F::random` rejecting zero. It is not glibc `random()`. Dimensions and edge
  counts are upstream's, so encoding work is identical. The exact upstream
  graphs are only needed for the test vectors, and come in through
  `from_graphs`.
- PCS soundness parameters in mode (b): the column-opening count uses
  `beta / r` of `SdigCode6`, which has the same `alpha`, `beta`, and `r`. Mode
  (b) is meant for like-for-like encoder comparison with SIMD-Spielman. LCPC's
  soundness analysis of the degree choice applies to mode (a), not to the
  fixed degrees of mode (b).
- `lcpc-bench --params lcpc-sdig6|lcpc-sdig3|simd-spielman` selects the mode.
  `--field fp2m61` is now accepted too. The bench builds every encoding through
  `SpielmanEncoding::with_params`. Mode-(a) seeds are unchanged from earlier
  steps, and mode (b) uses seed tag `0x5353`. `level_parameters` in the JSON
  report are read from the sampled graphs (see `docs/bench-json-schema.md`).

## Tests

- `lcpc-brakedown-pc` unit tests:
  - `graph::tests`: counting-sort transpose vs. a naive `O(R * E)` transpose
    (parallel edges, isolated right nodes, variable degree).
  - `matgen::tests::simd_spielman_dims_small`.
  - `batched_col_major_encode_matches_per_row_reference`: upstream's
    per-row `sprs` CSC encoder (verbatim, run on the CSC matrix rebuilt from the
    R2L graph) vs. `encode`, `encode_col_major`, and `encode_rows`, batch sizes
    1–33, five fields, `SdigCode3`/`SdigCode6`.
  - `runtime_lcpc_params_match_compile_time_spec`: `SpielmanEncoding` with
    `lcpc-sdig6`/`lcpc-sdig3` equals `SdigEncodingS<F, SdigCode6/3>` (graphs,
    dimensions, PCS parameters).
  - `simd_spielman_mode_encodes_and_commits`: mode-(b) graph shapes and edge
    counts, systematic prefix, batch invariance, linearity, and a
    commit/prove/verify round trip for four fields.
  - The existing golden-vector, Mont127-vs-Ft127, and end-to-end tests are
    unchanged in substance.
- `lcpc-oracle-tests` (against unmodified LCPC `f0e2116`):
  - `fork_fields_match_original_lcpc_mode_a` (unchanged): encoded matrix, root,
    proof bytes, and verification.
  - New `fork_r2l_graphs_are_transposed_original_graphs`: every R2L graph is
    exactly upstream's CSC matrix transposed (indices and weight
    representations), for n in {32, 100, 1000, 4096}, three seeds,
    `SdigCode3`/`SdigCode6`.
  - New `fork_runtime_params_match_original_lcpc_mode_a`: larger shapes
    (16 x 1024, 7 x 777) through the full PCS, plus runtime-parameter
    `SpielmanEncoding` `encode_rows` vs. upstream per-row `encode`.
  - New `simd_spielman_tv.rs`: the fork's own encoder in mode (b) on all 18
    `SIMD-Spielman-for-ZKPs/tv/` pairs (Mont127, Mers127, Fp2M61; N = 2^12,
    2^14, 2^16; Brakedown and Orion shapes). It feeds the glibc-sampled
    upstream graphs through `R2lGraph::from_left_edges_fixed_degree` and
    `ExpanderCode::from_graphs`. It checks that `matgen::simd_spielman_dims`
    equals upstream's dimensions, that the fork's transpose equals the
    oracle's R2L graphs, and that `encode_col_major`, `encode_rows`, and
    `encode` are bit-exact with `tv/*_out.txt` (including the zero padding
    past the codeword).
- `lcpc-bench --check-golden tests/golden/lcpc-orig/golden-v1.json` passes.
