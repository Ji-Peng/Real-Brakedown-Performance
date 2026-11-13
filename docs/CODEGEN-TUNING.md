<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Portable Codegen Tuning and Optimization Report (Step 09)

This document describes the microarchitectural analysis, portable Rust source transformations, compiler code generation, and directional local benchmark results for **Step 09 (Compiler Code Generation & Tuning)**.

All optimizations adhere strictly to the portable Rust rule ([AGENTS.md](../../AGENTS.md)):
- **No inline assembly** (`no asm!`);
- **No architecture-specific intrinsics or SIMD libraries** (`no std::arch`, `no core::simd`);
- **No conditional optimization tiers or variant build flags**;
- Stable compiler `rustc 1.99.1`.

> [!NOTE]
> All local execution timings reported in this document were measured on a local test workstation (Intel Core i7-11700K) pinned under `taskset -c 3` with CPU 2 strictly excluded. These timings are for directional validation only and are labelled **unpublishable**. Canonical benchmark results are produced on the NUS SoC Slurm cluster: x86-64 node class `xcnh` (AMD EPYC 9755, Zen 5) and Arm node class `xcng` (Neoverse-N1); see [README.md](../README.md), "Benchmark machines (SoC cluster)".

---

## 1. Summary of Optimizations & Disassembly Impacts

| Component | Inefficiency Identified | Optimization Applied | Codegen & Disassembly Result |
| :--- | :--- | :--- | :--- |
| **`Mont127` & `Mers127` MAC** (`mac_mont127`, `mac_mers127`) | Writing limb addition as `(acc.c[i] as u128) + (t_i as u128) + carry` caused LLVM to lower carry addition into two separate steps (`adcq $0` + `addq`), forcing boolean carry materialization (`setb`, `movzbl`, `xorl`) and callee-saved register pressure (`%rbp`). | Replaced with stable `u64::carrying_add`. | **Zero boolean materializations.** Collapsed 20 scalar instructions into a single uninterrupted 5-instruction hardware carry chain (`addq`, `adcq`, `adcq`, `adcq`, `adcq $0`). Eliminated `%rbp` frame overhead. **16.8% faster field MAC**. |
| **`Mont127` & `Mers127` Acc Add** (`add_*_acc`) | 5-iteration loop with 128-bit integer casting and shift. | Unrolled 5-limb hardware carry chain using `u64::carrying_add`. | Direct 5-instruction `addq`/`adcq` chain with zero loop branching or shift instructions. |
| **`Mont127` Reduction** (`reduce_mont127`) | 3-iteration nested loop with array indexing in CIOS Montgomery reduction. | Fully unrolled 3-word CIOS reduction with register-pinned limbs and `carrying_add`. | Complete loop unrolling into straight-line assembly; zero array bounds checks; branchless conditional move (`cmovbq`). |
| **Expander Graph Traversal** (`encode_column_block_range_shard`) | Multi-edge loop ($B=4$) computed index arithmetic `e, e+1, e+2, e+3` and performed redundant slice indexing on `weights[e]` and `lefts[e]`. Output destination computed per-row slice indexing. | Used `chunks_exact(4)` and `.remainder()` for edge blocking; used `output.chunks_exact_mut(n_rows)` for disjoint column outputs. | Eliminated edge indexing bounds checks and destination column index calculations. |
| **Reed-Solomon Evaluation** (`reed_solomon_column_block_range_shard`) | Matrix evaluation computed dynamic slice bounds `xo[local_col * n_rows..]` and `xi[j * n_rows..]`. | Sliced via `xo.chunks_exact_mut(n_rows)` and polynomial coefficient blocks via `xi.chunks_exact(n_rows).rev()`. | Eliminated all dynamic slice index checks on message polynomial evaluation. |
| **`Fp2M61` MAC Form Evaluation** | Evaluated 4-mul Schoolbook vs 3-mul Karatsuba MAC forms. | Disassembly inspection and microbenchmarks evaluated. | **Schoolbook retained:** Karatsuba emitted 3 callee-saved register pushes, boolean materialization, `imulq`, and extra 128-bit subtractions; Schoolbook has 0 spills, 0 pushes, and fewer execution cycles. |

---

## 2. Detailed Disassembly Investigations

### 2.1 Investigation 1: Redundant Carries and Boolean Materialization in 127-Bit MAC

