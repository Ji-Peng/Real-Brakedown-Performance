#!/usr/bin/env python3
# Copyright 2026 RealBrakedownPerf Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""
check_codegen.py - Disassembles Rust binaries, verifies hot kernels contain NO
vector-register arithmetic, reports vector moves separately, and outputs listings
and JSON reports.

Per Plan 03 (CPU Microarchitecture & Codegen):
- Target architectures: x86_64, aarch64, riscv64
- Pass/Fail Gate: Rejects any SIMD arithmetic instructions in hot kernels
- Memory Movement Filter: Flags non-arithmetic vector moves separately
- Quality Signals: Tallies multiplication variants (mulx vs mul), carry chains (adc/adcx/adox),
  stack spills, and memory traffic
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Dict, List, Optional, Set, Tuple

# Default hot kernel patterns per Plan 03 §1, §3.4, §6.1
DEFAULT_HOT_PATTERNS = [
    r"encode_column_block",
    r"reed_solomon_column_block",
    r"reduce_column",
    r"LazyField::mac",
    r"LazyField::reduce",
    r"mac_column",
    r"fold_column",
]

# Regex patterns for parsing disassembly
# Greedy: demangled generic symbols contain nested `<...>`, e.g.
# `encode_column_block::<lcpc_fields::mont127::Mont127>`.
SYM_HEADER_RE = re.compile(r"^[0-9a-fA-F]+\s+<(.+)>:\s*$")
# Standard GNU objdump and llvm-objdump place a TAB between raw instruction bytes and the mnemonic
INSN_LINE_TAB_RE = re.compile(r"^\s*([0-9a-fA-F]+):\s*(?:[0-9a-fA-F\s]+)?\t\s*([a-zA-Z0-9_\.]+)(?:\s+(.*))?$")
INSN_LINE_FALLBACK_RE = re.compile(
    r"^\s*([0-9a-fA-F]+):\s+(?:((?:[0-9a-fA-F]{2,8}\s+)+)\s+)?([a-zA-Z0-9_\.]+)(?:\s+(.*))?$"
)


def parse_instruction_line(line: str) -> Optional[Tuple[str, str, str]]:
    """Extracts (address, mnemonic, operands) from a disassembly line."""
    m_tab = INSN_LINE_TAB_RE.match(line)
    if m_tab:
        return (m_tab.group(1), m_tab.group(2), m_tab.group(3) or "")
    m_fallback = INSN_LINE_FALLBACK_RE.match(line)
    if m_fallback:
        return (m_fallback.group(1), m_fallback.group(3), m_fallback.group(4) or "")
    return None

# Architecture instruction sets
# -------------------------------------------------------------
# x86-64 classification
# -------------------------------------------------------------
X86_VEC_REG_RE = re.compile(r"\b([xyz]mm[0-9]+)\b", re.IGNORECASE)

# Vector memory movement opcodes (NOT vector arithmetic)
X86_VEC_MOVES = {
    "vmovdqu", "vmovdqu8", "vmovdqu16", "vmovdqu32", "vmovdqu64", "movdqu",
    "vmovdqa", "vmovdqa32", "vmovdqa64", "movdqa",
    "vmovaps", "vmovups", "movaps", "movups",
    "vmovapd", "vmovupd", "movapd", "movupd",
    "vmovntdq", "movntdq", "vmovntps", "movntps", "vmovntpd", "movntpd",
    "vmovd", "movd", "vmovq", "movq",
    "vbroadcastss", "vbroadcastsd", "vbroadcastf32x4", "vbroadcastf64x2",
    "vbroadcastf32x8", "vbroadcastf64x4", "vbroadcasti32x4", "vbroadcasti64x2",
    "vbroadcasti32x8", "vbroadcasti64x4", "vpbroadcastb", "vpbroadcastw",
    "vpbroadcastd", "vpbroadcastq",
    "vmovshdup", "vmovsldup",
}

# XOR forms that zero a vector register when all operands are the same register
X86_ZERO_IDIOMS = {"xorps", "vxorps", "xorpd", "vxorpd", "pxor", "vpxor", "vpxord", "vpxorq"}

