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

set -eu

# Pinned commit hashes
LCPC_COMMIT="f0e2116001af5a1bbf39481535f837d6a25772e8"
UPSTREAM_COMMIT="7b399dbfecc406c79349015f1e21accc4def1be7"

# Logging helpers
log_info() {
    printf "[INFO] %s\n" "$*"
}

log_warn() {
    printf "[WARN] %s\n" "$*" >&2
}

log_error() {
    printf "[ERROR] %s\n" "$*" >&2
}

log_cmd() {
    printf "[CMD] %s\n" "$*"
    "$@"
}

# Resolve directory of reproduction scripts
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
CODE_DIR="$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)"

# Pin rustup toolchain from rust-toolchain.toml so out-of-tree cargo calls (e.g. cd /) work
# even when rustup was installed with --default-toolchain none.
if [ -z "${RUSTUP_TOOLCHAIN:-}" ] && [ -f "$CODE_DIR/rust-toolchain.toml" ]; then
    _rbd_tc=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$CODE_DIR/rust-toolchain.toml" | head -n 1)
    if [ -n "$_rbd_tc" ]; then
        RUSTUP_TOOLCHAIN="$_rbd_tc"
        export RUSTUP_TOOLCHAIN
    fi
fi

# Source deps/env.sh whenever present (written by reproduce/fetch_deps.sh)
_COMMON_DEPS_DIR="${RBD_DEPS_DIR:-$CODE_DIR/deps}"
if [ -f "$_COMMON_DEPS_DIR/env.sh" ]; then
    # shellcheck disable=SC1090
    . "$_COMMON_DEPS_DIR/env.sh"
fi

# Default options
THREADS="1"
MAX_THREADS="8"
LOGN="16,18,20,22,24,26"
CORES=""
RUNS="5"
WARMUP="2"
OUT="out"
PARAMS="lcpc-sdig6"
HOST_PROFILE="${RBD_HOST_PROFILE:-}"
HOST_LABEL_EXPLICIT=""
FIELD="mont127"
SLICES="adaptive"
AUTOTUNE=0
EXPLICIT_CORES=0
SHAPE="brakedown"

# Host defaults (populated by external RBD_HOST_PROFILE or reproduce/lib/cores.py)
HOST_NAME=""
HOST_DEFAULT_CORES=""
HOST_DEFAULT_SINGLE_CORE=""
HOST_ALLOWED_CORES=""
HOST_L3_DOMAIN=""
HOST_CACHE_AWARE=""

# Sanitized short hostname used for per-node filenames and default --host-label.
host_instance_name() {
    _instance=$(hostname -s 2>/dev/null || echo unknown-host)
    printf '%s\n' "$_instance" | tr -c 'A-Za-z0-9._\n-' '_'
}

# Compute the key for RBD_TEST_STAMP (commit, dirty flag, rustc version, host, arch).
compute_test_stamp_key() {
    _sha=$(git -C "$CODE_DIR" rev-parse HEAD 2>/dev/null || echo "nogit")
    _dirty="clean"
    if [ "$_sha" != "nogit" ] && [ -n "$(git -C "$CODE_DIR" status --porcelain --untracked-files=no 2>/dev/null || true)" ]; then
        _dirty="dirty"
    fi
    _rustc=$(rustc --version 2>/dev/null || echo "norustc")
    _host=$(host_instance_name)
    _arch=$(uname -m)
    printf '%s|%s|%s|%s|%s' "$_sha" "$_dirty" "$_rustc" "$_host" "$_arch" | sha256sum | awk '{print $1}'
}

# Return 0 iff RBD_TEST_STAMP points to a valid passed stamp matching this checkout and host.
test_stamp_matches() {
    [ -n "${RBD_TEST_STAMP:-}" ] && [ -f "${RBD_TEST_STAMP}" ] || return 1
    _want=$(compute_test_stamp_key)
    STAMP_PATH="$RBD_TEST_STAMP" STAMP_WANT="$_want" python3 - <<'PY'
import json, os, sys
try:
    data = json.load(open(os.environ["STAMP_PATH"], "r", encoding="utf-8"))
except Exception:
    sys.exit(1)
ok = data.get("status") == "passed" and data.get("key") == os.environ["STAMP_WANT"]
sys.exit(0 if ok else 1)
PY
}

