<!-- Copyright 2026 RealBrakedownPerf Authors. Licensed under Apache-2.0. -->

# Compiler Code Generation and Portability Report

This document reports on the scalar assembly codegen, portability verification, and microarchitectural behavior of the LCPC fork in [`code/`](../) across target 64-bit architectures (`x86_64`, `aarch64`, `riscv64gc`), following [Plan 03 (64-Bit CPU Microarchitecture and Compiler Code Generation)](../../agent/plan/03-cpu-microarchitecture-and-codegen.md).

All inspection listings and machine-readable JSON reports are archived under [`docs/codegen/`](codegen/).

---

## 1. Design Principle: Portable Rust, Compiler-Emitted Scalar Code

As established in [CLAUDE.md](../../.claude/CLAUDE.md) and Plan 03 §1, the project maintains **one implementation**: the stable Rust LCPC fork. It contains:
- **No inline assembly:** `no asm!`;
- **No architecture-specific intrinsics or SIMD libraries:** `no std::arch`, `no core::arch`, `no core::simd`;
- **No SIMD-accelerated dependencies:** `blake3` is compiled in portable-only mode (`features = ["pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]`);
- **Auto-vectorization disabled:** crates are compiled with `-C no-vectorize-loops -C no-vectorize-slp`.

The scalar machine code is generated entirely by the compiler (`rustc 1.99.1` and LLVM 23.1.1). The hot loops subjected to automated scalar disassembly inspection are:
1. **Field lazy MAC (`LazyField::mac` / `mac_column_*`):** accumulating unreduced products ($w \cdot x$) across 64-bit limbs;
2. **Right-node modular reduction (`LazyField::reduce` / `reduce_column_*`):** normalizing multi-limb accumulators modulo $q$ (or $p = 2^{61}-1$) at the end of each right node;
3. **Blocked column-major expander loop (`encode_column_block`):** applying $B = 4$ blocked edges to column slices in `lcpc-brakedown-pc`;
4. **Reed-Solomon evaluation loop (`reed_solomon_column_block`):** Vandermonde polynomial evaluation via Horner's rule over column blocks.

---

## 2. Portability Build & Test Suite Verification

### 2.1 Portability Build (`x86_64` Baseline without `target-cpu=native`)

A portability build compiles without host-specific CPU target extensions (`-C target-cpu=native` omitted), ensuring the workspace targets baseline x86-64-v1 and relies on no machine-specific assumptions or instructions.

**Command Executed:**
```bash
RUSTFLAGS="-C no-vectorize-loops -C no-vectorize-slp" cargo test --workspace --release
```

**Results:**
- All workspace crates (`lcpc-2d`, `lcpc-fields`, `lcpc-test-fields`, `lcpc-brakedown-pc`, `lcpc-spielman-oracle`, `lcpc-ligero-pc`, `lcpc-oracle-tests`, `bench`) compiled cleanly without errors or warnings.
- **100% of tests passed** (101 unit/integration tests passed, 0 failures, 6 ignored bench markers):
  - `lcpc-2d`: 7 passed;
  - `lcpc-brakedown-pc`: 28 passed, 2 ignored benches;
  - `lcpc-fields` (`tests/fields_test.rs`): 14 passed (axioms, constants, boundary conditions, differential vs bigint oracle, and scalar oracle match);
  - `lcpc-ligero-pc`: 6 passed, 3 ignored benches;
  - `lcpc-oracle-tests` (`tests/original_lcpc.rs`): 3 passed (bit-exact verification against upstream LCPC `f0e2116`);
  - `lcpc-oracle-tests` (`tests/simd_spielman_tv.rs`): 18 passed (golden test vectors for Brakedown and Orion across all fields and dimensions $N=12, 14, 16$);
  - `lcpc-spielman-oracle`: 23 passed;
  - `lcpc-test-fields`: 2 passed.

