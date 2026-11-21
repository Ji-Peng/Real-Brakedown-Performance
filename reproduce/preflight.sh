#!/bin/sh
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

# Checks host capabilities, tools, memory, disk, core topology, and git tags,
# evaluates every claim in reproduce/claims.tsv, writes preflight.json, and
# prints a summary table. Exits 2 when C0 (correctness tests) cannot run.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

if [ -f "${CARGO_HOME:-$HOME/.cargo}/env" ]; then
    # shellcheck disable=SC1091
    . "${CARGO_HOME:-$HOME/.cargo}/env"
fi

MODE="quick"
CLAIMS="all"
OUT="out"
CORES=""
MAX_THREADS="8"
HOST_LABEL=""
JSON_OUT=""
DEPS_DIR="${RBD_DEPS_DIR:-$CODE_DIR/deps}"
QUIET=0

usage() {
    cat <<EOF
Usage: $0 [--mode quick|full] [--claims all|C0,...] [--out DIR]
          [--cores LIST] [--max-threads N] [--host-label NAME]
          [--deps DIR] [--json-out FILE] [--quiet]
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --mode) [ $# -ge 2 ] || { echo "[ERROR] --mode requires an argument" >&2; exit 2; }; MODE="$2"; shift 2 ;;
        --claims) [ $# -ge 2 ] || { echo "[ERROR] --claims requires an argument" >&2; exit 2; }; CLAIMS="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || { echo "[ERROR] --out requires an argument" >&2; exit 2; }; OUT="$2"; shift 2 ;;
        --cores) [ $# -ge 2 ] || { echo "[ERROR] --cores requires an argument" >&2; exit 2; }; CORES="$2"; shift 2 ;;
        --max-threads) [ $# -ge 2 ] || { echo "[ERROR] --max-threads requires an argument" >&2; exit 2; }; MAX_THREADS="$2"; shift 2 ;;
        --host-label|--host) [ $# -ge 2 ] || { echo "[ERROR] $1 requires an argument" >&2; exit 2; }; HOST_LABEL="$2"; shift 2 ;;
        --deps) [ $# -ge 2 ] || { echo "[ERROR] --deps requires an argument" >&2; exit 2; }; DEPS_DIR="$2"; shift 2 ;;
        --json-out) [ $# -ge 2 ] || { echo "[ERROR] --json-out requires an argument" >&2; exit 2; }; JSON_OUT="$2"; shift 2 ;;
        --quiet) QUIET=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "[ERROR] Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$MODE" in
    quick|full) ;;
    *) echo "[ERROR] --mode must be quick or full (got '$MODE')" >&2; exit 2 ;;
esac

if [ -f "$DEPS_DIR/env.sh" ]; then
    # shellcheck disable=SC1090
    . "$DEPS_DIR/env.sh"
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[ERROR] python3 is required for preflight and analysis (C0 cannot run)" >&2
    exit 2
fi

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac

if [ -z "$HOST_LABEL" ]; then
    HOST_LABEL=$(hostname -s 2>/dev/null || echo unknown-host)
    HOST_LABEL=$(printf '%s' "$HOST_LABEL" | tr -c 'A-Za-z0-9._-' '_')
fi

if [ -z "$JSON_OUT" ]; then
    JSON_OUT="$OUT_ROOT/$HOST_LABEL/preflight.json"
fi

PRE_CODE_DIR="$CODE_DIR" \
PRE_MODE="$MODE" \
PRE_CLAIMS="$CLAIMS" \
PRE_OUT_ROOT="$OUT_ROOT" \
PRE_CORES="$CORES" \
PRE_MAX_THREADS="$MAX_THREADS" \
PRE_HOST_LABEL="$HOST_LABEL" \
PRE_JSON_OUT="$JSON_OUT" \
PRE_DEPS_DIR="$DEPS_DIR" \
PRE_QUIET="$QUIET" \
python3 - <<'PY'
import datetime
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys

code_dir = os.environ["PRE_CODE_DIR"]
mode = os.environ["PRE_MODE"]
claims_arg = os.environ["PRE_CLAIMS"]
out_root = os.environ["PRE_OUT_ROOT"]
cores_override = os.environ["PRE_CORES"] or None
max_threads = int(os.environ["PRE_MAX_THREADS"])
host_label = os.environ["PRE_HOST_LABEL"]
json_out = os.environ["PRE_JSON_OUT"]
deps_dir = os.environ["PRE_DEPS_DIR"]
quiet = os.environ["PRE_QUIET"] == "1"

