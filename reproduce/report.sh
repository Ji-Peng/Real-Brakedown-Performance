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

# Single report entry point: turns any results tree (archived results/ or a
# fresh reproduce/run.sh run) into analyze.py CSVs/LaTeX/plots, paper-format
# tables, claims.csv, and a compiled tables.pdf (or tables.tex without TeX).

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
CODE_DIR=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

RESULTS=""
SELECTION=""
OUT=""
STRICT=0
ARCHIVED=0
NO_PDF=0
MT_THREADS=""

usage() {
    cat <<EOF
Usage:
  reproduce/report.sh --results DIR --selection FILE --out DIR [--strict] [--no-pdf] [--mt-threads N]
  reproduce/report.sh --archived [--out DIR] [--no-pdf]

Options:
  --archived         Shorthand for --results results --selection results/selection.json --strict --out out/report-archived
  --results DIR      Results root directory
  --selection FILE   Selection file (rbd-selection-v1)
  --out DIR          Output report directory
  --strict           Fatal paper_tables.py assertions (default: lenient)
  --no-pdf           Skip PDF compilation (still writes tables.tex)
  --mt-threads N     Multi-thread comparison point override for analyze.py
  -h, --help         Show this help
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --archived) ARCHIVED=1; shift ;;
        --results) [ $# -ge 2 ] || { echo "[ERROR] --results requires an argument" >&2; exit 2; }; RESULTS="$2"; shift 2 ;;
        --selection) [ $# -ge 2 ] || { echo "[ERROR] --selection requires an argument" >&2; exit 2; }; SELECTION="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || { echo "[ERROR] --out requires an argument" >&2; exit 2; }; OUT="$2"; shift 2 ;;
        --strict) STRICT=1; shift ;;
        --no-pdf) NO_PDF=1; shift ;;
        --mt-threads) [ $# -ge 2 ] || { echo "[ERROR] --mt-threads requires an argument" >&2; exit 2; }; MT_THREADS="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "[ERROR] Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ "$ARCHIVED" -eq 1 ]; then
    RESULTS="${RESULTS:-$CODE_DIR/results}"
    SELECTION="${SELECTION:-$CODE_DIR/results/selection.json}"
    OUT="${OUT:-$CODE_DIR/out/report-archived}"
    STRICT=1
fi

if [ -z "$RESULTS" ] || [ -z "$SELECTION" ] || [ -z "$OUT" ]; then
    echo "[ERROR] --results, --selection, and --out are required (or use --archived)." >&2
    usage >&2
    exit 2
fi

resolve_path() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s\n' "$PWD/$1" ;;
    esac
}

RESULTS_ABS=$(resolve_path "$RESULTS")
SELECTION_ABS=$(resolve_path "$SELECTION")
OUT_ABS=$(resolve_path "$OUT")

ANALYSIS_DIR="$OUT_ABS/analysis"
PAPER_DIR="$OUT_ABS/paper"
CLAIMS_CSV="$OUT_ABS/claims.csv"
mkdir -p "$ANALYSIS_DIR" "$PAPER_DIR"

MPLCONFIGDIR="${MPLCONFIGDIR:-${CARGO_TARGET_DIR:-$CODE_DIR/target-harness}/matplotlib}"
mkdir -p "$MPLCONFIGDIR"
export MPLCONFIGDIR

# 1. Run analyze.py --check (always continue to build the report even if check fails)
ANALYZE_RC=0
set -- "$CODE_DIR/scripts/analyze.py" \
    --results-dir "$RESULTS_ABS" \
    --selection "$SELECTION_ABS" \
    --out-dir "$ANALYSIS_DIR" \
    --no-latex-compile \
    --check
[ -z "$MT_THREADS" ] || set -- "$@" --mt-threads "$MT_THREADS"
if ! python3 "$@"; then
    ANALYZE_RC=1
    echo "[WARN] analyze.py --check reported failures; continuing to build report marked CHECK FAILED." >&2