### 2.2 Cross-Target Compilation & Missing Tool Report

The toolchain components for `aarch64-unknown-linux-gnu` and `riscv64gc-unknown-linux-gnu` were added via `rustup`:
```bash
rustup target add aarch64-unknown-linux-gnu riscv64gc-unknown-linux-gnu
```

#### Compilation Status:
- `cargo check --workspace --release --target aarch64-unknown-linux-gnu` and `cargo check --workspace --release --target riscv64gc-unknown-linux-gnu` **succeeded with exit code 0** across all workspace members.
- Full ELF relocatable machine object files (`.o`, `.rcgu.o`) and assembly listings (`.s`) were emitted for all crates, including the full benchmark driver (`lcpc-bench`), via `cargo rustc --target <target> --release --bin lcpc-bench -- --emit=asm,obj`.

#### Missing Tools Diagnostic:
In accordance with task constraints (do not sudo-install packages), cross-linking and user-mode execution could not run on this host machine due to absent system packages:
- **Missing Cross-Linkers:** `aarch64-linux-gnu-gcc` and `riscv64-linux-gnu-gcc` are not installed on the host. When rustc invokes the host default linker (`cc`), linking fails at final ELF executable resolution with `rust-lld: incompatible target architecture`.
- **Missing Emulators:** `qemu-aarch64` and `qemu-riscv64` (along with `/usr/aarch64-linux-gnu` and `/usr/riscv64-linux-gnu` sysroots) are not installed on the host.
- **Cross-Inspection Methodology:** Because `rustc` and LLVM compile the machine code into native ELF object files (`.rcgu.o`) before invoking the system linker, code generation for all hot kernels is 100% complete and fully verifiable by `llvm-objdump`.

---

## 3. Disassembly Analysis: What LLVM Emitted Across Targets

Disassembly of all 16 hot kernels was verified across all three architectures using `scripts/check_codegen.py`.

### 3.1 Cross-Architecture Summary Table

| Metric | x86-64 Native (BMI2) | x86-64 Generic (Portability) | AArch64 (ARMv8-A) | RISC-V 64 (RV64GC) |
| :--- | :---: | :---: | :---: | :---: |
| **Compiler Target** | `x86_64-unknown-linux-gnu` | `x86_64-unknown-linux-gnu` | `aarch64-unknown-linux-gnu` | `riscv64gc-unknown-linux-gnu` |
| **Target CPU Flag** | `-C target-cpu=native` | Default generic (`x86-64-v1`) | Default generic (`armv8-a`) | Default generic (`rv64gc`) |
| **Hot Kernels Checked** | 16 | 16 | 16 | 16 |
| **Vector Arithmetic Violations** | **0 (PASS)** | **0 (PASS)** | **0 (PASS)** | **0 (PASS)** |
| **Primary Widening Multiply** | `mulxq` (3-operand BMI2) | `mulq` (implicit `%rax`/`%rdx`) | `mul` + `umulh` (pair) | `mul` + `mulhu` (pair) |
| **Primary Carry Synthesis** | `adcq` (hardware flag) | `adcq` (hardware flag) | `adds` + `adcs` / `cinc` | `add` + `sltu` (emulated) |
| **Hot Kernel Multiply Insns** | 358 (`mulx` 255, `mul` 103) | 352 (`mul` 352) | 586 (`mul`/`umulh`) | 55 (`mul`/`mulhu`) |
| **Hot Kernel Carry Insns** | 403 (`adcq`) | 391 (`adcq`) | 393 (`adcs`/`cinc`) | 57 (`sltu`) |
| **Hot Kernel Vector Moves** | 94 (zeroing, 32-byte copies) | 152 (zeroing, 16-byte copies) | 26 (`movi` zeroing, `stp q`) | 0 |
| **Hot Kernel Stack Accesses** | 717 (`mov` to/from `(%rsp)`) | 767 (`mov` to/from `(%rsp)`) | 161 (`str`/`stp` to `[sp]`) | 224 (`sd` to `(sp)`) |
| **Total Hot Instructions** | 5,521 | 6,087 | 4,481 | 1,281 |

