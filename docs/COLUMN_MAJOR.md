<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 03: Batched Column-Major Encoding (`step/03-column-major`)

This document describes batched column-major encoding of the coefficient matrix in `lcpc-brakedown-pc`. Upstream LCPC (`f0e2116`) encodes each of the `n_rows` rows independently (Rayon `par_chunks_mut` over rows, one `sprs` sparse matrix-vector product per precode/postcode and row). Here, all rows of a slice are encoded in **one batched pass** over a column-major buffer; R2L graph evaluation, lazy modular reduction, multi-edge blocking, LLC slicing, and expander-level multithreading build on this layout (see [R2L.md](R2L.md), [LAZY.md](LAZY.md), [BLOCKING.md](BLOCKING.md), [SLICING.md](SLICING.md), and [THREADS.md](THREADS.md)).

## Layout

The batch of `n_rows` codewords of length `n_cols` is stored position-major,
row-minor: element `(row, col)` is at `buf[col * n_rows + row]`. Each codeword
position is one contiguous column of `n_rows` elements.

## Kernel (`lcpc-brakedown-pc/src/encode.rs`)

- `encode_col_major(buf, n_rows, code)` follows the same recursion as upstream
  `encode` (precodes down, base case, postcodes back up), with every offset
  scaled by `n_rows`.
- `encode_column_block` evaluates one R2L sparse graph on a column block: for
  each right node `r`, incoming edges `(l, w)` are gathered across the entire
  column `row in 0..n_rows` (`out[r][row] += w * in[l][row]`). Both columns are
  unit-stride and consecutive rows are independent.
- `reed_solomon_column_block` evaluates the Vandermonde base case with Horner's
  rule element-wise across each column.
- The single-vector `encode` is `encode_col_major` with `n_rows = 1`, so the
  verifier's 1-vector encodings use the same kernel.
- `encode_column_block` and `reed_solomon_column_block` are `#[inline(never)]`
  so their disassembly can be inspected.

Field addition is associative and commutative, so the outputs are bit-exact
with the per-row `sprs` products this replaces.

## PCS integration (`lcpc-2d`)

- The trait method `LcEncoding::encode_rows(&self, comm, n_cols)` encodes every
  row of a row-major codeword matrix whose message prefixes are filled in. The
  default implementation is upstream's row-parallel loop, so `lcpc-ligero-pc`
  is unchanged.
- `commit` copies the message prefixes into `comm` and calls `encode_rows`.
- `SdigEncodingS` overrides `encode_rows`: transpose message slices into a
  column-major slice buffer, run the batched column-major encoder, and transpose
  the redundancy columns back (the message columns are unchanged because the
  code is systematic).

`LcCommit::comm` stays row-major, so serialization, `hash_columns`,
`open_column`, `prove`, and `verify` are unchanged. Keeping `comm` row-major
avoids changing downstream PCS interfaces, at the cost of a column-major slice
buffer (sized by horizontal slicing in [SLICING.md](SLICING.md)) and two
transposes per `commit`.

## Threads

When `threads > 1`, the batched expander encoder executes across the persistent
pinned worker pool (see [THREADS.md](THREADS.md)); Rayon parallelizes the
coefficient copy, column hashing, and Merkle tree. Single-thread per-step
attribution (`reproduce/steps.sh`) sets `threads = 1`.

## Benchmark harness

`lcpc-bench`'s `encode_matrix` calls `LcEncoding::encode_rows`, the entry
point that `commit` uses. Its time includes allocating the column-major slice
buffer and both transposes (see [bench-json-schema.md](bench-json-schema.md)).

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
