#!/usr/bin/env python3
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

"""
analyze.py - Aggregates raw benchmark JSON results from:
  - This Work (portable scalar Rust fork)
  - Original LCPC baseline (f0e2116)
  - SIMD-Spielman AVX-512 baseline (7b399db)
  - Per-step tagged commit history (step/01 .. HEAD)

Generates:
  - Detailed CSV summaries
  - LaTeX tables for paper/
  - Publication-ready plots (if matplotlib is installed)

All numbers are derived directly from measurement output. No hand-entered values.
"""

import argparse
import csv
import glob
import json
import math
import os
import re
import sys
from collections import defaultdict
from typing import Any, Dict, List, Optional, Tuple

try:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    HAS_MATPLOTLIB = True
except ImportError:
    HAS_MATPLOTLIB = False


def safe_float(val: Any) -> Optional[float]:
    try:
        return float(val)
    except (ValueError, TypeError):
        return None


def format_ns_to_ms(ns: float) -> str:
    return f"{ns / 1_000_000.0:.2f}"


def format_ns_to_s(ns: float) -> str:
    return f"{ns / 1_000_000_000.0:.3f}"


def format_bytes_to_mb(b: int) -> str:
    return f"{b / (1024.0 * 1024.0):.1f}"


class BenchmarkAnalyzer:
    def __init__(self, out_dir: str):
        self.out_dir = out_dir
        os.makedirs(out_dir, exist_ok=True)
        self.csv_dir = os.path.join(out_dir, "csv")
        self.latex_dir = os.path.join(out_dir, "latex")
        self.plots_dir = os.path.join(out_dir, "plots")
        os.makedirs(self.csv_dir, exist_ok=True)
        os.makedirs(self.latex_dir, exist_ok=True)
        os.makedirs(self.plots_dir, exist_ok=True)

        self.reports_this_work: List[Dict] = []
        self.reports_lcpc_orig: List[Dict] = []
        self.reports_simd_spielman: List[Dict] = []
        self.reports_steps: Dict[str, List[Dict]] = defaultdict(list)
        self.field_benchmarks: List[Dict] = []

    def load_results(self, root_paths: List[str]):
        """Recursively discovers and categorizes JSON result files."""
        for root in root_paths:
            for dirpath, _, filenames in os.walk(root):
                for fname in filenames:
                    if not fname.endswith(".json"):
                        continue
                    if "manifest" in fname or "codegen" in fname:
                        continue
                    path = os.path.join(dirpath, fname)
                    try:
                        with open(path, "r", encoding="utf-8") as f:
                            data = json.load(f)
                    except Exception:
                        continue

                    # Handle field microbenchmarks list
                    if isinstance(data, list) and len(data) > 0 and isinstance(data[0], dict) and "mul" in data[0] and "mac" in data[0]:
                        self.field_benchmarks.extend(data)
                        continue

                    # Handle lcpc-bench-v1 lists
                    if isinstance(data, list):
                        for item in data:
                            if isinstance(item, dict) and item.get("schema_version") == "lcpc-bench-v1":
                                self._categorize_report(path, item)
                    elif isinstance(data, dict):
                        if data.get("schema_version") == "lcpc-bench-v1":
                            self._categorize_report(path, data)
                        # SIMD-Spielman raw summary files
                        elif "benchmark" in data or "effective_slice_counts_by_benchmark" in data or "simd_spielman" in path:
                            self.reports_simd_spielman.append(data)

        print(f"[INFO] Loaded: {len(self.reports_this_work)} This Work cases, "
              f"{len(self.reports_lcpc_orig)} Original LCPC cases, "
              f"{len(self.reports_simd_spielman)} SIMD-Spielman entries, "
              f"{len(self.reports_steps)} Step groups, "
              f"{len(self.field_benchmarks)} Field benchmarks.")

    def _categorize_report(self, path: str, item: Dict):
        norm_path = path.replace("\\", "/")
        if "/steps/" in norm_path or "step_" in norm_path:
            # Extract step tag
            match = re.search(r"/steps/([^/]+)/", norm_path)
            step_tag = match.group(1) if match else "unknown_step"
            self.reports_steps[step_tag].append(item)
        elif "/lcpc_orig/" in norm_path or item.get("implementation") == "lcpc-original-upstream":
            self.reports_lcpc_orig.append(item)
        elif "/this_work/" in norm_path or item.get("implementation") == "lcpc-fork":
            self.reports_this_work.append(item)

    def generate_all(self):
        """Generates all CSV tables, LaTeX documents, and visualizations."""
        self.generate_csv_encoding()
        self.generate_csv_pcs()
        self.generate_csv_steps()
        self.generate_csv_fields()
        self.generate_latex_encoding()
        self.generate_latex_pcs()
        self.generate_latex_steps()
        self.generate_latex_fields()
        if HAS_MATPLOTLIB:
            self.generate_plots()
        else:
            print("[INFO] Matplotlib not found; skipping plot generation.")

    def generate_csv_encoding(self):
        path = os.path.join(self.csv_dir, "prover_encoding_comparison.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            writer.writerow([
                "Implementation", "Category", "Field", "Params", "Threads", "logN", "N",
                "Rows_k", "Cols_n", "Time_Median_ms", "Time_IQR_ms", "Time_Mean_ms",
                "Throughput_elem_per_s", "Peak_RSS_MB"
            ])
            for r in self.reports_this_work:
                meas = r.get("measurements", {}).get("encode_matrix", {}).get("summary", {})
                dim = r.get("dimensions", {})
                med_ns = meas.get("median_ns", 0.0)
                iqr_ns = meas.get("q3_ns", 0) - meas.get("q1_ns", 0)
                mean_ns = meas.get("mean_ns", 0.0)
                n_elem = dim.get("coefficient_count", 0)
                tput = (n_elem / (med_ns / 1e9)) if med_ns > 0 else 0.0
                rss_mb = r.get("peak_rss_bytes", 0) / (1024.0 * 1024.0)

                writer.writerow([
                    "This Work", "Portable Scalar Rust", r.get("field"), r.get("params"),
                    r.get("threads"), r.get("log_n"), n_elem, dim.get("n_rows"), dim.get("n_cols"),
                    f"{med_ns / 1e6:.3f}", f"{iqr_ns / 1e6:.3f}", f"{mean_ns / 1e6:.3f}",
                    f"{tput:.1f}", f"{rss_mb:.2f}"
                ])

            for r in self.reports_lcpc_orig:
                meas = r.get("measurements", {}).get("encode_matrix", {}).get("summary", {})
                dim = r.get("dimensions", {})
                med_ns = meas.get("median_ns", 0.0)
                iqr_ns = meas.get("q3_ns", 0) - meas.get("q1_ns", 0)
                mean_ns = meas.get("mean_ns", 0.0)
                n_elem = dim.get("coefficient_count", 0)
                tput = (n_elem / (med_ns / 1e9)) if med_ns > 0 else 0.0
                rss_mb = r.get("peak_rss_bytes", 0) / (1024.0 * 1024.0)

                writer.writerow([
                    "Original LCPC", "Rayon Upstream Baseline", r.get("field"), r.get("params"),
                    r.get("threads"), r.get("log_n"), n_elem, dim.get("n_rows"), dim.get("n_cols"),
                    f"{med_ns / 1e6:.3f}", f"{iqr_ns / 1e6:.3f}", f"{mean_ns / 1e6:.3f}",
                    f"{tput:.1f}", f"{rss_mb:.2f}"
                ])
        print(f"[INFO] Wrote CSV: {path}")

    def generate_csv_pcs(self):
        path = os.path.join(self.csv_dir, "pcs_breakdown.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            writer.writerow([
                "Implementation", "Field", "Params", "Threads", "logN",
                "Commit_Total_ms", "Encode_Matrix_ms", "Hash_Matrix_ms",
                "Encode_Fraction_Pct", "Hash_Fraction_Pct", "Prove_ms", "Verify_ms"
            ])
            for group, label in [(self.reports_this_work, "This Work"), (self.reports_lcpc_orig, "Original LCPC")]:
                for r in group:
                    m = r.get("measurements", {})
                    enc_ms = m.get("encode_matrix", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                    hash_ms = m.get("hash_matrix_decomposition", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                    comm_ms = m.get("commit_total", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                    prv_ms = m.get("prove", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                    ver_ms = m.get("verify", {}).get("summary", {}).get("median_ns", 0.0) / 1e6

                    denom = (enc_ms + hash_ms) if (enc_ms + hash_ms) > 0 else 1.0
                    enc_pct = (enc_ms / denom) * 100.0
                    hash_pct = (hash_ms / denom) * 100.0

                    writer.writerow([
                        label, r.get("field"), r.get("params"), r.get("threads"), r.get("log_n"),
                        f"{comm_ms:.3f}", f"{enc_ms:.3f}", f"{hash_ms:.3f}",
                        f"{enc_pct:.1f}", f"{hash_pct:.1f}", f"{prv_ms:.3f}", f"{ver_ms:.3f}"
                    ])
        print(f"[INFO] Wrote CSV: {path}")

    def generate_csv_steps(self):
        path = os.path.join(self.csv_dir, "steps_ablation.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            writer.writerow([
                "Step_Tag", "Order_Index", "Field", "Params", "Threads", "logN",
                "Encode_Matrix_ms", "Commit_Total_ms"
            ])
            sorted_tags = sorted(self.reports_steps.keys())
            for idx, tag in enumerate(sorted_tags, start=1):
                for r in self.reports_steps[tag]:
                    m = r.get("measurements", {})
                    enc_ms = m.get("encode_matrix", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                    comm_ms = m.get("commit_total", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                    writer.writerow([
                        tag, idx, r.get("field"), r.get("params"), r.get("threads"), r.get("log_n"),
                        f"{enc_ms:.3f}", f"{comm_ms:.3f}"
                    ])
        print(f"[INFO] Wrote CSV: {path}")

    def generate_csv_fields(self):
        path = os.path.join(self.csv_dir, "field_microbenchmarks.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            writer.writerow(["Field", "Mul_ns_per_op", "Mac_ns_per_op", "Reduce_ns_per_op"])
            for fb in self.field_benchmarks:
                field = fb.get("field", "Unknown")
                mul_ns = fb.get("mul", {}).get("ns_per_op", 0.0)
                mac_ns = fb.get("mac", {}).get("ns_per_op", 0.0)
                red_ns = fb.get("reduce", {}).get("ns_per_op", 0.0)
                writer.writerow([field, f"{mul_ns:.3f}", f"{mac_ns:.3f}", f"{red_ns:.3f}"])
        print(f"[INFO] Wrote CSV: {path}")

    def generate_latex_encoding(self):
        path = os.path.join(self.latex_dir, "table_encoding_comparison.tex")
        with open(path, "w", encoding="utf-8") as f:
            f.write("% Auto-generated by analyze.py. Do not edit by hand.\n")
            f.write("\\begin{table*}[t]\n")
            f.write("\\centering\n")
            f.write("\\caption{Prover Encoding Latency and Throughput Comparison (Brakedown Square Shape $k = \\sqrt{N}$)}\n")
            f.write("\\label{tab:encoding_comparison}\n")
            f.write("\\begin{tabular}{lccccrrr}\n")
            f.write("\\toprule\n")
            f.write("Implementation & Field & Threads & $\\log_2 N$ & $k$ & Median (ms) & IQR (ms) & Throughput (Melem/s) \\\\\n")
            f.write("\\midrule\n")

            # This Work
            for r in sorted(self.reports_this_work, key=lambda x: (x.get("threads", 1), x.get("log_n", 0))):
                meas = r.get("measurements", {}).get("encode_matrix", {}).get("summary", {})
                dim = r.get("dimensions", {})
                med_ms = meas.get("median_ns", 0.0) / 1e6
                iqr_ms = (meas.get("q3_ns", 0) - meas.get("q1_ns", 0)) / 1e6
                n_elem = dim.get("coefficient_count", 0)
                tput = (n_elem / (meas.get("median_ns", 1.0) / 1e9)) / 1e6
                f.write(f"This Work (Scalar) & {r.get('field')} & {r.get('threads')} & {r.get('log_n')} & {dim.get('n_rows')} & "
                        f"{med_ms:.2f} & {iqr_ms:.2f} & {tput:.2f} \\\\\n")

            f.write("\\midrule\n")
            # Original LCPC
            for r in sorted(self.reports_lcpc_orig, key=lambda x: (x.get("threads", 1), x.get("log_n", 0))):
                meas = r.get("measurements", {}).get("encode_matrix", {}).get("summary", {})
                dim = r.get("dimensions", {})
                med_ms = meas.get("median_ns", 0.0) / 1e6
                iqr_ms = (meas.get("q3_ns", 0) - meas.get("q1_ns", 0)) / 1e6
                n_elem = dim.get("coefficient_count", 0)
                tput = (n_elem / (meas.get("median_ns", 1.0) / 1e9)) / 1e6
                f.write(f"Original LCPC & {r.get('field')} & {r.get('threads')} & {r.get('log_n')} & {dim.get('n_rows')} & "
                        f"{med_ms:.2f} & {iqr_ms:.2f} & {tput:.2f} \\\\\n")

            f.write("\\bottomrule\n")
            f.write("\\end{tabular}\n")
            f.write("\\end{table*}\n")
        print(f"[INFO] Wrote LaTeX: {path}")

    def generate_latex_pcs(self):
        path = os.path.join(self.latex_dir, "table_pcs_breakdown.tex")
        with open(path, "w", encoding="utf-8") as f:
            f.write("% Auto-generated by analyze.py. Do not edit by hand.\n")
            f.write("\\begin{table*}[t]\n")
            f.write("\\centering\n")
            f.write("\\caption{End-to-End PCS Breakdown: Encoding vs. Portable BLAKE3 Hashing}\n")
            f.write("\\label{tab:pcs_breakdown}\n")
            f.write("\\begin{tabular}{lcccccc}\n")
            f.write("\\toprule\n")
            f.write("Implementation & $\\log_2 N$ & Commit (ms) & Encode (ms) & Hash (ms) & Prove (ms) & Verify (ms) \\\\\n")
            f.write("\\midrule\n")
            for r in sorted(self.reports_this_work, key=lambda x: x.get("log_n", 0)):
                m = r.get("measurements", {})
                enc_ms = m.get("encode_matrix", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                hash_ms = m.get("hash_matrix_decomposition", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                comm_ms = m.get("commit_total", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                prv_ms = m.get("prove", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                ver_ms = m.get("verify", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                f.write(f"This Work & {r.get('log_n')} & {comm_ms:.2f} & {enc_ms:.2f} & {hash_ms:.2f} & {prv_ms:.2f} & {ver_ms:.2f} \\\\\n")
            f.write("\\bottomrule\n")
            f.write("\\end{tabular}\n")
            f.write("\\end{table*}\n")
        print(f"[INFO] Wrote LaTeX: {path}")

    def generate_latex_steps(self):
        path = os.path.join(self.latex_dir, "table_steps_ablation.tex")
        with open(path, "w", encoding="utf-8") as f:
            f.write("% Auto-generated by analyze.py. Do not edit by hand.\n")
            f.write("\\begin{table}[t]\n")
            f.write("\\centering\n")
            f.write("\\caption{Cumulative Optimization Technique Breakdown Across Tagged Commits}\n")
            f.write("\\label{tab:steps_ablation}\n")
            f.write("\\begin{tabular}{llrr}\n")
            f.write("\\toprule\n")
            f.write("Step Tag & Added Technique & Encode Latency (ms) & Speedup vs Prev \\\\\n")
            f.write("\\midrule\n")
            prev_ms = None
            for tag in sorted(self.reports_steps.keys()):
                reports = self.reports_steps[tag]
                if not reports:
                    continue
                r = reports[0]
                m = r.get("measurements", {})
                enc_ms = m.get("encode_matrix", {}).get("summary", {}).get("median_ns", 0.0) / 1e6
                speedup_str = "-" if prev_ms is None else f"{prev_ms / enc_ms:.2f}\\times"
                clean_tag = tag.replace("_", "\\_")
                f.write(f"{clean_tag} & Cumulative & {enc_ms:.2f} & {speedup_str} \\\\\n")
                prev_ms = enc_ms
            f.write("\\bottomrule\n")
            f.write("\\end{tabular}\n")
            f.write("\\end{table}\n")
        print(f"[INFO] Wrote LaTeX: {path}")

    def generate_latex_fields(self):
        path = os.path.join(self.latex_dir, "table_field_microbenchmarks.tex")
        with open(path, "w", encoding="utf-8") as f:
            f.write("% Auto-generated by analyze.py. Do not edit by hand.\n")
            f.write("\\begin{table}[t]\n")
            f.write("\\centering\n")
            f.write("\\caption{Field Arithmetic Microbenchmarks (ns/op)}\n")
            f.write("\\label{tab:field_microbenchmarks}\n")
            f.write("\\begin{tabular}{lccc}\n")
            f.write("\\toprule\n")
            f.write("Field & Multiplication (ns) & MAC (ns) & Reduction (ns) \\\\\n")
            f.write("\\midrule\n")
            for fb in self.field_benchmarks:
                field = fb.get("field", "Unknown")
                mul_ns = fb.get("mul", {}).get("ns_per_op", 0.0)
                mac_ns = fb.get("mac", {}).get("ns_per_op", 0.0)
                red_ns = fb.get("reduce", {}).get("ns_per_op", 0.0)
                f.write(f"{field} & {mul_ns:.2f} & {mac_ns:.2f} & {red_ns:.2f} \\\\\n")
            f.write("\\bottomrule\n")
            f.write("\\end{tabular}\n")
            f.write("\\end{table}\n")
        print(f"[INFO] Wrote LaTeX: {path}")

    def generate_plots(self):
        """Generates PNG and PDF plots."""
        # 1. Prover encoding time vs logN
        if self.reports_this_work:
            fig, ax = plt.subplots(figsize=(7, 4.5))
            # Group by implementation
            x_vals = [r.get("log_n") for r in self.reports_this_work]
            y_vals = [r.get("measurements", {}).get("encode_matrix", {}).get("summary", {}).get("median_ns", 0.0) / 1e6 for r in self.reports_this_work]
            ax.plot(x_vals, y_vals, marker="o", linewidth=2, label="This Work (Scalar Rust)")

            if self.reports_lcpc_orig:
                x_orig = [r.get("log_n") for r in self.reports_lcpc_orig]
                y_orig = [r.get("measurements", {}).get("encode_matrix", {}).get("summary", {}).get("median_ns", 0.0) / 1e6 for r in self.reports_lcpc_orig]
                ax.plot(x_orig, y_orig, marker="s", linestyle="--", linewidth=2, label="Original LCPC")

            ax.set_xlabel(r"Problem Size ($\log_2 N$)")
            ax.set_ylabel("Encoding Latency (ms)")
            ax.set_yscale("log")
            ax.set_title("Prover Encoding Latency vs. Matrix Size")
            ax.grid(True, which="both", linestyle=":", alpha=0.6)
            ax.legend()
            fig.tight_layout()

            png_path = os.path.join(self.plots_dir, "encoding_latency_vs_logn.png")
            pdf_path = os.path.join(self.plots_dir, "encoding_latency_vs_logn.pdf")
            fig.savefig(png_path, dpi=300)
            fig.savefig(pdf_path)
            plt.close(fig)
            print(f"[INFO] Wrote Plot: {png_path} and {pdf_path}")

        # 2. PCS Commit Breakdown (Stacked Bar)
        if self.reports_this_work:
            fig, ax = plt.subplots(figsize=(7, 4.5))
            labels = [f"N=2^{r.get('log_n')}" for r in self.reports_this_work]
            enc_times = [r.get("measurements", {}).get("encode_matrix", {}).get("summary", {}).get("median_ns", 0.0) / 1e6 for r in self.reports_this_work]
            hash_times = [r.get("measurements", {}).get("hash_matrix_decomposition", {}).get("summary", {}).get("median_ns", 0.0) / 1e6 for r in self.reports_this_work]

            ax.bar(labels, enc_times, label="Encoding (Batched)", color="#1f77b4")
            ax.bar(labels, hash_times, bottom=enc_times, label="Hashing (Portable BLAKE3)", color="#ff7f0e")
            ax.set_ylabel("Latency (ms)")
            ax.set_title("PCS Commit Latency Breakdown: Encoding vs. BLAKE3 Hashing")
            ax.legend()
            fig.tight_layout()

            png_path = os.path.join(self.plots_dir, "pcs_breakdown.png")
            pdf_path = os.path.join(self.plots_dir, "pcs_breakdown.pdf")
            fig.savefig(png_path, dpi=300)
            fig.savefig(pdf_path)
            plt.close(fig)
            print(f"[INFO] Wrote Plot: {png_path} and {pdf_path}")


def main():
    parser = argparse.ArgumentParser(description="Analyze benchmark JSON results.")
    parser.add_argument("--results-dir", nargs="+", required=True, help="One or more directories containing benchmark JSON outputs")
    parser.add_argument("--out-dir", default="./analysis", help="Output directory for CSVs, LaTeX tables, and plots")
    args = parser.parse_args()

    analyzer = BenchmarkAnalyzer(args.out_dir)
    analyzer.load_results(args.results_dir)
    analyzer.generate_all()
    print(f"[INFO] Analysis completed successfully. Artifacts saved in {args.out_dir}")


if __name__ == "__main__":
    main()
