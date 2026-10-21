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
if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi

PROFILE=""
SESSION_ID=""
SEED=""
SPECIFIC_TAGS=""
DRY_RUN=0
PASSTHROUGH_ARGS=""

usage() {
    cat <<EOF
Usage: $0 [--profile full|arm|smoke] [--session <id>] [--seed <int>] [options]

Master campaign orchestrator for RealBrakedownPerf evaluation per plan 06 §5/§6:
  - full:  x86-64 primary campaign (E1-E13, logN 16..28, T 1..15, all baselines)
  - arm:   Arm portability campaign (E14: This Work + Original LCPC, logN 18,22,26, T 1,8)
  - smoke: Tiny dry-run across all targets, outputs watermarked "LOCAL SMOKE — NOT A RESULT"

Options:
  --profile full|arm|smoke Campaign workload profile (default: detected or smoke on local)
  --session <id>           Session ID recorded in every JSON report (default: session-YYYYMMDD-HHMMSS)
  --seed <int>             Random seed for interleaving block execution order (default: auto-generated)
  --host <name>            Host profile from reproduce/hosts/<name>.sh
  --out <dir>              Output directory (default: results)
  --tags <tags>            Comma-separated step tags for steps.sh progression
  --dry-run                Print scheduled execution blocks and exit without running
  -h, --help               Show this help message
EOF
}

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

# Auto-detect profile if not provided
if [ -z "$PROFILE" ]; then
    if [ "$HOST_NAME" = "gcp-c3d-highmem-30" ]; then
        PROFILE="full"
    elif [ "$HOST_NAME" = "gcp-c4a-highmem-16" ]; then
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
RUN_DATE=$(date -u +%Y-%m-%d)
RUN_ID="${RUN_DATE}-${CODE_SHA}"
SESSION_DIR="$OUT_ROOT/$HOST_NAME/$RUN_ID/$SESSION_ID"
mkdir -p "$SESSION_DIR"

WATERMARK=""
if [ "$PROFILE" = "smoke" ]; then
    WATERMARK="LOCAL SMOKE — NOT A RESULT"
fi

log_info "=========================================================="
log_info "Master Benchmark Campaign: reproduce/all.sh"
log_info "Profile:       $PROFILE"
log_info "Session ID:    $SESSION_ID"
log_info "Random Seed:   $SEED"
log_info "Host:          $HOST_NAME (test_only=$HOST_IS_TEST_ONLY)"
log_info "Session Dir:   $SESSION_DIR"
if [ -n "$WATERMARK" ]; then
    log_info "Watermark:     $WATERMARK"
fi
log_info "=========================================================="

# Python script to generate and randomize execution blocks, and tag JSON results
SCHEDULE_FILE="$SESSION_DIR/schedule.json"
TAG_SCRIPT="$SESSION_DIR/tag_json.py"

cat > "$TAG_SCRIPT" << 'PY_TAG'
import sys, os, json

def tag_file(path, session_id, seed, watermark=None, freq_before=None, freq_after=None, block_id=None):
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except Exception:
        return
    modified = False
    if isinstance(data, list):
        for item in data:
            if isinstance(item, dict):
                item["session_id"] = session_id
                item["session_seed"] = seed
                if block_id: item["block_id"] = block_id
                if freq_before: item["freq_before_mhz"] = freq_before
                if freq_after: item["freq_after_mhz"] = freq_after
                if watermark: item["watermark"] = watermark
        modified = True
    elif isinstance(data, dict):
        data["session_id"] = session_id
        data["session_seed"] = seed
        if block_id: data["block_id"] = block_id
        if freq_before: data["freq_before_mhz"] = freq_before
        if freq_after: data["freq_after_mhz"] = freq_after
        if watermark: data["watermark"] = watermark
        modified = True
    if modified:
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)

if __name__ == "__main__":
    target_dir = sys.argv[1]
    session_id = sys.argv[2]
    seed = sys.argv[3]
    watermark = sys.argv[4] if len(sys.argv) > 4 and sys.argv[4] else None
    freq_before = sys.argv[5] if len(sys.argv) > 5 and sys.argv[5] else None
    freq_after = sys.argv[6] if len(sys.argv) > 6 and sys.argv[6] else None
    block_id = sys.argv[7] if len(sys.argv) > 7 and sys.argv[7] else None

    for root, _, files in os.walk(target_dir):
        for fname in files:
            if fname.endswith(".json") and "manifest" not in fname and "schedule" not in fname:
                tag_file(os.path.join(root, fname), session_id, seed, watermark, freq_before, freq_after, block_id)
PY_TAG

# Build execution block schedule in Python
python3 - << PY_SCHED
import json, random, os

