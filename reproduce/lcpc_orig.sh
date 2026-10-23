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
# Absolute, because the builds run from / (see build_variant).
TARGET_ROOT=$(CDPATH= cd -- "$TARGET_ROOT" && pwd)
export TMPDIR="$TARGET_ROOT/tmp"

# The fork-flags variants use exactly the rustflags that the fork's committed
# .cargo/config.toml applies to This Work on this host (target-cpu=native and
# no auto-vectorization on x86_64; only no auto-vectorization on aarch64).
FORK_RUSTFLAGS=$(fork_config_rustflags)
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
# Per-variant build records for the manifest (scratch file, not a result).
VARIANT_RECORDS=$(mktemp "$TMPDIR/lcpc_orig_variants.XXXXXX")
trap 'rm -f "$VARIANT_RECORDS"' 0

build_variant() {
    variant_name=$1
    build_profile=$2
    hash_mode=$3
    target_dir="$TARGET_ROOT/$variant_name"
    if [ "$build_profile" = "plain-release" ]; then
        cargo_profile=release
        variant_rustflags=""
    else
        cargo_profile=fork-release
        variant_rustflags=$FORK_RUSTFLAGS
    fi
    if [ "$hash_mode" = "portable-only" ]; then
        feature_args="--features portable-only"
    else
        feature_args=""
    fi

    # Build from / so that Cargo does not pick up code/.cargo/config.toml (its
    # rustflags and rustc-wrapper): Cargo searches config files from the
    # current directory, not from --manifest-path. The as-published variants
    # thus get true Cargo defaults (empty rustflags, auto-vectorization on,
    # generic target CPU); the fork-flags variants get FORK_RUSTFLAGS explicitly.
    # Without the wrapper, the crate-scoped bootstrap for the pinned upstream
    # crate's `#![feature(test)]` is passed directly.
    log_info "Building original LCPC: $variant_name (cargo profile $cargo_profile, RUSTFLAGS='$variant_rustflags', cwd /)"
    # shellcheck disable=SC2086 # feature_args is empty or two words
    (cd / && env CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target_dir" RUSTC_BOOTSTRAP=lcpc_brakedown_pc \
        RUSTFLAGS="$variant_rustflags" RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" cargo build \
        --manifest-path "$DRIVER_DIR/Cargo.toml" --locked --profile "$cargo_profile" $feature_args)
    binary="$target_dir/$cargo_profile/lcpc-orig-driver"

    [ -x "$binary" ] || {
        log_error "Expected driver binary not found: $binary"
        exit 1
    }
    # Record the rustflags Cargo actually used, from the build fingerprint.
    effective_flags_json=$(cargo_fingerprint_rustflags "$target_dir/$cargo_profile" lcpc-orig-driver)
    log_info "Effective rustflags for $variant_name: $effective_flags_json"

    log_info "Checking pinned LCPC golden vectors with $variant_name"
    env RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" "$binary" \
        --threads "$THREADS" --cores "$CORES" --check-golden "$GOLDEN_FILE"

    raw_name="${PARAMS}-${build_profile}-blake3-${hash_mode}.json"
    log_info "Benchmark output: $DEST/$raw_name"
    env RAYON_NUM_THREADS="$THREADS" taskset -c "$CORES" "$binary" \
        --params "$PARAMS" --logN "$LOGN" --runs "$RUNS" \
        --warmup "$WARMUP" --threads "$THREADS" --cores "$CORES" \
        --output "$DEST/$raw_name"

    # Stamp every record with its build variant and the flags actually used.
    VARIANT_JSON="$DEST/$raw_name" VARIANT_NAME="$variant_name" VARIANT_FLAGS="$effective_flags_json" \
    python3 - <<'PY'
import json, os
path = os.environ["VARIANT_JSON"]
flags = " ".join(json.loads(os.environ["VARIANT_FLAGS"]))
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
for item in (data if isinstance(data, list) else [data]):
    if isinstance(item, dict):
        item["build_variant"] = os.environ["VARIANT_NAME"]
        item["effective_rustflags"] = flags
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
PY
    printf '%s\t%s\t%s\t%s\n' "$variant_name" "$cargo_profile" "$hash_mode" "$effective_flags_json" >> "$VARIANT_RECORDS"
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
LCPC_VARIANT_RECORDS="$VARIANT_RECORDS" \
LCPC_DRIVER_LOCK_SHA="$DRIVER_LOCK_SHA" \
LCPC_DRIVER_MANIFEST_SHA="$DRIVER_MANIFEST_SHA" \
LCPC_FORK_RUSTFLAGS="$FORK_RUSTFLAGS" \
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

base_features = ["std", "traits-preview"]
portable_features = base_features + ["pure", "no_sse2", "no_sse41", "no_avx2", "no_avx512"]
variants = {}
with open(os.environ["LCPC_VARIANT_RECORDS"], "r", encoding="utf-8") as f:
    for line in f:
        name, profile, hash_mode, flags_json = line.rstrip("\n").split("\t")
        flags = json.loads(flags_json)
        variants[name] = {
            "cargo_profile": profile,
            "build_cwd": "/ (code/.cargo/config.toml and its rustc-wrapper do not apply)",
            "requested_rustflags": "" if name.startswith("as-published") else os.environ["LCPC_FORK_RUSTFLAGS"],
            "effective_rustflags": " ".join(flags),
            "effective_rustflags_source": "Cargo fingerprint of the built lcpc-orig-driver binary",
            "blake3_version": "1.8.7",
            "blake3_features": portable_features if hash_mode == "portable-only" else base_features,
            "auto_vectorization": "disabled" if "no-vectorize-loops" in flags else "enabled",
            "target_cpu": "native" if "target-cpu=native" in flags else "Cargo default (generic for the host triple)",
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
rm -f "$VARIANT_RECORDS"

# The driver is not built under code/.cargo/config.toml; record that instead of
# the fork's configured flags (per-variant flags are in extra.baseline.variants).
MANIFEST_RUST_OVERRIDES_JSON='{"effective_rustflags": null, "rustflags_source": "per build variant: extra.baseline.variants.<variant>.effective_rustflags (built from /, code/.cargo/config.toml not applied)", "cargo_rustc_wrapper": null}'
export MANIFEST_RUST_OVERRIDES_JSON
write_manifest "$DEST/manifest.json" "$EXTRA_FILE"
log_info "Original LCPC raw JSON and manifest written under $DEST"
