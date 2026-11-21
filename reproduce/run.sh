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

# Single portable entry point for RBP-LCPC artifact evaluation.
# Sequence: preflight -> fetch_deps -> test (C0) -> stages -> make_selection -> report.
# Exit codes:
#   0 = every scheduled block and report succeeded
#   1 = a benchmark block or report failed
#   2 = usage or preflight error
#   3 = correctness tests (C0) failed

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

if [ -f "${CARGO_HOME:-$HOME/.cargo}/env" ]; then
    # shellcheck disable=SC1091
    . "${CARGO_HOME:-$HOME/.cargo}/env"
fi

# Capture exact invocation for stage manifests.
RBD_CLI="reproduce/run.sh"
for _arg in "$@"; do
    RBD_CLI="$RBD_CLI $_arg"
done
export RBD_CLI

MODE="quick"
CLAIMS="all"
OUT="./out"
CORES=""
MAX_THREADS="8"
HOST_LABEL=""
SESSION_ID=""
SEED=""
NO_REPORT=0
DRY_RUN=0
LIST_CLAIMS=0
MARKDOWN=0
KEEP_GOING=0

usage() {
    cat <<EOF
Usage: reproduce/run.sh [--mode quick|full] [--claims all|C0,C2,...] [--out DIR]
                        [--cores LIST] [--max-threads N] [--host-label NAME]
                        [--session ID] [--seed N] [--no-report] [--dry-run]
                        [--list-claims [--markdown]] [--keep-going]

Defaults: --mode quick, --claims all, --out ./out, --max-threads 8.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --mode) [ $# -ge 2 ] || { echo "[ERROR] --mode requires an argument" >&2; exit 2; }; MODE="$2"; shift 2 ;;
        --claims) [ $# -ge 2 ] || { echo "[ERROR] --claims requires an argument" >&2; exit 2; }; CLAIMS="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || { echo "[ERROR] --out requires an argument" >&2; exit 2; }; OUT="$2"; shift 2 ;;
        --cores) [ $# -ge 2 ] || { echo "[ERROR] --cores requires an argument" >&2; exit 2; }; CORES="$2"; shift 2 ;;
        --max-threads) [ $# -ge 2 ] || { echo "[ERROR] --max-threads requires an argument" >&2; exit 2; }; MAX_THREADS="$2"; shift 2 ;;
        --host-label|--host) [ $# -ge 2 ] || { echo "[ERROR] $1 requires an argument" >&2; exit 2; }; HOST_LABEL="$2"; shift 2 ;;
        --session) [ $# -ge 2 ] || { echo "[ERROR] --session requires an argument" >&2; exit 2; }; SESSION_ID="$2"; shift 2 ;;
        --seed) [ $# -ge 2 ] || { echo "[ERROR] --seed requires an argument" >&2; exit 2; }; SEED="$2"; shift 2 ;;
        --no-report) NO_REPORT=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --list-claims) LIST_CLAIMS=1; shift ;;
        --markdown) MARKDOWN=1; shift ;;
        --keep-going) KEEP_GOING=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "[ERROR] Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$MODE" in
    quick|full) ;;
    *) echo "[ERROR] --mode must be quick or full (got '$MODE')" >&2; exit 2 ;;
esac
case "$MAX_THREADS" in
    ''|*[!0-9]*|0) echo "[ERROR] --max-threads must be a positive integer" >&2; exit 2 ;;
esac

