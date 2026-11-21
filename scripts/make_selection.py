#!/usr/bin/env python3
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

"""Generates an rbd-selection-v1 selection.json for a fresh reproduce/run.sh run."""

import argparse
import json
import os
import pathlib
import sys


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

STEP_TAGS = [
    "step/01-modernize",
    "step/02-fields",
    "step/03-column-major",
    "step/04-r2l",
    "step/05-lazy",
    "step/06-blocking",
    "step/07-slicing",
    "step/08-threads",
    "step/09-codegen",
    "HEAD",
]


def load_claims_tsv(code_dir):
    path = os.path.join(code_dir, "reproduce", "claims.tsv")
    rows = []
    with open(path, "r", encoding="utf-8") as f:
        header = None
        for line in f:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if header is None:
                header = parts
                continue
            rows.append(dict(zip(header, parts)))
    return {r["id"]: r for r in rows}


def kdhr26_reference_cpu(cpu_model):
    if cpu_model and "EPYC" in cpu_model.upper():
        return "EPYC 9754"
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results-root", required=True)
    parser.add_argument("--host-label", required=True)
    parser.add_argument("--run", required=True, dest="run_id")
    parser.add_argument("--session", required=True)
    parser.add_argument("--mode", choices=["quick", "full"], required=True)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--claims", required=True, help="Comma-separated claim IDs that ran (e.g. C0,C2)")
    parser.add_argument("--preflight", required=True, help="Path to preflight.json")
    parser.add_argument("--mt-threads", type=int, required=True)
    parser.add_argument("--out", default=None, help="Output path (default: <results-root>/selection.json)")
    args = parser.parse_args()

    code_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    claims_meta = load_claims_tsv(code_dir)

    with open(args.preflight, "r", encoding="utf-8") as f:
        pf = json.load(f)

    pf_claims = {c["id"]: c for c in pf.get("claims", [])}
    dropped_stages = pf.get("dropped_stages") or {}
    cpu_model = pf.get("cpu_model") or "unknown"
    kd_cpu = kdhr26_reference_cpu(cpu_model)
    arch = args.arch
    mode = args.mode
    mt = int(args.mt_threads)
    t_list = [1] if mt <= 1 else [1, mt]
    selected_claims = {c.strip() for c in args.claims.split(",") if c.strip()}

    # Determine which stages actually ran.
    c3_runnable = pf_claims.get("C3", {}).get("status") == "runnable"
    stages_ran = set()
    need_codegen = False
    for cid in selected_claims:
        row = claims_meta.get(cid)
        if not row:
            continue
        raw = row["x86_stages"] if arch == "x86_64" else row["arm_stages"]
        if raw == "-":
            continue
        for tok in [t.strip() for t in raw.split(",") if t.strip()]:
            if tok == "TEST":
                continue
            optional = tok.startswith("+")
            base = tok[1:] if optional else tok
            if base == "CODEGEN":
                need_codegen = True
                continue
            if optional and base == "S" and not c3_runnable:
                continue
            st = base if mode == "full" else FULL_TO_QUICK.get(base)
            if not st or st in dropped_stages:
                continue
            stages_ran.add(st)

    if need_codegen and not (stages_ran & TW_PROVIDING_STAGES):
        stages_ran.add("CODEGEN" if mode == "full" else "QCODEGEN")

    # Also inspect session schedule.json if present to see any blocks skipped at runtime.
    session_sched_path = os.path.join(args.results_root, args.host_label, args.run_id, args.session, "schedule.json")
    skipped_blocks_reasons = {}
    if os.path.isfile(session_sched_path):
        try:
            sched = json.load(open(session_sched_path, "r", encoding="utf-8"))
            for st in sched.get("stages") or []:
                for b in st.get("blocks") or []:
                    if b.get("status") == "skipped":
                        skipped_blocks_reasons[b["id"]] = b.get("skip_reason") or "skipped"
        except Exception:
            pass

    def claim_skip_reason(cid):
        pfc = pf_claims.get(cid, {})
        if pfc.get("status") == "skipped" and pfc.get("reason"):
            return pfc["reason"]
        if cid not in selected_claims:
            return "claim not selected"
        return "quick mode" if mode == "quick" else "not run"

    has_a = ("A" in stages_ran) or ("QA" in stages_ran)
    has_h = ("H" in stages_ran) or ("QH" in stages_ran)
    has_s = ("S" in stages_ran) or ("QS" in stages_ran)
    has_c = ("C" in stages_ran) or ("QC" in stages_ran)
    has_d = ("D" in stages_ran) or ("QD" in stages_ran)
    has_b = "B" in stages_ran
    has_arm = ("ARM" in stages_ran) or ("QARM" in stages_ran)
    has_codegen_only = ("CODEGEN" in stages_ran) or ("QCODEGEN" in stages_ran)
    has_any_tw = bool(stages_ran & (TW_PROVIDING_STAGES | {"CODEGEN", "QCODEGEN"}))

    expected_rows = {}
    omissions = []

    def add_omission(kind, reason, cells):
        if cells:
            omissions.append({"kind": kind, "reason": reason, "cells": cells})

    test_stamp_path = os.path.join(args.results_root, args.host_label, "tests", "test-stamp.json")
    has_test_stamp = os.path.isfile(test_stamp_path)
    has_test_evidence = has_test_stamp or has_any_tw or has_d

    # T0: correctness gates
    if not has_test_evidence:
        if "C0" not in selected_claims:
            add_omission("not-run", "claim not selected", ["T0|*|Status"])
    else:
        if not has_any_tw:
            add_omission("not-run", claim_skip_reason("C1"), ["T0|Vector-instruction audit*|Status"])
            add_omission("not-run", "no timed This Work benchmark blocks", ["T0|PCS end-to-end verification|Status"])
        if not (has_a or has_arm or ("T" in stages_ran) or ("QT" in stages_ran)):
            add_omission("not-run", "no timed original-LCPC benchmark blocks", ["T0|Original LCPC PCS verification|Status"])

    # T1: headline encoding (C2)
    if has_a:
        t1_sizes = [16, 18, 20] if mode == "quick" else ([16, 18, 20, 22, 24, 26] + ([28] if has_b else []))
        expected_rows["T1"] = {"log2(N)": t1_sizes, "T": t_list}
    elif has_codegen_only and arch == "x86_64":
        add_omission("not-run", "claim not selected", [
            "T1|*|LCPC*", "T1|*|Speedup*", "T1|*|Significance*", "T1|*|PolyCommit*",
        ])
    else:
        add_omission("not-run", claim_skip_reason("C2"), ["T1|*|*"])

    # T2: SIMD-Spielman Brakedown (C3)
    if has_a or has_s:
        if has_s:
            if mode == "quick":
                expected_rows["T2"] = {"Field": ["Fp2M61", "Mers127", "Mont127"], "T": t_list, "log2(N)": [16, 18, 20]}
                add_omission("not-run", "quick mode", [
                    "T2|*,*,18|SIMD*", "T2|*,*,18|Gap vs SIMD*", "T2|*,*,18|Measured LCPC*",
                ])
            else:
                t2_sizes = [16, 18, 20, 22, 24, 26] + ([28] if has_b else [])
                expected_rows["T2"] = {"Field": ["Fp2M61", "Mers127", "Mont127"], "T": t_list, "log2(N)": t2_sizes}
        else:
            # QA/A ran tw_b, but SIMD-Spielman (S/QS) did not run.
            add_omission("not-run", claim_skip_reason("C3"), [
                "T2|*|SIMD*", "T2|*|Gap vs SIMD*", "T2|*|Measured LCPC*",
            ])
        if kd_cpu is None:
            add_omission("not-run", "no KDHR26 reference for this CPU", ["T2|*|*KDHR26*"])
    else:
        add_omission("not-run", claim_skip_reason("C3"), ["T2|*|*"])

    # T3: steps progression (C4)
    if has_d:
        expected_rows["T3"] = {"Mode": ["a", "b"], "Step Tag": STEP_TAGS}
    else:
        add_omission("not-run", claim_skip_reason("C4"), ["T3|*|*"])

    # T4: slice sweep (C5)
    if has_c:
        t4_sizes = [20, 22] if mode == "quick" else [20, 24, 26]
        expected_rows["T4"] = {"log2(N)": t4_sizes, "T": t_list}
        if mode == "quick":
            # Quick mode sweeps {1, 4, 16, adaptive}; on CPUs whose adaptive s* is 2 or 8,
            # the explicit-sweep column at adaptive s is not measured.
            add_omission("not-run", "quick mode", [
                "T4|*|Sweep at adaptive s (ms)", "T4|*|Adaptive Gap (%)", "T4|*|Gap <= 10%",
            ])
    else:
        add_omission("not-run", claim_skip_reason("C5"), ["T4|*|*"])

    # T5: field comparison (C7)
    if has_a:
        expected_rows["T5"] = {"Field": ["Mont127", "Mers127", "Fp2M61", "Ft127"]}
        if mode == "quick":
            add_omission("not-run", "quick mode", [
                "T5|*|Encode N=2^24 T=1 (ms)", "T5|*|Cost N=2^24 (ns/MAC)",
            ])
    elif has_any_tw:
        # field_microbenchmarks.json is written by every this_work block, but mode (b) encodings did not run.
        add_omission("not-run", claim_skip_reason("C7"), ["T5|*|Encode*", "T5|*|Cost*"])
    else:
        add_omission("not-run", claim_skip_reason("C7"), ["T5|*|*"])

    # T6: PCS breakdown (C8)
    if has_a:
        t6_sizes = [20] if mode == "quick" else ([20, 24] + ([28] if has_b else []))
        t6_specs = [
            {"Implementation": ["This Work", "Original LCPC as-published-as-is", "Original LCPC fork-flags-portable"],
             "T": t_list, "log2(N)": t6_sizes},
            {"Implementation": ["This Work Ft127"], "T": t_list, "log2(N)": [20] if mode == "quick" else [20, 24]},
        ]
        if has_h:
            t6_specs.append({
                "Implementation": ["Original LCPC as-published-portable", "Original LCPC fork-flags-as-is"],
                "T": t_list, "log2(N)": [20] if mode == "quick" else [20, 24],
            })
        expected_rows["T6"] = t6_specs
    else:
        add_omission("not-run", claim_skip_reason("C8"), ["T6|*|*"])

    # T7: verifier encoding (C8)
    if has_a:
        t7_sizes = [16, 18, 20] if mode == "quick" else ([16, 18, 20, 22, 24, 26] + ([28] if has_b else []))
        expected_rows["T7"] = {"Mode": ["a", "b"], "Field": ["Mont127"], "log2(N)": t7_sizes}
        if has_s and mode == "quick":
            add_omission("not-run", "quick mode", ["T7|b,Mont127,18|SIMD*"])
        elif not has_s:
            add_omission("not-run", claim_skip_reason("C3"), ["T7|b,Mont127,*|SIMD*"])
        if kd_cpu is None:
            add_omission("not-run", "no KDHR26 reference for this CPU", ["T7|*|*KDHR26*"])
    elif has_codegen_only and arch == "x86_64":
        pass  # mode (a) logN=16 row has all cells filled (SIMD/KDHR26 are N/A for mode a)
    else:
        add_omission("not-run", claim_skip_reason("C8"), ["T7|*|*"])

    # T8: setup and memory (C9)
    if has_s:
        add_omission("omitted",
                     "SIMD-Spielman setup is derived from GNU time process wall time minus encode samples, "
                     "so PCS setup, input generation, and verifier initialization cannot be separated",
                     ["T8|SIMD-Spielman,*|PCS setup (ms)", "T8|SIMD-Spielman,*|Input gen. (ms)",
                      "T8|SIMD-Spielman,*|Verifier init (ms)", "T8|SIMD-Spielman,*|PCS setup / Encode"])
    if has_a:
        t8_sizes = [20] if mode == "quick" else ([20, 24] + ([28] if has_b else []))
        t8_impls = ["This Work", "Original LCPC as-published-as-is", "Original LCPC fork-flags-portable"]
        if has_s:
            t8_impls.append("SIMD-Spielman")
        expected_rows["T8"] = {"Implementation": t8_impls, "log2(N)": t8_sizes}
    else:
        add_omission("not-run", claim_skip_reason("C9"), ["T8|*|*"])

    # T9: Orion shape (C3)
    if has_a or has_s:
        if has_s:
            if mode == "quick":
                expected_rows["T9"] = {"Field": ["Fp2M61", "Mers127", "Mont127"], "T": t_list, "log2(N)": [16, 18, 20]}
                add_omission("not-run", "quick mode", ["T9|*,*,18|SIMD*", "T9|*,*,18|Gap vs SIMD*"])
            else:
                t9_sizes = [16, 18, 20, 22, 24, 26] + ([28] if has_b else [])
                expected_rows["T9"] = {"Field": ["Fp2M61", "Mers127", "Mont127"], "T": t_list, "log2(N)": t9_sizes}
                if has_b:
                    add_omission("omitted",
                                 "SIMD-Spielman Orion shape at 2^28 exceeds 90 min single-threaded graph generation "
                                 "(--max-orion-logn 26)",
                                 ["T9|*,*,28|SIMD-Spielman Orion*", "T9|*,*,28|SIMD / This Work",
                                  "T9|*,*,28|SIMD best-of-T*", "T9|*,*,28|Gap vs SIMD best-of-T",
                                  "T9|*,*,28|SIMD MT slower than its T=1", "T9|*,*,28|SIMD params.patch"])
        else:
            add_omission("not-run", claim_skip_reason("C3"), ["T9|*|SIMD*", "T9|*|Gap vs SIMD*"])
        if kd_cpu is None:
            add_omission("not-run", "no KDHR26 reference for this CPU", ["T9|*|*KDHR26*"])
    else:
        add_omission("not-run", claim_skip_reason("C3"), ["T9|*|*"])

    # T10: Ligero RS (C10)
    if has_a:
        expected_rows["T10"] = {"log2(N)": [20] if mode == "quick" else [20, 22, 24]}
    else:
        add_omission("not-run", claim_skip_reason("C10"), ["T10|*|*"])

    # T11: codegen (C1)
    if not has_any_tw:
        add_omission("not-run", claim_skip_reason("C1"), ["T11|*|*"])

    # T12: Arm portability (C11)
    if has_arm:
        t12_sizes = [18, 20] if mode == "quick" else [18, 22, 26]
        expected_rows["T12"] = {"Host": [args.host_label], "log2(N)": t12_sizes, "Threads": t_list}
    else:
        add_omission("not-run", claim_skip_reason("C11"), ["T12|*|*"])

    selection = {
        "schema_version": "rbd-selection-v1",
        "description": f"Auto-generated selection for {mode} run on {args.host_label} (claims: {args.claims}).",
        "results_root": ".",
        "mode": mode,
        "mt_threads": mt,
        "claims_requested": sorted(selected_claims),
        "machines": {
            args.host_label: {
                "role": f"Fresh {mode} run on {args.host_label} ({arch}, {cpu_model})",
                "expected_sessions": [args.session],
                "kdhr26_reference_cpu": kd_cpu,
                "runs": [
                    {
                        "run_dir": args.run_id,
                        "sessions": [args.session],
                        "stage": f"reproduce/run.sh --mode {mode} --claims {args.claims}",
                    }
                ],
            }
        },
        "exclude": [
            {"path": "*/analysis", "reason": "per-session analyze.py output (derived, not raw data)"},
            {"path": "report/*", "reason": "generated report artefacts"},
            {"path": "*/tests/*.log", "reason": "C0 test logs (test-stamp.json feeds T0)"},
            {"path": "*/preflight.json", "reason": "preflight capability report"},
            {"path": "*.prev-*", "reason": "earlier attempt of a re-run block moved aside by reproduce/all.sh"},
        ],
        "omissions": omissions,
        "expected_rows": expected_rows,
    }

    out_path = pathlib.Path(args.out or os.path.join(args.results_root, "selection.json"))
    out_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = out_path.with_suffix(out_path.suffix + ".tmp")
    tmp.write_text(json.dumps(selection, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, out_path)
    print(f"[INFO] Wrote selection: {out_path}")


if __name__ == "__main__":
    main()