# Source external host profile if configured (--host-profile or RBD_HOST_PROFILE).
load_host_profile() {
    _prof="${1:-$HOST_PROFILE}"
    if [ -n "$_prof" ]; then
        if [ -f "$_prof" ]; then
            log_info "Loading external host profile: $_prof"
            RBD_HOST_PROFILE="$_prof"
            export RBD_HOST_PROFILE
            # shellcheck disable=SC1090
            . "$_prof"
            HOST_PROFILE="$_prof"
        else
            log_error "External host profile not found: $_prof"
            exit 1
        fi
    fi
}

# Autodetect cores via reproduce/lib/cores.py when not supplied by an external profile.
autodetect_cores() {
    if [ -n "$HOST_DEFAULT_CORES" ] && [ -n "$HOST_DEFAULT_SINGLE_CORE" ]; then
        return 0
    fi
    _cores_info=$(python3 "$SCRIPT_DIR/lib/cores.py" --max-threads "$MAX_THREADS") || {
        log_error "reproduce/lib/cores.py failed to detect benchmark cores"
        exit 1
    }
    IFS='|' read -r _det_single _det_multi _det_allowed _det_domain _det_cache <<CORES_EOF
$_cores_info
CORES_EOF
    HOST_DEFAULT_SINGLE_CORE="${HOST_DEFAULT_SINGLE_CORE:-$_det_single}"
    HOST_DEFAULT_CORES="${HOST_DEFAULT_CORES:-$_det_multi}"
    HOST_ALLOWED_CORES="${HOST_ALLOWED_CORES:-$_det_allowed}"
    HOST_L3_DOMAIN="${HOST_L3_DOMAIN:-$_det_domain}"
    HOST_CACHE_AWARE="${HOST_CACHE_AWARE:-$_det_cache}"
}

# Print the rustflags that the fork's committed .cargo/config.toml applies on
# this host (space separated). Cargo uses target.<host-triple>.rustflags when
# present and otherwise build.rustflags (the two are not merged).
fork_config_rustflags() {
    FORK_CONFIG="$CODE_DIR/.cargo/config.toml" python3 - <<'PY'
import os, re, subprocess, sys

out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
host = None
for line in out.splitlines():
    if line.startswith("host:"):
        host = line.split(":", 1)[1].strip()

path = os.environ["FORK_CONFIG"]
try:
    import tomllib
    with open(path, "rb") as f:
        config = tomllib.load(f)
    cfg_sections = [k for k in config.get("target", {}) if k.startswith("cfg(")]
    if cfg_sections:
        sys.exit(f"fork .cargo/config.toml has [target.'cfg(...)'] sections {cfg_sections}; "
                 "fork_config_rustflags (reproduce/common.sh) must be extended to evaluate them")
    flags = config.get("target", {}).get(host, {}).get("rustflags")
    if flags is None:
        flags = config.get("build", {}).get("rustflags", [])
    print(" ".join(flags))
except ImportError:
    # Minimal fallback for Python 3.10 without tomllib.
    text = open(path, "r", encoding="utf-8").read()
    if re.search(r"^\s*\[target\.'cfg\(", text, re.MULTILINE):
        sys.exit("fork .cargo/config.toml has [target.'cfg(...)'] sections; Python 3.11+ required")
    sections = {}
    cur = None
    buf = []
    for raw in text.splitlines():
        line = re.sub(r"#.*$", "", raw).strip()
        if not line:
            continue
        m = re.match(r"^\[([^\]]+)\]$", line)
        if m:
            if cur is not None:
                sections[cur] = "\n".join(buf)
            cur = m.group(1).strip()
            buf = []
        else:
            buf.append(line)
    if cur is not None:
        sections[cur] = "\n".join(buf)

    def extract_flags(sec_text):
        m = re.search(r"rustflags\s*=\s*\[([^\]]*)\]", sec_text or "", re.DOTALL)
        if not m:
            return None
        return re.findall(r'"([^"]*)"', m.group(1))

    flags = extract_flags(sections.get(f"target.{host}"))
    if flags is None:
        flags = extract_flags(sections.get("build")) or []
    print(" ".join(flags))
PY
}

