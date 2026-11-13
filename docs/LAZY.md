<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 05: Lazy Modular Reduction in R2L Gather Kernels (`step/05-lazy`)

This document describes **lazy modular reduction** in the right-to-left (R2L) expander graph gather kernels of `lcpc-brakedown-pc`.

---

## 1. Background & Motivation

Right-to-left (R2L) graph evaluation replaces random L2R read-modify-writes with linear writes to the output columns (see [R2L.md](R2L.md)). With eager field arithmetic, every incoming edge would perform a full modular multiplication and modular addition per row; lazy reduction defers those reductions.

Under Brakedown's `SdigCode6` parameters ($\alpha = 0.238, \beta = 0.1205, r = 1.72$):
- In mode (b) (`SimdSpielman`), average right-node in-degrees are $\overline{\text{deg}}(G_1) \approx 42.02$ in precodes and $\overline{\text{deg}}(G_2) \approx 26.36$ in postcodes.
- In mode (a) (`Lcpc`), average right-node in-degrees range from 28 to 79 across levels.

Because R2L gathers all incoming edges for each right (output) node $r$ before moving to the next node, we only need to keep the accumulators for a single output column live at any moment (the `scratch` buffer of length $n_{\text{rows}}$). For a batch of $n_{\text{rows}} = 1024$ and 32–40 bytes per accumulator, `scratch` occupies 32–40 KiB, fitting entirely within core L1d cache. Deferring modular reduction until all incoming edges of right node $r$ have been accumulated replaces roughly 26–42 modular reductions with **exactly one reduction per output column**.

---

## 2. Kernel Design (`lcpc-brakedown-pc/src/encode.rs`)

`encode_column_block<F: LazyField>` evaluates an expander graph on a column block:

```rust
pub fn encode_column_block<F: LazyField>(
    graph: &R2lGraph<F>,
    input: &[F],
    output: &mut [F],
    n_rows: usize,
) {
    let mut scratch = vec![F::Acc::default(); n_rows];

    for (r, dst) in output.chunks_exact_mut(n_rows).enumerate() {
        let (lefts, weights) = graph.incoming(r);
        let num_edges = lefts.len();
        if num_edges == 0 {
            dst.fill(F::ZERO);
            continue;
        }

        let mut since_fold = 0;

        for e in 0..num_edges {
            if since_fold >= F::MAX_ACCUMULATIONS {
                for acc in scratch.iter_mut() {
                    F::fold(acc);
                }
                since_fold = 1;
            }

            let w = F::prepare_weight(&weights[e]);
            let l = lefts[e] as usize * n_rows;
            let src = &input[l..l + n_rows];

            for (acc, s) in scratch.iter_mut().zip(src) {
                F::mac(acc, &w, s);
            }

            since_fold += 1;
        }

        for (d, acc) in dst.iter_mut().zip(scratch.iter_mut()) {
            *d = F::reduce(*acc);
            *acc = F::Acc::default();
        }
    }
}
```

### Key Principles:
1. **Unblocked Reference Structure vs. Multi-Edge Blocking**: The snippet above shows the unblocked ($B = 1$) lazy reduction loop; the current implementation adds $B = 4$ multi-edge blocking on top of this same `LazyField` structure (see [BLOCKING.md](BLOCKING.md)).
2. **Scratch Buffer in L1d**: `scratch` is allocated once and reused across all output columns $r$. For each column, it accumulates unreduced products and is zeroed upon reduction.
3. **Weight Preparation**: `F::prepare_weight(&weights[e])` transforms raw edge weights into the representation expected by `F::mac` outside the inner row loop:
   - For `Mont127`: computes $w \cdot R_{192} \bmod q$, enabling 3-word Montgomery reduction ($R = 2^{192}$).
   - For `Mers127` and `Fp2M61`: identity operation.
4. **Intermediate Folding for Arbitrary Degrees**:
   - `Mont127` and `Mers127` have capacity for $D \le 2^{64}$ accumulations without overflow (`MAX_ACCUMULATIONS = usize::MAX`).
   - `Fp2M61` accumulates in two `u128` limbs with exact headroom for $D = 32$ edges (`MAX_ACCUMULATIONS = 32`). When `since_fold >= 32`, `F::fold` compresses both limbs back down to $< 2^{62}$ with 4 bitwise shift/mask instructions, restoring headroom for another 31 MACs.
5. **Universal Support**: The kernel is generic over `F: LazyField`, handling all three field types (`Mont127`, `Mers127`, `Fp2M61`), test fields (`Ft63`, `Ft127`, `Ft191`, `Ft255`), both code-parameter modes (mode (a) and mode (b)), and any batch size ($n_{\text{rows}} \ge 1$, including verifier encoding $n_{\text{rows}} = 1$).

---

## 3. Verification & Correctness

1. **Bit-Exactness**:
   - All 18 SIMD-Spielman test vectors (`tv/`) across `Mont127`, `Mers127`, and `Fp2M61` (Brakedown and Orion shapes, $N \in \{2^{12}, 2^{14}, 2^{16}\}$) match bit-for-bit.
   - Original LCPC oracle tests match bit-for-bit with upstream `f0e2116`.
   - Golden vectors for mode (a) match upstream LCPC bit-for-bit.
2. **Worst-Case Accumulation & Fold Testing**:
   - `test_worst_case_accumulation_and_fold` tests degree-100 graphs with worst-case $(p-1)$ inputs and weights across `Mont127`, `Mers127`, and `Fp2M61` for both single-row ($n_{\text{rows}} = 1$) and batched ($n_{\text{rows}} = 8$) configurations, verifying correct intermediate folding when degrees exceed `MAX_ACCUMULATIONS`.
3. **Scalar Codegen Verification**:
   - Disassembly is verified with `scripts/check_codegen.py --strict`: **0 vector-register arithmetic violations** in `encode_column_block` and all hot field kernels across all targets.
