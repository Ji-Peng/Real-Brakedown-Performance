#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$SCRIPT_DIR/common.sh"
. "$HOME/.cargo/env"
parse_args "$@"
REFERENCE_FIELD=$FIELD
FIELD="Mont127"

POLYCOMMIT_COMMIT="1eaec5090ba5bc87d75a69570ed38f6fcf8b9417"
EXPECTED_PATCH_SHA256="15ba42ea9ab990a707e4efb2e983420f1e810b9a587618cec5595f04b01a75d1"
DRIVER_DIR="$CODE_DIR/external/polycommit"
DRIVER_MANIFEST="$DRIVER_DIR/Cargo.toml"
DRIVER_LOCK="$DRIVER_DIR/Cargo.lock"
PATCH_FILE="$DRIVER_DIR/polycommit.patch"
SEC_PARAM=${POLYCOMMIT_SEC_PARAM:-80}
TARGET_DIR="${CARGO_TARGET_DIR:-$CODE_DIR/target-eval-polycommit}/polycommit"

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac
CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD)
RUN_ID=$(date -u +%Y-%m-%d)-$CODE_SHA
DEST="$OUT_ROOT/$HOST_NAME/$RUN_ID/polycommit"
mkdir -p "$DEST"

for tool in cargo rustc taskset python3 sha256sum git awk date tar mktemp; do
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
[ -f "$PATCH_FILE" ] || { log_error "Missing PolyCommit patch: $PATCH_FILE"; exit 1; }

if ! grep -Fq "rev = \"$POLYCOMMIT_COMMIT\"" "$DRIVER_MANIFEST"; then
    log_error "Cargo.toml does not pin the requested arkworks commit"
    exit 1
fi
if ! grep -Fq "$POLYCOMMIT_COMMIT" "$DRIVER_LOCK"; then
    log_error "Cargo.lock does not contain the requested arkworks commit"
    exit 1
fi

PATCH_SHA256=$(sha256sum "$PATCH_FILE" | awk '{print $1}')
if [ "$PATCH_SHA256" != "$EXPECTED_PATCH_SHA256" ]; then
    log_error "PolyCommit patch SHA-256 mismatch (expected $EXPECTED_PATCH_SHA256, got $PATCH_SHA256)"
    exit 1
fi
case "$SEC_PARAM" in
    ''|*[!0-9]*|0) log_error "POLYCOMMIT_SEC_PARAM must be a positive integer"; exit 1 ;;
esac

mkdir -p "$DEST" "$TARGET_DIR/tmp"
export CARGO_TARGET_DIR="$TARGET_DIR"
export TMPDIR="$TARGET_DIR/tmp"
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS

SOURCE_REPO=
CARGO_GIT_ROOT="${CARGO_HOME:-$HOME/.cargo}/git/checkouts"
for candidate in "$CARGO_GIT_ROOT"/poly-commit-*/1eaec50; do
    [ -d "$candidate/.git" ] || continue
    if [ "$(git -C "$candidate" rev-parse HEAD 2>/dev/null || true)" = "$POLYCOMMIT_COMMIT" ]; then
        SOURCE_REPO=$candidate
        break
    fi
done
if [ -z "$SOURCE_REPO" ]; then
    SOURCE_REPO="$TARGET_DIR/upstream-poly-commit"
    if [ ! -d "$SOURCE_REPO/.git" ]; then
        log_info "Pinned Cargo checkout unavailable; cloning arkworks PolyCommit"
        git clone --no-checkout https://github.com/arkworks-rs/poly-commit "$SOURCE_REPO"
    fi
    git -C "$SOURCE_REPO" fetch --depth 1 origin "$POLYCOMMIT_COMMIT"
    git -C "$SOURCE_REPO" checkout --detach "$POLYCOMMIT_COMMIT"
fi
SOURCE_COMMIT=$(git -C "$SOURCE_REPO" rev-parse HEAD)
if [ "$SOURCE_COMMIT" != "$POLYCOMMIT_COMMIT" ]; then
    log_error "Pinned PolyCommit source mismatch: expected $POLYCOMMIT_COMMIT, got $SOURCE_COMMIT"
    exit 1
fi

RUN_ROOT=$(mktemp -d "$TARGET_DIR/polycommit-run.XXXXXXXX")
cleanup() { rm -rf "$RUN_ROOT"; }
trap cleanup 0
trap 'cleanup; exit 1' HUP INT TERM
PATCHED_ROOT="$RUN_ROOT/source"
mkdir -p "$PATCHED_ROOT"
git -C "$SOURCE_REPO" archive "$POLYCOMMIT_COMMIT" | tar -x -C "$PATCHED_ROOT"
git -C "$PATCHED_ROOT" init -q
if ! git -C "$PATCHED_ROOT" apply --check "$PATCH_FILE"; then
    log_error "polycommit.patch does not apply to pinned commit $POLYCOMMIT_COMMIT"
    exit 1