# Print the rustflags Cargo actually used for a built binary, read from the
# Cargo fingerprint of that binary (newest one if several exist), as a JSON
# list. Arguments: <target-dir>/<profile-dir> and the binary name.
cargo_fingerprint_rustflags() {
    FP_PROFILE_DIR="$1" FP_BIN="$2" python3 - <<'PY'
import glob, json, os, sys
pattern = os.path.join(os.environ["FP_PROFILE_DIR"], ".fingerprint", "*", "bin-" + os.environ["FP_BIN"] + ".json")
paths = sorted(glob.glob(pattern), key=os.path.getmtime)
if not paths:
    sys.exit(f"no Cargo fingerprint matches {pattern}")
with open(paths[-1], "r", encoding="utf-8") as f:
    print(json.dumps(json.load(f).get("rustflags", [])))
PY
}

# Helper to expand a core list (e.g. "0-3") into space-separated integers
expand_cores() {
    _raw_cores="${1:-}"
    if [ -z "$_raw_cores" ]; then
        echo ""
        return 0
    fi
    if command -v python3 >/dev/null 2>&1; then
        EXPAND_INPUT="$_raw_cores" python3 -c '
import os
raw = os.environ.get("EXPAND_INPUT", "")
cores = set()
for part in raw.split(","):
    part = part.strip()
    if not part:
        continue
    if "-" in part:
        lo, hi = part.split("-", 1)
        cores.update(range(int(lo), int(hi) + 1))
    else:
        cores.add(int(part))
print(" ".join(str(c) for c in sorted(cores)))
'
    else
        _old_ifs="$IFS"
        IFS=','
        _res=""
        for _item in $_raw_cores; do
            case "$_item" in
                *-*)
                    _lo="${_item%%-*}"
                    _hi="${_item##*-}"
                    _curr="$_lo"
                    while [ "$_curr" -le "$_hi" ]; do
                        _res="$_res $_curr"
                        _curr=$((_curr + 1))
                    done
                    ;;
                *)
                    _res="$_res $_item"
                    ;;
            esac
        done
        IFS="$_old_ifs"
        echo "$_res"
    fi
}

