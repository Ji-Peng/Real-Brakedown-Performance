<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# arkworks PolyCommit Brakedown encoding driver

This standalone stable-Rust driver benchmarks arkworks `poly-commit` at commit
`1eaec5090ba5bc87d75a69570ed38f6fcf8b9417`. It calls the public
`linear_codes::MultilinearBrakedown` encoder, applying it to each row of a
square coefficient matrix with a Rayon pool. The dependency is pinned to Git
revision `1eaec5090ba5bc87d75a69570ed38f6fcf8b9417`; the standalone driver has
its own `Cargo.lock`, separate from the main workspace. At build time the
reproduction script applies `polycommit.patch` to a temporary copy of that
revision and points Cargo at the patched copy. It verifies the source commit,
patch SHA-256, and lockfile pin before building. The cached Cargo checkout and
the committed `Cargo.toml`/`Cargo.lock` are never edited.

## Measurement scope

The measured operation is the Brakedown linear-code encoding of all matrix
rows. Arkworks graph/code setup is timed and reported in `setup_ns` separately.
Deterministic matrix generation is outside the timed interval. Each warm-up and
timed repetition gets a newly generated matrix from a fixed seed sequence. The
encoder's returned rows and their output allocation are included in the timed
operation. Caches are left warm between repetitions.

The pinned `BrakedownPCParams::default` hard-codes `sec_param = 128`, then calls
`calculate_t`. Its bound requires `N / |F| < 2^-128`, which cannot hold over
LCPC's 127-bit `Mont127` prime at any positive `N`. [KDHR26, Table 2]
nevertheless measures PolyCommit with that prime. The patch adds
`BrakedownPCParams::with_dims`: it retains arkworks' default code constants and
the same private matrix sizing and graph generation, but accepts explicit
matrix dimensions and a caller-selected opening `sec_param`. The driver uses
the square shape `k = sqrt(N)` rows and `sec_param = 80`, which the field
supports at the planned sizes. The security parameter is used for Brakedown's
column-opening query count; it does not affect graph generation or linear
encoding work. Every record includes `patched: true`, the patch hash, and the
selected `sec_param`. Results therefore use the same 127-bit prime as LCPC,
while the reduced opening parameter is stated explicitly.

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
It obtains the pinned source from the Cargo Git checkout when available, or
clones it when online; checks the resolved Git revision and patch SHA-256;
applies the patch only to an archived temporary copy; and builds offline in
release mode without the fork's Cargo configuration. It then runs the linearity
check at the selected opening parameter and writes the shared JSON schema plus
a host manifest. Cargo uses the committed lockfile as the starting point, with
only the patched `ark-poly-commit` source entry changed to a local path in the
temporary build copy. This preserves all pinned dependency versions without
changing the committed driver manifest or lockfile.

```sh
. "$HOME/.cargo/env"
sh reproduce/polycommit.sh --threads 1 --cores 3 --logN 12,16 --runs 2 \
  --warmup 1 --out target-eval-polycommit/smoke
```

The output schema is `lcpc-bench-v1`. Local smoke data is development evidence
only. Published figures must use the GCP c3d machine and the campaign protocol
in `agent/plan/06-evaluation-and-paper-tables.md`.

`Cargo.lock` pins the standalone driver's Git dependency and transitive crates.
The generated build copy uses the same locked package versions with a path
override for the one patched crate; the generated lockfile and patched source
are temporary build inputs and are not committed.
