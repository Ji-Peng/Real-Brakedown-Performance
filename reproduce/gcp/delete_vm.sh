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
ACTION=delete
QUIET=0

usage() {
    cat <<EOF
Usage: $0 [--machine c3d|c4a] [--stop] [--yes] [--instance NAME] [--zone ZONE]

By default, deletes the VM and boot disk.
  --stop              Stops the VM and retains its boot disk (prevents CPU/RAM billing)
  --machine c3d|c4a   Target machine family (default: c3d; selects rbd-bench-c3d or rbd-bench-c4a)
  --instance NAME     Override instance name
  --zone ZONE         GCP zone (default: us-central1-c for c3d, us-central1-a for c4a)
  --yes               Passes --quiet to gcloud to skip interactive confirmation
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
        --stop)
            ACTION=stop
            shift
            ;;
        --yes)
            QUIET=1
            shift
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

if [ -z "$INSTANCE" ]; then
    [ -n "$ZONE" ] || { if [ "$MACHINE" = c3d ]; then ZONE=us-central1-c; else ZONE=us-central1-a; fi; }
    case "$MACHINE" in
        c3d) INSTANCE=rbd-bench-c3d ;;
        c4a) INSTANCE=rbd-bench-c4a ;;
        *)
            printf 'Invalid --machine: %s (must be c3d or c4a)\n' "$MACHINE" >&2
            exit 2
            ;;
    esac
fi

case "$INSTANCE" in
    ''|*[!A-Za-z0-9._-]*) printf 'Invalid instance name: %s\n' "$INSTANCE" >&2; exit 2 ;;
esac
case "$ZONE" in
    ''|*[!A-Za-z0-9._-]*) printf 'Invalid zone: %s\n' "$ZONE" >&2; exit 2 ;;
esac

printf 'Command to %s the VM:\n  %s compute instances %s %s --zone=%s' \
    "$ACTION" "$GCLOUD" "$ACTION" "$INSTANCE" "$ZONE"
if [ "$QUIET" -eq 1 ]; then
    printf ' --quiet'
fi
printf '\n'

if ! command -v "$GCLOUD" >/dev/null 2>&1; then
    printf 'gcloud command not found: %s\n' "$GCLOUD" >&2
    printf '%s\n' 'Set GCLOUD="$HOME/.local/share/google-cloud-sdk/bin/gcloud" if needed.' >&2
    exit 127
fi

if [ "$QUIET" -eq 1 ]; then
    "$GCLOUD" compute instances "$ACTION" "$INSTANCE" --zone="$ZONE" --quiet
else
    "$GCLOUD" compute instances "$ACTION" "$INSTANCE" --zone="$ZONE"
fi
