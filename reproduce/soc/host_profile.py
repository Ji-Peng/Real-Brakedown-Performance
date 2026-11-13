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

"""Benchmark-core selection and node record for the SoC Slurm cluster.

Used by reproduce/hosts/soc-xcnh.sh and soc-xcng.sh. The cores are derived
from what the Slurm job may use, never from all online CPUs of the shared node:

  * usable CPUs  = the process's CPU affinity (one hardware thread per core
                   with --threads-per-core=1);
  * owned CPUs   = the job's cgroup cpuset (it also lists the SMT siblings of
                   the allocated cores; falls back to the affinity).

xcnh (AMD EPYC, L3 shared by 8 cores): the benchmark set is the 8 physical
cores of one L3 domain whose CPUs, siblings included, the job owns entirely,
so no other job runs inside that L3 domain. xcng (Arm Neoverse-N1, no L3
entry in sysfs): the first 8 usable cores.

Modes:
  --manifest PATH   write the node record (JSON) and print
                    'single|benchmark|allowed|domain' on stdout
  --noise           print a one-line JSON noise snapshot (load average,
                    CPUs allocated on this node, of which this job holds
                    job_cpus_on_node, and its running jobs)
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

BENCH_CORES = 8


def read_text(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def run(cmd, timeout=20):
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=True)
        return res.stdout.strip()
    except (OSError, subprocess.SubprocessError):
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


def format_cpu_list(cpus):
    cpus = sorted(cpus)
    out = []
    i = 0
    while i < len(cpus):
        j = i
        while j + 1 < len(cpus) and cpus[j + 1] == cpus[j] + 1:
            j += 1
        out.append(str(cpus[i]) if i == j else f"{cpus[i]}-{cpus[j]}")
        i = j + 1
    return ",".join(out)


def cgroup_cpuset():
    """Effective cpuset of this process's cgroup (v2, else v1), or None."""
    text = read_text("/proc/self/cgroup") or ""
    for line in text.splitlines():
        parts = line.split(":", 2)
        if len(parts) != 3:
            continue
        hier, controllers, path = parts
        if hier == "0" and controllers == "":
            candidate = f"/sys/fs/cgroup{path}/cpuset.cpus.effective"
        elif "cpuset" in controllers.split(","):
            candidate = f"/sys/fs/cgroup/cpuset{path}/cpuset.effective_cpus"
        else:
            continue
        value = read_text(candidate)
        if value:
            return parse_cpu_list(value), candidate
    return None, None


def hostname():
    return platform.node().split(".")[0] or "unknown-host"


def scontrol_node(node):
    out = run(["scontrol", "show", "node", "-o", node])
    fields = {}
    if out:
        for token in out.split():
            if "=" in token:
                key, value = token.split("=", 1)
                fields[key] = value
    return fields


def noise_snapshot():
    node = hostname()
    fields = scontrol_node(node)
    jobs = run(["squeue", "-h", "-t", "R", "-w", node, "-o", "%i"])
    load = read_text("/proc/loadavg")

    def as_int(value):
        try:
            return int(value)
        except (TypeError, ValueError):
            return None

    def as_float(value):
        try:
            return float(value)
        except (TypeError, ValueError):
            return None

    return {
        "utc": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "node": node,
        "loadavg_1_5_15": [float(x) for x in load.split()[:3]] if load else None,
        "node_cpu_alloc": as_int(fields.get("CPUAlloc")),
        "job_cpus_on_node": as_int(os.environ.get("SLURM_CPUS_ON_NODE")),
        "node_cpu_total": as_int(fields.get("CPUTot")),
        "node_cpu_load": as_float(fields.get("CPULoad")),
        "node_running_jobs": len(jobs.splitlines()) if jobs is not None else None,
    }


def lscpu_values():
    values = {}
    for line in (run(["lscpu"]) or "").splitlines():
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
    # perf stat of an unprivileged process needs perf_event_paranoid <= 2.
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


