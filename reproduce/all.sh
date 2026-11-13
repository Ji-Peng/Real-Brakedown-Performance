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

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/common.sh"
# CARGO_HOME selects a per-architecture toolchain on hosts whose home
# directory is shared between x86-64 and aarch64 nodes (reproduce/soc/).
if [ -f "${CARGO_HOME:-$HOME/.cargo}/env" ]; then
    # shellcheck disable=SC1091
    . "${CARGO_HOME:-$HOME/.cargo}/env"
fi
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$CODE_DIR/target-harness"}
export CARGO_TARGET_DIR

PROFILE=""
SESSION_ID=""
SEED=""
SPECIFIC_TAGS=""
DRY_RUN=0
PASSTHROUGH_ARGS=""
BLOCK_FILTER=""
LOGN_FILTER=""
WARMUP_OVERRIDE=""
STAGE_NAME=""
RUN_DATE_OVERRIDE=""

usage() {
    cat <<EOF
Usage: $0 [--profile full|arm|smoke] [--session <id>] [--seed <int>] [options]

Master campaign orchestrator for the RealBrakedownPerf evaluation:
  - full:  x86-64 primary campaign (E1-E13, logN 16..28, T 1..8 on the host
           profile's 8-core benchmark set, all baselines; SIMD-Spielman Orion
           shape up to 2^26, see simd_spielman.sh --max-orion-logn)
  - arm:   Arm portability campaign (E14: This Work + Original LCPC, logN 18,22,26, T 1,8)
  - smoke: Tiny dry-run across all targets, outputs watermarked "LOCAL SMOKE — NOT A RESULT"

Options:
  --profile full|arm|smoke Campaign workload profile (default: detected or smoke on local)
  --session <id>           Session ID recorded in every JSON report (default: session-YYYYMMDD-HHMMSS)
  --seed <int>             Random seed for interleaving block execution order (default: auto-generated)
  --host <name>            Host profile from reproduce/hosts/<name>.sh
  --out <dir>              Output directory (default: results)
  --tags <tags>            Comma-separated step tags for steps.sh progression
  --blocks <regex>         Run only blocks whose id matches (staged campaigns)
  --logn <list>            Keep only these logN values; a block's id and output dir get a
                           _n<list> suffix so stages never overwrite each other
  --warmup <int>           Override every block's warm-up count
  --lcpc-variants <list>   Original-LCPC build variants to measure (default: all four)
  --stage <name>           Stage label for a staged campaign (default: a hash of the
                           stage arguments). Each invocation writes its schedule,
                           manifest and execution log under
                           <session>/stages/<UTC time>_<stage>/ and never overwrites
                           an earlier stage's records; <session>/schedule.json is
                           append-only and lists every stage of the session.
  --run-date YYYY-MM-DD    Date part of the run id <date>-<commit> (default: today,
                           UTC); a staged driver passes its start date so that a
                           session crossing midnight stays in one run dir
  --dry-run                Print scheduled execution blocks and exit without running

Each block is pinned by its thread count: T=1 blocks use the host's single
benchmark core; T>1 blocks use the first T cores of the multi-core set (T+1 for
SIMD-Spielman, whose spin-waiting coordinator is pinned after its T workers).
Before a block runs, an existing output dir of the same block id is moved to
<out>.prev-<UTC time> inside the session (analysis excludes "*.prev-*"), so a
re-run never mixes records of two stages; every record of the block's own
output dir is (re-)tagged with the block id, stage and block status.
The exit status is 1 if any block failed (listed in <session>/failed_blocks.tsv).
  -h, --help               Show this help message
EOF
}

ANY_BLOCK_FAILED=0
while [ $# -gt 0 ]; do
    case "$1" in
        --profile)
            [ $# -ge 2 ] || { log_error "Option --profile requires an argument (full|arm|smoke)"; exit 1; }
            PROFILE="$2"
            shift 2
            ;;
        --session)
            [ $# -ge 2 ] || { log_error "Option --session requires an argument"; exit 1; }
            SESSION_ID="$2"
            shift 2
            ;;
        --seed)
            [ $# -ge 2 ] || { log_error "Option --seed requires an argument"; exit 1; }
            SEED="$2"
            shift 2
            ;;
        --tags)
            [ $# -ge 2 ] || { log_error "Option --tags requires an argument"; exit 1; }
            SPECIFIC_TAGS="$2"
            shift 2
            ;;
        --blocks)
            [ $# -ge 2 ] || { log_error "Option --blocks requires a regex"; exit 1; }
            BLOCK_FILTER="$2"
            shift 2
            ;;
        --logn)
            [ $# -ge 2 ] || { log_error "Option --logn requires a list"; exit 1; }
            LOGN_FILTER="$2"
            shift 2
            ;;
        --warmup)
            [ $# -ge 2 ] || { log_error "Option --warmup requires an integer"; exit 1; }
            WARMUP_OVERRIDE="$2"
            shift 2
            ;;
        --lcpc-variants)
            [ $# -ge 2 ] || { log_error "Option --lcpc-variants requires a list"; exit 1; }
            LCPC_VARIANTS="$2"
            export LCPC_VARIANTS
            shift 2
            ;;
        --stage)
            [ $# -ge 2 ] || { log_error "Option --stage requires a name"; exit 1; }
            STAGE_NAME="$2"
            case "$STAGE_NAME" in
                *[!A-Za-z0-9._-]*) log_error "Invalid --stage '$STAGE_NAME' (use A-Z a-z 0-9 . _ -)"; exit 1 ;;
            esac
            shift 2
            ;;
        --run-date)
            [ $# -ge 2 ] || { log_error "Option --run-date requires YYYY-MM-DD"; exit 1; }
            RUN_DATE_OVERRIDE="$2"
            case "$RUN_DATE_OVERRIDE" in
                [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
                *) log_error "Invalid --run-date '$RUN_DATE_OVERRIDE' (use YYYY-MM-DD)"; exit 1 ;;
            esac
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            PASSTHROUGH_ARGS="$PASSTHROUGH_ARGS $1"
            shift
            ;;
    esac
done

# shellcheck disable=SC2086
parse_args $PASSTHROUGH_ARGS
export CORES

# parse_args resolves CORES for its default threads=1, i.e. a single core.
# Blocks pin per thread count: T=1 blocks use one benchmark core, T>1 blocks
# use the multi-core benchmark set (an explicit --cores list, else the host
# profile's default set).
if [ "$EXPLICIT_CORES" -eq 1 ]; then
    MULTI_CORES="$CORES"
    SINGLE_CORE="$(expand_cores "$CORES" | awk '{print $1}')"
else
    MULTI_CORES="${HOST_DEFAULT_CORES:-$CORES}"
    SINGLE_CORE="${HOST_DEFAULT_SINGLE_CORE:-$CORES}"
fi
export MULTI_CORES SINGLE_CORE

# Auto-detect profile if not provided
if [ -z "$PROFILE" ]; then
    if [ "$HOST_NAME" = "soc-xcnh" ]; then
        PROFILE="full"
    elif [ "$HOST_NAME" = "soc-xcng" ]; then
        PROFILE="arm"
    else
        PROFILE="smoke"
    fi
fi

case "$PROFILE" in
    full|arm|smoke) ;;
    *)
        log_error "Invalid --profile '$PROFILE'; must be full, arm, or smoke"
        exit 1
        ;;