fi

# 2. Run paper_tables.py (--strict or --lenient)
PAPER_RC=0
MODE_FLAG="--lenient"
[ "$STRICT" -eq 1 ] && MODE_FLAG="--strict"
if ! python3 "$CODE_DIR/scripts/paper_tables.py" \
    --csv-dir "$ANALYSIS_DIR/csv" \
    --results-dir "$RESULTS_ABS" \
    --selection "$SELECTION_ABS" \
    --out-dir "$PAPER_DIR" \
    --claims-out "$CLAIMS_CSV" \
    "$MODE_FLAG"; then
    PAPER_RC=1
    echo "[WARN] paper_tables.py exited non-zero ($MODE_FLAG)." >&2
fi

# 3. Instantiate tables.tex, cover.tex, claims_table.tex, paper_section.tex, appendix_section.tex
cp "$CODE_DIR/scripts/paper_macros.tex" "$OUT_ABS/paper_macros.tex"
cp "$CODE_DIR/scripts/report_tables.tex" "$OUT_ABS/tables.tex"

REP_CODE_DIR="$CODE_DIR" \
REP_RESULTS="$RESULTS_ABS" \
REP_SELECTION="$SELECTION_ABS" \
REP_OUT="$OUT_ABS" \
REP_ANALYZE_RC="$ANALYZE_RC" \
REP_STRICT="$STRICT" \
REP_ARCHIVED="$ARCHIVED" \
python3 - <<'PY'
import csv
import fnmatch
import glob
import json
import os
import pathlib

code_dir = pathlib.Path(os.environ["REP_CODE_DIR"])
results_dir = pathlib.Path(os.environ["REP_RESULTS"])
sel_path = pathlib.Path(os.environ["REP_SELECTION"])
out_dir = pathlib.Path(os.environ["REP_OUT"])
analyze_rc = int(os.environ["REP_ANALYZE_RC"])
strict = os.environ["REP_STRICT"] == "1"
archived = os.environ["REP_ARCHIVED"] == "1"

ESC_MAP = {
    "\\": r"\textbackslash{}", "_": r"\_", "%": r"\%", "&": r"\&", "#": r"\#",
    "$": r"\$", "{": r"\{", "}": r"\}", "^": r"\^{}", "~": r"\~{}",
    "<": r"\textless{}", ">": r"\textgreater{}",
}


def esc(s):
    return "".join(ESC_MAP.get(ch, ch) for ch in str(s))


sel = json.loads(sel_path.read_text(encoding="utf-8")) if sel_path.is_file() else {}
mode = "archived" if archived else (sel.get("mode") or ("strict" if strict else "lenient"))
hosts = list(sel.get("machines", {}).keys())
runs_desc = []
commits = set()
toolchains = set()
for h, cfg in sel.get("machines", {}).items():
    hdir = results_dir / h
    for r in cfg.get("runs", []):
        rpat = r.get("run_dir", "*")
        matched_dirs = sorted(d for d in hdir.iterdir() if d.is_dir() and fnmatch.fnmatch(d.name, rpat)) if hdir.is_dir() else []
        for rd in matched_dirs:
            for s in r.get("sessions", []):
                if (rd / s).is_dir():
                    runs_desc.append(f"{h}/{rd.name}/{s}")
                    for mf in sorted((rd / s).glob("**/manifest.json"))[:5]:
                        try:
                            m = json.loads(mf.read_text(encoding="utf-8"))
                            sha = (m.get("git") or {}).get("code_repo_sha")
                            if sha:
                                commits.add(sha[:12])
                            rv = (m.get("rust_toolchain") or {}).get("rustc_version")
                            if rv:
                                toolchains.add(rv)
                        except Exception:
                            pass

