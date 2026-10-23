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

# Staged campaign driver: runs one session as a sequence of short stages on
# one existing VM (results are copied back after every stage by
# run_session.sh), then stops the VM. Each stage is one all.sh invocation
# restricted by a block-id regex and a logN list; all.sh keeps every stage's
# schedule, manifest and execution log separately under
# results/<host>/<run>/<session>/stages/.
#
# The 2026-10-22/23 campaign was run with an out-of-tree predecessor of this
# script (same logic) as, per session s1/s2 on c3d-highmem-16 VMs:
#   stages A, S, C, D, B of --preset x86, with LCPC variants
#   as-published-as-is,fork-flags-portable and warm-up 1; stage C then used
#   the old block ids '^full_(slice_sweep|thread_scaling)' (renamed since to
#   full_this_work_threads_* / full_lcpc_orig_threads_*, their output dirs);
#   stage R '^full_(lcpc_orig_mode_a|simd_spielman_.*_t[78])' @ 16..26 after
#   the LCPC driver and SIMD-Spielman coordinator fixes (superseding those
#   stage-A blocks);
# and session arm1 on c4a-highmem-16 with --machine c4a --preset arm.
# Orion-shape SIMD-Spielman at 2^28 was stopped by an external process killer
# then; all.sh now omits it explicitly (simd_spielman.sh --max-orion-logn 26).
#
# --preset remeasure re-measures what the 2026-10-23 audit invalidated or found
# missing (every original-LCPC as-published record was built with the fork's
# .cargo/config.toml; thread scaling stopped at 2^24; s1 lacked SIMD-Spielman
# Mont127 T=1 at 2^28; Arm This Work failed a wrong AArch64 codegen gate).

set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
GCLOUD=${GCLOUD:-gcloud}
MACHINE=c3d
PROFILE=""
INSTANCE=""
ZONE=""
SESSION_ID=""
LCPC_VARIANTS=as-published-as-is,fork-flags-portable
WARMUP=1
MAX_START_TRIES=30
START_RETRY_SEC=180
PRESET=""
RUN_DATE=""
PRINT_STAGES=0

