#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$SCRIPT_DIR/common.sh"
. "$HOME/.cargo/env"
parse_args "$@"

if [ "$PARAMS" != "lcpc-sdig6" ] && [ "$PARAMS" != "lcpc-sdig3" ]; then
    log_error "Original LCPC supports --params lcpc-sdig6 or lcpc-sdig3"
    exit 1
fi
if [ "$FIELD" != "mont127" ]; then
    log_error "Original-LCPC benchmark driver supports Ft127 only"
    exit 1
fi

for tool in cargo rustc taskset python3 sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || {
        log_error "Required command not found: $tool"
        exit 1
    }
done

DRIVER_DIR="$CODE_DIR/external/lcpc-orig"
GOLDEN_FILE="$CODE_DIR/tests/golden/lcpc-orig/golden-v1.json"
DRIVER_LOCK="$DRIVER_DIR/Cargo.lock"
if [ ! -f "$DRIVER_LOCK" ]; then
    log_error "Missing pinned driver lockfile: $DRIVER_LOCK"
    exit 1
fi
if [ ! -f "$GOLDEN_FILE" ]; then
    log_error "Missing golden vectors: $GOLDEN_FILE (run external/lcpc-orig/gen_golden.sh)"
    exit 1
fi

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac
CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD)
RUN_ID=$(date -u +%Y-%m-%d)-$CODE_SHA
DEST="$OUT_ROOT/$HOST_NAME/$RUN_ID/lcpc_orig"
mkdir -p "$DEST"
TARGET_ROOT="${CARGO_TARGET_DIR:-$CODE_DIR/target-lcpc-orig}"
mkdir -p "$TARGET_ROOT/tmp"
export TMPDIR="$TARGET_ROOT/tmp"

FORK_RUSTFLAGS="-C target-cpu=native -C no-vectorize-loops -C no-vectorize-slp"
unset RUSTFLAGS

build_variant() {
    variant_name=$1
    build_profile=$2
    hash_mode=$3
    target_dir="$TARGET_ROOT/$variant_name"

    log_info "Building original LCPC: $variant_name"
    if [ "$build_profile" = "plain-release" ]; then
        if [ "$hash_mode" = "portable-only" ]; then
            env CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target_dir" RUSTC_BOOTSTRAP=lcpc_brakedown_pc \
                RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" cargo build \
                --manifest-path "$DRIVER_DIR/Cargo.toml" --locked --release \
                --features portable-only
        else
            env CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target_dir" RUSTC_BOOTSTRAP=lcpc_brakedown_pc \
                RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" cargo build \
                --manifest-path "$DRIVER_DIR/Cargo.toml" --locked --release
        fi
        binary="$target_dir/release/lcpc-orig-driver"
    else
        if [ "$hash_mode" = "portable-only" ]; then
            env CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target_dir" RUSTC_BOOTSTRAP=lcpc_brakedown_pc \
                RUSTFLAGS="$FORK_RUSTFLAGS" RAYON_NUM_THREADS="$THREADS" \
                taskset -c "$CORES" cargo build \
                --manifest-path "$DRIVER_DIR/Cargo.toml" --locked \
                --profile fork-release --features portable-only
        else
            env CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target_dir" RUSTC_BOOTSTRAP=lcpc_brakedown_pc \
                RUSTFLAGS="$FORK_RUSTFLAGS" RAYON_NUM_THREADS="$THREADS" \
                taskset -c "$CORES" cargo build \
                --manifest-path "$DRIVER_DIR/Cargo.toml" --locked \
                --profile fork-release
        fi
        binary="$target_dir/fork-release/lcpc-orig-driver"
    fi

    [ -x "$binary" ] || {
        log_error "Expected driver binary not found: $binary"
        exit 1
    }

    log_info "Checking pinned LCPC golden vectors with $variant_name"
    env RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" "$binary" \
        --threads "$THREADS" --cores "$CORES" --check-golden "$GOLDEN_FILE"

    raw_name="${PARAMS}-${build_profile}-blake3-${hash_mode}.json"
    log_info "Benchmark output: $DEST/$raw_name"
    env RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" "$binary" \
        --params "$PARAMS" --logN "$LOGN" --runs "$RUNS" \
        --warmup "$WARMUP" --threads "$THREADS" --cores "$CORES" \
        --output "$DEST/$raw_name"
}

