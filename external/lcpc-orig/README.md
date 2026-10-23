<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Original LCPC baseline driver

This standalone stable-Rust project calls the unmodified upstream `lcpc-2d`,
`lcpc-brakedown-pc`, and `lcpc-test-fields` crates at
`f0e2116001af5a1bbf39481535f837d6a25772e8`. It is excluded from the fork's
Cargo workspace and has its own `Cargo.lock`. No upstream files are copied or
patched here; the timing loop and the PCS calls are driver code (see
"Benchmark CLI").

Upstream `lcpc-brakedown-pc` contains an unconditional `#![feature(test)]` even
though this driver calls only its stable public API. Commands set
`RUSTC_BOOTSTRAP=lcpc_brakedown_pc`, which enables that attribute only while
compiling the pinned upstream crate. The driver itself uses stable Rust and
`std::time::Instant`.

The old `lcpc-2d` API requires `digest` 0.9. This driver adapts BLAKE3's
streaming `Hasher` to that digest interface without changing its selected
backend or algorithm. It pins BLAKE3 1.8.7 and enables the same
`traits-preview` feature used by upstream's development dependency. The
adapter makes the newer BLAKE3 digest version compatible with LCPC's older
generic digest API. This is a documented deviation from the plan's proposed
1.2.0 pin; the reported JSON and manifest record 1.8.7.

The default build uses BLAKE3's default `std` backend selection. The
`portable-only` Cargo feature additionally enables `pure`, `no_sse2`,
`no_sse41`, `no_avx2`, and `no_avx512`. The two builds therefore measure the
same BLAKE3 version and output function with different backend configurations.

## Benchmark CLI

Build and run from this directory, after sourcing the pinned Rust environment:

```sh
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="../../target-lcpc-orig/manual"
RUSTC_BOOTSTRAP=lcpc_brakedown_pc \
  taskset -c 0,1,3-7 cargo run --locked --release -- \
    --params lcpc-sdig6 --logN 16,18 --runs 5 --warmup 1 \
    --threads 1 --cores 0
```

Supported options are:

- `--params lcpc-sdig6|lcpc-sdig3` (default `lcpc-sdig6`)
- `--logN 16,18,...` (default `16,18,20,22,24,26`; each value must be even)
- `--runs N`, `--warmup N`, and `--threads N`
- `--cores LIST`, recorded as metadata; pin the process separately with
  `taskset` to that same list
- `--output PATH` (otherwise JSON is written to stdout)
- `--check-golden PATH` for the committed vector check
- `--generate-golden PATH` for deterministic vector regeneration

The benchmark uses `Ft127`, a deterministic coefficient vector, and a square
matrix with `n_rows = n_per_row = 2^(logN/2)`. Graph construction and input / row
buffer preparation are timed separately. `encode_matrix` times the driver's
own loop over the rows, which calls upstream's public row encoder
(`LcEncoding::encode`). Graph construction, allocation, and input copying are
outside the interval. It reports the effective per-level `c_n`/`d_n`,
dimensions, and recursion depth.

The row loop is `matrix.par_chunks_mut(n_cols).for_each(|row| enc.encode(row))`
(`encode_matrix` in `src/main.rs`). It reproduces the work split of upstream
`lcpc_2d::commit` at `f0e2116` (`lcpc-2d/src/lib.rs`, `comm.par_chunks_mut(n_cols)`
followed by `enc.encode(r)`): one Rayon task per row. `RAYON_NUM_THREADS`
(set from `--threads` by `lcpc_orig.sh`) is its parallelism. **Errata:** before
fork commit `18d75f8` (2026-10-22) this loop was a serial `for` over the rows,
so T > 1 measured the same as T = 1. Every original-LCPC block measured with
the serial loop (`full_lcpc_orig_mode_a_t{1,8}_n16-18-20-22-24-26` in run dirs
`2026-10-22-2557b5517338` and the earlier invalid run dirs) is superseded and
excluded through `results/selection.json`; those cells were re-measured in
stage R. `commit_total` always used upstream's own parallel `commit` and was
not affected.

`commit_total`, `prove`, and `verify` are measured through the original public
PCS API, with a fresh transcript for each proof and verification. The
independent `hash_matrix_decomposition` repeats LCPC's BLAKE3 column leaves
and Merkle reduction and checks its root against `commit_total`. It includes
its scratch allocations and is diagnostic; `commit_total` is the authoritative
end-to-end commitment measurement because upstream does not expose its
internal hash stage. Hash timing therefore must not be subtracted from commit
timing to infer an exact encoding or hashing duration.