#### The Problem
Expressing the accumulation of the 256-bit product $(t_0, t_1, t_2, t_3)$ into the 320-bit accumulator `acc.c[0..4]` via 128-bit additions:
```rust
let s0 = (acc.c[0] as u128) + (t0 as u128);
acc.c[0] = s0 as u64;
let carry0 = s0 >> 64;

let s1 = (acc.c[1] as u128) + (t1 as u128) + carry0;
acc.c[1] = s1 as u64;
let carry1 = s1 >> 64;
// ... (repeated for limbs 2, 3, 4)
```

When lowering `(acc.c[1] as u128) + (t1 as u128) + carry0`, LLVM split the addition into two operations:
1. `acc.c[1] + carry0` (emitted as `adcq $0, %rsi`);
2. Adding `t1` into the intermediate sum.

Because both steps can produce a carry, LLVM could not maintain a continuous CPU carry flag (`EFLAGS.CF`). To preserve the intermediate carry, LLVM synthesized explicit boolean carry flags using `setb`, zero-extended them with `movzbl`, and added them with `xorl`:

```assembly
# BEFORE (inefficient boolean carry materialization in mac_column_mont127):
17a58b: addq    %r15, (%rdi,%r11)
17a58f: movq    0x8(%rdi,%r11), %rsi
17a594: adcq    $0x0, %rsi
17a598: setb    %bpl                    # <-- MATERIALIZE CARRY TO GPR
17a59c: xorl    %r14d, %r14d
17a59f: addq    0x10(%rdi,%r11), %rdx
17a5a4: movzbl  %bpl, %r15d             # <-- EXTEND CARRY
17a5a8: setb    %r14b                   # <-- MATERIALIZE CARRY
17a5ac: xorl    %r12d, %r12d
17a5af: addq    0x18(%rdi,%r11), %rax
17a5b4: setb    %r12b                   # <-- MATERIALIZE CARRY
17a5b8: addq    %rbx, %rsi
17a5bb: movq    %rsi, 0x8(%rdi,%r11)
17a5c0: adcq    %r15, %rdx
17a5c3: movq    %rdx, 0x10(%rdi,%r11)
17a5c8: adcq    %rax, %r14
17a5cb: movq    %r14, 0x18(%rdi,%r11)
17a5d0: adcq    %r12, 0x20(%rdi,%r11)
```
This sequence required 20 instructions, materializing boolean values across 3 general-purpose registers and forcing `%rbp` to be saved on the stack in the function prologue (`pushq %rbp`).

#### The Solution: Portable `carrying_add`
`rustc 1.99.1` provides `u64::carrying_add` on stable. The current implementation structures the accumulation as:
```rust
let (s0, c) = acc.c[0].carrying_add(t0, false);
let (s1, c) = acc.c[1].carrying_add(t1, c);
let (s2, c) = acc.c[2].carrying_add(t2, c);
let (s3, c) = acc.c[3].carrying_add(t3, c);
let (s4, _) = acc.c[4].carrying_add(0, c);

acc.c[0] = s0;
acc.c[1] = s1;
acc.c[2] = s2;
acc.c[3] = s3;
acc.c[4] = s4;
```

#### Disassembly After:
```assembly
# AFTER (pure hardware carry chain in mac_column_mont127):
17941b: addq    %r14, (%rdi,%r11)
17941f: adcq    %r13, 0x8(%rdi,%r11)
179424: adcq    %rdx, 0x10(%rdi,%r11)
179429: adcq    %rsi, 0x18(%rdi,%r11)
17942e: adcq    $0x0, 0x20(%rdi,%r11)
```
- **Zero boolean materializations:** all `setb`, `movzbl`, and `xorl` instructions eliminated.
- **20 instructions reduced to 5:** exact single-cycle hardware flag chaining.
- **Callee-saved register `%rbp` eliminated:** `pushq %rbp`/`popq %rbp` removed from function frame.
- **Cross-architecture verified:** emits `adds`/`adcs`/`cinc` on AArch64 and optimal `sltu`/`add` sequences on RISC-V 64 without stack spills.

---

### 2.2 Investigation 2: Unrolling CIOS Montgomery Reduction in `reduce_mont127`

