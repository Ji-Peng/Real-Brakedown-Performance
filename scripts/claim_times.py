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

"""Compute per-claim full-mode wall times from archived execution_log.json files.

Reads results/*/*/*/stages/*/execution_log.json, maps stages to claims through
reproduce/claims.tsv and reproduce/stages.tsv, and prints per-claim full-mode
wall time on the authors' archived hosts (Markdown or CSV).
"""

import argparse
import csv
import glob
import json
import os
import pathlib
import sys

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
CODE_DIR = SCRIPT_DIR.parent


def load_tsv(path):
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
    return rows


def fmt_duration(seconds):
    if seconds is None:
        return "n/a"
    if seconds < 60:
        return f"{seconds:.0f} s"
    if seconds < 3600:
        return f"{seconds / 60.0:.0f} min"
    return f"{seconds / 3600.0:.1f} h"


def collect_stage_durations(results_dir):
    """Returns {(host, session): {stage_name: total_seconds}}."""
    by_session = {}
    pattern = str(pathlib.Path(results_dir) / "*" / "*" / "*" / "stages" / "*" / "execution_log.json")
    for p in sorted(glob.glob(pattern)):
        parts = pathlib.Path(p).relative_to(results_dir).parts
        if len(parts) < 6:
            continue
        host, _run, session, _stages_lit, stage_dir = parts[:5]
        stage_name = stage_dir.split("_", 1)[1] if "_" in stage_dir else stage_dir
        try:
            entries = json.loads(pathlib.Path(p).read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        dur = sum(int(e.get("duration_s") or 0) for e in entries if isinstance(e, dict))
        by_session.setdefault((host, session), {})
        by_session[(host, session)][stage_name] = by_session[(host, session)].get(stage_name, 0) + dur
    return by_session


def compute_claim_times(results_dir=None, code_dir=None):
    code_dir = pathlib.Path(code_dir or CODE_DIR)
    results_dir = pathlib.Path(results_dir or (code_dir / "results"))
    claims = load_tsv(code_dir / "reproduce" / "claims.tsv")
    stages_full = {
        (r["arch"], r["name"]): r
        for r in load_tsv(code_dir / "reproduce" / "stages.tsv")
        if r["mode"] == "full"
    }
    by_session = collect_stage_durations(results_dir)

    # Identify full x86-64 sessions (those that ran stages A,H,S,C,T,T28,D,B) and AArch64 sessions (ARM).
    x86_req = {"A", "H", "S", "C", "T", "T28", "D", "B"}
    x86_sessions = [(h, s, d) for (h, s), d in sorted(by_session.items()) if x86_req <= set(d)]
    arm_sessions = [(h, s, d) for (h, s), d in sorted(by_session.items()) if "ARM" in d]

    # Prefer primary x86 session s1 (and report mean across complete sessions when >1)
    # and primary Arm host (longest ARM session or first in selection order).
    def stage_mean(sessions, st_name):
        vals = [d[st_name] for _, _, d in sessions if st_name in d]
        return (sum(vals) / len(vals)) if vals else None

    x86_total = sum(stage_mean(x86_sessions, st) or 0 for st in x86_req) if x86_sessions else None
    # Use the primary cluster Arm session (first session matching selection.json order, else first).
    sel_order = []
    sel_path = results_dir / "selection.json"
    if sel_path.is_file():
        sel_order = list(json.loads(sel_path.read_text(encoding="utf-8")).get("machines", {}).keys())
    arm_sessions.sort(key=lambda x: (sel_order.index(x[0]) if x[0] in sel_order else 999, x[0], x[1]))
    arm_primary = arm_sessions[:1]
    arm_total = stage_mean(arm_primary, "ARM") if arm_primary else None

    rows = []
    for c in claims:
        cid = c["id"]
        x_raw = c["x86_stages"]
        a_raw = c["arm_stages"]
        if cid == "C0":
            full_str = "~2 min (tests)"
            secs = 120.0
            stages_used = "TEST"
            host_used = "x86-64 / AArch64"
        elif cid == "C1":
            full_str = "included in C2/C11"
            secs = 0.0
            stages_used = "CODEGEN"
            host_used = "x86-64 / AArch64"
        elif x_raw != "-" and x86_sessions:
            sts = [t.lstrip("+").strip() for t in x_raw.split(",") if t.strip()]
            sts = [st for st in sts if ("x86_64", st) in stages_full]
            secs = sum(stage_mean(x86_sessions, st) or 0 for st in sts)
            full_str = fmt_duration(secs)
            stages_used = ",".join(sts)
            host_used = x86_sessions[0][0]
        elif a_raw != "-" and arm_primary:
            sts = [t.lstrip("+").strip() for t in a_raw.split(",") if t.strip()]
            sts = [st for st in sts if ("aarch64", st) in stages_full]
            secs = sum(stage_mean(arm_primary, st) or 0 for st in sts)
            full_str = fmt_duration(secs)
            stages_used = ",".join(sts)
            host_used = arm_primary[0][0]
        else:
            secs = None
            full_str = "n/a"
            stages_used = "-"
            host_used = "-"
        rows.append({
            "claim_id": cid,
            "stages": stages_used,
            "host": host_used,
            "full_seconds": round(secs) if secs is not None else "",
            "full_time": full_str,
            "quick_time": "TODO(measure)",
        })

    totals = {
        "x86_sessions": [(h, s, sum(d.values())) for h, s, d in x86_sessions],
        "x86_mean_seconds": round(x86_total) if x86_total is not None else None,
        "x86_mean_human": fmt_duration(x86_total),
        "arm_sessions": [(h, s, d.get("ARM", 0)) for h, s, d in arm_sessions],
        "arm_primary_seconds": round(arm_total) if arm_total is not None else None,
        "arm_primary_human": fmt_duration(arm_total),
    }
    return rows, totals


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results-dir", default=str(CODE_DIR / "results"))
    parser.add_argument("--format", choices=["markdown", "csv", "json"], default="markdown")
    args = parser.parse_args()

    rows, totals = compute_claim_times(results_dir=args.results_dir)
    if args.format == "csv":
        w = csv.writer(sys.stdout)
        w.writerow(["claim_id", "stages", "host", "full_seconds", "full_time", "quick_time"])
        for r in rows:
            w.writerow([r["claim_id"], r["stages"], r["host"], r["full_seconds"], r["full_time"], r["quick_time"]])
    elif args.format == "json":
        print(json.dumps({"claims": rows, "totals": totals}, indent=2))
    else:
        print("| Claim | Stages | Archived host | Full-mode wall time | Quick-mode time |")
        print("| :--- | :--- | :--- | ---: | ---: |")
        for r in rows:
            print(f"| {r['claim_id']} | `{r['stages']}` | `{r['host']}` | {r['full_time']} | {r['quick_time']} |")
        print("")
        print("Archived full-session totals (non-overlapping stages):")
        for h, s, sec in totals["x86_sessions"]:
            print(f"- `{h}/{s}`: {fmt_duration(sec)} ({sec} s)")
        for h, s, sec in totals["arm_sessions"]:
            print(f"- `{h}/{s}`: {fmt_duration(sec)} ({sec} s)")


if __name__ == "__main__":
    main()
