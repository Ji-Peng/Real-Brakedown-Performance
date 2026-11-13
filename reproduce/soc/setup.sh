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

# Prepares one node class of the SoC cluster for the benchmark campaign and
# writes the node's first-run record. Run it from the cluster clone
# (<root>/code, see sync.sh) on the login node: outside a Slurm job it
# re-runs itself through srun on a node of the requested class, because the
# login node must not build or compute. Inside a job it
#   1. installs rustup and the toolchain pinned in rust-toolchain.toml into
#      <root>/toolchain/<arch>/ (the home directory is shared by x86-64 and
#      aarch64 nodes, so ~/.cargo cannot hold both);
#   2. checks out the pinned SIMD-Spielman upstream commit next to code/
#      (the fork's tests read its test vectors);
#   3. on xcnh, if the system has no Boost headers, unpacks the pinned Boost
#      release into <root>/deps/ and records it in <root>/deps/simd-spielman.env
#      (SIMD_SPIELMAN_EXTRA_INCLUDE, read by reproduce/simd_spielman.sh);
#   4. writes results/soc-<class>/first-run-<hostname>.json (node, toolchain,
#      compilers, checks) and exits 1 if a check fails.
# --record-only skips 1-3; reproduce/soc/job.sh uses it at the start of every
# session so that each node used carries its own record.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ROOT=$(CDPATH= cd -- "$CODE_DIR/.." && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib.sh"

BOOST_VERSION=1.86.0
BOOST_DIR_NAME=boost_1_86_0
BOOST_URL=https://archives.boost.io/release/1.86.0/source/boost_1_86_0.tar.bz2
BOOST_SHA256=1bed88e40401b2cb7a1f76d4bab499e352fa4d0c5f31c0dbae64e24d34d7513b

NODE_CLASS=
PARTITION=normal
RECORD_ONLY=0

usage() {
    cat <<EOF
Usage: $0 --node-class xcnh|xcng [--partition P] [--record-only]

  --node-class C   node class to prepare (xcnh: x86-64 AMD EPYC; xcng: Arm Neoverse-N1)
  --partition P    partition for the srun that this script submits from the
                   login node (default: normal)
  --record-only    only write the first-run record of the current node
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --node-class) [ "$#" -ge 2 ] || soc_die "--node-class needs a value"; NODE_CLASS=$2; shift 2 ;;
        --partition) [ "$#" -ge 2 ] || soc_die "--partition needs a value"; PARTITION=$2; shift 2 ;;
        --record-only) RECORD_ONLY=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
EXPECTED_ARCH=$(soc_class_arch "$NODE_CLASS") || { usage >&2; exit 2; }

if [ -z "${SLURM_JOB_ID:-}" ]; then
    command -v srun >/dev/null 2>&1 || soc_die "not inside a Slurm job and no srun on this host"
    _extra=
    [ "$RECORD_ONLY" -eq 0 ] || _extra=--record-only
    soc_log "submitting setup to a $NODE_CLASS node (partition $PARTITION)"
    exec srun -p "$PARTITION" --constraint="$NODE_CLASS" -n1 -c 16 --threads-per-core=1 \
        --mem=32G -t 01:00:00 --cpu-freq=Performance -J "rbd-setup-$NODE_CLASS" \
        sh "$0" --node-class "$NODE_CLASS" $_extra </dev/null
fi

[ "$(uname -m)" = "$EXPECTED_ARCH" ] || soc_die "node $(hostname -s) is $(uname -m), not $EXPECTED_ARCH ($NODE_CLASS)"

TOOLCHAIN=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$CODE_DIR/rust-toolchain.toml")
[ -n "$TOOLCHAIN" ] || soc_die "no channel in $CODE_DIR/rust-toolchain.toml"
soc_toolchain_env "$ROOT"
UPSTREAM_DIR="$ROOT/SIMD-Spielman-for-ZKPs"
BOOST_ROOT_DIR="$ROOT/deps/$BOOST_DIR_NAME"

boost_usable() {
    printf '#include <boost/multiprecision/cpp_int.hpp>\nint main() {}\n' | \
        g++ -x c++ -fsyntax-only ${1:+-I"$1"} - >/dev/null 2>&1
}