> **Correction (independent verification, 2026-10-21; see `docs/STATUS.md`).** Earlier versions of this table were produced by a `check_codegen.py` that classified every AT&T `movq`/`movd` and every AArch64 `ldr`/`str` as a vector move, even on general-purpose registers. That inflated the vector-move counts (previously 1,711 / 2,297 / 327) and hid stack accesses, so x86-64 reported 0 spills. The classifier now counts these instructions as vector moves only when a vector register is involved. It treats any other instruction on a vector register as vector arithmetic, except the register-zeroing idiom. The numbers above come from the fixed checker on the same code (the instruction totals are unchanged). The verdict still holds: no hot kernel contains vector arithmetic on any target. The stack-access row counts loads and stores anywhere in the function on x86-64, but only stores on AArch64 and RISC-V. In the x86-64 `B = 4` inner row loop of `encode_column_block`, the stack accesses are real spills and reloads (§5.2).

---

### 3.2 x86-64 with BMI2 (`-C target-cpu=native`)

On modern x86-64 processors (Intel Cypress Cove / AMD Zen 4), `-C target-cpu=native` enables BMI2 instruction selection.

#### 1. Widening Multiplications: `mulxq`
In portable Rust:
```rust
let prod = (a as u128) * (b as u128);
let lo = prod as u64;
let hi = (prod >> 64) as u64;
```
LLVM lowers this pattern directly to:
```assembly
movq    %rbx, %rdx                  # load multiplier into %rdx
mulxq   (%rsi), %r11, %r12          # %r11 = lo, %r12 = hi (NO FLAGS MODIFIED)
```
**Microarchitectural Advantage:** Unlike legacy `mulq`, `mulxq` writes both low and high 64-bit products to arbitrary destination registers and **does not modify `EFLAGS`**. This allows LLVM to interleave widening limb multiplications directly inside active `adcq` carry chains without clobbering the carry flag.

#### 2. Carry Chains: `adcq`
Multi-limb additions unroll into continuous carry flag chains:
```assembly
addq    %r11, %r8
adcq    %r12, %r9
adcq    $0, %r10
```
The out-of-line field MAC kernels (`mac_column_*`) touch the stack only in their prologue (two argument stores); their row loops have no stack accesses.

---

### 3.3 x86-64 Generic Portability (`x86-64-v1`)

Without `-C target-cpu=native`, BMI2 is unavailable.

#### 1. Widening Multiplications: `mulq`
LLVM lowers 64×64→128 multiplies to standard x86 `mulq`:
```assembly
movq    %r14, %rax                  # multiplier must reside in %rax
mulq    %rbx                        # [%rdx:%rax] = %rax * %rbx (FLAGS CLOBBERED)
movq    %rax, %r11
movq    %rdx, %r12
```
**Overhead:** `mulq` clobbers `%rax`, `%rdx`, and `EFLAGS`. The compiler must emit additional register shuffle moves (`movq`) and cannot interleave multiplies with carry additions without breaking the carry flag.

---

### 3.4 AArch64 (`aarch64-unknown-linux-gnu`)

#### 1. Widening Multiplications: `mul` + `umulh`
AArch64 provides separate instructions for the low and high 64 bits of a widening product:
```assembly
mul     x16, x13, x8                # x16 = lo(x13 * x8)
umulh   x15, x13, x8                # x15 = hi(x13 * x8)
```
Each 64×64→128 multiply requires two scalar instructions. Modern AArch64 cores (Neoverse V1/V2, Cortex-X series) dual-issue `mul` and `umulh` across independent multiply pipelines.

