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

# Workstation side: copies session results from the cluster clone
# (<root>/code/results/<host>/<run>/<session>) into results/<host>/... here,
# with the per-node host profiles and first-run records of that host.
# Whole session directories are copied (stages/, failed_blocks.tsv and the
# <block>.prev-<UTC> directories included; analysis skips the latter). By
# default the codegen disassembly listings stay on the cluster and every
# codegen_report.json is stored xz-compressed (analyze.py reads *.json.xz).
# Uses RBD_SSH, RBD_LOGIN and RBD_REMOTE_ROOT (see lib.sh).

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib.sh"

DEST="$CODE_DIR/results"
HOST=
RUN='*'
SESSION='*'
LIST=0
LISTINGS=0
COMPRESS=1

usage() {
    cat <<EOF
Usage: $0 --host soc-xcnh|soc-xcng [--run RUN] [--session S] [--dest DIR] [options]
       $0 --list

  --host H          host directory under results/ (soc-xcnh or soc-xcng)
  --run RUN         run directory <UTC date>-<commit> or a glob (default: all)
  --session S       session id or a glob (default: all)
  --dest DIR        local results root (default: $CODE_DIR/results)
  --list            list the remote run and session directories and exit
  --with-listings   also copy the codegen disassembly listings (disassembly.txt)
  --no-xz           keep codegen_report.json uncompressed
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host|--run|--session|--dest)
            [ "$#" -ge 2 ] || soc_die "$1 needs a value"
            case "$1" in
                --host) HOST=$2 ;;
                --run) RUN=$2 ;;
                --session) SESSION=$2 ;;
                --dest) DEST=$2 ;;
            esac
            shift 2
            ;;
        --list) LIST=1; shift ;;
        --with-listings) LISTINGS=1; shift ;;
        --no-xz) COMPRESS=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

case "$RBD_REMOTE_ROOT$RUN$SESSION" in
    *[!]A-Za-z0-9._*?/[-]*) soc_die "--run, --session and RBD_REMOTE_ROOT may contain only A-Z a-z 0-9 . _ - / and the glob characters * ? [ ]" ;;
esac

if [ "$LIST" -eq 1 ]; then
    soc_remote_sh 'cd "$HOME/$1/code/results" || exit 3
for s in soc-*/*/*/schedule.json; do [ -f "$s" ] && dirname "$s"; done; exit 0' "$RBD_REMOTE_ROOT"
    exit 0
fi
case "$HOST" in
    soc-xcnh|soc-xcng) ;;
    *) usage >&2; exit 2 ;;
esac

mkdir -p "$DEST"
# Remote arguments: root, host, run glob, session glob, listings flag.
# Session directories are those holding a session-level schedule.json.
REMOTE_SCRIPT='set -eu
cd "$HOME/$1/code/results"
paths=
for s in $2/$3/$4/schedule.json; do
    [ -f "$s" ] && paths="$paths ${s%/schedule.json}"
done
[ -n "$paths" ] || { echo "[soc] ERROR: no session matches $2/$3/$4" >&2; exit 3; }
for f in $2/host-profile-*.json $2/first-run-*.json; do
    [ -f "$f" ] && paths="$paths $f"
done
echo "[soc] sending:$paths" >&2
if [ "$5" = 1 ]; then
    tar -cf - $paths
else
    tar -cf - --exclude=disassembly.txt $paths
fi'

soc_log "fetching $HOST/$RUN/$SESSION into $DEST"
soc_remote_sh "$REMOTE_SCRIPT" "$RBD_REMOTE_ROOT" "$HOST" "$RUN" "$SESSION" "$LISTINGS" | tar -xf - -C "$DEST"

if [ "$COMPRESS" -eq 1 ]; then
    command -v xz >/dev/null 2>&1 || soc_die "xz not found (use --no-xz)"
    find "$DEST/$HOST" -name codegen_report.json -type f -exec xz -9 -f {} \;
fi
soc_log "done; sessions under $DEST/$HOST"
