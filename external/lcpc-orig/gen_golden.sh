#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

set -eu

DRIVER_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH='' cd -- "$DRIVER_DIR/../.." && pwd)
GOLDEN_FILE="$CODE_DIR/tests/golden/lcpc-orig/golden-v1.json"

CORES=""
while [ $# -gt 0 ]; do
    case "$1" in
        --cores) [ $# -ge 2 ] || { echo "--cores requires a value" >&2; exit 2; }; CORES="$2"; shift 2 ;;
        -h|--help) echo "Usage: $0 [--cores LIST]"; exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

if [ -f "${CARGO_HOME:-$HOME/.cargo}/env" ]; then
    # shellcheck disable=SC1091
    . "${CARGO_HOME:-$HOME/.cargo}/env"
fi

if [ -z "$CORES" ]; then
    _cores_info=$(python3 "$CODE_DIR/reproduce/lib/cores.py" --max-threads 1)
    IFS='|' read -r CORES _rest <<CORES_EOF
$_cores_info
CORES_EOF
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$CODE_DIR/target-lcpc-orig/golden}"
export RUSTC_BOOTSTRAP=lcpc_brakedown_pc
export RAYON_NUM_THREADS=1

taskset -c "$CORES" cargo run \
    --manifest-path "$DRIVER_DIR/Cargo.toml" \
    --locked --profile fork-release --features portable-only -- \
    --threads 1 --cores "$CORES" --generate-golden "$GOLDEN_FILE"

python3 - "$GOLDEN_FILE" <<'PY'
import os
import sys

path = sys.argv[1]
size = os.path.getsize(path)
if size >= 2 * 1024 * 1024:
    raise SystemExit(f"golden file is {size} bytes; limit is 2 MiB")
print(f"wrote {path} ({size} bytes)")
PY