if [ "$RECORD_ONLY" -eq 0 ]; then
    mkdir -p "$ROOT/deps" "$ROOT/jobs" "$(dirname -- "$RUSTUP_HOME")"

    # 1. Rust toolchain.
    if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
        soc_log "installing rustup into $CARGO_HOME"
        _init="$ROOT/deps/rustup-init-$EXPECTED_ARCH.sh"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$_init"
        sh "$_init" -y --no-modify-path --profile minimal --default-toolchain none
        rm -f "$_init"
    fi
    rustup toolchain install "$TOOLCHAIN" --profile minimal --component rustfmt --component clippy

    # 2. SIMD-Spielman upstream at the pinned commit.
    if [ ! -d "$UPSTREAM_DIR/.git" ]; then
        git clone --quiet "$RBD_UPSTREAM_URL" "$UPSTREAM_DIR"
    fi
    git -C "$UPSTREAM_DIR" checkout --quiet --force --detach "$RBD_UPSTREAM_COMMIT"

    # 3. Boost headers for the SIMD-Spielman baseline (x86-64 only).
    if [ "$NODE_CLASS" = xcnh ] && ! boost_usable; then
        if [ ! -f "$BOOST_ROOT_DIR/boost/multiprecision/cpp_int.hpp" ]; then
            _archive="$ROOT/deps/$BOOST_DIR_NAME.tar.bz2"
            if [ ! -f "$_archive" ] || ! printf '%s  %s\n' "$BOOST_SHA256" "$_archive" | sha256sum -c - >/dev/null 2>&1; then
                soc_log "downloading Boost $BOOST_VERSION headers"
                curl -fsSL -o "$_archive" "$BOOST_URL"
            fi
            printf '%s  %s\n' "$BOOST_SHA256" "$_archive" | sha256sum -c - >/dev/null || \
                soc_die "Boost archive checksum mismatch: $_archive"
            tar -xjf "$_archive" -C "$ROOT/deps" "$BOOST_DIR_NAME/boost" "$BOOST_DIR_NAME/LICENSE_1_0.txt"
        fi
        boost_usable "$BOOST_ROOT_DIR" || soc_die "Boost headers in $BOOST_ROOT_DIR are not usable by g++"
        {
            printf '# Written by reproduce/soc/setup.sh: Boost %s headers (sha256 %s of %s)\n' \
                "$BOOST_VERSION" "$BOOST_SHA256" "$BOOST_URL"
            printf 'SIMD_SPIELMAN_EXTRA_INCLUDE=%s\n' "$BOOST_ROOT_DIR"
            printf 'export SIMD_SPIELMAN_EXTRA_INCLUDE\n'
        } > "$ROOT/deps/simd-spielman.env"
        soc_toolchain_env "$ROOT"
    fi
fi

# 4. First-run record of this node.
RECORD_DIR="$CODE_DIR/results/soc-$NODE_CLASS"
RECORD="$RECORD_DIR/first-run-$(hostname -s).json"
mkdir -p "$RECORD_DIR"
RECORD="$RECORD" NODE_CLASS="$NODE_CLASS" EXPECTED_ARCH="$EXPECTED_ARCH" TOOLCHAIN="$TOOLCHAIN" \
UPSTREAM_DIR="$UPSTREAM_DIR" UPSTREAM_COMMIT="$RBD_UPSTREAM_COMMIT" BOOST_VERSION="$BOOST_VERSION" \
BOOST_SHA256="$BOOST_SHA256" BOOST_URL="$BOOST_URL" CODE_DIR="$CODE_DIR" \
python3 - "$SCRIPT_DIR/host_profile.py" <<'PY'
import datetime, json, os, platform, re, shutil, subprocess, sys

sys.path.insert(0, os.path.dirname(sys.argv[1]))
import host_profile as hp  # noqa: E402

env = os.environ

def run(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, check=True, timeout=60).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None

def first_line(cmd):
    out = run(cmd)
    return out.splitlines()[0] if out else None

lscpu = hp.lscpu_values()
online = hp.parse_cpu_list(hp.read_text("/sys/devices/system/cpu/online"))
usable = sorted(os.sched_getaffinity(0))
l3 = {}
for cpu in sorted(online):
    for index in range(8):
        base = f"/sys/devices/system/cpu/cpu{cpu}/cache/index{index}"
        if hp.read_text(f"{base}/level") == "3":
            shared = hp.read_text(f"{base}/shared_cpu_list")
            l3.setdefault(shared, hp.read_text(f"{base}/size"))
caches_cpu0 = []
for index in range(8):
    base = f"/sys/devices/system/cpu/cpu{usable[0]}/cache/index{index}"
    level = hp.read_text(f"{base}/level")
    if level:
        caches_cpu0.append({"level": level, "type": hp.read_text(f"{base}/type"),
                            "size": hp.read_text(f"{base}/size"),
                            "shared_cpu_list": hp.read_text(f"{base}/shared_cpu_list")})

rustc = run(["rustc", "-vV"]) or ""
cargo = run(["cargo", "-vV"]) or ""
boost_header = None
for inc in [p for p in (env.get("SIMD_SPIELMAN_EXTRA_INCLUDE") or "").split(":") if p] + ["/usr/include", "/usr/local/include"]:
    path = os.path.join(inc, "boost", "version.hpp")
    if os.path.exists(path):
        boost_header = path
        break
