#!/usr/bin/env python3
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

"""
run_benchmarks.py - Orchestrates timed benchmark runs, validates core pinning
(rejecting CPU 2 on the local host), records environment manifests, and outputs raw JSON.
"""

import argparse
import datetime
import glob
import hashlib
import json
import os
import platform
import subprocess
import sys
from typing import Dict, List, Optional, Set

DEFAULT_LOCAL_CORES = "0,1,3-7"
RESERVED_CORE = 2


def is_local_i7_host() -> bool:
    """Checks whether the current system is the local Intel i7-11700K workstation."""
    if os.path.exists("/proc/cpuinfo"):
        try:
            with open("/proc/cpuinfo", "r", encoding="utf-8", errors="replace") as f:
                content = f.read()
                return "i7-11700K" in content or "11th Gen Intel(R) Core(TM) i7-11700K" in content
        except Exception:
            pass
    return False


def parse_core_list(cores_str: str) -> Set[int]:
    """Expands a core specification string (e.g. '0,1,3-7') into a set of integers."""
    cores = set()
    for part in cores_str.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            try:
                lo_str, hi_str = part.split("-", 1)
                lo, hi = int(lo_str), int(hi_str)
                cores.update(range(lo, hi + 1))
            except ValueError:
                raise ValueError(f"Invalid core range: '{part}'")
        else:
            try:
                cores.add(int(part))
            except ValueError:
                raise ValueError(f"Invalid core integer: '{part}'")
    return cores


def validate_cores(cores_str: str) -> None:
    """Enforces host-specific core restrictions."""
    parsed = parse_core_list(cores_str)
    if is_local_i7_host():
        if RESERVED_CORE in parsed:
            sys.stderr.write(
                f"[ERROR] CPU {RESERVED_CORE} is reserved on this host and MUST NOT be used!\n"
                f"Requested cores: {cores_str} (parsed: {sorted(parsed)})\n"
            )
            sys.exit(1)


def get_environment_manifest(code_dir: str, cores: str, threads: int, field: str, params: str, logn: str) -> Dict:
    """Constructs environment manifest dictionary."""
    def safe_read(p):
        try:
            with open(p, "r", encoding="utf-8", errors="replace") as f:
                return f.read().strip()
        except Exception:
            return None

    def run_cmd(cmd):
        try:
            r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
            return r.stdout.strip()
        except Exception:
            return None

    cpu_info = {}
    if os.path.exists("/proc/cpuinfo"):
        with open("/proc/cpuinfo", "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                if ":" in line:
                    k, v = line.split(":", 1)
                    k, v = k.strip(), v.strip()
                    if k == "model name" and "model_name" not in cpu_info:
                        cpu_info["model_name"] = v
                    elif k == "microcode" and "microcode" not in cpu_info:
                        cpu_info["microcode"] = v

    git_sha = run_cmd(["git", "-C", code_dir, "rev-parse", "HEAD"])
    rustc_ver = run_cmd(["rustc", "--version"])

    return {
        "manifest_schema_version": "1.0.0",
        "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "is_local_test_host": is_local_i7_host(),
        "cpu": cpu_info,
        "git_sha": git_sha,
        "rustc_version": rustc_ver,
        "pinning": {
            "cores": cores,
            "threads": threads,
            "parsed_cores": sorted(parse_core_list(cores)),
        },
        "parameters": {
            "field": field,
            "params": params,
            "logN": [int(x.strip()) for x in logn.split(",") if x.strip().isdigit()],
        }
    }


def main():
    parser = argparse.ArgumentParser(description="LCPC Benchmark Runner")
    parser.add_argument("--field", default="mont127", choices=["mont127", "mers127", "fp2m61", "ft127", "ft63", "all"])
    parser.add_argument("--params", default="lcpc-sdig6", choices=["lcpc-sdig6", "lcpc-sdig3", "simd-spielman"])
    parser.add_argument("--logN", default="16,18,20,22,24,26", help="Comma-separated log2N values")
    parser.add_argument("--threads", type=int, default=1)
    parser.add_argument("--cores", default="", help="Cores to pin via taskset (default: 0,1,3-7 on local)")
    parser.add_argument("--slices", default="adaptive", help="LLC slice count: adaptive | N")
    parser.add_argument("--autotune", action="store_true", help="Auto-tune LLC slice count empirically")
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--output", default="", help="Output path for benchmark JSON")
    parser.add_argument("--manifest", default="", help="Output path for manifest JSON")
    parser.add_argument("--target-dir", default="", help="Cargo target directory")
    parser.add_argument("--bin", default="lcpc-bench", choices=["lcpc-bench", "bench_encode", "bench_pcs", "bench_fields"])
    parser.add_argument("--mode", default="all", choices=["all", "encode", "pcs", "verifier", "fields"])
    args = parser.parse_args()

    # Determine code dir
    script_dir = os.path.dirname(os.path.abspath(__file__))
    code_dir = os.path.dirname(script_dir)

    # Core assignment and validation
    cores = args.cores
    if not cores:
        if is_local_i7_host():
            cores = "0" if args.threads == 1 else DEFAULT_LOCAL_CORES
        else:
            cores = "0"
    validate_cores(cores)

    # Resolve target directory and binary
    target_dir = args.target_dir or os.environ.get("CARGO_TARGET_DIR") or os.path.join(code_dir, "target-harness")
    bin_path = os.path.join(target_dir, "release", args.bin)

    if not os.path.exists(bin_path):
        print(f"[INFO] Building {args.bin} in {target_dir}...")
        cmd_build = [
            "taskset", "-c", cores,
            "cargo", "build", "--release", "--manifest-path", os.path.join(code_dir, "Cargo.toml"),
            "--bin", args.bin
        ]
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = target_dir
        subprocess.run(cmd_build, env=env, check=True)

    # Prepare benchmark execution command
    cmd = ["taskset", "-c", cores, bin_path]
    if args.bin == "bench_fields":
        cmd.extend(["--field", args.field, "--runs", str(args.runs * 10000)])
    else:
        cmd.extend([
            "--field", args.field,
            "--params", args.params,
            "--logN", args.logN,
            "--threads", str(args.threads),
            "--cores", cores,
            "--slices", args.slices,
            "--runs", str(args.runs),
            "--warmup", str(args.warmup),
            "--mode", args.mode,
        ])
        if args.autotune:
            cmd.append("--autotune")

    if args.output:
        cmd.extend(["--output", args.output])

    print(f"[CMD] {' '.join(cmd)}")
    env = os.environ.copy()
    env["RAYON_NUM_THREADS"] = str(args.threads)

    res = subprocess.run(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if res.returncode != 0:
        sys.stderr.write(f"[ERROR] Benchmark failed with exit code {res.returncode}:\n{res.stderr}\n")
        sys.exit(res.returncode)

    if not args.output:
        print(res.stdout)

    if args.manifest:
        manifest_data = get_environment_manifest(code_dir, cores, args.threads, args.field, args.params, args.logN)
        os.makedirs(os.path.dirname(os.path.abspath(args.manifest)), exist_ok=True)
        with open(args.manifest, "w", encoding="utf-8") as f:
            json.dump(manifest_data, f, indent=2)
        print(f"[INFO] Manifest written to {args.manifest}")


if __name__ == "__main__":
    main()