usage() {
    cat <<EOF
Usage: $0 --instance NAME --zone ZONE --session ID [options] [--preset x86|arm|remeasure] ['NAME|REGEX|LOGN[|lcpc=LIST]' ...]

Runs the given stages of one benchmark session on an existing, set-up VM and
stops the VM at the end (also on failure or interruption). Each stage spec is
'NAME|REGEX|LOGN': a stage label, an all.sh --blocks regex (may contain '|')
and an all.sh --logn list, optionally followed by '|lcpc=LIST' to set the
original-LCPC build variants of that stage (default: --lcpc-variants).
All stages pass the driver's start date as all.sh --run-date, so a session
that crosses midnight (UTC) stays in one run dir <date>-<commit>.

Options:
  --instance NAME        VM instance name (required)
  --zone ZONE            VM zone (required)
  --session ID           Session id, e.g. s1 or s2 (required)
  --machine c3d|c4a      Machine family (default: c3d)
  --profile full|arm     all.sh profile (default: full for c3d, arm for c4a)
  --lcpc-variants LIST   Original-LCPC build variants
                         (default: as-published-as-is,fork-flags-portable)
  --warmup N             Warm-up count for every block (default: 1)
  --max-start-tries N    VM start attempts per stage, 3 min apart, to ride out
                         zone stockouts (default: 30)
  --run-date YYYY-MM-DD  Run-id date passed to every stage (default: today, UTC)
  --print-stages         Print the expanded stage list (name, regex, logN, LCPC
                         variants; tab-separated) and exit; needs no gcloud. Use it
                         with all.sh --dry-run to check a preset locally.
  --preset x86|arm|remeasure
                         Append the stages of the 2026-10 campaign:
                           x86: A  '^full_(this_work_mode|lcpc_orig_mode|polycommit|ligero)'  16..26
                                S  '^full_simd_spielman'                                       16..26
                                C  '^full_(slice_sweep|this_work_threads|lcpc_orig_threads)'   20,24,26
                                D  '^full_steps'                                               16,20,24
                                B  '^full_(this_work_mode|lcpc_orig_mode|polycommit|simd_spielman_mont127)' 28
                           arm: ARM '^arm_'                                                    18,22,26
                         or, with remeasure, the audit re-measurement (depends on --machine):
                           c3d: RL    '^full_lcpc_orig_mode_a_t[18]\$'  16..26  as-published-as-is,fork-flags-portable
                                RL28  '^full_lcpc_orig_mode_a_t[18]\$'  28      as-published-as-is,fork-flags-portable
                                RH    '^full_lcpc_orig_mode_a_t[18]\$'  20,24   as-published-portable,fork-flags-as-is
                                RT    '^full_(this_work|lcpc_orig)_threads_t[0-9]+\$'  20,24,26
                                RT28A '^full_lcpc_orig_threads_t[12]\$'                 28
                                RT28B '^full_(this_work_threads_t[0-9]+|lcpc_orig_threads_t[48])\$' 28
                                RS    '^full_simd_spielman_mont127_t1\$' 28 (Brakedown shape; Orion
                                      above 2^26 is omitted by --max-orion-logn 26)
                                (RT* use as-published-as-is,fork-flags-portable; RH isolates
                                the BLAKE3 backend from the compiler flags, audit E8/docs-12)
                           c4a: ARM   '^arm_'  18,22,26  as-published-as-is,fork-flags-portable
                         Each stage is planned to take at most about 2 h on its VM.
                         (SIMD-Spielman Orion-shape cases above 2^26 are omitted by
                         all.sh itself via simd_spielman.sh --max-orion-logn 26.)
  -h, --help             Show this help

The exit status is 0 only if every stage exited 0; each stage's status is
printed as STAGE_<NAME>_EXIT=<code>.

If gcloud is not on PATH, set GCLOUD to
  \$HOME/.local/share/google-cloud-sdk/bin/gcloud
EOF
}

STAGES_FILE=$(mktemp "${TMPDIR:-/tmp}/rbd-stages.XXXXXX") || exit 1
add_stage() {
    printf '%s\n' "$1" >> "$STAGES_FILE"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --instance|--zone|--session|--machine|--profile|--lcpc-variants|--warmup|--max-start-tries|--preset|--run-date)
            [ "$#" -ge 2 ] || { printf '[ERROR] %s requires an argument\n' "$1" >&2; exit 2; }
            case "$1" in
                --instance) INSTANCE=$2 ;;
                --zone) ZONE=$2 ;;
                --session) SESSION_ID=$2 ;;
                --machine) MACHINE=$2 ;;
                --profile) PROFILE=$2 ;;
                --lcpc-variants) LCPC_VARIANTS=$2 ;;
                --warmup) WARMUP=$2 ;;
                --max-start-tries) MAX_START_TRIES=$2 ;;
                --preset) PRESET=$2 ;;
                --run-date) RUN_DATE=$2 ;;
            esac
            shift 2
            ;;
        --print-stages)
            PRINT_STAGES=1
            shift
            ;;
        -h|--help)
            usage
            rm -f "$STAGES_FILE"
            exit 0
            ;;
        -*)
            printf '[ERROR] Unknown option: %s\n' "$1" >&2
            usage >&2
            rm -f "$STAGES_FILE"
            exit 2
            ;;
        *)
            add_stage "$1"
            shift
            ;;
    esac
done