if [ -n "${RUSTFLAGS:-}" ] || [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
    echo "[ERROR] RUSTFLAGS and CARGO_ENCODED_RUSTFLAGS must be unset for reproducible runs" >&2
    exit 2
fi

if [ -z "$HOST_LABEL" ]; then
    HOST_LABEL=$(hostname -s 2>/dev/null || echo unknown-host)
    HOST_LABEL=$(printf '%s' "$HOST_LABEL" | tr -c 'A-Za-z0-9._-' '_')
fi

if [ -z "$SESSION_ID" ]; then
    if [ "$MODE" = "quick" ]; then
        SESSION_ID="quick1"
    else
        SESSION_ID="full1"
    fi
fi

case "$OUT" in
    /*) OUT_ROOT="$OUT" ;;
    *) OUT_ROOT="$CODE_DIR/$OUT" ;;
esac

if [ "$OUT_ROOT" = "$CODE_DIR/results" ]; then
    echo "[WARN] Writing into archived results/ directory; omit --out to write to out/ instead." >&2
fi

# --list-claims [--markdown]
if [ "$LIST_CLAIMS" -eq 1 ]; then
    if [ "$MARKDOWN" -eq 1 ]; then
        CODE_DIR_ENV="$CODE_DIR" python3 - <<'PY'
import csv
import os
import pathlib
import sys

code_dir = pathlib.Path(os.environ["CODE_DIR_ENV"])
sys.path.insert(0, str(code_dir / "scripts"))
import claim_times  # noqa: E402

claims_path = code_dir / "reproduce" / "claims.tsv"
rows = claim_times.load_tsv(claims_path)
times_list, _totals = claim_times.compute_claim_times(
    results_dir=code_dir / "results", code_dir=code_dir
)
times_by_id = {r["claim_id"]: r for r in times_list}

ref_claims = {}
ref_path = code_dir / "scripts" / "paper_reference" / "claims.csv"
if ref_path.is_file():
    with open(ref_path, "r", encoding="utf-8", newline="") as f:
        for r in csv.DictReader(f):
            if r.get("claim_id"):
                ref_claims[r["claim_id"]] = r

print("| ID | Paper location | Command | Output (CSV / paper table) | Full-mode time | Quick-mode time | Requirements | Expected result and tolerance |")
print("| :--- | :--- | :--- | :--- | ---: | ---: | :--- | :--- |")
for r in rows:
    cid = r["id"]
    cmd = f"`reproduce/run.sh --mode quick --claims C0,{cid}`" if cid != "C0" else "`reproduce/run.sh --mode quick --claims C0`"
    reqs = r["requires"] if r["requires"] != "-" else "any Linux"
    tinfo = times_by_id.get(cid, {})
    full_t = tinfo.get("full_time", "n/a")
    quick_t = tinfo.get("quick_time", "TODO(measure)")
    rc = ref_claims.get(cid)
    if rc:
        exp_tol = f"{rc['expected']} ({rc['tolerance_rule']})"
    else:
        exp_tol = r["title"]
    # Escape pipes so Markdown table structure never breaks.
    cells = [
        f"**{cid}**",
        r["paper_refs"],
        cmd,
        r["tables"],
        full_t,
        quick_t,
        reqs,
        exp_tol,
    ]
    print("| " + " | ".join(c.replace("|", "\\|") for c in cells) + " |")
PY
        exit 0
    fi
    set -- --mode "$MODE" --claims "$CLAIMS" --out "$OUT_ROOT" --max-threads "$MAX_THREADS" --host-label "$HOST_LABEL" --json-out -
    [ -z "$CORES" ] || set -- "$@" --cores "$CORES"
    sh "$SCRIPT_DIR/preflight.sh" "$@"
    exit $?
fi

# 1. Preflight
PREFLIGHT_JSON="$OUT_ROOT/$HOST_LABEL/preflight.json"
if [ "$DRY_RUN" -eq 1 ]; then
    PREFLIGHT_JSON=$(mktemp "${TMPDIR:-/tmp}/rbd-preflight.XXXXXX")
    trap 'rm -f "$PREFLIGHT_JSON"' EXIT INT TERM
fi

set -- --mode "$MODE" --claims "$CLAIMS" --out "$OUT_ROOT" --max-threads "$MAX_THREADS" --host-label "$HOST_LABEL" --json-out "$PREFLIGHT_JSON"
[ -z "$CORES" ] || set -- "$@" --cores "$CORES"
sh "$SCRIPT_DIR/preflight.sh" "$@" || exit 2

# Resolve stages and parameters from preflight.json, claims.tsv, and stages.tsv
PLAN_FILE=$(mktemp "${TMPDIR:-/tmp}/rbd-plan.XXXXXX")
if [ "$DRY_RUN" -eq 1 ]; then
    trap 'rm -f "$PREFLIGHT_JSON" "$PLAN_FILE"' EXIT INT TERM
else
    trap 'rm -f "$PLAN_FILE"' EXIT INT TERM
fi

PLAN_CODE_DIR="$CODE_DIR" PLAN_PREFLIGHT="$PREFLIGHT_JSON" PLAN_MODE="$MODE" \
PLAN_CLAIMS="$CLAIMS" PLAN_OUT="$PLAN_FILE" \
python3 - <<'PY'
import json
import os
import sys

code_dir = os.environ["PLAN_CODE_DIR"]
mode = os.environ["PLAN_MODE"]
claims_arg = os.environ["PLAN_CLAIMS"]

with open(os.environ["PLAN_PREFLIGHT"], "r", encoding="utf-8") as f:
    pf = json.load(f)

arch = pf["arch"]
claims_by_id = {c["id"]: c for c in pf["claims"]}

# Validate --claims
if claims_arg.lower() == "all":
    selected_ids = [c["id"] for c in pf["claims"] if c["status"] == "runnable"]
else:
    req_ids = [x.strip() for x in claims_arg.split(",") if x.strip()]
    unknown = [x for x in req_ids if x not in claims_by_id]
    if unknown:
        print(f"[ERROR] Unknown claim id(s): {', '.join(unknown)}", file=sys.stderr)
        sys.exit(2)
    selected_ids = []
    for cid in req_ids:
        c = claims_by_id[cid]
        if c["status"] != "runnable":
            print(f"[WARN] Requested claim {cid} cannot run on this host ({c['reason']}); skipping.", file=sys.stderr)
        else:
            selected_ids.append(cid)

# Load stages.tsv
stages_path = os.path.join(code_dir, "reproduce", "stages.tsv")
stages_table = {}
stages_order = []
with open(stages_path, "r", encoding="utf-8") as f:
    header = None
    for line in f:
        line = line.rstrip("\n")
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if header is None:
            header = parts
            continue
        row = dict(zip(header, parts))
        key = (row["mode"], row["arch"], row["name"])
        stages_table[key] = row
        stages_order.append(key)

# Map full stage names in claims.tsv to quick stage names
FULL_TO_QUICK = {
    "A": "QA",
    "H": "QH",
    "S": "QS",
    "C": "QC",
    "T": "QT",
    "T28": None,
    "D": "QD",
    "B": None,
    "ARM": "QARM",
    "CODEGEN": "QCODEGEN",
}
TW_PROVIDING_STAGES = {"A", "QA", "C", "QC", "T", "QT", "T28", "B", "ARM", "QARM"}

c3_runnable = claims_by_id.get("C3", {}).get("status") == "runnable"
dropped_stages = pf.get("dropped_stages") or {}

needed_stages = set()
need_codegen = False
for cid in selected_ids:
    c = claims_by_id[cid]
    raw_stages = c["x86_stages"] if arch == "x86_64" else c["arm_stages"]
    if raw_stages == "-":
        continue
    for tok in [t.strip() for t in raw_stages.split(",") if t.strip()]:
        if tok == "TEST":
            continue
        optional = tok.startswith("+")
        base = tok[1:] if optional else tok
        if base == "CODEGEN":
            need_codegen = True
            continue
        if optional and base == "S" and not c3_runnable:
            continue
        st_name = base if mode == "full" else FULL_TO_QUICK.get(base)
        if not st_name:
            continue
        if st_name in dropped_stages:
            continue
        needed_stages.add(st_name)

# Only add standalone CODEGEN/QCODEGEN when C1 was requested and no This Work stage runs.
if need_codegen and not (needed_stages & TW_PROVIDING_STAGES):
    needed_stages.add("CODEGEN" if mode == "full" else "QCODEGEN")

ordered_stages = []
for key in stages_order:
    m, a, name = key
    if m == mode and a == arch and name in needed_stages:
        ordered_stages.append(stages_table[key])

# Also record full stage catalog for this mode/arch for dry-run reporting
all_mode_stages = [
    stages_table[k] for k in stages_order
    if k[0] == mode and k[1] == arch and k[2] not in ("CODEGEN", "QCODEGEN")
]

plan = {
    "arch": arch,
    "mode": mode,
    "mt_threads": pf["cores"]["mt_threads"],
    "selected_claims": selected_ids,
    "dropped_stages": dropped_stages,
    "stages": ordered_stages,
    "all_mode_stages": all_mode_stages,
}
with open(os.environ["PLAN_OUT"], "w", encoding="utf-8") as f:
    json.dump(plan, f, indent=2)
PY

if [ "$DRY_RUN" -eq 1 ]; then
    PLAN_FILE_ENV="$PLAN_FILE" python3 - <<'PY'
import json, os

with open(os.environ["PLAN_FILE_ENV"], "r", encoding="utf-8") as f:
    plan = json.load(f)

print(f"\n[DRY-RUN] mode={plan['mode']} arch={plan['arch']} mt_threads={plan['mt_threads']} "
      f"claims={','.join(plan['selected_claims'])}")
print("name\tblocks_regex\tlogn\tlcpc_variants\twarmup\tstatus")
scheduled_names = {s["name"] for s in plan["stages"]}
dropped = plan.get("dropped_stages") or {}
# Print stages in canonical order; include QCODEGEN/CODEGEN if scheduled.
rows = list(plan["all_mode_stages"])
for s in plan["stages"]:
    if s["name"] not in {r["name"] for r in rows}:
        rows.append(s)
for s in rows:
    if s["name"] in scheduled_names:
        status = "run"
    elif s["name"] in dropped:
        status = f"dropped({dropped[s['name']]})"
    else:
        status = "skipped"
    print(f"{s['name']}\t{s['blocks_regex']}\t{s['logn']}\t{s['lcpc_variants']}\t{s['warmup']}\t{status}")
PY
    exit 0
fi

# 2. Fetch dependencies (idempotent)
DEPS_ARGS=""
if [ "$MODE" = "quick" ]; then
    # Only fetch step/* crates when C4 (steps) is actually scheduled.
    if ! PLAN_FILE_ENV="$PLAN_FILE" python3 -c 'import json,os,sys; p=json.load(open(os.environ["PLAN_FILE_ENV"])); sys.exit(0 if any(s["name"] in ("D","QD") for s in p["stages"]) else 1)'; then
        DEPS_ARGS="--skip-steps-fetch"
    fi
fi
# shellcheck disable=SC2086
sh "$SCRIPT_DIR/fetch_deps.sh" $DEPS_ARGS

# Source deps/env.sh so test.sh and all.sh see SIMD_SPIELMAN_TV_DIR and Boost includes
DEPS_DIR="${RBD_DEPS_DIR:-$CODE_DIR/deps}"
if [ -f "$DEPS_DIR/env.sh" ]; then
    # shellcheck disable=SC1090
    . "$DEPS_DIR/env.sh"
fi

# 3. Correctness tests (C0 gate; abort with exit 3 on failure)
set -- --out "$OUT_ROOT" --host-label "$HOST_LABEL" --max-threads "$MAX_THREADS"
[ -z "$CORES" ] || set -- "$@" --cores "$CORES"
if ! sh "$SCRIPT_DIR/test.sh" "$@"; then
    echo "[ERROR] C0 correctness tests failed; aborting without running benchmarks." >&2
    exit 3
fi

RBD_TEST_STAMP="$OUT_ROOT/$HOST_LABEL/tests/test-stamp.json"
export RBD_TEST_STAMP

# 4. Run each selected stage via reproduce/all.sh
RUN_DATE=$(date -u +%Y-%m-%d)
CODE_SHA=$(git -C "$CODE_DIR" rev-parse --short=12 HEAD 2>/dev/null || echo "detached")
RUN_ID="${RUN_DATE}-${CODE_SHA}"
ARCH=$(uname -m)
MT_THREADS=$(PLAN_FILE_ENV="$PLAN_FILE" python3 -c 'import json,os; print(json.load(open(os.environ["PLAN_FILE_ENV"]))["mt_threads"])')
export RBD_MODE="$MODE"
export RBD_MT_THREADS="$MT_THREADS"

STAGES_TSV=$(mktemp "${TMPDIR:-/tmp}/rbd-run-stages.XXXXXX")
trap 'rm -f "$PLAN_FILE" "$STAGES_TSV"' EXIT INT TERM
PLAN_FILE_ENV="$PLAN_FILE" python3 - <<'PY' > "$STAGES_TSV"
import json, os
plan = json.load(open(os.environ["PLAN_FILE_ENV"], encoding="utf-8"))
for s in plan["stages"]:
    print("\t".join([s["name"], s["blocks_regex"], s["logn"], s["lcpc_variants"], s["warmup"]]))
PY

ALL_PROFILE="$MODE"
if [ "$MODE" = "full" ] && [ "$ARCH" = "aarch64" ]; then
    ALL_PROFILE="arm"
fi

ANY_FAILED=0
TAB=$(printf '\t')
while IFS="$TAB" read -r st_name st_regex st_logn st_lcpc st_warmup; do
    [ -n "$st_name" ] || continue
    echo "[run.sh] === Stage $st_name (profile=$ALL_PROFILE logN=$st_logn warmup=$st_warmup) ==="
    set -- --profile "$ALL_PROFILE" --stage "$st_name" --blocks "$st_regex" \
        --logn "$st_logn" --warmup "$st_warmup" --lcpc-variants "$st_lcpc" \
        --host-label "$HOST_LABEL" --out "$OUT_ROOT" --session "$SESSION_ID" \
        --run-date "$RUN_DATE" --max-threads "$MAX_THREADS"
    [ -z "$CORES" ] || set -- "$@" --cores "$CORES"
    [ -z "$SEED" ] || set -- "$@" --seed "$SEED"
    if ! sh "$SCRIPT_DIR/all.sh" "$@" </dev/null; then
        echo "[WARN] Stage $st_name reported one or more failed blocks." >&2
        ANY_FAILED=1
        if [ "$KEEP_GOING" -eq 0 ]; then
            # Per spec §1.3, one stage's failure is recorded and the run continues.
            :
        fi
    fi
done < "$STAGES_TSV"

# 5. Generate selection.json
SELECTED_CLAIMS=$(PLAN_FILE_ENV="$PLAN_FILE" python3 -c 'import json,os; print(",".join(json.load(open(os.environ["PLAN_FILE_ENV"]))["selected_claims"]))')
python3 "$CODE_DIR/scripts/make_selection.py" \
    --results-root "$OUT_ROOT" \
    --host-label "$HOST_LABEL" \
    --run "$RUN_ID" \
    --session "$SESSION_ID" \
    --mode "$MODE" \
    --arch "$ARCH" \
    --claims "$SELECTED_CLAIMS" \
    --preflight "$PREFLIGHT_JSON" \
    --mt-threads "$MT_THREADS"

# 6. Report (unless --no-report)
REPORT_RC=0
if [ "$NO_REPORT" -eq 0 ]; then
    if [ -x "$SCRIPT_DIR/report.sh" ] || [ -f "$SCRIPT_DIR/report.sh" ]; then
        sh "$SCRIPT_DIR/report.sh" --results "$OUT_ROOT" --selection "$OUT_ROOT/selection.json" --out "$OUT_ROOT/report" || REPORT_RC=$?
    else
        echo "[run.sh] reproduce/report.sh not yet present; running scripts/analyze.py --check directly."
        mkdir -p "$OUT_ROOT/report/analysis"
        MPLCONFIGDIR="${CARGO_TARGET_DIR:-$CODE_DIR/target-harness}/matplotlib"
        mkdir -p "$MPLCONFIGDIR"
        export MPLCONFIGDIR
        python3 "$CODE_DIR/scripts/analyze.py" \
            --results-dir "$OUT_ROOT" \
            --selection "$OUT_ROOT/selection.json" \
            --mt-threads "$MT_THREADS" \
            --out-dir "$OUT_ROOT/report/analysis" \
            --check || REPORT_RC=$?
    fi
fi

if [ "$ANY_FAILED" -ne 0 ] || [ "$REPORT_RC" -ne 0 ]; then
    exit 1
fi
exit 0