esac

# Session ID
if [ -z "$SESSION_ID" ]; then
    SESSION_ID="session-$(date -u +%Y%m%d-%H%M%S)"
fi

# Seed for block randomization
if [ -z "$SEED" ]; then
    SEED=$(python3 -c 'import secrets; print(secrets.randbelow(1_000_000_000))' 2>/dev/null || date +%s)
fi

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac

CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD 2>/dev/null || echo "detached")
RUN_DATE=${RUN_DATE_OVERRIDE:-$(date -u +%Y-%m-%d)}
RUN_ID="${RUN_DATE}-${CODE_SHA}"
SESSION_DIR="$OUT_ROOT/$HOST_NAME/$RUN_ID/$SESSION_ID"
mkdir -p "$SESSION_DIR"

# Staged campaigns call all.sh several times for the same session directory.
# Each invocation (stage) keeps its own schedule, manifest and execution log
# under stages/<UTC start time>_<stage label>/, so later stages never
# overwrite the provenance of earlier ones. Without --stage the label is a
# hash of the stage-defining arguments.
STAGE_ARGS_STRING="profile=$PROFILE blocks=$BLOCK_FILTER logn=$LOGN_FILTER warmup=$WARMUP_OVERRIDE lcpc_variants=${LCPC_VARIANTS:-} tags=$SPECIFIC_TAGS seed=$SEED"
STAGE_HASH=$(printf '%s' "$STAGE_ARGS_STRING" | sha256sum | cut -c1-8)
STAGE_LABEL=${STAGE_NAME:-h$STAGE_HASH}
STAGE_STARTED_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)
STAGE_TAG="$(date -u +%Y%m%dT%H%M%SZ)_$STAGE_LABEL"
STAGE_DIR="$SESSION_DIR/stages/$STAGE_TAG"
mkdir -p "$STAGE_DIR"
INSTANCE_NAME=$(host_instance_name)

WATERMARK=""
if [ "$PROFILE" = "smoke" ]; then
    WATERMARK="LOCAL SMOKE — NOT A RESULT"
fi

log_info "=========================================================="
log_info "Master Benchmark Campaign: reproduce/all.sh"
log_info "Profile:       $PROFILE"
log_info "Session ID:    $SESSION_ID"
log_info "Stage:         $STAGE_TAG"
log_info "Random Seed:   $SEED"
log_info "Host:          $HOST_NAME (test_only=$HOST_IS_TEST_ONLY, instance=$INSTANCE_NAME)"
log_info "Session Dir:   $SESSION_DIR"
if [ -n "$WATERMARK" ]; then
    log_info "Watermark:     $WATERMARK"
fi
log_info "=========================================================="

# Per-node evidence: keep a copy of this node's host profile and first-run
# record with the stage, so it travels with the session's results.
for _evidence in "$CODE_DIR/results/$HOST_NAME/host-profile-$INSTANCE_NAME.json" \
                 "$CODE_DIR/results/$HOST_NAME/first-run-$INSTANCE_NAME.json"; do
    if [ -f "$_evidence" ]; then
        cp "$_evidence" "$STAGE_DIR/"
    fi
done

SCHEDULE_FILE="$STAGE_DIR/schedule.json"
BLOCKS_TSV="$STAGE_DIR/blocks.tsv"
TAG_SCRIPT="$STAGE_DIR/tag_json.py"
SESSION_SCHEDULE="$SESSION_DIR/schedule.json"
MOVED_TSV="$STAGE_DIR/moved_blocks.tsv"

# Tags the JSON records of ONE block's own output directory (never other
# blocks' outputs), for failed blocks too, with the block id and status. The
# directory only holds this run's records (an earlier run's dir was moved to
# <out>.prev-<UTC>), so every record is (re-)tagged.
cat > "$TAG_SCRIPT" << 'PY_TAG'
import json, os, sys

# Legacy This Work binaries (before bench/src/lib.rs recorded cpu_mhz_source)
# computed cycles_per_mac from the same reading.
LEGACY_MHZ_SOURCE = "/proc/cpuinfo first 'cpu MHz' entry (CPU 0); nominal, not the pinned core's measured clock"

def tag_item(item, fields):
    if not isinstance(item, dict):
        return False
    item.update({k: v for k, v in fields.items() if v is not None})
    # cycles_per_mac comes from the benchmark's own /proc/cpuinfo MHz reading
    # (cpu_mhz_source), never from the block-level freq_* fields above, so it
    # is always nominal: flag it from that same source, on every host.
    if item.get("cycles_per_mac") is not None:
        item["cycles_per_mac_is_nominal"] = True
        item.setdefault("cpu_mhz_source", LEGACY_MHZ_SOURCE)
    return True

def tag_file(path, fields):
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except Exception:
        return
    items = data if isinstance(data, list) else [data]
    modified = False
    for item in items:
        modified = tag_item(item, fields) or modified
    if modified:
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)

if __name__ == "__main__":
    (target_dir, session_id, seed, watermark, freq_before, freq_after, freq_source,
     block_id, block_status, stage_tag) = sys.argv[1:11]
    nominal = freq_source.startswith("nominal")
    fields = {
        "session_id": session_id,
        "session_seed": seed,
        "stage": stage_tag,
        "block_id": block_id,
        "block_status": block_status,
        "freq_before_mhz": freq_before or None,
        "freq_after_mhz": freq_after or None,
        "freq_source": freq_source,
        "freq_is_nominal": nominal,
        "watermark": watermark or None,
    }
    if not os.path.isdir(target_dir):
        sys.exit(0)
    for root, _, files in os.walk(target_dir):
        for fname in files:
            if fname.endswith(".json") and "manifest" not in fname and "schedule" not in fname:
                tag_file(os.path.join(root, fname), fields)