case "$PRESET" in
    '') ;;
    x86)
        add_stage 'A|^full_(this_work_mode|lcpc_orig_mode|polycommit|ligero)|16,18,20,22,24,26'
        add_stage 'S|^full_simd_spielman|16,18,20,22,24,26'
        add_stage 'C|^full_(slice_sweep|this_work_threads|lcpc_orig_threads)|20,24,26'
        add_stage 'D|^full_steps|16,20,24'
        add_stage 'B|^full_(this_work_mode|lcpc_orig_mode|polycommit|simd_spielman_mont127)|28'
        ;;
    arm)
        add_stage 'ARM|^arm_|18,22,26'
        ;;
    remeasure)
        # Block ids are those of the current all.sh (the _n<logN> suffix is
        # added after the --blocks filter, so '$' anchors the base id).
        LCPC_MAIN='lcpc=as-published-as-is,fork-flags-portable'
        case "$MACHINE" in
            c3d)
                add_stage "RL|^full_lcpc_orig_mode_a_t[18]\$|16,18,20,22,24,26|$LCPC_MAIN"
                add_stage "RL28|^full_lcpc_orig_mode_a_t[18]\$|28|$LCPC_MAIN"
                add_stage 'RH|^full_lcpc_orig_mode_a_t[18]$|20,24|lcpc=as-published-portable,fork-flags-as-is'
                add_stage "RT|^full_(this_work|lcpc_orig)_threads_t[0-9]+\$|20,24,26|$LCPC_MAIN"
                add_stage "RT28A|^full_lcpc_orig_threads_t[12]\$|28|$LCPC_MAIN"
                add_stage "RT28B|^full_(this_work_threads_t[0-9]+|lcpc_orig_threads_t[48])\$|28|$LCPC_MAIN"
                add_stage 'RS|^full_simd_spielman_mont127_t1$|28'
                ;;
            c4a)
                add_stage "ARM|^arm_|18,22,26|$LCPC_MAIN"
                ;;
            *) printf '[ERROR] --preset remeasure needs --machine c3d or c4a\n' >&2; rm -f "$STAGES_FILE"; exit 2 ;;
        esac
        ;;
    *) printf '[ERROR] Unknown --preset %s (x86|arm|remeasure)\n' "$PRESET" >&2; rm -f "$STAGES_FILE"; exit 2 ;;
esac

fail_usage() {
    printf '[ERROR] %s\n' "$1" >&2
    rm -f "$STAGES_FILE"
    exit 2
}
[ -s "$STAGES_FILE" ] || fail_usage "no stages given (stage specs or --preset)"
RUN_DATE=${RUN_DATE:-$(date -u +%Y-%m-%d)}
case "$RUN_DATE" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) fail_usage "--run-date must be YYYY-MM-DD" ;;
esac

# Split a stage spec into SPEC_NAME, SPEC_REGEX, SPEC_LOGN and SPEC_LCPC.
parse_spec() {
    SPEC_LCPC=$LCPC_VARIANTS
    _spec=$1
    case "$_spec" in
        *'|lcpc='*)
            SPEC_LCPC=${_spec##*|lcpc=}
            _spec=${_spec%|lcpc=*}
            ;;
    esac
    SPEC_NAME=${_spec%%|*}
    _rest=${_spec#*|}
    SPEC_REGEX=${_rest%|*}
    SPEC_LOGN=${_rest##*|}
}
case "$MACHINE" in
    c3d) PROFILE=${PROFILE:-full} ;;
    c4a) PROFILE=${PROFILE:-arm} ;;
    *) fail_usage "--machine must be c3d or c4a" ;;
esac
case "$WARMUP:$MAX_START_TRIES" in
    *[!0-9:]*|:*|*:) fail_usage "--warmup and --max-start-tries must be integers" ;;
esac
while IFS= read -r spec; do
    case "$spec" in
        *'|'*'|'*) ;;
        *) fail_usage "stage spec must be 'NAME|REGEX|LOGN[|lcpc=LIST]': $spec" ;;
    esac
    parse_spec "$spec"
    case "$SPEC_NAME" in
        ''|*[!A-Za-z0-9._-]*) fail_usage "invalid stage name in: $spec" ;;
    esac
    case "$SPEC_LOGN" in
        ''|*[!0-9,]*) fail_usage "invalid logN list in: $spec" ;;
    esac
    case "$SPEC_LCPC" in
        ''|*[!a-z,-]*) fail_usage "invalid LCPC variant list in: $spec" ;;
    esac
