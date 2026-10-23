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
  (x86 vpadd/vpmul/..., AArch64 add/mul/umull/pmull/shl/ushr/and/orr/eor/...
  on SIMD&FP registers, RVV arithmetic)
- Memory Movement Filter: vector-register data movement used for zeroing and
  copying (x86 vector loads/stores and zero idioms; AArch64 movi/dup/mov,
  ld1/st1 and ldr/str/ldp/stp of q/d/s registers) is reported separately as
  vector moves and does NOT fail the gate
- Branches and PC-relative address forms (AArch64 b, b.cond, bl, cbz, cbnz,
  tbz, tbnz, ret, adr, adrp; x86 jmp/jcc/call) are scalar; their target
  addresses and <symbol> annotations are never parsed as registers
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

# Version of the instruction classifier, recorded in every report so that
# reports written by an older classifier can be told apart (v1 read AArch64
# branch targets such as `b.eq b3334` and `bl d470` as SIMD registers b3334/d470
# and lost the mnemonic of unconditional `b` lines).
CLASSIFIER_VERSION = "rbd-codegen-classifier-v2"

# Regex patterns for parsing disassembly
# Greedy: demangled generic symbols contain nested `<...>`, e.g.
# `encode_column_block::<lcpc_fields::mont127::Mont127>`.
SYM_HEADER_RE = re.compile(r"^[0-9a-fA-F]+\s+<(.+)>:\s*$")
# GNU objdump: `addr:\t<raw bytes> \t<mnemonic>\t<operands>`; llvm-objdump:
# `addr: <raw bytes> \t<mnemonic>\t<operands>`; with --no-show-raw-insn the
# raw bytes are absent. Raw bytes are hex groups each followed by one space,
# so a hex-looking mnemonic (AArch64 `b`, x86 `add`) is never taken for bytes.
_MNEM = r"([a-zA-Z][a-zA-Z0-9_\.]*)"
INSN_LINE_RE = re.compile(
    r"^\s*([0-9a-fA-F]+):\s+(?:(?:[0-9a-fA-F]{2,8} )+\s*\t\s*)?" + _MNEM + r"(?:\s+(.*))?$"
)
# A continuation line of a long x86 instruction carries raw bytes only.
RAW_BYTES_ONLY_RE = re.compile(r"^\s*[0-9a-fA-F]+:\s+(?:[0-9a-fA-F]{2} ?)+\s*$")
# Listing written by generate_listing(): `    <addr>: <mnemonic> <operands>`.
LISTING_LINE_RE = re.compile(r"^    ([0-9a-fA-F]+): (\S+)(?: (.*))?$")


def parse_instruction_line(line: str) -> Optional[Tuple[str, str, str]]:
    """Extracts (address, mnemonic, operands) from a disassembly line."""
    if RAW_BYTES_ONLY_RE.match(line):
        return None
    m = INSN_LINE_RE.match(line)
    if m:
        return (m.group(1), m.group(2), m.group(3) or "")
    return None


def strip_annotations(arch: str, operands: str) -> str:
    """Removes objdump comments and `<symbol+off>` annotations from operands.

    Symbol names are not operands (demangled Rust names contain e.g.
    `typenum::bit::B1`), and AArch64 comments such as `// b.none` are not
    instructions. On x86 (AT&T) `#` starts a comment; on AArch64 it marks an
    immediate and is kept.
    """
    ops = operands or ""
    if "<" in ops:
        ops = ops[:ops.index("<")]
    if "//" in ops:
        ops = ops[:ops.index("//")]
    if arch == "x86_64" and "#" in ops:
        ops = ops[:ops.index("#")]
    return ops.strip()

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
# SIMD&FP registers as they appear in operands: vector arrangements (v0.2d,
# v1.16b, lanes v0.d[1]), scalar views of the same register file used by
# vector loads and stores (q0/d0/s0/h0/b0), and SVE z0-z31/p0-p15. Register
# numbers are limited to the architectural range, so a hex branch target such
# as `b3334` or `d470` can never match.
_R31 = r"(?:3[01]|[12][0-9]|[0-9])"
AARCH64_SIMD_FP_REG_RE = re.compile(
    r"(?<![\w.#])(v" + _R31 + r"(?:\.[0-9]*[bshdq])?|[qdshb]" + _R31 + r"|z" + _R31
    + r"(?:\.[bhsdq])?|p(?:1[0-5]|[0-9]))(?![\w])",
    re.IGNORECASE,
)

