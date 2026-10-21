<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# arkworks PolyCommit Brakedown encoding driver

This standalone stable-Rust driver benchmarks arkworks `poly-commit` at commit
`1eaec5090ba5bc87d75a69570ed38f6fcf8b9417`. It calls the public
`linear_codes::MultilinearBrakedown` encoder, applying it to each row of the
coefficient matrix with a Rayon pool. The dependency is a pinned Git revision;
the driver needs its own `Cargo.lock`, separate from the main workspace.

## Measurement scope

The measured operation is the Brakedown linear-code encoding of all matrix
rows. Arkworks graph/code setup is timed and reported in `setup_ns` separately.
Deterministic matrix generation is outside the timed interval. Each warm-up and
timed repetition gets a newly generated matrix from a fixed seed sequence. The
encoder's returned rows and their output allocation are included in the timed
operation. Caches are left warm between repetitions.

The pinned arkworks defaults require 128-bit soundness and reject LCPC's
127-bit `Ft127` field, including at the smallest planned benchmark size. The
driver therefore uses arkworks' BN254 scalar field (`BN254-Fr`, generator `5`)
and records its modulus and the mismatch in each report. This is a different
field from LCPC and the field difference must remain visible in comparisons.
The code uses `BrakedownPCParams::default` from the pinned arkworks version.
Effective matrix dimensions are read from those parameters and included in
each report; the setup policy is not LCPC's `SdigCode6` mode.

This driver does not run column hashing, Merkle construction, commit, open, or
verify. Arkworks' linear encoder is generic over the Merkle and column hash
types, so the driver supplies private marker types only to instantiate those
generics. The marker hash methods panic if called. The JSON therefore records
hashing as not invoked and uses `null` for PCS and verifier timings. Do not
interpret this output as an end-to-end PolyCommit measurement or as a hash
comparison. The linearity check encodes `x`, `y`, and `x + y` and verifies
`Enc(x + y) = Enc(x) + Enc(y)` before timings are accepted.

## Build and smoke run

The reproduction script uses the shared host validation and manifest writer.
It checks the Git revision in `Cargo.toml` and `Cargo.lock`, builds the driver
in release mode without the fork's Cargo configuration, runs the linearity
check, then writes JSON and a host manifest.

```sh
. "$HOME/.cargo/env"
sh reproduce/polycommit.sh --threads 1 --cores 3 --logN 16 --runs 2 \
  --warmup 2 --out target-eval-polycommit/smoke
```

The output schema is `lcpc-bench-v1`. Local smoke data is development evidence
only. Published figures must use the GCP c3d machine and the campaign protocol
in `agent/plan/06-evaluation-and-paper-tables.md`.

`Cargo.lock` pins the standalone driver dependencies. The reproduction script
checks the arkworks Git revision in both `Cargo.toml` and `Cargo.lock` and builds
with `--locked`.