profile = "$PROFILE"
seed = int("$SEED")
host = "$HOST_NAME"
session_dir = "$SESSION_DIR"
script_dir = "$SCRIPT_DIR"
tags_arg = "$SPECIFIC_TAGS"

blocks = []

if profile == "smoke":
    # Tiny sizes, 1-2 runs, test-only local dry run
    blocks.append({
        "id": "smoke_this_work_mode_a_t1",
        "description": "This Work mode (a) tiny smoke T=1",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 12,14 --threads 1 --runs 2 --warmup 1 --slices adaptive --test-only --host '{host}' --out '{session_dir}/this_work'"
    })
    blocks.append({
        "id": "smoke_lcpc_orig_a_t1",
        "description": "Original LCPC baseline tiny smoke T=1",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 12,14 --threads 1 --runs 2 --warmup 1 --host '{host}' --out '{session_dir}/lcpc_orig'"
    })
    blocks.append({
        "id": "smoke_this_work_mode_b_t1",
        "description": "This Work mode (b) tiny smoke T=1",
        "cmd": f"'{script_dir}/this_work.sh' --params simd-spielman --field mont127 --logN 12,14 --threads 1 --runs 2 --warmup 1 --slices adaptive --test-only --host '{host}' --out '{session_dir}/this_work'"
    })
    blocks.append({
        "id": "smoke_steps",
        "description": "Steps progression smoke",
        "cmd": f"'{script_dir}/steps.sh' --params lcpc-sdig6 --field mont127 --logN 12 --threads 1 --runs 2 --warmup 1 --slices adaptive --tags 'step/01-modernize,HEAD' --test-only --host '{host}' --out '{session_dir}/steps'"
    })
    blocks.append({
        "id": "smoke_fields",
        "description": "Field arithmetic microbenchmarks smoke",
        "cmd": f"taskset -c 0 '{script_dir}/../target-harness/release/bench_fields' --field all --runs 1000 --output '{session_dir}/field_microbenchmarks.json'"
    })
elif profile == "arm":
    # E14: Arm portability campaign on Google Axion (c4a-highmem-16)
    # This Work + Original LCPC only; logN 18, 22, 26; T 1, 8
    blocks.append({
        "id": "arm_this_work_t1",
        "description": "This Work mode (a) on Arm T=1 (logN 18,22,26)",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 1 --runs 5 --warmup 2 --slices adaptive --host '{host}' --out '{session_dir}/this_work'"
    })
    blocks.append({
        "id": "arm_lcpc_orig_t1",
        "description": "Original LCPC mode (a) on Arm T=1 (logN 18,22,26)",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 1 --runs 5 --warmup 2 --host '{host}' --out '{session_dir}/lcpc_orig'"
    })
    blocks.append({
        "id": "arm_this_work_t8",
        "description": "This Work mode (a) on Arm T=8 (logN 18,22,26)",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 8 --runs 5 --warmup 2 --slices adaptive --host '{host}' --out '{session_dir}/this_work'"
    })
    blocks.append({
        "id": "arm_lcpc_orig_t8",
        "description": "Original LCPC mode (a) on Arm T=8 (logN 18,22,26)",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 18,22,26 --threads 8 --runs 5 --warmup 2 --host '{host}' --out '{session_dir}/lcpc_orig'"
    })
    blocks.append({
        "id": "arm_field_microbenchmarks",
        "description": "Field arithmetic microbenchmarks on Arm",
        "cmd": f"taskset -c 1 '{script_dir}/../target-harness/release/bench_fields' --field all --runs 100000 --output '{session_dir}/field_microbenchmarks.json'"
    })