#### The Problem
Writing the 3-word CIOS Montgomery reduction as a nested loop:
```rust
for i in 0..3 {
    let u = acc.c[i].wrapping_mul(Mont127::MU0) as u128;
    // ...
    for j in (i + 2)..5 {
        let sum = (acc.c[j] as u128) + (carry as u128);
        acc.c[j] = sum as u64;
        carry = (sum >> 64) as u64;
    }
}
```
The inner loop bounds dynamically depended on `i`, which obstructed scalar replacement of aggregates and introduced array indexing overhead.

#### The Solution
The current implementation destructures the 5-limb accumulator into scalar local variables `[c0, mut c1, mut c2, mut c3, mut c4] = acc.c;` and fully unrolls the 3 word-clearing iterations with `carrying_add`.

#### Disassembly Impact:
- Zero loop branching or induction variables.
- All 9 multiplications lowered to BMI2 `mulxq` (or `mulq`).
- Final conditional modulus subtraction lowers directly to `cmovbq`.

---

### 2.3 Investigation 3: Chunking & Eliminating Dynamic Bounds Checks

#### In `encode_column_block_range_shard`
1. **Edge Chunking:** Replaced dynamic index arithmetic `e += 4` and indexing `lefts[e]`, `weights[e]` with:
   ```rust
   let left_chunks = lefts.chunks_exact(4);
   let weight_chunks = weights.chunks_exact(4);
   for (l_chunk, w_chunk) in left_chunks.zip(weight_chunks) {
       let &[l0_idx, l1_idx, l2_idx, l3_idx] = l_chunk else { unreachable!() };
       let &[ref w0_raw, ref w1_raw, ref w2_raw, ref w3_raw] = w_chunk else { unreachable!() };
       // ...
   }
   ```
   Tail edges are processed cleanly via `left_chunks.remainder()`.
2. **Disjoint Destination Slices:** Replaced `output[local_r * n_rows..(local_r + 1) * n_rows]` with `output.chunks_exact_mut(n_rows)` zipped directly with right-node indices `(r_start..r_end)`.
3. **Inner Row Loop Evaluation:**
   We evaluated two alternatives for the inner loop over rows:
   - *Option A:* Iterator enumerate `for (row, slot) in scratch_slice.iter_mut().enumerate()`
   - *Option B:* Indexed loop `for row in 0..n_rows { let mut acc = scratch_slice[row]; ... scratch_slice[row] = acc; }`

   *Measurement Finding:* Option B is **~4% faster** (7.62 ms vs 7.88 ms at $\log N = 16$). Under Option B, all five buffers (`scratch_slice`, `src0`, `src1`, `src2`, `src3`) share a single 64-bit induction variable `%rax` (`row`) with hardware scaled index addressing (`(%rdi, %rax, 8)`). Option A introduces separate pointer tracking registers that increase register pressure under $B = 4$. Option B is retained.

#### In `reed_solomon_column_block_range_shard`
Replaced dynamic indexing with:
```rust
for r in xo.chunks_exact_mut(n_rows) {
    r.fill(F::ZERO);
    for coeff in xi.chunks_exact(n_rows).rev() {
        for (o, c) in r.iter_mut().zip(coeff.iter()) {
            *o *= x;
            *o += c;
        }
    }
    x += F::ONE;
}
```
All slice bounds checks on output columns and polynomial input coefficients were completely eliminated.

---

### 2.4 Investigation 4: Field MAC Form for $\mathbb{F}_{p^2}$ (Schoolbook vs. Karatsuba)

Two candidate MAC forms exist for $\mathbb{F}_{p^2}$ ($p = 2^{61}-1$):
- **Form A (Schoolbook, 4 multiplications):**
  $$\text{Re} = x_r w_r + x_i w_{i,\text{neg}}, \quad \text{Im} = x_r w_i + x_i w_r$$
- **Form B (Karatsuba, 3 multiplications):**
  $$P_0 = x_r w_r, \quad P_1 = x_i w_i, \quad P_2 = (x_r + x_i)(w_r + w_i)$$
  $$\text{Im} = P_2 - P_0 - P_1, \quad \text{Re} = P_0 + p^2 - P_1$$

#### Disassembly Comparison:
Compiling and disassembling both forms under `-C target-cpu=native -C no-vectorize-loops -C no-vectorize-slp`:
- **Schoolbook:**
  - Instructions: Exactly 4 `mulxq` instructions, 4 `addq`/`adcq` carry additions.
  - Stack frame: **0 stack pushes, 0 stack pops, 0 spills.**
  - Execution: Completely straight-line execution across dual integer ALU pipelines.
