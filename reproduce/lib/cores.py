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

"""Portable CPU-affinity and cache-topology core selector for RBP-LCPC.

Derives benchmark cores from the process CPU affinity mask (sched_getaffinity)
and Linux sysfs physical-core / L3 cache topology. External restrictions such
as taskset, container cpusets, or job-scheduler cgroups therefore compose
automatically without machine-specific configuration.

Default stdout format:
    single|multi|allowed|l3_domain|cache_aware
"""

import argparse
import datetime
import glob
import json
import os
import pathlib
import platform
import shutil
import subprocess
import sys


def read_text(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def run_cmd(cmd, timeout=20):
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=True)
        return res.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None


def parse_cpu_list(value):
    cpus = set()
    if not value:
        return cpus
    for part in str(value).split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            first, last = part.split("-", 1)
            cpus.update(range(int(first), int(last) + 1))
        else:
            cpus.add(int(part))
    return cpus


def format_cpu_list(cpus):
    cpus = sorted(set(cpus))
    out = []
    i = 0
    while i < len(cpus):
        j = i
        while j + 1 < len(cpus) and cpus[j + 1] == cpus[j] + 1:
            j += 1
        out.append(str(cpus[i]) if i == j else f"{cpus[i]}-{cpus[j]}")
        i = j + 1
    return ",".join(out)


def hostname_short():
    raw = platform.node().split(".")[0] or "unknown-host"
    return "".join(ch if (ch.isalnum() or ch in "._-") else "_" for ch in raw)


def lscpu_values():
    values = {}
    for line in (run_cmd(["lscpu"]) or "").splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            values[key.strip()] = value.strip()
    return values


def perf_record():
    paranoid_raw = read_text("/proc/sys/kernel/perf_event_paranoid")
    try:
        paranoid = int(paranoid_raw) if paranoid_raw is not None else None
    except ValueError:
        paranoid = None
    perf_bin = shutil.which("perf")
    available = perf_bin is not None and paranoid is not None and paranoid <= 2
    if available:
        note = "perf stat available to this user"
    elif perf_bin is None:
        note = "unavailable: no perf binary"
    else:
        note = f"unavailable: kernel.perf_event_paranoid = {paranoid_raw} (> 2) blocks unprivileged PMU counters"
    return {
        "perf_event_paranoid": paranoid,
        "perf_binary": perf_bin,
        "available": available,
        "status": "available" if available else "unavailable",
        "note": note,
    }


def cpu_freq_state(cpus):
    state = {}
    for cpu in cpus:
        base = f"/sys/devices/system/cpu/cpu{cpu}/cpufreq"
        state[str(cpu)] = {
            "scaling_governor": read_text(f"{base}/scaling_governor"),
            "scaling_driver": read_text(f"{base}/scaling_driver"),
            "scaling_cur_freq_khz": read_text(f"{base}/scaling_cur_freq"),
            "scaling_max_freq_khz": read_text(f"{base}/scaling_max_freq"),
        }
    return state