#### 2. Carry Chains: `adds`, `adcs`, `cinc`
Carry propagation is handled via condition flags and conditional increments:
```assembly
adds    x14, x16, x14               # add with carry-out to C flag
cinc    x15, x15, hs                # increment high limb if carry set (C=1)
adcs    x16, x16, xzr               # add with carry and zero register
```

#### 3. Paired Memory Operations: `ldp` and `stp`
LLVM utilizes AArch64's paired load/store instructions (`ldp`/`stp`) to load and store two 64-bit limbs (128 bits total) in a single instruction:
```assembly
ldp     x12, x13, [x10, #-8]        # loads 128-bit field element in 1 insn
stp     x1, x4, [sp]                # stores 128 bits in 1 insn
```

---

### 3.5 RISC-V 64 (`riscv64gc-unknown-linux-gnu`)

#### 1. Widening Multiplications: `mul` + `mulhu`
RISC-V 64 separates low and high products:
```assembly
mul     a4, a4, a2                  # a4 = lo(a4 * a2)
mulhu   a5, a3, a2                  # a5 = hi(a3 * a2)
```
Modern out-of-order RISC-V decoders (such as SiFive P550/P670) recognize adjacent `mul` + `mulhu` pairs sharing identical source operands and fuse them into a single execution macro-op.

#### 2. Carry Flag Emulation: `add` + `sltu`
RV64GC features no condition code registers and no hardware carry flag. Every carry generation must be computed explicitly via set-less-than-unsigned (`sltu`):
```assembly
add     t0, a0, a1                  # t0 = a0 + a1
sltu    t1, t0, a0                  # t1 = 1 if carry occurred, else 0
add     t2, t2, t1                  # propagate carry to high accumulator limb
```
This fundamental ISA design requirement is why RISC-V exhibits 53 `sltu` instructions in place of hardware `adc`.

---

## 4. Vector Instructions Analysis: Arithmetic vs Moves

A central tenet of the project is ensuring our mathematical kernels remain **purely scalar**, while distinguishing vector data movement introduced by compiler memory routines or third-party crates.

### 4.1 Zero Vector Arithmetic in Hot Kernels

`scripts/check_codegen.py` scanned all 16 hot kernel symbols across all compiled targets:
- **Vector Arithmetic Instructions Detected:** **0**
- **Violations:** **0**
- **Verdict:** **PASS** on `x86_64` (native and portability), `aarch64`, and `riscv64gc`.

Every mathematical operation in `LazyField::mac`, `LazyField::reduce`, `encode_column_block`, and `reed_solomon_column_block` executes on 64-bit general-purpose registers:
- x86-64: `%rax`, `%rcx`, `%rdx`, `%rbx`, `%rsi`, `%rdi`, `%r8`–`%r15`;
- AArch64: `x0`–`x30`;
- RISC-V: `x0`–`x31` (`a0`–`a7`, `t0`–`t6`, `s0`–`s11`).

### 4.2 Vector Moves (Buffer Zeroing and Bulk Movement)

The disassembly listings record vector move instructions (`vmovaps`, `vmovups`, `vmovdqu`, `movi v0.2d`) inside `encode_column_block`:
- **Source:** Instantiation of the accumulator buffer:
  ```rust
  let mut scratch = vec![F::Acc::default(); n_rows];
  ```
  and slice resets (`acc.fill(F::Acc::default())`).
- **Codegen:** LLVM synthesizes AVX/NEON vector moves (`vmovaps %ymm0, (%rcx)` on x86, `movi v0.2d, #0` on AArch64) to zero 256 bits or 128 bits per instruction when initializing scratch buffers.
- **Also:** `vmovups` pairs copy 32-byte accumulators (`Mont127Acc`, `Mers127Acc`) and `vxorps %xmm0, %xmm0, %xmm0` creates the zero register for the stores. The checker classifies this zeroing idiom as a move.
- **AArch64:** the same zeroing and copies appear as `movi v0.2d, #0` followed by `stp q0, q0, [x8]` (and `str`/`ldr`/`ldp q` copies of accumulators).
- **Classification:** These instructions perform bulk memory initialization (`memset`) and copies, not mathematical computation. They do not alter arithmetic results and are classified separately as `VEC_MOVE` (`vector_moves` in the JSON report).