PY_TAG

# Build execution block schedule in Python (stage filters are passed through
# the environment so regexes need no quoting inside the Python source).
STAGE_BLOCK_FILTER="$BLOCK_FILTER" STAGE_LOGN_FILTER="$LOGN_FILTER" STAGE_WARMUP_OVERRIDE="$WARMUP_OVERRIDE" \
STAGE_TAG="$STAGE_TAG" STAGE_LABEL="$STAGE_LABEL" STAGE_STARTED_UTC="$STAGE_STARTED_UTC" \
SCHEDULE_FILE="$SCHEDULE_FILE" BLOCKS_TSV="$BLOCKS_TSV" \
python3 - << PY_SCHED
import json, random, os

profile = "$PROFILE"
seed = int("$SEED")
host = "$HOST_NAME"
session_dir = "$SESSION_DIR"
session_id = "$SESSION_ID"
script_dir = "$SCRIPT_DIR"
tags_arg = "$SPECIFIC_TAGS"

blocks = []

if profile == "smoke":
    # Representative, small local runs covering every experiment family.
    # These outputs are watermarked and are never publishable measurements.
    smoke_cores = os.environ.get("CORES", "0")
    step_tags_flag = f"--tags '{tags_arg}'" if tags_arg else ""
    blocks.append({
        "id": "smoke_this_work_mode_a_t1",
        "description": "This Work mode (a) tiny smoke T=1",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 16,20 --threads 1 --runs 2 --warmup 1 --slices adaptive --cores '{smoke_cores}' --test-only --host '{host}' --out '{session_dir}/smoke_this_work_mode_a_t1'"
    })
    blocks.append({
        "id": "smoke_lcpc_orig_a_t1",
        "description": "Original LCPC baseline tiny smoke T=1",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 16,20 --threads 1 --runs 2 --warmup 1 --cores '{smoke_cores}' --host '{host}' --out '{session_dir}/smoke_lcpc_orig_a_t1'"
    })
    blocks.append({
        "id": "smoke_this_work_mode_b_mont127_t1",
        "description": "This Work mode (b) tiny smoke T=1",
        "cmd": f"'{script_dir}/this_work.sh' --params simd-spielman --field mont127 --logN 20 --threads 1 --runs 2 --warmup 1 --slices adaptive --shape both --cores '{smoke_cores}' --test-only --host '{host}' --out '{session_dir}/smoke_this_work_mode_b_mont127_t1'"
    })
    for smoke_field in ["mers127", "fp2m61"]:
        blocks.append({
            "id": f"smoke_this_work_mode_b_{smoke_field}_t1",
            "description": f"This Work mode (b) {smoke_field} smoke T=1",
            "cmd": f"'{script_dir}/this_work.sh' --params simd-spielman --field {smoke_field} --logN 20 --threads 1 --runs 2 --warmup 1 --slices adaptive --shape both --cores '{smoke_cores}' --test-only --host '{host}' --out '{session_dir}/smoke_this_work_mode_b_{smoke_field}_t1'"
        })
    blocks.append({
        "id": "smoke_steps",
        "description": "All tagged steps progression smoke",
        "cmd": f"'{script_dir}/steps.sh' --logN 16,20,24 --threads 1 --runs 2 --warmup 1 {step_tags_flag} --cores '{smoke_cores}' --session-id '{session_id}' --smoke --host '{host}' --out '{session_dir}/smoke_steps'"
    })
    blocks.append({
        "id": "smoke_simd_spielman",
        "description": "SIMD-Spielman baseline smoke (Brakedown and Orion)",
        "cmd": f"'{script_dir}/simd_spielman.sh' --shape both --logN 16,20 --threads 1 --runs 2 --warmup 1 --cores '{smoke_cores}' --host '{host}' --out '{session_dir}/smoke_simd_spielman'"
    })
    blocks.append({
        "id": "smoke_polycommit",
        "description": "PolyCommit baseline smoke",
        "cmd": f"sh '{script_dir}/polycommit.sh' --field mont127 --logN 16,20 --threads 1 --runs 2 --warmup 1 --cores '{smoke_cores}' --host '{host}' --out '{session_dir}/smoke_polycommit'"
    })
    for smoke_threads in [2, 4, 7]:
        blocks.append({
            "id": f"smoke_this_work_threads_t{smoke_threads}",
            "description": f"This Work thread-scaling smoke T={smoke_threads}",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 20 --threads {smoke_threads} --runs 2 --warmup 1 --slices adaptive --cores '{smoke_cores}' --test-only --host '{host}' --out '{session_dir}/smoke_this_work_threads_t{smoke_threads}'"
        })
        blocks.append({
            "id": f"smoke_lcpc_orig_threads_t{smoke_threads}",
            "description": f"Original LCPC thread-scaling smoke T={smoke_threads}",
            "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 20 --threads {smoke_threads} --runs 2 --warmup 1 --cores '{smoke_cores}' --host '{host}' --out '{session_dir}/smoke_lcpc_orig_threads_t{smoke_threads}'"
        })
    for smoke_slices in [1, 2]:
        blocks.append({
            "id": f"smoke_this_work_slice_s{smoke_slices}",
            "description": f"This Work slice sweep smoke s={smoke_slices}",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 20 --threads 1 --runs 2 --warmup 1 --slices {smoke_slices} --cores '{smoke_cores}' --test-only --host '{host}' --out '{session_dir}/smoke_this_work_slice_s{smoke_slices}'"
        })
    blocks.append({
        "id": "smoke_ligero_rs",
        "description": "Same-library Ligero Reed-Solomon encoding smoke",
        "cmd": f"'{script_dir}/this_work.sh' --params rs-ligero --field mont127 --logN 20 --threads 1 --runs 2 --warmup 1 --cores '{smoke_cores}' --test-only --host '{host}' --out '{session_dir}/smoke_ligero_rs'"
    })
