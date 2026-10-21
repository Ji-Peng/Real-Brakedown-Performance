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

# If gcloud is not on PATH on the workstation, set GCLOUD to
# "$HOME/.local/share/google-cloud-sdk/bin/gcloud".
GCLOUD=${GCLOUD:-gcloud}
MACHINE=${MACHINE:-c3d}
INSTANCE=${INSTANCE:-}
ZONE=${ZONE:-}  # default per machine: c3d -> us-central1-c, c4a -> us-central1-a
REMOTE_CODE_DIR=${REMOTE_CODE_DIR:-}
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/rbd-gcp-setup.XXXXXXXX")

cleanup() {
    rm -rf "$TEMP_DIR"
}
trap cleanup 0

usage() {
    cat <<EOF
Usage: $0 [--machine c3d|c4a] [--instance NAME] [--zone ZONE] [--remote-dir ABSOLUTE_PATH]

Installs the pinned Rust toolchain and SIMD-Spielman build prerequisites on an
existing VM, syncs this code/ checkout, and writes a first-boot manifest under
code/results/<host>/ on the VM. It does not create a VM.

Options:
  --machine c3d|c4a   Target machine family (default: c3d)
                        c3d: rbd-bench-c3d (results/gcp-c3d-highmem-16)
                        c4a: rbd-bench-c4a (results/gcp-c4a-highmem-16)
  --instance NAME     Override instance name
  --zone ZONE         GCP zone (default: us-central1-c for c3d, us-central1-a for c4a)
  --remote-dir PATH   Remote checkout path (default: ~/RealBrakedownPerf/code)
  -h, --help          Show this help message

If gcloud is not on PATH, set GCLOUD to
  \$HOME/.local/share/google-cloud-sdk/bin/gcloud
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --machine)
            [ "$#" -ge 2 ] || { printf '%s\n' "--machine requires an argument (c3d|c4a)" >&2; exit 2; }
            MACHINE=$2
            shift 2
            ;;
        --instance)
            [ "$#" -ge 2 ] || { printf '%s\n' "--instance requires a name" >&2; exit 2; }
            INSTANCE=$2
            shift 2
            ;;
        --zone)
            [ "$#" -ge 2 ] || { printf '%s\n' "--zone requires a zone" >&2; exit 2; }
            ZONE=$2
            shift 2
            ;;
        --remote-dir)
            [ "$#" -ge 2 ] || { printf '%s\n' "--remote-dir requires an absolute path" >&2; exit 2; }
            REMOTE_CODE_DIR=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'Unknown option: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

[ -n "$ZONE" ] || { if [ "$MACHINE" = c3d ]; then ZONE=us-central1-c; else ZONE=us-central1-a; fi; }
case "$MACHINE" in
    c3d)
        DEFAULT_INSTANCE=rbd-bench-c3d
        HOST_PROFILE_NAME=gcp-c3d-highmem-16
        ;;
    c4a)
        DEFAULT_INSTANCE=rbd-bench-c4a
        HOST_PROFILE_NAME=gcp-c4a-highmem-16
        ;;
    *)
        printf 'Invalid --machine: %s (must be c3d or c4a)\n' "$MACHINE" >&2
        exit 2
        ;;
esac

INSTANCE=${INSTANCE:-$DEFAULT_INSTANCE}

case "$INSTANCE" in
    ''|*[!A-Za-z0-9._-]*) printf 'Invalid instance name: %s\n' "$INSTANCE" >&2; exit 2 ;;
esac
case "$ZONE" in
    ''|*[!A-Za-z0-9._-]*) printf 'Invalid zone: %s\n' "$ZONE" >&2; exit 2 ;;
