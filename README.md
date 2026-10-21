# Real Brakedown Performance (LCPC fork)

This repository is a fork of [LCPC](https://github.com/conroi/lcpc) by Riad S. Wahby
(Apache-2.0, see `LICENSE`), imported with its full history at upstream commit
`f0e2116001af5a1bbf39481535f837d6a25772e8`. The original upstream README is kept
unchanged in [`README-lcpc.md`](README-lcpc.md).

The fork integrates the scalar/software optimizations analysed in *High-Performance
SIMD Software for Spielman Codes in Zero-Knowledge Proofs* (Krieger, Dobrouschek,
Hirner, Sinha Roy; TCHES 2026) into LCPC's Brakedown (SDIG) encoder, without explicit
SIMD: portable stable Rust, compiler-generated scalar code. Each technique is added as
one tagged commit (`step/NN-<name>`) on top of the import (`step/00-import`), so the
history itself documents what changed relative to upstream.

Build, test, and reproduction instructions will be added as the work progresses.