# Vector arithmetic opcode prefixes / patterns
X86_VEC_ARITH_PREFIXES = (
    # Addition / Subtraction
    "vpadd", "padd", "vpsub", "psub", "addps", "addpd", "addss", "addsd",
    "subps", "subpd", "subss", "subsd",
    # Multiplication / MAC
    "vpmul", "pmul", "vpmadd", "pmadd", "vmul", "mulps", "mulpd", "mulss", "mulsd",
    "vfmadd", "vfmsub", "vfnmadd", "vfnmsub",
    # Division / Sqrt
    "vdiv", "divps", "divpd", "divss", "divsd", "vsqrt", "sqrt",
    # Bitwise / Logic
    "vpxor", "pxor", "vpor", "por", "vpand", "pand", "vpandn", "pandn", "vternlog",
    # Shifts
    "vpsll", "psll", "vpsrl", "psrl", "vpsra", "psra", "vprol", "vpror",
    # Comparison
    "vpcmp", "pcmp", "vpcmpeq", "pcmpeq", "vpmin", "pmin", "vpmax", "pmax",
    "ucomiss", "ucomisd", "comiss", "comisd", "minps", "minpd", "maxps", "maxpd",
    # Permute / Shuffle / Blend
    "vpermi", "vpermt", "vpermil", "vperm", "vpshuf", "pshuf",
    "vpblend", "pblend", "vblend", "blend", "vpunpck", "punpck", "vpack", "pack",
    # Crypto / Specials
    "vaes", "aes", "vsha", "sha", "vgfni", "gf2p8",
)

# -------------------------------------------------------------
# AArch64 classification
# -------------------------------------------------------------
# SIMD&FP registers as they appear in operands: vector arrangements (v0.2d),
# scalar views of the same register file used by vector loads and stores
# (q0/d0/s0/h0/b0), and SVE z/p registers.
AARCH64_SIMD_FP_REG_RE = re.compile(
    r"\b(v[0-9]+(?:\.[0-9]*[bshdq])?|[qdshb][0-9]+|z[0-9]+|p[0-9]+)\b", re.IGNORECASE
)

# Data movement on SIMD&FP registers (loads, stores, register moves, immediates).
# `ldr`/`str`/`ldp`/`stp`/`ldur`/`stur` count only when a SIMD&FP register is
# involved; on general-purpose registers they are ordinary scalar accesses.
AARCH64_VEC_MOVES = {
    "ld1", "st1", "ld2", "st2", "ld3", "st3", "ld4", "st4",
    "ld1r", "ld2r", "ld3r", "ld4r", "ldr", "str", "ldp", "stp", "ldur", "stur",
    "mov", "movi", "mvni", "dup", "ins", "umov", "fmov",
}


# -------------------------------------------------------------
# RISC-V 64 classification
# -------------------------------------------------------------
RISCV_VEC_REG_RE = re.compile(r"\b(v[0-9]+)\b", re.IGNORECASE)

RISCV_VEC_MOVES = {
    "vle8.v", "vle16.v", "vle32.v", "vle64.v",
    "vse8.v", "vse16.v", "vse32.v", "vse64.v",
    "vlse8.v", "vlse16.v", "vlse32.v", "vlse64.v",
    "vsse8.v", "vsse16.v", "vsse32.v", "vsse64.v",
    "vmv.v.v", "vmv.v.x", "vmv.v.i", "vmv.x.s", "vmv.s.x",
}



def demangle(symbol: str) -> str:
    """Attempts demangling using c++filt or rustfilt if available."""
    if not (symbol.startswith("_R") or symbol.startswith("_Z")):
        return symbol
    for tool in ["rustfilt", "c++filt"]:
        if shutil.which(tool):
            try:
                res = subprocess.run([tool, symbol], stdout=subprocess.PIPE, text=True, check=True)
                demangled = res.stdout.strip()
                if demangled:
                    return demangled
            except Exception:
                pass
    return symbol


def detect_arch(file_path: str, user_arch: Optional[str] = None) -> str:
    """Detect target architecture from user option, ELF header, or default host."""
    if user_arch and user_arch != "auto":
        return user_arch.lower()

    if shutil.which("file"):
        try:
            out = subprocess.check_output(["file", file_path], text=True).lower()
            if "x86-64" in out or "x86_64" in out:
                return "x86_64"
            if "aarch64" in out or "arm64" in out:
                return "aarch64"
            if "risc-v" in out or "riscv" in out:
                return "riscv64"
        except Exception:
            pass

    # Default to host architecture
    mach = os.uname().machine.lower()
    if mach in ["x86_64", "amd64"]:
        return "x86_64"
    if mach in ["aarch64", "arm64"]:
        return "aarch64"
    if mach in ["riscv64"]:
        return "riscv64"
    return "x86_64"