else:
    # full: x86-64 primary campaign (c3d-highmem-30, E1-E13, logN 16..28)
    # E1: Headline comparison mode (a) Mont127 T=1 and T=8
    blocks.append({
        "id": "full_this_work_mode_a_t1",
        "description": "E1: This Work mode (a) Mont127 T=1 (logN 16..28)",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --warmup 2 --slices adaptive --host '{host}' --out '{session_dir}/this_work'"
    })
    blocks.append({
        "id": "full_lcpc_orig_mode_a_t1",
        "description": "E1: Original LCPC mode (a) Ft127 T=1 (logN 16..28)",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --warmup 2 --host '{host}' --out '{session_dir}/lcpc_orig'"
    })
    blocks.append({
        "id": "full_this_work_mode_a_t8",
        "description": "E1: This Work mode (a) Mont127 T=8 (logN 16..28)",
        "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --warmup 2 --slices adaptive --host '{host}' --out '{session_dir}/this_work'"
    })
    blocks.append({
        "id": "full_lcpc_orig_mode_a_t8",
        "description": "E1: Original LCPC mode (a) Ft127 T=8 (logN 16..28)",
        "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --warmup 2 --host '{host}' --out '{session_dir}/lcpc_orig'"
    })

    # E2: Comparison with SIMD-Spielman (mode b, Mont127, Mers127, Fp2M61)
    for field_name in ["mont127", "mers127", "fp2m61"]:
        blocks.append({
            "id": f"full_this_work_mode_b_{field_name}_t1",
            "description": f"E2: This Work mode (b) {field_name} T=1 (logN 16..28)",
            "cmd": f"'{script_dir}/this_work.sh' --params simd-spielman --field {field_name} --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --warmup 2 --slices adaptive --host '{host}' --out '{session_dir}/this_work'"
        })
        blocks.append({
            "id": f"full_this_work_mode_b_{field_name}_t8",
            "description": f"E2: This Work mode (b) {field_name} T=8 (logN 16..28)",
            "cmd": f"'{script_dir}/this_work.sh' --params simd-spielman --field {field_name} --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --warmup 2 --slices adaptive --host '{host}' --out '{session_dir}/this_work'"
        })
        blocks.append({
            "id": f"full_simd_spielman_{field_name}_t1",
            "description": f"E2: SIMD-Spielman AVX-512 {field_name} T=1 (logN 16..28)",
            "cmd": f"'{script_dir}/simd_spielman.sh' --field {field_name} --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --host '{host}' --out '{session_dir}/simd_spielman'"
        })
        blocks.append({
            "id": f"full_simd_spielman_{field_name}_t8",
            "description": f"E2: SIMD-Spielman AVX-512 {field_name} T=8 (logN 16..28)",
            "cmd": f"'{script_dir}/simd_spielman.sh' --field {field_name} --logN 16,18,20,22,24,26,28 --threads 8 --runs 5 --host '{host}' --out '{session_dir}/simd_spielman'"
        })

    # E3: Per-technique breakdown (steps.sh mode a Mont127 and mode b Mers127)
    step_tags_flag = f"--tags '{tags_arg}'" if tags_arg else ""
    blocks.append({
        "id": "full_steps_mode_a",
        "description": "E3: Tagged steps progression mode (a) Mont127 (logN 16,20,24,28)",
        "cmd": f"'{script_dir}/steps.sh' --params lcpc-sdig6 --field mont127 --logN 16,20,24,28 --threads 1 --runs 5 --warmup 2 {step_tags_flag} --host '{host}' --out '{session_dir}/steps'"
    })
    blocks.append({
        "id": "full_steps_mode_b",
        "description": "E3: Tagged steps progression mode (b) Mers127 (logN 16,20,24,28)",
        "cmd": f"'{script_dir}/steps.sh' --params simd-spielman --field mers127 --logN 16,20,24,28 --threads 1 --runs 5 --warmup 2 {step_tags_flag} --host '{host}' --out '{session_dir}/steps'"
    })

    # E5: Slice sweep on Mont127 (s in {1, 2, 4, 8, 16, 32, 64, 128, 256}, logN 20,22,24,26,28, T 1,8)
    for slice_val in [1, 2, 4, 8, 16, 32, 64, 128, 256]:
        blocks.append({
            "id": f"full_slice_sweep_s{slice_val}_t1",
            "description": f"E5: Slice sweep s={slice_val} Mont127 T=1",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 20,22,24,26,28 --threads 1 --slices {slice_val} --runs 5 --warmup 2 --host '{host}' --out '{session_dir}/this_work_slices'"
        })

    # E6: Thread scaling T in {1, 2, 4, 8, 15} at logN 20, 24, 28
    for t_count in [1, 2, 4, 8, 15]:
        blocks.append({
            "id": f"full_thread_scaling_this_work_t{t_count}",
            "description": f"E6: Thread scaling This Work T={t_count}",
            "cmd": f"'{script_dir}/this_work.sh' --params lcpc-sdig6 --field mont127 --logN 20,24,28 --threads {t_count} --runs 5 --warmup 2 --host '{host}' --out '{session_dir}/this_work_threads'"
        })
        blocks.append({
            "id": f"full_thread_scaling_lcpc_orig_t{t_count}",
            "description": f"E6: Thread scaling Original LCPC T={t_count}",
            "cmd": f"'{script_dir}/lcpc_orig.sh' --params lcpc-sdig6 --field mont127 --logN 20,24,28 --threads {t_count} --runs 5 --warmup 2 --host '{host}' --out '{session_dir}/lcpc_orig_threads'"
        })

    # E7: Field microbenchmarks
    blocks.append({
        "id": "full_field_microbenchmarks",
        "description": "E7: Field arithmetic microbenchmarks (mul, mac, reduce)",
        "cmd": f"taskset -c 1 '{script_dir}/../target-harness/release/bench_fields' --field all --runs 100000 --output '{session_dir}/field_microbenchmarks.json'"
    })

    # PolyCommit baseline (if driver script exists)
    polycommit_script = os.path.join(script_dir, "polycommit.sh")
    if os.path.isfile(polycommit_script) and os.access(polycommit_script, os.X_OK):
        blocks.append({
            "id": "full_polycommit_baseline",
            "description": "E1: PolyCommit baseline comparison",
            "cmd": f"'{polycommit_script}' --field mont127 --logN 16,18,20,22,24,26,28 --threads 1 --runs 5 --host '{host}' --out '{session_dir}/polycommit'"
        })