sys.path.insert(0, os.path.join(code_dir, "reproduce", "lib"))
import cores as cores_lib  # noqa: E402


def run_cmd(cmd, timeout=15):
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=True)
        return res.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None


def read_text(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


arch = platform.machine()
cpuinfo = read_text("/proc/cpuinfo")
avx512ifma = bool(re.search(r"\bavx512ifma\b", cpuinfo))
lscpu = cores_lib.lscpu_values()
cpu_model = lscpu.get("Model name")
if not cpu_model:
    m = re.search(r"^model name\s*:\s*(.+)$", cpuinfo, re.MULTILINE)
    cpu_model = m.group(1).strip() if m else "unknown"

selection, single, bench, usable, l3_domain, cache_aware = cores_lib.select_cores(
    max_threads=max_threads, explicit_cores=cores_override
)
phys_cores = selection["usable_physical_cores"]
bench_count = len(bench)
mt_threads = selection["mt_threads"]

# Memory: /proc/meminfo + cgroup memory limit if present.
meminfo = {}
for line in read_text("/proc/meminfo").splitlines():
    if ":" in line:
        k, v = line.split(":", 1)
        m = re.search(r"(\d+)", v)
        if m:
            meminfo[k.strip()] = int(m.group(1))
mem_total_kb = meminfo.get("MemTotal", 0)
mem_avail_kb = meminfo.get("MemAvailable", mem_total_kb)

cgroup_limit = None
for p in ("/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes"):
    raw = read_text(p).strip()
    if raw.isdigit():
        val = int(raw)
        if 0 < val < (1 << 60):
            cgroup_limit = val
            break
eff_mem_bytes = mem_avail_kb * 1024
if cgroup_limit is not None:
    eff_mem_bytes = min(eff_mem_bytes, cgroup_limit)
# 2^28 stages peak at ~34 GiB RSS; 50 GiB available/limit safely covers nominal 64 GB hosts.
ram_64g_ok = eff_mem_bytes >= 50 * (1024 ** 3)


def free_bytes(path):
    p = pathlib.Path(path)
    while not p.exists() and p != p.parent:
        p = p.parent
    try:
        return shutil.disk_usage(str(p)).free
    except OSError:
        return None


target_dir = os.environ.get("CARGO_TARGET_DIR") or os.path.join(code_dir, "target-harness")
disk_out_free = free_bytes(out_root)
disk_target_free = free_bytes(target_dir)

# Pinned Rust toolchain from rust-toolchain.toml
tc_text = read_text(os.path.join(code_dir, "rust-toolchain.toml"))
m_tc = re.search(r'channel\s*=\s*"([^"]+)"', tc_text)
pinned_toolchain = m_tc.group(1) if m_tc else "1.99.1"

cargo_path = shutil.which("cargo")
rustc_path = shutil.which("rustc")
rustup_path = shutil.which("rustup")
rustc_ver = run_cmd(["rustc", "--version"]) if rustc_path else None
rustup_toolchains = run_cmd(["rustup", "toolchain", "list"]) if rustup_path else ""
toolchain_installed = bool(
    (rustc_ver and pinned_toolchain in rustc_ver)
    or (rustup_toolchains and pinned_toolchain in rustup_toolchains)
)
rust_ok = bool(cargo_path and (toolchain_installed or rustup_path))

try:
    import matplotlib
    matplotlib_ver = matplotlib.__version__
except Exception:
    matplotlib_ver = None

gxx_path = shutil.which("g++")
cmake_path = shutil.which("cmake")
taskset_path = shutil.which("taskset")
git_path = shutil.which("git")
objdump_path = shutil.which("objdump")
latexmk_path = shutil.which("latexmk")
pdflatex_path = shutil.which("pdflatex")

# Boost headers check (system or SIMD_SPIELMAN_EXTRA_INCLUDE or deps/boost_1_86_0).
boost_inc_dirs = [p for p in (os.environ.get("SIMD_SPIELMAN_EXTRA_INCLUDE") or "").split(":") if p]
deps_boost = os.path.join(deps_dir, "boost_1_86_0")
if os.path.isdir(deps_boost) and deps_boost not in boost_inc_dirs:
    boost_inc_dirs.append(deps_boost)
boost_found = False
boost_path = None
for inc in boost_inc_dirs + ["/usr/include", "/usr/local/include"]:
    cand = os.path.join(inc, "boost", "multiprecision", "cpp_int.hpp")
    if os.path.isfile(cand):
        boost_found = True
        boost_path = cand
        break
# On x86_64 with g++ and network (not offline), fetch_deps.sh can download Boost headers.
offline = os.environ.get("RBD_OFFLINE") == "1" or os.path.isfile(os.path.join(deps_dir, ".offline"))
boost_fetchable = bool(gxx_path and not offline and shutil.which("curl") and shutil.which("tar"))

# Git step/* tags check
step_tags = []
if git_path and os.path.exists(os.path.join(code_dir, ".git")):
    out_tags = run_cmd(["git", "-C", code_dir, "tag", "--list", "step/*"]) or ""
    step_tags = [t for t in out_tags.splitlines() if t and t != "step/00-import"]
has_git_tags = len(step_tags) >= 9

# Deps presence
tv_env = os.environ.get("SIMD_SPIELMAN_TV_DIR") or ""
tv_in_deps = os.path.isfile(os.path.join(deps_dir, "SIMD-Spielman-for-ZKPs", "tv", "bd_spielman_extension_N12_in.txt"))
tv_in_env = bool(tv_env and os.path.isfile(os.path.join(tv_env, "bd_spielman_extension_N12_in.txt")))
poly_in_deps = os.path.isdir(os.path.join(deps_dir, "poly-commit", ".git"))
deps_present = tv_in_deps or tv_in_env
network_or_deps_ok = deps_present or (not offline)

tools = {
    "cargo": cargo_path,
    "rustc": rustc_path,
    "rustc_version": rustc_ver,
    "rustup": rustup_path,
    "pinned_toolchain": pinned_toolchain,
    "toolchain_installed": toolchain_installed,
    "python3": sys.executable,
    "python3_version": platform.python_version(),
    "python3_ge_3_11": sys.version_info >= (3, 11),
    "matplotlib": matplotlib_ver,
    "taskset": taskset_path,
    "git": git_path,
    "g++": gxx_path,
    "cmake": cmake_path,
    "boost_headers": boost_path,
    "boost_available_or_fetchable": boost_found or boost_fetchable,
    "objdump": objdump_path,
    "latexmk": latexmk_path,
    "pdflatex": pdflatex_path,
}

# Load claims.tsv
claims_path = os.path.join(code_dir, "reproduce", "claims.tsv")
claims_list = []
with open(claims_path, "r", encoding="utf-8") as f:
    header = None
    for line in f:
        line = line.rstrip("\n")
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if header is None:
            header = parts
            continue
        claims_list.append(dict(zip(header, parts)))


def check_req(req):
    if not req or req == "-":
        return True, None
    # Handle mode qualifier like cores>=8(full)
    m = re.match(r"^(.+)\((full|quick)\)$", req)
    if m:
        req_base, req_mode = m.group(1), m.group(2)
        if mode != req_mode:
            return True, None
        req = req_base
    if req == "x86_64":
        return (arch == "x86_64"), "needs x86_64"
    if req == "aarch64":
        return (arch in ("aarch64", "arm64")), "needs aarch64"
    if req == "avx512ifma":
        return avx512ifma, "needs avx512ifma"
    if req == "git-tags":
        return has_git_tags, "needs git-tags"
    if req == "cores>=8":
        return (bench_count >= 8), "fewer than 8 cores"
    if req == "ram>=64G":
        return ram_64g_ok, "insufficient RAM"
    if req == "boost":
        return (boost_found or boost_fetchable), "needs boost"
    if req == "cmake":
        return bool(cmake_path and gxx_path), "needs cmake"
    if req == "network-or-deps":
        return network_or_deps_ok, "needs network-or-deps"
    return True, None


c0_blockers = []
if not rust_ok:
    c0_blockers.append("missing cargo/rustup")
if not taskset_path:
    c0_blockers.append("missing taskset")

claim_results = []
for row in claims_list:
    cid = row["id"]
    reasons = []
    if cid == "C0" and c0_blockers:
        reasons.extend(c0_blockers)
    for req in [r.strip() for r in row["requires"].split(",") if r.strip() and r.strip() != "-"]:
        ok, reason = check_req(req)
        if not ok and reason not in reasons:
            reasons.append(reason)
    stages = row["x86_stages"] if arch == "x86_64" else row["arm_stages"]
    if stages == "-" and not reasons:
        reasons.append("needs aarch64" if arch == "x86_64" else "needs x86_64")
    claim_results.append({
        "id": cid,
        "title": row["title"],
        "paper_refs": row["paper_refs"],
        "tables": row["tables"],
        "x86_stages": row["x86_stages"],
        "arm_stages": row["arm_stages"],
        "requires": row["requires"],
        "quick_note": row["quick_note"],
        "status": "runnable" if not reasons else "skipped",
        "reasons": reasons,
        "reason": "; ".join(reasons) if reasons else "",
    })

dropped_stages = {}
if mode == "full" and arch == "x86_64" and not ram_64g_ok:
    dropped_stages["B"] = "ram"
    dropped_stages["T28"] = "ram"

report = {
    "schema_version": "rbd-preflight-v1",
    "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "host_label": host_label,
    "hostname": cores_lib.hostname_short(),
    "mode": mode,
    "claims_requested": claims_arg,
    "arch": arch,
    "cpu_model": cpu_model,
    "avx512ifma": avx512ifma,
    "cores": {
        "single": single,
        "multi": ",".join(map(str, bench)),
        "multi_list": bench,
        "allowed": cores_lib.format_cpu_list(usable),
        "l3_domain": l3_domain,
        "cache_aware": cache_aware,
        "usable_physical_cores": phys_cores,
        "benchmark_cores_count": bench_count,
        "mt_threads": mt_threads,
        "l3_domains_on_node": selection["l3_domains_on_node"],
    },
    "memory": {
        "mem_total_kb": mem_total_kb,
        "mem_available_kb": mem_avail_kb,
        "cgroup_limit_bytes": cgroup_limit,
        "effective_available_bytes": eff_mem_bytes,
        "ram_64g_ok": ram_64g_ok,
    },
    "disk": {
        "out_root": out_root,
        "out_free_bytes": disk_out_free,
        "cargo_target_dir": target_dir,
        "cargo_target_free_bytes": disk_target_free,
    },
    "tools": tools,
    "git_tags": {
        "has_step_tags": has_git_tags,
        "count": len(step_tags),
        "tags": step_tags,
    },
    "deps": {
        "deps_dir": deps_dir,
        "offline": offline,
        "tv_in_deps": tv_in_deps,
        "tv_in_env": tv_in_env,
        "polycommit_in_deps": poly_in_deps,
        "boost_found": boost_found,
        "boost_fetchable": boost_fetchable,
        "network_or_deps_ok": network_or_deps_ok,
    },
    "dropped_stages": dropped_stages,
    "claims": claim_results,
    "c0_runnable": not c0_blockers,
}

if json_out and json_out != "-":
    p = pathlib.Path(json_out)
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(p.suffix + ".tmp")
    tmp.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, p)