**Classification rules (`check_codegen.py`, classifier `rbd-codegen-classifier-v2`).** The report gives two counts per symbol and for the hot kernels: `vector_arithmetic` and `vector_moves`. Only vector arithmetic in a hot kernel makes the status `FAIL`; a run that matches no hot kernel is `NO_HOT_KERNELS`, not `PASS`. Every report records `classifier_version`; reports without it (all reports of the 2026-10-22/23 campaign) come from the earlier classifier, which misread AArch64 branch targets (§4.5).

| Class | x86-64 | AArch64 | Effect |
| :--- | :--- | :--- | :--- |
| Scalar | general-purpose-register instructions (`mulx`, `adc`, …), `jmp`/`jcc`/`call` | general-purpose-register instructions; branches and PC-relative forms (`b`, `b.cond`, `bl`, `br`, `blr`, `ret`, `cbz`, `cbnz`, `tbz`, `tbnz`, `adr`, `adrp`), whose target addresses and `<symbol>` annotations are never parsed as registers | none |
| Vector move | loads, stores and register copies of `xmm`/`ymm`/`zmm` (`vmovups`, `vmovaps`, `vmovdqu`, …) and the zero idiom with identical operands (`vxorps %xmm0,%xmm0,%xmm0`) | `movi`, `mvni`, `dup`, `mov`, `ins`, `umov`, `smov`, `fmov`, `ld1`/`st1`, and `ldr`/`str`/`ldp`/`stp`/`ldur`/`stur`/`ldnp`/`stnp` with a SIMD&FP register (`q`/`d`/`s` views) | counted as `vector_moves`, does not fail |
| Vector arithmetic | `vpadd*`, `vpmul*`, `vpmadd52*`, shifts, logic with distinct operands, shuffles/inserts, and any unlisted SIMD instruction | `add`, `sub`, `mul`, `mla`, `umull`, `pmull`, `shl`, `ushr`, `and`, `orr`, `eor`, … on `vN.<arrangement>`, scalar SIMD&FP arithmetic, permutes, conversions, and anything unrecognized on SIMD&FP registers | `FAIL` in a hot kernel |

### 4.3 Precompiled Standard Library & Runtime `memcpy`

On Linux platforms (`*-unknown-linux-gnu`), memory operations such as `memcpy`, `memmove`, and `memset` resolve to the host C library (`glibc` 2.39). Glibc provides architecture-specific IFUNC resolvers that dynamically select AVX2 or AVX-512 implementations at program launch based on CPUID. These routines exist outside our compiled crates and do not violate the scalar-kernel invariant.

### 4.4 BLAKE3 Portable-Only Configuration

In `Cargo.toml`, `blake3` is configured with:
```toml
features = ["pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]
```
- **Investigation:** Inspection of the binary shows vector-register arithmetic outside the hot kernels. Most of it is in `blake3::sse2::compress_in_place` and `blake3::sse41::compress_in_place` (316 and 307 instructions). Those modules are compiled but never selected at run time. The rest is scalar SSE floating point (`addsd`, `cvtsi2sd`, and so on) in the benchmark harness and in `matgen::get_dims`, which the conservative classifier also counts.
- **Run-time Verification:** When built with `no_sse2`, `no_sse41`, `no_avx2`, and `no_avx512`, BLAKE3's dynamic platform detection (`blake3::platform::Platform::detect()`) is hardwired to always select the portable scalar implementation. The compiled SIMD code blocks are dead code paths and are never executed at run time.

### 4.5 Wording of the scalar claim (campaign audit, 2026-10-23)