usage() {
    cat <<EOF
Usage: $0 [options]

Options:
  --threads <n>       Number of worker threads (default: 1; e.g. 1 or 8)
  --max-threads <n>   Maximum threads for autodetected multi-core set (default: 8)
  --logN <list>       Comma-separated log2(N) sizes (default: 16,18,20,22,24,26)
  --cores <list>      Core list for CPU pinning (e.g. 0-3; autodetected if omitted)
  --runs <n>          Number of timed repetitions (default: 5)
  --warmup <n>        Number of untimed warm-up repetitions (default: 2)
  --out <dir>         Output directory for results and manifests (default: out)
  --params <mode>     Benchmark mode: lcpc-sdig6 | lcpc-sdig3 | simd-spielman | rs-ligero (default: lcpc-sdig6)
  --host-label <name> Host label naming the output directory (default: hostname -s; alias: --host)
  --host-profile <f>  Optional external host profile script (or env RBD_HOST_PROFILE)
  --field <name>      Field: mont127 | mers127 | fp2m61 | ft127 (default: mont127)
  --slices <n>        LLC slice count: adaptive | 1 | 2 | 4 | 8 | 16... (default: adaptive)
  --shape <name>      Matrix shape: brakedown | orion | both (default: brakedown)
  --autotune          Auto-tune LLC slice count empirically
  -h, --help          Show this help message
EOF
}

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --threads)
                [ $# -ge 2 ] || { log_error "Option --threads requires an argument"; exit 1; }
                THREADS="$2"
                shift 2
                ;;
            --max-threads)
                [ $# -ge 2 ] || { log_error "Option --max-threads requires an argument"; exit 1; }
                MAX_THREADS="$2"
                shift 2
                ;;
            --logN)
                [ $# -ge 2 ] || { log_error "Option --logN requires an argument"; exit 1; }
                LOGN="$2"
                shift 2
                ;;
            --cores)
                [ $# -ge 2 ] || { log_error "Option --cores requires an argument"; exit 1; }
                CORES="$2"
                EXPLICIT_CORES=1
                shift 2
                ;;
            --runs)
                [ $# -ge 2 ] || { log_error "Option --runs requires an argument"; exit 1; }
                RUNS="$2"
                shift 2
                ;;
            --warmup)
                [ $# -ge 2 ] || { log_error "Option --warmup requires an argument"; exit 1; }
                WARMUP="$2"
                shift 2
                ;;
            --out)
                [ $# -ge 2 ] || { log_error "Option --out requires an argument"; exit 1; }
                OUT="$2"
                shift 2
                ;;
            --params)
                [ $# -ge 2 ] || { log_error "Option --params requires an argument"; exit 1; }
                case "$2" in
                    lcpc-sdig6|lcpc-sdig3|simd-spielman|rs-ligero) PARAMS="$2" ;;
                    *) log_error "Invalid --params '$2'. Must be lcpc-sdig6, lcpc-sdig3, simd-spielman, or rs-ligero."; exit 1 ;;
                esac
                shift 2
                ;;
            --host-label|--host)
                [ $# -ge 2 ] || { log_error "Option $1 requires an argument"; exit 1; }
                HOST_LABEL_EXPLICIT="$2"
                shift 2
                ;;
            --host-profile)
                [ $# -ge 2 ] || { log_error "Option --host-profile requires an argument"; exit 1; }
                HOST_PROFILE="$2"
                shift 2
                ;;
            --field)
                [ $# -ge 2 ] || { log_error "Option --field requires an argument"; exit 1; }
                FIELD="$2"
                shift 2
                ;;
            --slices)
                [ $# -ge 2 ] || { log_error "Option --slices requires an argument"; exit 1; }
                SLICES="$2"
                shift 2
                ;;
            --shape)
                [ $# -ge 2 ] || { log_error "Option --shape requires an argument"; exit 1; }
                # shellcheck disable=SC2034 # Used by callers sourcing common.sh
                SHAPE="$2"
                shift 2
                ;;
            --autotune)
                # shellcheck disable=SC2034 # Used by callers sourcing common.sh
                AUTOTUNE=1
                shift
                ;;
            --test-only)
                log_warn "Option --test-only is deprecated and ignored."
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                usage
                exit 1
                ;;
        esac
    done

    # Cargo environment flags override the checked-in portable scalar policy.
    if [ -n "${RUSTFLAGS:-}" ] || [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
        log_error "RUSTFLAGS and CARGO_ENCODED_RUSTFLAGS must be unset for reproducible runs"
        exit 1
    fi

    # Warn if writing into the archived results/ directory
    case "$OUT" in
        results|./results|"$CODE_DIR/results")
            log_warn "Writing into archived results/ directory; omit --out to write to out/ instead."
            ;;
    esac

    # Load optional external host profile (RBD_HOST_PROFILE / --host-profile)
    load_host_profile "$HOST_PROFILE"

    # Resolve HOST_NAME: explicit --host-label/--host > profile HOST_NAME > sanitized hostname -s
    if [ -n "$HOST_LABEL_EXPLICIT" ]; then
        HOST_NAME="$HOST_LABEL_EXPLICIT"
    elif [ -z "$HOST_NAME" ]; then
        HOST_NAME=$(host_instance_name)
    fi

    # Autodetect cores via reproduce/lib/cores.py when not set by external profile
    autodetect_cores

    # Resolve default CORES if not explicitly supplied
    if [ "$EXPLICIT_CORES" -eq 0 ]; then
        if [ "$THREADS" -gt 1 ]; then
            CORES="$HOST_DEFAULT_CORES"
        else
            CORES="${HOST_DEFAULT_SINGLE_CORE:-${HOST_DEFAULT_CORES%%,*}}"
        fi
    fi
}

