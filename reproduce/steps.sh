#!/bin/sh
# Copyright 2026 RealBrakedownPerf Authors.
# Licensed under Apache-2.0.

set -eu

# The repository pins rustc through rustup; load Cargo before resolving it.
. "$HOME/.cargo/env"

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REPO_DIR=$(CDPATH= cd -- "$CODE_DIR/.." && pwd)
DRIVER_SRC="$CODE_DIR/external/steps-driver"
TARGET_ROOT="${CARGO_TARGET_DIR:-$CODE_DIR/target-eval-steps}"
WORKTREE_ROOT="$TARGET_ROOT/worktrees"

SPECIFIC_TAGS=""
LOGN="16,20,24,28"
RUNS=5
WARMUP_OVERRIDE=""
SESSION_ID_OVERRIDE=""
THREADS=1
CORES="0,1,3-7"
OUT="$CODE_DIR/results"
HOST_NAME=$(hostname -s 2>/dev/null || echo generic)
TIMEOUT_SEC=1800
RUN_CLASS="campaign"

usage() {
    cat <<EOF
Usage: $0 [options]

Runs E3 at step/01-modernize through HEAD with the external steps-driver.
Options:
  --tags <list>       Comma-separated tags (default: all step/* tags, then HEAD)
  --logN <list>       Even log2(N) values (default: 16,20,24,28)
  --runs <n>          Timed runs (default: 5)
  --warmup <n>        Warm-ups (default: 2; automatically 1 for logN >= 26)
  --threads <n>       E3 uses one thread; only 1 is accepted
  --session-id <id>   Session label (default: this invocation's unique run ID)
  --cores <list>      Explicit CPU affinity (default: 0,1,3-7); CPU 2 is forbidden
  --timeout-sec <n>   Per-configuration limit (default: 1800)
  --host <name>       Result host label (default: hostname -s)
  --out <dir>         Result root (default: code/results)
  --smoke             Watermark records as local smoke, not research results
  -h, --help          Show this help
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --tags) [ $# -ge 2 ] || { echo "--tags requires a value" >&2; exit 2; }; SPECIFIC_TAGS=$2; shift 2 ;;
        --logN) [ $# -ge 2 ] || { echo "--logN requires a value" >&2; exit 2; }; LOGN=$2; shift 2 ;;
        --runs) [ $# -ge 2 ] || { echo "--runs requires a value" >&2; exit 2; }; RUNS=$2; shift 2 ;;
        --warmup) [ $# -ge 2 ] || { echo "--warmup requires a value" >&2; exit 2; }; WARMUP_OVERRIDE=$2; shift 2 ;;
        --session-id) [ $# -ge 2 ] || { echo "--session-id requires a value" >&2; exit 2; }; SESSION_ID_OVERRIDE=$2; shift 2 ;;
        --threads) [ $# -ge 2 ] || { echo "--threads requires a value" >&2; exit 2; }; THREADS=$2; shift 2 ;;
        --cores) [ $# -ge 2 ] || { echo "--cores requires a value" >&2; exit 2; }; CORES=$2; shift 2 ;;
        --timeout-sec) [ $# -ge 2 ] || { echo "--timeout-sec requires a value" >&2; exit 2; }; TIMEOUT_SEC=$2; shift 2 ;;
        --host) [ $# -ge 2 ] || { echo "--host requires a value" >&2; exit 2; }; HOST_NAME=$2; shift 2 ;;
        --out) [ $# -ge 2 ] || { echo "--out requires a value" >&2; exit 2; }; OUT=$2; shift 2 ;;
        --smoke) RUN_CLASS="local-smoke-not-a-result"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ "$THREADS" != "1" ]; then
    echo "E3 is fixed at T=1; --threads only accepts 1" >&2
    exit 2
fi

if [ -n "${RUSTFLAGS:-}" ] || [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
    echo "RUSTFLAGS and CARGO_ENCODED_RUSTFLAGS must be unset" >&2
    exit 1
fi

for tool in cargo rustc taskset timeout python3 sha256sum git sort; do
    command -v "$tool" >/dev/null 2>&1 || { echo "Required command not found: $tool" >&2; exit 1; }
done

case "$RUNS:$TIMEOUT_SEC" in
    *[!0-9:]*|:*) echo "--runs and --timeout-sec must be positive integers" >&2; exit 2 ;;
esac
[ "$RUNS" -gt 0 ] && [ "$TIMEOUT_SEC" -gt 0 ] || { echo "--runs and --timeout-sec must be greater than zero" >&2; exit 2; }
if [ -n "$WARMUP_OVERRIDE" ]; then
    case "$WARMUP_OVERRIDE" in *[!0-9]*) echo "--warmup must be a nonnegative integer" >&2; exit 2 ;; esac
fi

CORE_LIST=$(
    CORE_LIST_INPUT="$CORES" python3 - <<'PY'
import os

raw = os.environ["CORE_LIST_INPUT"]
cores = set()
for part in raw.split(","):
    part = part.strip()
    if not part:
        raise SystemExit("invalid empty item in --cores")
    if "-" in part:
        lo, hi = part.split("-", 1)
        if not lo.isdecimal() or not hi.isdecimal() or int(lo) > int(hi):
            raise SystemExit(f"invalid core range: {part}")
        cores.update(range(int(lo), int(hi) + 1))
    else:
        if not part.isdecimal():
            raise SystemExit(f"invalid core id: {part}")
        cores.add(int(part))
if 2 in cores:
    raise SystemExit("CPU 2 is reserved and must not be used")
if not cores:
    raise SystemExit("--cores must select at least one CPU")
print(",".join(str(core) for core in sorted(cores)))
PY
) || exit 2

LOGN_LIST=$(LOGN_INPUT="$LOGN" python3 - <<'PY'
import os

values = os.environ["LOGN_INPUT"].split(",")
if not values or any(not item.isdecimal() for item in values):
    raise SystemExit("--logN must be a comma-separated list of even integers")
sizes = [int(item) for item in values]
if any(size < 10 or size % 2 for size in sizes):
    raise SystemExit("--logN values must be even and at least 10")
print(" ".join(str(size) for size in sizes))
PY
) || exit 2

TV_DIR=${SIMD_SPIELMAN_TV_DIR:-"$REPO_DIR/SIMD-Spielman-for-ZKPs/tv"}
if [ ! -f "$TV_DIR/bd_spielman_extension_N12_in.txt" ]; then
    echo "SIMD-Spielman test vectors not found in $TV_DIR (set SIMD_SPIELMAN_TV_DIR)" >&2
    exit 1
fi

mkdir -p "$TARGET_ROOT" "$WORKTREE_ROOT"
ln -sfn "$(dirname -- "$TV_DIR")" "$WORKTREE_ROOT/SIMD-Spielman-for-ZKPs"
export CARGO_TARGET_DIR="$TARGET_ROOT"
export SIMD_SPIELMAN_TV_DIR="$TV_DIR"

if [ -n "$SPECIFIC_TAGS" ]; then
    TAGS_TO_RUN=$(printf '%s' "$SPECIFIC_TAGS" | tr ',' ' ')
else
    TAGS_TO_RUN=$(git -C "$CODE_DIR" tag --list 'step/*' | sort -V | awk '$0 != "step/00-import" { printf "%s ", $0 }')
    if ! git -C "$CODE_DIR" describe --tags --exact-match HEAD >/dev/null 2>&1; then
        TAGS_TO_RUN="$TAGS_TO_RUN HEAD"
    fi
fi
[ -n "$TAGS_TO_RUN" ] || { echo "No step tags selected" >&2; exit 2; }

case "$OUT" in /*) OUT_ROOT=$OUT ;; *) OUT_ROOT="$CODE_DIR/$OUT" ;; esac
CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD)
RUN_ID=$(date -u +%Y-%m-%dT%H%M%SZ)-$CODE_SHA
if [ -n "$SESSION_ID_OVERRIDE" ]; then SESSION_ID=$SESSION_ID_OVERRIDE; else SESSION_ID=$RUN_ID; fi
DEST="$OUT_ROOT/$HOST_NAME/$RUN_ID/steps"
mkdir -p "$DEST"

CURRENT_WORKTREE=""
cleanup_worktree() {
    if [ -n "$CURRENT_WORKTREE" ] && [ -d "$CURRENT_WORKTREE" ]; then
        git -C "$CODE_DIR" worktree remove --force "$CURRENT_WORKTREE" >/dev/null 2>&1 || rm -rf "$CURRENT_WORKTREE"
        CURRENT_WORKTREE=""
    fi
}
trap cleanup_worktree EXIT HUP INT TERM

echo "[steps] E3 per-tag comparison; T=1; logN=$LOGN; runs=$RUNS; cores=$CORES"
echo "[steps] Same timing loop: steps-driver-v1; run class: $RUN_CLASS"
echo "[steps] Output: $DEST"
echo "[steps] CARGO_TARGET_DIR: $TARGET_ROOT"

write_placeholder() {
    PLACEHOLDER_PATH=$1 \
    PLACEHOLDER_STATUS=$2 \
    PLACEHOLDER_TAG=$3 \
    PLACEHOLDER_COMMIT=$4 \
    PLACEHOLDER_MODE=$5 \
    PLACEHOLDER_FIELD=$6 \
    PLACEHOLDER_LOGN=$7 \
    PLACEHOLDER_RUNS=$8 \
    PLACEHOLDER_WARMUP=$9 \
    PLACEHOLDER_ADAPTER=${10} \
    PLACEHOLDER_PARAMS=${11} \
    PLACEHOLDER_SOURCE_LOCK=${12} \
    PLACEHOLDER_DRIVER_LOCK=${13} \
    PLACEHOLDER_LOG=${14} \
    PLACEHOLDER_EXIT=${15} \
    PLACEHOLDER_CLASS="$RUN_CLASS" \
    PLACEHOLDER_SESSION_ID="$SESSION_ID" \
    python3 - <<'PY'
import json
import os

status = os.environ["PLACEHOLDER_STATUS"]
record = {
    "schema_version": "steps-bench-v1",
    "harness_version": "steps-driver-v1",
    "adapter_version": os.environ["PLACEHOLDER_ADAPTER"],
    "status": status,
    "run_class": os.environ["PLACEHOLDER_CLASS"],
    "watermark": "LOCAL SMOKE — NOT A RESULT" if os.environ["PLACEHOLDER_CLASS"] == "local-smoke-not-a-result" else None,
    "tag": os.environ["PLACEHOLDER_TAG"],
    "session_id": os.environ["PLACEHOLDER_SESSION_ID"],
    "commit": os.environ["PLACEHOLDER_COMMIT"],
    "mode": os.environ["PLACEHOLDER_MODE"],
    "params": os.environ["PLACEHOLDER_PARAMS"],
    "field": os.environ["PLACEHOLDER_FIELD"],
    "log_n": int(os.environ["PLACEHOLDER_LOGN"]),
    "threads": 1,
    "warmup": int(os.environ["PLACEHOLDER_WARMUP"]),
    "runs": int(os.environ["PLACEHOLDER_RUNS"]),
    "samples_ns": [],
    "min_ns": None,
    "median_ns": None,
    "max_ns": None,
    "mean_ns": None,
    "source_lock_sha256": os.environ["PLACEHOLDER_SOURCE_LOCK"],
    "driver_lock_sha256": os.environ["PLACEHOLDER_DRIVER_LOCK"],
}
if status == "not_applicable":
    record["reason"] = "Mode (b) is unavailable before step/04-r2l introduced runtime CodeParams and SIMD-Spielman parameter support."
else:
    record["process_exit_code"] = int(os.environ["PLACEHOLDER_EXIT"])
    record["log"] = os.environ["PLACEHOLDER_LOG"]
with open(os.environ["PLACEHOLDER_PATH"], "w", encoding="utf-8") as output:
    json.dump(record, output, indent=2)
    output.write("\n")
PY
}

stage_for_worktree() {
    if [ ! -d "$CURRENT_WORKTREE/lcpc-fields" ]; then
        API_STAGE=rowwise
    elif ! grep -q 'fn encode_rows' "$CURRENT_WORKTREE/lcpc-2d/src/lib.rs"; then
        API_STAGE=rowwise
    elif ! grep -q 'pub enum CodeParams' "$CURRENT_WORKTREE/lcpc-brakedown-pc/src/codespec.rs"; then
        API_STAGE=batched
    elif grep -q 'fn effective_slices' "$CURRENT_WORKTREE/lcpc-brakedown-pc/src/lib.rs"; then
        API_STAGE=runtime-adaptive
    else
        API_STAGE=runtime-unsliced
    fi
    if grep -q 'SimdSpielman' "$CURRENT_WORKTREE/lcpc-brakedown-pc/src/codespec.rs"; then
        MODE_B_AVAILABLE=1
    else
        MODE_B_AVAILABLE=0
    fi
}

prepare_driver() {
    MODE_TO_BUILD=$1
    DRIVER_FIELD_TYPE=$2
    DRIVER_FIELD_NAME=$3
    DRIVER_ADAPTER=$4
    DRIVER_MANIFEST_KIND=$5
    PROJECT="$TARGET_ROOT/projects/${TAG_CLEAN}-${MODE_TO_BUILD}-$$"
    DRIVER_TARGET="$TARGET_ROOT/cargo/driver-${TAG_CLEAN}-${MODE_TO_BUILD}"
    mkdir -p "$PROJECT/src" "$PROJECT/build-logs"
    cp "$DRIVER_SRC/src/main.rs" "$PROJECT/src/main.rs"
    {
        sed "s/adapter-field/$DRIVER_FIELD_NAME/g" "$DRIVER_SRC/src/adapters/$DRIVER_ADAPTER"
        printf '\npub type F = %s;\n' "$DRIVER_FIELD_TYPE"
    } > "$PROJECT/src/adapter.rs"
    sed "s|@WORKTREE@|$CURRENT_WORKTREE|g" \
        "$DRIVER_SRC/Cargo.toml.$DRIVER_MANIFEST_KIND" > "$PROJECT/Cargo.toml"
    if [ -f "$CURRENT_WORKTREE/Cargo.lock" ]; then
        cp "$CURRENT_WORKTREE/Cargo.lock" "$PROJECT/Cargo.lock"
    fi
    mkdir -p "$DRIVER_TARGET"
    DRIVER_BUILD_LOG="$STEP_DEST/build-${MODE_TO_BUILD}.log"
    echo "[steps] Building timing harness for $TAG ($MODE_TO_BUILD; $DRIVER_ADAPTER)"
    if ! (cd "$CURRENT_WORKTREE" && \
        env CARGO_TARGET_DIR="$DRIVER_TARGET" RAYON_NUM_THREADS=1 \
        taskset -c "$CORES" cargo build --release --offline --manifest-path "$PROJECT/Cargo.toml") \
        > "$DRIVER_BUILD_LOG" 2>&1; then
        echo "Timing harness build failed for $TAG; see $DRIVER_BUILD_LOG" >&2
        tail -n 80 "$DRIVER_BUILD_LOG" >&2 || true
        return 1
    fi
    DRIVER_BIN="$DRIVER_TARGET/release/steps-driver"
    DRIVER_LOCK_SHA=$(sha256sum "$PROJECT/Cargo.lock" | awk '{print $1}')
    SOURCE_LOCK_SHA=$(sha256sum "$CURRENT_WORKTREE/Cargo.lock" | awk '{print $1}')
    RUSTC_VERSION=$(cd "$CURRENT_WORKTREE" && rustc --version --verbose | tr '\n' ' ')
}

run_configuration() {
    MODE_TO_RUN=$1
    LOG_N_VALUE=$2
    if [ -n "$WARMUP_OVERRIDE" ]; then
        WARMUP_VALUE=$WARMUP_OVERRIDE
    elif [ "$LOG_N_VALUE" -ge 26 ]; then
        WARMUP_VALUE=1
    else
        WARMUP_VALUE=2
    fi
    if [ "$MODE_TO_RUN" = "a" ]; then MODE_LABEL=mode-a; else MODE_LABEL=mode-b; fi
    RAW_DIR="$STEP_DEST/$MODE_LABEL"
    mkdir -p "$RAW_DIR"
    OUTPUT="$RAW_DIR/${MODE_LABEL}-logN${LOG_N_VALUE}.json"
    LOG_FILE="$RAW_DIR/${MODE_LABEL}-logN${LOG_N_VALUE}.log"
    echo "[steps] Timing $TAG mode=$MODE_TO_RUN field=$DRIVER_FIELD_NAME logN=$LOG_N_VALUE warmup=$WARMUP_VALUE runs=$RUNS"
    set +e
    (cd "$CURRENT_WORKTREE" && \
        env RAYON_NUM_THREADS=1 timeout --signal=TERM --kill-after=30s "${TIMEOUT_SEC}s" \
        taskset -c "$CORES" "$DRIVER_BIN" \
            --mode "$MODE_TO_RUN" --logN "$LOG_N_VALUE" --warmup "$WARMUP_VALUE" --runs "$RUNS" \
            --tag "$TAG" --commit "$TAG_COMMIT" \
            --session-id "$SESSION_ID" \
            --source-lock-sha256 "$SOURCE_LOCK_SHA" --driver-lock-sha256 "$DRIVER_LOCK_SHA" \
            --rustc "$RUSTC_VERSION" --run-class "$RUN_CLASS" --output "$OUTPUT") \
        > "$LOG_FILE" 2>&1
    STATUS=$?
    set -e
    if [ "$STATUS" -eq 0 ]; then return 0; fi
    if [ "$STATUS" -eq 124 ]; then RESULT_STATUS=timeout
    elif [ "$STATUS" -eq 137 ]; then RESULT_STATUS=oom_or_killed
    else RESULT_STATUS=error; fi
    write_placeholder "$OUTPUT" "$RESULT_STATUS" "$TAG" "$TAG_COMMIT" "$MODE_TO_RUN" \
        "$DRIVER_FIELD_NAME" "$LOG_N_VALUE" "$RUNS" "$WARMUP_VALUE" "$ADAPTER_VERSION" \
        "$PARAMS_NAME" "$SOURCE_LOCK_SHA" "$DRIVER_LOCK_SHA" "$LOG_FILE" "$STATUS"
    echo "[steps] $TAG mode=$MODE_TO_RUN logN=$LOG_N_VALUE recorded as $RESULT_STATUS (exit $STATUS); see $LOG_FILE" >&2
    if [ "$RESULT_STATUS" = "error" ]; then
        tail -n 80 "$LOG_FILE" >&2 || true
        return 1
    fi
}

STEP_INDEX=1
for TAG in $TAGS_TO_RUN; do
    git -C "$CODE_DIR" rev-parse --verify "$TAG^{commit}" >/dev/null 2>&1 || {
        echo "Unknown tag or revision: $TAG" >&2
        exit 2
    }
    TAG_CLEAN=$(printf '%s' "$TAG" | tr '/ ' '__')
    STEP_DEST="$DEST/$TAG_CLEAN"
    mkdir -p "$STEP_DEST"
    WORKTREE_DIR=$(mktemp -d "$WORKTREE_ROOT/wt-${TAG_CLEAN}-XXXXXX")
    CURRENT_WORKTREE=$WORKTREE_DIR
    git -C "$CODE_DIR" worktree add --detach "$CURRENT_WORKTREE" "$TAG" >/dev/null
    TAG_COMMIT=$(git -C "$CURRENT_WORKTREE" rev-parse HEAD)
    TAG_SUBJECT=$(git -C "$CURRENT_WORKTREE" log -1 --pretty=format:%s)
    TAG_CLEAN=$(printf '%s' "$TAG" | tr '/ ' '__')
    TAG_TARGET="$TARGET_ROOT/cargo/workspace-$TAG_CLEAN"
    mkdir -p "$TAG_TARGET"

    echo "[steps] [$STEP_INDEX] $TAG ($TAG_COMMIT): $TAG_SUBJECT"
    TEST_LOG="$STEP_DEST/workspace-tests.log"
    if ! (cd "$CURRENT_WORKTREE" && \
        env CARGO_TARGET_DIR="$TAG_TARGET" RAYON_NUM_THREADS=1 SIMD_SPIELMAN_TV_DIR="$TV_DIR" \
        taskset -c "$CORES" cargo test --workspace --release --locked) \
        > "$TEST_LOG" 2>&1; then
        echo "Workspace tests failed at $TAG; refusing to time this tag. See $TEST_LOG" >&2
        tail -n 100 "$TEST_LOG" >&2 || true
        exit 1
    fi

    if ! (cd "$CURRENT_WORKTREE" && \
        env CARGO_TARGET_DIR="$TAG_TARGET" RAYON_NUM_THREADS=1 \
        taskset -c "$CORES" cargo build --release --locked --bin lcpc-bench) \
        > "$STEP_DEST/workspace-build.log" 2>&1; then
        echo "Workspace benchmark build failed at $TAG; see $STEP_DEST/workspace-build.log" >&2
        tail -n 80 "$STEP_DEST/workspace-build.log" >&2 || true
        exit 1
    fi

    stage_for_worktree
    CODEGEN_ARGS=""
    case "$API_STAGE" in rowwise|batched) ;; *) CODEGEN_ARGS="--strict" ;; esac
    # shellcheck disable=SC2086
    if ! taskset -c "$CORES" python3 "$CODE_DIR/scripts/check_codegen.py" \
        "$TAG_TARGET/release/lcpc-bench" $CODEGEN_ARGS \
        --output-json "$STEP_DEST/codegen_report.json" \
        --output-listing "$STEP_DEST/disassembly.txt" \
        > "$STEP_DEST/codegen.log" 2>&1; then
        echo "Codegen check failed at $TAG; refusing to time this tag. See $STEP_DEST/codegen.log" >&2
        tail -n 80 "$STEP_DEST/codegen.log" >&2 || true
        exit 1
    fi

    if [ "$API_STAGE" = "rowwise" ] && [ ! -d "$CURRENT_WORKTREE/lcpc-fields" ]; then
        MODE_A_FIELD_TYPE=lcpc_test_fields::ft127::Ft127
        MODE_A_FIELD_NAME=Ft127
        MODE_A_MANIFEST=legacy
    else
        MODE_A_FIELD_TYPE=lcpc_fields::Mont127
        MODE_A_FIELD_NAME=Mont127
        MODE_A_MANIFEST=fields
    fi
    case "$API_STAGE" in
        rowwise) MODE_A_ADAPTER=rowwise.rs ;;
        batched) MODE_A_ADAPTER=batched-unsliced.rs ;;
        runtime-unsliced) MODE_A_ADAPTER=runtime-unsliced.rs ;;
        runtime-adaptive) MODE_A_ADAPTER=runtime-adaptive.rs ;;
    esac

    prepare_driver a "$MODE_A_FIELD_TYPE" "$MODE_A_FIELD_NAME" "$MODE_A_ADAPTER" "$MODE_A_MANIFEST"
    ADAPTER_VERSION=$(grep '^pub const ADAPTER_VERSION:' "$DRIVER_SRC/src/adapters/$MODE_A_ADAPTER" | sed 's/.*= "\([^"]*\)";.*/\1/')
    DRIVER_FIELD_NAME=$MODE_A_FIELD_NAME
    PARAMS_NAME=lcpc-sdig6
    for LOG_N_VALUE in $LOGN_LIST; do run_configuration a "$LOG_N_VALUE" || exit 1; done

    if [ "$MODE_B_AVAILABLE" -eq 1 ]; then
        MODE_B_FIELD_TYPE=lcpc_fields::Mers127
        MODE_B_FIELD_NAME=Mers127
        MODE_B_MANIFEST=fields
        case "$API_STAGE" in
            runtime-unsliced) MODE_B_ADAPTER=runtime-unsliced.rs ;;
            runtime-adaptive) MODE_B_ADAPTER=runtime-adaptive.rs ;;
            *) echo "Runtime mode (b) detected without a supported timing adapter at $TAG" >&2; exit 1 ;;
        esac
        prepare_driver b "$MODE_B_FIELD_TYPE" "$MODE_B_FIELD_NAME" "$MODE_B_ADAPTER" "$MODE_B_MANIFEST"
        ADAPTER_VERSION=$(grep '^pub const ADAPTER_VERSION:' "$DRIVER_SRC/src/adapters/$MODE_B_ADAPTER" | sed 's/.*= "\([^"]*\)";.*/\1/')
        DRIVER_FIELD_NAME=$MODE_B_FIELD_NAME
        PARAMS_NAME=simd-spielman
        for LOG_N_VALUE in $LOGN_LIST; do run_configuration b "$LOG_N_VALUE" || exit 1; done
    else
        PARAMS_NAME=simd-spielman
        ADAPTER_VERSION=not-applicable-before-runtime-params-v1
        for LOG_N_VALUE in $LOGN_LIST; do
            if [ -n "$WARMUP_OVERRIDE" ]; then WARMUP_VALUE=$WARMUP_OVERRIDE; \
            elif [ "$LOG_N_VALUE" -ge 26 ]; then WARMUP_VALUE=1; else WARMUP_VALUE=2; fi
            NA_DIR="$STEP_DEST/mode-b"
            mkdir -p "$NA_DIR"
            write_placeholder "$NA_DIR/mode-b-logN${LOG_N_VALUE}.json" not_applicable "$TAG" "$TAG_COMMIT" b \
                Mers127 "$LOG_N_VALUE" "$RUNS" "$WARMUP_VALUE" "$ADAPTER_VERSION" simd-spielman \
                "$(sha256sum "$CURRENT_WORKTREE/Cargo.lock" | awk '{print $1}')" "" "" 0
        done
        echo "[steps] $TAG mode (b): recorded N/A (runtime parameters are not present yet)"
    fi

    TAG_MANIFEST="$STEP_DEST/manifest.json" \
    STEP_TAG="$TAG" STEP_COMMIT="$TAG_COMMIT" STEP_SUBJECT="$TAG_SUBJECT" \
    STEP_STAGE="$API_STAGE" STEP_TEST_LOG="$TEST_LOG" STEP_CODEGEN="$STEP_DEST/codegen_report.json" \
    STEP_INDEX="$STEP_INDEX" STEP_RUN_CLASS="$RUN_CLASS" \
    STEP_SESSION_ID="$SESSION_ID" \
    python3 - <<'PY'
