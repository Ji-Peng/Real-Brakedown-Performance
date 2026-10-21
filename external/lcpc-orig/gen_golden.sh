#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

set -eu

DRIVER_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$DRIVER_DIR/../.." && pwd)
GOLDEN_FILE="$CODE_DIR/tests/golden/lcpc-orig/golden-v1.json"

. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$CODE_DIR/target-lcpc-orig/golden"
export RUSTC_BOOTSTRAP=lcpc_brakedown_pc
export RAYON_NUM_THREADS=1

taskset -c 0,1,3-7 cargo run \
    --manifest-path "$DRIVER_DIR/Cargo.toml" \
    --locked --profile fork-release --features portable-only -- \
    --threads 1 --cores 0,1,3-7 --generate-golden "$GOLDEN_FILE"

python3 - "$GOLDEN_FILE" <<'PY'
import os
import sys

path = sys.argv[1]
size = os.path.getsize(path)
if size >= 2 * 1024 * 1024:
    raise SystemExit(f"golden file is {size} bytes; limit is 2 MiB")
print(f"wrote {path} ({size} bytes)")
PY