write_manifest() {
    _out_file="${1:-manifest.json}"
    _extra_json="${2:-}"
    _expanded_cores="$(expand_cores "$CORES")"
    _host_identity_json=""
    if command -v host_machine_identity >/dev/null 2>&1; then
        _host_identity_json="$(host_machine_identity 2>/dev/null || true)"
    fi

    MANIFEST_OUT_FILE="$_out_file" \
    MANIFEST_EXTRA_JSON="$_extra_json" \
    MANIFEST_CODE_DIR="$CODE_DIR" \
    MANIFEST_LCPC_COMMIT="$LCPC_COMMIT" \
    MANIFEST_UPSTREAM_COMMIT="$UPSTREAM_COMMIT" \
    MANIFEST_HOST_NAME="$HOST_NAME" \
    MANIFEST_CORES="$CORES" \
    MANIFEST_EXPANDED_CORES="$_expanded_cores" \
    MANIFEST_THREADS="$THREADS" \
    MANIFEST_PARAMS="$PARAMS" \
    MANIFEST_FIELD="$FIELD" \
    MANIFEST_LOGN="$LOGN" \
    MANIFEST_SLICES="$SLICES" \
    MANIFEST_RUNS="$RUNS" \
    MANIFEST_WARMUP="$WARMUP" \
    MANIFEST_HOST_PROFILE="$HOST_PROFILE" \
    MANIFEST_HOST_IDENTITY_JSON="$_host_identity_json" \
    MANIFEST_RBD_CLI="${RBD_CLI:-}" \
    MANIFEST_RBD_MODE="${RBD_MODE:-}" \
    MANIFEST_RBD_MT_THREADS="${RBD_MT_THREADS:-}" \
    MANIFEST_CORE_SELECTION_JSON="${RBD_CORE_SELECTION_JSON:-}" \
    python3 - << 'EOF'
import os, sys, json, subprocess, glob, hashlib, platform, datetime

def safe_read(path):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            return f.read().strip()
    except Exception:
        return None

def run_cmd(cmd):
    try:
        res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
        return res.stdout.strip()
    except Exception:
        return None

code_dir = os.environ.get("MANIFEST_CODE_DIR", ".")
out_file = os.environ.get("MANIFEST_OUT_FILE", "manifest.json")
extra_json = os.environ.get("MANIFEST_EXTRA_JSON", "")
cores = os.environ.get("MANIFEST_CORES", "")
expanded_cores = os.environ.get("MANIFEST_EXPANDED_CORES", "").split()
threads_str = os.environ.get("MANIFEST_THREADS", "1")
threads = int(threads_str) if threads_str.isdigit() else threads_str
runs_str = os.environ.get("MANIFEST_RUNS", "5")
runs = int(runs_str) if runs_str.isdigit() else runs_str
warmup_str = os.environ.get("MANIFEST_WARMUP", "2")
warmup = int(warmup_str) if warmup_str.isdigit() else warmup_str
params_mode = os.environ.get("MANIFEST_PARAMS", "lcpc-sdig6")
field = os.environ.get("MANIFEST_FIELD", "mont127")
logn_str = os.environ.get("MANIFEST_LOGN", "16,18,20,22,24,26")
slices = os.environ.get("MANIFEST_SLICES", "adaptive")
host_name = os.environ.get("MANIFEST_HOST_NAME", "generic")
host_profile = os.environ.get("MANIFEST_HOST_PROFILE", "")
lcpc_commit = os.environ.get("MANIFEST_LCPC_COMMIT", "")
upstream_commit = os.environ.get("MANIFEST_UPSTREAM_COMMIT", "")

# 1. CPU info
cpu_info = {}
if os.path.exists("/proc/cpuinfo"):
    with open("/proc/cpuinfo", "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if ":" in line:
                k, v = line.split(":", 1)
                k = k.strip()
                v = v.strip()
                if k == "model name" and "model_name" not in cpu_info:
                    cpu_info["model_name"] = v
                elif k == "microcode" and "microcode" not in cpu_info:
                    cpu_info["microcode"] = v
                elif k == "vendor_id" and "vendor_id" not in cpu_info:
                    cpu_info["vendor_id"] = v
                elif k == "cpu family" and "cpu_family" not in cpu_info:
                    cpu_info["cpu_family"] = v
                elif k == "model" and "model" not in cpu_info:
                    cpu_info["model"] = v
                elif k == "stepping" and "stepping" not in cpu_info:
                    cpu_info["stepping"] = v

cpu_info["online_cpus"] = safe_read("/sys/devices/system/cpu/online")
cpu_info["offline_cpus"] = safe_read("/sys/devices/system/cpu/offline")
cpu_info["smt_control"] = safe_read("/sys/devices/system/cpu/smt/control")
cpu_info["lscpu"] = run_cmd(["lscpu"])

siblings_map = {}
for p in sorted(glob.glob("/sys/devices/system/cpu/cpu[0-9]*/topology/thread_siblings_list")):
    c = p.split("/")[5]
    siblings_map[c] = safe_read(p)
cpu_info["thread_siblings"] = siblings_map

# 2. Cache topology
cache_instances = {}
for cd in sorted(glob.glob("/sys/devices/system/cpu/cpu[0-9]*/cache/index*")):
    entry = {"index": os.path.basename(cd)}
    for attr in ["level", "type", "size", "shared_cpu_list", "ways_of_associativity", "coherency_line_size"]:
        val = safe_read(os.path.join(cd, attr))
        if val is not None:
            entry[attr] = val
    key = tuple(entry.get(k) for k in ("level", "type", "size", "shared_cpu_list"))
    cache_instances.setdefault(key, entry)
cache_topology = list(cache_instances.values())

machine_identity = {
    "hostname": platform.node().split(".")[0] or None,
}
host_identity_raw = os.environ.get("MANIFEST_HOST_IDENTITY_JSON", "")
if host_identity_raw:
    try:
        extra_ident = json.loads(host_identity_raw)
        if isinstance(extra_ident, dict):
            machine_identity.update(extra_ident)
    except ValueError:
        pass

# 3. Frequency and turbo state
no_turbo = safe_read("/sys/devices/system/cpu/intel_pstate/no_turbo")
amd_boost = safe_read("/sys/devices/system/cpu/cpufreq/boost")
turbo_disabled = None
if no_turbo is not None:
    turbo_disabled = (no_turbo == "1")
elif amd_boost is not None:
    turbo_disabled = (amd_boost == "0")

cur_freqs = {}
pinned_governors = {}
for c in expanded_cores:
    base = f"/sys/devices/system/cpu/cpu{c}/cpufreq"
    cur_freqs[f"cpu{c}"] = safe_read(f"{base}/scaling_cur_freq")
    pinned_governors[f"cpu{c}"] = safe_read(f"{base}/scaling_governor")

governor = safe_read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")

freq_turbo_info = {
    "turbo_disabled": turbo_disabled,
    "intel_no_turbo_raw": no_turbo,
    "amd_boost_raw": amd_boost,
    "scaling_governor": governor,
    "scaling_governor_pinned_cores": pinned_governors,
    "scaling_cur_freq_khz": cur_freqs,
}

# 4. Memory from /proc/meminfo
mem_info = {}
if os.path.exists("/proc/meminfo"):
    with open("/proc/meminfo", "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if ":" in line:
                k, v = line.split(":", 1)
                k = k.strip()
                v = v.strip()
                if k in ["MemTotal", "MemFree", "MemAvailable", "SwapTotal", "SwapFree"]:
                    mem_info[k] = v

# 5. OS and Kernel
os_info = {
    "kernel_sysname": platform.system(),
    "kernel_release": platform.release(),
    "kernel_version": platform.version(),
    "machine": platform.machine(),
    "glibc_version": platform.libc_ver()[1] or run_cmd(["getconf", "GNU_LIBC_VERSION"]),
}
if os.path.exists("/etc/os-release"):
    with open("/etc/os-release", "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if "=" in line:
                k, v = line.strip().split("=", 1)
                v = v.strip("\"'")
                if k in ["NAME", "VERSION", "ID", "PRETTY_NAME"]:
                    os_info[k] = v

# 6. Rust toolchain
rustc_ver = run_cmd(["rustc", "--version"])
rustc_verbose = run_cmd(["rustc", "--version", "--verbose"])
cargo_ver = run_cmd(["cargo", "--version"])
cargo_verbose = run_cmd(["cargo", "--version", "--verbose"])

def parse_verbose(text):
    if not text:
        return {}
    res = {}
    for line in text.splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            res[k.strip()] = v.strip()
    return res

rustc_host = parse_verbose(rustc_verbose).get("host")
rustflags_source = "RUSTFLAGS environment" if os.environ.get("RUSTFLAGS") else None
effective_rustflags = os.environ.get("RUSTFLAGS")
if os.environ.get("CARGO_ENCODED_RUSTFLAGS"):
    rustflags_source = "CARGO_ENCODED_RUSTFLAGS environment"
    effective_rustflags = os.environ["CARGO_ENCODED_RUSTFLAGS"].split(chr(0))
else:
    effective_rustflags = effective_rustflags.split() if effective_rustflags else None

cargo_config_path = os.path.join(code_dir, ".cargo", "config.toml")
cargo_config_raw = safe_read(cargo_config_path)
if effective_rustflags is None and cargo_config_raw:
    try:
        import tomllib
        with open(cargo_config_path, "rb") as f:
            cargo_config = tomllib.load(f)
        target_config = cargo_config.get("target", {}).get(rustc_host, {})
        configured_flags = target_config.get("rustflags")
        if configured_flags is None:
            configured_flags = cargo_config.get("build", {}).get("rustflags")
        effective_rustflags = configured_flags
        rustflags_source = f"{cargo_config_path} target={rustc_host}"
    except Exception:
        rustflags_source = f"{cargo_config_path} (raw config retained; TOML parse unavailable)"

lockfile_path = os.path.join(code_dir, "Cargo.lock")
lockfile_sha256 = None
locked_blake3_version = None
if os.path.exists(lockfile_path):
    with open(lockfile_path, "rb") as f:
        lockfile_data = f.read()
        lockfile_sha256 = hashlib.sha256(lockfile_data).hexdigest()
    try:
        import tomllib
        lock_data = tomllib.loads(lockfile_data.decode("utf-8"))
        locked_blake3_version = next(
            (pkg.get("version") for pkg in lock_data.get("package", []) if pkg.get("name") == "blake3"),
            None,
        )
    except Exception:
        pass

blake3_declarations = {}
try:
    import tomllib
    for manifest_path in glob.glob(os.path.join(code_dir, "**", "Cargo.toml"), recursive=True):
        if "/target" in manifest_path or "/.git/" in manifest_path or "/deps/" in manifest_path:
            continue
        try:
            with open(manifest_path, "rb") as f:
                manifest_data = tomllib.load(f)
        except Exception:
            continue
        declarations = {}
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            spec = manifest_data.get(section, {}).get("blake3")
            if spec is not None:
                declarations[section] = spec
        if declarations:
            relpath = os.path.relpath(manifest_path, code_dir)
            blake3_declarations[relpath] = declarations
except Exception:
    blake3_declarations = {"parse_error": "Python tomllib unavailable"}

rust_info = {
    "rustc_version": rustc_ver,
    "rustc_verbose": parse_verbose(rustc_verbose),
    "cargo_version": cargo_ver,
    "cargo_verbose": parse_verbose(cargo_verbose),
    "cargo_lock_sha256": lockfile_sha256,
    "profile": "release",
    "env_rustflags": os.environ.get("RUSTFLAGS"),
    "env_cargo_encoded_rustflags": os.environ.get("CARGO_ENCODED_RUSTFLAGS"),
    "effective_rustflags": effective_rustflags,
    "rustflags_source": rustflags_source,
    "cargo_rustc_wrapper": cargo_config.get("build", {}).get("rustc-wrapper") if "cargo_config" in locals() else None,
    "cargo_config_raw": cargo_config_raw,
}
rust_overrides = os.environ.get("MANIFEST_RUST_OVERRIDES_JSON", "")
if rust_overrides:
    rust_info.update(json.loads(rust_overrides))
    if rust_info.get("cargo_config_raw") is not None:
        rust_info["fork_cargo_config_raw_not_applied"] = rust_info["cargo_config_raw"]
        rust_info["cargo_config_raw"] = None

# 7. Compilers for SIMD-Spielman
gxx_out = run_cmd(["g++", "--version"])
cmake_out = run_cmd(["cmake", "--version"])
clangxx_out = run_cmd(["clang++", "--version"])
simd_compilers = {
    "gxx_version": gxx_out.splitlines()[0] if gxx_out else None,
    "cmake_version": cmake_out.splitlines()[0] if cmake_out else None,
    "clangxx_version": clangxx_out.splitlines()[0] if clangxx_out else None,
}

# 8. Git SHAs and tags
git_sha = run_cmd(["git", "-C", code_dir, "rev-parse", "HEAD"])
git_tags = run_cmd(["git", "-C", code_dir, "describe", "--tags", "--always"])
git_changes = run_cmd(["git", "-C", code_dir, "status", "--porcelain", "--untracked-files=no"])
git_changed_paths = [line.split(None, 1)[-1] for line in git_changes.splitlines()] if git_changes else []

patch_file = os.path.join(code_dir, "external/simd_spielman/params.patch")
patch_sha256 = None
if os.path.exists(patch_file):
    with open(patch_file, "rb") as f:
        patch_sha256 = hashlib.sha256(f.read()).hexdigest()

git_info = {
    "code_repo_sha": git_sha,
    "code_repo_tags": git_tags,
    "code_repo_dirty": bool(git_changed_paths),
    "code_repo_changed_paths": git_changed_paths,
    "pinned_lcpc_commit": lcpc_commit,
    "pinned_upstream_commit": upstream_commit,
    "simd_params_patch_sha256": patch_sha256,
}

# 9. Pinning and execution parameters
pinning_info = {
    "cores": cores,
    "threads": threads,
    "affinity_cores_expanded": [int(x) for x in expanded_cores if x.isdigit()],
}

benchmark_params = {
    "params_mode": params_mode,
    "field": field,
    "logN": [int(x.strip()) for x in logn_str.split(",") if x.strip().isdigit()],
    "slices": slices,
    "runs": runs,
    "warmup": warmup,
    "host_profile": host_profile or None,
}

manifest = {
    "manifest_schema_version": "1.0.0",
    "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "host_name": host_name,
    "host_profile": host_profile or None,
    "machine_identity": machine_identity,
    "cpu": cpu_info,
    "cache_topology": cache_topology,
    "frequency_turbo": freq_turbo_info,
    "memory": mem_info,
    "os_and_kernel": os_info,
    "rust_toolchain": rust_info,
    "blake3": {
        "locked_version": locked_blake3_version,
        "manifest_declarations": blake3_declarations,
    },
    "compilers_simd_spielman": simd_compilers,
    "git": git_info,
    "pinning": pinning_info,
    "benchmark_parameters": benchmark_params,
}

if os.environ.get("MANIFEST_RBD_CLI"):
    manifest["rbd_cli"] = os.environ["MANIFEST_RBD_CLI"]
if os.environ.get("MANIFEST_RBD_MODE"):
    manifest["mode"] = os.environ["MANIFEST_RBD_MODE"]
mt_env = os.environ.get("MANIFEST_RBD_MT_THREADS")
if mt_env and mt_env.isdigit():
    manifest["mt_threads"] = int(mt_env)
core_sel_raw = os.environ.get("MANIFEST_CORE_SELECTION_JSON")
if core_sel_raw:
    try:
        manifest["core_selection"] = json.loads(core_sel_raw)
    except ValueError:
        pass

if extra_json:
    if os.path.exists(extra_json):
        try:
            with open(extra_json, "r", encoding="utf-8") as f:
                manifest["extra"] = json.load(f)
        except Exception as e:
            manifest["extra_error"] = str(e)
    else:
        try:
            manifest["extra"] = json.loads(extra_json)
        except Exception:
            manifest["extra_raw"] = extra_json

if out_file == "-" or not out_file:
    print(json.dumps(manifest, indent=2))
else:
    os.makedirs(os.path.dirname(os.path.abspath(out_file)), exist_ok=True)
    with open(out_file, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    print(f"[INFO] Manifest written to {out_file}")
EOF
}

# If run directly as a script
if [ "${0##*/}" = "common.sh" ]; then
    if [ "${1:-}" = "--test-manifest" ]; then
        HOST_NAME=$(host_instance_name)
        autodetect_cores
        CORES="$HOST_DEFAULT_SINGLE_CORE"
        write_manifest "${2:--}"
        exit 0
    fi
    parse_args "$@"
    log_info "common.sh parsed arguments successfully."
    log_info "Host: $HOST_NAME, Threads: $THREADS, Cores: $CORES, Params: $PARAMS, Field: $FIELD, Runs: $RUNS"
fi