def discover_topology():
    online = parse_cpu_list(read_text("/sys/devices/system/cpu/online"))
    try:
        usable = set(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        usable = set(online)
    if not online:
        online = set(usable)
    if not usable:
        raise SystemExit("No usable CPUs found in process affinity mask")

    topology = {}
    for cpu in sorted(online | usable):
        base = f"/sys/devices/system/cpu/cpu{cpu}/topology"
        topology[cpu] = (
            read_text(f"{base}/physical_package_id") or "0",
            read_text(f"{base}/core_id") or str(cpu),
            sorted(parse_cpu_list(read_text(f"{base}/thread_siblings_list")) or {cpu}),
        )

    domains = {}
    for cache_dir in sorted(glob.glob("/sys/devices/system/cpu/cpu[0-9]*/cache/index*")):
        if read_text(f"{cache_dir}/level") != "3":
            continue
        shared_raw = read_text(f"{cache_dir}/shared_cpu_list")
        shared = parse_cpu_list(shared_raw) & online
        if shared:
            domains.setdefault(tuple(sorted(shared)), {
                "shared_cpu_list": shared_raw,
                "size": read_text(f"{cache_dir}/size"),
                "cpus": sorted(shared),
            })

    return online, usable, topology, domains


def numa_node(cpu):
    nodes = glob.glob(f"/sys/devices/system/cpu/cpu{cpu}/node[0-9]*")
    return int(os.path.basename(nodes[0])[4:]) if nodes else None


def select_cores(max_threads=8, explicit_cores=None):
    online, usable, topology, domains = discover_topology()
    max_threads = max(1, int(max_threads))

    # Map each physical core (package, core_id) to its lowest usable hardware thread.
    usable_phys = {}
    for cpu in sorted(usable):
        pkg, core, _ = topology[cpu]
        usable_phys.setdefault((pkg, core), cpu)
    all_usable_reps = sorted(usable_phys.values())

    record = {
        "usable_cpus_affinity": format_cpu_list(usable),
        "usable_physical_cores": len(all_usable_reps),
        "l3_domains_on_node": len(domains),
        "max_threads_requested": max_threads,
    }

    if explicit_cores:
        req = sorted(parse_cpu_list(explicit_cores))
        if not req:
            raise SystemExit(f"Empty --cores list: {explicit_cores!r}")
        bench = req[:max_threads] if len(req) > max_threads else req
        # Check if all chosen cores fall in a single L3 domain.
        matching_domains = [d for d in domains.values() if set(bench) <= set(d["cpus"])]
        if matching_domains:
            dom = matching_domains[0]
            cache_aware = True
            l3_domain = dom["shared_cpu_list"]
            record["selected_l3_domain"] = {"shared_cpu_list": dom["shared_cpu_list"], "size": dom["size"]}
        else:
            cache_aware = False
            l3_domain = "none" if not domains else "multi"
            record["selected_l3_domain"] = None
        record["selection_rule"] = "explicit --cores list"
        single = bench[0]
    elif domains:
        full_candidates = []
        partial_candidates = []
        for dom in domains.values():
            dom_phys = {(topology[c][0], topology[c][1]) for c in dom["cpus"] if c in topology}
            reps = sorted(usable_phys[pc] for pc in dom_phys if pc in usable_phys)
            if not reps:
                continue
            all_usable = len(reps) == len(dom_phys) and len(dom_phys) > 0
            if all_usable and len(reps) >= max_threads:
                full_candidates.append((reps[0], dom, reps))
            partial_candidates.append((-len(reps), reps[0], dom, reps))

        if full_candidates:
            full_candidates.sort(key=lambda x: x[0])
            _, dom, reps = full_candidates[0]
            bench = reps[:max_threads]
            cache_aware = True
            l3_domain = dom["shared_cpu_list"]
            record["selection_rule"] = (
                "first L3 domain whose physical cores are all usable with >= max_threads cores; "
                "lowest usable hardware thread per physical core"
            )
            record["selected_l3_domain"] = {"shared_cpu_list": dom["shared_cpu_list"], "size": dom["size"]}
        elif partial_candidates:
            partial_candidates.sort(key=lambda x: (x[0], x[1]))
            _, _, dom, reps = partial_candidates[0]
            bench = list(reps[:max_threads])
            if len(bench) < max_threads:
                extras = [c for c in all_usable_reps if c not in bench]
                if extras:
                    bench.extend(extras[: max_threads - len(bench)])
                    cache_aware = False
                    l3_domain = "multi"
                    record["selection_rule"] = (
                        "fallback: best L3 domain plus additional usable physical cores from other domains "
                        "(spans multiple L3 domains)"
                    )
                    record["selected_l3_domain"] = None
                else:
                    cache_aware = True
                    l3_domain = dom["shared_cpu_list"]
                    record["selection_rule"] = (
                        "fallback: L3 domain with the most usable physical cores (single L3 domain)"
                    )
                    record["selected_l3_domain"] = {"shared_cpu_list": dom["shared_cpu_list"], "size": dom["size"]}
            else:
                cache_aware = True
                l3_domain = dom["shared_cpu_list"]
                record["selection_rule"] = (
                    "fallback: L3 domain with the most usable physical cores (single L3 domain)"
                )
                record["selected_l3_domain"] = {"shared_cpu_list": dom["shared_cpu_list"], "size": dom["size"]}
        else:
            bench = all_usable_reps[:max_threads]
            cache_aware = False
            l3_domain = "none"
            record["selection_rule"] = "usable physical cores (no matching L3 domain)"
            record["selected_l3_domain"] = None
        single = next((cpu for cpu in bench if cpu != 0), bench[0])
    else:
        bench = all_usable_reps[:max_threads]
        cache_aware = False
        l3_domain = "none"
        record["selection_rule"] = "first usable physical cores (no level-3 cache entry in sysfs)"
        record["selected_l3_domain"] = None
        single = next((cpu for cpu in bench if cpu != 0), bench[0])

    record["cache_aware"] = cache_aware
    record["l3_domain"] = l3_domain
    record["benchmark_cpus"] = bench
    record["single_thread_core"] = single
    record["mt_threads"] = min(max_threads, len(bench))
    record["selected_physical_cores"] = [
        {
            "cpu": cpu,
            "physical_package_id": topology[cpu][0],
            "core_id": topology[cpu][1],
            "thread_siblings": topology[cpu][2],
            "numa_node": numa_node(cpu),
        }
        for cpu in bench
        if cpu in topology
    ]
    record["spare_usable_cpus"] = format_cpu_list(sorted(usable - set(bench)))
    return record, single, bench, usable, l3_domain, cache_aware


def write_host_profile_manifest(path_str, host_label, selection, bench):
    lscpu = lscpu_values()
    node = hostname_short()
    manifest = {
        "schema_version": "rbd-host-profile-v1",
        "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host_label": host_label or node,
        "hostname": node,
        "node": {
            "cpu_model": lscpu.get("Model name"),
            "architecture": platform.machine(),
            "sockets": lscpu.get("Socket(s)"),
            "cores_per_socket": lscpu.get("Core(s) per socket"),
            "threads_per_core": lscpu.get("Thread(s) per core"),
            "online_cpus": read_text("/sys/devices/system/cpu/online"),
            "numa_nodes": lscpu.get("NUMA node(s)"),
            "kernel": platform.release(),
            "os_release": (read_text("/etc/os-release") or "").splitlines()[:2],
        },
        "core_selection": selection,
        "frequency": {
            "boost_raw": read_text("/sys/devices/system/cpu/cpufreq/boost"),
            "intel_no_turbo_raw": read_text("/sys/devices/system/cpu/intel_pstate/no_turbo"),
            "benchmark_cpus": cpu_freq_state(bench),
        },
        "perf": perf_record(),
        "avx512ifma_present": "avx512ifma" in (read_text("/proc/cpuinfo") or ""),
    }
    path = pathlib.Path(path_str)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, path)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--max-threads", type=int, default=8, help="Maximum benchmark threads / cores (default: 8)")
    parser.add_argument("--cores", default=None, help="Explicit core list override (e.g. 0-7)")
    parser.add_argument("--host-label", default=None, help="Host label recorded in the manifest")
    parser.add_argument("--manifest", default=None, help="Write host-profile JSON to this path")
    parser.add_argument("--json", action="store_true", help="Print full core selection JSON to stdout")
    args = parser.parse_args()

    try:
        selection, single, bench, usable, l3_domain, cache_aware = select_cores(
            max_threads=args.max_threads,
            explicit_cores=args.cores,
        )
    except SystemExit as exc:
        print(f"[ERROR] {exc}", file=sys.stderr)
        sys.exit(1)

    if args.manifest:
        write_host_profile_manifest(args.manifest, args.host_label, selection, bench)

    if args.json:
        payload = dict(selection)
        payload["single"] = single
        payload["multi"] = ",".join(map(str, bench))
        payload["allowed"] = format_cpu_list(usable)
        print(json.dumps(payload, indent=2))
    else:
        print("|".join([
            str(single),
            ",".join(map(str, bench)),
            format_cpu_list(usable),
            l3_domain,
            "true" if cache_aware else "false",
        ]))


if __name__ == "__main__":
    main()
