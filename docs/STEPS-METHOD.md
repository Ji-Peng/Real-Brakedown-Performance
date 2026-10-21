<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Per-step evaluation method

`reproduce/steps.sh` evaluates E3 at every tagged step from `step/01-modernize`
through the checked-out `HEAD`. It creates detached worktrees and runs each
worktree's complete release test suite before building the timing adapter or
collecting timings. The script does not move or rewrite tags or commits.

## Shared harness and adapter boundary

All configurations use the same source file, `external/steps-driver/src/main.rs`
(harness version `steps-driver-v1`). It owns the input generation, graph/code
setup boundary, matrix allocation, warm-up loop, timer, sample collection,
median/minimum/maximum/mean calculation, and JSON serialization. Its driver
binary is built separately against each tag's path dependencies. Each JSON
record includes `harness_version`, `adapter_version`, the source and generated
driver lockfile hashes, and the source commit.

The only tag-dependent code is a small adapter around the public encoder API:

| API stage | Adapter | Timed call |
| --- | --- | --- |
| `step/01-modernize` and `step/02-fields` | `rowwise-encode-v1` | Calls `LcEncoding::encode` once for each row, sequentially. |
| `step/03-column-major` | `encode-rows-v1` | Calls `LcEncoding::encode_rows` once for the complete matrix. |
| `step/04-r2l` through `step/06-blocking` | `runtime-encode-rows-v1` | Calls `LcEncoding::encode_rows` once for the complete matrix. |
| `step/07-slicing` through `HEAD` | `runtime-adaptive-encode-rows-v1` | Calls `LcEncoding::encode_rows` once for the complete matrix with the encoder's default adaptive slicing. |

These are the available equivalents of the same operation: encode every row
of one row-major coefficient matrix into its complete systematic codeword. The
early LCPC API exposes only single-codeword `encode`; at `step/03-column-major`,
the fork adds the public matrix entry point used by the PCS commit path. Thus
the adapter changes where the API requires it, while the measured operation and
the timing loop remain the same. The `encode_rows` call's column-major scratch
allocation and transposes are inside the timed region because they are part of
that implementation's matrix encoding. Graph sampling/code construction,
row-major matrix allocation, and input restoration are outside the timer.

For each warm-up and timed sample, the driver reinitializes the message prefixes
from the same fixed ChaCha20 seed before starting the timer. This makes each
sample a fresh encoding of identical input data without timing input generation
or buffer restoration. Caches are not flushed. T is fixed at 1; `RAYON_NUM_THREADS=1`
is set and the entire test/build/check/timing process is constrained to the
explicit `--cores` list. CPU 2 is rejected.

## E3 configurations and unavailable cells

- Mode (a): `SdigCode6`, `logN ∈ {16, 20, 24, 28}`, T=1. The historical
  `step/01-modernize` checkout has no `lcpc-fields::Mont127`, so its adapter
  uses LCPC's `Ft127`. It is the same 127-bit modulus and random stream as
  `Mont127`; the field compatibility is covered by the fork's correctness
  tests. Later steps use `Mont127`.
- Mode (b): `SimdSpielman` parameters with `Mers127`, the same sizes, and T=1.
  Mode (b) depends on the runtime `CodeParams` API introduced at
  `step/04-r2l`. Earlier tags produce explicit `not_applicable` records for
  every requested size, with the reason in each JSON object.
- The default statistics are two warm-ups and five timed runs, except one
  warm-up for `logN >= 26`. Each record stores raw nanosecond samples and their
  minimum, median, maximum, and mean. `--runs` and `--warmup` can be reduced for
  local smoke runs; those records carry the watermark `LOCAL SMOKE — NOT A RESULT`.
- Run the script once in each of the two independent benchmark sessions, using
  `--session-id` to label each session. Compare session medians and retain the
  per-session min–max ranges. Say "faster" only when the ranges do not overlap
  in both sessions; otherwise report "comparable". Flag a greater-than-5%
  median difference between sessions for a possible third block.
- Every timing configuration has a default 1800-second limit. A timeout or a
  process killed with exit 137 is retained as an explicit status record rather
  than silently dropped. Exit 137 is labelled `oom_or_killed`, since the script
  cannot distinguish the kernel OOM killer from another SIGKILL source.

The resulting JSON records are per-tag, per-mode, and per-size under the output
directory. Tag manifests point to each test log and codegen report; the run
manifest lists all records and their harness/adapter versions. Results from
`--smoke` are for local wiring checks only and are never benchmark evidence.

## Commands

The planned E3 run (on the configured benchmark VM) uses defaults:

```sh
./reproduce/steps.sh --cores <explicit-core-list> --out results
```

A local wiring check can use a small even size while avoiding reserved CPU 2:

```sh
taskset -c 0,1,3-7 ./reproduce/steps.sh \
  --smoke --logN 10 --warmup 0 --runs 1 \
  --cores 0,1,3-7 --out "$PWD/target-eval-steps/smoke"
```

The shell script loads the pinned rustup environment, gives every tag its own
target subdirectory below `target-eval-steps`, and preserves all output under
the requested directory. No values from a local smoke run are publishable.