esac
if [ -n "$REMOTE_CODE_DIR" ]; then
    case "$REMOTE_CODE_DIR" in
        /*) ;;
        *) printf '%s\n' '--remote-dir must be absolute' >&2; exit 2 ;;
    esac
    case "$REMOTE_CODE_DIR" in
        *[!A-Za-z0-9_./-]*) printf '%s\n' '--remote-dir may contain only letters, numbers, and /._-' >&2; exit 2 ;;
    esac
fi

if ! command -v "$GCLOUD" >/dev/null 2>&1; then
    printf 'gcloud command not found: %s\n' "$GCLOUD" >&2
    printf '%s\n' 'Set GCLOUD="$HOME/.local/share/google-cloud-sdk/bin/gcloud" if needed.' >&2
    exit 127
fi
for required_command in git tar sha256sum awk sed; do
    if ! command -v "$required_command" >/dev/null 2>&1; then
        printf 'Required local command not found: %s\n' "$required_command" >&2
        exit 127
    fi
done

TOOLCHAIN=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$CODE_DIR/rust-toolchain.toml")
if [ -z "$TOOLCHAIN" ]; then
    printf 'Could not read the pinned Rust channel from %s/rust-toolchain.toml\n' "$CODE_DIR" >&2
    exit 1
fi
SOURCE_SHA=$(git -C "$CODE_DIR" rev-parse HEAD)
BUNDLE="$TEMP_DIR/code.bundle"
SOURCE_ARCHIVE="$TEMP_DIR/code-working-tree.tar.gz"
REMOTE_HELPER="$TEMP_DIR/setup_vm_remote.sh"

git -C "$CODE_DIR" bundle create "$BUNDLE" --all HEAD
git bundle verify "$BUNDLE" >/dev/null
tar --exclude=./.git --exclude=./target --exclude=./target-* \
    -czf "$SOURCE_ARCHIVE" -C "$CODE_DIR" .
sha256sum "$BUNDLE" "$SOURCE_ARCHIVE"

cat > "$REMOTE_HELPER" <<'REMOTE_SETUP'
#!/bin/sh
set -eu

TOOLCHAIN=$1
SOURCE_SHA=$2
ZONE=$3
INSTANCE=$4
MACHINE=$5
HOST_PROFILE_NAME=$6
REMOTE_CODE_DIR=${7:-}
REMOTE_STAGE=/tmp/realbrakedownperf-gcp-setup
BUNDLE=/tmp/code.bundle            # copied by gcloud compute scp into /tmp/
SOURCE_ARCHIVE=/tmp/code-working-tree.tar.gz

if [ -z "$REMOTE_CODE_DIR" ]; then
    REMOTE_CODE_DIR=$HOME/RealBrakedownPerf/code
fi
case "$REMOTE_CODE_DIR" in
    /*) ;;
    *) printf '%s\n' 'Remote code directory must be absolute' >&2; exit 2 ;;
esac

sudo -n apt-get update
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y \
    build-essential ca-certificates cmake curl g++ git libboost-all-dev \
    pkg-config python3 python3-matplotlib python3-numpy || true

# Attempt to install linux-perf if available
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y linux-perf || true

mkdir -p "$REMOTE_STAGE"
if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        -o "$REMOTE_STAGE/rustup-init.sh"
    sh "$REMOTE_STAGE/rustup-init.sh" -y --profile minimal --default-toolchain none
fi
PATH="$HOME/.cargo/bin:$PATH"
export PATH
rustup toolchain install "$TOOLCHAIN" --profile minimal --component rustfmt --component clippy

if [ -d "$REMOTE_CODE_DIR/.git" ] || [ -f "$REMOTE_CODE_DIR/.git" ]; then
    git -C "$REMOTE_CODE_DIR" fetch "$BUNDLE" --tags
    git -C "$REMOTE_CODE_DIR" fetch "$BUNDLE" "$SOURCE_SHA"
elif [ ! -e "$REMOTE_CODE_DIR" ]; then
    mkdir -p "$(dirname -- "$REMOTE_CODE_DIR")"
    git clone --no-checkout "$BUNDLE" "$REMOTE_CODE_DIR"
else
    printf 'Remote path exists and is not a Git checkout: %s\n' "$REMOTE_CODE_DIR" >&2
    exit 1
fi
# Force the checkout: an earlier sync may have overlaid working-tree files.
# Keep results/ and cargo build caches; remove other untracked files.
git -C "$REMOTE_CODE_DIR" checkout --force --detach "$SOURCE_SHA"
git -C "$REMOTE_CODE_DIR" clean -fdq -e results/ -e target/ -e 'target-*/'

tar -xzf "$SOURCE_ARCHIVE" --no-same-owner -C "$REMOTE_CODE_DIR"

# The fork's tests read SIMD-Spielman's test vectors from ../SIMD-Spielman-for-ZKPs/tv
# (the parent repository's read-only submodule). Provide the same pinned upstream
# checkout next to code/ on the VM.
UPSTREAM_COMMIT=7b399dbfecc406c79349015f1e21accc4def1be7
UPSTREAM_DIR=$(dirname -- "$REMOTE_CODE_DIR")/SIMD-Spielman-for-ZKPs
if [ ! -d "$UPSTREAM_DIR/.git" ]; then
    git clone --quiet https://github.com/flokrieger/SIMD-Spielman-for-ZKPs "$UPSTREAM_DIR"