# Branches and PC-relative address formation: scalar control flow whose
# operands are general-purpose registers, immediates and target addresses.
AARCH64_BRANCHES = {
    "b", "bl", "br", "blr", "ret", "cbz", "cbnz", "tbz", "tbnz", "eret",
    "braa", "brab", "blraa", "blrab", "braaz", "brabz", "blraaz", "blrabz", "retaa", "retab",
    "adr", "adrp",
}

# Data movement on SIMD&FP registers (loads, stores, register moves, immediates):
# zeroing (movi v0.2d, #0) and copying (ldp/stp q0, q1) of memory. `ldr`/`str`/
# `ldp`/`stp`/`ldur`/`stur` count only when a SIMD&FP register is involved; on
# general-purpose registers they are ordinary scalar accesses.
AARCH64_VEC_MOVES = {
    "ld1", "st1", "ld2", "st2", "ld3", "st3", "ld4", "st4",
    "ld1r", "ld2r", "ld3r", "ld4r", "ldr", "str", "ldp", "stp", "ldur", "stur",
    "ldnp", "stnp",
    "mov", "movi", "mvni", "dup", "ins", "umov", "smov", "fmov",
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
    ops = strip_annotations(arch, operands)

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
        # Branches (b, b.cond, bc.cond, bl, cbz, cbnz, tbz, tbnz, ret, ...) and
        # adr/adrp are scalar whatever their target address looks like.
        if mnem in AARCH64_BRANCHES or mnem.startswith(("b.", "bc.")):
            return ("SCALAR_BRANCH", f"{mnem} {ops}".strip())
        has_vec_reg = bool(AARCH64_SIMD_FP_REG_RE.search(ops))
        if has_vec_reg:
            if mnem in AARCH64_VEC_MOVES:
                return ("VEC_MOVE", f"{mnem} {ops}".strip())
            # Conservative: arithmetic (add/sub/mul/mla/umull/pmull/shl/ushr/
            # and/orr/eor/... on vN.<arrangement>), scalar SIMD&FP arithmetic,
            # permutes, conversions and anything unrecognized count as vector
            # arithmetic.
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

    # A PASS with no matched hot-kernel symbol would be vacuous; report it as
    # NO_HOT_KERNELS (not a failure unless --strict, but not scalar evidence).
    if hot_kernel_violations:
        overall_status = "FAIL"
    elif hot_kernels_checked == 0:
        overall_status = "NO_HOT_KERNELS"
    else:
        overall_status = "PASS"

    return {
        "status": overall_status,
        "arch": arch,
        "classifier_version": CLASSIFIER_VERSION,
        "hot_patterns": hot_patterns,
        "hot_kernels_checked": hot_kernels_checked,
        "hot_kernel_violations": hot_kernel_violations,
        # Both counts over the hot kernels: vector arithmetic fails the gate,
        # vector moves (zeroing/copying through vector registers) do not.
        "vector_arithmetic_count": hot_stats["vector_arithmetic_count"],
        "vector_moves_count": hot_stats["vector_moves_count"],
        "summary": {
            "hot_kernels": hot_stats,
            "outside_hot_kernels": outside_stats,
        },
        "symbols": symbols_report,
    }


LISTING_SYMBOL_RE = re.compile(r"^\[[A-Z_]+\] \[(?:HOT KERNEL|NON-KERNEL SYMBOL)\] <(.+)>$")
LISTING_ARCH_RE = re.compile(r"Target Architecture: ([A-Za-z0-9_]+)")


def listing_to_disassembly(listing_text: str) -> Tuple[Optional[str], str, int]:
    """Converts a listing written by generate_listing() back into objdump-style
    text for parse_and_check(), so that an archived listing (disassembly.txt)
    can be re-classified without the binary.

    Listings written by classifier v1 lost the mnemonic of unconditional `b`
    lines (`b 48ac8 <sym>` was parsed as mnemonic `48ac8` with operand `<sym>`);
    such AArch64 lines are restored as `b <target> <sym>`, and the x86 raw-byte
    continuation lines that v1 listed as instructions are dropped. Returns (arch, text,
    restored_b_count). Only listed symbols (every hot kernel, plus non-kernel
    symbols that the earlier run flagged) are present in a listing.
    """
    arch = None
    out: List[str] = []
    in_disasm = False
    restored = 0
    for line in listing_text.splitlines():
        if arch is None:
            m_arch = LISTING_ARCH_RE.search(line)
            if m_arch:
                arch = m_arch.group(1)
        m_sym = LISTING_SYMBOL_RE.match(line)
        if m_sym:
            out.append(f"0000000000000000 <{m_sym.group(1)}>:")
            in_disasm = False
            continue
        if line.strip() == "Disassembly:":
            in_disasm = True
            continue
        if not in_disasm:
            continue
        m = LISTING_LINE_RE.match(line)
        if not m:
            continue
        addr, mnem, ops = m.group(1), m.group(2), m.group(3) or ""
        if all(re.fullmatch(r"[0-9a-fA-F]{2}", tok) for tok in f"{mnem} {ops}".split()):
            # v1 also listed x86 raw-byte continuation lines (`9640e: 00 00`)
            # as instructions; they are not.
            continue
        if arch == "aarch64" and re.fullmatch(r"[0-9a-fA-F]+", mnem) and ops.startswith("<"):
            ops = f"{mnem} {ops}"
            mnem = "b"
            restored += 1
        out.append(f"  {addr}:\t{mnem}\t{ops}".rstrip())
    return arch, "\n".join(out) + "\n", restored


def generate_listing(report: Dict) -> str:
    """Formats annotated disassembly listing of hot kernels and flagged instructions."""
    lines = [
        "================================================================================",
        "RealBrakedownPerf - Code Generation & Scalar Verification Listing",
        f"Status: {report['status']} | Target Architecture: {report['arch']}"
        f" | Classifier: {report.get('classifier_version', 'v1')}",
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
        # Vector arithmetic on SIMD&FP registers fails the gate.
        ("aarch64", "add", "v0.2d, v1.2d, v2.2d", "VEC_ARITH"),
        ("aarch64", "sub", "v3.4s, v3.4s, v4.4s", "VEC_ARITH"),
        ("aarch64", "mul", "v0.4s, v1.4s, v2.4s", "VEC_ARITH"),
        ("aarch64", "mla", "v0.4s, v1.4s, v2.s[1]", "VEC_ARITH"),
        ("aarch64", "umull", "v0.2d, v1.2s, v2.2s", "VEC_ARITH"),
        ("aarch64", "umull2", "v0.2d, v1.4s, v2.4s", "VEC_ARITH"),
        ("aarch64", "pmull", "v0.1q, v1.1d, v2.1d", "VEC_ARITH"),
        ("aarch64", "shl", "v0.2d, v1.2d, #3", "VEC_ARITH"),
        ("aarch64", "and", "v0.16b, v1.16b, v2.16b", "VEC_ARITH"),
        ("aarch64", "orr", "v0.16b, v1.16b, v2.16b", "VEC_ARITH"),
        ("aarch64", "eor", "v0.16b, v1.16b, v2.16b", "VEC_ARITH"),
        ("aarch64", "add", "d0, d1, d2", "VEC_ARITH"),  # AdvSIMD scalar
        # Scalar data processing with similar mnemonics stays scalar.
        ("aarch64", "add", "x29, sp, #0x170", "SCALAR_ADD"),
        ("aarch64", "eor", "x8, x9, x10", "SCALAR"),
        ("aarch64", "mul", "x9, x9, x28", "SCALAR_MUL"),
        # Vector-register zeroing/copying is a vector move, not a violation.
        ("aarch64", "movi", "v1.2d, #0x0", "VEC_MOVE"),
        ("aarch64", "dup", "v0.2d, xzr", "VEC_MOVE"),
        ("aarch64", "ldp", "q0, q1, [x2, #64]", "VEC_MOVE"),
        ("aarch64", "stur", "q0, [sp, #88]", "VEC_MOVE"),
        # Branches and adrp: hex targets (b3334, d470, b0000) and <symbol>
        # annotations (typenum::bit::B1) are not registers (classifier v1 bug).
        ("aarch64", "b.eq", "b3334 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_test_fields::ft63::Ft63>+0x40>  // b.none", "SCALAR_BRANCH"),
        ("aarch64", "cbz", "x0, b3988 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_test_fields::ft63::Ft63>+0x694>", "SCALAR_BRANCH"),
        ("aarch64", "bl", "d470 <<alloc::raw_vec::RawVecInner<_>>::reserve::do_reserve_and_handle::<alloc::alloc::Global>>", "SCALAR_BRANCH"),
        ("aarch64", "b", "b3a64 <x+0x10>", "SCALAR_BRANCH"),
        ("aarch64", "tbnz", "w8, #0, b5930 <x+0x354>", "SCALAR_BRANCH"),
        ("aarch64", "adrp", "x9, b0000 <core::fmt::float::float_to_decimal_common_shortest::<f64>+0x3a4>", "SCALAR_BRANCH"),
        ("aarch64", "csel", "x8, x9, x8, cc\t// cc = lo, ul, last", "SCALAR"),
        ("aarch64", "ldr", "x1, [x2, #16]", "SCALAR"),
        ("aarch64", "stp", "x29, x30, [sp, #368]", "STACK_SPILL"),
        ("riscv64", "vsetvli", "t0, a0, e64, m1, ta, ma", "VEC_ARITH"),
        ("riscv64", "mulhu", "a0, a1, a2", "SCALAR_MULH"),
    ]
    for arch, mnem, ops, expected in cases:
        got, _ = classify_instruction(arch, mnem, ops)
        if got != expected:
            print(f"[SELF-TEST FAILED] {arch} '{mnem} {ops}': expected {expected}, got {got}")
            return False
    print(f"[SELF-TEST] Instruction classification: {len(cases)} cases OK")

    # Line parsing: GNU objdump and llvm-objdump forms, including the
    # unconditional AArch64 `b` whose mnemonic v1 mistook for a raw byte.
    line_cases = [
        ("   49034:\t14000000 \tb\t49060 <f+0x5d8>", ("49034", "b", "49060 <f+0x5d8>")),
        ("   48a88:\td10743ff \tsub\tsp, sp, #0x1d0", ("48a88", "sub", "sp, sp, #0x1d0")),
        ("   48aa4: fd c3 05 91  \tadd\tx29, sp, #0x170", ("48aa4", "add", "x29, sp, #0x170")),
        ("  401000:\t48 01 d8             \tadd    %rbx,%rax", ("401000", "add", "%rbx,%rax")),
        ("  401003:\t00 00 00 ", None),  # x86 continuation of raw bytes
    ]
    for line, expected in line_cases:
        got = parse_instruction_line(line)
        if got != expected:
            print(f"[SELF-TEST FAILED] parse '{line}': expected {expected}, got {got}")
            return False
    print(f"[SELF-TEST] Line parsing: {len(line_cases)} cases OK")

    # Real AArch64 hot-kernel lines (GNU objdump 2.40, rustc 1.99.1, Neoverse-V2
    # c4a-highmem-16), copied verbatim from the listing of the 2026-10-23 Arm
    # session, code/results/gcp-c4a-highmem-16/2026-10-23-acfac2349c11/arm1/
    # arm_this_work_t1_n18-22-26/.../this_work/codegen/disassembly.txt.
    # Classifier v1 reported 56 vector-arithmetic hits (all branches) there;
    # these must PASS with vector moves only.
    real_arm_listing = """\
Status: FAIL | Target Architecture: aarch64
[FAIL] [HOT KERNEL] <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::mont127::Mont127>>
  Disassembly:
    b46c0: stp x2, x5, [x29, #-96]
    b4714: b.cs b4730 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::mont127::Mont127>+0x94>  // b.hs, b.nlast
    b4730: movi v0.2d, #0x0
    b4748: stp q0, q0, [x8]
    b4754: movi v0.2d, #0x0
    b4760: stp q0, q0, [x8]
    b4874: b.eq b5244 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::mont127::Mont127>+0xba8>  // b.none
    b4ccc: b.hi b5278 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::mont127::Mont127>+0xbdc>  // b.pmore
    b5070: b.ls b5094 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::mont127::Mont127>+0x9f8>  // b.plast
    b517c: stp q0, q0, [x11]
[FAIL] [HOT KERNEL] <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_test_fields::ft63::Ft63>>
  Disassembly:
    b332c: b.eq b3334 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_test_fields::ft63::Ft63>+0x40>  // b.none
    b3358: cbz x0, b3988 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_test_fields::ft63::Ft63>+0x694>
    b3368: b.cc b3388 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_test_fields::ft63::Ft63>+0x94>  // b.lo, b.ul, b.last
    b33a0: b.ne b3938 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_test_fields::ft63::Ft63>+0x644>  // b.any
[FAIL] [HOT KERNEL] <lcpc_brakedown_pc::encode::reed_solomon_column_block::<lcpc_test_fields::ft127::Ft127>>
  Disassembly:
    b5608: cbnz x8, b5930 <lcpc_brakedown_pc::encode::reed_solomon_column_block::<lcpc_test_fields::ft127::Ft127>+0x354>
    b5918: b.eq b5730 <lcpc_brakedown_pc::encode::reed_solomon_column_block::<lcpc_test_fields::ft127::Ft127>+0x154>  // b.none
[PASS] [HOT KERNEL] <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::fp2m61::Fp2M61>>
  Disassembly:
    48ac0: b.eq 48ac8 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::fp2m61::Fp2M61>+0x40>  // b.none
    48ac4: bl e0e4 <alloc::raw_vec::capacity_overflow>
    48b0c: movi v0.2d, #0x0
    48b1c: stp q0, q0, [x8], #32
    48b2c: mul x9, x9, x28
    48b5c: csel x8, x9, x8, cc\t// cc = lo, ul, last
    48bb0: adrp x9, 1bd000 <__do_global_dtors_aux_fini_array_entry+0x17c8>
    48bcc: 48be4 <lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::fp2m61::Fp2M61>+0x15c>
    48cb0: extr x13, x9, x10, #61
    48e04: umulh x20, x16, x11
    49894: adcs x4, x20, x4
[PASS] [HOT KERNEL] <lcpc_fields::mont127::reduce_column_mont127>
  Disassembly:
    1249e4: movi v0.2d, #0x0
    1249fc: stp q0, q0, [x2], #32
    124a54: stp x11, x12, [x8, #-8]
"""
    arm_arch, arm_text, arm_restored = listing_to_disassembly(real_arm_listing)
    rep_arm = parse_and_check(arm_text, arm_arch or "aarch64", DEFAULT_HOT_PATTERNS)
    arm_hot = rep_arm["summary"]["hot_kernels"]
    print(f"[SELF-TEST] Real AArch64 lines: status={rep_arm['status']}, "
          f"vector_arith={arm_hot['vector_arithmetic_count']}, vector_moves={arm_hot['vector_moves_count']}, "
          f"restored_b={arm_restored}")
    if (arm_arch != "aarch64" or rep_arm["status"] != "PASS" or arm_hot["vector_arithmetic_count"] != 0
            or arm_hot["vector_moves_count"] != 9 or arm_restored != 1 or rep_arm["hot_kernels_checked"] != 5):
        print("[SELF-TEST FAILED] Real AArch64 hot-kernel lines must PASS with 9 vector moves and no vector arithmetic")
        return False
    # The same kernel with one vector multiply-accumulate must FAIL.
    arm_bad = arm_text.replace("\tmul\tx9, x9, x28", "\tumlal\tv0.2d, v1.2s, v2.2s")
    if parse_and_check(arm_bad, "aarch64", DEFAULT_HOT_PATTERNS)["status"] != "FAIL":
        print("[SELF-TEST FAILED] AArch64 vector arithmetic (umlal v0.2d) in a hot kernel was not flagged")
        return False

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

        # 5. The same snippet for AArch64, when the target's standard library
        # is installed (rustup target add aarch64-unknown-linux-gnu) and an
        # objdump that understands AArch64 (llvm-objdump) is available.
        arm_status = "skipped"
        arm_objdump = objdump_cmd or find_llvm_objdump()
        obj_arm_vec = os.path.join(td, "arm_vec.o")
        obj_arm_novec = os.path.join(td, "arm_novec.o")
        base_arm = [rustc, "--crate-type=staticlib", "-O", "--target", "aarch64-unknown-linux-gnu", "--emit=obj", src]
        try:
            subprocess.check_call(base_arm + ["-o", obj_arm_vec], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            subprocess.check_call(base_arm + ["-C", "no-vectorize-loops", "-C", "no-vectorize-slp", "-o", obj_arm_novec],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception:
            arm_objdump = None
        if arm_objdump:
            try:
                rep_arm_vec = parse_and_check(disassemble_file(obj_arm_vec, arm_objdump), "aarch64", [r"vectorizable_kernel"])
                rep_arm_novec = parse_and_check(disassemble_file(obj_arm_novec, arm_objdump), "aarch64", [r"vectorizable_kernel"])
            except Exception as e:
                print(f"[SELF-TEST] AArch64 compile check skipped ({e})")
            else:
                print(f"[SELF-TEST] AArch64 Case A (Vectorized): status={rep_arm_vec['status']}, "
                      f"vector_arith={rep_arm_vec['summary']['hot_kernels']['vector_arithmetic_count']}")
                print(f"[SELF-TEST] AArch64 Case B (Scalar)    : status={rep_arm_novec['status']}, "
                      f"vector_arith={rep_arm_novec['summary']['hot_kernels']['vector_arithmetic_count']}, "
                      f"vector_moves={rep_arm_novec['summary']['hot_kernels']['vector_moves_count']}")
                if rep_arm_vec["status"] != "FAIL" or rep_arm_novec["status"] != "PASS":
                    print("[SELF-TEST FAILED] AArch64 vectorized snippet must FAIL and the scalar build must PASS")
                    return False
                arm_status = "ok"
        if arm_status != "ok":
            print("[SELF-TEST] AArch64 compile check skipped (no aarch64 rust-std or AArch64-capable objdump)")

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
    parser.add_argument("--from-listing", action="store_true",
                        help="Inputs are listings (disassembly.txt) written by an earlier run of this script; "
                             "re-classify their instructions with the current classifier (no binary needed; "
                             "only the symbols present in the listing are checked)")
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

        restored_b = None
        if args.from_listing:
            with open(file_path, "r", encoding="utf-8", errors="replace") as f:
                listing_arch, disasm, restored_b = listing_to_disassembly(f.read())
            arch = args.arch if args.arch != "auto" else (listing_arch or "x86_64")
            if not args.quiet:
                print(f"[INFO] Re-classifying listing {file_path} (arch: {arch})...")
        else:
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
        if args.from_listing:
            rep["source"] = {
                "kind": "re-classified listing",
                "listing": os.path.abspath(file_path),
                "restored_unconditional_b_lines": restored_b,
                "note": "only symbols present in the listing (all hot kernels of the original run) were checked; "
                        "outside_hot_kernels counts cover the listed non-kernel symbols only",
            }
        all_reports.append(rep)

        if rep["status"] == "FAIL":
            overall_exit_code = 1

        if args.strict and rep["hot_kernels_checked"] == 0:
            print(f"[ERROR] Strict mode: No symbols matched hot patterns in {file_path}!", file=sys.stderr)
            overall_exit_code = 1

        if not args.quiet:
            print(f"  Result: {rep['status']} | Hot kernels checked: {rep['hot_kernels_checked']} | "
                  f"Violations: {rep['hot_kernel_violations']} | Vector arithmetic: "
                  f"{rep['vector_arithmetic_count']} | Vector moves: {rep['vector_moves_count']}")
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
