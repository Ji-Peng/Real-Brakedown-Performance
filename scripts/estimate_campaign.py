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
estimate_campaign.py - Estimates the wall time of the benchmark sessions that
reproduce/soc/run_stages.sh submits (--preset x86 on xcnh, --preset arm on
xcng), stage by stage.

The stage list comes from `run_stages.sh --preset P --print-stages`, the block
catalogue (block id -> logN list) mirrors reproduce/all.sh, and each block's
time is

    fixed(block) + sum over its logN values n of rate(block) * 2^(n - 28)

with rates in seconds per 2^28 for one warm-up and five timed runs (the
campaign setting). The rates were measured per block on an 8-core AMD Zen 4
server with one thread per core (x86-64) and on an Arm Neoverse-V2 server
(Arm); --speed-x86 and --speed-arm scale them for the target nodes
(defaults X86_DEFAULT_SPEED and ARM_DEFAULT_SPEED, from smoke runs on xcnh
and xcng compared with the reference machines). Original-LCPC rates are per build variant.
SIMD-Spielman blocks use a per-size table instead, because upstream's
single-threaded Orion graph generation dominates them up to 2^26.
"""

import argparse
import os
import re
import subprocess
import sys
from typing import Dict, List, Tuple

CODE_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUN_STAGES = os.path.join(CODE_DIR, "reproduce", "soc", "run_stages.sh")

FULL_LOGN = [16, 18, 20, 22, 24, 26, 28]
X86_DEFAULT_SPEED = 1.3  # EPYC 9755 / Zen 4 reference: 1.5 encode and commit, 1.1 prove at 2^20, T = 1
ARM_DEFAULT_SPEED = 0.3  # Neoverse-N1 / Neoverse-V2: 0.25 encode, 0.32 commit at 2^18, T = 1

# (seconds per 2^28 for warm-up 1 + 5 runs, fixed seconds per block)
X86_RATES: Dict[str, Tuple[float, float]] = {
    "tw_a_t1": (630.0, 30.0),      # This Work mode (a), encode + commit/prove/verify
    "tw_a_t2": (330.0, 30.0),
    "tw_a_t4": (180.0, 25.0),
    "tw_a_t8": (95.0, 25.0),
    "tw_b_t1": (1305.0, 20.0),     # mode (b), one field, Brakedown and Orion shapes
    "tw_b_t8": (315.0, 20.0),
    "lcpc_t1": (1720.0, 25.0),     # original LCPC, per build variant
    "lcpc_t2": (872.0, 15.0),
    "lcpc_t4": (444.0, 10.0),
    "lcpc_t8": (225.0, 10.0),
    "poly_t1": (540.0, 0.0),
    "poly_t8": (90.0, 30.0),
    "ligero": (180.0, 30.0),
}
# SIMD-Spielman, seconds per logN value and block (one thread count, both
# shapes up to 2^26, Brakedown only at 2^28), plus the build and upstream tests.
SIMD_SECONDS = {16: 20.0, 18: 20.0, 20: 40.0, 22: 60.0, 24: 200.0, 26: 1100.0, 28: 100.0}
SIMD_FIXED = 60.0
# Per-technique breakdown (steps.sh, one mode): ten builds plus the timings.
STEPS_FIXED = 1500.0  # 2297 s on xcnh for both modes with runs 2 (smoke): mostly the per-tag builds
STEPS_RATE = 6000.0
# Arm (Neoverse-V2 reference): This Work mode (a) and original LCPC per variant.
ARM_RATES = {"tw_a_t1": (1160.0, 30.0), "tw_a_t8": (90.0, 10.0),
             "lcpc_t1": (1465.0, 25.0), "lcpc_t8": (290.0, 10.0)}


def full_catalogue() -> List[Tuple[str, List[int], str]]:
    """(block id, logN list, rate key) of the 'full' profile of reproduce/all.sh."""
    blocks = []
    for t in (1, 8):
        blocks.append((f"full_this_work_mode_a_t{t}", FULL_LOGN, f"tw_a_t{t}"))
        blocks.append((f"full_lcpc_orig_mode_a_t{t}", FULL_LOGN, f"lcpc_t{t}"))
        blocks.append((f"full_this_work_ft127_t{t}", [20, 24], f"tw_a_t{t}"))
    for field in ("mont127", "mers127", "fp2m61"):
        for t in (1, 8):
            blocks.append((f"full_this_work_mode_b_{field}_t{t}", FULL_LOGN, f"tw_b_t{t}"))
        for t in (1, 8, 7):
            blocks.append((f"full_simd_spielman_{field}_t{t}", FULL_LOGN, "simd"))
    blocks.append(("full_steps_mode_a", [16, 20, 24, 28], "steps"))
    blocks.append(("full_steps_mode_b", [16, 20, 24, 28], "steps"))
    for t in (1, 8):
        for s in (1, 2, 4, 8, 16, 32, 64, 128, 256):
            blocks.append((f"full_slice_sweep_s{s}_t{t}", [20, 22, 24, 26, 28], f"tw_a_t{t}"))
    for t in (1, 2, 4, 8):
        blocks.append((f"full_this_work_threads_t{t}", [20, 24, 26, 28], f"tw_a_t{t}"))
        blocks.append((f"full_lcpc_orig_threads_t{t}", [20, 24, 26, 28], f"lcpc_t{t}"))
    blocks.append(("full_polycommit_t1", FULL_LOGN, "poly_t1"))
    blocks.append(("full_polycommit_t8", FULL_LOGN, "poly_t8"))
    blocks.append(("full_ligero_rs", [20, 22, 24], "ligero"))
    return blocks


def arm_catalogue() -> List[Tuple[str, List[int], str]]:
    return [(f"arm_{impl}_t{t}", [18, 22, 26], f"{key}_t{t}")
            for impl, key in (("this_work", "tw_a"), ("lcpc_orig", "lcpc")) for t in (1, 8)]


def preset_stages(preset: str) -> List[Tuple[str, str, List[int], int]]:
    """(name, regex, logN list or [], LCPC variant count) from run_stages.sh --print-stages."""
    out = subprocess.run(["sh", RUN_STAGES, "--preset", preset, "--print-stages"],
                         capture_output=True, text=True, check=True).stdout
    stages = []
    for line in out.splitlines()[1:]:
        name, regex, logn, variants = line.split("\t")
        stages.append((name, regex, [int(x) for x in logn.split(",") if x], len(variants.split(","))))
    return stages


def block_seconds(rate_key: str, logns: List[int], variants: int, rates: Dict, speed: float) -> float:
    if not logns:
        return 0.0
    if rate_key == "simd":
        return (SIMD_FIXED + sum(SIMD_SECONDS[n] for n in logns)) / speed
    if rate_key == "steps":
        return (STEPS_FIXED + STEPS_RATE * sum(2.0 ** (n - 28) for n in logns)) / speed
    rate, fixed = rates[rate_key]
    work = rate * sum(2.0 ** (n - 28) for n in logns)
    if rate_key.startswith("lcpc"):
        work *= variants
    return (fixed + work) / speed


def estimate(preset: str, speed: float) -> List[Tuple[str, int, float]]:
    catalogue, rates = (full_catalogue(), X86_RATES) if preset == "x86" else (arm_catalogue(), ARM_RATES)
    rows = []
    for name, regex, logn_filter, variants in preset_stages(preset):
        seconds = 0.0
        count = 0
        for block_id, logns, key in catalogue:
            if regex and not re.search(regex, block_id):
                continue
            kept = [n for n in logns if not logn_filter or n in logn_filter]
            if not kept:
                continue
            count += 1
            seconds += block_seconds(key, kept, variants, rates, speed)
        rows.append((name, count, seconds))
    return rows


def main():
    parser = argparse.ArgumentParser(description="Estimate the wall time of the SoC cluster benchmark sessions.")
    parser.add_argument("--preset", choices=["x86", "arm", "all"], default="all")
    parser.add_argument("--speed-x86", type=float, default=X86_DEFAULT_SPEED,
                        help=f"throughput of the xcnh cores relative to the x86-64 reference (default {X86_DEFAULT_SPEED})")
    parser.add_argument("--speed-arm", type=float, default=ARM_DEFAULT_SPEED,
                        help=f"throughput of the xcng cores relative to the Arm reference (default {ARM_DEFAULT_SPEED})")
    args = parser.parse_args()

    presets = ["x86", "arm"] if args.preset == "all" else [args.preset]
    for preset in presets:
        speed = args.speed_x86 if preset == "x86" else args.speed_arm
        rows = estimate(preset, speed)
        total = sum(s for _, _, s in rows)
        node = "xcnh" if preset == "x86" else "xcng"
        print(f"### --preset {preset} (one session on {node}, speed factor {speed:g})\n")
        print("| Stage | Blocks | Estimated wall time |")
        print("| :--- | ---: | ---: |")
        for name, count, seconds in rows:
            print(f"| {name} | {count} | {seconds / 60.0:.0f} min |")
        print(f"| **Session** | {sum(c for _, c, _ in rows)} | **{total / 3600.0:.1f} h** |\n")
    print("Rates: measured block times of an earlier campaign on 8-core AMD Zen 4 and Arm Neoverse-V2 servers; "
          "scale with --speed-x86 / --speed-arm.")


if __name__ == "__main__":
    sys.exit(main())