def select_cores(node_class):
    online = parse_cpu_list(read_text("/sys/devices/system/cpu/online"))
    if not online:
        raise SystemExit("No online CPU list in sysfs")
    try:
        usable = set(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        usable = set(online)
    owned, cgroup_file = cgroup_cpuset()
    if not owned:
        owned = set(usable)
    owned |= usable

    topology = {}
    for cpu in sorted(online):
        base = f"/sys/devices/system/cpu/cpu{cpu}/topology"
        topology[cpu] = (
            read_text(f"{base}/physical_package_id") or "0",
            read_text(f"{base}/core_id") or str(cpu),
            sorted(parse_cpu_list(read_text(f"{base}/thread_siblings_list")) or {cpu}),
        )

    def numa_node(cpu):
        nodes = glob.glob(f"/sys/devices/system/cpu/cpu{cpu}/node[0-9]*")
        return int(os.path.basename(nodes[0])[4:]) if nodes else None

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

    record = {
        "usable_cpus_affinity": format_cpu_list(usable),
        "owned_cpus_cgroup": format_cpu_list(owned),
        "cgroup_cpuset_file": cgroup_file,
        "l3_domains_on_node": len(domains),
    }

    if node_class == "xcnh":
        if not domains:
            raise SystemExit("No L3 cache domains in sysfs; cannot select an L3 domain")
        candidates = []
        for key, domain in domains.items():
            cpus = set(domain["cpus"])
            if not cpus <= owned:
                continue
            cores = {}
            for cpu in sorted(cpus):
                package, core, _ = topology[cpu]
                if cpu in usable:
                    cores.setdefault((package, core), cpu)
            representatives = sorted(cores.values())
            if len(representatives) >= BENCH_CORES:
                candidates.append((representatives[0], domain, representatives))
        if not candidates:
            partial = [
                {"shared_cpu_list": d["shared_cpu_list"],
                 "owned": format_cpu_list(set(d["cpus"]) & owned)}
                for d in domains.values() if set(d["cpus"]) & owned
            ]
            raise SystemExit(
                "No L3 domain with %d physical cores is owned entirely by this job "
                "(owned CPUs %s; partly owned domains %s). Request whole L3 domains "
                "(-c 16 --threads-per-core=1) on an idle node or pin another node with "
                "--nodelist." % (BENCH_CORES, format_cpu_list(owned), json.dumps(partial)))
        candidates.sort(key=lambda item: item[0])
        _, domain, representatives = candidates[0]
        bench = representatives[:BENCH_CORES]
        record["selection_rule"] = (
            "first L3 domain (lowest CPU id) whose CPUs, SMT siblings included, all belong to the "
            "job's cgroup cpuset; one usable CPU per physical core, the first eight")
        record["selected_l3_domain"] = {
            "shared_cpu_list": domain["shared_cpu_list"],
            "size": domain["size"],
            "fully_owned_by_job": True,
        }
        domain_label = domain["shared_cpu_list"]
    else:
        cores = {}
        for cpu in sorted(usable):
            package, core, _ = topology[cpu]
            cores.setdefault((package, core), cpu)
        representatives = sorted(cores.values())
        if len(representatives) < BENCH_CORES:
            raise SystemExit("The job may use only %d physical cores; %d are needed"
                             % (len(representatives), BENCH_CORES))
        bench = representatives[:BENCH_CORES]
        record["selection_rule"] = "first eight usable physical cores (no level-3 cache entry in sysfs)"
        record["selected_l3_domain"] = None
        record["l3_note"] = ("sysfs exposes no level-3 cache on this node class; the eight "
                             "cores are not selected by cache domain") if not domains else None
        domain_label = "none"

    single = next((cpu for cpu in bench if cpu != 0), bench[0])
    record["benchmark_cpus"] = bench
    record["single_thread_core"] = single
    record["selected_physical_cores"] = [
        {"cpu": cpu, "physical_package_id": topology[cpu][0], "core_id": topology[cpu][1],
         "thread_siblings": topology[cpu][2], "numa_node": numa_node(cpu)}
        for cpu in bench
    ]
    spare = sorted(usable - set(bench))
    record["spare_usable_cpus"] = format_cpu_list(spare)
    return record, single, bench, usable, domain_label


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--node-class", choices=["xcnh", "xcng"])
    parser.add_argument("--host-profile")
    parser.add_argument("--manifest")
    parser.add_argument("--noise", action="store_true")
    args = parser.parse_args()

    if args.noise:
        print(json.dumps(noise_snapshot(), separators=(",", ":")))
        return
    if not args.node_class or not args.manifest:
        parser.error("--node-class and --manifest are required unless --noise is given")

    try:
        selection, single, bench, usable, domain_label = select_cores(args.node_class)
    except SystemExit as exc:
        print(f"[ERROR] {exc}", file=sys.stderr)
        sys.exit(1)

    lscpu = lscpu_values()
    node = hostname()
    node_fields = scontrol_node(node)
    env = os.environ
    manifest = {
        "schema_version": "soc-host-profile-v1",
        "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host_profile": args.host_profile,
        "node_class": args.node_class,
        "hostname": node,
        "slurm": {
            "job_id": env.get("SLURM_JOB_ID"),
            "step_id": env.get("SLURM_STEP_ID"),
            "job_name": env.get("SLURM_JOB_NAME"),
            "partition": env.get("SLURM_JOB_PARTITION"),
            "nodelist": env.get("SLURM_JOB_NODELIST"),
            "cpus_per_task": env.get("SLURM_CPUS_PER_TASK"),
            "mem_per_node_mb": env.get("SLURM_MEM_PER_NODE"),
            "cpu_freq_request": env.get("SLURM_CPU_FREQ_REQ"),
            "cluster": env.get("SLURM_CLUSTER_NAME"),
        },
        "node": {
            "cpu_model": lscpu.get("Model name"),
            "architecture": platform.machine(),
            "sockets": lscpu.get("Socket(s)"),
            "cores_per_socket": lscpu.get("Core(s) per socket"),
            "threads_per_core": lscpu.get("Thread(s) per core"),
            "online_cpus": read_text("/sys/devices/system/cpu/online"),
            "numa_nodes": lscpu.get("NUMA node(s)"),
            "scontrol_cpu_alloc": node_fields.get("CPUAlloc"),
            "scontrol_cpu_total": node_fields.get("CPUTot"),
            "scontrol_cpu_load": node_fields.get("CPULoad"),
            "scontrol_real_memory_mb": node_fields.get("RealMemory"),
            "scontrol_alloc_memory_mb": node_fields.get("AllocMem"),
            "scontrol_state": node_fields.get("State"),
            "scontrol_features": node_fields.get("AvailableFeatures"),
            "kernel": platform.release(),
            "os_release": (read_text("/etc/os-release") or "").splitlines()[:2],
        },
        "noise_at_profile_load": noise_snapshot(),
        "core_selection": selection,
        "frequency": {
            "boost_raw": read_text("/sys/devices/system/cpu/cpufreq/boost"),
            "boost_note": "turbo/boost is a node setting and cannot be disabled by the job",
            "benchmark_cpus": cpu_freq_state(bench),
        },
        "perf": perf_record(),
        "avx512ifma_present": "avx512ifma" in (read_text("/proc/cpuinfo") or ""),
    }
    path = pathlib.Path(args.manifest)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, path)
    print("|".join([str(single), ",".join(map(str, bench)), format_cpu_list(usable), domain_label]))


if __name__ == "__main__":
    main()