def classify_instruction(arch: str, mnemonic: str, operands: str) -> Tuple[str, Optional[str]]:
    """
    Classifies instruction into:
      ('VEC_ARITH', detail), ('VEC_MOVE', detail), ('SCALAR', detail), or ('OTHER', detail)
    """
    mnem = mnemonic.lower()
    ops = operands if operands else ""

    if arch == "x86_64":
        has_vec_reg = bool(X86_VEC_REG_RE.search(ops))
        is_vec_mnem = mnem.startswith("v") or mnem.startswith("p") or (mnem.endswith(("ps", "pd", "ss", "sd")) and len(mnem) in (4, 5, 6, 7))

        # AT&T `movq`/`movd` are also the ordinary 64/32-bit general-purpose
        # moves; they are vector moves only when a vector register is involved.
        if mnem in X86_VEC_MOVES and (has_vec_reg or mnem not in ("movq", "movd")):
            return ("VEC_MOVE", f"{mnem} {ops}".strip())

        # Register-zeroing idiom (`vxorps %xmm0, %xmm0, %xmm0`): it only
        # materializes zeros for subsequent vector stores (memset-style moves).
        if has_vec_reg and mnem in X86_ZERO_IDIOMS:
            regs = [r.strip().lower() for r in ops.split(",")]
            if len(regs) >= 2 and len(set(regs)) == 1 and X86_VEC_REG_RE.fullmatch(regs[0].lstrip("%")):
                return ("VEC_MOVE", f"{mnem} {ops} (zero idiom)".strip())

        if has_vec_reg or is_vec_mnem:
            for pfx in X86_VEC_ARITH_PREFIXES:
                if mnem.startswith(pfx):
                    return ("VEC_ARITH", f"{mnem} {ops}".strip())

        # Conservative: any other instruction on a vector register is not
        # known to be data movement, so it counts as vector arithmetic.
        if has_vec_reg:
            return ("VEC_ARITH", f"{mnem} {ops} (unclassified vector-register instruction)".strip())

        # Scalar classification & quality signals
        if mnem in ("mulx", "mulxq"):
            return ("SCALAR_MULX", f"{mnem} {ops}".strip())
        if mnem in ("mul", "imul", "mulq", "imulq"):
            return ("SCALAR_MUL", f"{mnem} {ops}".strip())
        if mnem in ("adcx", "adox", "adcxq", "adoxq"):
            return ("SCALAR_ADCX", f"{mnem} {ops}".strip())
        if mnem in ("adc", "adcq"):
            return ("SCALAR_ADC", f"{mnem} {ops}".strip())
        if mnem in ("add", "addq"):
            return ("SCALAR_ADD", f"{mnem} {ops}".strip())

        # Stack spill detection
        if ("(%rsp)" in ops or "(%rbp)" in ops) and mnem.startswith("mov"):
            return ("STACK_SPILL", f"{mnem} {ops}".strip())

        return ("SCALAR", f"{mnem} {ops}".strip())

    elif arch == "aarch64":
        has_vec_reg = bool(AARCH64_SIMD_FP_REG_RE.search(ops))
        if has_vec_reg:
            if mnem in AARCH64_VEC_MOVES:
                return ("VEC_MOVE", f"{mnem} {ops}".strip())
            # Conservative: arithmetic, permutes, conversions, and anything
            # unrecognized on SIMD&FP registers count as vector arithmetic.
            return ("VEC_ARITH", f"{mnem} {ops}".strip())

        # Scalar AArch64
        if mnem in ("umulh", "smulh"):
            return ("SCALAR_UMULH", f"{mnem} {ops}".strip())
        if mnem in ("mul",):
            return ("SCALAR_MUL", f"{mnem} {ops}".strip())
        if mnem in ("adcs", "adc", "cinc"):
            return ("SCALAR_ADC", f"{mnem} {ops}".strip())
        if mnem in ("adds", "add"):
            return ("SCALAR_ADD", f"{mnem} {ops}".strip())
        if ("[sp" in ops) and (mnem.startswith("str") or mnem.startswith("stp")):
            return ("STACK_SPILL", f"{mnem} {ops}".strip())
        return ("SCALAR", f"{mnem} {ops}".strip())

    elif arch == "riscv64":
        has_vec_reg = bool(RISCV_VEC_REG_RE.search(ops))
        if mnem in RISCV_VEC_MOVES:
            return ("VEC_MOVE", f"{mnem} {ops}".strip())

        if mnem.startswith("v") or has_vec_reg:
            # Conservative: every RVV instruction other than the listed moves
            # (including vsetvli and unrecognized ones) counts as vector arithmetic.
            return ("VEC_ARITH", f"{mnem} {ops}".strip())

        # Scalar RISC-V
        if mnem in ("mulhu", "mulh", "mulhsu"):
            return ("SCALAR_MULH", f"{mnem} {ops}".strip())
        if mnem == "mul":
            return ("SCALAR_MUL", f"{mnem} {ops}".strip())
        if mnem in ("sltu", "slt"):
            return ("SCALAR_SLTU", f"{mnem} {ops}".strip())
        if mnem in ("add", "addi"):
            return ("SCALAR_ADD", f"{mnem} {ops}".strip())
        if ("(sp)" in ops) and (mnem.startswith("sd") or mnem.startswith("sw") or mnem.startswith("sh") or mnem.startswith("sb") or mnem == "s"):
            return ("STACK_SPILL", f"{mnem} {ops}".strip())
        return ("SCALAR", f"{mnem} {ops}".strip())

    return ("SCALAR", f"{mnem} {ops}".strip())