elif profile == "arm":
    # E14: Arm portability campaign (SoC cluster xcng, Neoverse-N1)
    # This Work + Original LCPC only; logN 18, 22, 26; T 1, 8
    blocks.append({
        "id": "arm_this_work_t1",
        "description": "This Work mode (a) on Arm T=1 (logN 18,22,26)",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 1 --runs 5 --warmup 2 --slices adaptive --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/arm_this_work_t1'"
    })
    blocks.append({
        "id": "arm_lcpc_orig_t1",
        "description": "Original LCPC mode (a) on Arm T=1 (logN 18,22,26)",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 1 --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/arm_lcpc_orig_t1'"
    })
    blocks.append({
        "id": "arm_this_work_t8",
        "description": "This Work mode (a) on Arm T=8 (logN 18,22,26)",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 8 --runs 5 --warmup 2 --slices adaptive --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/arm_this_work_t8'"
    })
    blocks.append({
        "id": "arm_lcpc_orig_t8",
        "description": "Original LCPC mode (a) on Arm T=8 (logN 18,22,26)",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 8 --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/arm_lcpc_orig_t8'"
    })
else:
    # full: x86-64 primary campaign (SoC cluster xcnh, E1-E13, logN 16..28)
    # E1: Headline comparison mode (a) Mont127 T=1 and T=8
    blocks.append({
        "id": "full_this_work_mode_a_t1",
        "description": "E1: This Work mode (a) Mont127 T=1 (logN 16..28)",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --warmup 2 --slices adaptive --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_this_work_mode_a_t1'"
    })
    blocks.append({
        "id": "full_lcpc_orig_mode_a_t1",
        "description": "E1: Original LCPC mode (a) Ft127 T=1 (logN 16..28)",
            "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_lcpc_orig_mode_a_t1'"
    })
    blocks.append({
        "id": "full_this_work_mode_a_t8",
        "description": "E1: This Work mode (a) Mont127 T=8 (logN 16..28)",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --warmup 2 --slices adaptive --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_this_work_mode_a_t8'"
    })
    blocks.append({
        "id": "full_lcpc_orig_mode_a_t8",
        "description": "E1: Original LCPC mode (a) Ft127 T=8 (logN 16..28)",
            "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_lcpc_orig_mode_a_t8'"
    })
    # E2: Comparison with SIMD-Spielman (mode b, Mont127, Mers127, Fp2M61)
    for field_name in ["mont127", "mers127", "fp2m61"]:
        blocks.append({
            "id": f"full_this_work_mode_b_{field_name}_t1",
            "description": f"E2: This Work mode (b) {field_name} T=1 (logN 16..28)",
            "cmd": f"'{script_dir}/this_work.sh' --params simd-spielman --field {field_name} --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --warmup 2 --slices adaptive --shape both --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_this_work_mode_b_{field_name}_t1'"
        })
        blocks.append({
            "id": f"full_this_work_mode_b_{field_name}_t8",
            "description": f"E2: This Work mode (b) {field_name} T=8 (logN 16..28)",
            "cmd": f"'{script_dir}/this_work.sh' --params simd-spielman --field {field_name} --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --warmup 2 --slices adaptive --shape both --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_this_work_mode_b_{field_name}_t8'"
        })
        blocks.append({
            "id": f"full_simd_spielman_{field_name}_t1",
            "description": f"E2: SIMD-Spielman AVX-512 {field_name} T=1 (logN 16..28)",
            "cmd": f"'{script_dir}/simd_spielman.sh' --field {field_name} --max-orion-logn 26 --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_simd_spielman_{field_name}_t1'"
        })
        blocks.append({
            "id": f"full_simd_spielman_{field_name}_t8",
            "description": f"E2: SIMD-Spielman AVX-512 {field_name} T=8 (logN 16..28)",
            "cmd": f"'{script_dir}/simd_spielman.sh' --field {field_name} --max-orion-logn 26 --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_simd_spielman_{field_name}_t8'"
        })
        # Upstream's MT encoder adds a spin-waiting coordinator thread to its workers;
        # on an 8-core host the fair 8-core point is 7 workers + coordinator (E2).
        blocks.append({
            "id": f"full_simd_spielman_{field_name}_t7",
            "description": f"E2: SIMD-Spielman AVX-512 {field_name} 7 workers + coordinator on 8 cores (logN 16..28)",
            "cmd": f"'{script_dir}/simd_spielman.sh' --field {field_name} --max-orion-logn 26 --logN 16,18,20,22,24,26,28 --threads 7 --runs 5 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_simd_spielman_{field_name}_t7'"
        })

    # E3: Per-technique breakdown, one block per mode: mode (a) lcpc-sdig6
    # Mont127 (Ft127 at step/01-modernize, before lcpc-fields existed) and
    # mode (b) simd-spielman Mers127 (N/A before step/04-r2l).
    step_tags_flag = f"--tags '{tags_arg}'" if tags_arg else ""
    blocks.append({
        "id": "full_steps_mode_a",
        "description": "E3: Tagged steps progression mode (a) Mont127 (logN 16,20,24,28)",
        "cmd": f"'{script_dir}/steps.sh' --mode a --logN 16,20,24,28 --threads 1 --runs 5 --warmup 2 {step_tags_flag} --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_steps_mode_a'"
    })
    blocks.append({
        "id": "full_steps_mode_b",
        "description": "E3: Tagged steps progression mode (b) Mers127 (logN 16,20,24,28)",
        "cmd": f"'{script_dir}/steps.sh' --mode b --logN 16,20,24,28 --threads 1 --runs 5 --warmup 2 {step_tags_flag} --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_steps_mode_b'"
    })

    # E5: Slice sweep on Mont127 (s in {1, 2, 4, 8, 16, 32, 64, 128, 256}, logN 20,22,24,26,28, T 1,8)
    for t_count in [1, 8]:
      for slice_val in [1, 2, 4, 8, 16, 32, 64, 128, 256]:
        blocks.append({
            "id": f"full_slice_sweep_s{slice_val}_t{t_count}",
            "description": f"E5: Slice sweep s={slice_val} Mont127 T={t_count}",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 20,22,24,26,28 --threads {t_count} --slices {slice_val} --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_slice_sweep_s{slice_val}_t{t_count}'"
        })

    # E6: Thread scaling T in {1, 2, 4, 8, 15} at logN 20, 24, 26, 28, limited to
    # the physical cores the host profile exposes (8 benchmark cores -> T <= 8).
    # Block ids equal their output directory names.
    def _ncores(spec):
        n = 0
        for part in str(spec).split(','):
            part = part.strip()
            if not part:
                continue
            if '-' in part:
                lo, hi = part.split('-', 1)
                n += int(hi) - int(lo) + 1
            else:
                n += 1
        return n
    _avail = _ncores(os.environ.get('MULTI_CORES', '0'))
    for t_count in [t for t in [1, 2, 4, 8, 15] if t <= max(1, _avail)]:
        blocks.append({
            "id": f"full_this_work_threads_t{t_count}",
            "description": f"E6: Thread scaling This Work T={t_count}",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 20,24,26,28 --threads {t_count} --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_this_work_threads_t{t_count}'"
        })
        blocks.append({
            "id": f"full_lcpc_orig_threads_t{t_count}",
            "description": f"E6: Thread scaling Original LCPC T={t_count}",
            "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 20,24,26,28 --threads {t_count} --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_lcpc_orig_threads_t{t_count}'"
        })

    # E7: Field microbenchmarks
    # PolyCommit baseline; invoke through sh because the source file need not be executable.
    polycommit_script = os.path.join(script_dir, "polycommit.sh")
    blocks.append({
        "id": "full_polycommit_t1",
        "description": "E1: PolyCommit baseline comparison",
        "cmd": f"sh '{polycommit_script}' --field mont127 --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_polycommit_t1'"
    })
    blocks.append({
        "id": "full_polycommit_t8",
        "description": "E1: PolyCommit baseline comparison T=8",
        "cmd": f"sh '{polycommit_script}' --field mont127 --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_polycommit_t8'"
    })

    blocks.append({
        "id": "full_ligero_rs",
        "description": "E12: Same-library Ligero Reed-Solomon encoding",
        "cmd": f"'{script_dir}/this_work.sh' --params rs-ligero --field mont127 --logN 20,22,24 --threads 1 --runs 5 --warmup 2 --cores '{os.environ.get('CORES', '0')}' --host '{host}' --out '{session_dir}/full_ligero_rs'"
    })

