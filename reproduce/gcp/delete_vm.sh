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
ACTION=delete
QUIET=0

usage() {
    cat <<EOF
Usage: $0 [--stop] [--yes] [--instance NAME] [--zone ZONE]

By default, deletes the VM. --stop stops it and retains its boot disk.
--yes passes --quiet to gcloud to skip its confirmation prompt.
Defaults: instance=$INSTANCE, zone=$ZONE.

If gcloud is not on PATH, set GCLOUD to
  \$HOME/.local/share/google-cloud-sdk/bin/gcloud
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
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
