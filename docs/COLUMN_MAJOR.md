<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 03: Batched Column-Major Encoding (`step/03-column-major`)

This step changes how the commitment encodes the coefficient matrix. Upstream
LCPC (`f0e2116`) encodes each of the `n_rows` rows independently (Rayon
`par_chunks_mut` over rows, one `sprs` sparse matrix-vector product per
precode/postcode and row). From this step on, all rows are encoded in **one
batched pass** over a column-major copy of the matrix. Field arithmetic stays
eager and the graph stays in `sprs::CsMat` form; R2L evaluation and lazy
reduction come in later steps.

## Layout

The batch of `n_rows` codewords of length `n_cols` is stored position-major,
row-minor: element `(row, col)` is at `buf[col * n_rows + row]`. Each codeword
position is one contiguous column of `n_rows` elements.

## Kernel (`lcpc-brakedown-pc/src/encode.rs`)

- `encode_col_major(buf, n_rows, precodes, postcodes)` follows the same
  recursion as upstream `encode` (precodes down, Reed-Solomon base case,
  postcodes back up), with every offset scaled by `n_rows`.
- `mac_column_block` evaluates one sparse matrix on a column block. Each
  nonzero `(r, l, w)` is loaded once and applied to the whole column:
  `out[r][row] += w * in[l][row]` for `row in 0..n_rows`. Both columns are
  unit-stride and consecutive rows are independent. Edges are visited in the
  matrix's storage order (by input node for LCPC's CSC matrices).
- `reed_solomon_column_block` evaluates the Vandermonde base case with Horner's
  rule element-wise across each column.
- The single-vector `encode` is `encode_col_major` with `n_rows = 1`, so the
  verifier's 1-vector encodings use the same kernel.
- `mac_column_block` and `reed_solomon_column_block` are `#[inline(never)]` so
  their disassembly can be inspected.

Field addition is associative and commutative, so the outputs are bit-exact
with the per-row `sprs` products this replaces.

## PCS integration (`lcpc-2d`)

- New trait method `LcEncoding::encode_rows(&self, comm, n_cols)`: encode every
  row of a row-major codeword matrix whose message prefixes are filled in. The
  default implementation is upstream's row-parallel loop, so `lcpc-ligero-pc`
  is unchanged.
- `commit` copies the message prefixes into `comm` and calls `encode_rows`.
- `SdigEncodingS` overrides `encode_rows`: transpose the messages into a
  column-major buffer, run `encode_col_major`, transpose the redundancy columns
  back (the message columns are unchanged because the code is systematic).

`LcCommit::comm` stays row-major, so serialization, `hash_columns`,
`open_column`, `prove`, and `verify` are unchanged. The README §3.2 option of
keeping `comm` column-major (which would make column hashing contiguous) is
left for a later step after measurement. Costs of this choice:

- one extra copy of the matrix (the column-major buffer) during `commit`;
- two transposes (tiled, single-threaded) per `commit`.

## Threads

The batched encoder runs on the calling thread. Rayon still parallelizes the
coefficient copy, column hashing, and Merkle tree. Multi-threaded encoding
returns in `step/08-threads` (expander-level worker pool). Multi-thread
`commit` timings between `step/03` and `step/07` are therefore not comparable
with upstream's row-parallel encoder; per-step attribution uses one thread.

## Benchmark harness

`lcpc-bench`'s `encode_matrix` now calls `LcEncoding::encode_rows`, the entry
point that `commit` uses. Its time includes allocating the column-major buffer
and both transposes (see `docs/bench-json-schema.md`).

## Tests

- `lcpc-brakedown-pc` `batched_col_major_encode_matches_per_row_reference`:
  upstream's per-row `sprs` encoder (kept verbatim as a test-only reference)
  vs. `encode`, `encode_col_major`, and `encode_rows` for batch sizes
  1, 2, 3, 5, 16, 33 on `Ft63`, `Ft127`, `Mont127`, `Mers127`, `Fp2M61` with
  `SdigCode3`/`SdigCode6`. The codeword region past the message is filled with
  random values first, to check that it is fully overwritten.
- Existing oracles still pass unchanged: original-LCPC bit-exactness
  (`lcpc-oracle-tests`: encoded matrix, roots, proofs, verification) and the
  golden vectors (`lcpc-bench --check-golden tests/golden/lcpc-orig/golden-v1.json`).
