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

# SIMD-Spielman test vectors for the step worktrees. Tags up to step/08-threads
# skip their tv/ checks with only a message when the vectors are not found, and
# their field test looks only at ../../SIMD-Spielman-for-ZKPs relative to the
# crate, i.e. at $TARGET_BASE for a worktree created below. Provide both the
# environment override and that path, and fail early if the vectors are absent.
TV_SOURCE="$(dirname -- "$CODE_DIR")/SIMD-Spielman-for-ZKPs"
if [ -z "${SIMD_SPIELMAN_TV_DIR:-}" ]; then
    SIMD_SPIELMAN_TV_DIR="$TV_SOURCE/tv"
fi
if [ ! -f "$SIMD_SPIELMAN_TV_DIR/bd_spielman_extension_N12_in.txt" ]; then
    log_error "SIMD-Spielman test vectors not found in $SIMD_SPIELMAN_TV_DIR (set SIMD_SPIELMAN_TV_DIR)"
    exit 1
fi
export SIMD_SPIELMAN_TV_DIR
ln -sfn "$(dirname -- "$SIMD_SPIELMAN_TV_DIR")" "$TARGET_BASE/SIMD-Spielman-for-ZKPs"

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
        cargo build --manifest-path "$WORKTREE_DIR/Cargo.toml" --release --locked --bin lcpc-bench

    # Test
    log_info "Testing $tag..."
    env CARGO_TARGET_DIR="$TAG_TARGET" taskset -c "$CORES" \
        cargo test --manifest-path "$WORKTREE_DIR/Cargo.toml" --release --locked || {
        log_error "Tests failed for tag $tag! Aborting."
        exit 1
    }

    # Record generated code for every historical step. The pre-R2L commits do not
    # expose the named kernels used by the strict checker; from step/04 onward, a
    # missing hot-kernel symbol or vector arithmetic is a failure.
    CODEGEN_DIR="$STEP_DEST/codegen"
    mkdir -p "$CODEGEN_DIR"
    CODEGEN_ARGS=""
    case "$tag" in
        step/01-modernize|step/02-fields|step/03-column-major) ;;
        *) CODEGEN_ARGS="--strict" ;;
    esac
    log_info "Checking generated code for $tag..."
    # shellcheck disable=SC2086
    taskset -c "$CORES" python3 "$CODE_DIR/scripts/check_codegen.py" \
        "$TAG_TARGET/release/lcpc-bench" $CODEGEN_ARGS \
        --output-json "$CODEGEN_DIR/codegen_report.json" \
        --output-listing "$CODEGEN_DIR/disassembly.txt" || {
        log_error "Codegen verification failed for $tag; refusing to benchmark."
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
STEP_TAG="$tag" \
STEP_COMMIT="$TAG_COMMIT" \
STEP_SUBJECT="$TAG_SUBJECT" \
STEP_WORKTREE="$WORKTREE_DIR" \
STEP_MANIFEST="$STEP_MANIFEST" \
STEP_INDEX="$STEP_INDEX" \
python3 - <<'PY'
import hashlib
import json
import os
import subprocess

manifest_path = os.environ["STEP_MANIFEST"]
with open(manifest_path, "r", encoding="utf-8") as f:
    m = json.load(f)
worktree = os.environ["STEP_WORKTREE"]
lockfile = os.path.join(worktree, "Cargo.lock")
if os.path.isfile(lockfile):
    with open(lockfile, "rb") as f:
        m["rust_toolchain"]["cargo_lock_sha256"] = hashlib.sha256(f.read()).hexdigest()
m["git"]["code_repo_sha"] = os.environ["STEP_COMMIT"]
m["git"]["code_repo_tags"] = os.environ["STEP_TAG"]
# The benchmarked source is the tag's worktree; the scripts driving the run
# (steps.sh, check_codegen.py) come from the main checkout, whose state
# write_manifest recorded. Keep both.
m["git"]["harness_repo_dirty"] = m["git"].pop("code_repo_dirty", None)
m["git"]["harness_repo_changed_paths"] = m["git"].pop("code_repo_changed_paths", None)
status = subprocess.run(
    ["git", "-C", worktree, "status", "--porcelain", "--untracked-files=no"],
    stdout=subprocess.PIPE, text=True, check=True,
).stdout
m["git"]["code_repo_dirty"] = bool(status.strip())
m["git"]["code_repo_changed_paths"] = [line[3:] for line in status.splitlines()]
m["step_info"] = {
    "tag": os.environ["STEP_TAG"],
    "step_index": int(os.environ["STEP_INDEX"]),
    "commit": os.environ["STEP_COMMIT"],
    "commit_subject": os.environ["STEP_SUBJECT"],
    "codegen_report": "codegen/codegen_report.json",
    "codegen_scope_note": "step/01 through step/03 predate the named R2L kernels; later steps require a matched hot kernel",
    "order_dependence_note": "Cumulative commit sequence. Measurements depend on commit order."
}
with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(m, f, indent=2)
    f.write("\n")
PY

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