# Pin each block by its thread count (see MULTI_CORES/SINGLE_CORE above):
# T=1 -> the single benchmark core; T>1 -> the first T cores of the multi-core
# set for every implementation (This Work pins worker i to core i; LCPC/rayon
# and PolyCommit threads then float over exactly T cores); SIMD-Spielman gets
# T+1 cores because its spin-waiting coordinator is pinned after the workers
# (capped at the set size, where the coordinator shares a worker's core).
import re

def expand(spec):
    cores = []
    for part in str(spec).split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            lo, hi = part.split("-", 1)
            cores.extend(range(int(lo), int(hi) + 1))
        else:
            cores.append(int(part))
    return sorted(set(cores))

multi = expand(os.environ["MULTI_CORES"])
for b in blocks:
    m = re.search(r"--threads (\d+)", b["cmd"])
    threads = int(m.group(1)) if m else 1
    b["threads"] = threads
    if threads > 1:
        need = threads + 1 if "simd_spielman.sh" in b["cmd"] else threads
        cores = ",".join(str(c) for c in multi[:need])
        # Fewer cores than threads (a small local core set): the block still
        # runs, but its threads share cores; analysis rejects such records.
        if len(multi) < threads:
            b["oversubscribed"] = True
    else:
        cores = os.environ["SINGLE_CORE"]
    b["cores"] = cores
    b["cmd"] = re.sub(r"--cores '[^']*'", f"--cores '{cores}'", b["cmd"])

# Staged campaigns: select blocks, intersect logN lists, override warm-ups.
block_filter = os.environ.get("STAGE_BLOCK_FILTER", "")
logn_filter = [x for x in os.environ.get("STAGE_LOGN_FILTER", "").split(",") if x]
warmup_override = os.environ.get("STAGE_WARMUP_OVERRIDE", "")
if block_filter:
    blocks = [b for b in blocks if re.search(block_filter, b["id"])]
if logn_filter:
    kept = []
    for b in blocks:
        m = re.search(r"--logN ([0-9,]+)", b["cmd"])
        if not m:
            continue
        keep = [x for x in m.group(1).split(",") if x in logn_filter]
        if not keep:
            continue
        # The suffix names the logN values the block actually runs (not the
        # whole filter), so block ids and output dirs describe their content.
        suffix = "_n" + "-".join(keep)
        b["cmd"] = b["cmd"].replace(m.group(0), "--logN " + ",".join(keep))
        b["cmd"] = re.sub(r"--out '([^']*)'", lambda o: f"--out '{o.group(1)}{suffix}'", b["cmd"])
        b["id"] += suffix
        kept.append(b)
    blocks = kept
if warmup_override:
    for b in blocks:
        b["cmd"] = re.sub(r"--warmup \d+", f"--warmup {warmup_override}", b["cmd"])
        if "--warmup" not in b["cmd"]:
            b["cmd"] = b["cmd"].replace(" --runs ", f" --warmup {warmup_override} --runs ", 1)

for b in blocks:
    m = re.search(r"--out '([^']*)'", b["cmd"])
    b["out"] = m.group(1) if m else session_dir
    # Block ids equal their output directory names (joinable to result paths),
    # and every block writes into its own directory directly below the session.
    if os.path.basename(b["out"]) != b["id"]:
        raise SystemExit(f"block id {b['id']} does not match its output dir {b['out']}")
    if os.path.dirname(b["out"]) != session_dir:
        raise SystemExit(f"block {b['id']} output dir {b['out']} is not directly below {session_dir}")

for b in blocks:
    if b.get("oversubscribed"):
        print(f"[WARN] Block {b['id']}: T={b['threads']} on cores {b['cores']} is oversubscribed")

# A block whose base id (without the _n<logN> suffix) already has an output
# dir with a different logN list in this session is a sibling: both dirs may
# hold the same configuration, so analysis must not select both (audit F8).
def base_id(block_id):
    return re.sub(r"_n[0-9]+(?:-[0-9]+)*$", "", block_id)
existing = []
if os.path.isdir(session_dir):
    existing = [d for d in os.listdir(session_dir)
                if os.path.isdir(os.path.join(session_dir, d)) and ".prev-" not in d and d != "stages"]
for b in blocks:
    b["siblings"] = sorted(d for d in existing if d != b["id"] and base_id(d) == base_id(b["id"]))
    if b["siblings"]:
        print(f"[WARN] Block {b['id']}: this session already has output dirs {b['siblings']} of the same "
              "block with a different logN list; select only one per configuration in analysis.")

# Randomize block order using the recorded seed (interleaving targets)
rng = random.Random(seed)
rng.shuffle(blocks)

