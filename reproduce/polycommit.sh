#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$SCRIPT_DIR/common.sh"
. "$HOME/.cargo/env"
parse_args "$@"

POLYCOMMIT_COMMIT="1eaec5090ba5bc87d75a69570ed38f6fcf8b9417"
DRIVER_DIR="$CODE_DIR/external/polycommit"
DRIVER_MANIFEST="$DRIVER_DIR/Cargo.toml"
DRIVER_LOCK="$DRIVER_DIR/Cargo.lock"
TARGET_DIR="$CODE_DIR/target-eval-polycommit"

for tool in cargo rustc taskset python3 sha256sum git awk date; do
    command -v "$tool" >/dev/null 2>&1 || {
        log_error "Required command not found: $tool"
        exit 1
    }
done

[ -f "$DRIVER_MANIFEST" ] || {
    log_error "Missing driver manifest: $DRIVER_MANIFEST"
    exit 1
}
[ -f "$DRIVER_LOCK" ] || {
    log_error "Missing pinned driver lockfile: $DRIVER_LOCK"
    exit 1
}

if ! grep -Fq "rev = \"$POLYCOMMIT_COMMIT\"" "$DRIVER_MANIFEST"; then
    log_error "Cargo.toml does not pin the requested arkworks commit"
    exit 1
fi
if ! grep -Fq "$POLYCOMMIT_COMMIT" "$DRIVER_LOCK"; then
    log_error "Cargo.lock does not contain the requested arkworks commit"
    exit 1
fi

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac
CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD)
RUN_ID=$(date -u +%Y-%m-%d)-$CODE_SHA
DEST="$OUT_ROOT/$HOST_NAME/$RUN_ID/polycommit"
mkdir -p "$DEST" "$TARGET_DIR/tmp"
export CARGO_TARGET_DIR="$TARGET_DIR"
export TMPDIR="$TARGET_DIR/tmp"
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS

# Running Cargo from / keeps the workspace's scalar-codegen config and wrapper
# from changing this external baseline's normal release build.
log_info "Building pinned arkworks PolyCommit driver with rustc $(rustc --version)"
cd /
taskset -c "$CORES" cargo build --manifest-path "$DRIVER_MANIFEST" --locked --release
DRIVER_BINARY="$TARGET_DIR/release/polycommit-bench-driver"
[ -x "$DRIVER_BINARY" ] || {
    log_error "Expected benchmark binary not found: $DRIVER_BINARY"
    exit 1
}

log_info "Checking arkworks linear-encoder correctness at logN=$LOGN"
taskset -c "$CORES" "$DRIVER_BINARY" \
    --threads "$THREADS" --cores "$CORES" --logN "$LOGN" --check

RAW_JSON="$DEST/polycommit-${FIELD}-${LOGN}.json"
log_info "Writing benchmark samples to $RAW_JSON"
taskset -c "$CORES" "$DRIVER_BINARY" \
    --logN "$LOGN" --runs "$RUNS" --warmup "$WARMUP" \
    --threads "$THREADS" --cores "$CORES" --output "$RAW_JSON"

DRIVER_LOCK_SHA=$(sha256sum "$DRIVER_LOCK" | awk '{print $1}')
DRIVER_MANIFEST_SHA=$(sha256sum "$DRIVER_MANIFEST" | awk '{print $1}')
RAW_JSON_SHA=$(sha256sum "$RAW_JSON" | awk '{print $1}')
EXTRA_FILE="$DEST/polycommit_manifest_extra.json"
POLY_EXTRA_FILE="$EXTRA_FILE" \
POLY_LOCK_SHA="$DRIVER_LOCK_SHA" \
POLY_MANIFEST_SHA="$DRIVER_MANIFEST_SHA" \
POLY_RAW_SHA="$RAW_JSON_SHA" \
POLY_LOGN="$LOGN" \
POLY_THREADS="$THREADS" \
POLY_CORES="$CORES" \
POLY_RUNS="$RUNS" \
POLY_WARMUP="$WARMUP" \
POLYCOMMIT_COMMIT="$POLYCOMMIT_COMMIT" \
python3 - <<'PY'
import json
import os

extra = {
    "baseline": {
        "implementation": "arkworks-poly-commit-brakedown-encoding-only",
        "repository": "https://github.com/arkworks-rs/poly-commit",
        "upstream_commit": os.environ["POLYCOMMIT_COMMIT"],
        "driver_version": "0.1.0",
        "driver_cargo_manifest_sha256": os.environ["POLY_MANIFEST_SHA"],
        "driver_cargo_lock_sha256": os.environ["POLY_LOCK_SHA"],
        "raw_json_sha256": os.environ["POLY_RAW_SHA"],
        "field": {
            "name": "Mont127",
            "modulus_hex": "0x6e754097ba20e0bf7f2bd90000000001",
            "modulus_decimal": "146823888364060453008360742206866194433",
            "generator": "3",
        },
        "parameters": "BrakedownPCParams::default",
        "measurement_scope": "linear-code encoding of all coefficient-matrix rows; setup and input generation separate",
        "hashing": "not invoked; private marker hash types only instantiate arkworks generics",
        "logN": [int(value) for value in os.environ["POLY_LOGN"].split(",")],
        "threads": int(os.environ["POLY_THREADS"]),
        "cores": os.environ["POLY_CORES"],
        "runs": int(os.environ["POLY_RUNS"]),
        "warmup": int(os.environ["POLY_WARMUP"]),
    }
}
with open(os.environ["POLY_EXTRA_FILE"], "w", encoding="utf-8") as output:
    json.dump(extra, output, indent=2)
    output.write("\n")
PY

UPSTREAM_COMMIT="$POLYCOMMIT_COMMIT"
LCPC_COMMIT=""
PARAMS="polycommit-brakedown-default"
FIELD="Mont127"
SLICES="not-applicable"
write_manifest "$DEST/manifest.json" "$EXTRA_FILE"
log_info "PolyCommit raw JSON and manifest written under $DEST"
