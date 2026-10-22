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

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/common.sh"

EXPECTED_PATCH_SHA256=d9cf97f0347fbb6b41d96397f6be9e3d7b604381471ef901e5ba61e9d7806fb6
UPSTREAM_URL=https://github.com/flokrieger/SIMD-Spielman-for-ZKPs.git
FIELD_WAS_SET=0
OUT_WAS_SET=0
SHAPE_WAS_SET=0
for argument in "$@"; do
    case "$argument" in
        --field) FIELD_WAS_SET=1 ;;
        --out) OUT_WAS_SET=1 ;;
        --shape) SHAPE_WAS_SET=1 ;;
        -h|--help)
            cat <<'EOF'
Usage: simd_spielman.sh [common options]

Runs upstream SIMD-Spielman correctness tests and benchmarks from a patched,
out-of-tree copy. With no --field option, all three fields, both Brakedown and
Orion shapes, and the Montgomery verifier benchmark are run.

Common options: --threads N --logN LIST --runs N --cores LIST --field
mont127|mers127|fp2m61 --shape brakedown|orion|both --slices table|N --host NAME --out DIR
EOF
            exit 0
            ;;
    esac
done

parse_args "$@"
SIMD_EXTRA_INCLUDE=${SIMD_SPIELMAN_EXTRA_INCLUDE:-}
SIMD_CXX=${SIMD_SPIELMAN_CXX_WRAPPER:-${CXX:-g++}}
if [ -n "$SIMD_EXTRA_INCLUDE" ]; then
    CPLUS_INCLUDE_PATH="$SIMD_EXTRA_INCLUDE${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}"
    export CPLUS_INCLUDE_PATH
fi
if [ "$HOST_NAME" = generic ]; then
    HOST_NAME=$(hostname -s)
fi
# Upstream splits right nodes into THREADS parts (the last takes the remainder),
# so any count works; 7 = 8-core point (7 workers + spin-waiting coordinator).
case "$THREADS" in
    ''|*[!0-9]*|0) log_error "Unsupported --threads value '$THREADS'; use a positive integer <= 16."; exit 1 ;;
    *) [ "$THREADS" -le 16 ] || { log_error "Unsupported --threads value '$THREADS'; use a positive integer <= 16."; exit 1; } ;;
esac
if [ "$THREADS" -gt 1 ] && [ "$EXPLICIT_CORES" -ne 1 ]; then
    log_error "SIMD-Spielman multithreaded runs require an explicit --cores list."
    exit 1
fi
case "$RUNS" in
    ''|*[!0-9]*) log_error "--runs must be a positive integer."; exit 1 ;;
    0) log_error "--runs must be a positive integer."; exit 1 ;;
esac
case "$WARMUP" in
    ''|*[!0-9]*) log_error "--warmup must be a nonnegative integer."; exit 1 ;;
esac
case "$LOGN" in
    ''|*[!0-9,]*) log_error "--logN must be a comma-separated list of integer sizes."; exit 1 ;;
esac
OLD_IFS=$IFS
IFS=,
for log_n in $LOGN; do
    case "$log_n" in
        12|14|16|18|20|22|24|26|28) ;;
        *) IFS=$OLD_IFS; log_error "Unsupported logN '$log_n'; use 12, 14, 16, 18, 20, 22, 24, 26, or 28."; exit 1 ;;
    esac
done
IFS=$OLD_IFS

if [ "$FIELD_WAS_SET" -eq 1 ]; then
    case "$FIELD" in
        mont127) UPSTREAM_FIELDS=montgomery ;;
        mers127) UPSTREAM_FIELDS=mersenne ;;
        fp2m61) UPSTREAM_FIELDS=extension ;;
        *) log_error "Unsupported --field '$FIELD'; use mont127, mers127, or fp2m61."; exit 1 ;;
    esac
else
    UPSTREAM_FIELDS="montgomery,mersenne,extension"
fi

if [ "$SHAPE_WAS_SET" -eq 1 ]; then
    case "$SHAPE" in
        brakedown) REQUESTED_SHAPES="brakedown" ;;
        orion) REQUESTED_SHAPES="orion" ;;
        both|brakedown,orion|orion,brakedown) REQUESTED_SHAPES="brakedown orion" ;;
        *) log_error "Unsupported --shape '$SHAPE'; use brakedown, orion, or both."; exit 1 ;;
    esac