schedule = {
    "profile": profile,
    "session_id": session_id,
    "stage": os.environ["STAGE_TAG"],
    "stage_label": os.environ["STAGE_LABEL"],
    "stage_started_utc": os.environ["STAGE_STARTED_UTC"],
    "seed": seed,
    "arguments": {
        "blocks": block_filter or None,
        "logn": ",".join(logn_filter) or None,
        "warmup": warmup_override or None,
        "lcpc_variants": os.environ.get("LCPC_VARIANTS") or None,
        "tags": tags_arg or None,
        "multi_cores": os.environ["MULTI_CORES"],
        "single_core": os.environ["SINGLE_CORE"],
    },
    "total_blocks": len(blocks),
    "blocks": blocks
}
with open(os.environ["SCHEDULE_FILE"], "w", encoding="utf-8") as f:
    json.dump(schedule, f, indent=2)
# Tab-separated block list for the shell loop (fields never contain tabs).
with open(os.environ["BLOCKS_TSV"], "w", encoding="utf-8") as f:
    for b in blocks:
        f.write("\t".join([b["id"], b["out"], b["cores"].split(",")[0], b["description"], b["cmd"]]) + "\n")

print(f"[INFO] Generated schedule of {len(blocks)} blocks randomized with seed {seed}.")
PY_SCHED

if [ "$DRY_RUN" -eq 1 ]; then
    log_info "Dry-run mode enabled. Scheduled block execution order:"
    python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    sched = json.load(f)
for i, b in enumerate(sched['blocks'], 1):
    print(f'  [{i}/{len(sched[\"blocks\"])}] {b[\"id\"]} (T={b[\"threads\"]}, cores {b[\"cores\"]}): {b[\"description\"]}')
" "$SCHEDULE_FILE"
    log_info "Schedule: $SCHEDULE_FILE"
    exit 0
fi

# Session-level schedule.json: append-only list of every stage run in this
# session (tag, filters, warm-up, variants, seed, block ids, per-block status
# and exit status). Analysis takes the session from this file's directory;
# stages/<tag>/schedule.json is the stage's own full schedule.
SESSION_SCHED_SCRIPT="$STAGE_DIR/session_schedule.py"
cat > "$SESSION_SCHED_SCRIPT" << 'PY_SESS'
import json, os, sys, datetime

def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

def load(path):
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None

