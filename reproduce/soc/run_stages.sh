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

# Submits one benchmark session as one Slurm job. Run it on the login node
# from the cluster clone (<root>/code, see sync.sh); --print-stages and
# --dry-run work anywhere. The job (reproduce/soc/job.sh) runs the session's
# stages one after another on one node; each stage is one all.sh invocation
# restricted by a block-id regex and a logN list, and all.sh keeps every
# stage's schedule, manifest and execution log separately under
# results/<host>/<run>/<session>/stages/.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ROOT=$(CDPATH= cd -- "$CODE_DIR/.." && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib.sh"

SESSION_ID=
PRESET=
NODE_CLASS=
PROFILE=
LCPC_VARIANTS=$RBD_LCPC_VARIANTS_MAIN
WARMUP=1
RUN_DATE=
PARTITION=long
TIME_LIMIT=
NODELIST=
MEM=160G
CPUS=16
PRINT_STAGES=0
DRY_RUN=0

usage() {
    cat <<EOF
Usage: $0 --session ID [--preset x86|arm] ['NAME|REGEX|LOGN[|lcpc=LIST]' ...] [options]

Submits the stages of one session as one Slurm job (sbatch). A stage spec is
a stage label, an all.sh --blocks regex (may contain '|'; empty = all blocks)
and an all.sh --logn list (empty = each block's own list), optionally followed
by '|lcpc=LIST' for that stage's original-LCPC build variants. Every stage gets
the same --run-date, so a session that crosses midnight (UTC) stays in one
run directory <date>-<commit>.

Options:
  --session ID           session id, e.g. s1, s2, arm1 (required)
  --preset x86|arm       append the campaign stages (below)
  --node-class xcnh|xcng node class (default: xcnh for x86, xcng for arm)
  --profile P            all.sh profile full|arm|smoke (default: full on xcnh, arm on xcng)
  --lcpc-variants LIST   default original-LCPC build variants
                         (default: $RBD_LCPC_VARIANTS_MAIN)
  --warmup N             warm-up count of every block (default: 1)
  --run-date YYYY-MM-DD  run-id date of every stage (default: today, UTC)
  --partition P          Slurm partition (default: long)
  --time T               Slurm time limit (default: 2-00:00:00 on xcnh, 12:00:00 on xcng)
  --nodelist NODE        run on this node (e.g. xcnh1), so that two sessions
                         use two different nodes
  --mem M                memory (default: 160G)
  --cpus N               CPUs of the job (default: 16, i.e. 16 physical cores
                         with --threads-per-core=1: on xcnh two whole L3
                         domains, one for the benchmark cores)
  --print-stages         print the expanded stage list (tab-separated) and exit
  --dry-run              print the sbatch command instead of submitting
  -h, --help             show this help

Presets (block ids of reproduce/all.sh; '\$' anchors the id before the
_n<logN> suffix):
  x86 (xcnh, profile full):
    A    '^full_(this_work_mode|lcpc_orig_mode|polycommit|ligero)'   16..26
    H    '^full_lcpc_orig_mode_a_t[18]\$'  20,24  lcpc=as-published-portable,fork-flags-as-is
    S    '^full_simd_spielman'                                     16..26
    C    '^full_slice_sweep'                                       20,24,26
    T    '^full_(this_work|lcpc_orig)_threads_t[0-9]+\$'            20,24,26
    T28  '^full_(this_work|lcpc_orig)_threads_t[0-9]+\$'            28
    D    '^full_steps'                                             16,20,24
    B    '^full_(this_work_mode|lcpc_orig_mode|polycommit|simd_spielman)'  28
         (SIMD-Spielman Orion shape above 2^26 is omitted by all.sh itself,
         simd_spielman.sh --max-orion-logn 26)
  arm (xcng, profile arm):
    ARM  '^arm_'                                                   18,22,26

The job's log is <root>/jobs/rbd-<session>-<jobid>.out; it prints
STAGE_<NAME>_EXIT=<code> per stage and exits 0 only if every stage did.
EOF
}

STAGES_FILE=$(mktemp "${TMPDIR:-/tmp}/rbd-stages.XXXXXX")
trap 'rm -f "$STAGES_FILE"' EXIT INT TERM
add_stage() {
    printf '%s\n' "$1" >> "$STAGES_FILE"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --session|--preset|--node-class|--profile|--lcpc-variants|--warmup|--run-date|--partition|--time|--nodelist|--mem|--cpus)
            [ "$#" -ge 2 ] || soc_die "$1 requires an argument"
            case "$1" in
                --session) SESSION_ID=$2 ;;
                --preset) PRESET=$2 ;;
                --node-class) NODE_CLASS=$2 ;;
                --profile) PROFILE=$2 ;;
                --lcpc-variants) LCPC_VARIANTS=$2 ;;
                --warmup) WARMUP=$2 ;;
                --run-date) RUN_DATE=$2 ;;
                --partition) PARTITION=$2 ;;
                --time) TIME_LIMIT=$2 ;;
                --nodelist) NODELIST=$2 ;;
                --mem) MEM=$2 ;;
                --cpus) CPUS=$2 ;;
            esac
            shift 2
            ;;
        --print-stages) PRINT_STAGES=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        -*) usage >&2; soc_die "unknown option: $1" ;;
        *) add_stage "$1"; shift ;;
    esac
