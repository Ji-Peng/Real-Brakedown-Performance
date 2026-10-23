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

# Host profile: GCP c4a-highmem-16 (Google Axion / Arm Neoverse V2, 16 physical cores, no SMT).
# Core IDs and cache topology are discovered from Linux sysfs; no core number is assumed.

HOST_NAME=gcp-c4a-highmem-16
HOST_IS_TEST_ONLY=0
HOST_FORBIDDEN_CORES=
HOST_DEFAULT_CORES=
HOST_DEFAULT_SINGLE_CORE=
HOST_ALLOWED_CORES=

_gcp_load_arm_host_profile() {
    # One file per VM instance: two VMs of the same machine type must not
    # overwrite each other's topology record when results are copied back.
    _gcp_instance=$(host_instance_name)
    _gcp_arm_profile_manifest="$CODE_DIR/results/gcp-c4a-highmem-16/host-profile-$_gcp_instance.json"
    mkdir -p "$(dirname -- "$_gcp_arm_profile_manifest")"

    _gcp_arm_profile_info=$(
        PROFILE_MANIFEST="$_gcp_arm_profile_manifest" PROFILE_INSTANCE="$_gcp_instance" python3 - <<'PY'
import datetime
import glob
import json
import os
import pathlib
import sys


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


online = parse_cpu_list(read_text("/sys/devices/system/cpu/online"))
if not online:
    # Fallback to os.sched_getaffinity or cpu_count
    try:
        online = set(os.sched_getaffinity(0))
    except Exception:
        online = set(range(os.cpu_count() or 16))

topology = {}
for cpu in sorted(online):
    base = f"/sys/devices/system/cpu/cpu{cpu}/topology"
    package = read_text(f"{base}/physical_package_id") or "0"
    core = read_text(f"{base}/core_id") or str(cpu)
    siblings = parse_cpu_list(read_text(f"{base}/thread_siblings_list") or str(cpu))
    topology[cpu] = {
        "physical_package_id": package,
        "core_id": core,
        "thread_siblings": sorted(siblings),
    }

# Scan cache levels
domains = {}
for cache_dir in sorted(glob.glob("/sys/devices/system/cpu/cpu[0-9]*/cache/index*")):
    level = read_text(f"{cache_dir}/level")
    if level not in ("2", "3"):
        continue
    shared_raw = read_text(f"{cache_dir}/shared_cpu_list")
    if not shared_raw:
        continue
    shared = parse_cpu_list(shared_raw) & online
    if not shared:
        continue
    key = (level, tuple(sorted(shared)))
    domain = domains.setdefault(key, {
        "level": level,
        "shared_cpu_list": shared_raw,
        "size": read_text(f"{cache_dir}/size"),
        "cpus": sorted(shared),
        "cores": {},
    })
    for cpu in sorted(shared):
        info = topology.get(cpu, {"physical_package_id": "0", "core_id": str(cpu)})
        physical_key = (info["physical_package_id"], info["core_id"])
        domain["cores"].setdefault(physical_key, cpu)

# Filter for domains with at least 8 cores, preferring highest cache level (L3 > L2)
candidates = []
for (level, _), domain in domains.items():
    representatives = sorted(domain["cores"].values())
    if len(representatives) >= 8:
        # Sort priority: level 3 first (int 3 > 2), then larger core count
        candidates.append((int(level), len(representatives), domain, representatives))

candidates.sort(key=lambda item: (-item[0], -item[1], item[3]))

if candidates:
    _, _, selected_domain, representatives = candidates[0]
    benchmark_cpus = representatives[:8]
    domain_shared_list = selected_domain["shared_cpu_list"]
    domain_size = selected_domain["size"]
else:
    # If sysfs doesn't expose L3 domain with >= 8 cores, use online cores
    sorted_online = sorted(online)
    if len(sorted_online) >= 8:
        # Avoid core 0 if possible
        if len(sorted_online) > 8 and 0 in sorted_online:
            benchmark_cpus = [c for c in sorted_online if c != 0][:8]
        else:
            benchmark_cpus = sorted_online[:8]
    else:
        benchmark_cpus = sorted_online
    domain_shared_list = ",".join(str(c) for c in benchmark_cpus)
    domain_size = "unknown"

# Single-core benchmark avoids core 0 where feasible to isolate from OS interrupts
single_core = next((cpu for cpu in benchmark_cpus if cpu != 0), benchmark_cpus[0])
benchmark_cores = ",".join(str(cpu) for cpu in benchmark_cpus)
allowed_cores = ",".join(str(cpu) for cpu in sorted(online))

domain_record = {
    "shared_cpu_list": domain_shared_list,
    "size": domain_size,
    "selected_benchmark_cpus": benchmark_cpus,
    "selected_physical_cores": [
        {"cpu": cpu, **topology.get(cpu, {})} for cpu in benchmark_cpus
    ],
    "single_thread_core": single_core,
}

def gce_zone():
    import urllib.request
    # Link-local metadata IP: no DNS lookup, so the timeout bounds the call.
    host = os.environ.get("GCE_METADATA_HOST", "169.254.169.254")
    req = urllib.request.Request(
        f"http://{host}/computeMetadata/v1/instance/zone",
        headers={"Metadata-Flavor": "Google"})
    try:
        with urllib.request.urlopen(req, timeout=0.3) as response:
            return response.read().decode("utf-8", errors="replace").strip().rsplit("/", 1)[-1]
    except Exception:
        return None


manifest = {
    "schema_version": "1.0.0",
    "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "host_profile": "gcp-c4a-highmem-16",
    "instance": os.environ.get("PROFILE_INSTANCE"),
    "zone": gce_zone(),
    "cpu_architecture": "aarch64",
    "source": "Linux sysfs at profile load time",
    "online_cpus": sorted(online),
    "selected_domain": domain_record,
}
manifest_path = pathlib.Path(os.environ["PROFILE_MANIFEST"])
manifest_path.parent.mkdir(parents=True, exist_ok=True)
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

print(f"{single_core}|{benchmark_cores}|{allowed_cores}|{domain_shared_list}")
PY
    ) || {
        printf '%s\n' '[ERROR] Could not derive eight-core benchmark configuration on c4a.' >&2
        return 1
    }

    _gcp_arm_single_core=
    _gcp_arm_benchmark_cores=
    _gcp_arm_allowed_cores=
    _gcp_arm_selected_domain=
    IFS='|' read -r _gcp_arm_single_core _gcp_arm_benchmark_cores _gcp_arm_allowed_cores _gcp_arm_selected_domain <<EOF
$_gcp_arm_profile_info
EOF
    if [ -z "$_gcp_arm_single_core" ] || [ -z "$_gcp_arm_benchmark_cores" ] || \
        [ -z "$_gcp_arm_allowed_cores" ]; then
        printf '%s\n' '[ERROR] GCP c4a host profile returned incomplete topology data.' >&2
        return 1
    fi
    HOST_DEFAULT_SINGLE_CORE=$_gcp_arm_single_core
    HOST_DEFAULT_CORES=$_gcp_arm_benchmark_cores
    HOST_ALLOWED_CORES=$_gcp_arm_allowed_cores

    printf '[INFO] GCP c4a core profile: single=%s, eight-core=%s, shared CPUs=%s\n' \
        "$HOST_DEFAULT_SINGLE_CORE" "$HOST_DEFAULT_CORES" "$_gcp_arm_selected_domain" >&2
    printf '[INFO] Derived topology recorded in %s\n' "$_gcp_arm_profile_manifest" >&2
}

_gcp_load_arm_host_profile

# shellcheck disable=SC2317 # Called by reproduce/common.sh after sourcing this profile.
host_check_mode() {
    printf '[INFO] Host profile: %s (Google Axion / Arm Neoverse V2, cores discovered from sysfs)\n' "$HOST_NAME"
}
