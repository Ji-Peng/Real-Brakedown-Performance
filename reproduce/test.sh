#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# C0 correctness gate:
#   1. cargo test --workspace --release --locked
#   2. original-LCPC golden vector check (external/lcpc-orig)
# Writes OUT/<label>/tests/test-<commit>.log and a stamp keyed by commit,
# dirty flag, rustc version, and host.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/common.sh"
if [ -f "${CARGO_HOME:-$HOME/.cargo}/env" ]; then
    # shellcheck disable=SC1091
    . "${CARGO_HOME:-$HOME/.cargo}/env"
fi

# Default to multi-core affinity for faster test execution unless --cores is passed.
parse_args "$@"
if [ "$EXPLICIT_CORES" -eq 0 ] && [ -n "${HOST_DEFAULT_CORES:-}" ]; then
    CORES="$HOST_DEFAULT_CORES"
fi

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

CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD 2>/dev/null || echo "detached")
TESTS_DIR="$OUT_ROOT/$HOST_NAME/tests"
mkdir -p "$TESTS_DIR"
TEST_LOG="$TESTS_DIR/test-${CODE_SHA}.log"
STAMP_FILE="$TESTS_DIR/test-stamp.json"

TARGET_DIR="${CARGO_TARGET_DIR:-$CODE_DIR/target-harness}"
mkdir -p "$TARGET_DIR"
TARGET_DIR=$(CDPATH='' cd -- "$TARGET_DIR" && pwd)

OFFLINE_FLAG=""
if [ "${RBD_OFFLINE:-0}" = "1" ] || [ -f "${RBD_DEPS_DIR:-$CODE_DIR/deps}/.offline" ]; then
    OFFLINE_FLAG="--offline"
fi

log_info "=========================================================="
log_info "C0 Correctness Gate: reproduce/test.sh"
log_info "Host: $HOST_NAME | Cores: $CORES | Commit: $CODE_SHA"
log_info "Log:  $TEST_LOG"
log_info "=========================================================="

: > "$TEST_LOG"

# 1. Workspace tests (unit, field differential, LCPC oracle, 18 SIMD-Spielman TVs)
log_info "Step 1/2: Running cargo test --workspace --release --locked $OFFLINE_FLAG"
# shellcheck disable=SC2086
if ! (cd "$CODE_DIR" && env CARGO_TARGET_DIR="$TARGET_DIR" taskset -c "$CORES" \
    cargo test --workspace --release --locked $OFFLINE_FLAG) >> "$TEST_LOG" 2>&1; then
    log_error "Workspace test suite failed! Tail of $TEST_LOG:"
    tail -n 60 "$TEST_LOG" >&2 || true
    exit 1
fi
log_info "Workspace tests passed."

# 2. Original-LCPC golden vector check (built from / so workspace .cargo/config.toml does not apply)
DRIVER_DIR="$CODE_DIR/external/lcpc-orig"
GOLDEN_FILE="$CODE_DIR/tests/golden/lcpc-orig/golden-v1.json"
if [ ! -f "$GOLDEN_FILE" ]; then
    log_error "Missing golden vectors: $GOLDEN_FILE"
    exit 1
fi
LCPC_TARGET="$TARGET_DIR/lcpc-orig-golden"
mkdir -p "$LCPC_TARGET"
FORK_RUSTFLAGS=$(fork_config_rustflags)

log_info "Step 2/2: Building external/lcpc-orig and verifying golden vectors"
# shellcheck disable=SC2086
if ! (cd / && env CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$LCPC_TARGET" \
    RUSTC_BOOTSTRAP=lcpc_brakedown_pc RUSTFLAGS="$FORK_RUSTFLAGS" \
    RAYON_NUM_THREADS=1 taskset -c "$CORES" cargo build \
    --manifest-path "$DRIVER_DIR/Cargo.toml" --locked $OFFLINE_FLAG \
    --profile fork-release --features portable-only) >> "$TEST_LOG" 2>&1; then
    log_error "Building external/lcpc-orig failed! Tail of $TEST_LOG:"
    tail -n 60 "$TEST_LOG" >&2 || true
    exit 1
fi

LCPC_BIN="$LCPC_TARGET/fork-release/lcpc-orig-driver"
if ! env RAYON_NUM_THREADS=1 taskset -c "$CORES" "$LCPC_BIN" \
    --threads 1 --cores "$CORES" --check-golden "$GOLDEN_FILE" >> "$TEST_LOG" 2>&1; then
    log_error "Original-LCPC golden vector check failed! Tail of $TEST_LOG:"
    tail -n 60 "$TEST_LOG" >&2 || true
    exit 1
fi
log_info "Original-LCPC golden vector check passed."

STAMP_KEY=$(compute_test_stamp_key)
STAMP_FILE_ENV="$STAMP_FILE" STAMP_KEY_ENV="$STAMP_KEY" STAMP_LOG_ENV="$TEST_LOG" \
STAMP_HOST_ENV="$HOST_NAME" STAMP_SHA_ENV="$CODE_SHA" \
python3 - <<'PY'
import datetime, json, os, pathlib, platform, subprocess

rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True, check=True).stdout.strip()
payload = {
    "schema_version": "rbd-test-stamp-v1",
    "status": "passed",
    "key": os.environ["STAMP_KEY_ENV"],
    "commit": os.environ["STAMP_SHA_ENV"],
    "rustc": rustc,
    "host": os.environ["STAMP_HOST_ENV"],
    "arch": platform.machine(),
    "log": os.environ["STAMP_LOG_ENV"],
    "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
}
p = pathlib.Path(os.environ["STAMP_FILE_ENV"])
tmp = p.with_suffix(p.suffix + ".tmp")
tmp.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
os.replace(tmp, p)
PY

log_info "C0 correctness gate PASSED. Stamp: $STAMP_FILE"