# LCPC_VARIANTS (comma list) restricts the measured build variants; default all four.
LCPC_VARIANTS="${LCPC_VARIANTS:-as-published-as-is,as-published-portable,fork-flags-as-is,fork-flags-portable}"
export LCPC_VARIANTS
run_variant() {
    case ",$LCPC_VARIANTS," in
        *",$1,"*) build_variant "$@" ;;
        *) log_info "Skipping original LCPC variant $1 (not in LCPC_VARIANTS)" ;;
    esac
}
run_variant as-published-as-is plain-release as-is
run_variant as-published-portable plain-release portable-only
run_variant fork-flags-as-is fork-release as-is
run_variant fork-flags-portable fork-release portable-only

DRIVER_LOCK_SHA=$(sha256sum "$DRIVER_LOCK" | awk '{print $1}')
DRIVER_MANIFEST_SHA=$(sha256sum "$DRIVER_DIR/Cargo.toml" | awk '{print $1}')
EXTRA_FILE="$DEST/lcpc_orig_manifest_extra.json"
LCPC_EXTRA_FILE="$EXTRA_FILE" \
LCPC_DRIVER_LOCK_SHA="$DRIVER_LOCK_SHA" \
LCPC_DRIVER_MANIFEST_SHA="$DRIVER_MANIFEST_SHA" \
LCPC_RUSTFLAGS="$FORK_RUSTFLAGS" \
LCPC_PARAMS="$PARAMS" \
LCPC_LOGN="$LOGN" \
LCPC_THREADS="$THREADS" \
LCPC_CORES="$CORES" \
LCPC_RUNS="$RUNS" \
LCPC_WARMUP="$WARMUP" \
LCPC_BOOTSTRAP="lcpc_brakedown_pc" \
python3 - <<'PY'
import json
import os

variants = {}
for name, profile, flags, features in [
    ("as-published-as-is", "release", "Cargo defaults", ["std", "traits-preview"]),
    ("as-published-portable", "release", "Cargo defaults", ["std", "traits-preview", "pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]),
    ("fork-flags-as-is", "fork-release", os.environ["LCPC_RUSTFLAGS"], ["std", "traits-preview"]),
    ("fork-flags-portable", "fork-release", os.environ["LCPC_RUSTFLAGS"], ["std", "traits-preview", "pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]),
]:
    if name not in os.environ["LCPC_VARIANTS"].split(","):
        continue
    variants[name] = {
        "cargo_profile": profile,
        "rustflags": flags,
        "blake3_version": "1.8.7",
        "blake3_features": features,
        "auto_vectorization": "enabled" if name.startswith("as-published") else "disabled",
    }
extra = {
    "baseline": {
        "upstream_commit": "f0e2116001af5a1bbf39481535f837d6a25772e8",
        "driver_version": "0.1.0",
        "driver_cargo_manifest_sha256": os.environ["LCPC_DRIVER_MANIFEST_SHA"],
        "driver_cargo_lock_sha256": os.environ["LCPC_DRIVER_LOCK_SHA"],
        "rustc_bootstrap_crate_scope": os.environ["LCPC_BOOTSTRAP"],
        "params": os.environ["LCPC_PARAMS"],
        "logN": [int(x) for x in os.environ["LCPC_LOGN"].split(",")],
        "threads": int(os.environ["LCPC_THREADS"]),
        "cores": os.environ["LCPC_CORES"],
        "runs": int(os.environ["LCPC_RUNS"]),
        "warmup": int(os.environ["LCPC_WARMUP"]),
        "variants": variants,
    }
}
with open(os.environ["LCPC_EXTRA_FILE"], "w", encoding="utf-8") as f:
    json.dump(extra, f, indent=2)
    f.write("\n")
PY

write_manifest "$DEST/manifest.json" "$EXTRA_FILE"
log_info "Original LCPC raw JSON and manifest written under $DEST"
