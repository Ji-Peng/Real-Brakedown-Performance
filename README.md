# Real Brakedown Performance (LCPC fork)

This repository is a fork of [LCPC](https://github.com/conroi/lcpc) by Riad S. Wahby
(Apache-2.0, see `LICENSE`), imported with its full history at upstream commit
`f0e2116001af5a1bbf39481535f837d6a25772e8`. The original upstream README is kept
unchanged in [`README-lcpc.md`](README-lcpc.md).

The fork integrates the scalar/software optimizations analysed in *High-Performance
SIMD Software for Spielman Codes in Zero-Knowledge Proofs* (Krieger, Dobrouschek,
Hirner, Sinha Roy; TCHES 2026) into LCPC's Brakedown (SDIG) encoder, without explicit
SIMD: portable stable Rust, compiler-generated scalar code. Each technique is added as
one tagged commit (`step/NN-<name>`) on top of the import (`step/00-import`), so the
history itself documents what changed relative to upstream.

Build, test, and reproduction instructions will be added as the work progresses.

## Benchmark machines (Google Cloud)

All published numbers come from Google Cloud VMs created with the `gcloud` CLI. A
local workstation is used only for building and testing; no published number comes
from it. The scripts in `reproduce/gcp/` wrap exactly the commands below; if the two
ever disagree, the commands in this section are the reference configuration.

### Configuration

| Setting | Primary x86-64 machine | Arm portability machine |
| :--- | :--- | :--- |
| Instance name | `rbd-bench-c3d` | `rbd-bench-c4a` |
| Machine type | `c3d-highmem-30` | `c4a-highmem-16` |
| CPU | AMD EPYC 9004 "Genoa" (Zen 4), AVX-512 incl. IFMA | Google Axion (Arm Neoverse V2) |
| vCPUs / cores | 30 vCPUs → 15 physical cores with `--threads-per-core=1` (SMT off) | 16 vCPUs = 16 cores (no SMT) |
| Memory | 240 GB | 128 GB |
| Zone | `us-central1-a` | `us-central1-a` |
| Image | `debian-12` from `debian-cloud` (x86-64) | `debian-12-arm64` from `debian-cloud` |
| Boot disk | 100 GB `pd-balanced` | 100 GB `hyperdisk-balanced` (C4A uses Hyperdisk) |
| Provisioning | on-demand (`STANDARD`), `--maintenance-policy=TERMINATE` (no live migration) | same |
| Used for | All implementations (this work, original LCPC, PolyCommit, SIMD-Spielman), 2 independent sessions | This work and original LCPC only (SIMD-Spielman needs AVX-512), 1 short session, log2 N in {18, 22, 26} |

Checked on 2026-10-21 with Google Cloud SDK 590.0.0: both machine types are offered
in `us-central1-a` (`c3d-highmem-30`: 30 vCPUs / 240 GB; `c4a-highmem-16`: 16 vCPUs /
128 GB). The images resolved to `debian-12-bookworm-v20261014` (X86_64) and
`debian-12-bookworm-arm64-v20261014` (ARM64). The manifest of every run records the
image, zone, CPU model, and guest cache topology that were actually used.

Fallback if `c3d` is unavailable: `c4-highmem-32` (Intel Xeon, 16 physical cores with
`--threads-per-core=1`, `hyperdisk-balanced` boot disk). One machine family is used for
a whole campaign; results from different families are never mixed in one table.

### Create the VMs

Authenticate and select the project first (`gcloud auth login`,
`gcloud config set project <PROJECT>`). Creating a VM starts billing.

```sh
# Primary x86-64 benchmark machine
gcloud compute instances create rbd-bench-c3d \
  --zone=us-central1-a \
  --machine-type=c3d-highmem-30 \
  --threads-per-core=1 \
  --image-family=debian-12 --image-project=debian-cloud \
  --boot-disk-size=100GB --boot-disk-type=pd-balanced \
  --provisioning-model=STANDARD --maintenance-policy=TERMINATE

# Arm portability machine
gcloud compute instances create rbd-bench-c4a \
  --zone=us-central1-a \
  --machine-type=c4a-highmem-16 \
  --image-family=debian-12-arm64 --image-project=debian-cloud \
  --boot-disk-size=100GB --boot-disk-type=hyperdisk-balanced \
  --provisioning-model=STANDARD --maintenance-policy=TERMINATE
```

Optional hardware performance counters: if the machine type supports a guest PMU, add
`--performance-monitoring-unit=enhanced` (core/L2 and LLC events) to collect `perf stat`
counters. If creation fails with that flag, create the VM without it and record that
counters were unavailable.

### First-boot checks

Run these on each VM; `reproduce/gcp/setup_vm.sh` saves them in the run manifest.

```sh
lscpu                                    # model; c3d: 15 cores, 1 thread per core
grep -o -m1 avx512ifma /proc/cpuinfo     # c3d only: required by SIMD-Spielman
cat /sys/devices/system/cpu/cpu*/cache/index3/shared_cpu_list | sort -u   # L3 domains
nproc; free -g; uname -a; cat /etc/os-release
```

Use one fixed core for 1-thread runs, and 8 distinct physical cores in one reported L3
domain for 8-thread runs. Leave the remaining cores for the OS and the harness.

### Stop or delete the VMs

Stop or delete the VMs as soon as no benchmark session is running:

```sh
gcloud compute instances stop   rbd-bench-c3d --zone=us-central1-a   # keeps the disk
gcloud compute instances delete rbd-bench-c3d --zone=us-central1-a   # removes VM and boot disk
gcloud compute instances stop   rbd-bench-c4a --zone=us-central1-a
gcloud compute instances delete rbd-bench-c4a --zone=us-central1-a
```

A stopped VM no longer bills for vCPUs and memory, but its disk is still billed.