def find_llvm_objdump() -> Optional[str]:
    """Finds llvm-objdump in PATH or in the active rustup toolchain sysroot."""
    tool = shutil.which("llvm-objdump")
    if tool:
        return tool
    rustc = shutil.which("rustc") or os.path.expanduser("~/.cargo/bin/rustc")
    if os.path.isfile(rustc) and os.access(rustc, os.X_OK):
        try:
            sysroot = subprocess.check_output([rustc, "--print", "sysroot"], text=True).strip()
            rustlib = os.path.join(sysroot, "lib", "rustlib")
            if os.path.isdir(rustlib):
                for entry in sorted(os.listdir(rustlib)):
                    candidate = os.path.join(rustlib, entry, "bin", "llvm-objdump")
                    if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
                        return candidate
        except Exception:
            pass
    return None


def disassemble_file(file_path: str, objdump_cmd: Optional[str] = None) -> str:
    """Disassembles file using llvm-objdump or GNU objdump with demangling."""
    tools = []
    if objdump_cmd:
        tools.append(objdump_cmd)
    llvm_tool = find_llvm_objdump()
    if llvm_tool and llvm_tool not in tools:
        tools.append(llvm_tool)
    if shutil.which("objdump") and "objdump" not in tools:
        tools.append("objdump")

    for tool in tools:
        cmd = [tool, "-d", "-C", file_path]
        try:
            res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
            return res.stdout
        except Exception:
            # Fallback without -C if -C is unsupported
            try:
                cmd_no_c = [tool, "-d", file_path]
                res = subprocess.run(cmd_no_c, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
                return res.stdout
            except Exception:
                continue

    raise RuntimeError(f"Failed to disassemble {file_path}. Neither llvm-objdump nor objdump succeeded.")


def parse_and_check(
    disasm_text: str,
    arch: str,
    hot_patterns: List[str],
    all_symbols: bool = False
) -> Dict:
    """Parses disassembly output and checks each symbol for vector arithmetic."""
    compiled_patterns = [re.compile(p) for p in hot_patterns]

    current_sym_raw = None
    current_sym_demangled = None
    current_is_hot = False
    current_lines = []

    symbols_report = []
    hot_kernels_checked = 0
    hot_kernel_violations = 0

    hot_stats = {
        "vector_arithmetic_count": 0,
        "vector_moves_count": 0,
        "scalar_mulx_count": 0,
        "scalar_mul_count": 0,
        "scalar_adc_count": 0,
        "stack_spill_count": 0,
        "total_instructions": 0,
    }

    outside_stats = {
        "vector_arithmetic_count": 0,
        "vector_moves_count": 0,
        "total_instructions": 0,
    }

    def finalize_symbol():
        nonlocal hot_kernels_checked, hot_kernel_violations
        if not current_sym_raw or not current_lines:
            return

        sym_vec_arith = []
        sym_vec_moves = []
        sym_scalar_stats = {
            "mulx": 0,
            "mul": 0,
            "adc": 0,
            "add": 0,
            "spills": 0,
            "total": len(current_lines),
        }

        for addr, mnem, ops in current_lines:
            kind, detail = classify_instruction(arch, mnem, ops)
            if kind == "VEC_ARITH":
                sym_vec_arith.append({"addr": addr, "insn": detail})
                if current_is_hot:
                    hot_stats["vector_arithmetic_count"] += 1
                else:
                    outside_stats["vector_arithmetic_count"] += 1
            elif kind == "VEC_MOVE":
                sym_vec_moves.append({"addr": addr, "insn": detail})
                if current_is_hot:
                    hot_stats["vector_moves_count"] += 1
                else:
                    outside_stats["vector_moves_count"] += 1
            elif kind == "SCALAR_MULX":
                sym_scalar_stats["mulx"] += 1
                if current_is_hot:
                    hot_stats["scalar_mulx_count"] += 1
            elif kind in ("SCALAR_MUL", "SCALAR_UMULH", "SCALAR_MULH"):
                sym_scalar_stats["mul"] += 1
                if current_is_hot:
                    hot_stats["scalar_mul_count"] += 1
            elif kind in ("SCALAR_ADCX", "SCALAR_ADC", "SCALAR_SLTU"):
                sym_scalar_stats["adc"] += 1
                if current_is_hot:
                    hot_stats["scalar_adc_count"] += 1
            elif kind == "SCALAR_ADD":
                sym_scalar_stats["add"] += 1
            elif kind == "STACK_SPILL":
                sym_scalar_stats["spills"] += 1
                if current_is_hot:
                    hot_stats["stack_spill_count"] += 1

            if current_is_hot:
                hot_stats["total_instructions"] += 1
            else:
                outside_stats["total_instructions"] += 1

        is_violation = (len(sym_vec_arith) > 0)
        status = "FAIL" if (current_is_hot and is_violation) else "PASS"

        if current_is_hot:
            hot_kernels_checked += 1
            if is_violation:
                hot_kernel_violations += 1

        if current_is_hot or all_symbols or is_violation:
            symbols_report.append({
                "symbol_raw": current_sym_raw,
                "symbol_demangled": current_sym_demangled,
                "is_hot_kernel": current_is_hot,
                "status": status,
                "vector_arithmetic": sym_vec_arith,
                "vector_moves": sym_vec_moves,
                "scalar_stats": sym_scalar_stats,
                "disassembly_lines": [f"  {a}: {m} {o}".strip() for a, m, o in current_lines],
            })

    for line in disasm_text.splitlines():
        line = line.rstrip()
        m_sym = SYM_HEADER_RE.match(line)
        if m_sym:
            finalize_symbol()
            current_sym_raw = m_sym.group(1)
            current_sym_demangled = demangle(current_sym_raw)
            current_is_hot = any(p.search(current_sym_demangled) or p.search(current_sym_raw) for p in compiled_patterns)
            current_lines = []
            continue

        parsed = parse_instruction_line(line)
        if parsed:
            addr, mnem, ops = parsed
            current_lines.append((addr, mnem, ops))

    finalize_symbol()

    overall_status = "PASS" if (hot_kernel_violations == 0) else "FAIL"

    return {
        "status": overall_status,
        "arch": arch,
        "hot_patterns": hot_patterns,
        "hot_kernels_checked": hot_kernels_checked,
        "hot_kernel_violations": hot_kernel_violations,
        "summary": {
            "hot_kernels": hot_stats,
            "outside_hot_kernels": outside_stats,
        },
        "symbols": symbols_report,
    }


def generate_listing(report: Dict) -> str:
    """Formats annotated disassembly listing of hot kernels and flagged instructions."""
    lines = [
        "================================================================================",
        "RealBrakedownPerf - Code Generation & Scalar Verification Listing",
        f"Status: {report['status']} | Target Architecture: {report['arch']}",
        f"Hot Kernels Checked: {report['hot_kernels_checked']} | Violations: {report['hot_kernel_violations']}",
        "================================================================================",
        "",
        "SUMMARY METRICS (Hot Kernels):",
        f"  Vector Arithmetic (VIOLATIONS) : {report['summary']['hot_kernels']['vector_arithmetic_count']}",
        f"  Vector Moves (Data Movement)   : {report['summary']['hot_kernels']['vector_moves_count']}",
        f"  Scalar BMI2 MULX               : {report['summary']['hot_kernels']['scalar_mulx_count']}",
        f"  Scalar Standard MUL            : {report['summary']['hot_kernels']['scalar_mul_count']}",
        f"  Scalar Carry ADC / ADCX        : {report['summary']['hot_kernels']['scalar_adc_count']}",
        f"  Stack Pointer Spills           : {report['summary']['hot_kernels']['stack_spill_count']}",
        f"  Total Instructions             : {report['summary']['hot_kernels']['total_instructions']}",
        "",
        "SUMMARY METRICS (Outside Hot Kernels - e.g. stdlib / portable blake3):",
        f"  Vector Arithmetic              : {report['summary']['outside_hot_kernels']['vector_arithmetic_count']}",
        f"  Vector Moves                   : {report['summary']['outside_hot_kernels']['vector_moves_count']}",
        "",
        "================================================================================",
        "HOT KERNEL LISTINGS:",
        "================================================================================",
    ]

    for sym in report["symbols"]:
        if not sym["is_hot_kernel"] and sym["status"] == "PASS":
            continue

        tag = "[HOT KERNEL]" if sym["is_hot_kernel"] else "[NON-KERNEL SYMBOL]"
        verdict = f"[{sym['status']}]"
        lines.append(f"\n{verdict} {tag} <{sym['symbol_demangled']}>")
        lines.append(f"  Raw Symbol: {sym['symbol_raw']}")
        lines.append(
            f"  Stats: mulx={sym['scalar_stats']['mulx']} mul={sym['scalar_stats']['mul']} "
            f"adc={sym['scalar_stats']['adc']} spills={sym['scalar_stats']['spills']} "
            f"total_insns={sym['scalar_stats']['total']}"
        )

        if sym["vector_arithmetic"]:
            lines.append("  *** VIOLATIONS (Vector Arithmetic): ***")
            for va in sym["vector_arithmetic"]:
                lines.append(f"    --> 0x{va['addr']}: {va['insn']}")

        if sym["vector_moves"]:
            lines.append("  Vector Moves (Memory copies / helpers):")
            for vm in sym["vector_moves"]:
                lines.append(f"        0x{vm['addr']}: {vm['insn']}")

        lines.append("\n  Disassembly:")
        for dis in sym.get("disassembly_lines", []):
            lines.append(f"    {dis}")

    return "\n".join(lines) + "\n"


def run_self_test(objdump_cmd: Optional[str] = None) -> bool:
    """
    Self-test: Compiles a vectorizable snippet with and without loop vectorization flags,
    and proves that check_codegen.py flags vector arithmetic in the former and passes the latter.
    """
    print("[SELF-TEST] Running check_codegen.py verification suite...")

    # Instruction-level classification cases: (arch, mnemonic, operands, expected class).
    cases = [
        ("x86_64", "movq", "%rcx, 0xa0(%rsp)", "STACK_SPILL"),  # GPR store, not a vector move
        ("x86_64", "movq", "%r8, %r14", "SCALAR"),
        ("x86_64", "movq", "%xmm0, %rax", "VEC_MOVE"),
        ("x86_64", "vmovaps", "%ymm0, (%rcx)", "VEC_MOVE"),
        ("x86_64", "vxorps", "%xmm0, %xmm0, %xmm0", "VEC_MOVE"),  # zero idiom
        ("x86_64", "vxorps", "%xmm1, %xmm2, %xmm0", "VEC_ARITH"),
        ("x86_64", "vpaddq", "%ymm1, %ymm2, %ymm0", "VEC_ARITH"),
        ("x86_64", "vinserti128", "$0x1, %xmm1, %ymm0, %ymm0", "VEC_ARITH"),  # unlisted
        ("x86_64", "mulxq", "%r8, %rax, %rdx", "SCALAR_MULX"),
        ("aarch64", "ldr", "x8, [sp, #16]", "SCALAR"),
        ("aarch64", "str", "x8, [sp, #16]", "STACK_SPILL"),
        ("aarch64", "str", "q0, [x8]", "VEC_MOVE"),
        ("aarch64", "movi", "v0.2d, #0000000000000000", "VEC_MOVE"),
        ("aarch64", "ushr", "v0.2d, v1.2d, #3", "VEC_ARITH"),  # unlisted
        ("aarch64", "umulh", "x9, x10, x11", "SCALAR_UMULH"),
        ("riscv64", "vsetvli", "t0, a0, e64, m1, ta, ma", "VEC_ARITH"),
        ("riscv64", "mulhu", "a0, a1, a2", "SCALAR_MULH"),
    ]
    for arch, mnem, ops, expected in cases:
        got, _ = classify_instruction(arch, mnem, ops)
        if got != expected:
            print(f"[SELF-TEST FAILED] {arch} '{mnem} {ops}': expected {expected}, got {got}")
            return False
    print(f"[SELF-TEST] Instruction classification: {len(cases)} cases OK")

    rustc = shutil.which("rustc") or os.path.expanduser("~/.cargo/bin/rustc")
    if not (os.path.isfile(rustc) and os.access(rustc, os.X_OK)):
        print(f"[SELF-TEST ERROR] rustc not found at {rustc}")
        return False

    with tempfile.TemporaryDirectory() as td:
        src = os.path.join(td, "test_snippet.rs")
        with open(src, "w") as f:
            f.write("""
#[no_mangle]
#[inline(never)]
pub fn vectorizable_kernel(a: &[u32; 1024], b: &[u32; 1024], c: &mut [u32; 1024]) {
    for i in 0..1024 {
        c[i] = a[i].wrapping_add(b[i]);
    }
}
""")

        # 1. Compile Case A: Default optimization (auto-vectorization enabled)
        obj_vec = os.path.join(td, "vec.o")
        cmd_vec = [
            rustc, "--crate-type=staticlib", "-O", "-C", "target-cpu=native",
            "--emit=obj", src, "-o", obj_vec
        ]
        try:
            subprocess.check_call(cmd_vec, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception as e:
            print(f"[SELF-TEST ERROR] Failed to compile vector test snippet: {e}")
            return False

        # 2. Compile Case B: Disabling loop and SLP vectorization
        obj_novec = os.path.join(td, "novec.o")
        cmd_novec = [
            rustc, "--crate-type=staticlib", "-O", "-C", "target-cpu=native",
            "-C", "no-vectorize-loops", "-C", "no-vectorize-slp",
            "--emit=obj", src, "-o", obj_novec
        ]
        try:
            subprocess.check_call(cmd_novec, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception as e:
            print(f"[SELF-TEST ERROR] Failed to compile scalar test snippet: {e}")
            return False

        # 3. Analyze Case A
        disasm_vec = disassemble_file(obj_vec, objdump_cmd)
        rep_vec = parse_and_check(disasm_vec, "x86_64", [r"vectorizable_kernel"])

        # 4. Analyze Case B
        disasm_novec = disassemble_file(obj_novec, objdump_cmd)
        rep_novec = parse_and_check(disasm_novec, "x86_64", [r"vectorizable_kernel"])

        print(f"[SELF-TEST] Case A (Vectorized): status={rep_vec['status']}, "
              f"violations={rep_vec['hot_kernel_violations']}, "
              f"vector_arith={rep_vec['summary']['hot_kernels']['vector_arithmetic_count']}, "
              f"vector_moves={rep_vec['summary']['hot_kernels']['vector_moves_count']}")

        print(f"[SELF-TEST] Case B (Scalar)    : status={rep_novec['status']}, "
              f"violations={rep_novec['hot_kernel_violations']}, "
              f"vector_arith={rep_novec['summary']['hot_kernels']['vector_arithmetic_count']}, "
              f"vector_moves={rep_novec['summary']['hot_kernels']['vector_moves_count']}")

        if rep_vec["status"] != "FAIL" or rep_vec["hot_kernel_violations"] == 0:
            print("[SELF-TEST FAILED] Expected Case A to be flagged for vector arithmetic, but it passed!")
            return False

        if rep_novec["status"] != "PASS" or rep_novec["hot_kernel_violations"] != 0:
            print("[SELF-TEST FAILED] Expected Case B to pass cleanly, but it failed!")
            return False

    print("[SELF-TEST PASSED] Codegen analyzer successfully differentiated vector arithmetic from scalar code!")
    return True


def main():
    parser = argparse.ArgumentParser(
        description="Verify that hot Rust kernels contain NO vector-register arithmetic."
    )
    parser.add_argument("inputs", nargs="*", help="Rust binaries, .o object files, or .a libraries to check")
    parser.add_argument("--arch", default="auto", choices=["auto", "x86_64", "aarch64", "riscv64"],
                        help="Target architecture (default: auto)")
    parser.add_argument("--symbols", nargs="+", default=DEFAULT_HOT_PATTERNS,
                        help="Regex patterns for hot-kernel symbols to verify")
    parser.add_argument("--all-symbols", action="store_true",
                        help="Analyze and report all symbols, not just matched kernels")
    parser.add_argument("--output-listing", help="Path to save disassembly listing")
    parser.add_argument("--output-json", help="Path to save JSON report")
    parser.add_argument("--objdump", help="Path to custom objdump or llvm-objdump binary")
    parser.add_argument("--strict", action="store_true",
                        help="Fail if no hot kernels were found matching the patterns")
    parser.add_argument("--self-test", action="store_true",
                        help="Run self-test to verify detection logic")
    parser.add_argument("--quiet", action="store_true", help="Suppress verbose stdout output")

    args = parser.parse_args()

    if args.self_test:
        success = run_self_test(args.objdump)
        sys.exit(0 if success else 1)

    if not args.inputs:
        parser.error("No input files specified. Provide at least one binary or use --self-test.")

    overall_exit_code = 0
    all_reports = []

    for file_path in args.inputs:
        if not os.path.exists(file_path):
            print(f"[ERROR] Input file not found: {file_path}", file=sys.stderr)
            overall_exit_code = 1
            continue

        arch = detect_arch(file_path, args.arch)
        if not args.quiet:
            print(f"[INFO] Inspecting {file_path} (detected arch: {arch})...")

        try:
            disasm = disassemble_file(file_path, args.objdump)
        except Exception as e:
            print(f"[ERROR] {e}", file=sys.stderr)
            overall_exit_code = 1
            continue

        rep = parse_and_check(disasm, arch, args.symbols, args.all_symbols)
        rep["file_path"] = os.path.abspath(file_path)
        all_reports.append(rep)

        if rep["status"] != "PASS":
            overall_exit_code = 1

        if args.strict and rep["hot_kernels_checked"] == 0:
            print(f"[ERROR] Strict mode: No symbols matched hot patterns in {file_path}!", file=sys.stderr)
            overall_exit_code = 1

        if not args.quiet:
            print(f"  Result: {rep['status']} | Hot kernels checked: {rep['hot_kernels_checked']} | "
                  f"Violations: {rep['hot_kernel_violations']}")
            if rep["hot_kernel_violations"] > 0:
                print(f"  [VIOLATION] Found vector-register arithmetic in hot kernels of {file_path}!",
                      file=sys.stderr)

    # Output listings and JSON
    if all_reports:
        combined_report = all_reports[0] if len(all_reports) == 1 else {
            "status": "PASS" if overall_exit_code == 0 else "FAIL",
            "reports": all_reports,
        }

        if args.output_json:
            os.makedirs(os.path.dirname(os.path.abspath(args.output_json)), exist_ok=True)
            with open(args.output_json, "w", encoding="utf-8") as f:
                json.dump(combined_report, f, indent=2)
            if not args.quiet:
                print(f"[INFO] JSON report written to {args.output_json}")

        if args.output_listing:
            os.makedirs(os.path.dirname(os.path.abspath(args.output_listing)), exist_ok=True)
            listing_str = "\n".join(generate_listing(r) for r in all_reports)
            with open(args.output_listing, "w", encoding="utf-8") as f:
                f.write(listing_str)
            if not args.quiet:
                print(f"[INFO] Listing written to {args.output_listing}")

    sys.exit(overall_exit_code)


if __name__ == "__main__":
    main()
