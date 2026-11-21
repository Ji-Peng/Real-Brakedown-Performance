<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 06: Multi-Edge Blocking in R2L Lazy Kernels (`step/06-blocking`)

This document describes the implementation, microarchitectural analysis, and empirical evaluation of **multi-edge blocking** in the right-to-left (R2L) lazy encoding kernels of `lcpc-brakedown-pc`.

---

## 1. Background & Motivation

The R2L kernels evaluate expander graphs via right-to-left sparse gathering (Algorithm 3 of SIMD-Spielman; see [R2L.md](R2L.md)), using the `LazyField` abstraction (`lcpc-fields`; see [LAZY.md](LAZY.md)) so accumulators defer modular reductions until all incoming edges for a given output column have been processed.

In an unblocked gather ($B = 1$), for each output column $r$, incoming edges $(l_0, w_0), (l_1, w_1), \dots$ are processed sequentially across all rows in the batch:

```text
for (left, w) in incoming_prepared_edges:
    for row in 0..n_rows:
        scratch[row] += w * in[left][row]
```

Even though `scratch[row]` fits easily in L1d cache (e.g., $n_{\text{rows}} = 1024$ rows $\times 32$ bytes $= 32\text{ KB}$ for `Mont127Acc`, fitting within a typical 32–48 KB L1d cache), an unblocked loop imposes heavy store and load pressure on the L1 cache interface:
- **1 load + 1 store** to `scratch[row]` per edge per row.
- On typical x86-64 microarchitectures (Intel Skylake/Cypress Cove/Golden Cove, AMD Zen 3/4), the execution core contains 2 load ports and 1 store port per cycle.
- When performing a wide multiply-accumulate on 128-bit or 256-bit wide accumulators, issuing a store to `scratch[row]` on every edge causes store-buffer fill and store-forwarding pressure, limiting loop throughput to memory execution port availability rather than arithmetic execution unit throughput.

---

## 2. Multi-Edge Blocking Architecture

Multi-edge blocking groups $B$ incoming edges together:

$$(l_0, w_0), (l_1, w_1), \dots, (l_{B-1}, w_{B-1})$$

and processes them in a single unrolled inner loop pass over `row in 0..n_rows`:

```text
for chunk in incoming_prepared_edges.chunks_exact(B):
    [w0, ..., w(B-1)] = chunk.weights
    
    for row in 0..n_rows:
        acc = scratch[row]              // 1 load
        acc += w0 * in[chunk[0].left][row]
        acc += w1 * in[chunk[1].left][row]
        ...
        acc += w(B-1) * in[chunk[B-1].left][row]
        scratch[row] = acc              // 1 store
```

### Key Architectural Benefits:
1. **$B\times$ Reduction in Scratch Memory Traffic:**
   - Instead of $B$ loads and $B$ stores to `scratch[row]`, the blocked kernel performs **1 load** and **1 store** for every $B$ edges.
   - For $B = 4$, scratch read/write traffic is cut by **75%** (4× reduction).
2. **Once-per-Code Weight Preparation & Register Hoisting:**
   - `ExpanderCode` transforms raw field weights via `F::prepare_weight` once on first use (`OnceLock`), and the blocked outer loop loads `w0..w(B-1)` once *outside* the inner `n_rows` loop without repeating `prepare_weight` per slice or per call.
3. **Register Reuse:**
   - The multi-limb accumulator `acc` remains pinned in general-purpose registers (GPRs) across all $B$ MACs, chaining additions without touching stack or cache.
4. **Intermediate Overflow & Fold Handling:**
   - For fields with strict accumulation limits (such as $\mathbb{F}_{p^2}$ with $p = 2^{61}-1$, where $M = 32$ accumulations must be folded to prevent 128-bit overflow), the outer loop maintains `since_fold`. If `since_fold + B > F::MAX_ACCUMULATIONS`, it invokes `F::fold(&mut scratch)` before consuming the next chunk.
5. **Bit-Exactness:**
   - Field operations are linear and associative. The blocking factor $B$ only changes the grouping of additions. Output codewords remain 100% bit-identical with upstream LCPC and all golden test vectors.