done

case "$PRESET" in
    '') ;;
    x86)
        NODE_CLASS=${NODE_CLASS:-xcnh}
        add_stage 'A|^full_(this_work_mode|lcpc_orig_mode|polycommit|ligero)|16,18,20,22,24,26'
        add_stage 'H|^full_lcpc_orig_mode_a_t[18]$|20,24|lcpc=as-published-portable,fork-flags-as-is'
        add_stage 'S|^full_simd_spielman|16,18,20,22,24,26'
        add_stage 'C|^full_slice_sweep|20,24,26'
        add_stage 'T|^full_(this_work|lcpc_orig)_threads_t[0-9]+$|20,24,26'
        add_stage 'T28|^full_(this_work|lcpc_orig)_threads_t[0-9]+$|28'
        add_stage 'D|^full_steps|16,20,24'
        add_stage 'B|^full_(this_work_mode|lcpc_orig_mode|polycommit|simd_spielman)|28'
        ;;
    arm)
        NODE_CLASS=${NODE_CLASS:-xcng}
        add_stage 'ARM|^arm_|18,22,26'
        ;;
    *) soc_die "unknown --preset $PRESET (x86|arm)" ;;
esac

[ -s "$STAGES_FILE" ] || soc_die "no stages given (stage specs or --preset)"
while IFS= read -r spec; do
    soc_check_spec "$spec" || exit 2
done < "$STAGES_FILE"
case "$LCPC_VARIANTS" in
    ''|*[!a-z,-]*) soc_die "invalid --lcpc-variants" ;;
esac
case "$WARMUP" in
    ''|*[!0-9]*) soc_die "--warmup must be an integer" ;;
esac

if [ "$PRINT_STAGES" -eq 1 ]; then
    printf 'name\tblocks_regex\tlogn\tlcpc_variants\n'
    while IFS= read -r spec; do
        soc_parse_spec "$spec" "$LCPC_VARIANTS"
        printf '%s\t%s\t%s\t%s\n' "$SPEC_NAME" "$SPEC_REGEX" "$SPEC_LOGN" "$SPEC_LCPC"
    done < "$STAGES_FILE"
    exit 0
fi

[ -n "$SESSION_ID" ] || soc_die "--session is required"
case "$SESSION_ID" in
    *[!A-Za-z0-9._-]*) soc_die "invalid session id: $SESSION_ID" ;;