def save(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    os.replace(tmp, path)

env = os.environ
path = env["SESSION_SCHEDULE"]
with open(env["SCHEDULE_FILE"], "r", encoding="utf-8") as f:
    stage_sched = json.load(f)
data = load(path)
if data is None or "stages" not in data:
    legacy = data
    data = {
        "format": "rbd-session-schedule-v1",
        "session_id": stage_sched["session_id"],
        "profile": stage_sched["profile"],
        "host": env["SESS_HOST"],
        "run_id": env["SESS_RUN_ID"],
        "note": ("Append-only: one entry per all.sh invocation (stage) of this session; each stage's "
                 "full schedule, manifest and execution log are under its stage_dir. Output dirs named "
                 "<block>.prev-<UTC> hold an earlier stage's run of that block and are not analysed."),
        "stages": [],
    }
    if legacy is not None:
        data["legacy_schedule"] = legacy
profiles = data.setdefault("profiles", [])
if stage_sched["profile"] not in profiles:
    profiles.append(stage_sched["profile"])

tag = stage_sched["stage"]
entry = next((e for e in data["stages"] if e.get("stage") == tag), None)
if entry is None:
    entry = {"stage": tag}
    data["stages"].append(entry)

session_dir = env["SESS_DIR"]
def rel(p):
    return os.path.relpath(p, session_dir) if p else None

if sys.argv[1] == "begin":
    entry.update({
        "stage_label": stage_sched["stage_label"],
        "stage_dir": rel(env["SESS_STAGE_DIR"]),
        "started_utc": stage_sched["stage_started_utc"],
        "finished_utc": None,
        "profile": stage_sched["profile"],
        "seed": stage_sched["seed"],
        "code_sha": env["SESS_CODE_SHA"],
        "instance": env["SESS_INSTANCE"],
        "arguments": stage_sched["arguments"],
        "block_ids": [b["id"] for b in stage_sched["blocks"]],
        "blocks": [{"id": b["id"], "out": rel(b["out"]), "threads": b["threads"], "cores": b["cores"],
                    "siblings": b.get("siblings", []), "status": "scheduled"} for b in stage_sched["blocks"]],
        "exit_status": None,
        "state": "running",
    })
else:
    rc = int(sys.argv[2])
    state = sys.argv[3] if len(sys.argv) > 3 else "finished"
    log = {}
    try:
        with open(env["SESS_EXEC_LOG"], "r", encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    item = json.loads(line)
                    log[item["block_id"]] = item
    except (OSError, ValueError):
        pass
    moved = {}
    try:
        with open(env["SESS_MOVED_TSV"], "r", encoding="utf-8") as f:
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) == 3:
                    moved[parts[0]] = parts[2]
    except OSError:
        pass
    for b in entry.get("blocks", []):
        item = log.get(b["id"])
        if item:
            b["status"] = item["status"]
            b["exit_code"] = item["exit_code"]
            b["duration_s"] = item["duration_s"]
        elif b.get("status") == "scheduled":
            b["status"] = "not_run"
        if b["id"] in moved:
            b["moved_previous_to"] = rel(moved[b["id"]])
    entry["finished_utc"] = now()
    entry["exit_status"] = rc
    entry["state"] = state
    entry["failed_blocks"] = [b["id"] for b in entry.get("blocks", []) if b.get("status") == "failed"]
save(path, data)
PY_SESS

session_schedule() {
    SESSION_SCHEDULE="$SESSION_SCHEDULE" SCHEDULE_FILE="$SCHEDULE_FILE" SESS_HOST="$HOST_NAME" \
    SESS_RUN_ID="$RUN_ID" SESS_DIR="$SESSION_DIR" SESS_STAGE_DIR="$STAGE_DIR" SESS_CODE_SHA="$CODE_SHA" \
    SESS_INSTANCE="$INSTANCE_NAME" SESS_EXEC_LOG="$STAGE_DIR/execution_log.jsonl" SESS_MOVED_TSV="$MOVED_TSV" \
        python3 "$SESSION_SCHED_SCRIPT" "$@"
}

SESSION_SCHEDULE_FINALIZED=0
on_stage_exit() {
    _rc=$?
    trap - EXIT
    if [ "$SESSION_SCHEDULE_FINALIZED" -eq 0 ]; then
        session_schedule end "$_rc" interrupted || log_warn "Could not finalize $SESSION_SCHEDULE"
    fi
    exit "$_rc"
}
: > "$STAGE_DIR/execution_log.jsonl"
session_schedule begin
trap on_stage_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

TAB=$(printf '\t')

# Execute scheduled blocks
TOTAL_BLOCKS=$(wc -l < "$BLOCKS_TSV" | tr -d ' ')
CURRENT_INDEX=1

# CPU frequency of the block's first pinned core. With a cpufreq driver this is
# the observed scaling_cur_freq; without one (e.g. many VM guests) only the
# constant 'cpu MHz' of /proc/cpuinfo exists, which is a nominal value and is
# labelled as such (freq_source / freq_is_nominal in every record).
cpu_freq_source() {
    if [ -r "/sys/devices/system/cpu/cpu$1/cpufreq/scaling_cur_freq" ]; then
        echo "observed: cpufreq scaling_cur_freq of CPU $1"
    elif [ -r /proc/cpuinfo ]; then
        echo "nominal: /proc/cpuinfo cpu MHz of CPU $1 (no cpufreq; not an observed clock)"
    else
        echo "unavailable"
    fi
}
get_cpu_freq() {
    if [ -r "/sys/devices/system/cpu/cpu$1/cpufreq/scaling_cur_freq" ]; then
        awk '{ printf "%.3f\n", $1 / 1000 }' "/sys/devices/system/cpu/cpu$1/cpufreq/scaling_cur_freq"
    elif [ -r /proc/cpuinfo ]; then
        awk -v cpu="$1" -F: '$1 ~ /^processor/ { p = $2 + 0 } $1 ~ /^cpu MHz/ && p == cpu { gsub(/ /, "", $2); print $2; exit }' /proc/cpuinfo
    fi
}

# Load and allocation snapshot of a shared node (host profiles that define
# host_noise_snapshot, e.g. the SoC cluster profiles); empty elsewhere.
noise_snapshot() {
    if command -v host_noise_snapshot >/dev/null 2>&1; then
        host_noise_snapshot
    fi
}

# execution_log.jsonl gets one line per finished block (crash-safe); it is
# converted to execution_log.json at the end of the stage.
EXECUTION_LOG_JSONL="$STAGE_DIR/execution_log.jsonl"

# Read the block list from a file, not a pipeline: a pipeline would run the
# loop in a subshell (dash) and lose ANY_BLOCK_FAILED. Block commands get
# </dev/null so they cannot consume the block list.
while IFS="$TAB" read -r block_id block_out block_core block_desc block_cmd; do
    log_info ""
    log_info ">>> Running Block [$CURRENT_INDEX/$TOTAL_BLOCKS]: $block_id <<<"
    log_info "Description: $block_desc"

    # Never mix two runs of a block: move an earlier output dir aside.
    BLOCK_PREV=""
    if [ -e "$block_out" ]; then
        _prev_ts=$(date -u +%Y%m%dT%H%M%SZ)
        BLOCK_PREV="$block_out.prev-$_prev_ts"
        _prev_n=1
        while [ -e "$BLOCK_PREV" ]; do
            BLOCK_PREV="$block_out.prev-$_prev_ts-$_prev_n"
            _prev_n=$((_prev_n + 1))
        done
        mv "$block_out" "$BLOCK_PREV"
        printf '%s\t%s\t%s\n' "$block_id" "$block_out" "$BLOCK_PREV" >> "$MOVED_TSV"
        log_warn "Block '$block_id' ran before in this session: moved its output dir to $(basename "$BLOCK_PREV") (not analysed)."
    fi

    FREQ_SOURCE=$(cpu_freq_source "$block_core")
    FREQ_BEFORE=$(get_cpu_freq "$block_core")
    NOISE_BEFORE=$(noise_snapshot)
    START_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    TIME_START=$(date -u +%s)

    if eval "$block_cmd" </dev/null; then
        BLOCK_RC=0
    else
        BLOCK_RC=$?
    fi

    TIME_END=$(date -u +%s)
    END_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    FREQ_AFTER=$(get_cpu_freq "$block_core")
    NOISE_AFTER=$(noise_snapshot)
    DURATION=$((TIME_END - TIME_START))

    if [ "$BLOCK_RC" -ne 0 ]; then
        # Record the failure and continue: every block runs its own correctness
        # gate before timing, and one failing baseline must not discard the rest
        # of a multi-hour session. Its outputs are tagged block_status=failed.
        BLOCK_STATUS=failed
        log_error "Block '$block_id' failed (exit $BLOCK_RC, duration: ${DURATION}s)! Continuing with the next block."
        printf '%s\t%s\t%s\t%s\n' "$block_id" "$DURATION" "$BLOCK_RC" "$STAGE_TAG" >> "$SESSION_DIR/failed_blocks.tsv"
        ANY_BLOCK_FAILED=1
    else
        BLOCK_STATUS=ok
        log_info "Block '$block_id' completed successfully in ${DURATION}s (CPU $block_core: ${FREQ_BEFORE:-?} MHz -> ${FREQ_AFTER:-?} MHz, $FREQ_SOURCE)."
    fi

    # Tag this block's own JSON records only (session, stage, block, status, frequency).
    python3 "$TAG_SCRIPT" "$block_out" "$SESSION_ID" "$SEED" "$WATERMARK" "$FREQ_BEFORE" "$FREQ_AFTER" \
        "$FREQ_SOURCE" "$block_id" "$BLOCK_STATUS" "$STAGE_TAG" || \
        log_warn "Could not tag the JSON records of block '$block_id'"

    python3 -c '
import json, sys
keys = ["block_id", "out", "status", "exit_code", "start_utc", "end_utc", "duration_s",
        "core_for_frequency", "freq_before_mhz", "freq_after_mhz", "freq_source", "moved_previous_to", "cmd",
        "noise_before", "noise_after"]
entry = dict(zip(keys, sys.argv[2:]))
entry["exit_code"] = int(entry["exit_code"])
entry["duration_s"] = int(entry["duration_s"])
entry["moved_previous_to"] = entry["moved_previous_to"] or None
for key in ("noise_before", "noise_after"):
    try:
        entry[key] = json.loads(entry.get(key) or "null")
    except ValueError:
        entry[key] = {"raw": entry[key]}
with open(sys.argv[1], "a", encoding="utf-8") as f:
    f.write(json.dumps(entry) + "\n")
' "$EXECUTION_LOG_JSONL" "$block_id" "$block_out" "$BLOCK_STATUS" "$BLOCK_RC" "$START_UTC" "$END_UTC" \
        "$DURATION" "$block_core" "$FREQ_BEFORE" "$FREQ_AFTER" "$FREQ_SOURCE" "$BLOCK_PREV" "$block_cmd" \
        "$NOISE_BEFORE" "$NOISE_AFTER" || \
        log_warn "Could not append block '$block_id' to $EXECUTION_LOG_JSONL"

    CURRENT_INDEX=$((CURRENT_INDEX + 1))
done < "$BLOCKS_TSV"

python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    entries = [json.loads(line) for line in f if line.strip()]
with open(sys.argv[2], "w", encoding="utf-8") as f:
    json.dump(entries, f, indent=2)
    f.write("\n")
' "$EXECUTION_LOG_JSONL" "$STAGE_DIR/execution_log.json"

# Stage manifest: host/toolchain/git provenance plus the stage's real
# arguments. Per-block parameters (threads, cores, logN, warm-ups) are in each
# block's own manifest and in schedule.json; the session-level pinning fields
# give the union of benchmark cores and the largest thread count scheduled.
log_info ""
log_info "Writing stage manifest..."
STAGE_MAX_THREADS=$(python3 -c 'import json,sys; b=json.load(open(sys.argv[1]))["blocks"]; print(max([x["threads"] for x in b] or [1]))' "$SCHEDULE_FILE")
STAGE_EXTRA="$STAGE_DIR/stage_extra.json"
STAGE_EXTRA_FILE="$STAGE_EXTRA" STAGE_SCHEDULE="$SCHEDULE_FILE" STAGE_ARGS_STRING="$STAGE_ARGS_STRING" \
STAGE_INSTANCE="$INSTANCE_NAME" STAGE_ANY_FAILED="$ANY_BLOCK_FAILED" python3 - <<'PY'
import json, os
with open(os.environ["STAGE_SCHEDULE"], encoding="utf-8") as f:
    sched = json.load(f)
extra = {
    "stage": sched["stage"],
    "stage_label": sched["stage_label"],
    "stage_started_utc": sched["stage_started_utc"],
    "profile": sched["profile"],
    "session_id": sched["session_id"],
    "seed": sched["seed"],
    "instance": os.environ["STAGE_INSTANCE"],
    "arguments": sched["arguments"],
    "arguments_string": os.environ["STAGE_ARGS_STRING"],
    "block_ids": [b["id"] for b in sched["blocks"]],
    "any_block_failed": os.environ["STAGE_ANY_FAILED"] == "1",
    "note": "benchmark_parameters.logN/warmup/threads describe the stage filters; per-block values are in schedule.json and each block's manifest",
}
with open(os.environ["STAGE_EXTRA_FILE"], "w", encoding="utf-8") as f:
    json.dump(extra, f, indent=2)
    f.write("\n")
PY
CORES="$MULTI_CORES"
THREADS="$STAGE_MAX_THREADS"
LOGN="${LOGN_FILTER:-per-block}"
WARMUP="${WARMUP_OVERRIDE:-per-block}"
RUNS="per-block"
PARAMS="per-block"
FIELD="per-block"
SLICES="per-block"
write_manifest "$STAGE_DIR/manifest.json" "$STAGE_EXTRA"

if [ "$PROFILE" = "smoke" ]; then
    # Smoke runs check that the analysis pipeline accepts every experiment
    # family. Campaign profiles do not analyse a single stage: paper artefacts
    # come from analyze.py over the selected run dirs (results/selection.json).
    log_info ""
    log_info "Generating smoke analysis tables via analyze.py..."
    ANALYSIS_DIR="$STAGE_DIR/analysis"
    mkdir -p "$ANALYSIS_DIR"
    mkdir -p "$CARGO_TARGET_DIR/matplotlib"
    MPLCONFIGDIR="$CARGO_TARGET_DIR/matplotlib" python3 "$CODE_DIR/scripts/analyze.py" \
        --results-dir "$SESSION_DIR" \
        --out-dir "$ANALYSIS_DIR" \
        --allow 'T2|Mers127|*KDHR26*' \
        --allow 'T2|Mers127|*LCPC*' \
        --allow 'T2|Fp2M61|*KDHR26*' \
        --allow 'T2|Fp2M61|*LCPC*' \
        --allow 'T5|Ft127|Encode N=2^* T=1 (ms)' \
        --allow 'T5|Ft127|Cost N=2^* (ns/MAC)' \
        --allow 'T5|*|Encode N=2^24 T=1 (ms)' \
        --allow 'T5|*|Cost N=2^24 (ns/MAC)' \
        --allow 'T8|SIMD-Spielman|Setup (ms)' \
        --allow 'T8|SIMD-Spielman|Setup / Encode' \
        --allow 'T8|SIMD-Spielman|Peak RSS (MB)' \
        --allow 'T9|Mont127|*Table 3*' \
        --allow 'T9|Mers127|*Table 3*' \
        --allow 'T12|*|This Work Arm (ms)' \
        --allow 'T12|*|Original LCPC * Arm (ms)' \
        --allow 'T12|*|Arm Speedup vs *' \
        --check || ANY_BLOCK_FAILED=1
else
    log_info "Analyse the campaign with scripts/analyze.py --selection results/selection.json --check over all selected run dirs."
fi

session_schedule end "$ANY_BLOCK_FAILED" finished || log_warn "Could not finalize $SESSION_SCHEDULE"
SESSION_SCHEDULE_FINALIZED=1

log_info "=========================================================="
if [ "$ANY_BLOCK_FAILED" -ne 0 ]; then
    log_error "Stage finished with FAILED blocks; see $SESSION_DIR/failed_blocks.tsv"
else
    log_info "Stage completed: all blocks succeeded."
fi
log_info "Profile:     $PROFILE"
log_info "Session ID:  $SESSION_ID"
log_info "Stage:       $STAGE_TAG"
log_info "Seed:        $SEED"
log_info "Results:     $SESSION_DIR"
log_info "Stage files: $STAGE_DIR"
log_info "Session schedule: $SESSION_SCHEDULE"
log_info "=========================================================="

if [ "$ANY_BLOCK_FAILED" -ne 0 ]; then
    exit 1
fi
exit 0