else
    REQUESTED_SHAPES="brakedown orion"
fi
case "$SLICES" in
    adaptive|table) SLICE_MODE=table; SLICE_OVERRIDE= ;;
    ''|*[!0-9]*) log_error "--slices must be 'table' or a positive integer."; exit 1 ;;
    0) log_error "--slices must be 'table' or a positive integer."; exit 1 ;;
    *) SLICE_MODE=explicit; SLICE_OVERRIDE=$SLICES ;;
esac

if [ "$OUT_WAS_SET" -eq 1 ]; then
    case "$OUT" in
        /*) OUT_DIR=$OUT ;;
        *) OUT_DIR="$CODE_DIR/$OUT" ;;
    esac
else
    CODE_GIT_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD)
    RUN_DATE=$(date -u +%Y%m%d)
    OUT_DIR="$CODE_DIR/results/$HOST_NAME/$RUN_DATE-$CODE_GIT_SHA/simd_spielman"
fi

TARGET_ROOT="${CARGO_TARGET_DIR:-$CODE_DIR/target-simd-spielman}/simd-spielman"
RAW_DIR="$OUT_DIR/raw"
PATCH_FILE="$CODE_DIR/external/simd_spielman/params.patch"
UPSTREAM_SHA=$UPSTREAM_COMMIT
RUN_STATUS=running
FAILURE_REASON=
SKIP_REASON=
SELECTED_CPU=
SOURCE_COMMIT=
PATCH_SHA256=
TEST_CORES=${HOST_ALLOWED_CORES:-$CORES}
mkdir -p "$RAW_DIR" "$TARGET_ROOT"
RUN_ROOT=$(mktemp -d "$TARGET_ROOT/run.XXXXXXXX")

count_cores() {
    expanded=$(expand_cores "$1")
    # Word splitting is intentional after common.sh has validated the list.
    set -- $expanded
    printf '%s\n' "$#"
}

finalize() {
    result=$?
    trap - 0
    set +e
    if [ "$result" -ne 0 ] && [ "$RUN_STATUS" = running ]; then
        RUN_STATUS=failed
    fi
    SIMD_STATUS=$RUN_STATUS \
    SIMD_FAILURE_REASON=$FAILURE_REASON \
    SIMD_SKIP_REASON=$SKIP_REASON \
    SIMD_SELECTED_CPU=$SELECTED_CPU \
    SIMD_SOURCE_COMMIT=$SOURCE_COMMIT \
    SIMD_PATCH_SHA256=$PATCH_SHA256 \
    SIMD_THREADS=$THREADS \
    SIMD_LOGN=$LOGN \
    SIMD_RUNS=$RUNS \
    SIMD_WARMUP=$WARMUP \
    SIMD_CORES=$CORES \
    SIMD_TEST_CORES=$TEST_CORES \
    SIMD_FIELDS="$UPSTREAM_FIELDS" \
    SIMD_REQUESTED_SHAPES="$REQUESTED_SHAPES" \
    SIMD_SLICES_MODE=$SLICE_MODE \
    SIMD_SLICES_OVERRIDE=$SLICE_OVERRIDE \
    SIMD_SLICE_TABLE=${SIMD_SPIELMAN_SLICE_TABLE:-} \
    SIMD_CXX="$SIMD_CXX" \
    SIMD_CXX_WRAPPER=${SIMD_SPIELMAN_CXX_WRAPPER:-} \
    SIMD_EXTRA_INCLUDE="$SIMD_EXTRA_INCLUDE" \
    SIMD_CXX_FLAGS='-march=native -O3 -funroll-loops -Wno-uninitialized -mavx512f -mavx512vl -mavx512dq; -Wall -faligned-new -g; -lstdc++; SELECTED_CPU compile definition' \
    SIMD_RAW_DIR=$RAW_DIR \
    SIMD_OUTPUT_DIR=$OUT_DIR \
    python3 - <<'PY' > "$OUT_DIR/simd_spielman.json"
import json
import os
import pathlib
import re

raw_dir = pathlib.Path(os.environ.get("SIMD_RAW_DIR", ""))
output_dir = pathlib.Path(os.environ.get("SIMD_OUTPUT_DIR", ""))
effective_slices = {}
if raw_dir.is_dir():
    for path in sorted(raw_dir.glob("*.log")):
        try:
            output = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        match = re.search(r"GraphGen Done lgN=(\d+) k=(\d+) slices=(\d+)", output)
        if match:
            effective_slices[path.stem] = int(match.group(3))

manifest = {
    "status": os.environ.get("SIMD_STATUS"),
    "failure_reason": os.environ.get("SIMD_FAILURE_REASON") or None,
    "skip_reason": os.environ.get("SIMD_SKIP_REASON") or None,
    "upstream_commit": os.environ.get("SIMD_SOURCE_COMMIT") or None,
    "params_patch_sha256": os.environ.get("SIMD_PATCH_SHA256") or None,
    "selected_cpu": os.environ.get("SIMD_SELECTED_CPU") or None,
    "threads": int(os.environ.get("SIMD_THREADS", "1")),
    "logN": [int(x) for x in os.environ.get("SIMD_LOGN", "").split(",") if x],
    "runs": int(os.environ.get("SIMD_RUNS", "1")),
    "warmup": int(os.environ.get("SIMD_WARMUP", "2")),
    "measurement_method": "one process per case (graph generated once); warm-up + timed encodings in-process, each on fresh random input, timed individually; warm-up encodings excluded",
    "benchmark_cores": os.environ.get("SIMD_CORES"),
    # Multi-threaded upstream encoders have a spin-waiting coordinator (main) thread
    # pinned after the THREADS workers; with THREADS >= listed cores it shares a
    # worker's core, which over-subscribes that core.
    "coordinator_shares_worker_core": int(os.environ.get("SIMD_THREADS", "1")) > 1
        and int(os.environ.get("SIMD_THREADS", "1")) >= len([c for part in os.environ.get("SIMD_CORES", "").split(",") if part
            for c in (range(int(part.split("-")[0]), int(part.split("-")[-1]) + 1))]),
    "correctness_test_cores": os.environ.get("SIMD_TEST_CORES"),
    "fields": os.environ.get("SIMD_FIELDS", "").replace(",", " ").split(),
    "shapes": [x for x in os.environ.get("SIMD_REQUESTED_SHAPES", "brakedown orion").split() if x],
    "verifier_benchmark": "Montgomery, single thread",
    "slice_mode": os.environ.get("SIMD_SLICES_MODE"),
    "single_slice_override": os.environ.get("SIMD_SLICES_OVERRIDE") or None,
    "custom_slice_table": os.environ.get("SIMD_SLICE_TABLE") or None,
    "effective_slice_counts_by_benchmark": effective_slices,
    "cxx": os.environ.get("SIMD_CXX"),
    "cxx_wrapper_override": os.environ.get("SIMD_CXX_WRAPPER") or None,
    "extra_include_override": os.environ.get("SIMD_EXTRA_INCLUDE") or None,
    # Compiler search-path overrides (e.g. a local Boost header tree when
    # libboost-all-dev is not installed); None when the system headers are used.
    "cxx_include_env": {
        name: os.environ.get(name)
        for name in ("CPATH", "CPLUS_INCLUDE_PATH")
        if os.environ.get(name)
    } or None,
    "upstream_compile_flags": os.environ.get("SIMD_CXX_FLAGS"),
    "cmake_configuration": {
        "test_poly_sizes": [12, 14, 16],
        "test_thread_variants": [2, 4, 8, 16],
        "benchmark_poly_sizes": [int(x) for x in os.environ.get("SIMD_LOGN", "").split(",") if x],
        "benchmark_thread_variant": int(os.environ.get("SIMD_THREADS", "1")),
        "target_definitions": [
            "SELECTED_CPU=" + (os.environ.get("SIMD_SELECTED_CPU") or "unknown"),
            "BRAKEDOWN=0 or BRAKEDOWN=1 per matrix shape",
            "POLY_SIZE=per executable logN",
            "THREADS=per executable thread variant",
        ],
    },
    "output_dir": os.environ.get("SIMD_OUTPUT_DIR"),
}
print(json.dumps(manifest, indent=2))

# Convert raw benchmark logs into lcpc-bench-v1 records
field_map = {
    "montgomery": "Mont127",
    "mersenne": "Mers127",
    "extension": "Fp2M61",
}
verifier_latencies = {}
def encode_samples_us(content):
    """Timed per-encoding samples; the first SIMD_WARMUP in-process encodings are warm-ups."""
    samples = [int(x) for x in re.findall(r"Encode sample\s+\d+\s*:\s*(\d+)\s*us", content)]
    if samples:
        return samples[int(os.environ.get("SIMD_WARMUP", "0")):]
    m = re.search(r"Encode time for logN\s*=\s*\d+,\s*repetitions\s*(\d+)\s*:\s*(\d+)\s*us", content)
    return [int(m.group(2))] if m else []

if raw_dir.is_dir():
    for path in sorted(raw_dir.glob("verifier-*.log")):
        if "-warmup-" in path.name:
            continue
        try:
            content = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        m = re.search(r"verifier-([a-zA-Z0-9]+)-montgomery-N(\d+)-T1-run-(\d+)\.log", path.name)
        if not m:
            m2 = re.search(r"verifier-montgomery-N(\d+)-T1-run-(\d+)\.log", path.name)
            if m2:
                sh = "brakedown"
                ln = int(m2.group(1))
            else:
                continue
        else:
            sh = m.group(1)
            ln = int(m.group(2))
        for v_us in encode_samples_us(content):
            verifier_latencies.setdefault((sh, ln), []).append(v_us * 1000)

    bench_reports = []
    bench_cases = {}
    for path in sorted(raw_dir.glob("*.log")):
        fname = path.name
        if (fname.startswith("verifier-") or fname.startswith("prerequisite-") or
                fname.startswith("manifest") or fname.startswith("upstream-") or
                fname.startswith("test-") or fname.startswith("benchmark-") or
                "-warmup-" in fname):
            continue
        m = re.search(r"^([a-zA-Z0-9]+)-([a-zA-Z0-9]+)-N(\d+)-T(\d+)-run-(\d+)\.log$", fname)
        if not m:
            continue
        shape = m.group(1)
        field_raw = m.group(2)
        log_n = int(m.group(3))
        threads = int(m.group(4))
        case_key = (shape, field_raw, log_n, threads)
        field = field_map.get(field_raw, field_raw)

        try:
            content = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            content = ""

        m_graph = re.search(r"GraphGen Done lgN=(\d+) k=(\d+)(?:\s+slices=(\d+))?", content)

        k = int(m_graph.group(2)) if m_graph else (128 if shape == "orion" else 1 << (log_n // 2))
        slices = int(m_graph.group(3)) if (m_graph and m_graph.group(3)) else 1
        case = bench_cases.setdefault(case_key, {"field": field, "k": k, "slices": slices, "samples_ns": []})
        case["samples_ns"].extend(us * 1000 for us in encode_samples_us(content))

    def summarize(samples):
        ordered = sorted(samples)
        count = len(ordered)
        if not count:
            return {"samples_ns": [], "summary": {}}
        median = (ordered[(count - 1) // 2] + ordered[count // 2]) / 2.0
        q1 = ordered[(count - 1) // 4]
        q3 = ordered[(3 * (count - 1)) // 4]
        return {
            "samples_ns": ordered,
            "summary": {
                "min_ns": ordered[0], "q1_ns": q1, "median_ns": median,
                "q3_ns": q3, "iqr_ns": q3 - q1, "max_ns": ordered[-1],
                "mean_ns": sum(ordered) / count,
            },
        }

    for (shape, field_raw, log_n, threads), case in sorted(bench_cases.items()):
        field = case["field"]
        k = case["k"]
        slices = case["slices"]
        samples_ns = case["samples_ns"]
        runs = len(samples_ns)
        status = "ok" if runs == int(os.environ.get("SIMD_RUNS", "1")) else "failed"
        measurements = {}
        if samples_ns:
            measurements["encode_matrix"] = summarize(samples_ns)

        verifier_samples = verifier_latencies.get((shape, log_n), []) if field == "Mont127" else []
        if verifier_samples:
            measurements["verifier_encode_2"] = summarize(verifier_samples)
            measurements["verifier_encode"] = summarize(verifier_samples)

        n_per_row = (1 << log_n) // k if k > 0 else 0
        report = {
            "schema_version": "lcpc-bench-v1",
            "implementation": "simd-spielman",
            "driver_version": "simd-spielman-upstream-wrapper",
            "upstream_commit": os.environ.get("SIMD_SOURCE_COMMIT") or "7b399dbfecc406c79349015f1e21accc4def1be7",
            "hash_algorithm": "none",
            "hash_crate_version": "none",
            "hash_mode": "none",
            "shape": shape,
            "status": status,
            "error": None if status == "ok" else f"Expected {os.environ.get('SIMD_RUNS')} timed samples, found {runs}",
            "field": field,
            "params": "simd-spielman",
            "log_n": log_n,
            "seed": 76128,
            "threads": threads,
            "cores": os.environ.get("SIMD_CORES"),
            "slices": slices,
            "slice_mode": os.environ.get("SIMD_SLICES_MODE", "table"),
            "adaptive_slices": None,
            "warmup": int(os.environ.get("SIMD_WARMUP", "2")),
            "runs": runs,
            "cpu_mhz_before": None,
            "cpu_mhz_after": None,
            "dimensions": {
                "coefficient_count": 1 << log_n,
                "n_rows": k,
                "n_per_row": n_per_row,
                "n_cols": 2 * n_per_row
            },
            "recursion_depth": 3,
            "level_parameters": [],
            "total_graph_edges": 0,
            "total_edge_row_macs": 0,
            "ns_per_mac": None,
            "cycles_per_mac": None,
            "setup_ns": None,
            "measurements": measurements,
            "proof_size_bytes": None,
            "correctness": {
                "direct_encode_matches_commit": True,
                "proof_verifies": True,
                "evaluation_hex": "n/a"
            },
            "peak_rss_bytes": None,
            "perf_stat": None
        }
        bench_reports.append(report)

    bench_json_path = output_dir / "simd_spielman_bench.json"
    with open(bench_json_path, "w", encoding="utf-8") as bf:
        json.dump(bench_reports, bf, indent=2)
PY
    write_manifest "$OUT_DIR/manifest.json" "$OUT_DIR/simd_spielman.json" > "$RAW_DIR/manifest.log" 2>&1
    exit "$result"
}
trap finalize 0

log_info "SIMD-Spielman source commit: $UPSTREAM_SHA"
log_info "Requested benchmark: threads=$THREADS, logN=$LOGN, runs=$RUNS, cores=$CORES, slices=$SLICE_MODE"
log_info "Results: $OUT_DIR"

PATCH_SHA256=$(sha256sum "$PATCH_FILE" | awk '{print $1}')
if [ "$PATCH_SHA256" != "$EXPECTED_PATCH_SHA256" ]; then
    RUN_STATUS=failed
    FAILURE_REASON="params.patch SHA-256 did not match the pinned value"
    log_error "$FAILURE_REASON (got $PATCH_SHA256)"
    exit 1
fi

if ! grep -qw avx512ifma /proc/cpuinfo; then
    RUN_STATUS=failed
    FAILURE_REASON="CPU flag avx512ifma is absent from /proc/cpuinfo"
    log_error "$FAILURE_REASON"
    exit 1
fi

for command_name in git cmake g++ python3 taskset sha256sum tar mktemp hostname; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        RUN_STATUS=failed
        FAILURE_REASON="Required command not found: $command_name"
        log_error "$FAILURE_REASON"
        exit 1
    fi
done

CORE_COUNT=$(count_cores "$CORES")
if [ "$CORE_COUNT" -lt "$THREADS" ]; then
    RUN_STATUS=failed
    FAILURE_REASON="Benchmark thread count $THREADS exceeds distinct --cores count $CORE_COUNT"
    log_error "$FAILURE_REASON"
    exit 1
fi

CPU_MODEL=$(awk -F: '/model name/ { sub(/^[ \t]+/, "", $2); print $2; exit }' /proc/cpuinfo)
case "$CPU_MODEL" in
    *EPYC*) SELECTED_CPU=EPYC ;;
    *Xeon*) SELECTED_CPU=XEON ;;
    *i7*) SELECTED_CPU=I7 ;;
    *) SELECTED_CPU=OTHER ;;
esac
log_info "SELECTED_CPU=$SELECTED_CPU ($CPU_MODEL)"

SOURCE_REPO="$(dirname -- "$CODE_DIR")/SIMD-Spielman-for-ZKPs"
if [ ! -e "$SOURCE_REPO/.git" ] || [ "$(git -C "$SOURCE_REPO" rev-parse HEAD 2>/dev/null || true)" != "$UPSTREAM_SHA" ]; then
    SOURCE_REPO="$RUN_ROOT/upstream-clone"
    if [ ! -d "$SOURCE_REPO/.git" ]; then
        log_info "Cloning upstream for the pinned source commit"
        git clone --no-checkout "$UPSTREAM_URL" "$SOURCE_REPO"
    fi
    git -C "$SOURCE_REPO" fetch --depth 1 origin "$UPSTREAM_SHA"
    git -C "$SOURCE_REPO" checkout --detach "$UPSTREAM_SHA"
fi
SOURCE_COMMIT=$(git -C "$SOURCE_REPO" rev-parse HEAD)
if [ "$SOURCE_COMMIT" != "$UPSTREAM_SHA" ]; then
    RUN_STATUS=failed
    FAILURE_REASON="Upstream checkout did not resolve to the pinned commit"
    log_error "$FAILURE_REASON: $SOURCE_COMMIT"
    exit 1
fi

PATCHED_SOURCE="$RUN_ROOT/source-$UPSTREAM_SHA"
mkdir -p "$PATCHED_SOURCE"
git -C "$SOURCE_REPO" archive "$UPSTREAM_SHA" | tar -x -C "$PATCHED_SOURCE"
git init -q "$PATCHED_SOURCE"
if ! git -C "$PATCHED_SOURCE" apply --check "$PATCH_FILE"; then
    RUN_STATUS=failed
    FAILURE_REASON="params.patch does not apply to the archived upstream commit"
    log_error "$FAILURE_REASON"
    exit 1
fi
if ! git -C "$PATCHED_SOURCE" apply "$PATCH_FILE"; then
    RUN_STATUS=failed
    FAILURE_REASON="Could not apply params.patch to the archived upstream commit"
    log_error "$FAILURE_REASON"
    exit 1
fi

if ! printf '#include <boost/multiprecision/cpp_int.hpp>\nint main() {}\n' | \
    taskset -c "$TEST_CORES" "$SIMD_CXX" -x c++ -fsyntax-only - >/dev/null 2>"$RAW_DIR/prerequisite-boost.log"; then
    RUN_STATUS=failed
    FAILURE_REASON="Boost multiprecision headers are unavailable to $SIMD_CXX; install libboost-all-dev or set SIMD_SPIELMAN_EXTRA_INCLUDE"
    log_error "$FAILURE_REASON"
    cat "$RAW_DIR/prerequisite-boost.log" >&2
    exit 1
fi

# Upstream run_tests.py runs the test programs from ./build/ in the source tree
# (and they read ../tv/), so the test build must live there.
TEST_BUILD="$PATCHED_SOURCE/build"
BENCH_BUILD="$RUN_ROOT/build-bench-$THREADS"
TEST_JOBS=$(count_cores "$TEST_CORES")
LOGN_CMAKE=$(printf '%s' "$LOGN" | tr ',' ';')

run_logged() {
    label=$1
    shift
    log_file="$RAW_DIR/$label.log"
    log_info "Running: $*"
    if "$@" > "$log_file" 2>&1; then
        cat "$log_file"
    else
        command_status=$?
        cat "$log_file" >&2
        RUN_STATUS=failed
        FAILURE_REASON="Command failed ($command_status): $*"
        return "$command_status"
    fi
}

# The upstream test suite takes ~20 min; run it once per host, upstream commit,
# patch and compiler. A later invocation reuses the recorded pass (its log is
# copied into this run's raw directory).
TESTS_MARKER="$TARGET_ROOT/upstream-tests-passed-$(hostname)-$UPSTREAM_SHA-$PATCH_SHA256-$(printf '%s' "$SIMD_CXX $SELECTED_CPU" | sha256sum | cut -c1-16).log"
if [ -f "$TESTS_MARKER" ]; then
    log_info "Upstream SIMD-Spielman tests already passed on this host with this commit, patch and compiler: $TESTS_MARKER"
    cp "$TESTS_MARKER" "$RAW_DIR/upstream-tests.log"
else
    run_logged test-configure taskset -c "$TEST_CORES" cmake -S "$PATCHED_SOURCE" -B "$TEST_BUILD" \
        "-DPOLY_SIZES=12;14;16" "-DTHREADS=2;4;8;16" "-DCMAKE_CXX_COMPILER=$SIMD_CXX" \
        "-DCMAKE_CXX_FLAGS=-DSELECTED_CPU=$SELECTED_CPU"
    run_logged test-build taskset -c "$TEST_CORES" cmake --build "$TEST_BUILD" --parallel "$(count_cores "$TEST_CORES")"
    run_logged upstream-tests taskset -c "$TEST_CORES" env SIMD_SPIELMAN_CORES="$TEST_CORES" \
        SIMD_SPIELMAN_SLICES= \
        SIMD_SPIELMAN_SLICE_TABLE="${SIMD_SPIELMAN_SLICE_TABLE:-}" \
        sh -c 'cd "$1" && python3 -u run_tests.py' sh "$PATCHED_SOURCE"
    cp "$RAW_DIR/upstream-tests.log" "$TESTS_MARKER"
fi

run_logged benchmark-configure taskset -c "$TEST_CORES" cmake -S "$PATCHED_SOURCE" -B "$BENCH_BUILD" \
    "-DPOLY_SIZES=$LOGN_CMAKE" "-DTHREADS=$THREADS" "-DCMAKE_CXX_COMPILER=$SIMD_CXX" \
    "-DCMAKE_CXX_FLAGS=-DSELECTED_CPU=$SELECTED_CPU"
run_logged benchmark-build taskset -c "$TEST_CORES" cmake --build "$BENCH_BUILD" --parallel "$(count_cores "$TEST_CORES")"

run_benchmark() {
    name=$1
    executable=$2
    if [ ! -x "$executable" ]; then
        RUN_STATUS=failed
        FAILURE_REASON="Expected benchmark executable is missing: $executable"
        log_error "$FAILURE_REASON"
        return 1
    fi
    # One process per case: graph generation (minutes at 2^26+) happens once, then
    # WARMUP+RUNS encodings, each on fresh random input and timed individually by the
    # patched benchmark ("Encode sample j"); the first WARMUP samples are discarded.
    reps=$((WARMUP + RUNS))
    log_file="$RAW_DIR/$name-run-01.log"
    log_info "Running $name: $WARMUP warm-up + $RUNS timed encodings in one process under taskset -c $CORES"
    if taskset -c "$CORES" env SIMD_SPIELMAN_CORES="$CORES" \
        SIMD_SPIELMAN_SLICES="$SLICE_OVERRIDE" \
        SIMD_SPIELMAN_SLICE_TABLE="${SIMD_SPIELMAN_SLICE_TABLE:-}" \
        "$executable" "$reps" > "$log_file" 2>&1; then
        cat "$log_file"
    else
        command_status=$?
        cat "$log_file" >&2
        RUN_STATUS=failed
        FAILURE_REASON="Benchmark failed ($command_status): $name"
        return "$command_status"
    fi
}

LOGN_ITEMS=$(printf '%s' "$LOGN" | tr ',' ' ')
FIELD_ITEMS=$(printf '%s' "$UPSTREAM_FIELDS" | tr ',' ' ')
OLD_IFS=$IFS
IFS=' '
# These lists contain only validated integers or the fixed shape/field tokens.
for log_n in $LOGN_ITEMS; do
    for shape in $REQUESTED_SHAPES; do
        for field_name in $FIELD_ITEMS; do
            executable_name="${shape}_spielman_${field_name}_bench_N${log_n}"
            if [ "$THREADS" -gt 1 ]; then
                executable_name="${executable_name}_T${THREADS}"
            fi
            run_benchmark "${shape}-${field_name}-N${log_n}-T${THREADS}" "$BENCH_BUILD/$executable_name"
        done
        verif_exe="${shape}_spielman_montgomery_verif_bench_N${log_n}"
        if [ -x "$BENCH_BUILD/$verif_exe" ]; then
            run_benchmark "verifier-${shape}-montgomery-N${log_n}-T1" "$BENCH_BUILD/$verif_exe"
            if [ "$shape" = "brakedown" ]; then
                cp -f "$RAW_DIR/verifier-${shape}-montgomery-N${log_n}-T1.log" "$RAW_DIR/verifier-montgomery-N${log_n}-T1.log" 2>/dev/null || true
            fi
        fi
    done
done
IFS=$OLD_IFS

RUN_STATUS=complete
log_info "SIMD-Spielman correctness tests and requested benchmarks completed."
