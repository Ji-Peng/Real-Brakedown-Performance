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
INSTANCE=${INSTANCE:-rbd-bench-c3d}
ZONE=${ZONE:-us-central1-a}
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
Usage: $0 [--instance NAME] [--zone ZONE] [--remote-dir ABSOLUTE_PATH]

Installs the pinned Rust toolchain and SIMD-Spielman build prerequisites on an
existing VM, syncs this code/ checkout, and writes a first-boot manifest under
code/results/gcp-c3d-highmem-30/ on the VM. It does not create a VM.

Defaults: instance=$INSTANCE, zone=$ZONE, remote directory=~/RealBrakedownPerf/code.
If gcloud is not on PATH, set GCLOUD to
  \$HOME/.local/share/google-cloud-sdk/bin/gcloud
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
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
REMOTE_CODE_DIR=${5:-}
REMOTE_STAGE=/tmp/realbrakedownperf-gcp-setup
BUNDLE=$REMOTE_STAGE/code.bundle
SOURCE_ARCHIVE=$REMOTE_STAGE/code-working-tree.tar.gz

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
    pkg-config python3

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
git -C "$REMOTE_CODE_DIR" checkout --detach "$SOURCE_SHA"
tar -xzf "$SOURCE_ARCHIVE" --no-same-owner -C "$REMOTE_CODE_DIR"

FIRST_BOOT_DIR=$REMOTE_CODE_DIR/results/gcp-c3d-highmem-30
mkdir -p "$FIRST_BOOT_DIR"
FIRST_BOOT_MANIFEST=$FIRST_BOOT_DIR/first-boot-manifest.json \
EXPECTED_ZONE=$ZONE EXPECTED_INSTANCE=$INSTANCE EXPECTED_TOOLCHAIN=$TOOLCHAIN \
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
    package = read_text(f"{base}/physical_package_id")
    core = read_text(f"{base}/core_id")
    siblings_raw = read_text(f"{base}/thread_siblings_list")
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
    entry = {
        "cpu": int(re.search(r"/cpu(\d+)/", cache_dir).group(1)),
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

checks = {
    "gcp_machine_type_c3d_highmem_30": metadata_machine_type == "c3d-highmem-30",
    "gcp_zone_matches_selection": metadata_zone == os.environ["EXPECTED_ZONE"],
    "gcp_instance_name_matches_selection": metadata_name == os.environ["EXPECTED_INSTANCE"],
    "debian_12": os_release_values.get("ID") == "debian" and os_release_values.get("VERSION_ID") == "12",
    "amd_epyc_9xx4_model": "AMD EPYC 9" in model_name.upper(),
    "fifteen_physical_cores": len(physical_core_ids) == 15 and reported_core_count == 15,
    "one_thread_per_core": cpu_count == 15 and threads_per_core == 1 and all_siblings_are_singletons and len(online_cpus) == 15,
    "avx512ifma_present": avx512ifma_present,
    "l3_cache_topology_available": bool(l3_domains),
    "eight_distinct_cores_in_one_l3_domain": any(domain["physical_core_count"] >= 8 for domain in l3_domains),
}

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
    "gcp_metadata": {
        "instance_name": metadata_name,
        "machine_type": metadata_machine_type,
        "machine_type_resource": metadata_machine_type_full,
        "zone": metadata_zone,
        "zone_resource": metadata_zone_full,
    },
    "expected_machine_type": "c3d-highmem-30",
    "expected_provisioning_model": "STANDARD",
    "expected_on_host_maintenance": "TERMINATE",
    "expected_threads_per_core": 1,
    "expected_boot_disk": {"size": "100GB", "type": "pd-balanced"},
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
printf 'Setup complete for %s in %s.\n' "$INSTANCE" "$ZONE"
REMOTE_SETUP

chmod 755 "$REMOTE_HELPER"
printf 'Syncing %s to %s:%s\n' "$CODE_DIR" "$INSTANCE" "${REMOTE_CODE_DIR:-~/RealBrakedownPerf/code}"
"$GCLOUD" compute scp --zone="$ZONE" \
    "$BUNDLE" "$SOURCE_ARCHIVE" "$REMOTE_HELPER" \
    "$INSTANCE:/tmp/"
printf 'Installing toolchain and packages, then checking the VM at %s.\n' "$ZONE"
"$GCLOUD" compute ssh "$INSTANCE" --zone="$ZONE" \
    --command "sh /tmp/setup_vm_remote.sh '$TOOLCHAIN' '$SOURCE_SHA' '$ZONE' '$INSTANCE' '$REMOTE_CODE_DIR'"