# cover.tex
cover = []
if analyze_rc != 0:
    cover.append(
        r"\par\noindent\fbox{\begin{minipage}{\dimexpr\linewidth-2\fboxsep-2\fboxrule\relax}"
        r"\color{red}\textbf{CHECK FAILED}: \code{scripts/analyze.py --check} exited non-zero; "
        r"inspect the console log and \code{analysis/csv/todo\_cells.csv}."
        r"\end{minipage}}\par\medskip"
    )
if mode == "quick":
    cover.append(
        r"\par\noindent\fbox{\begin{minipage}{\dimexpr\linewidth-2\fboxsep-2\fboxrule\relax}"
        r"\textbf{QUICK MODE}: reduced sizes and repetitions; not comparable cell-by-cell with the paper."
        r"\end{minipage}}\par\medskip"
    )
cover += [
    r"\begin{table}[h]",
    r"\centering",
    r"\begin{tabular}{ll}",
    r"\toprule",
    r"Field & Value \\",
    r"\midrule",
    f"Mode & \\code{{{esc(mode)}}} \\\\",
    f"Hosts & {esc(', '.join(hosts) or 'none')} \\\\",
    f"Runs / sessions & {esc(', '.join(runs_desc) or 'none')} \\\\",
    f"Fork commit(s) & \\code{{{esc(', '.join(sorted(commits)) or 'detached/unknown')}}} \\\\",
    f"Rust toolchain & \\code{{{esc(', '.join(sorted(toolchains)) or 'unknown')}}} \\\\",
    f"Analysis check & {'PASS' if analyze_rc == 0 else 'CHECK FAILED'} \\\\",
    r"\bottomrule",
    r"\end{tabular}",
    r"\end{table}",
]
(out_dir / "cover.tex").write_text("\n".join(cover) + "\n", encoding="utf-8")

# claims_table.tex
claims_path = out_dir / "claims.csv"
claims_lines = []
if claims_path.is_file():
    with open(claims_path, "r", encoding="utf-8", newline="") as f:
        rows = [r for r in csv.reader(f) if r and not r[0].startswith("#")]
    if len(rows) >= 2:
        claims_lines += [
            r"\begin{table}[h]",
            r"\centering",
            r"\begin{talltblr}[caption={Claims evaluation summary (\code{claims.csv})},label={tab:claims-summary}]{",
            r"  colspec = {Q[l] X[1.3,l] X[1.2,l] X[1.3,l] Q[c]},",
            r"  cells = {font=\scriptsize},",
            r"  width = \linewidth,",
            r"  colsep = 3pt,",
            r"}",
            r"\toprule",
            r"ID & Check & Expected (ref) & Observed & Status \\",
            r"\midrule",
        ]
        for r in rows[1:]:
            cid, chk, _scope, exp, obs, st, _tol = (r + [""] * 7)[:7]
            claims_lines.append(
                f"\\textbf{{{esc(cid)}}} & {esc(chk)} & {esc(exp)} & {esc(obs)} & \\textbf{{{esc(st)}}} \\\\"
            )
        claims_lines += [r"\bottomrule", r"\end{talltblr}", r"\end{table}"]
(out_dir / "claims_table.tex").write_text("\n".join(claims_lines) + "\n", encoding="utf-8")

# paper_section.tex (one table per page so tables.pdf has >= 1 page per paper table)
status_map = {}
status_csv = out_dir / "paper" / "paper_tables_status.csv"
if status_csv.is_file():
    with open(status_csv, "r", encoding="utf-8", newline="") as f:
        for r in list(csv.reader(f))[1:]:
            if len(r) >= 3:
                status_map[r[0]] = (r[1], r[2])

