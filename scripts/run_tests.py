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
run_tests.py - Runs cargo test for the workspace under taskset.
- Default core list: 0,1,3-7 (never uses CPU 2 on the local host).
- Refuses any core list containing CPU 2 when running on the local host.
- Supports an optional portability build without target-cpu=native via a separate
  CARGO_TARGET_DIR and RUSTFLAGS override.
"""

import argparse
import os
import shutil
import subprocess
import sys
from typing import List, Set

DEFAULT_LOCAL_CORES = "0,1,3-7"
RESERVED_CORE = 2


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


def is_local_i7_host() -> bool:
    """Checks whether the current system is the local Intel i7-11700K workstation."""
    if os.path.exists("/proc/cpuinfo"):
        try:
            with open("/proc/cpuinfo", "r", encoding="utf-8", errors="replace") as f:
                content = f.read()
                if "i7-11700K" in content:
                    return True
        except Exception:
            pass
    return False


def get_cargo_bin() -> str:
    """Finds cargo binary, checking PATH and ~/.cargo/bin."""
    cargo = shutil.which("cargo")
    if cargo:
        return cargo
    home_cargo = os.path.expanduser("~/.cargo/bin/cargo")
    if os.path.isfile(home_cargo) and os.access(home_cargo, os.X_OK):
        return home_cargo
    return "cargo"


def run_command_under_taskset(
    cmd: List[str],
    cores_str: str,
    env: dict,
    cwd: str,
) -> int:
    """Runs command pinned to cores_str using taskset."""
    taskset_bin = shutil.which("taskset")
    if taskset_bin:
        full_cmd = [taskset_bin, "-c", cores_str] + cmd
    else:
        print("[WARN] taskset not found in PATH; running unpinned.", file=sys.stderr)
        full_cmd = cmd

    print(f"[CMD] {' '.join(full_cmd)}")
    sys.stdout.flush()
    res = subprocess.run(full_cmd, env=env, cwd=cwd)
    return res.returncode


def main():
    parser = argparse.ArgumentParser(
        description="Run cargo test under taskset with core safety and optional portability build."
    )
    parser.add_argument(
        "--cores",
        default=DEFAULT_LOCAL_CORES,
        help=f"Comma-separated core list or ranges for taskset (default: '{DEFAULT_LOCAL_CORES}')",
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="Run tests in release mode (--release)",
    )
    parser.add_argument(
        "--target-dir",
        help="Specify CARGO_TARGET_DIR (default: respects env or target-test)",
    )
    parser.add_argument(
        "--portability",
        action="store_true",
        help="Run portability build without target-cpu=native using a separate target dir",
    )
    parser.add_argument(
        "--portability-only",
        action="store_true",
        help="Run ONLY the portability build/test",
    )
    parser.add_argument(
        "--check-only",
        action="store_true",
        help="Run 'cargo check' instead of 'cargo test'",
    )
    parser.add_argument(
        "cargo_args",
        nargs="*",
        help="Additional arguments passed directly to cargo (e.g. package names, test filters, etc.)",
    )

    args = parser.parse_args()

    # 1. Core verification
    try:
        cores_set = parse_core_list(args.cores)
    except ValueError as e:
        print(f"[ERROR] {e}", file=sys.stderr)
        sys.exit(1)

    if is_local_i7_host():
        if RESERVED_CORE in cores_set:
            print(
                f"[ERROR] Local workstation constraint violated: CPU {RESERVED_CORE} is reserved for another task and MUST NEVER be used!\n"
                f"        Requested core list: '{args.cores}' (expanded: {sorted(cores_set)})",
                file=sys.stderr,
            )
            sys.exit(1)

    cargo_bin = get_cargo_bin()
    script_dir = os.path.dirname(os.path.abspath(__file__))
    code_dir = os.path.dirname(script_dir)

    # Base environment
    base_env = os.environ.copy()
    # Ensure cargo is in PATH
    cargo_dir = os.path.dirname(cargo_bin)
    if cargo_dir and cargo_dir not in base_env.get("PATH", "").split(os.pathsep):
        base_env["PATH"] = f"{cargo_dir}{os.pathsep}{base_env.get('PATH', '')}"

    cargo_subcmd = "check" if args.check_only else "test"

    # Base cargo arguments
    base_cargo_args = [cargo_bin, cargo_subcmd, "--workspace"]
    if args.release:
        base_cargo_args.append("--release")
    if args.cargo_args:
        base_cargo_args.extend(args.cargo_args)

    overall_exit = 0

    # 2. Main build / test run (unless --portability-only)
    if not args.portability_only:
        main_env = base_env.copy()
        target_dir = args.target_dir or os.environ.get("CARGO_TARGET_DIR")
        if target_dir:
            main_env["CARGO_TARGET_DIR"] = target_dir

        print(f"[INFO] Running main workspace tests on cores {args.cores}...")
        rc = run_command_under_taskset(base_cargo_args, args.cores, main_env, code_dir)
        if rc != 0:
            print(f"[ERROR] Main test run failed with exit code {rc}", file=sys.stderr)
            overall_exit = rc

    # 3. Portability build check (if requested)
    if args.portability or args.portability_only:
        port_env = base_env.copy()
        # Separate target directory for portability build
        port_target = os.path.join(code_dir, "target-portability")
        port_env["CARGO_TARGET_DIR"] = port_target
        # RUSTFLAGS override: strictly omit -C target-cpu=native
        # Disable loop and SLP auto-vectorization across all crates
        port_env["RUSTFLAGS"] = "-C no-vectorize-loops -C no-vectorize-slp"

        print(f"\n[INFO] Running portability build check without target-cpu=native (target: {port_target})...")
        print(f"[INFO] Effective RUSTFLAGS: '{port_env['RUSTFLAGS']}'")
        rc_port = run_command_under_taskset(base_cargo_args, args.cores, port_env, code_dir)
        if rc_port != 0:
            print(f"[ERROR] Portability test run failed with exit code {rc_port}", file=sys.stderr)
            if overall_exit == 0:
                overall_exit = rc_port
        else:
            print("[INFO] Portability build check passed cleanly!")

    sys.exit(overall_exit)


if __name__ == "__main__":
    main()