# Randomize block order using the recorded seed (interleaving targets)
rng = random.Random(seed)
rng.shuffle(blocks)

schedule = {
    "profile": profile,
    "session_id": "$SESSION_ID",
    "seed": seed,
    "total_blocks": len(blocks),
    "blocks": blocks
}
with open("$SCHEDULE_FILE", "w", encoding="utf-8") as f:
    json.dump(schedule, f, indent=2)

print(f"[INFO] Generated schedule of {len(blocks)} blocks randomized with seed {seed}.")
PY_SCHED

if [ "$DRY_RUN" -eq 1 ]; then
    log_info "Dry-run mode enabled. Scheduled block execution order:"
    python3 -c "
import json
with open('$SCHEDULE_FILE') as f:
    sched = json.load(f)
for i, b in enumerate(sched['blocks'], 1):
    print(f'  [{i}/{len(sched[\"blocks\"])}] {b[\"id\"]}: {b[\"description\"]}')
"
    exit 0
fi

# Execute scheduled blocks
TOTAL_BLOCKS=$(python3 -c "import json; print(len(json.load(open('$SCHEDULE_FILE'))['blocks']))")
CURRENT_INDEX=1

get_cpu_freq() {
    if [ -f /proc/cpuinfo ]; then
        grep -m1 "cpu MHz" /proc/cpuinfo | awk '{print $4}' 2>/dev/null || echo "unknown"
    else
        echo "unknown"
    fi
}

EXECUTION_LOG="$SESSION_DIR/execution_log.json"
echo "[" > "$EXECUTION_LOG"

python3 -c "
import json
with open('$SCHEDULE_FILE') as f:
    sched = json.load(f)
for b in sched['blocks']:
    print(f\"{b['id']}|{b['description']}|{b['cmd']}\")
" | while IFS='|' read -r block_id block_desc block_cmd; do
    log_info ""
    log_info ">>> Running Block [$CURRENT_INDEX/$TOTAL_BLOCKS]: $block_id <<<"
    log_info "Description: $block_desc"
    
    FREQ_BEFORE=$(get_cpu_freq)
    TIME_START=$(date -u +%s)
    
    # Run block command
    BLOCK_FAILED=0
    eval "$block_cmd" || BLOCK_FAILED=1
    
    TIME_END=$(date -u +%s)
    FREQ_AFTER=$(get_cpu_freq)
    DURATION=$((TIME_END - TIME_START))
    
    if [ "$BLOCK_FAILED" -ne 0 ]; then
        log_error "Block '$block_id' failed (duration: ${DURATION}s)!"
        exit 1
    fi
    log_info "Block '$block_id' completed successfully in ${DURATION}s (Freq: ${FREQ_BEFORE} MHz -> ${FREQ_AFTER} MHz)."
    
    # Tag generated JSON files with session_id, seed, watermark, frequencies, block_id
    python3 "$TAG_SCRIPT" "$SESSION_DIR" "$SESSION_ID" "$SEED" "$WATERMARK" "$FREQ_BEFORE" "$FREQ_AFTER" "$block_id"
    
    CURRENT_INDEX=$((CURRENT_INDEX + 1))
done

# Write combined session manifest
log_info ""
log_info "Writing combined session manifest..."
write_manifest "$SESSION_DIR/manifest.json"

# Run analysis and LaTeX table generation
log_info ""
log_info "Generating analysis tables, LaTeX documents, and figures via analyze.py..."
ANALYSIS_DIR="$SESSION_DIR/analysis"
mkdir -p "$ANALYSIS_DIR"
python3 "$CODE_DIR/scripts/analyze.py" \
    --results-dir "$SESSION_DIR" \
    --out-dir "$ANALYSIS_DIR" || {
    log_warn "Analysis script generated warnings or skipped empty tables."
}

log_info "=========================================================="
log_info "Master Campaign Session Completed Successfully!"
log_info "Profile:     $PROFILE"
log_info "Session ID:  $SESSION_ID"
log_info "Seed:        $SEED"
log_info "Results:     $SESSION_DIR"
log_info "Analysis:    $ANALYSIS_DIR"
log_info "=========================================================="