paper_tables = [
    ("tab_platforms", "Platforms and builds"),
    ("tab_correctness", "Correctness checks (C0)"),
    ("tab_codegen", "Scalar codegen check (C1)"),
    ("tab_headline", "Headline prover encoding (C2)"),
    ("tab_simd", "Comparison with SIMD-Spielman (C3)"),
    ("tab_steps", "Per-technique breakdown (C4)"),
    ("tab_slices", "LLC slice sweep (C5)"),
    ("tab_fields", "Field arithmetic comparison (C7)"),
    ("tab_pcs", "PCS operations breakdown (C8)"),
    ("tab_setup", "PCS setup, input generation, and peak RSS (C9)"),
    ("tab_arm", "AArch64 portability (C11)"),
]
psec = []
for idx, (stem, title) in enumerate(paper_tables):
    if idx > 0:
        psec.append(r"\clearpage")
    tex_file = out_dir / "paper" / f"{stem}.tex"
    if tex_file.is_file():
        psec.append(rf"\input{{paper/{stem}.tex}}")
        psec.append(r"\FloatBarrier")
    else:
        reason = status_map.get(stem, ("skipped", "not generated"))[1] or "not generated"
        psec.append(rf"\notrunbox{{{esc(title)} (\code{{{esc(stem)}}})}}{{not run: {esc(reason)}}}")

for fig_stem, fig_title in (("fig_llc", "LLC scaling figure (C5)"), ("fig_threads", "Thread scaling figure (C6)")):
    psec.append(r"\clearpage")
    pdf_file = out_dir / "paper" / f"{fig_stem}.pdf"
    if pdf_file.is_file():
        psec += [
            r"\begin{figure}[htbp]",
            r"\centering",
            rf"\includegraphics[width=0.95\linewidth]{{{fig_stem}.pdf}}",
            rf"\caption{{{esc(fig_title)}}}",
            r"\end{figure}",
            r"\FloatBarrier",
        ]
    else:
        reason = status_map.get(fig_stem, ("skipped", "not generated"))[1] or "not generated"
        psec.append(rf"\notrunbox{{{esc(fig_title)} (\code{{{esc(fig_stem)}}})}}{{not run: {esc(reason)}}}")
(out_dir / "paper_section.tex").write_text("\n".join(psec) + "\n", encoding="utf-8")

# appendix_section.tex (all analyze.py tables T0..T12 and figures F1..F3)
asec = []
for i in range(13):
    matches = sorted(glob.glob(str(out_dir / "analysis" / "latex" / f"table_T{i}_*.tex")))
    if matches:
        rel = os.path.relpath(matches[0], out_dir).replace(os.sep, "/")
        # Wrap wide analyze.py tables safely using \scriptsize; rewrite \begin{table*} -> \begin{table}[htbp]
        # inside a small wrapper file so landscape single-column article renders cleanly.
        raw = pathlib.Path(matches[0]).read_text(encoding="utf-8")
        wrapped = (
            raw.replace(r"\begin{table*}[t]", r"\begin{table}[htbp]\scriptsize\renewcommand{\arraystretch}{0.88}")
            .replace(r"\begin{table}[t]", r"\begin{table}[htbp]\scriptsize\renewcommand{\arraystretch}{0.88}")
            .replace(r"\end{table*}", r"\end{table}")
            .replace(r"\begin{threeparttable}", r"\fitlandscape{\begin{threeparttable}")
            .replace(r"\end{threeparttable}", r"\end{threeparttable}}")
        )
        wpath = out_dir / "analysis" / "latex" / f"wrapped_T{i}.tex"
        wpath.write_text(wrapped, encoding="utf-8")
        wrel = os.path.relpath(wpath, out_dir).replace(os.sep, "/")
        asec.append(rf"\input{{{wrel}}}")
        asec.append(r"\FloatBarrier\clearpage")
    else:
        asec.append(rf"\notrunbox{{Analysis Table T{i}}}{{table\_T{i} not generated}}")