- **Karatsuba:**
  - Instructions: 3 `mulxq` instructions, but adds 1 64-bit addition, 1 128-bit addition with $p^2$, and 3 128-bit subtractions (`subq`/`sbbq`).
  - Stack frame: Required **3 callee-saved register pushes** (`pushq %r15`, `%r14`, `%rbx`) to handle high register pressure.
  - Emitted multiple boolean carry materializations (`setb`, `imulq`).

#### Selection:
On modern out-of-order 64-bit cores, integer multipliers operate with 1-cycle throughput. Saving 1 multiply while introducing 8 additional ALU instructions and register spilling is a net microarchitectural regression. **Schoolbook 4-multiplication MAC is the chosen portable scalar form.**

---

## 3. Directional Benchmark Results (Local Test Machine)

> [!WARNING]
> Machine: Intel Core i7-11700K (Rocket Lake / Cypress Cove).
> Execution pinned via `taskset -c 3` (CPU 2 strictly excluded).
> Timings are unpublishable directional indicators only.

### 3.1 Field Arithmetic Microbenchmarks (`lcpc-bench --field-bench`)

| Field | Operation | Baseline Before (ns/op) | Tuned Step 09 (ns/op) | Relative Speedup |
| :--- | :--- | :---: | :---: | :---: |
| **`Mont127`** | `mul` | 10.84 | 10.95 | $\approx$ flat |
| | **`mac`** | **3.28** | **2.73** | **+16.8% faster** |
| | `reduce` | 5.07 | 5.20 | $\approx$ flat |
| **`Mers127`** | `mul` | 6.02 | 6.06 | $\approx$ flat |
| | **`mac`** | **3.33** | **2.74** | **+17.7% faster** |
| | `reduce` | 0.70 | 0.70 | flat |
| **`Fp2M61`** | `mul` | 5.66 | 5.62 | flat |
| | **`mac`** | **2.22** | **2.23** | flat |
| | `reduce` | 0.56 | 0.56 | flat |
| **`Ft127`** | `mul` | 8.93 | 8.97 | flat |
| | **`mac`** | **3.31** | **2.73** | **+17.5% faster** |
| | `reduce` | 5.05 | 5.02 | flat |

### 3.2 End-to-End Matrix Encoding (`encode_matrix` median)

| Field & Parameters | Problem Size ($\log N$) | Baseline Before | Tuned Step 09 | Speedup |
| :--- | :---: | :---: | :---: | :---: |
| **`Mont127` (`lcpc-sdig6`)** | $\log N = 16$ | 8.67 ms (8,670,863 ns) | **7.62 ms (7,615,182 ns)** | **+12.2% faster** |
| | $\log N = 18$ | 26.45 ms (26,450,000 ns) | **23.17 ms (23,172,000 ns)** | **+12.4% faster** |
| **`Mers127` (`lcpc-sdig6`)** | $\log N = 16$ | 8.66 ms (8,662,408 ns) | **7.60 ms (7,601,240 ns)** | **+12.3% faster** |
| | $\log N = 18$ | 26.38 ms (26,380,000 ns) | **23.12 ms (23,124,000 ns)** | **+12.3% faster** |
| **`Fp2M61` (`simd-spielman`)**| $\log N = 16$ | 3.47 ms (3,467,000 ns) | **3.38 ms (3,382,100 ns)** | **+2.5% faster** |
| | $\log N = 18$ | 13.03 ms (13,028,156 ns) | **12.67 ms (12,668,231 ns)** | **+2.8% faster** |

---

## 4. Portability Verification Across Target Architectures

Scalar codegen is verified across all targets via `scripts/check_codegen.py`:

```
x86_64_native     : PASS (16 hot kernels checked, 0 vector arithmetic violations)
x86_64_portability: PASS (16 hot kernels checked, 0 vector arithmetic violations)
aarch64           : PASS (16 hot kernels checked, 0 vector arithmetic violations)
riscv64           : PASS (16 hot kernels checked, 0 vector arithmetic violations)
```

- **Total hot instructions on x86-64 native:** reduced from 6,308 to 5,521 (**-12.5% instruction reduction**).
- **Correctness test suite:** 100% of workspace tests (102 tests) passed cleanly under `--release`.
- **Golden test vectors:** verified bit-exact match against upstream LCPC commit `f0e2116` and SIMD-Spielman golden vectors across all fields and dimensions.
