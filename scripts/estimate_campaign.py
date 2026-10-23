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
estimate_campaign.py - Evaluates workload scaling and estimates wall-clock VM hours
for Google Cloud benchmarking sessions (plan 06 §6 item 10).

Fits per-implementation O(N) work constants from smoke run benchmark measurements,
or falls back to reference models when raw smoke data is unavailable, and computes
estimated wall-clock runtime and GCP costs for:
  - Profile 'full' (x86-64 c3d-highmem-16, 8 benchmark cores: E1-E13, logN 16..28)
  - Profile 'arm'  (Arm c4a-highmem-16: E14, logN 18,22,26)
"""

import argparse
import json
import math
import os
import sys
from collections import defaultdict
from typing import Dict, List, Optional, Tuple

# GCP On-Demand hourly pricing (approximate US-Central1 list price in USD)
# c3d-highmem-16 (16 vCPUs = 8 cores with --threads-per-core=1, 128 GB RAM):
# scaled linearly from the earlier c3d-highmem-30 figure of 2.17 USD/h;
# check the current list price (or pass --price-c3d).
PRICE_PER_HOUR_C3D = 2.17 * 16 / 30
PRICE_PER_HOUR_C4A = 0.88  # c4a-highmem-16 (16 vCPUs, 128 GB RAM)

# Reference defaults (nanoseconds per coefficient element) used when smoke data is unavailable
DEFAULT_RATES = {
    # implementation: {threads: ns_per_element}
    "this_work_mode_a": {1: 28.0, 2: 15.0, 4: 8.5, 7: 6.0, 8: 5.5},
    "this_work_mode_b": {1: 18.0, 2: 9.8, 4: 5.6, 7: 4.2, 8: 3.8},
    "lcpc_orig":        {1: 215.0, 2: 118.0, 4: 68.0, 7: 46.0, 8: 42.0},
    "simd_spielman":    {1: 4.8, 2: 2.6, 4: 1.6, 7: 1.2, 8: 1.1},
    "polycommit":       {1: 160.0, 2: 90.0, 4: 52.0, 7: 35.0, 8: 32.0},
}

# Thread counts of the full profile on the 8-core benchmark set (all.sh).
THREAD_SCALING_T = [1, 2, 4, 8]
THREAD_SCALING_LOGN = [20, 24, 26, 28]
SIMD_THREADS = [1, 7, 8]  # T=7: 7 workers + spin-waiting coordinator on 8 cores
MAX_ORION_LOGN = 26       # all.sh full profile: simd_spielman.sh --max-orion-logn 26

# Upstream SIMD-Spielman generates its Orion-shape graphs single-threaded once
# per benchmark process (one process per case). Observed in the 2026-10
# campaign on c3d-highmem-16: about 10 min per process at 2^26 and more than
# 90 min at 2^28 (hence --max-orion-logn 26); modelled as 600 s * (N/2^26)^1.6.
ORION_GRAPH_GEN_S_AT_26 = 600.0
ORION_GRAPH_GEN_EXPONENT = 1.6

# Wall time per original-LCPC driver run (warm-up or timed; encode, commit,
# prove and verify plus its share of build and golden checks) at 2^28, T=1,
# calibrated from the 2026-10-23 stage-B block full_lcpc_orig_mode_a_t1_n28:
# 3533 s for 2 build variants x (1 warm-up + 5 timed runs).
LCPC_BLOCK_S_PER_RUN_AT_28_T1 = 3533.0 / 12


def orion_graph_gen_seconds(logn: int) -> float:
    return ORION_GRAPH_GEN_S_AT_26 * (2.0 ** (logn - 26)) ** ORION_GRAPH_GEN_EXPONENT


def safe_float(v) -> Optional[float]:
    try:
        return float(v)
    except (ValueError, TypeError):
        return None


def runs_count_for_logn(logn: int) -> int:
    """Statistics per plan 06 §3: 2 warmups + 5 runs for logN < 26; 1 warmup + 5 runs for logN >= 26."""
    return 6 if logn >= 26 else 7


class CampaignEstimator:
    def __init__(self, results_dir: Optional[str] = None, lcpc_variants: int = 4):
        self.results_dir = results_dir
        # Each original-LCPC block builds and measures this many build variants.
        self.lcpc_variants = lcpc_variants
        self.fitted_rates: Dict[str, Dict[int, float]] = defaultdict(dict)
        self.samples_count: Dict[str, int] = defaultdict(int)
        self.data_source = "Reference Baseline Model"
        if results_dir and os.path.isdir(results_dir):
            self.load_smoke_results(results_dir)

    def load_smoke_results(self, root_dir: str):
        points: Dict[str, Dict[int, List[Tuple[int, float]]]] = defaultdict(lambda: defaultdict(list))

        for dirpath, _, filenames in os.walk(root_dir):
            for fname in filenames:
                if not fname.endswith(".json") or "manifest" in fname or "schedule" in fname:
                    continue
                path = os.path.join(dirpath, fname)
                try:
                    with open(path, "r", encoding="utf-8") as f:
                        data = json.load(f)
                except Exception:
                    continue

                items = data if isinstance(data, list) else [data]
                for item in items:
                    if not isinstance(item, dict):
                        continue
                    schema = item.get("schema_version")
                    meas = item.get("measurements", {}).get("encode_matrix", {}).get("summary", {})
                    med_ns = safe_float(meas.get("median_ns"))
                    dim = item.get("dimensions", {})
                    n_elem = dim.get("coefficient_count")
                    threads = item.get("threads", 1)

                    if not med_ns or not n_elem or n_elem <= 0:
                        continue

                    impl = item.get("implementation", "")
                    params = item.get("params", "")

                    key = None
                    if "lcpc-original" in impl or "lcpc_orig" in path:
                        key = "lcpc_orig"
                    elif "lcpc-fork" in impl or "this_work" in path:
                        if params == "simd-spielman":
                            key = "this_work_mode_b"
                        else:
                            key = "this_work_mode_a"
                    elif "simd_spielman" in path or "simd" in impl:
                        key = "simd_spielman"
                    elif "polycommit" in path or "poly" in impl:
                        key = "polycommit"

                    if key:
                        points[key][threads].append((n_elem, med_ns))

        # Fit rate (ns per element)
        for key, threads_map in points.items():
            for th, pt_list in threads_map.items():
                if pt_list:
                    # Linear O(N) fit: slope = sum(N * T) / sum(N^2)
                    sum_nt = sum(n * t for n, t in pt_list)
                    sum_n2 = sum(n * n for n, _ in pt_list)
                    rate = (sum_nt / sum_n2) if sum_n2 > 0 else (sum(t / n for n, t in pt_list) / len(pt_list))
                    self.fitted_rates[key][th] = max(rate, 0.1)
                    self.samples_count[key] += len(pt_list)

        if any(self.samples_count.values()):
            self.data_source = f"Fitted from Smoke Results in {root_dir}"

    def get_rate(self, impl: str, threads: int) -> float:
        """Returns ns per element for the given implementation and thread count."""
        if impl in self.fitted_rates and threads in self.fitted_rates[impl]:
            return self.fitted_rates[impl][threads]
        # Check nearest thread count in fitted
        if impl in self.fitted_rates and self.fitted_rates[impl]:
            avail_th = list(self.fitted_rates[impl].keys())
            closest_th = min(avail_th, key=lambda t: abs(t - threads))
            # Scale by thread ratio
            return self.fitted_rates[impl][closest_th] * (closest_th / threads)

        # Fallback to reference defaults
        base_rates = DEFAULT_RATES.get(impl, DEFAULT_RATES["this_work_mode_a"])
        if threads in base_rates:
            return base_rates[threads]
        # Approximate Amdahl scaling from single thread rate
        single_rate = base_rates.get(1, 30.0)
        eff = 0.85
        return (single_rate / (threads ** eff))

    def estimate_seconds(self, impl: str, threads: int, logn: int, runs: int) -> float:
        n = 2 ** logn
        ns_per_elem = self.get_rate(impl, threads)
        latency_ns = ns_per_elem * n
        total_ns = latency_ns * runs
        return total_ns / 1e9

    def evaluate_profile_arm(self) -> Dict:
        """
        Profile 'arm' (E14):
        Google Axion (c4a-highmem-16), logN in {18, 22, 26}, T in {1, 8}.
        Targets: This Work mode (a), Original LCPC mode (a).
        """
        logn_sizes = [18, 22, 26]
        threads_list = [1, 8]
        experiments = []

        total_seconds = 0.0

        # This Work
        tw_sec = 0.0
        for th in threads_list:
            for ln in logn_sizes:
                r = runs_count_for_logn(ln)
                s = self.estimate_seconds("this_work_mode_a", th, ln, r)
                tw_sec += s
        experiments.append(("This Work (mode a)", tw_sec))
        total_seconds += tw_sec

        # Original LCPC
        orig_sec = 0.0
        for th in threads_list:
            for ln in logn_sizes:
                r = runs_count_for_logn(ln)
                s = self.estimate_seconds("lcpc_orig", th, ln, r)
                orig_sec += s * self.lcpc_variants
        experiments.append(("Original LCPC (mode a)", orig_sec))
        total_seconds += orig_sec

        # Field benchmarks & setup overhead (10%)
        field_sec = 60.0
        overhead = total_seconds * 0.10
        experiments.append(("Field microbenchmarks", field_sec))
        experiments.append(("Setup, transpositions, JIT/OS overhead", overhead))
        total_seconds += field_sec + overhead

        hours = total_seconds / 3600.0
        cost_per_session = hours * PRICE_PER_HOUR_C4A

        return {
            "profile": "arm",
            "machine": "c4a-highmem-16",
            "experiments": experiments,
            "total_seconds": total_seconds,
            "total_hours": hours,
            "cost_per_session": cost_per_session,
            "sessions": 1,
            "total_campaign_cost": cost_per_session * 1,
        }

    def evaluate_profile_full(self) -> Dict:
        """
        Profile 'full' (E1-E13):
        x86-64 c3d-highmem-16 (8 cores), logN in {16, 18, 20, 22, 24, 26, 28}.
        All baselines and technique ablations.
        """
        full_logn = [16, 18, 20, 22, 24, 26, 28]
        experiments = []
        total_seconds = 0.0

        # E1: Headline comparison (This Work vs Original LCPC, T=1,8)
        e1_tw = sum(self.estimate_seconds("this_work_mode_a", th, ln, runs_count_for_logn(ln))
                    for th in [1, 8] for ln in full_logn)
        e1_orig = sum(self.estimate_seconds("lcpc_orig", th, ln, runs_count_for_logn(ln))
                      for th in [1, 8] for ln in full_logn) * self.lcpc_variants
        experiments.append(("E1: Headline This Work mode (a) T=1,8", e1_tw))
        experiments.append(("E1: Headline Original LCPC mode (a) T=1,8", e1_orig))
        total_seconds += e1_tw + e1_orig

        # E2: SIMD-Spielman comparison (mode b, 3 fields; Brakedown and Orion shapes)
        # 3 fields: Mont127, Mers127, Fp2M61 (approx equal complexity)
        e2_tw = sum(self.estimate_seconds("this_work_mode_b", th, ln, runs_count_for_logn(ln))
                    for th in [1, 8] for ln in full_logn) * 3 * 2
        e2_simd = sum(self.estimate_seconds("simd_spielman", th, ln, runs_count_for_logn(ln))
                      * (2 if ln <= MAX_ORION_LOGN else 1)
                      for th in SIMD_THREADS for ln in full_logn) * 3
        # One process per (shape, field, N, T) case, each generating its graph.
        e2_orion_gen = sum(orion_graph_gen_seconds(ln) for th in SIMD_THREADS
                           for ln in full_logn if ln <= MAX_ORION_LOGN) * 3
        experiments.append(("E2: Comparison This Work mode (b) 3 fields T=1,8, both shapes", e2_tw))
        experiments.append(("E2: Comparison SIMD-Spielman AVX-512 3 fields T=1,7,8 (Orion <= 2^26)", e2_simd))
        experiments.append(("E2: SIMD-Spielman Orion graph generation (one process per case, single-threaded)",
                            e2_orion_gen))
        total_seconds += e2_tw + e2_simd + e2_orion_gen

        # E3: Per-technique progression (steps.sh: 8 tags x 2 modes x 4 sizes: 16, 20, 24, 28)
        e3_sec = 0.0
        step_sizes = [16, 20, 24, 28]
        # Average rate across steps ~ 1.5x This Work rate
        for ln in step_sizes:
            r = runs_count_for_logn(ln)
            step_lat = self.estimate_seconds("this_work_mode_a", 1, ln, r) * 1.5
            e3_sec += step_lat * 8 * 2  # 8 tags x 2 fields
        experiments.append(("E3: Per-technique progression (steps.sh 8 tags x 2 fields)", e3_sec))
        total_seconds += e3_sec

        # E4/E5: Slice sweeps (9 slice counts x 5 sizes: 20..28 x 2 thread counts)
        slice_sizes = [20, 22, 24, 26, 28]
        e5_sec = sum(self.estimate_seconds("this_work_mode_a", th, ln, runs_count_for_logn(ln))
                     for th in [1, 8] for ln in slice_sizes) * 8  # 8 explicit slices beyond adaptive
        experiments.append(("E4/E5: Slicing scaling & sweep (s in 1..256)", e5_sec))
        total_seconds += e5_sec

        # E6: Thread scaling (T in {1, 2, 4, 8} at logN 20, 24, 26, 28)
        e6_tw = sum(self.estimate_seconds("this_work_mode_a", th, ln, runs_count_for_logn(ln))
                    for th in THREAD_SCALING_T for ln in THREAD_SCALING_LOGN)
        e6_orig = sum(self.estimate_seconds("lcpc_orig", th, ln, runs_count_for_logn(ln))
                      for th in THREAD_SCALING_T for ln in THREAD_SCALING_LOGN) * self.lcpc_variants
        experiments.append(("E6: Thread scaling This Work (T=1,2,4,8)", e6_tw))
        experiments.append(("E6: Thread scaling Original LCPC (T=1,2,4,8)", e6_orig))
        total_seconds += e6_tw + e6_orig

        # E7, E8, E9, E10, E11, E12: Microbenchmarks, PCS breakdown, verifier, Orion
        # PCS (Commit/Prove/Verify): ~1.8x encode latency
        pcs_sec = sum(self.estimate_seconds("this_work_mode_a", th, ln, runs_count_for_logn(ln)) * 1.8
                      for th in [1, 8] for ln in [20, 24, 28])
        experiments.append(("E8: End-to-end PCS Commit/Prove/Verify breakdown", pcs_sec))
        total_seconds += pcs_sec

        misc_sec = 180.0  # Field microbenchmarks & verifier encode
        overhead = total_seconds * 0.12  # Compilation, codegen checks, cache warmups
        experiments.append(("E7/E9: Field arithmetic & verifier encode", misc_sec))
        experiments.append(("Builds, codegen disassembly checks, harness setup", overhead))
        total_seconds += misc_sec + overhead

        hours = total_seconds / 3600.0
        cost_per_session = hours * PRICE_PER_HOUR_C3D

        return {
            "profile": "full",
            "machine": "c3d-highmem-16",
            "experiments": experiments,
            "total_seconds": total_seconds,
            "total_hours": hours,
            "cost_per_session": cost_per_session,
            "sessions": 2,  # Plan 06 §3 specifies 2 independent sessions
            "total_campaign_cost": cost_per_session * 2,
        }

    def lcpc_block_seconds(self, threads_list, logn_list, variants: int) -> float:
        """Original-LCPC block wall time from the 2^28 T=1 calibration (linear in N, T^0.85)."""
        return sum(LCPC_BLOCK_S_PER_RUN_AT_28_T1 * 2.0 ** (ln - 28) / th ** 0.85 * 6 * variants
                   for th in threads_list for ln in logn_list)

    def evaluate_remeasure(self) -> List[Tuple[str, float]]:
        """Stages of reproduce/gcp/run_stages.sh --preset remeasure (warm-up 1, 5 runs)."""
        lcpc = self.lcpc_block_seconds
        tw = lambda ths, lns: sum(self.estimate_seconds("this_work_mode_a", th, ln, 6) * 2.0
                                  for th in ths for ln in lns)  # x2: commit/prove/verify
        build_overhead = 180.0  # per block: build, workspace tests or golden checks, codegen check
        main = [16, 18, 20, 22, 24, 26]
        ts = [1, 2, 4, 8]
        return [
            ("RL: LCPC mode (a) T=1,8, 2^16..2^26, 2 variants", lcpc([1, 8], main, 2) + 2 * build_overhead),
            ("RL28: LCPC mode (a) T=1,8, 2^28, 2 variants", lcpc([1, 8], [28], 2) + 2 * build_overhead),
            ("RH: LCPC mode (a) T=1,8, 2^20,2^24, 2 hash/flag variants", lcpc([1, 8], [20, 24], 2) + 2 * build_overhead),
            ("RT: thread scaling T=1,2,4,8 at 2^20,24,26 (This Work + LCPC 2 variants)",
             lcpc(ts, [20, 24, 26], 2) + tw(ts, [20, 24, 26]) + 8 * build_overhead),
            ("RT28A: LCPC thread scaling T=1,2 at 2^28, 2 variants", lcpc([1, 2], [28], 2) + 2 * build_overhead),
            ("RT28B: This Work T=1,2,4,8 + LCPC T=4,8 at 2^28", lcpc([4, 8], [28], 2) + tw(ts, [28]) + 6 * build_overhead),
            ("RS: SIMD-Spielman Mont127 T=1 2^28, Brakedown shape (graph generation not modelled)",
             self.estimate_seconds("simd_spielman", 1, 28, 6) + build_overhead),
            ("ARM (c4a): This Work + LCPC T=1,8 at 2^18,22,26 (x86 rates; Arm not calibrated)",
             lcpc([1, 8], [18, 22, 26], 2) + tw([1, 8], [18, 22, 26]) + 4 * build_overhead),
        ]

    def print_report(self, out=sys.stdout):
        arm_res = self.evaluate_profile_arm()
        full_res = self.evaluate_profile_full()

        out.write("# RealBrakedownPerf Campaign Workload and Cost Estimate\n\n")
        out.write(f"**Data Basis:** {self.data_source}\n")
        if any(self.samples_count.values()):
            sample_str = ", ".join(f"{k}: {v} pts" for k, v in self.samples_count.items())
            out.write(f"**Fitted Samples:** {sample_str}\n")
        out.write("\n")

        # Table 1: Measured / Fitted Scaling Rates
        out.write("### Fitted Work Scaling Rates (ns per coefficient element)\n\n")
        out.write("| Target Implementation | Single-Thread (T=1) | Multithreaded (T=8) | Basis |\n")
        out.write("| :--- | :---: | :---: | :--- |\n")
        for impl, name in [
            ("this_work_mode_a", "This Work (mode a, SdigCode6)"),
            ("this_work_mode_b", "This Work (mode b, SIMD-Spielman)"),
            ("lcpc_orig", "Original LCPC (Rayon baseline)"),
            ("simd_spielman", "SIMD-Spielman (AVX-512 IFMA)"),
        ]:
            r1 = self.get_rate(impl, 1)
            r8 = self.get_rate(impl, 8)
            status = "Measured Smoke Fit" if impl in self.fitted_rates else "Reference Model"
            out.write(f"| {name} | {r1:.2f} ns | {r8:.2f} ns | {status} |\n")
        out.write("\n---\n\n")

        # Table 2: Breakdown for Profile 'full'
        out.write("### Profile: `full` (x86-64 Primary Campaign: `c3d-highmem-16`)\n\n")
        out.write(f"Machine: AMD EPYC 9004 (Zen 4), 8 physical cores (--threads-per-core=1), 128 GB RAM "
                  f"(${PRICE_PER_HOUR_C3D:.2f}/hr, scaled estimate)\n")
        out.write(f"Original-LCPC build variants per block: {self.lcpc_variants}\n\n")
        out.write("| Experiment / Workload Block | Estimated Wall-Clock Time |\n")
        out.write("| :--- | :---: |\n")
        for desc, sec in full_res["experiments"]:
            if sec >= 3600:
                time_str = f"{sec / 3600.0:.2f} hrs"
            elif sec >= 60:
                time_str = f"{sec / 60.0:.1f} min"
            else:
                time_str = f"{sec:.1f} s"
            out.write(f"| {desc} | {time_str} |\n")
        out.write("| :--- | :---: |\n")
        out.write(f"| **Estimated Time per Session** | **{full_res['total_hours']:.2f} hours** |\n")
        out.write(f"| **Estimated Cost per Session** | **${full_res['cost_per_session']:.2f}** |\n")
        out.write(f"| **Full Campaign (2 sessions, Plan 06 §3)** | **{full_res['total_hours'] * 2:.2f} hrs (${full_res['total_campaign_cost']:.2f})** |\n\n")

        # Table 3: Breakdown for Profile 'arm'
        out.write("### Profile: `arm` (Arm Portability Campaign: `c4a-highmem-16`)\n\n")
        out.write(f"Machine: Google Axion (Arm Neoverse V2), 16 cores, 128 GB RAM (${PRICE_PER_HOUR_C4A:.2f}/hr)\n\n")
        out.write("| Experiment / Workload Block | Estimated Wall-Clock Time |\n")
        out.write("| :--- | :---: |\n")
        for desc, sec in arm_res["experiments"]:
            if sec >= 3600:
                time_str = f"{sec / 3600.0:.2f} hrs"
            elif sec >= 60:
                time_str = f"{sec / 60.0:.1f} min"
            else:
                time_str = f"{sec:.1f} s"
            out.write(f"| {desc} | {time_str} |\n")
        out.write("| :--- | :---: |\n")
        out.write(f"| **Estimated Time per Session** | **{arm_res['total_hours']:.2f} hours** |\n")
        out.write(f"| **Estimated Cost per Session** | **${arm_res['cost_per_session']:.2f}** |\n")
        out.write(f"| **Arm Campaign Total (1 session, Plan 06 §4)** | **{arm_res['total_hours']:.2f} hrs (${arm_res['total_campaign_cost']:.2f})** |\n\n")

        # Table 4: audit re-measurement stages
        out.write("### Preset: `remeasure` (reproduce/gcp/run_stages.sh, per session)\n\n")
        out.write("Original-LCPC times are calibrated from the measured 2^28 T=1 stage-B block (see "
                  "LCPC_BLOCK_S_PER_RUN_AT_28_T1); each stage should stay below about 2 h.\n\n")
        out.write("| Stage | Estimated Wall-Clock Time |\n")
        out.write("| :--- | :---: |\n")
        for desc, sec in self.evaluate_remeasure():
            out.write(f"| {desc} | {sec / 60.0:.0f} min |\n")
        out.write("\n")

        # Summary
        total_campaign_cost = full_res["total_campaign_cost"] + arm_res["total_campaign_cost"]
        total_campaign_hours = (full_res["total_hours"] * 2) + arm_res["total_hours"]
        out.write("### Campaign Summary\n\n")
        out.write(f"- **Total GCP VM Time:** {total_campaign_hours:.2f} machine hours\n")
        out.write(f"- **Total Estimated Cost:** ${total_campaign_cost:.2f} USD\n")
        out.write("- **Cost-Minimization Compliance:** `run_session.sh` stops VMs immediately upon session completion.\n")


def main():
    parser = argparse.ArgumentParser(description="Estimate RealBrakedownPerf benchmark campaign duration and GCP costs.")
    parser.add_argument("--results-dir", type=str, default="results", help="Directory containing smoke run JSON results to fit scaling.")
    parser.add_argument("--output", type=str, default=None, help="Output file to write Markdown report.")
    parser.add_argument("--lcpc-variants", type=int, default=4,
                        help="Original-LCPC build variants measured per block (default 4, as all.sh without "
                             "--lcpc-variants; run_stages.sh measures 2)")
    parser.add_argument("--price-c3d", type=float, default=None,
                        help="c3d-highmem-16 USD/hour (default: scaled estimate)")
    args = parser.parse_args()

    if args.price_c3d is not None:
        global PRICE_PER_HOUR_C3D
        PRICE_PER_HOUR_C3D = args.price_c3d
    estimator = CampaignEstimator(args.results_dir, args.lcpc_variants)
    if args.output:
        os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
        with open(args.output, "w", encoding="utf-8") as f:
            estimator.print_report(f)
        print(f"[INFO] Campaign estimate report written to {args.output}")
    else:
        estimator.print_report(sys.stdout)


if __name__ == "__main__":
    main()