---

## 3. Microarchitectural GPR Budget Analysis (x86-64)

Standard x86-64 provides 16 general-purpose 64-bit integer registers: `rax, rbx, rcx, rdx, rsi, rdi, rbp, rsp, r8..r15`. Excluding `rsp` (stack pointer) and `rbp` (frame pointer), approximately 14 registers are allocatable without spilling.

Let us evaluate the register footprint for `Mont127` and `Fp2M61`:

### Mont127:
- **Accumulator (`Mont127Acc`):** 5 limbs of `u64` (`a0..a4`) $= 5$ GPRs.
- **Weights:** Each Montgomery weight fits in 2 `u64` limbs (since $p < 2^{127}$, limb 1 is $\le 2^{63}-1$).
  - For $B = 2$: $2 \times 2 = 4$ GPRs for weights. Total $= 5 + 4 = 9$ GPRs.
  - For $B = 4$: While a rough count of active values (5 accumulator limbs plus weights) is around 12–13 GPRs, the generated code does spill (see [CODEGEN.md](CODEGEN.md) §5.2): in the x86-64 `B = 4` inner row loop of `encode_column_block::<Mont127>`, 22 of 113 instructions access the stack (reloaded weights and source pointers, and a spilled partial accumulator); `Mers127` has 20 and `Fp2M61` 34. The choice of `B = 4` rests on measured timings, not on spill-free code.
  - For $B = 8$: Requires $\ge 8 \times 2 = 16$ GPRs for weights alone! This causes severe register spills to the stack on every iteration, destroying performance.

### Fp2M61:
- **Accumulator (`Fp2M61Acc`):** 4 limbs of `u128` (representing $c_0, c_1$ real and imaginary parts).
- **Weights:** Hoisted into registers; additions use `mul`, `add`, and `adc`.
- Similar to `Mont127`, $B = 4$ provides the optimal trade-off between store elimination and register pressure on 64-bit scalar architectures.

---

## 4. Empirical Selection of $B$

During development, single-threaded microbenchmarks across $B \in \{1, 2, 4\}$ over `Mont127` and `Fp2M61` showed that $B = 4$ consistently outperformed $B = 1$ and $B = 2$ while avoiding the severe register spilling observed at $B = 8$. Those exploratory tuning runs were performed during development and are not part of the paper's evaluation; canonical evaluation measurements live in `results/` and are reproduced via `reproduce/run.sh`.

---

## 5. Fixed Constant

- The blocking factor is fixed as a compile-time constant in `lcpc-brakedown-pc/src/encode.rs`:
  ```rust
  pub const BLOCK_FACTOR: usize = 4;
  ```
- It is **not** exposed as a runtime argument or Cargo feature.

---

## 6. Verification & Codegen Invariants

1. **Bit-Exactness Across All Tests:**
   - Full workspace tests run via `reproduce/test.sh` (or `cargo test --workspace --release --locked`).
   - All tests pass, including:
     - Bit-exactness against unmodified LCPC `f0e2116001af5a1bbf39481535f837d6a25772e8` (`lcpc-oracle-tests/tests/original_lcpc.rs`).
     - Bit-exactness against all 18 SIMD-Spielman test vectors (`lcpc-oracle-tests/tests/simd_spielman_tv.rs`).
     - Worst-case accumulator overflow and intermediate fold stress test (`test_worst_case_accumulation_and_fold`).
2. **Disassembly & Vector-Free Verification:**
   - Verified with `python3 scripts/check_codegen.py target-blocking/release/lcpc-bench --strict`:
     ```text
     [INFO] Inspecting target-blocking/release/lcpc-bench (detected arch: x86_64)...
       Result: PASS | Hot kernels checked: 16 | Violations: 0
     ```
   - All 16 hot kernels (including `encode_column_block` for Mont127, Mers127, and Fp2M61) use purely scalar general-purpose instructions (`mov`, `add`, `adc`, `mul`, `imul`), with zero vector registers (`xmm`, `ymm`, `zmm`) utilized.