The evidence supports **"no vector arithmetic in the hot kernels; the compiler emits vector registers only for zeroing and copy stores"**. It does not support "the binary contains no SIMD" or "the hot kernels contain no vector instructions". Known gaps, to state with T11 and in the paper:

- **Vector stores in hot kernels, on both architectures.** With `target-cpu=native`, the GCP x86-64 codegen reports show `vector_arithmetic = 0` but about 99 vector moves in the hot kernels, e.g. `vmovups %ymm0,(%rcx)` and `vmovups %zmm0,0x40(%rcx)` in `encode_column_block::<Mont127>` (zeroing and 32-byte accumulator copies, §4.2). The Arm hot kernels on the `c4a` VM contain 26 vector moves, e.g. `movi v0.2d, #0` and `stp q0, q0, [x8]`. So the claim is "zeroing and copy stores only" on x86-64 **and** Arm.
- **BLAKE3 SIMD code is linked.** The `pure` feature still compiles BLAKE3's Rust SSE2/SSE4.1 modules on x86-64; they are dead code because `no_sse2`/`no_sse41` make `Platform::detect()` return the portable backend (§4.4). BLAKE3 documents those features as a testing-only short-circuit; the run-time platform is not asserted by the driver.
- **Layout conversion is not a hot pattern.** `transpose_into` (row-major to column-major) runs inside This Work's timed `encode_matrix` but is not in `check_codegen.py`'s hot-pattern list, so its instruction mix is not checked. SIMD-Spielman's timed region contains no layout conversion at all (its benchmark generates input in the reordered layout), which slightly favours SIMD-Spielman.
- **x86-64 and Arm use different target flags.** `.cargo/config.toml` sets `target-cpu=native` only for `x86_64-unknown-linux-gnu`; the Arm build is generic `aarch64` with only the no-vectorize flags.
- **Arm checker false positive.** On the `c4a` VM, the campaign's `check_codegen.py --strict` failed This Work's Arm blocks with 56 "vector arithmetic" instructions in hot kernels. All 56 are scalar branches (`b.eq`, `b.ne`, `cbz`, `b.cs`, `b.cc`, `b.hi`, `b.ls`, `cbnz`) whose GNU `objdump` branch-target addresses (e.g. `b3334`) the AArch64 classifier misread as `b` registers. The cross-compiled object listings in §3.1 (from `llvm-objdump`) were unaffected. The classifier now follows the rules in §4.2 (branches scalar, zeroing/copy moves reported as `vector_moves`, vector arithmetic fails). The Arm This Work blocks still have to be re-run with it (`docs/STATUS.md` §7.6); until then there is no Arm This Work timing, and the campaign's Arm codegen reports (status `FAIL`) are not evidence of vector arithmetic.

---

## 5. Microarchitectural Quality & Notable Inefficiencies

### 5.1 Field Kernel Limb Analysis

#### 1. `Mont127` and `Mers127` (127-Bit Multiplications)
- Full 127×127-bit multiplication requires 4 limb products ($w_0 x_0, w_0 x_1, w_1 x_0, w_1 x_1$).
- **x86-64 Native:** Exactly 4 `mulxq` instructions and 8 `adcq` instructions. Zero stack spills.
- **x86-64 Portability:** Exactly 4 `mulq` instructions and 8 `adcq` instructions. Zero stack spills.
- **AArch64:** Exactly 4 pairs of (`mul` + `umulh`) = 8 multiply instructions and 8 carry additions.
- **RISC-V 64:** Exactly 4 pairs of (`mul` + `mulhu`) = 8 multiply instructions and 11 `sltu`/`add` carry operations.

