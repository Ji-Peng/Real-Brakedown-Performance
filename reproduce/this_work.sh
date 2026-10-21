#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/common.sh"
. "$HOME/.cargo/env"
parse_args "$@"

for tool in cargo rustc taskset python3 sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || {
        log_error "Required command not found: $tool"
        exit 1
    }
done

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac

CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD)
RUN_ID=$(date -u +%Y-%m-%d)-$CODE_SHA
DEST="$OUT_ROOT/$HOST_NAME/$RUN_ID/this_work"
mkdir -p "$DEST"

TARGET_DIR="${CARGO_TARGET_DIR:-$CODE_DIR/target-harness}"
mkdir -p "$TARGET_DIR"

log_info "=========================================================="
log_info "This Work (LCPC Fork HEAD): Build, Test, Codegen, Benchmark"
log_info "Host: $HOST_NAME, Threads: $THREADS, Cores: $CORES, Params: $PARAMS, Field: $FIELD"
log_info "Destination: $DEST"
log_info "=========================================================="

# 1. Build
log_info "Step 1/5: Building workspace in release mode..."
env CARGO_TARGET_DIR="$TARGET_DIR" taskset -c "$CORES" \
    cargo build --workspace --release --locked

# 2. Correctness tests (abort on failure!)
log_info "Step 2/5: Running workspace correctness test suite..."
env CARGO_TARGET_DIR="$TARGET_DIR" taskset -c "$CORES" \
    cargo test --workspace --release --locked || {
    log_error "Workspace tests failed! Aborting benchmark run."
    exit 1
}

# 3. Disassembly inspection / codegen verification
log_info "Step 3/5: Checking disassembly for scalar codegen..."
CODEGEN_DIR="$DEST/codegen"
mkdir -p "$CODEGEN_DIR"
taskset -c "$CORES" python3 "$CODE_DIR/scripts/check_codegen.py" \
    "$TARGET_DIR/release/lcpc-bench" \
    --strict \
    --output-json "$CODEGEN_DIR/codegen_report.json" \
    --output-listing "$CODEGEN_DIR/disassembly.txt" || {
    log_error "Codegen verification failed; refusing to benchmark (see $CODEGEN_DIR/codegen_report.json)"
    exit 1
}

# 4. Prover/verifier encoding and PCS benchmarks
log_info "Step 4/5: Running encoding and PCS benchmarks..."
RAW_OUT="$DEST/this_work_${PARAMS}_${FIELD}_T${THREADS}.json"
BENCH_RUN_ARGS="--field $FIELD --params $PARAMS --logN $LOGN --threads $THREADS --cores $CORES --slices $SLICES --runs $RUNS --warmup $WARMUP --output $RAW_OUT"
if [ "$AUTOTUNE" -eq 1 ]; then
    BENCH_RUN_ARGS="$BENCH_RUN_ARGS --autotune"
fi
env RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" \
    "$TARGET_DIR/release/lcpc-bench" $BENCH_RUN_ARGS

# Also run field microbenchmarks
log_info "Step 5/5: Running field arithmetic microbenchmarks..."
FIELD_OUT="$DEST/field_microbenchmarks.json"
env taskset -c "$CORES" \
    "$TARGET_DIR/release/bench_fields" \
    --field "$FIELD" \
    --runs "$((RUNS * 10000))" \
    --output "$FIELD_OUT"

# 5. Write manifest
log_info "Writing run manifest..."
write_manifest "$DEST/manifest.json" "$RAW_OUT"

log_info "=========================================================="
log_info "This Work benchmark campaign complete."
log_info "Raw JSON results: $RAW_OUT"
log_info "Manifest:         $DEST/manifest.json"
log_info "Codegen report:   $CODEGEN_DIR/codegen_report.json"
log_info "=========================================================="
