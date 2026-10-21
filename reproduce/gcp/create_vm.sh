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
PMU=0
CONFIRMED=0

usage() {
    cat <<EOF
Usage: $0 [--machine c3d|c4a] [--instance NAME] [--zone ZONE] [--pmu] [--yes]

Creates a Google Cloud benchmark VM as documented in code/README.md.
By default, prints the planned GCP command. The VM is created only when --yes is supplied.

Options:
  --machine c3d|c4a   Target machine family (default: c3d)
                        c3d: rbd-bench-c3d (c3d-highmem-16, pd-balanced, --threads-per-core=1)
                        c4a: rbd-bench-c4a (c4a-highmem-16, hyperdisk-balanced)
  --instance NAME     Override instance name
  --zone ZONE         GCP zone (default: us-central1-c for c3d, us-central1-a for c4a)
  --pmu               Enable guest hardware performance monitoring (--performance-monitoring-unit=enhanced)
                      with automatic fallback retry if unsupported
  --yes               Execute the gcloud command to create the VM
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
        --pmu)
            PMU=1
            shift
            ;;
        --yes)
            CONFIRMED=1
            shift
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
        MACHINE_TYPE=c3d-highmem-16
        IMAGE_FAMILY=debian-12
        IMAGE_PROJECT=debian-cloud
        BOOT_DISK_SIZE=100GB
        BOOT_DISK_TYPE=pd-balanced
        THREADS_PER_CORE=1
        ;;
    c4a)
        DEFAULT_INSTANCE=rbd-bench-c4a
        MACHINE_TYPE=c4a-highmem-16
        IMAGE_FAMILY=debian-12-arm64
        IMAGE_PROJECT=debian-cloud
        BOOT_DISK_SIZE=100GB
        BOOT_DISK_TYPE=hyperdisk-balanced
        THREADS_PER_CORE=""
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

printf 'Planned command to create the VM (%s on %s):\n' "$MACHINE" "$INSTANCE"
printf '  %s compute instances create %s \\\n' "$GCLOUD" "$INSTANCE"
printf '    --zone=%s \\\n' "$ZONE"
printf '    --machine-type=%s \\\n' "$MACHINE_TYPE"
if [ -n "$THREADS_PER_CORE" ]; then
    printf '    --threads-per-core=%s \\\n' "$THREADS_PER_CORE"
fi
printf '    --image-family=%s --image-project=%s \\\n' "$IMAGE_FAMILY" "$IMAGE_PROJECT"
printf '    --boot-disk-size=%s --boot-disk-type=%s \\\n' "$BOOT_DISK_SIZE" "$BOOT_DISK_TYPE"
printf '    --provisioning-model=STANDARD --maintenance-policy=TERMINATE'
if [ "$PMU" -eq 1 ]; then
    printf ' \\\n    --performance-monitoring-unit=enhanced\n'
    printf '  (Note: if enhanced PMU is unsupported, it will automatically retry without it)\n'
else
    printf '\n'
fi

if [ "$CONFIRMED" -ne 1 ]; then
    printf '%s\n' 'Not executed. Re-run with --yes to create this on-demand VM.' >&2
    exit 2
fi

if ! command -v "$GCLOUD" >/dev/null 2>&1; then
    printf 'gcloud command not found: %s\n' "$GCLOUD" >&2
    printf '%s\n' 'Set GCLOUD="$HOME/.local/share/google-cloud-sdk/bin/gcloud" if needed.' >&2
    exit 127
fi

do_create() {
    _pmu_arg=${1:-}
    if [ -n "$THREADS_PER_CORE" ]; then
        if [ -n "$_pmu_arg" ]; then
            "$GCLOUD" compute instances create "$INSTANCE" \
                --zone="$ZONE" \
                --machine-type="$MACHINE_TYPE" \
                --threads-per-core="$THREADS_PER_CORE" \
                --image-family="$IMAGE_FAMILY" \
                --image-project="$IMAGE_PROJECT" \
                --boot-disk-size="$BOOT_DISK_SIZE" \
                --boot-disk-type="$BOOT_DISK_TYPE" \
                --provisioning-model=STANDARD \
                --maintenance-policy=TERMINATE \
                "$_pmu_arg"
        else
            "$GCLOUD" compute instances create "$INSTANCE" \
                --zone="$ZONE" \
                --machine-type="$MACHINE_TYPE" \
                --threads-per-core="$THREADS_PER_CORE" \
                --image-family="$IMAGE_FAMILY" \
                --image-project="$IMAGE_PROJECT" \
                --boot-disk-size="$BOOT_DISK_SIZE" \
                --boot-disk-type="$BOOT_DISK_TYPE" \
                --provisioning-model=STANDARD \
                --maintenance-policy=TERMINATE
        fi
    else
        if [ -n "$_pmu_arg" ]; then
            "$GCLOUD" compute instances create "$INSTANCE" \
                --zone="$ZONE" \
                --machine-type="$MACHINE_TYPE" \
                --image-family="$IMAGE_FAMILY" \
                --image-project="$IMAGE_PROJECT" \
                --boot-disk-size="$BOOT_DISK_SIZE" \
                --boot-disk-type="$BOOT_DISK_TYPE" \
                --provisioning-model=STANDARD \
                --maintenance-policy=TERMINATE \
                "$_pmu_arg"
        else
            "$GCLOUD" compute instances create "$INSTANCE" \
                --zone="$ZONE" \
                --machine-type="$MACHINE_TYPE" \
                --image-family="$IMAGE_FAMILY" \
                --image-project="$IMAGE_PROJECT" \
                --boot-disk-size="$BOOT_DISK_SIZE" \
                --boot-disk-type="$BOOT_DISK_TYPE" \
                --provisioning-model=STANDARD \
                --maintenance-policy=TERMINATE
        fi
    fi
}

if [ "$PMU" -eq 1 ]; then
    printf 'Creating instance %s with guest PMU enabled...\n' "$INSTANCE"
    if ! do_create "--performance-monitoring-unit=enhanced"; then
        printf 'Creation with --performance-monitoring-unit=enhanced failed; retrying without PMU...\n' >&2
        do_create ""
    fi
else
    do_create ""
fi