#### 2. `Fp2M61` (Degree-2 Extension over Mersenne-61)
- Degree-2 multiplication $(a_0 + a_1 i)(b_0 + b_1 i) = (a_0 b_0 - a_1 b_1) + (a_0 b_1 + a_1 b_0)i$.
- Evaluated via schoolbook / Karatsuba over 61-bit limbs.
- **x86-64 Native:** 12 `mulxq` instructions across unrolled row elements, 12 `adcq` instructions, 0 stack spills.
- **AArch64:** 8 `mul`/`umulh` instructions, 4 `adc` instructions, 2 stack spills (prologue/epilogue).
- **RISC-V 64:** 8 `mul`/`mulhu` instructions, 4 `sltu` carry instructions.

### 5.2 Notable Inefficiencies & Microarchitectural Costs

1. **RISC-V Carry Emulation Penalty:**
   - Because RV64GC lacks a hardware carry flag, each carry propagation costs 2 instructions (`add` + `sltu`) and introduces a data-dependency latency of 2 cycles compared to 1 cycle for `adcq` (x86) and `adcs` (AArch64).
   - *Impact:* RISC-V requires approximately 20–30% more instructions for multi-limb arithmetic. Redundant-headroom accumulators (Plan 02 §4.2) that postpone carry propagation until reduction offer the highest relative speedup on RISC-V.

2. **Register Pressure under Multi-Edge Blocking ($B = 4$):**
   - x86-64 has only 16 GPRs. With $B = 4$, keeping a 4-word accumulator (`scratch[row]`), 4 input pointers, row counter, and edge weights resident in registers stresses the register allocator. **Corrected:** LLVM does *not* keep everything in registers. In the `B = 4` inner row loop of `encode_column_block::<Mont127>` (113 instructions, 16 `mulx`), 22 instructions access `(%rsp)`. They reload prepared weights and source pointers and spill and reload a partial accumulator; `Mers127` has 20 and `Fp2M61` 34. Choosing `B = 4` (D8) therefore rests on the timings in `docs/BLOCKING.md`, not on spill-free code. Whether `B = 2` would avoid the spills on x86-64 has not been measured.
   - RISC-V has 32 GPRs, but LLVM emitted 245 stack accesses across the full outer function frame due to conservative frame-pointer and callee-saved register preservation (`s0`–`s11`).

3. **Single vs Dual Instruction Widening Multiplies:**
   - x86-64 `mulxq` produces both 64-bit low and high results from a single instruction.
   - AArch64 and RISC-V require two separate instructions (`mul` and `umulh`/`mulhu`) per 64×64→128 multiply, doubling decode bandwidth requirements in the multiplication frontend.

---

## 6. How to Reproduce

All reports and listings can be re-generated from the repository root:

```bash
cd code

# 1. Run check_codegen self-test
python3 scripts/check_codegen.py --self-test

# 2. Portability build and test
RUSTFLAGS="-C no-vectorize-loops -C no-vectorize-slp" cargo test --workspace --release

# 3. Generate x86-64 native listings & JSON report
cargo build -p lcpc-bench --bin lcpc-bench --release
python3 scripts/check_codegen.py target/release/lcpc-bench \
    --output-listing docs/codegen/x86_64_native.listing.txt \
    --output-json docs/codegen/x86_64_native.report.json

# 4. Generate cross-architecture listings (AArch64 & RISC-V)
cargo rustc -p lcpc-bench --target aarch64-unknown-linux-gnu --release --bin lcpc-bench -- --emit=asm,obj
python3 scripts/check_codegen.py target/aarch64-unknown-linux-gnu/release/deps/lcpc_bench-*.rcgu.o \
    --output-listing docs/codegen/aarch64.listing.txt \
    --output-json docs/codegen/aarch64.report.json

cargo rustc -p lcpc-bench --target riscv64gc-unknown-linux-gnu --release --bin lcpc-bench -- --emit=asm,obj
python3 scripts/check_codegen.py target/riscv64gc-unknown-linux-gnu/release/deps/lcpc_bench-*.rcgu.o \
    --output-listing docs/codegen/riscv64.listing.txt \
    --output-json docs/codegen/riscv64.report.json
```