esac
soc_class_arch "$NODE_CLASS" >/dev/null || soc_die "--node-class must be xcnh or xcng (or use --preset)"
case "$NODE_CLASS" in
    xcnh) PROFILE=${PROFILE:-full}; TIME_LIMIT=${TIME_LIMIT:-2-00:00:00} ;;
    xcng) PROFILE=${PROFILE:-arm}; TIME_LIMIT=${TIME_LIMIT:-12:00:00} ;;
esac
case "$PROFILE" in
    full|arm|smoke) ;;
    *) soc_die "--profile must be full, arm or smoke" ;;
esac
[ "$PROFILE:$NODE_CLASS" != full:xcng ] || soc_die "profile full needs AVX-512 IFMA (xcnh); use --profile arm on xcng"
RUN_DATE=${RUN_DATE:-$(date -u +%Y-%m-%d)}
case "$RUN_DATE" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) soc_die "--run-date must be YYYY-MM-DD" ;;
esac
case "$NODELIST$PARTITION$TIME_LIMIT$MEM$CPUS" in
    *[!A-Za-z0-9:,._-]*) soc_die "invalid --nodelist, --partition, --time, --mem or --cpus" ;;
esac

COMMIT=$(git -C "$CODE_DIR" rev-parse HEAD)
JOBS_DIR="$ROOT/jobs"
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
JOB_FILE="$JOBS_DIR/rbd-$SESSION_ID-$STAMP.job"

set -- sbatch --parsable -J "rbd-$SESSION_ID" -p "$PARTITION" --constraint="$NODE_CLASS" \
    -n1 -c "$CPUS" --threads-per-core=1 --mem="$MEM" --time="$TIME_LIMIT" \
    --cpu-freq=Performance --no-requeue -o "$JOBS_DIR/rbd-$SESSION_ID-%j.out"
[ -z "$NODELIST" ] || set -- "$@" --nodelist="$NODELIST"
set -- "$@" "$SCRIPT_DIR/job.sh" "$CODE_DIR" "$JOB_FILE"

# Job file read by job.sh: key=value settings, then one 'stage=<spec>' line
# per stage in order.
JOB_TMP=$(mktemp "${TMPDIR:-/tmp}/rbd-job.XXXXXX")
{
    printf 'session=%s\nnode_class=%s\nprofile=%s\nrun_date=%s\nwarmup=%s\nlcpc_variants=%s\ncommit=%s\n' \
        "$SESSION_ID" "$NODE_CLASS" "$PROFILE" "$RUN_DATE" "$WARMUP" "$LCPC_VARIANTS" "$COMMIT"
    while IFS= read -r spec; do
        printf 'stage=%s\n' "$spec"
    done < "$STAGES_FILE"
} > "$JOB_TMP"
trap 'rm -f "$STAGES_FILE" "$JOB_TMP"' EXIT INT TERM

if [ "$DRY_RUN" -eq 1 ]; then
    printf '%s\n' "$*"
    printf '[soc] job file (%s):\n' "$JOB_FILE"
    cat "$JOB_TMP"
    exit 0
fi
command -v sbatch >/dev/null 2>&1 || soc_die "sbatch not found: run this on the cluster login node"
if [ -n "$(git -C "$CODE_DIR" status --porcelain --untracked-files=no)" ]; then
    soc_die "the clone $CODE_DIR has uncommitted changes; sync a commit with sync.sh"
fi
mkdir -p "$JOBS_DIR"
cp "$JOB_TMP" "$JOB_FILE"
JOB_ID=$("$@")
soc_log "submitted job $JOB_ID: session $SESSION_ID on $NODE_CLASS${NODELIST:+ ($NODELIST)}, profile $PROFILE, run date $RUN_DATE, commit $(printf '%.12s' "$COMMIT")"
soc_log "log: $JOBS_DIR/rbd-$SESSION_ID-$JOB_ID.out"
printf '%s\n' "$JOB_ID"
