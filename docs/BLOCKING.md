<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Step 06: Multi-Edge Blocking in R2L Lazy Kernels (`step/06-blocking`)

This document records the implementation, microarchitectural analysis, and empirical evaluation of **multi-edge blocking** (Plan 01 §3.5, Plan 03 §2.2, Decision D8) in the right-to-left (R2L) lazy encoding kernels of `lcpc-brakedown-pc`.

---

## 1. Background & Motivation

In `step/04-r2l`, graph evaluation was reformulated from left-to-right (L2R) sparse scattering into right-to-left (R2L) sparse gathering (Algorithm 3 of SIMD-Spielman). In `step/05-lazy`, we introduced the `LazyField` abstraction (`lcpc-fields`), allowing accumulators to defer expensive modular reductions until all incoming edges for a given output column have been processed.

In an unblocked gather ($B = 1$), for each output column $r$, incoming edges $(l_0, w_0), (l_1, w_1), \dots$ are processed sequentially across all rows in the batch:

```text
for (left, weight) in incoming_edges:
    w = prepare_weight(weight)
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
for chunk in incoming_edges.chunks_exact(B):
    w0 = prepare_weight(chunk[0].weight)
    ...
    w(B-1) = prepare_weight(chunk[B-1].weight)
    
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
2. **Weight Preparation Hoisting:**
   - `F::prepare_weight(w)` converts raw field weights (e.g., Montgomery form or precomputed multiplier format) once *outside* the inner `n_rows` loop.
   - For a batch of $n_{\text{rows}} = 1024$, weight conversion cost is amortized over 1024 rows ($< 0.1\%$ overhead).
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
  - For $B = 4$: LLVM holds weights in registers or uses single-limb constants where applicable. Total active registers $\approx 12\text{--}13$ GPRs, which this estimate placed within the 14 available GPRs. **Corrected (see `docs/CODEGEN.md` §5.2):** the generated code does spill: in the x86-64 `B = 4` inner row loop of `encode_column_block::<Mont127>`, 22 of 113 instructions access the stack (reloaded weights and source pointers, a spilled partial accumulator); `Mers127` has 20 and `Fp2M61` 34. The choice of `B = 4` rests on measured timings, not on spill-free code.
  - For $B = 8$: Requires $\ge 8 \times 2 = 16$ GPRs for weights alone! This causes severe register spills to the stack on every iteration, destroying performance.

### Fp2M61:
- **Accumulator (`Fp2M61Acc`):** 4 limbs of `u128` (representing $c_0, c_1$ real and imaginary parts).
- **Weights:** Hoisted into registers; additions use `mul`, `add`, and `adc`.
- Similar to `Mont127`, $B = 4$ provides the optimal trade-off between store elimination and register pressure on 64-bit scalar architectures.

---

## 4. Empirical Evaluation & Selection of $B$

> [!NOTE]
> **Measurement Notice:** The following benchmarks were conducted locally on the development machine (Intel Core i7-11700K, pinned to Core 3 via `taskset -c 3`, turbo boost disabled, single-threaded, `params = lcpc-sdig6`, 10 runs, 2 warmups). As dictated by the project instructions, these numbers are **direction-only engineering measurements** intended solely to guide the choice of the compile-time constant $B$. They are not published benchmark numbers.

### Median Matrix Encoding Time (`encode_matrix`)

| Field | $\log_2(N)$ | $B = 1$ (unblocked lazy) | $B = 2$ | $B = 4$ | Speedup ($B=4$ vs $B=1$) |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Mont127** | 16 | 10.22 ms | 9.25 ms | **8.89 ms** | **+13.0%** ($1.15\times$) |
| **Mont127** | 18 | 31.50 ms | 28.85 ms | **27.83 ms** | **+11.7%** ($1.13\times$) |
| **Mont127** | 20 | 122.16 ms | 112.72 ms | **109.71 ms** | **+10.2%** ($1.11\times$) |
| **Fp2M61** | 16 | 5.76 ms | 5.47 ms | **5.29 ms** | **+8.2%** ($1.09\times$) |
| **Fp2M61** | 18 | 19.95 ms | 18.68 ms | **18.37 ms** | **+7.9%** ($1.09\times$) |
| **Fp2M61** | 20 | 76.82 ms | 71.81 ms | **71.71 ms** | **+6.7%** ($1.07\times$) |

### Comparison with Prior Eager Baseline (`step/04-r2l`):
- **Mont127 ($\log_2 N = 16$):** Eager was **44.4 ms** $\longrightarrow$ Blocked Lazy ($B=4$) is **8.89 ms** (**$5.0\times$ faster**).
- **Fp2M61 ($\log_2 N = 16$):** Eager was **12.8 ms** $\longrightarrow$ Blocked Lazy ($B=4$) is **5.29 ms** (**$2.4\times$ faster**).

### Analysis:
- In all test cases and across all matrix sizes ($\log_2 N \in \{16, 18, 20\}$), **$B = 4$ consistently outperforms $B = 2$ and $B = 1$**.
- Mont127 sees a greater relative improvement from blocking (+10.2% to +13.0%) than Fp2M61 (+6.7% to +8.2%), as Mont127's 5-limb accumulator involves wider memory transfers per load/store, making the $4\times$ reduction in store port traffic especially impactful.

---

## 5. Decision & Fixed Constant

In accordance with Decision D8 and project requirements:
- The blocking factor is fixed as a compile-time constant:
  ```rust
  pub const BLOCK_FACTOR: usize = 4;
  ```
  in `lcpc-brakedown-pc/src/encode.rs`.
- It is **not** exposed as a runtime argument or Cargo feature.

---

## 6. Verification & Codegen Invariants

1. **Bit-Exactness Across All Tests:**
   - Full workspace tests run with `taskset -c 0,1,3-7 cargo test --workspace --release`.
   - All tests pass, including:
     - Bit-exactness against unmodified LCPC `f0e2116001af5a1bbf39481535f837d6a25772e8` (`lcpc-oracle-tests/tests/original_lcpc.rs`).
     - Bit-exactness against all 18 SIMD-Spielman test vectors (`lcpc-oracle-tests/tests/simd_spielman_tv.rs`).
     - Worst-case accumulator overflow and intermediate fold stress test (`test_worst_case_accumulation_and_fold`).
2. **Disassembly & Vector Free Verification:**
   - Executed `python3 scripts/check_codegen.py target-blocking/release/lcpc-bench --strict`.
   - Result:
     ```text
     [INFO] Inspecting target-blocking/release/lcpc-bench (detected arch: x86_64)...
       Result: PASS | Hot kernels checked: 16 | Violations: 0
     ```
   - All 16 hot kernels (including `encode_column_block` for Mont127, Mers127, and Fp2M61) use purely scalar general-purpose instructions (`mov`, `add`, `adc`, `mul`, `imul`), with zero vector registers (`xmm`, `ymm`, `zmm`) utilized.