for fig_name in ("figure_F1_steps_breakdown", "figure_F2_llc_scaling", "figure_F3_thread_scaling"):
    fp = out_dir / "analysis" / "plots" / f"{fig_name}.pdf"
    if fp.is_file():
        asec += [
            r"\begin{figure}[htbp]",
            r"\centering",
            rf"\includegraphics[width=0.85\linewidth]{{{fig_name}.pdf}}",
            rf"\caption{{{esc(fig_name.replace('_', ' '))}}}",
            r"\end{figure}",
            r"\FloatBarrier\clearpage",
        ]
(out_dir / "appendix_section.tex").write_text("\n".join(asec) + "\n", encoding="utf-8")
PY

# 4. Compile tables.pdf when TeX is available and --no-pdf is not set
PDF_RC=0
HAS_LATEXMK=0
HAS_PDFLATEX=0
command -v latexmk >/dev/null 2>&1 && HAS_LATEXMK=1
command -v pdflatex >/dev/null 2>&1 && HAS_PDFLATEX=1

if [ "$HAS_LATEXMK" -eq 0 ] && [ "$HAS_PDFLATEX" -eq 0 ]; then
    echo "TeX not found: $OUT_ABS/tables.tex written, PDF skipped"
elif [ "$NO_PDF" -eq 1 ]; then
    echo "PDF compilation skipped (--no-pdf): $OUT_ABS/tables.tex written"
elif [ "$HAS_LATEXMK" -eq 1 ]; then
    echo "[report.sh] Compiling $OUT_ABS/tables.tex with latexmk..."
    if ! (cd "$OUT_ABS" && latexmk -pdf -interaction=nonstopmode -synctex=1 tables.tex > "$OUT_ABS/latexmk.log" 2>&1); then
        echo "[ERROR] latexmk failed; see $OUT_ABS/latexmk.log" >&2
        tail -n 40 "$OUT_ABS/latexmk.log" >&2 || true
        PDF_RC=1
    fi
else
    echo "[report.sh] Compiling $OUT_ABS/tables.tex with pdflatex (2 passes)..."
    if ! (cd "$OUT_ABS" && pdflatex -interaction=nonstopmode -synctex=1 tables.tex > "$OUT_ABS/pdflatex.log" 2>&1 && \
          pdflatex -interaction=nonstopmode -synctex=1 tables.tex >> "$OUT_ABS/pdflatex.log" 2>&1); then
        echo "[ERROR] pdflatex failed; see $OUT_ABS/pdflatex.log" >&2
        tail -n 40 "$OUT_ABS/pdflatex.log" >&2 || true
        PDF_RC=1
    fi
fi

# 5. Print summary paths and claims verdict table
echo ""
echo "=========================================================="
echo "Report directory: $OUT_ABS"
echo "  Analysis CSVs:  $ANALYSIS_DIR/csv"
echo "  Paper tables:   $PAPER_DIR"
echo "  Claims verdict: $CLAIMS_CSV"
echo "  LaTeX source:   $OUT_ABS/tables.tex"
if [ -f "$OUT_ABS/tables.pdf" ]; then
    echo "  Compiled PDF:   $OUT_ABS/tables.pdf"
fi
echo "=========================================================="
if [ -f "$CLAIMS_CSV" ]; then
    CLAIMS_PATH="$CLAIMS_CSV" python3 - <<'PY'
import csv, os
with open(os.environ["CLAIMS_PATH"], "r", encoding="utf-8", newline="") as f:
    rows = [r for r in csv.reader(f) if r and not r[0].startswith("#")]
if len(rows) >= 2:
    print(f"{'Claim':<6} {'Status':<9} {'Check':<48} Observed")
    print("-" * 110)
    for r in rows[1:]:
        print(f"{r[0]:<6} {r[5]:<9} {r[1][:47]:<48} {r[4]}")
PY
fi

# 6. Exit 0 only when analysis check, paper_tables, hard claims, and PDF build all pass
if [ "$ANALYZE_RC" -ne 0 ] || [ "$PAPER_RC" -ne 0 ] || [ "$PDF_RC" -ne 0 ]; then
    exit 1
fi
exit 0