import json
import os

manifest = {
    "schema_version": "steps-tag-manifest-v1",
    "harness_version": "steps-driver-v1",
    "tag": os.environ["STEP_TAG"],
    "commit": os.environ["STEP_COMMIT"],
    "commit_subject": os.environ["STEP_SUBJECT"],
    "step_index": int(os.environ["STEP_INDEX"]),
    "session_id": os.environ["STEP_SESSION_ID"],
    "detected_api_stage": os.environ["STEP_STAGE"],
    "run_class": os.environ["STEP_RUN_CLASS"],
    "watermark": "LOCAL SMOKE — NOT A RESULT" if os.environ["STEP_RUN_CLASS"] == "local-smoke-not-a-result" else None,
    "workspace_tests": os.environ["STEP_TEST_LOG"],
    "codegen_report": os.environ["STEP_CODEGEN"],
}
with open(os.environ["TAG_MANIFEST"], "w", encoding="utf-8") as output:
    json.dump(manifest, output, indent=2)
    output.write("\n")
PY

    git -C "$CODE_DIR" worktree remove --force "$CURRENT_WORKTREE" >/dev/null
    CURRENT_WORKTREE=""
    STEP_INDEX=$((STEP_INDEX + 1))
done

RUN_MANIFEST="$DEST/manifest.json" \
RUN_ID_VALUE="$RUN_ID" RUN_SESSION_ID="$SESSION_ID" RUN_HOST="$HOST_NAME" RUN_CORES="$CORES" RUN_CORE_LIST="$CORE_LIST" \
RUN_LOGN="$LOGN" RUNS_VALUE="$RUNS" WARMUP_OVERRIDE_VALUE="$WARMUP_OVERRIDE" \
RUN_TIMEOUT="$TIMEOUT_SEC" RUN_CLASS_VALUE="$RUN_CLASS" RUN_TARGET_ROOT="$TARGET_ROOT" \
RUN_TAGS="$TAGS_TO_RUN" RUN_SOURCE_SHA="$CODE_SHA" RUN_SIMD_DIR="$TV_DIR" \
python3 - <<'PY'
import glob
import json
import os
import platform
import subprocess

