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

# Slurm batch script of one benchmark session, submitted by run_stages.sh:
#   sbatch ... job.sh <code dir> <job file>
# The batch step only launches the session as a job step (srun), because
# Slurm applies --cpu-freq=Performance to job steps, not to the batch step.
# The step checks that the clone is still at the submitted commit, writes the
# node's first-run record (setup.sh --record-only), then runs every stage of
# the job file with reproduce/all.sh --host soc-<class> and prints
# STAGE_<NAME>_EXIT=<code>. Each session has its own Cargo target directory
# <root>/target/<class>-<session>, so two sessions never share build trees.

set -u

if [ "${1:-}" != --step ]; then
    [ "$#" -eq 2 ] || { echo "usage: job.sh <code dir> <job file>" >&2; exit 2; }
    [ -n "${SLURM_JOB_ID:-}" ] || { echo "job.sh runs inside a Slurm job (see run_stages.sh)" >&2; exit 2; }
    exec srun -n1 -c "${SLURM_CPUS_PER_TASK:-1}" --threads-per-core=1 --cpu-freq=Performance \
        sh "$1/reproduce/soc/job.sh" --step "$1" "$2" </dev/null
fi

CODE_DIR=$2
JOB_FILE=$3
ROOT=$(CDPATH= cd -- "$CODE_DIR/.." && pwd)
# shellcheck disable=SC1091
. "$CODE_DIR/reproduce/soc/lib.sh"

SESSION='' NODE_CLASS='' PROFILE='' RUN_DATE='' WARMUP='' LCPC_VARIANTS='' COMMIT=''
STAGES_FILE=$(mktemp "${TMPDIR:-/tmp}/rbd-stages.XXXXXX")
trap 'rm -f "$STAGES_FILE"' EXIT
while IFS= read -r line; do
    case "$line" in
        session=*) SESSION=${line#*=} ;;
        node_class=*) NODE_CLASS=${line#*=} ;;
        profile=*) PROFILE=${line#*=} ;;
        run_date=*) RUN_DATE=${line#*=} ;;
        warmup=*) WARMUP=${line#*=} ;;
        lcpc_variants=*) LCPC_VARIANTS=${line#*=} ;;
        commit=*) COMMIT=${line#*=} ;;
        stage=*) printf '%s\n' "${line#stage=}" >> "$STAGES_FILE" ;;
        '') ;;
        *) soc_die "unknown line in $JOB_FILE: $line" ;;
    esac
done < "$JOB_FILE"
[ -n "$SESSION" ] && [ -n "$NODE_CLASS" ] && [ -n "$PROFILE" ] && [ -n "$RUN_DATE" ] && \
    [ -n "$WARMUP" ] && [ -n "$LCPC_VARIANTS" ] && [ -n "$COMMIT" ] || soc_die "incomplete job file $JOB_FILE"

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

soc_toolchain_env "$ROOT"
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
CARGO_TARGET_DIR="$ROOT/target/$NODE_CLASS-$SESSION"
export CARGO_TARGET_DIR
mkdir -p "$CARGO_TARGET_DIR"

HEAD=$(git -C "$CODE_DIR" rev-parse HEAD)
[ "$HEAD" = "$COMMIT" ] || soc_die "the clone is at $HEAD, but the job was submitted for $COMMIT (sync.sh ran in between)"
[ -z "$(git -C "$CODE_DIR" status --porcelain --untracked-files=no)" ] || soc_die "the clone has uncommitted changes"

printf '%s job %s on %s: session %s, class %s, profile %s, commit %.12s, run date %s\n' \
    "$(ts)" "${SLURM_JOB_ID:-?}" "$(hostname -s)" "$SESSION" "$NODE_CLASS" "$PROFILE" "$COMMIT" "$RUN_DATE"
printf '%s CPU affinity: %s; target dir: %s\n' "$(ts)" \
    "$(python3 -c 'import os; print(sorted(os.sched_getaffinity(0)))')" "$CARGO_TARGET_DIR"

if ! sh "$CODE_DIR/reproduce/soc/setup.sh" --node-class "$NODE_CLASS" --record-only; then
    soc_die "first-run checks failed on $(hostname -s); no stage was run"
fi

OVERALL_RC=0
while IFS= read -r spec <&3; do
    soc_parse_spec "$spec" "$LCPC_VARIANTS"
    printf '%s === stage %s (blocks %s @ logN %s, LCPC variants %s) ===\n' \
        "$(ts)" "$SPEC_NAME" "${SPEC_REGEX:-all}" "${SPEC_LOGN:-per block}" "$SPEC_LCPC"
    set -- --host "soc-$NODE_CLASS" --profile "$PROFILE" --session "$SESSION" --stage "$SPEC_NAME" \
        --warmup "$WARMUP" --lcpc-variants "$SPEC_LCPC" --run-date "$RUN_DATE"
    [ -z "$SPEC_REGEX" ] || set -- "$@" --blocks "$SPEC_REGEX"
    [ -z "$SPEC_LOGN" ] || set -- "$@" --logn "$SPEC_LOGN"
    (cd "$CODE_DIR" && sh reproduce/all.sh "$@") </dev/null
    rc=$?
    printf '%s STAGE_%s_EXIT=%s\n' "$(ts)" "$SPEC_NAME" "$rc"
    [ "$rc" -eq 0 ] || OVERALL_RC=1
done 3< "$STAGES_FILE"

printf '%s session %s finished (exit %s); results: %s/results/soc-%s/%s-%.12s/%s\n' \
    "$(ts)" "$SESSION" "$OVERALL_RC" "$CODE_DIR" "$NODE_CLASS" "$RUN_DATE" "$COMMIT" "$SESSION"
exit "$OVERALL_RC"