done < "$STAGES_FILE"

if [ "$PRINT_STAGES" -eq 1 ]; then
    printf 'name\tblocks_regex\tlogn\tlcpc_variants\n'
    while IFS= read -r spec; do
        parse_spec "$spec"
        printf '%s\t%s\t%s\t%s\n' "$SPEC_NAME" "$SPEC_REGEX" "$SPEC_LOGN" "$SPEC_LCPC"
    done < "$STAGES_FILE"
    rm -f "$STAGES_FILE"
    exit 0
fi

[ -n "$INSTANCE" ] || fail_usage "--instance is required"
[ -n "$ZONE" ] || fail_usage "--zone is required"
[ -n "$SESSION_ID" ] || fail_usage "--session is required"
command -v "$GCLOUD" >/dev/null 2>&1 || fail_usage "gcloud command not found: $GCLOUD (set GCLOUD)"

ts() { date -u +%H:%M; }

vm_status() {
    "$GCLOUD" compute instances describe "$INSTANCE" --zone="$ZONE" --format='value(status)' 2>/dev/null
}

# Cost rule: the VM is stopped when this driver ends, however it ends.
stop_vm() {
    trap '' EXIT INT TERM
    "$GCLOUD" compute instances stop "$INSTANCE" --zone="$ZONE" --quiet >/dev/null 2>&1
    printf '%s ALL_STAGES_DONE (VM status: %s)\n' "$(ts)" "$(vm_status)"
    rm -f "$STAGES_FILE"
}
on_exit() {
    _rc=$?
    stop_vm
    exit "$_rc"
}
trap on_exit EXIT
trap 'exit 130' INT TERM

# Start the VM, retrying through zone stockouts.
start_vm() {
    tries=0
    while [ "$(vm_status)" != RUNNING ]; do
        "$GCLOUD" compute instances start "$INSTANCE" --zone="$ZONE" >/dev/null 2>&1 && break
        tries=$((tries + 1))
        printf '%s start failed (%s/%s)\n' "$(ts)" "$tries" "$MAX_START_TRIES"
        [ "$tries" -lt "$MAX_START_TRIES" ] || return 1
        sleep "$START_RETRY_SEC"
    done
}

OVERALL_RC=0
# The stage list is read on fd 3 so that run_session.sh (and the gcloud calls
# inside it) cannot consume it from stdin.
while IFS= read -r spec <&3; do
    parse_spec "$spec"
    name=$SPEC_NAME
    regex=$SPEC_REGEX
    logn=$SPEC_LOGN
    printf '%s === stage %s (%s @ %s, LCPC variants %s) ===\n' "$(ts)" "$name" "$regex" "$logn" "$SPEC_LCPC"
    if ! start_vm; then
        printf '%s STAGE_%s_EXIT=start-failed\n' "$(ts)" "$name"
        OVERALL_RC=1
        continue
    fi
    GCLOUD=$GCLOUD sh "$SCRIPT_DIR/run_session.sh" --machine "$MACHINE" --instance "$INSTANCE" \
        --zone "$ZONE" --session "$SESSION_ID" --profile "$PROFILE" --keep-running \
        --stage "$name" --blocks "$regex" --logn "$logn" --warmup "$WARMUP" \
        --lcpc-variants "$SPEC_LCPC" --run-date "$RUN_DATE" </dev/null
    rc=$?
    printf '%s STAGE_%s_EXIT=%s\n' "$(ts)" "$name" "$rc"
    [ "$rc" -eq 0 ] || OVERALL_RC=1
done 3< "$STAGES_FILE"

exit "$OVERALL_RC"
