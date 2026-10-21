#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/common.sh"
. "$HOME/.cargo/env"
SPECIFIC_TAGS=""
PASSTHROUGH_ARGS=""
while [ $# -gt 0 ]; do
    case "$1" in
        --tags)
            [ $# -ge 2 ] || { log_error "Option --tags requires an argument"; exit 1; }
            SPECIFIC_TAGS="$2"
            shift 2
            ;;
        *)
            PASSTHROUGH_ARGS="$PASSTHROUGH_ARGS $1"
            shift
            ;;
    esac
done

# shellcheck disable=SC2086
parse_args $PASSTHROUGH_ARGS

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac

CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD)
RUN_ID=$(date -u +%Y-%m-%d)-$CODE_SHA
SESSION_DIR="$OUT_ROOT/$HOST_NAME/$RUN_ID"
mkdir -p "$SESSION_DIR"

log_info "=========================================================="
log_info "Master Benchmark Campaign: All Targets in One Session"
log_info "Targets: This Work (HEAD), Tagged Steps, Original LCPC, SIMD-Spielman"
log_info "Host: $HOST_NAME, Threads: $THREADS, Cores: $CORES, Params: $PARAMS, Field: $FIELD"
log_info "Session Directory: $SESSION_DIR"
log_info "=========================================================="

COMMON_ARGS="--threads $THREADS --logN $LOGN --cores $CORES --runs $RUNS --warmup $WARMUP --params $PARAMS --field $FIELD --slices $SLICES --host $HOST_PROFILE"
if [ "$TEST_ONLY" -eq 1 ]; then
    COMMON_ARGS="$COMMON_ARGS --test-only"
fi
if [ "$AUTOTUNE" -eq 1 ]; then
    COMMON_ARGS="$COMMON_ARGS --autotune"
fi

# 1. This Work (Fork HEAD)
log_info ""
log_info ">>> Running Target 1/4: This Work (Fork HEAD) <<<"
"$SCRIPT_DIR/this_work.sh" $COMMON_ARGS --out "$SESSION_DIR/this_work"

# 2. Tagged Commit Steps (step/01 .. HEAD)
log_info ""
log_info ">>> Running Target 2/4: Tagged Step Progression <<<"
STEPS_ARGS="$COMMON_ARGS"
if [ -n "$SPECIFIC_TAGS" ]; then
    STEPS_ARGS="$STEPS_ARGS --tags $SPECIFIC_TAGS"
fi
"$SCRIPT_DIR/steps.sh" $STEPS_ARGS --out "$SESSION_DIR/steps"

# 3. Original LCPC (f0e2116)
log_info ""
log_info ">>> Running Target 3/4: Original LCPC Baseline <<<"
# Original LCPC driver runs mode (a) on mont127/ft127
LCPC_PARAMS="$PARAMS"
if [ "$PARAMS" = "simd-spielman" ]; then
    LCPC_PARAMS="lcpc-sdig6"
fi
"$SCRIPT_DIR/lcpc_orig.sh" --threads "$THREADS" --logN "$LOGN" --cores "$CORES" --runs "$RUNS" --warmup "$WARMUP" --params "$LCPC_PARAMS" --field "mont127" --host "$HOST_PROFILE" --out "$SESSION_DIR/lcpc_orig"

# 4. SIMD-Spielman (AVX-512 IFMA)
log_info ""
log_info ">>> Running Target 4/4: SIMD-Spielman AVX-512 Baseline <<<"
if ! grep -q "avx512ifma" /proc/cpuinfo 2>/dev/null; then
    log_warn "Host does not have avx512ifma flag; skipping SIMD-Spielman AVX-512 target cleanly."
elif ! echo '#include <boost/multiprecision/cpp_int.hpp>' | g++ -x c++ -E - >/dev/null 2>&1; then
    log_warn "Boost headers (<boost/multiprecision/cpp_int.hpp>) not installed; skipping SIMD-Spielman AVX-512 target."
else
    "$SCRIPT_DIR/simd_spielman.sh" --threads "$THREADS" --logN "$LOGN" --cores "$CORES" --runs "$RUNS" --field "$FIELD" --slices "$SLICES" --host "$HOST_PROFILE" --out "$SESSION_DIR/simd_spielman" || {
        log_error "SIMD-Spielman execution failed; campaign is incomplete."
        exit 1
    }
fi

# 5. Combined session manifest
log_info ""
log_info "Writing combined session manifest..."
write_manifest "$SESSION_DIR/manifest.json"

# 6. Analysis and report generation
log_info ""
log_info "Generating analysis tables and charts..."
ANALYSIS_DIR="$SESSION_DIR/analysis"
mkdir -p "$ANALYSIS_DIR"
python3 "$CODE_DIR/scripts/analyze.py" \
    --results-dir "$SESSION_DIR" \
    --out-dir "$ANALYSIS_DIR" || {
    log_error "Analysis generation failed; campaign artifacts are incomplete."
    exit 1
}

log_info "=========================================================="
log_info "Master Campaign Completed Successfully!"
log_info "Results and manifest: $SESSION_DIR"
log_info "Analysis and tables:  $ANALYSIS_DIR"
log_info "=========================================================="