fi
git -C "$UPSTREAM_DIR" checkout --quiet --force --detach "$UPSTREAM_COMMIT"
[ "$(git -C "$UPSTREAM_DIR" rev-parse HEAD)" = "$UPSTREAM_COMMIT" ] || { printf '%s\n' 'SIMD-Spielman upstream commit mismatch' >&2; exit 1; }
ls "$UPSTREAM_DIR/tv/bd_spielman_extension_N12_in.txt" >/dev/null

FIRST_BOOT_DIR=$REMOTE_CODE_DIR/results/$HOST_PROFILE_NAME
mkdir -p "$FIRST_BOOT_DIR"
FIRST_BOOT_MANIFEST=$FIRST_BOOT_DIR/first-boot-manifest.json \
EXPECTED_ZONE=$ZONE EXPECTED_INSTANCE=$INSTANCE EXPECTED_TOOLCHAIN=$TOOLCHAIN \
EXPECTED_MACHINE=$MACHINE EXPECTED_HOST_PROFILE=$HOST_PROFILE_NAME \
REMOTE_CODE_DIR=$REMOTE_CODE_DIR python3 - <<'PY'
import datetime
import glob
import json
import os
import pathlib
import platform
import re
import subprocess
import sys
import urllib.error
import urllib.request


def read_text(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def parse_cpu_list(value):
    cpus = set()
    if not value:
        return cpus
    for part in value.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            first, last = part.split("-", 1)
            cpus.update(range(int(first), int(last) + 1))
        else:
            cpus.add(int(part))
    return cpus


def expand_cpu_list(cpus):
    return ",".join(str(cpu) for cpu in sorted(cpus))


def lscpu_value(values, key):
    value = values.get(key)
    if value is None:
        return None
    match = re.match(r"\s*(\d+)", value)
    return int(match.group(1)) if match else None


def gcp_metadata(path):
    request = urllib.request.Request(
        f"http://metadata.google.internal/computeMetadata/v1/{path}",
        headers={"Metadata-Flavor": "Google"},
    )
    try:
        with urllib.request.urlopen(request, timeout=2) as response:
            return response.read().decode("utf-8", errors="replace").strip()
    except (OSError, urllib.error.URLError):
        return None


lscpu_result = subprocess.run(["lscpu"], check=True, text=True, capture_output=True)
lscpu_output = lscpu_result.stdout
lscpu_values = {}
for line in lscpu_output.splitlines():
    if ":" in line:
        key, value = line.split(":", 1)
        lscpu_values[key.strip()] = value.strip()

online_raw = read_text("/sys/devices/system/cpu/online")
online_cpus = parse_cpu_list(online_raw)
cpu_topology = []
cpu_topology_by_id = {}
physical_core_ids = set()
all_siblings_are_singletons = True
for cpu in sorted(online_cpus):
    base = f"/sys/devices/system/cpu/cpu{cpu}/topology"
    package = read_text(f"{base}/physical_package_id") or "0"
    core = read_text(f"{base}/core_id") or str(cpu)
    siblings_raw = read_text(f"{base}/thread_siblings_list") or str(cpu)
    siblings = parse_cpu_list(siblings_raw)
    all_siblings_are_singletons = all_siblings_are_singletons and len(siblings) == 1
    if package is not None and core is not None:
        physical_core_ids.add((package, core))
    topology_entry = {
        "cpu": cpu,
        "physical_package_id": package,
        "core_id": core,
        "thread_siblings_list": siblings_raw,
    }
    cpu_topology.append(topology_entry)
    cpu_topology_by_id[cpu] = topology_entry

cache_by_key = {}
for cache_dir in sorted(glob.glob("/sys/devices/system/cpu/cpu[0-9]*/cache/index*")):
    level = read_text(f"{cache_dir}/level")
    cache_type = read_text(f"{cache_dir}/type")
    shared_raw = read_text(f"{cache_dir}/shared_cpu_list")
    cpu_match = re.search(r"/cpu(\d+)/", cache_dir)
    cpu_num = int(cpu_match.group(1)) if cpu_match else 0
    entry = {
        "cpu": cpu_num,
        "index": os.path.basename(cache_dir),
        "level": level,
        "type": cache_type,
        "size": read_text(f"{cache_dir}/size"),
        "shared_cpu_list": shared_raw,
        "coherency_line_size": read_text(f"{cache_dir}/coherency_line_size"),
    }
    cache_by_key[(entry["cpu"], entry["index"], shared_raw)] = entry

l3_groups = {}
for entry in cache_by_key.values():
    if entry["level"] != "3":
        continue
    shared = parse_cpu_list(entry["shared_cpu_list"])
    online_shared = sorted(shared & online_cpus)
    core_ids = set()
    for cpu in online_shared:
        info = cpu_topology_by_id.get(cpu)
        if info and info["physical_package_id"] is not None and info["core_id"] is not None:
            core_ids.add((info["physical_package_id"], info["core_id"]))
    key = entry["shared_cpu_list"] or "unknown"
    group = l3_groups.setdefault(key, {
        "shared_cpu_list": key,
        "size": entry["size"],
        "online_cpus": expand_cpu_list(online_shared),
        "physical_core_count": len(core_ids),
    })
    group["physical_core_count"] = max(group["physical_core_count"], len(core_ids))

l3_domains = sorted(l3_groups.values(), key=lambda item: item["shared_cpu_list"])
model_name = lscpu_values.get("Model name", "")
cpu_count = lscpu_value(lscpu_values, "CPU(s)")
cores_per_socket = lscpu_value(lscpu_values, "Core(s) per socket")
sockets = lscpu_value(lscpu_values, "Socket(s)")
threads_per_core = lscpu_value(lscpu_values, "Thread(s) per core")
reported_core_count = (cores_per_socket * sockets) if cores_per_socket is not None and sockets is not None else None
os_release = read_text("/etc/os-release") or ""
os_release_values = {}
for line in os_release.splitlines():
    if "=" in line:
        key, value = line.split("=", 1)
        os_release_values[key] = value.strip().strip("\"'")

metadata_machine_type_full = gcp_metadata("instance/machine-type")
metadata_zone_full = gcp_metadata("instance/zone")
metadata_name = gcp_metadata("instance/name")
metadata_machine_type = metadata_machine_type_full.rsplit("/", 1)[-1] if metadata_machine_type_full else None
metadata_zone = metadata_zone_full.rsplit("/", 1)[-1] if metadata_zone_full else None

cpuinfo = read_text("/proc/cpuinfo") or ""
cpu_blocks = re.split(r"\n\s*\n", cpuinfo)
cpu_flags = []
for block in cpu_blocks:
    if re.search(r"^processor\s*:\s*\d+", block, re.MULTILINE):
        flags_line = re.search(r"^(?:flags|Features)\s*:\s*(.*)$", block, re.MULTILINE)
        if flags_line:
            cpu_flags.append(set(flags_line.group(1).split()))
avx512ifma_cpu_count = sum(1 for flags in cpu_flags if "avx512ifma" in flags)
avx512ifma_grep = subprocess.run(
    ["grep", "-o", "avx512ifma", "/proc/cpuinfo"],
    text=True,
    capture_output=True,
)
avx512ifma_grep_output = avx512ifma_grep.stdout.strip()
avx512ifma_present = avx512ifma_grep.returncode == 0 and bool(avx512ifma_grep_output)

expected_machine = os.environ["EXPECTED_MACHINE"]
if expected_machine == "c3d":
    checks = {
        "gcp_machine_type_c3d_highmem_16": metadata_machine_type == "c3d-highmem-16",
        "gcp_zone_matches_selection": metadata_zone == os.environ["EXPECTED_ZONE"],
        "gcp_instance_name_matches_selection": metadata_name == os.environ["EXPECTED_INSTANCE"],
        "debian_12": os_release_values.get("ID") == "debian" and os_release_values.get("VERSION_ID") == "12",
        "amd_epyc_9xx4_model": "AMD EPYC 9" in model_name.upper(),
        "eight_physical_cores": len(physical_core_ids) == 8 and reported_core_count == 8,
        "one_thread_per_core": cpu_count == 8 and threads_per_core == 1 and all_siblings_are_singletons and len(online_cpus) == 8,
        "avx512ifma_present": avx512ifma_present,
        "l3_cache_topology_available": bool(l3_domains),
        "eight_distinct_cores_in_one_l3_domain": any(domain["physical_core_count"] >= 8 for domain in l3_domains),
    }
    expected_machine_type = "c3d-highmem-16"
    expected_boot_disk = {"size": "100GB", "type": "pd-balanced"}
    expected_threads_per_core = 1
else:
    # c4a (Google Axion / Arm Neoverse V2, 16 physical cores, no SMT)
    checks = {
        "gcp_machine_type_c4a_highmem_16": metadata_machine_type == "c4a-highmem-16",
        "gcp_zone_matches_selection": metadata_zone == os.environ["EXPECTED_ZONE"],
        "gcp_instance_name_matches_selection": metadata_name == os.environ["EXPECTED_INSTANCE"],
        "debian_12": os_release_values.get("ID") == "debian" and os_release_values.get("VERSION_ID") == "12",
        "aarch64_architecture": platform.machine().lower() == "aarch64",
        "sixteen_cores": (len(physical_core_ids) == 16 or cpu_count == 16) and len(online_cpus) == 16,
        "one_thread_per_core": threads_per_core in (None, 1) and all_siblings_are_singletons,
        "cache_topology_available": len(cache_by_key) > 0,
    }
    expected_machine_type = "c4a-highmem-16"
    expected_boot_disk = {"size": "100GB", "type": "hyperdisk-balanced"}
    expected_threads_per_core = 1

rustc = subprocess.run(
    ["rustup", "run", os.environ["EXPECTED_TOOLCHAIN"], "rustc", "--version", "--verbose"],
    check=True,
    text=True,
    capture_output=True,
).stdout.strip()
cargo = subprocess.run(
    ["rustup", "run", os.environ["EXPECTED_TOOLCHAIN"], "cargo", "--version", "--verbose"],
    check=True,
    text=True,
    capture_output=True,
).stdout.strip()

manifest = {
    "schema_version": "1.0.0",
    "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "instance": os.environ["EXPECTED_INSTANCE"],
    "zone": os.environ["EXPECTED_ZONE"],
    "machine_family": expected_machine,
    "host_profile": os.environ["EXPECTED_HOST_PROFILE"],
    "gcp_metadata": {
        "instance_name": metadata_name,
        "machine_type": metadata_machine_type,
        "machine_type_resource": metadata_machine_type_full,
        "zone": metadata_zone,
        "zone_resource": metadata_zone_full,
    },
    "expected_machine_type": expected_machine_type,
    "expected_provisioning_model": "STANDARD",
    "expected_on_host_maintenance": "TERMINATE",
    "expected_threads_per_core": expected_threads_per_core,
    "expected_boot_disk": expected_boot_disk,
    "lscpu": lscpu_output,
    "cpu": {
        "model_name": model_name,
        "logical_cpu_count_lscpu": cpu_count,
        "online_cpus": online_raw,
        "physical_core_count_sysfs": len(physical_core_ids),
        "physical_core_count_lscpu": reported_core_count,
        "sockets": sockets,
        "threads_per_core": threads_per_core,
        "topology": cpu_topology,
    },
    "avx512ifma": {
        "present": avx512ifma_present,
        "cpu_count_with_flag": avx512ifma_cpu_count,
        "grep_o_output": avx512ifma_grep_output,
    },
    "cache_topology": sorted(cache_by_key.values(), key=lambda item: (item["cpu"], item["index"])),
    "l3_domains": l3_domains,
    "rust_toolchain": {
        "pinned_channel": os.environ["EXPECTED_TOOLCHAIN"],
        "rustc_version_verbose": rustc,
        "cargo_version_verbose": cargo,
    },
    "os": {
        "platform": platform.platform(),
        "release": os_release,
        "machine": platform.machine(),
        "kernel": platform.release(),
    },
    "checks": checks,
    "status": "passed" if all(checks.values()) else "failed",
}

manifest_path = pathlib.Path(os.environ["FIRST_BOOT_MANIFEST"])
manifest_path.parent.mkdir(parents=True, exist_ok=True)
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
print(json.dumps({"manifest": str(manifest_path), "status": manifest["status"], "checks": checks}, indent=2))
if not all(checks.values()):
    sys.exit(1)
PY

printf 'First-boot manifest: %s\n' "$FIRST_BOOT_DIR/first-boot-manifest.json"
printf 'Setup complete for %s (%s) in %s.\n' "$INSTANCE" "$MACHINE" "$ZONE"
REMOTE_SETUP

chmod 755 "$REMOTE_HELPER"
printf 'Syncing %s to %s:%s\n' "$CODE_DIR" "$INSTANCE" "${REMOTE_CODE_DIR:-~/RealBrakedownPerf/code}"
"$GCLOUD" compute scp --zone="$ZONE" \
    "$BUNDLE" "$SOURCE_ARCHIVE" "$REMOTE_HELPER" \
    "$INSTANCE:/tmp/"
printf 'Installing toolchain and packages, then running first-boot checks for %s in %s.\n' "$MACHINE" "$ZONE"
"$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
    --command "sh /tmp/setup_vm_remote.sh '$TOOLCHAIN' '$SOURCE_SHA' '$ZONE' '$INSTANCE' '$MACHINE' '$HOST_PROFILE_NAME' '$REMOTE_CODE_DIR'"
