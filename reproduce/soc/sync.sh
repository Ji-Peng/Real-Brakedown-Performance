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

# Workstation side: copies the committed tree at HEAD (never uncommitted
# changes) to the cluster clone <root>/code and checks it out detached at the
# same commit. The transfer is a git bundle of HEAD and every tag, so the
# clone has the history and the step/* tags that reproduce/steps.sh builds,
# and run directories are named after the same commit id as here. Results
# already in the clone (untracked) are kept.
#
# Refuses to move the clone while one of this project's Slurm jobs (name
# rbd-*) is pending or running, because those jobs run from the clone.
# Uses RBD_SSH, RBD_LOGIN and RBD_REMOTE_ROOT (see lib.sh).

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib.sh"

FORCE=0
usage() {
    cat <<EOF
Usage: $0 [--force]

Copies HEAD ($(git -C "$CODE_DIR" rev-parse --short=12 HEAD 2>/dev/null || echo '?')) and all tags to
\$RBD_LOGIN:~/\$RBD_REMOTE_ROOT/code ($RBD_LOGIN:~/$RBD_REMOTE_ROOT/code) and checks it out.

  --force   move the clone even if rbd-* Slurm jobs are pending or running
EOF
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --force) FORCE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

SHA=$(git -C "$CODE_DIR" rev-parse HEAD)
if [ -n "$(git -C "$CODE_DIR" status --porcelain --untracked-files=no)" ]; then
    soc_log "note: the working tree has uncommitted changes; only commit ${SHA%"${SHA#????????????}"} is copied"
fi
TMP=$(mktemp -d "${TMPDIR:-/tmp}/rbd-sync.XXXXXX")
trap 'rm -rf "$TMP"' EXIT INT TERM
BUNDLE="$TMP/code.bundle"
git -C "$CODE_DIR" bundle create "$BUNDLE" HEAD --tags >/dev/null 2>&1
git -C "$CODE_DIR" bundle verify "$BUNDLE" >/dev/null 2>&1 || soc_die "bundle verification failed"
soc_log "bundle $(wc -c < "$BUNDLE" | tr -d ' ') bytes, commit $SHA"

# The remote script receives the bundle on stdin; arguments: root, commit, force.
REMOTE_SCRIPT='set -eu
root=$HOME/$1; sha=$2; force=$3
mkdir -p "$root/jobs"
if [ "$force" != 1 ] && command -v squeue >/dev/null 2>&1; then
    active=$(squeue -h -u "$USER" -o "%i %j %T" | awk "\$2 ~ /^rbd-/" || true)
    if [ -n "$active" ]; then
        printf "[soc] ERROR: rbd-* jobs are queued or running (they use the clone):\n%s\n" "$active" >&2
        exit 3
    fi
fi
bundle=$root/jobs/sync-$sha.bundle
cat > "$bundle"
git bundle verify "$bundle" >/dev/null 2>&1 || { echo "[soc] ERROR: bundle damaged in transfer" >&2; exit 1; }
if [ ! -d "$root/code/.git" ]; then
    git clone --quiet --no-checkout "$bundle" "$root/code"
fi
git -C "$root/code" fetch --quiet --tags --force "$bundle" "+HEAD:refs/remotes/sync/head"
git -C "$root/code" checkout --quiet --force --detach "$sha"
git -C "$root/code" clean -fdq -e results/ -e "target*/"
rm -f "$bundle"
printf "[soc] cluster clone %s at %s\n" "$root/code" "$(git -C "$root/code" rev-parse --short=12 HEAD)"
git -C "$root/code" status --short --untracked-files=no | head -5'

soc_remote_sh "$REMOTE_SCRIPT" "$RBD_REMOTE_ROOT" "$SHA" "$FORCE" < "$BUNDLE"