All timing fields contain raw nanosecond samples plus min, quartiles, median,
maximum, and mean. The output structure is specified in
[`docs/bench-json-schema.md`](docs/bench-json-schema.md).

## Golden vectors

`../..` is the `code/` repository. Golden vectors live in
`tests/golden/lcpc-orig/golden-v1.json`. They cover `Ft63` and `Ft127`, both
`SdigCode3` and `SdigCode6`, and `(n_rows, n_per_row)` values `(2,32)`,
`(3,64)`, and `(4,128)`. Each case stores deterministic coefficients, every
encoded row, the BLAKE3 commitment root, and the result of proof verification.
To keep the artifact below 2 MiB, each case stores the BLAKE3 digest and byte
length of its deterministic bincode proof rather than the full proof bytes.
Regeneration reconstructs each proof and records its digest. Field encodings
use the upstream prime-field representation in little-endian byte order; all
binary values are lowercase hex. The file is checked to remain below 2 MiB.

Regenerate all vectors with:

```sh
sh gen_golden.sh
```

The script pins its target directory to `code/target-lcpc-orig/golden`, uses
the portable-only BLAKE3 build, and runs on cores `0,1,3-7`. The vector file
does not record the backend mode because the output is identical across the
two BLAKE3 configurations.

## Reproduction

From `code/`, run `sh reproduce/lcpc_orig.sh`. The script can build four
configurations: plain release defaults ("as published") and the fork's
release profile and no-vectorization rustflags ("fork flags"), each with
default ("as-is") and portable-only BLAKE3. The `LCPC_VARIANTS` environment
variable (a comma list; `all.sh --lcpc-variants` sets it) selects a subset; the 2026-10-22/23 campaign measured
`as-published-as-is` and `fork-flags-portable`. The script checks the golden
vectors with every built variant before collecting timings and writes raw
JSON and a host manifest under `results/<host>/<date>-<gitsha>/lcpc_orig/`.
Use `--out` to choose another output root, such as `target-lcpc-orig/smoke`
for a local smoke run.

Each record carries `build_variant` (`as-published-as-is`,
`as-published-portable`, `fork-flags-as-is`, or `fork-flags-portable`) and
`effective_rustflags`, the flags the build actually used
(`docs/bench-json-schema.md`). All records of the 2026-10-22/23 campaign
(x86-64 and Arm) predate these fields and are identified by their file name: `lcpc-sdig6-plain-release-blake3-as-is.json` is
`as-published-as-is`, `...-plain-release-blake3-portable-only.json` is
`as-published-portable`, `...-fork-release-blake3-as-is.json` is
`fork-flags-as-is`, and `...-fork-release-blake3-portable-only.json` is
`fork-flags-portable`.

**Errata (as-published builds, 2026-10-23 audit).** Cargo reads
`.cargo/config.toml` from the working directory, not from `--manifest-path`.
`lcpc_orig.sh` ran with the working directory `code/`, so the "as-published"
builds picked up the fork's `code/.cargo/config.toml`: `-C target-cpu=native
-C no-vectorize-loops -C no-vectorize-slp` on x86-64 and the two
no-vectorize flags on Arm. Their manifests nevertheless recorded "Cargo
defaults, auto-vectorization enabled". Every `as-published-*` record of the
campaign (x86-64 and Arm) therefore measured the fork's flags with the plain
release profile, not the published build. These records are excluded in
`results/selection.json` with the reason "as-published built with fork
.cargo/config.toml" and must be re-measured with the fixed script, which
builds every variant outside `code/` (the `fork-flags-*` variants pass the
fork's flags explicitly). The `fork-flags-*` records are unaffected; on Arm,
`fork-flags-portable` is therefore the only original-LCPC data of the
campaign. The re-measurement (stages RL, RL28, RH, RT*, ARM; launched on
2026-10-23) is recorded in `docs/STATUS.md` §7.6; its blocks supersede the
campaign's original-LCPC blocks of the same cells.

`RAYON_NUM_THREADS` follows `--threads`. `--cores` is metadata only, and
`taskset` performs pinning. In the campaign, T > 1 blocks were pinned to all 8
cores while the Rayon pool had T threads (This Work pins its T workers to the
first T cores); this asymmetry affects only the T = 2 and T = 4 thread-scaling
points. `reproduce/all.sh` now pins every T > 1 block to the first T cores. Local numbers are development evidence only.
