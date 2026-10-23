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

# Host profile: GCP c3d-highmem-16. Core IDs are discovered from Linux sysfs;
# no core number or L3 sharing layout is assumed.

HOST_NAME=gcp-c3d-highmem-16
HOST_IS_TEST_ONLY=0
HOST_FORBIDDEN_CORES=
HOST_DEFAULT_CORES=
HOST_DEFAULT_SINGLE_CORE=
HOST_ALLOWED_CORES=

_gcp_load_host_profile() {
# One file per VM instance: two VMs of the same machine type must not
# overwrite each other's topology record when results are copied back.
_gcp_instance=$(host_instance_name)
_gcp_profile_manifest="$CODE_DIR/results/gcp-c3d-highmem-16/host-profile-$_gcp_instance.json"
mkdir -p "$(dirname -- "$_gcp_profile_manifest")"

_gcp_profile_info=$(
    PROFILE_MANIFEST="$_gcp_profile_manifest" PROFILE_INSTANCE="$_gcp_instance" python3 - <<'PY'
import datetime
import glob
import json
import os
import pathlib
import re
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
    print("No online CPU list found in sysfs", file=sys.stderr)
    sys.exit(1)

topology = {}
for cpu in sorted(online):
    base = f"/sys/devices/system/cpu/cpu{cpu}/topology"
    package = read_text(f"{base}/physical_package_id")
    core = read_text(f"{base}/core_id")
    siblings = parse_cpu_list(read_text(f"{base}/thread_siblings_list"))
    if package is None or core is None:
        print(f"Missing physical topology for CPU {cpu}", file=sys.stderr)
        sys.exit(1)
    topology[cpu] = {
        "physical_package_id": package,
        "core_id": core,
        "thread_siblings": sorted(siblings),
    }

domains = {}
for cache_dir in sorted(glob.glob("/sys/devices/system/cpu/cpu[0-9]*/cache/index*")):
    if read_text(f"{cache_dir}/level") != "3":
        continue
    shared_raw = read_text(f"{cache_dir}/shared_cpu_list")
    if not shared_raw:
        continue
    shared = parse_cpu_list(shared_raw) & online
    if not shared:
        continue
    key = tuple(sorted(shared))
    domain = domains.setdefault(key, {
        "shared_cpu_list": shared_raw,
        "size": read_text(f"{cache_dir}/size"),
        "cpus": sorted(shared),
        "cores": {},
    })
    for cpu in sorted(shared):
        info = topology[cpu]
        physical_key = (info["physical_package_id"], info["core_id"])
        domain["cores"].setdefault(physical_key, cpu)

candidates = []
for domain in domains.values():
    representatives = sorted(domain["cores"].values())
    if len(representatives) >= 8:
        candidates.append((domain, representatives))
if not candidates:
    print("No single L3 domain exposes eight distinct physical cores", file=sys.stderr)
    sys.exit(1)

candidates.sort(key=lambda item: (-len(item[1]), item[1]))
selected_domain, representatives = candidates[0]
benchmark_cpus = representatives[:8]
single_core = next((cpu for cpu in benchmark_cpus if cpu != 0), benchmark_cpus[0])
benchmark_cores = ",".join(str(cpu) for cpu in benchmark_cpus)
domain_record = {
    "shared_cpu_list": selected_domain["shared_cpu_list"],
    "size": selected_domain["size"],
    "selected_benchmark_cpus": benchmark_cpus,
    "selected_physical_cores": [
        {"cpu": cpu, **topology[cpu]} for cpu in benchmark_cpus
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
    "host_profile": "gcp-c3d-highmem-16",
    "instance": os.environ.get("PROFILE_INSTANCE"),
    "zone": gce_zone(),
    "source": "Linux sysfs at profile load time",
    "online_cpus": sorted(online),
    "all_l3_domains": [
        {
            "shared_cpu_list": domain["shared_cpu_list"],
            "size": domain["size"],
            "online_cpus": domain["cpus"],
            "physical_core_count": len(domain["cores"]),
        }
        for domain in domains.values()
    ],
    "selected_l3_domain": domain_record,
}
manifest_path = pathlib.Path(os.environ["PROFILE_MANIFEST"])
manifest_path.parent.mkdir(parents=True, exist_ok=True)
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

print(f"{single_core}|{benchmark_cores}|{benchmark_cores}|{selected_domain['shared_cpu_list']}")
PY
) || {
    printf '%s\n' '[ERROR] Could not derive an eight-core L3 domain from sysfs.' >&2
    return 1
}

_gcp_single_core=
_gcp_benchmark_cores=
_gcp_allowed_cores=
_gcp_selected_l3_domain=
IFS='|' read -r _gcp_single_core _gcp_benchmark_cores _gcp_allowed_cores _gcp_selected_l3_domain <<EOF
$_gcp_profile_info
EOF
if [ -z "$_gcp_single_core" ] || [ -z "$_gcp_benchmark_cores" ] || \
    [ -z "$_gcp_allowed_cores" ] || [ -z "$_gcp_selected_l3_domain" ]; then
    printf '%s\n' '[ERROR] GCP host profile returned incomplete topology data.' >&2
    return 1
fi
HOST_DEFAULT_SINGLE_CORE=$_gcp_single_core
HOST_DEFAULT_CORES=$_gcp_benchmark_cores
HOST_ALLOWED_CORES=$_gcp_allowed_cores

printf '[INFO] GCP core profile: single=%s, eight-core=%s, L3 shared CPUs=%s\n' \
    "$HOST_DEFAULT_SINGLE_CORE" "$HOST_DEFAULT_CORES" "$_gcp_selected_l3_domain" >&2
printf '[INFO] Derived topology recorded in %s\n' "$_gcp_profile_manifest" >&2
}

_gcp_load_host_profile

# shellcheck disable=SC2317 # Called by reproduce/common.sh after sourcing this profile.
host_check_mode() {
    printf '[INFO] Host profile: %s (cores and L3 domain discovered from sysfs)\n' "$HOST_NAME"
}