root = os.path.dirname(os.environ["RUN_MANIFEST"])
records = []
for path in sorted(glob.glob(os.path.join(root, "*", "mode-*", "mode-*-logN*.json"))):
    with open(path, "r", encoding="utf-8") as source:
        item = json.load(source)
    records.append({
        "path": os.path.relpath(path, root),
        "tag": item.get("tag"),
        "commit": item.get("commit"),
        "mode": item.get("mode"),
        "log_n": item.get("log_n"),
        "status": item.get("status"),
        "harness_version": item.get("harness_version"),
        "adapter_version": item.get("adapter_version"),
    })
manifest = {
    "schema_version": "steps-run-manifest-v1",
    "harness_version": "steps-driver-v1",
    "run_id": os.environ["RUN_ID_VALUE"],
    "session_id": os.environ["RUN_SESSION_ID"],
    "source_checkout_head": os.environ["RUN_SOURCE_SHA"],
    "host": os.environ["RUN_HOST"],
    "run_class": os.environ["RUN_CLASS_VALUE"],
    "watermark": "LOCAL SMOKE — NOT A RESULT" if os.environ["RUN_CLASS_VALUE"] == "local-smoke-not-a-result" else None,
    "machine": {"platform": platform.platform(), "uname": platform.uname()._asdict()},
    "configuration": {
        "modes": {"a": "lcpc-sdig6 / Mont127 (Ft127 at step/01-modernize)", "b": "simd-spielman / Mers127 from step/04-r2l"},
        "threads": 1,
        "log_n": os.environ["RUN_LOGN"],
        "runs": int(os.environ["RUNS_VALUE"]),
        "warmup_override": os.environ["WARMUP_OVERRIDE_VALUE"] or None,
        "default_warmups": "2, except 1 at logN >= 26",
        "timeout_sec": int(os.environ["RUN_TIMEOUT"]),
        "cores_requested": os.environ["RUN_CORES"],
        "cores_expanded": os.environ["RUN_CORE_LIST"],
    },
    "toolchain": {
        "rustc": subprocess.run(["rustc", "--version", "--verbose"], text=True, capture_output=True, check=True).stdout.strip(),
        "cargo": subprocess.run(["cargo", "--version", "--verbose"], text=True, capture_output=True, check=True).stdout.strip(),
    },
    "simd_spielman_test_vectors": os.environ["RUN_SIMD_DIR"],
    "cargo_target_dir": os.environ["RUN_TARGET_ROOT"],
    "tags_requested": os.environ["RUN_TAGS"].split(),
    "records": records,
}
with open(os.environ["RUN_MANIFEST"], "w", encoding="utf-8") as output:
    json.dump(manifest, output, indent=2)
    output.write("\n")
PY

echo "[steps] Complete. Combined manifest: $DEST/manifest.json"
if [ "$RUN_CLASS" = "local-smoke-not-a-result" ]; then
    echo "[steps] These local smoke records are watermarked and must not be reported as benchmark results."
fi