if not quiet:
    mem_gb = eff_mem_bytes / (1024 ** 3)
    print(f"[PREFLIGHT] host={host_label} arch={arch} cpu='{cpu_model}' avx512ifma={avx512ifma}")
    print(f"[PREFLIGHT] cores: single={single} multi={','.join(map(str, bench))} "
          f"mt_threads={mt_threads} l3_domain={l3_domain} cache_aware={cache_aware}")
    print(f"[PREFLIGHT] memory: effective_available={mem_gb:.1f} GiB (ram>=64G: {ram_64g_ok}); "
          f"python={platform.python_version()} matplotlib={matplotlib_ver or 'missing'} "
          f"tex={'yes' if (latexmk_path or pdflatex_path) else 'no'}")
    if not tools["python3_ge_3_11"]:
        print("[WARN] Python < 3.11 detected; tomllib fallback will be used where needed (3.11+ recommended).")
    if dropped_stages:
        print("[PREFLIGHT] Dropped 2^28 stages due to RAM (< 64 GB): " + ", ".join(sorted(dropped_stages)))
    print("")
    print(f"{'Claim':<6} {'Status':<10} {'Reason':<26} {'Tables':<28} Title")
    print("-" * 110)
    for c in claim_results:
        print(f"{c['id']:<6} {c['status']:<10} {c['reason'] or '-':<26} {c['tables']:<28} {c['title']}")

if c0_blockers:
    print(f"[ERROR] C0 cannot run: {'; '.join(c0_blockers)}", file=sys.stderr)
    sys.exit(2)
PY