boost_lib_version = None
if boost_header:
    m = re.search(r'#define BOOST_LIB_VERSION "([^"]+)"', open(boost_header, encoding="utf-8", errors="replace").read())
    boost_lib_version = m.group(1) if m else None
upstream_head = run(["git", "-C", env["UPSTREAM_DIR"], "rev-parse", "HEAD"])
tv_ok = os.path.exists(os.path.join(env["UPSTREAM_DIR"], "tv", "bd_spielman_extension_N12_in.txt"))
try:
    import matplotlib  # noqa: F401
    matplotlib_version = matplotlib.__version__
except Exception:
    matplotlib_version = None

model = lscpu.get("Model name") or ""
checks = {
    "architecture": platform.machine() == env["EXPECTED_ARCH"],
    "pinned_rust_toolchain": f"release: {env['TOOLCHAIN']}" in rustc,
    "python_has_tomllib": sys.version_info >= (3, 11),
    "upstream_commit_and_test_vectors": upstream_head == env["UPSTREAM_COMMIT"] and tv_ok,
    "objdump_available": shutil.which("objdump") is not None,
    "eight_usable_physical_cores": len(usable) >= 8,
}
if env["NODE_CLASS"] == "xcnh":
    checks["amd_epyc_cpu"] = "AMD EPYC" in model
    checks["avx512ifma_present"] = "avx512ifma" in (hp.read_text("/proc/cpuinfo") or "")
    checks["l3_topology_available"] = bool(l3)
    checks["boost_headers"] = boost_header is not None
else:
    checks["neoverse_n1_cpu"] = "Neoverse-N1" in model

record = {
    "schema_version": "soc-first-run-v1",
    "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "node_class": env["NODE_CLASS"],
    "hostname": hp.hostname(),
    "slurm_job_id": env.get("SLURM_JOB_ID"),
    "slurm_partition": env.get("SLURM_JOB_PARTITION"),
    "code_commit": run(["git", "-C", env["CODE_DIR"], "rev-parse", "HEAD"]),
    "lscpu": run(["lscpu"]),
    "cpu": {
        "model_name": model,
        "sockets": lscpu.get("Socket(s)"),
        "cores_per_socket": lscpu.get("Core(s) per socket"),
        "threads_per_core": lscpu.get("Thread(s) per core"),
        "online_cpus": hp.read_text("/sys/devices/system/cpu/online"),
        "job_usable_cpus": hp.format_cpu_list(usable),
    },
    "caches_of_first_usable_cpu": caches_cpu0,
    "l3_domains": [{"shared_cpu_list": k, "size": v} for k, v in l3.items()],
    "frequency": {
        "governor_first_usable_cpu": hp.read_text(f"/sys/devices/system/cpu/cpu{usable[0]}/cpufreq/scaling_governor"),
        "scaling_driver": hp.read_text(f"/sys/devices/system/cpu/cpu{usable[0]}/cpufreq/scaling_driver"),
        "boost_raw": hp.read_text("/sys/devices/system/cpu/cpufreq/boost"),
    },
    "perf": hp.perf_record(),
    "memory_total": next((l for l in (hp.read_text("/proc/meminfo") or "").splitlines() if l.startswith("MemTotal")), None),
    "os": {
        "release": hp.read_text("/etc/os-release"),
        "kernel": platform.release(),
        "glibc": platform.libc_ver()[1],
        "python": sys.version.split()[0],
        "matplotlib": matplotlib_version,
    },
    "rust_toolchain": {"pinned_channel": env["TOOLCHAIN"], "rustc_verbose": rustc, "cargo_verbose": cargo,
                       "rustup_home": env.get("RUSTUP_HOME"), "cargo_home": env.get("CARGO_HOME")},
    "compilers": {
        "gxx": first_line(["g++", "--version"]),
        "clangxx": first_line(["clang++", "--version"]),
        "cmake": first_line(["cmake", "--version"]),
        "objdump": first_line(["objdump", "--version"]),
    },
    "simd_spielman": {
        "upstream_dir": env["UPSTREAM_DIR"],
        "upstream_head": upstream_head,
        "test_vectors_present": tv_ok,
        "boost_header": boost_header,
        "boost_lib_version": boost_lib_version,
        "boost_archive": {"url": env["BOOST_URL"], "sha256": env["BOOST_SHA256"]}
        if env.get("SIMD_SPIELMAN_EXTRA_INCLUDE") else None,
        "extra_include_override": env.get("SIMD_SPIELMAN_EXTRA_INCLUDE"),
    },
    "checks": checks,
    "status": "passed" if all(checks.values()) else "failed",
}
with open(env["RECORD"], "w", encoding="utf-8") as f:
    json.dump(record, f, indent=2)
    f.write("\n")
print(json.dumps({"record": env["RECORD"], "status": record["status"], "checks": checks}, indent=2))
sys.exit(0 if record["status"] == "passed" else 1)
PY
soc_log "first-run record: $RECORD"
