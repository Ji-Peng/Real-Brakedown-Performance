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

for tool in cargo rustc taskset python3 sha256sum git; do
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
DEST="$OUT_ROOT/$HOST_NAME/$RUN_ID/steps"
mkdir -p "$DEST"

TARGET_BASE="${CARGO_TARGET_DIR:-$CODE_DIR/target-harness}"
mkdir -p "$TARGET_BASE"

# Collect list of step tags to evaluate
TAGS_TO_RUN=""
if [ -n "$SPECIFIC_TAGS" ]; then
    TAGS_TO_RUN=$(echo "$SPECIFIC_TAGS" | tr ',' ' ')
else
    for t in $(git -C "$CODE_DIR" tag -l "step/*" | sort -V); do
        case "$t" in
            step/00-import) ;; # Initial import, benchmarked via lcpc_orig.sh
            *) TAGS_TO_RUN="$TAGS_TO_RUN $t" ;;
        esac
    done

    # If HEAD is not an exact step tag, include HEAD as the final step
    LATEST_TAG=$(git -C "$CODE_DIR" describe --tags --exact-match HEAD 2>/dev/null || echo "")
    if [ -z "$LATEST_TAG" ]; then
        TAGS_TO_RUN="$TAGS_TO_RUN HEAD"
    fi
fi

log_info "=========================================================="
log_info "Step-by-Step Cumulative Technique Breakdown"
log_info "NOTE: Commits are cumulative in git history; measured deltas"
log_info "are order-dependent (e.g. R2L precedes lazy reduction)."
log_info "Tags to benchmark: $TAGS_TO_RUN"
log_info "Host: $HOST_NAME, Threads: $THREADS, Cores: $CORES, Params: $PARAMS, Field: $FIELD"
log_info "Destination: $DEST"
log_info "=========================================================="

CURRENT_WORKTREE=""
cleanup_worktree() {
    if [ -n "$CURRENT_WORKTREE" ] && [ -d "$CURRENT_WORKTREE" ]; then
        log_info "Cleaning up worktree: $CURRENT_WORKTREE"
        git -C "$CODE_DIR" worktree remove --force "$CURRENT_WORKTREE" >/dev/null 2>&1 || rm -rf "$CURRENT_WORKTREE"
        CURRENT_WORKTREE=""
    fi
}
trap cleanup_worktree EXIT INT TERM

STEP_INDEX=1
for tag in $TAGS_TO_RUN; do
    tag_clean=$(echo "$tag" | tr '/' '_')
    STEP_DEST="$DEST/$tag_clean"
    mkdir -p "$STEP_DEST"

    log_info "----------------------------------------------------------"
    log_info "Step [$STEP_INDEX]: Evaluating $tag ($tag_clean)"
    log_info "----------------------------------------------------------"

    # Create temporary detached worktree
    WORKTREE_DIR=$(mktemp -d "$TARGET_BASE/worktree-${tag_clean}-XXXXXX")
    CURRENT_WORKTREE="$WORKTREE_DIR"
    git -C "$CODE_DIR" worktree add --detach "$WORKTREE_DIR" "$tag" >/dev/null 2>&1

    TAG_COMMIT=$(git -C "$WORKTREE_DIR" rev-parse HEAD)
    TAG_SUBJECT=$(git -C "$WORKTREE_DIR" log -1 --pretty=format:"%s")

    # Check parameter and field compatibility for this tag
    EFFECTIVE_PARAMS="$PARAMS"
    EFFECTIVE_FIELD="$FIELD"

    # Before step/04-r2l, simd-spielman was not supported
    if [ "$PARAMS" = "simd-spielman" ]; then
        if ! grep -q "simd-spielman" "$WORKTREE_DIR/bench/src/main.rs" 2>/dev/null; then
            log_warn "Tag $tag does not support --params simd-spielman; falling back to lcpc-sdig6"
            EFFECTIVE_PARAMS="lcpc-sdig6"
        fi
    fi

    # Before step/02-fields, only ft127/mont127 was supported
    if [ "$FIELD" != "mont127" ] && [ "$FIELD" != "ft127" ]; then
        if ! grep -q "$FIELD" "$WORKTREE_DIR/bench/src/main.rs" 2>/dev/null; then
            log_warn "Tag $tag does not support --field $FIELD; falling back to mont127"
            EFFECTIVE_FIELD="mont127"
        fi
    fi

    TAG_TARGET="$TARGET_BASE/target-$tag_clean"
    mkdir -p "$TAG_TARGET"

    # Build
    log_info "Building $tag..."
    env CARGO_TARGET_DIR="$TAG_TARGET" taskset -c "$CORES" \
        cargo build --manifest-path "$WORKTREE_DIR/Cargo.toml" --release --bin lcpc-bench

    # Test
    log_info "Testing $tag..."
    env CARGO_TARGET_DIR="$TAG_TARGET" taskset -c "$CORES" \
        cargo test --manifest-path "$WORKTREE_DIR/Cargo.toml" --release || {
        log_error "Tests failed for tag $tag! Aborting."
        exit 1
    }

    # Benchmark
    BENCH_BIN="$TAG_TARGET/release/lcpc-bench"
    RAW_OUT="$STEP_DEST/${tag_clean}_${EFFECTIVE_PARAMS}_${EFFECTIVE_FIELD}_T${THREADS}.json"
    log_info "Benchmarking $tag -> $RAW_OUT"

    BENCH_ARGS="--params $EFFECTIVE_PARAMS --logN $LOGN --threads $THREADS --cores $CORES --runs $RUNS --warmup $WARMUP --output $RAW_OUT"

    if "$BENCH_BIN" --help 2>&1 | grep -q -- "--field"; then
        BENCH_ARGS="--field $EFFECTIVE_FIELD $BENCH_ARGS"
    fi
    if "$BENCH_BIN" --help 2>&1 | grep -q -- "--slices"; then
        BENCH_ARGS="--slices $SLICES $BENCH_ARGS"
    fi
    if [ "$AUTOTUNE" -eq 1 ]; then
        if "$BENCH_BIN" --help 2>&1 | grep -q -- "--autotune"; then
            BENCH_ARGS="--autotune $BENCH_ARGS"
        fi
    fi

    env RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" \
        "$BENCH_BIN" $BENCH_ARGS

    # Write per-step manifest
    STEP_MANIFEST="$STEP_DEST/manifest.json"
    PARAMS="$EFFECTIVE_PARAMS" FIELD="$EFFECTIVE_FIELD" write_manifest "$STEP_MANIFEST" "$RAW_OUT"

    # Add step metadata to step manifest
    python3 -c "
import json
with open('$STEP_MANIFEST', 'r') as f:
    m = json.load(f)
m['step_info'] = {
    'tag': '$tag',
    'step_index': $STEP_INDEX,
    'commit': '$TAG_COMMIT',
    'commit_subject': '$TAG_SUBJECT',
    'order_dependence_note': 'Cumulative commit sequence. Measurements depend on commit order.'
}
with open('$STEP_MANIFEST', 'w') as f:
    json.dump(m, f, indent=2)
"

    cleanup_worktree
    STEP_INDEX=$((STEP_INDEX + 1))
done

# Write overarching steps manifest
log_info "Writing combined steps manifest..."
write_manifest "$DEST/manifest.json"

log_info "=========================================================="
log_info "All steps successfully benchmarked."
log_info "Results saved in $DEST"
log_info "=========================================================="