fi
git -C "$PATCHED_ROOT" apply "$PATCH_FILE"
if ! grep -Fq "pub fn with_dims" "$PATCHED_ROOT/poly-commit/src/linear_codes/brakedown.rs"; then
    log_error "The verified PolyCommit patch did not add BrakedownPCParams::with_dims"
    exit 1
fi

BUILD_DIR="$RUN_ROOT/driver"
mkdir -p "$BUILD_DIR"
cp -R "$DRIVER_DIR/src" "$BUILD_DIR/src"
cp "$DRIVER_MANIFEST" "$DRIVER_LOCK" "$BUILD_DIR/"
cat >> "$BUILD_DIR/Cargo.toml" <<EOF

[patch."https://github.com/arkworks-rs/poly-commit"]
ark-poly-commit = { path = "$PATCHED_ROOT/poly-commit" }
EOF
# The committed lock pins the Git source. A path-patched copy has the same
# package version/dependencies but Cargo identifies that one package as local.
python3 - "$BUILD_DIR/Cargo.lock" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
pattern = r'(\[\[package\]\]\nname = "ark-poly-commit"\nversion = "0\.5\.0"\n)source = "git\+https://github\.com/arkworks-rs/poly-commit[^\n]*\n'
text, count = re.subn(pattern, r'\1', text, count=1)
if count != 1:
    raise SystemExit("could not convert the pinned ark-poly-commit lock entry to a local path entry")
path.write_text(text, encoding="utf-8")
PY

# Build from / to avoid the fork's workspace config and codegen wrapper.
log_info "Building patched arkworks PolyCommit driver with rustc $(rustc --version)"
cd /
# Populate the cargo cache from the committed lockfile when the network is
# reachable (fresh VMs); the build itself stays --offline and --locked.
cargo fetch --locked --manifest-path "$BUILD_DIR/Cargo.toml" >/dev/null 2>&1 || \
    log_warn "cargo fetch failed (offline?); building from the existing cargo cache"
taskset -c "$CORES" env POLYCOMMIT_PATCH_SHA256="$PATCH_SHA256" \
    cargo build --offline --manifest-path "$BUILD_DIR/Cargo.toml" --locked --release
DRIVER_BINARY="$TARGET_DIR/release/polycommit-bench-driver"
[ -x "$DRIVER_BINARY" ] || {
    log_error "Expected benchmark binary not found: $DRIVER_BINARY"
    exit 1
}

log_info "Checking arkworks linear-encoder correctness at logN=$LOGN"
taskset -c "$CORES" "$DRIVER_BINARY" \
    --threads "$THREADS" --cores "$CORES" --sec-param "$SEC_PARAM" --logN "$LOGN" --check

RAW_JSON="$DEST/polycommit-${FIELD}-${LOGN}.json"
log_info "Writing benchmark samples to $RAW_JSON"
taskset -c "$CORES" "$DRIVER_BINARY" \
    --logN "$LOGN" --runs "$RUNS" --warmup "$WARMUP" \
    --threads "$THREADS" --cores "$CORES" --sec-param "$SEC_PARAM" --output "$RAW_JSON"

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
POLY_REFERENCE_FIELD="$REFERENCE_FIELD" \
POLY_PATCH_SHA="$PATCH_SHA256" \
POLY_SEC_PARAM="$SEC_PARAM" \
POLYCOMMIT_COMMIT="$POLYCOMMIT_COMMIT" \
python3 - <<'PY'
import json
import os

extra = {
    "baseline": {
        "implementation": "arkworks-poly-commit-brakedown-encoding-only",
        "repository": "https://github.com/arkworks-rs/poly-commit",
        "upstream_commit": os.environ["POLYCOMMIT_COMMIT"],
        "patched": True,
        "patch_sha256": os.environ["POLY_PATCH_SHA"],
        "sec_param": int(os.environ["POLY_SEC_PARAM"]),
        "driver_version": "0.1.0",
        "driver_cargo_manifest_sha256": os.environ["POLY_MANIFEST_SHA"],
        "driver_cargo_lock_sha256": os.environ["POLY_LOCK_SHA"],
        "raw_json_sha256": os.environ["POLY_RAW_SHA"],
        "field": {
            "name": "Mont127",
            "modulus_hex": "0x6e754097ba20e0bf7f2bd90000000001",
            "modulus_decimal": "146823888364060453008360742206866194433",
            "generator": "3",
            "requested_reference_field": os.environ["POLY_REFERENCE_FIELD"],
            "comparison_note": "Matches LCPC Mont127; arkworks sec_param=80 is used for column openings because the 127-bit field cannot satisfy default 128-bit soundness.",
        },
        "parameters": "patched BrakedownPCParams::with_dims; default code constants; explicit square matrix",
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
PARAMS="polycommit-brakedown-square"
FIELD="Mont127"
SLICES="not-applicable"
write_manifest "$DEST/manifest.json" "$EXTRA_FILE"
log_info "PolyCommit raw JSON and manifest written under $DEST"
