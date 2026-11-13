#!/usr/bin/env python3
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

"""
analyze.py - Aggregates raw benchmark JSON results and produces paper artefacts.

Generates the evaluation artefacts:
  - Tables T0 - T12 (LaTeX booktabs in latex/ and CSV summaries in csv/)
  - Figures F1 - F3 (PDF and PNG in plots/ via matplotlib, with their data in csv/)

Result selection (paper mode)
  Paper artefacts are built only from the run directories listed in
  results/selection.json (default; override with --selection). Every
  measurement record found under the results root must be either included by
  that file or excluded by one of its "exclude" rules (each with a reason);
  any other record is "uncovered" and the analyzer refuses to run. Passing
  --results-dir without --selection analyzes everything under those
  directories (used by reproduce/all.sh for per-session and local smoke
  output); such output is not paper evidence.

Cells and statistics
  - A cell is one configuration: (implementation, build variant, field, code
    parameters, matrix shape, slice mode and slice count, log2 N, threads,
    block family, machine). The analyzer refuses to pool records whose cell
    keys differ (for example slice-sweep runs with the adaptive headline, or
    two original-LCPC build variants).
  - Sessions come from the session directory's schedule.json "session_id"
    (fallback: the directory name). Each session contributes exactly one
    record per cell; a second, different record is a duplicate and --check
    fails. Byte-identical copies of one measurement (the mode-(b) driver writes
    a combined and a per-shape file) are dropped at load time.
  - Per session: median of that session's timed samples. Table value: mean of
    the session medians, with the min-max over all samples. Cells whose
    session medians differ by more than 5% are marked "D<x>%" in the table and
    listed in csv/session_divergence.csv. Cells measured in fewer sessions than
    the machine's expected sessions are marked "[s1 only]" and fail --check
    unless selection.json documents them as omissions.
  - Speedup = ratio of the mean session medians, with its range over the
    per-session ratios. "faster" only when, in every common session, the
    slowest This Work sample is faster than the fastest baseline sample
    (non-overlapping ranges); otherwise "comparable" (or "slower").
  - Provenance footer on every table: the run directories and sessions that
    contributed, with their fork commit, Slurm job, node and baseline/patch hashes.
  - Published [KDHR26] values load only from scripts/kdhr26_reported.json and
    are looked up by (reference CPU of the machine, log2 N).
  - --check fails on missing artefacts or cells, duplicates, missing sessions,
    oversubscribed pinning and heterogeneous cells, except for documented
    omissions in selection.json (and --allow selectors).
"""

import argparse
import csv
import fnmatch
import glob
import hashlib
import json
import lzma
import math
import os
import re
import sys
from collections import defaultdict
from typing import Any, Dict, Iterable, List, Optional, Set, Tuple

try:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    HAS_MATPLOTLIB = True
except ImportError:
    HAS_MATPLOTLIB = False


SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
DEFAULT_SELECTION = os.path.normpath(os.path.join(SCRIPT_DIR, "..", "results", "selection.json"))

# Original LCPC is reported "as published" and "with the fork's
# flags"; the BLAKE3 backend is a second axis. Order = column order in tables.
LCPC_VARIANTS = ("as-published-as-is", "as-published-portable", "fork-flags-as-is", "fork-flags-portable")
PRIMARY_LCPC_VARIANTS = ("as-published-as-is", "fork-flags-portable")
# The other two variants are measured only at the sizes below (stage RH of the
# 375a6e1 re-measurement), so that T6 can separate the BLAKE3 backend from the
# compiler flags; no other table reports them.
SECONDARY_LCPC_VARIANTS = ("as-published-portable", "fork-flags-as-is")
SECONDARY_LCPC_SIZES = (20, 24)
# This Work over Ft127 (full_this_work_ft127_*) is measured at these sizes and
# reported in T6 only.
FT127_SIZES = (20, 24)
# Block families measured as same-boot pairs (This Work next to original LCPC):
# superseding is per cell, so a re-run that lands for one side only would pair
# records from different runs; analyze.py warns about it.
PARTNER_FAMILIES = (("tw_threads", "lcpc_threads"), ("arm_this_work", "arm_lcpc_orig"))

DIVERGENCE_THRESHOLD = 0.05
MEASURED_STATUSES = {None, "", "ok", "complete", "completed", "success", "pass"}

# Block-id families (shared interface 4). Profile prefixes (full_, smoke_) are
# stripped first; Arm blocks keep their own families.
_FAMILY_RULES = [
    ("arm_this_work", r"^arm_this_work"),
    ("arm_lcpc_orig", r"^arm_lcpc_orig"),
    ("tw_a", r"^this_work_mode_a(_|$)"),
    ("tw_b", r"^this_work_mode_b(_|$)"),
    ("tw_ft127", r"^this_work_ft127_"),
    ("slice_sweep", r"^(slice_sweep_|this_work_slice_s)"),
    ("tw_threads", r"^(this_work_threads_|thread_scaling_this_work_)"),
    ("lcpc_threads", r"^(lcpc_orig_threads_|thread_scaling_lcpc_orig_)"),
    ("steps", r"^steps"),
    ("lcpc_a", r"^lcpc_orig_(mode_)?a(_|$)"),
    ("simd", r"^simd_spielman"),
    ("polycommit", r"^polycommit"),
    ("ligero", r"^ligero_rs"),
]


def block_family(block_id: str) -> Optional[str]:
    """Maps a block id (e.g. full_this_work_mode_a_t1_n16-...) to its family."""
    if not block_id:
        return None
    if block_id.startswith("arm_"):
        stripped = block_id
    else:
        stripped = re.sub(r"^(full|smoke)_", "", block_id)
    for family, pattern in _FAMILY_RULES:
        if re.search(pattern, stripped):
            return family
    return None


def infer_build_variant(record: Dict, filename: str) -> Tuple[Optional[str], bool]:
    """Returns (build_variant, recorded) for an original-LCPC record.

    New records carry "build_variant"; older ones are named
    lcpc-sdig6-{plain,fork}-release-blake3-{as-is,portable-only}.json.
    """
    recorded = record.get("build_variant")
    if recorded:
        return str(recorded), True
    name = os.path.basename(filename)
    profile = "as-published" if "plain-release" in name else ("fork-flags" if "fork-release" in name else None)
    if "portable-only" in name:
        hashing = "portable"
    elif "as-is" in name:
        hashing = "as-is"
    else:
        hashing = None
    if profile and hashing:
        return f"{profile}-{hashing}", False
    return None, False


def median(values: List[float]) -> Optional[float]:
    if not values:
        return None
    s = sorted(values)
    n = len(s)
    return float(s[n // 2]) if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2.0


def safe_float(val: Any) -> Optional[float]:
    try:
        f = float(val)
    except (ValueError, TypeError):
        return None
    return None if math.isnan(f) else f


def format_ns(ns: Optional[float], unit: str = "ms", decimals: Optional[int] = None) -> str:
    if ns is None:
        return "-"
    div, dec = {"ms": (1e6, 2), "us": (1e3, 1), "s": (1e9, 3)}[unit]
    return f"{ns / div:.{dec if decimals is None else decimals}f}"


def format_ns_to_ms(ns: Optional[float], decimals: int = 2) -> str:
    return format_ns(ns, "ms", decimals)


def format_bytes_to_mb(b: Optional[float], decimals: int = 1) -> str:
    if b is None or b <= 0:
        return "-"
    return f"{b / (1024.0 * 1024.0):.{decimals}f}"


_LATEX_TEXT_ESCAPES = {
    "\\": r"\textbackslash{}", "&": r"\&", "%": r"\%", "_": r"\_", "#": r"\#", "{": r"\{", "}": r"\}",
    "~": r"\textasciitilde{}", "^": r"\textasciicircum{}", "<": r"\textless{}", ">": r"\textgreater{}",
    "|": r"\textbar{}", "$": r"\$",
}
_LATEX_UNICODE = {"\u2264": r"$\le$", "\u2265": r"$\ge$", "\u2192": r"$\rightarrow$", "\u00d7": r"$\times$",
                  "\u2248": r"$\approx$", "\u00b5": r"$\mu$", "\u03bc": r"$\mu$"}
# Powers written in the analyzer's plain text ("2^26", "2^{n}", "N=2^20") become
# math; "<=" / ">=" become \le / \ge. Everything else is escaped as text.
_LATEX_MATH_RE = re.compile(r"(?P<pow>(?P<base>\d+|[A-Za-z])\^(?:\{(?P<be>[^{}]+)\}|(?P<e>\d+|[A-Za-z]\b)))"
                            r"|(?P<le><=)|(?P<ge>>=)|(?P<math>\$[^$]+\$)")


def _escape_latex_text(s: str) -> str:
    out = []
    for ch in s:
        out.append(_LATEX_TEXT_ESCAPES.get(ch) or _LATEX_UNICODE.get(ch) or ch)
    return "".join(out)


def escape_latex(s: str) -> str:
    """Converts analyzer text to LaTeX that compiles in text mode.

    Every LaTeX special character is escaped; powers such as 2^26 or 2^{n}
    are emitted as math ($2^{26}$), "<=" / ">=" as $\\le$ / $\\ge$, and an
    explicit $...$ segment is kept as math.
    """
    s = str(s)
    out, pos = [], 0
    for m in _LATEX_MATH_RE.finditer(s):
        out.append(_escape_latex_text(s[pos:m.start()]))
        if m.group("pow"):
            exp = m.group("be") or m.group("e")
            out.append(f"${m.group('base')}^{{{_escape_latex_text(exp)}}}$")
        elif m.group("le"):
            out.append(r"$\le$")
        elif m.group("ge"):
            out.append(r"$\ge$")
        else:
            out.append(m.group("math"))
        pos = m.end()
    out.append(_escape_latex_text(s[pos:]))
    return "".join(out)


LATEX_PREAMBLE = (
    "\\documentclass{article}\n\\usepackage[T1]{fontenc}\n\\usepackage[utf8]{inputenc}\n"
    "\\usepackage[a3paper,landscape,margin=1cm]{geometry}\n\\usepackage{booktabs}\n\\usepackage{threeparttable}\n"
    "\\begin{document}\n"
)


def compile_latex_tables(tex_paths: List[str], build_dir: str, pdflatex: Optional[str] = None) -> Tuple[List[str], List[str]]:
    """Compiles each generated table standalone with pdflatex (booktabs + threeparttable).

    Returns (compiled, errors). Each table gets its own wrapper document in
    build_dir, so an error is attributed to one table. Raises FileNotFoundError
    when pdflatex is not installed.
    """
    import shutil
    import subprocess
    exe = pdflatex or shutil.which("pdflatex")
    if not exe:
        raise FileNotFoundError("pdflatex not found")
    os.makedirs(build_dir, exist_ok=True)
    compiled, errors = [], []
    for tex in sorted(tex_paths):
        name = os.path.splitext(os.path.basename(tex))[0]
        wrapper = os.path.join(build_dir, f"{name}_doc.tex")
        with open(wrapper, "w", encoding="utf-8") as f:
            f.write(LATEX_PREAMBLE + f"\\input{{{os.path.abspath(tex)}}}\n\\end{{document}}\n")
        proc = subprocess.run([exe, "-interaction=nonstopmode", "-halt-on-error", "-file-line-error",
                               "-output-directory", build_dir, wrapper],
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, cwd=build_dir, timeout=120)
        log = proc.stdout.decode("utf-8", "replace").splitlines()
        if proc.returncode != 0:
            first = next((l for l in log if l.startswith("!") or re.search(r"\.tex:\d+:", l)), "")
            errors.append(f"{os.path.basename(tex)}: pdflatex exit {proc.returncode}: {first.strip()} "
                          f"(log {os.path.join(build_dir, name + '_doc.log')})")
        else:
            compiled.append(tex)
    return compiled, errors


def cell_missing_reason(value: Any, header: str, row_values: List[str]) -> Optional[str]:
    """Returns why a generated table cell is missing, or None if it is a value.

    "N/A (<reason>)" is a documented not-applicable value and is accepted;
    bare "N/A", "-", empty, NaN, SKIPPED, TODO and "omitted" are missing and need a
    documented omission (selection.json) or --allow selector.
    """
    text = "" if value is None else str(value).strip()
    normalized = text.casefold()
    column = header.casefold()
    row_context = " ".join(row_values).casefold()

    if normalized in {"timeout", "oom"} and "status" in column:
        return None
    if normalized in {"n/a", "na"} and re.search(r"(^|\s)b(\s|$)|mode b", row_context):
        match = re.search(r"step[/_ ]0?(\d+)", row_context)
        if match and int(match.group(1)) < 4:
            return None

    if not text:
        return "empty"
    if normalized in {"-", "nan", "+nan", "-nan", "null", "none", "n/a", "na", "timeout", "oom"}:
        return text
    if normalized.startswith(("skipped", "todo", "omitted")):
        return text
    if re.match(r"^n/a\s*\(.+\)$", normalized):
        return None
    if normalized.startswith("n/a"):
        return text
    return None


def parse_allow_pattern(raw: str) -> str:
    """Validates an allow selector formatted as TABLE|ROW|COLUMN (glob syntax)."""
    if len(raw.split("|")) != 3 or any(part == "" for part in raw.split("|")):
        raise argparse.ArgumentTypeError("--allow must be TABLE|ROW|COLUMN (each part may use * globs)")
    return raw


def selector_matches(selector: str, patterns: Iterable[str]) -> bool:
    return any(selector == p or fnmatch.fnmatchcase(selector, p) for p in patterns)


def _match_value(actual: Any, expected: Any) -> bool:
    if isinstance(expected, (list, tuple, set, frozenset)):
        return any(_match_value(actual, e) for e in expected)
    # Compare as lower-case text so JSON false/null match "false"/"none".
    return fnmatch.fnmatchcase(str(actual).lower(), str(expected).lower())


def matching_patterns(selectors: Iterable[str], patterns: Iterable[str]) -> List[str]:
    """The patterns (exact or glob) that match any of the selectors."""
    sels = list(selectors)
    return [p for p in patterns if any(s == p or fnmatch.fnmatchcase(s, p) for s in sels)]


_CHECK_CODEGEN: Dict[str, Any] = {}


def load_check_codegen():
    """Imports scripts/check_codegen.py by path (works under python3 -I); None if unavailable."""
    if "module" not in _CHECK_CODEGEN:
        try:
            import importlib.util
            spec = importlib.util.spec_from_file_location("rbd_check_codegen", os.path.join(SCRIPT_DIR, "check_codegen.py"))
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            _CHECK_CODEGEN["module"] = module
        except Exception:
            _CHECK_CODEGEN["module"] = None
    return _CHECK_CODEGEN["module"]


# A stored disassembly line as written by check_codegen.py: "<addr>: <mnemonic> <operands>".
_STORED_LINE_RE = re.compile(r"^\s*([0-9a-fA-F]+):\s+(\S+)(?:\s+(.*))?$")


def recheck_codegen_report(report: Dict) -> Optional[Dict[str, Any]]:
    """Re-classifies a codegen report's stored hot-kernel disassembly with the current check_codegen.py.

    check_codegen.py stores every instruction line of every hot kernel, so the
    re-check covers the whole hot-kernel code, not only the reported hits.
    Returns None when check_codegen.py cannot be imported. Keys: arith, moves,
    scalar, lines, complete (stored lines == the report's hot-kernel
    instruction count), unparsed, classifier, hits_reclassified (kind -> count
    for the report's own vector-arithmetic hits) and examples (up to two of
    the report's hits that the current rules no longer count as arithmetic).
    """
    cc = load_check_codegen()
    if cc is None:
        return None
    arch = str(report.get("arch", "x86_64"))
    out: Dict[str, Any] = {"arith": 0, "moves": 0, "scalar": 0, "lines": 0, "unparsed": 0,
                           "classifier": getattr(cc, "CLASSIFIER_VERSION", "?"), "hits_reclassified": defaultdict(int),
                           "examples": []}
    try:
        for sym in report.get("symbols", []):
            if not sym.get("is_hot_kernel"):
                continue
            for line in sym.get("disassembly_lines") or []:
                m = _STORED_LINE_RE.match(str(line))
                if not m:
                    out["unparsed"] += 1
                    continue
                out["lines"] += 1
                kind = cc.classify_instruction(arch, m.group(2), m.group(3) or "")[0]
                out["arith" if kind == "VEC_ARITH" else ("moves" if kind == "VEC_MOVE" else "scalar")] += 1
            for hit in sym.get("vector_arithmetic", []):
                text = str(hit.get("insn", "")).strip()
                mnem, _, ops = text.partition(" ")
                kind = cc.classify_instruction(arch, mnem, ops)[0]
                out["hits_reclassified"][kind] += 1
                if kind != "VEC_ARITH" and len(out["examples"]) < 2:
                    out["examples"].append(re.sub(r"\s*<.*$", " <...>", text))
    except Exception:
        return None
    total = ((report.get("summary") or {}).get("hot_kernels") or {}).get("total_instructions")
    out["complete"] = bool(out["lines"]) and out["unparsed"] == 0 and (total is None or int(total) == out["lines"])
    out["hits_reclassified"] = dict(out["hits_reclassified"])
    return out


def short_symbol(name: str) -> str:
    """encode::encode_column_block::<lcpc_fields::mont127::Mont127> -> encode_column_block::<Mont127>."""
    return re.sub(r"(?:[A-Za-z_][A-Za-z0-9_]*::)+([A-Za-z_][A-Za-z0-9_]*)", r"\1", name)


# ---------------------------------------------------------------------------
# Result selection (shared interface 2)
# ---------------------------------------------------------------------------

class Selection:
    """results/selection.json: which run dirs/sessions feed the paper artefacts.

    Schema (rbd-selection-v1):
      results_root:   directory the paths are relative to (default: the file's dir)
      machines:       {machine: {expected_sessions, kdhr26_reference_cpu,
                       runs: [{run_dir, sessions, stage, status?, note?}]}}
      exclude:        [{path: glob, match?: {field: glob}, reason}] - checked first;
                       "path" is a path glob (fnmatch, '*' also matches '/'),
                       not a block-id glob: it is matched against
                       machine/run/session/block and each of its prefixes, and
                       against the file path below the results root; "match"
                       is matched against record fields plus build_variant /
                       build_variant_recorded / family.
      omissions:      [{cells: [TABLE|ROW|COLUMN glob], kind: pending|omitted, reason}]
                       Omissions only document missing or not-applicable cells
                       (and cells measured in fewer sessions than expected,
                       expected rows that are absent, and FAIL codegen reports
                       via T11/T3 selectors). They never excuse integrity
                       errors (duplicates, mixed configurations,
                       oversubscription, uncovered records, interrupted
                       blocks). Omission patterns that match no cell are
                       warned about.
      supersede:      [{by: [machine/run glob], blocks: [block path glob], reason}]
                       Applied after "exclude", among included records: when
                       a run matching "by" contributes records of a cell in a
                       block matching "blocks", every record of that cell in
                       any other run's matching block is excluded with the
                       reason (all sessions: a cell is never mixed across the
                       superseding and the superseded runs). Inert while no
                       run matches "by". The cell is the configuration
                       (implementation, machine, family, LCPC build variant,
                       field, parameters, shape, slice mode, explicit slice
                       count, log2 N, threads); codegen reports are superseded
                       per (machine, block family).
      expected_rows:  {TABLE: {key column header: [values]} or a list of
                       such specs} - the rows each table must contain (the
                       cartesian product of each spec, in the table's
                       key-column order; a list is the union of its specs,
                       e.g. rows that exist only at some sizes); a missing row
                       is reported by --check as TABLE|v1,v2,...|(row) unless
                       an omission or --allow selector documents it.
      runs[].run_dir may be a glob (e.g. a re-measurement whose run directory
      name is known only up to its commit prefix); it is inert until a
      matching directory exists.
    """

    def __init__(self, path: str):
        self.path = os.path.abspath(path)
        with open(self.path, "rb") as f:
            raw = f.read()
        self.sha256 = hashlib.sha256(raw).hexdigest()
        data = json.loads(raw.decode("utf-8"))
        if data.get("schema_version") != "rbd-selection-v1":
            raise ValueError(f"{path}: unsupported schema_version {data.get('schema_version')!r}")
        self.data = data
        self.root = os.path.normpath(os.path.join(os.path.dirname(self.path), data.get("results_root", ".")))
        self.machines: Dict[str, Dict] = data.get("machines", {})
        self.exclude: List[Dict] = data.get("exclude", [])
        self.omissions: List[Dict] = data.get("omissions", [])
        self.supersede: List[Dict] = data.get("supersede", [])
        # TABLE -> list of specs ({key column header: [values]}); a single
        # spec may be written without the list.
        self.expected_rows: Dict[str, List[Dict[str, List[Any]]]] = {}
        self.matched_patterns: Set[str] = set()
        for i, ex in enumerate(self.exclude):
            if not ex.get("reason") or not (ex.get("path") or ex.get("match")):
                raise ValueError(f"{path}: exclude[{i}] needs a path or match and a reason")
        for i, om in enumerate(self.omissions):
            if not om.get("reason") or not om.get("cells") or om.get("kind") not in ("pending", "omitted"):
                raise ValueError(f"{path}: omissions[{i}] needs cells, kind (pending|omitted) and a reason")
        for i, sp in enumerate(self.supersede):
            if not sp.get("reason") or not sp.get("by") or not sp.get("blocks"):
                raise ValueError(f"{path}: supersede[{i}] needs by, blocks and a reason")
            for k in ("by", "blocks"):
                if isinstance(sp[k], str):
                    sp[k] = [sp[k]]
        for table, specs in data.get("expected_rows", {}).items():
            specs = [specs] if isinstance(specs, dict) else specs
            if not isinstance(specs, list) or not specs or not all(
                    isinstance(spec, dict) and spec and all(isinstance(v, list) and v for v in spec.values())
                    for spec in specs):
                raise ValueError(f"{path}: expected_rows[{table}] must map key column headers to non-empty value "
                                 "lists (or be a non-empty list of such maps)")
            self.expected_rows[table] = specs

    def run_entry(self, machine: str, run: str) -> Optional[Dict]:
        entries = self.machines.get(machine, {}).get("runs", [])
        for entry in entries:
            if entry.get("run_dir") == run:
                return entry
        for entry in entries:
            if fnmatch.fnmatchcase(run, str(entry.get("run_dir", ""))):
                return entry
        return None

    def expected_sessions(self, machine: str) -> Optional[List[str]]:
        m = self.machines.get(machine)
        return list(m["expected_sessions"]) if m and m.get("expected_sessions") else None

    def kdhr26_cpu(self, machine: str) -> Optional[str]:
        return self.machines.get(machine, {}).get("kdhr26_reference_cpu")

    def classify(self, block_rel: str, file_rel: str, fields: Optional[Dict]) -> Tuple[str, str]:
        """Returns ("exclude", reason) | ("include", "") | ("uncovered", "")."""
        parts = block_rel.split("/")
        prefixes = ["/".join(parts[:i]) for i in range(1, len(parts) + 1)]
        for ex in self.exclude:
            pattern = ex.get("path")
            if pattern and not (fnmatch.fnmatchcase(file_rel, pattern)
                                or any(fnmatch.fnmatchcase(p, pattern) for p in prefixes)):
                continue
            match = ex.get("match")
            if match:
                if fields is None or not all(_match_value(fields.get(k), v) for k, v in match.items()):
                    continue
            return "exclude", ex["reason"]
        if len(parts) >= 3:
            entry = self.run_entry(parts[0], parts[1])
            if entry and parts[2] in entry.get("sessions", []):
                return "include", ""
        return "uncovered", ""

    def omission_for(self, selector: str) -> Optional[Dict]:
        """The first omission documenting the selector; every matching pattern is credited as used."""
        first = None
        for om in self.omissions:
            cells = om["cells"] if isinstance(om["cells"], list) else [om["cells"]]
            hit = [p for p in cells if selector == p or fnmatch.fnmatchcase(selector, p)]
            if hit:
                self.matched_patterns.update(hit)
                first = first or om
        return first

    def omission_patterns(self) -> List[str]:
        out = []
        for om in self.omissions:
            out.extend(om["cells"] if isinstance(om["cells"], list) else [om["cells"]])
        return out

    def unmatched_omission_patterns(self) -> List[str]:
        """Omission cell patterns that documented no cell (stale once the data landed)."""
        return [p for p in self.omission_patterns() if p not in self.matched_patterns]

    def listed_runs(self) -> List[Tuple[str, str, List[str], Dict]]:
        """Listed runs; a glob run_dir expands to the matching directories (none while pending)."""
        out = []
        for machine, m in self.machines.items():
            for entry in m.get("runs", []):
                run = entry["run_dir"]
                if any(ch in run for ch in "*?["):
                    mdir = os.path.join(self.root, machine)
                    names = sorted(os.listdir(mdir)) if os.path.isdir(mdir) else []
                    for name in names:
                        if fnmatch.fnmatchcase(name, run) and os.path.isdir(os.path.join(mdir, name)):
                            out.append((machine, name, list(entry.get("sessions", [])), entry))
                else:
                    out.append((machine, run, list(entry.get("sessions", [])), entry))
        return out

    def pending_run_globs(self) -> List[Tuple[str, str, Dict]]:
        """Glob run_dir entries that match no directory yet (inert, e.g. a re-measurement still running)."""
        out = []
        for machine, m in self.machines.items():
            mdir = os.path.join(self.root, machine)
            names = sorted(os.listdir(mdir)) if os.path.isdir(mdir) else []
            for entry in m.get("runs", []):
                run = entry["run_dir"]
                if any(ch in run for ch in "*?[") and not any(fnmatch.fnmatchcase(n, run) for n in names):
                    out.append((machine, run, entry))
        return out


# ---------------------------------------------------------------------------
# Self-test (fast, no data needed)
# ---------------------------------------------------------------------------

def run_self_test() -> None:
    assert cell_missing_reason("", "Latency", ["20"]) == "empty"
    assert cell_missing_reason("NaN", "Latency", ["20"]) == "NaN"
    assert cell_missing_reason("SKIPPED", "Latency", ["20"]) == "SKIPPED"
    assert cell_missing_reason("-", "Latency", ["20"]) == "-"
    assert cell_missing_reason("timeout", "Status", ["20"]) is None
    assert cell_missing_reason("oom", "Status", ["20"]) is None
    assert cell_missing_reason("timeout", "Latency", ["20"]) == "timeout"
    assert cell_missing_reason("N/A", "Latency", ["mode b", "step/03-fields"]) is None
    assert cell_missing_reason("N/A", "Latency", ["mode b", "step/04-r2l"]) == "N/A"
    assert cell_missing_reason("N/A (KDHR26 reports Mont127 only)", "Latency", ["20"]) is None
    assert cell_missing_reason("TODO (pending)", "Latency", ["20"]) == "TODO (pending)"
    assert cell_missing_reason("12.30 [12.1-12.5]", "Latency", ["20"]) is None
    assert fnmatch.fnmatchcase("T12|18,1|This Work Arm (ms)", "T12|*|This Work Arm (ms)")
    assert block_family("full_this_work_mode_a_t1_n16-18-20-22-24-26") == "tw_a"
    assert block_family("full_slice_sweep_s4_t8_n20-24-26") == "slice_sweep"
    assert block_family("smoke_lcpc_orig_a_t1") == "lcpc_a"
    assert block_family("full_simd_spielman_mont127_t7_n28") == "simd"
    assert block_family("arm_lcpc_orig_t8_n18-22-26") == "arm_lcpc_orig"
    assert block_family("full_thread_scaling_this_work_t4") == "tw_threads"
    assert infer_build_variant({}, "lcpc-sdig6-plain-release-blake3-as-is.json") == ("as-published-as-is", False)
    assert infer_build_variant({}, "x/lcpc-sdig6-fork-release-blake3-portable-only.json") == ("fork-flags-portable", False)
    assert infer_build_variant({}, "lcpc-sdig6-plain-release-blake3-portable-only.json") == ("as-published-portable", False)
    assert infer_build_variant({}, "lcpc-sdig6-fork-release-blake3-as-is.json") == ("fork-flags-as-is", False)
    assert infer_build_variant({"build_variant": "fork-flags-as-is"}, "a.json") == ("fork-flags-as-is", True)
    assert median([3, 1, 2]) == 2 and median([1, 2, 3, 4]) == 2.5
    assert escape_latex("N=2^{n} (ms)") == "N=$2^{n}$ (ms)"
    assert escape_latex("N/A (no KDHR26 EPYC 9754 entry for 2^28)") == "N/A (no KDHR26 EPYC 9754 entry for $2^{28}$)"
    assert escape_latex("gap <= 10% a_b #1 {x} ~ ^ <") == (r"gap $\le$ 10\% a\_b \#1 \{x\} \textasciitilde{} "
                                                         r"\textasciicircum{} \textless{}")
    assert escape_latex(r"$\log_2 N$ & more") == r"$\log_2 N$ \& more"
    assert block_family("full_this_work_mode_a_t1_n20.prev-20261023T101010Z") == "tw_a"
    assert short_symbol("lcpc_brakedown_pc::encode::encode_column_block::<lcpc_fields::mont127::Mont127>") == \
        "encode_column_block::<Mont127>"
    assert matching_patterns(["T1|20,1|x"], ["T1|*|x", "T2|*|x"]) == ["T1|*|x"]
    # check_codegen.py is imported by path, so the re-check also works under python3 -I.
    cc = load_check_codegen()
    assert cc is not None and cc.classify_instruction("aarch64", "b.eq", "b3334 <f+0x40>")[0] != "VEC_ARITH"
    rc = recheck_codegen_report({"arch": "aarch64", "summary": {"hot_kernels": {"total_instructions": 2}}, "symbols": [
        {"is_hot_kernel": True, "disassembly_lines": ["b332c: b.eq b3334 <f+0x40>", "b3330: add v0.2d, v1.2d, v2.2d"],
         "vector_arithmetic": [{"insn": "b.eq b3334 <f+0x40>"}]}]})
    assert rc["complete"] and rc["arith"] == 1 and rc["hits_reclassified"] == {"SCALAR_BRANCH": 1}
    print("[PASS] analyze.py self-test (cell rules, families, LCPC variant inference, median, LaTeX escaping, "
          "codegen re-check)")


# ---------------------------------------------------------------------------
# Analyzer
# ---------------------------------------------------------------------------

class BenchmarkAnalyzer:
    def __init__(self, out_dir: str, kdhr26_path: str, is_smoke: bool = False,
                 selection: Optional[Selection] = None, compile_latex: bool = True,
                 latex_build_dir: Optional[str] = None):
        self.out_dir = out_dir
        self.compile_latex = compile_latex
        self.latex_build_dir = latex_build_dir
        self.kdhr26_path = kdhr26_path
        self.is_smoke = is_smoke
        self.selection = selection

        self.csv_dir = os.path.join(out_dir, "csv")
        self.latex_dir = os.path.join(out_dir, "latex")
        self.plots_dir = os.path.join(out_dir, "plots")
        for d in (self.csv_dir, self.latex_dir, self.plots_dir):
            os.makedirs(d, exist_ok=True)

        # Loaded measurement records (each dict carries "_meta").
        self.reports_this_work: List[Dict] = []
        self.reports_lcpc_orig: List[Dict] = []
        self.reports_simd_spielman: List[Dict] = []
        self.reports_polycommit: List[Dict] = []
        self.reports_steps: Dict[str, List[Dict]] = defaultdict(list)
        self.reports_arm: List[Dict] = []
        self.reports_ligero_rs: List[Dict] = []
        self.field_benchmarks: List[Dict] = []
        self.codegen_reports: List[Dict] = []
        self.unmeasured: List[Dict] = []          # status failed / skipped / not_applicable
        self.skipped_baselines: Dict[str, str] = {}

        # Load bookkeeping
        self.result_dirs: List[str] = []
        self.excluded: Dict[str, int] = defaultdict(int)     # reason -> record count
        self.uncovered: List[str] = []
        self.exact_duplicates_dropped = 0
        self.block_id_mismatches: List[str] = []
        self.session_info: Dict[Tuple[str, str, str], Dict[str, Set[str]]] = defaultdict(lambda: defaultdict(set))
        self.session_ids: Set[str] = set()
        self.pinning_errors: List[str] = []
        self.hosts: Set[str] = set()
        self.has_smoke_watermark = False
        self.profiles: Set[str] = set()
        self.profile: Optional[str] = None
        self._seen_keys: Set[Tuple] = set()
        self.missing_schedule: Set[str] = set()
        self.skipped_prev_dirs: List[str] = []
        # (machine, run, session_dir) -> session-level schedule.json "stages" list (interface a)
        self.session_stages: Dict[Tuple[str, str, str], List[Dict]] = {}
        # (machine, run, session_dir, block) -> SIMD-Spielman params.patch sha256 prefix
        self.block_simd_patch: Dict[Tuple[str, str, str, str], Set[str]] = defaultdict(set)
        self.failed_block_records: Dict[str, int] = defaultdict(int)
        # Records of blocks whose run was interrupted (re-run moved the old
        # output aside but never finished): partial data, an integrity error.
        self.interrupted_block_records: Dict[str, int] = defaultdict(int)
        self.failed_blocks: Set[Tuple[str, str, str, str]] = set()
        # Included records/reports wait here until the selection's supersede rules ran.
        self._pending: List[Tuple[str, Dict]] = []     # ("record" | "codegen", item with _meta)
        self.superseded: Dict[str, int] = defaultdict(int)   # block_rel -> superseded record count
        self.partner_run_mismatches: List[str] = []     # same-boot pairs whose sides come from different runs
        self.step_codegen_reports: List[Dict] = []      # per-tag reports of the steps blocks (T3 gate)

        # Generation bookkeeping
        # Integrity errors (duplicates, mixed configurations): never excused by
        # omissions or --allow (interface d).
        self.cell_errors: List[Tuple[str, str]] = []     # (selector, message)
        # Missing-value errors (fewer sessions than expected, FAIL codegen
        # reports): excused by a matching omission or --allow selector.
        self.missing_cell_errors: List[Tuple[str, str]] = []
        # Selected codegen reports that are not PASS and not documented (interface c).
        self.codegen_errors: List[str] = []
        self.cell_log: Dict[str, Dict] = {}
        self.todo_cells: Dict[str, str] = {}             # selector -> reason
        self._used: Set[Tuple[str, str, str]] = set()
        self._used_records: List[Dict] = []
        self._current_table: str = ""
        self.matched_allow: Set[str] = set()             # --allow selectors that excused something

        self.kdhr26_data: Dict[str, Any] = {}
        self._load_kdhr26()

    def _load_kdhr26(self):
        if os.path.isfile(self.kdhr26_path):
            try:
                with open(self.kdhr26_path, "r", encoding="utf-8") as f:
                    self.kdhr26_data = json.load(f)
                print(f"[INFO] Loaded KDHR26 reference data from {self.kdhr26_path}")
            except Exception as e:
                print(f"[WARN] Failed to load KDHR26 data from {self.kdhr26_path}: {e}")
        else:
            print(f"[WARN] KDHR26 data file not found at {self.kdhr26_path}")

    # ------------------------------------------------------------------
    # Loading
    # ------------------------------------------------------------------

    def _locate(self, path: str, scan_root: str, cache: Dict[str, Optional[str]]) -> Dict[str, Any]:
        """Derives machine/run/session/block for a file from its directory layout.

        The session directory is the nearest ancestor holding schedule.json;
        the block is the next path component below it.
        """
        directory = os.path.dirname(os.path.abspath(path))
        session_dir = None
        probe = directory
        stop = os.path.dirname(os.path.abspath(scan_root))
        while probe and probe != stop and probe != os.path.dirname(probe):
            # <session>/stages/<tag>/ holds one stage's schedule/manifest/log
            # (interface a); it is part of its session, never a session itself.
            if os.path.basename(probe) == "stages" or os.path.basename(os.path.dirname(probe)) == "stages":
                probe = os.path.dirname(probe)
                continue
            if probe not in cache:
                cache[probe] = os.path.join(probe, "schedule.json") if os.path.isfile(os.path.join(probe, "schedule.json")) else None
            if cache[probe]:
                session_dir = probe
                break
            probe = os.path.dirname(probe)
        root = self.selection.root if self.selection else os.path.abspath(scan_root)
        if session_dir:
            rel_inside = os.path.relpath(os.path.abspath(path), session_dir).split(os.sep)
            block = rel_inside[0] if len(rel_inside) > 1 else ""
            stage_meta = block == "stages"
            if stage_meta:
                block = ""   # stage-level metadata belongs to the session, not to a block
            run_dir = os.path.dirname(session_dir)
            machine_dir = os.path.dirname(run_dir)
            session_name = os.path.basename(session_dir)
            try:
                with open(os.path.join(session_dir, "schedule.json"), "r", encoding="utf-8") as f:
                    sched = json.load(f)
                session_id = str(sched.get("session_id") or session_name)
            except Exception:
                session_id = session_name
            meta = {"machine": os.path.basename(machine_dir), "run": os.path.basename(run_dir),
                    "session_dir": session_name, "session": session_id, "block": block,
                    "session_path": session_dir, "stage_meta": stage_meta}
        elif self.selection and len(os.path.relpath(os.path.abspath(path), root).split(os.sep)) >= 5:
            # Session copied back without its schedule.json (still running or
            # partial copy): fall back to the machine/run/session/block layout.
            machine, run, session_name, block = os.path.relpath(os.path.abspath(path), root).split(os.sep)[:4]
            self.missing_schedule.add(f"{machine}/{run}/{session_name}")
            meta = {"machine": machine, "run": run, "session_dir": session_name, "session": session_name,
                    "block": "" if block == "stages" else block, "stage_meta": block == "stages",
                    "session_path": os.path.join(root, machine, run, session_name)}
        else:
            rel = os.path.relpath(os.path.abspath(path), os.path.abspath(scan_root)).split(os.sep)
            meta = {"machine": "", "run": "", "session_dir": "", "session": "", "block": rel[0] if len(rel) > 1 else "",
                    "session_path": os.path.abspath(scan_root), "stage_meta": False}
            if meta["block"] == "stages":
                meta["block"], meta["stage_meta"] = "", True
        meta["file_rel"] = os.path.relpath(os.path.abspath(path), root).replace(os.sep, "/")
        meta["block_rel"] = "/".join(p for p in (meta["machine"], meta["run"], meta["session_dir"], meta["block"]) if p)
        if not meta["machine"]:
            meta["block_rel"] = meta["file_rel"].rsplit("/", 1)[0] if "/" in meta["file_rel"] else ""
        return meta

    def load_results(self, root_paths: List[str]):
        """Discovers JSON results, applies the selection and categorizes records."""
        self.result_dirs.extend(root_paths)
        cache: Dict[str, Optional[str]] = {}
        for root in root_paths:
            for dirpath, dirnames, filenames in os.walk(root):
                dirnames.sort()
                # Per-session analyze.py outputs are derived files, never inputs.
                dirnames[:] = [d for d in dirnames if d != "analysis"]
                if not self.selection:
                    # <block>.prev-<UTC>: an earlier attempt that all.sh moved
                    # aside before re-running the block (interface b). With a
                    # selection the "*.prev-*" exclude rule handles them.
                    prev = [d for d in dirnames if ".prev-" in d]
                    if prev:
                        self.skipped_prev_dirs.extend(os.path.join(dirpath, d) for d in prev)
                        dirnames[:] = [d for d in dirnames if ".prev-" not in d]
                for fname in sorted(filenames):
                    if fname == "failed_blocks.tsv":
                        self._load_failed_blocks(os.path.join(dirpath, fname))
                        continue
                    # Large records (codegen reports) are committed xz-compressed
                    # as <name>.json.xz and read as <name>.json.
                    if fname.endswith(".json.xz"):
                        opener, fname = lzma.open, fname[:-len(".xz")]
                    elif fname.endswith(".json"):
                        opener = open
                    else:
                        continue
                    path = os.path.join(dirpath, fname if opener is open else fname + ".xz")
                    try:
                        with opener(path, "rt", encoding="utf-8") as f:
                            data = json.load(f)
                    except Exception:
                        continue
                    meta = self._locate(path, root, cache)
                    self._load_file(path, fname, data, meta)

        self._apply_supersede()
        self._check_partner_runs()
        for kind, item in self._pending:
            if kind == "codegen":
                self.codegen_reports.append(item)
            else:
                self._categorize(item)
        self._pending = []
        if self.superseded:
            blocks = {b for b, n in self.superseded.items() if n}
            self.pinning_errors = [e for e in self.pinning_errors
                                   if not any(e.startswith(b + ":") for b in blocks if self._fully_superseded(b))]

        if self.selection:
            for machine, run, sessions, entry in self.selection.listed_runs():
                for s in sessions:
                    if not os.path.isdir(os.path.join(self.selection.root, machine, run, s)):
                        status = entry.get("status", "")
                        print(f"[WARN] Selected session {machine}/{run}/{s} is not present"
                              + (f" ({status})" if status else ""))
            for machine, run, entry in self.selection.pending_run_globs():
                print(f"[INFO] Selected run pattern {machine}/{run} matches no run directory yet (inert until it "
                      f"lands): {entry.get('stage', '')}")
        for sess in sorted(self.missing_schedule):
            print(f"[WARN] Session {sess} has no schedule.json (still running or partially copied?); "
                  "its session id is taken from the directory name.")
        if self.uncovered:
            print("[FAIL] Records not covered by the selection (add their run dir to "
                  f"{self.selection.path if self.selection else 'selection.json'} or exclude them with a reason):")
            for item in sorted(set(self.uncovered))[:50]:
                print(f"  - {item}")
            if len(set(self.uncovered)) > 50:
                print(f"  ... {len(set(self.uncovered)) - 50} more")
            raise SystemExit(2)

        print(f"[INFO] Loaded: {len(self.reports_this_work)} This Work cases, "
              f"{len(self.reports_lcpc_orig)} Original LCPC cases, "
              f"{len(self.reports_simd_spielman)} SIMD-Spielman cases, "
              f"{len(self.reports_polycommit)} PolyCommit cases, "
              f"{sum(len(v) for v in self.reports_steps.values())} step records in {len(self.reports_steps)} tags, "
              f"{len(self.reports_ligero_rs)} Ligero RS cases, {len(self.reports_arm)} Arm cases, "
              f"{len(self.field_benchmarks)} field benchmarks, {len(self.codegen_reports)} codegen reports; "
              f"{self.exact_duplicates_dropped} byte-identical duplicate records dropped.")
        for reason, count in sorted(self.excluded.items()):
            print(f"[INFO] Excluded {count} records: {reason}")
        if self.block_id_mismatches:
            print(f"[WARN] {len(self.block_id_mismatches)} records carry a block_id that differs from their block "
                  "directory (renamed block or tagged by a later block, audit data-12); the directory is used. "
                  "First: " + self.block_id_mismatches[0])
        if self.skipped_prev_dirs:
            print(f"[INFO] Skipped {len(self.skipped_prev_dirs)} '<block>.prev-<UTC>' directories (earlier attempts "
                  "moved aside by all.sh); first: " + self.skipped_prev_dirs[0])
        for block, count in sorted(self.superseded.items()):
            print(f"[INFO] Superseded {count} records of {block}"
                  + (" (whole block)" if self._fully_superseded(block) else ""))
        for msg in self.partner_run_mismatches:
            print(f"[WARN] Same-boot pair split across runs (a superseding re-run landed for one side only?): {msg}")
        for block, count in sorted(self.failed_block_records.items()):
            print(f"[WARN] {count} included records come from block {block}, which the session schedule or the "
                  "record's block_status marks as failed; check them or exclude the block in selection.json.")
        for block, count in sorted(self.interrupted_block_records.items()):
            print(f"[WARN] {count} included records come from block {block}, whose latest run was interrupted "
                  "(output moved aside or stage never finalized, records untagged): partial data; --check fails "
                  "until the block is re-run or excluded in selection.json.")
        # SIMD-Spielman's params.patch hash is recorded per block (simd_spielman.json
        # or the block manifest); attach it to the block's records so it is part of
        # the cell key and shown next to SIMD-Spielman values.
        for r in self.reports_simd_spielman + [u for u in self.unmeasured if u["_meta"]["impl"] == "simd"]:
            m = r["_meta"]
            patches = self.block_simd_patch.get((m["machine"], m["run"], m["session_dir"], m["block"]), set())
            if r.get("params_patch_sha256"):
                patches = {str(r["params_patch_sha256"])[:12]}
            m["simd_patch"] = "/".join(sorted(patches)) or None

    def _load_failed_blocks(self, path: str):
        """<session>/failed_blocks.tsv (block id <TAB> exit status) written by all.sh."""
        session_dir = os.path.dirname(os.path.abspath(path))
        run_dir = os.path.dirname(session_dir)
        key = (os.path.basename(os.path.dirname(run_dir)), os.path.basename(run_dir), os.path.basename(session_dir))
        try:
            with open(path, "r", encoding="utf-8") as f:
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if parts and parts[0].strip():
                        self.failed_blocks.add(key + (parts[0].strip(),))
        except OSError:
            pass

    _FAILED_STATUSES = ("failed", "fail", "error")

    def _block_failed(self, m: Dict, item: Dict) -> Optional[str]:
        """None (ok), "failed" or "interrupted" for the block run that produced a record.

        all.sh tags every record of a block it completes, ok or failed, with
        block_status (interface b), so a record's own block_status is
        authoritative. Untagged records come from a run that never completed;
        in a session with the stage layout the session schedule decides: the
        latest stage that lists the block either finished it (ok/failed) or
        did not, and a block whose output was moved aside
        (moved_previous_to) or whose stage was never finalized without a final
        status is an interrupted re-run whose directory holds partial data.
        Older sessions without a session schedule use failed_blocks.tsv.
        """
        if item.get("block_status"):
            status = str(item["block_status"]).lower()
            if status == "interrupted":
                return "interrupted"
            return "failed" if status in self._FAILED_STATUSES else None
        key = (m["machine"], m["run"], m["session_dir"], m["block"])
        if not self.session_stages.get(key[:3]):
            return "failed" if key in self.failed_blocks else None
        # Walk the stages in order; the latest stage that ran the block (or
        # left a run of it unfinished) decides.
        verdict: Optional[str] = None
        completed = False      # some stage so far finished a run of the block
        for stage in self.session_stages.get(key[:3], []):
            finalized = str(stage.get("state") or "").lower() == "finished" or (
                stage.get("exit_status") is not None and not stage.get("state"))
            for b in stage.get("blocks") or []:
                if not isinstance(b, dict) or (b.get("id") or b.get("block_id")) != m["block"]:
                    continue
                status = str(b.get("status") or "").lower()
                if status not in ("", "scheduled", "not_run", "skipped"):
                    completed = True
                    verdict = ("interrupted" if status == "interrupted" else
                               "failed" if status in self._FAILED_STATUSES else None)
                elif b.get("moved_previous_to"):
                    verdict = "interrupted"     # old output moved aside, re-run never completed
                elif status == "scheduled" and not finalized:
                    verdict = "interrupted"     # stage killed before all.sh could finalize it
                elif status in ("scheduled", "not_run") and not completed:
                    # Not completed in this stage and never before: untagged
                    # records in its directory are output of a killed first run.
                    verdict = "interrupted"
            if m["block"] in (stage.get("failed_blocks") or []):
                verdict = "failed"
        return verdict

    def _classify(self, meta: Dict, fields: Optional[Dict]) -> str:
        if not self.selection:
            return "include"
        status, reason = self.selection.classify(meta["block_rel"], meta["file_rel"], fields)
        if status == "exclude":
            self.excluded[reason] += 1
        elif status == "uncovered":
            self.uncovered.append(meta["block_rel"] or meta["file_rel"])
        return status

    @staticmethod
    def _supersede_key(kind: str, item: Dict) -> Tuple:
        """The cell a record measures for the supersede rules (codegen: per machine and block family)."""
        m = item["_meta"]
        if kind == "codegen":
            return ("codegen", m["machine"], m["family"])
        if m["impl"] == "steps":
            return ("steps", m["machine"], str(item.get("tag")), str(item.get("mode", "a")), item.get("field"),
                    item.get("params"), int(item.get("log_n", 0)), int(item.get("threads", 1)))
        explicit = item.get("slice_mode") == "explicit"
        return (m["impl"], m["machine"], m["family"], m.get("variant"), item.get("field"), item.get("params"),
                item.get("shape") or "brakedown", item.get("slice_mode"), item.get("slices") if explicit else None,
                int(item.get("log_n", 0)), int(item.get("threads", 1)))

    def _apply_supersede(self):
        """selection.json "supersede": newer runs replace every older record of the cells they re-measured."""
        self._block_included: Dict[str, int] = defaultdict(int)
        for _, item in self._pending:
            self._block_included[item["_meta"]["block_rel"]] += 1
        if not self.selection or not self.selection.supersede:
            return
        drop: Set[int] = set()
        for rule in self.selection.supersede:
            def in_blocks(item):
                return any(fnmatch.fnmatchcase(item["_meta"]["block_rel"], p) for p in rule["blocks"])

            def in_by(item):
                mr = f"{item['_meta']['machine']}/{item['_meta']['run']}"
                return any(fnmatch.fnmatchcase(mr, p) for p in rule["by"])

            newer = {self._supersede_key(k, it) for k, it in self._pending if in_blocks(it) and in_by(it)}
            if not newer:
                continue      # inert until a superseding run has landed
            for i, (k, it) in enumerate(self._pending):
                if i in drop or in_by(it) or not in_blocks(it):
                    continue
                if self._supersede_key(k, it) in newer:
                    drop.add(i)
                    self.excluded[f"superseded: {rule['reason']}"] += 1
                    self.superseded[it["_meta"]["block_rel"]] += 1
        self._pending = [p for i, p in enumerate(self._pending) if i not in drop]

    def _check_partner_runs(self):
        """PARTNER_FAMILIES: both sides of a (machine, session, log2 N, T) pair should come from one run."""
        runs: Dict[Tuple, Dict[str, Set[str]]] = defaultdict(lambda: defaultdict(set))
        for kind, it in self._pending:
            if kind != "record":
                continue
            m = it["_meta"]
            for pair in PARTNER_FAMILIES:
                if m.get("family") in pair:
                    key = (pair, m["machine"], m["session"], int(it.get("log_n", 0)), int(it.get("threads", 1)))
                    runs[key][m["family"]].add(m["run"])
        for key, sides in sorted(runs.items(), key=lambda kv: str(kv[0])):
            pair, machine, session, log_n, threads = key
            if len(sides) == 2 and sides[pair[0]] != sides[pair[1]]:
                self.partner_run_mismatches.append(
                    f"{machine} {session} 2^{log_n} T={threads}: {pair[0]} from {'/'.join(sorted(sides[pair[0]]))}, "
                    f"{pair[1]} from {'/'.join(sorted(sides[pair[1]]))}")

    def _fully_superseded(self, block_rel: str) -> bool:
        n = self.superseded.get(block_rel, 0)
        return n > 0 and n >= getattr(self, "_block_included", {}).get(block_rel, 0)

    def _load_file(self, path: str, fname: str, data: Any, meta: Dict):
        is_session_level = not meta["block"]

        if isinstance(data, dict) and fname == "schedule.json":
            if self.selection and self._classify(meta, None) != "include":
                return
            if meta.get("stage_meta"):
                return   # per-stage copy; the session-level schedule.json accumulates all stages
            if isinstance(data.get("stages"), list):
                self.session_stages[(meta["machine"], meta["run"], meta["session_dir"])] = [
                    s for s in data["stages"] if isinstance(s, dict)]
            profile = data.get("profile")
            if profile:
                self.profiles.add(str(profile))
                self.profile = self.profile or str(profile)
            if profile == "smoke":
                self.has_smoke_watermark = True
            if data.get("session_id"):
                self.session_ids.add(str(data["session_id"]))
            return

        if isinstance(data, dict) and ("manifest" in fname):
            if is_session_level or (self.selection and self.selection.classify(meta["block_rel"], meta["file_rel"], None)[0] != "include"):
                # Session-level manifests are overwritten by every stage (audit
                # data-11); provenance comes from the per-block manifests.
                return
            self._load_manifest(path, data, meta)
            return

        if fname == "simd_spielman.json" and isinstance(data, dict):
            # Only blocks the selection includes contribute (audit review 5).
            if self.selection and self.selection.classify(meta["block_rel"], meta["file_rel"], None)[0] != "include":
                return
            if data.get("status") == "skipped":
                self.skipped_baselines["SIMD-Spielman"] = str(data.get("skip_reason") or "prerequisite unavailable")
            if data.get("params_patch_sha256"):
                self.block_simd_patch[(meta["machine"], meta["run"], meta["session_dir"], meta["block"])].add(
                    str(data["params_patch_sha256"])[:12])
            return
        if fname == "polycommit_skipped.json" and isinstance(data, dict):
            if data.get("status") == "skipped":
                self.skipped_baselines["PolyCommit"] = str(data.get("skip_reason") or "prerequisite unavailable")
            return

        if isinstance(data, dict) and ("codegen" in fname or "hot_kernels_checked" in data):
            if self._classify(meta, data) != "include":
                return
            data = dict(data)
            data["_meta"] = self._record_meta(meta, data, path, impl="codegen")
            if "/steps/" in meta["file_rel"] or block_family(meta["block"]) == "steps":
                # Per-tag step reports: not T11 evidence (T11 reports the measured
                # HEAD binaries) but part of the FAIL gate, with T3 footnotes.
                parts = meta["file_rel"].split("/")
                tag = parts[parts.index("steps") + 1] if "steps" in parts[:-1] else "unknown_step"
                if tag != "HEAD" and not tag.startswith("step/"):
                    tag = "step/" + tag.removeprefix("step_")
                data["_meta"]["tag"] = tag
                self.step_codegen_reports.append(data)
                return
            self._pending.append(("codegen", data))
            return

        if isinstance(data, list) and data and isinstance(data[0], dict) and "mul" in data[0] and "mac" in data[0]:
            for item in data:
                if isinstance(item, dict) and self._classify(meta, item) == "include":
                    item = dict(item)
                    item["_meta"] = self._record_meta(meta, item, path, impl="microbench")
                    self.field_benchmarks.append(item)
            return

        items = data if isinstance(data, list) else [data]
        for item in items:
            if not isinstance(item, dict):
                continue
            schema = item.get("schema_version")
            if schema not in ("lcpc-bench-v1", "steps-bench-v1"):
                continue
            if item.get("watermark") or item.get("run_class") == "smoke":
                self.has_smoke_watermark = True
            impl = self._impl_of(item, meta)
            rmeta = self._record_meta(meta, item, path, impl=impl)
            fields = dict(item)
            fields["build_variant"] = rmeta.get("variant")
            fields["build_variant_recorded"] = rmeta.get("variant_recorded")
            fields["family"] = rmeta.get("family")
            if self._classify(meta, fields) != "include":
                continue
            item = dict(item)
            item["_meta"] = rmeta
            self._pending.append(("record", item))

    def _load_manifest(self, path: str, data: Dict, meta: Dict):
        key = (meta["machine"], meta["run"], meta["session_dir"])
        info = self.session_info[key]
        git = data.get("git") if isinstance(data.get("git"), dict) else {}
        if git.get("code_repo_sha"):
            info["fork"].add(str(git["code_repo_sha"])[:7] + ("+dirty" if git.get("code_repo_dirty") else ""))
        elif data.get("source_checkout_head"):
            info["fork"].add(str(data["source_checkout_head"])[:7])
        if git.get("simd_params_patch_sha256") and block_family(meta["block"]) == "simd":
            self.block_simd_patch[(meta["machine"], meta["run"], meta["session_dir"], meta["block"])].add(
                str(git["simd_params_patch_sha256"])[:12])
        ident = data.get("machine_identity") if isinstance(data.get("machine_identity"), dict) else {}
        if ident.get("slurm_job_id"):
            info["zone"].add("job " + str(ident["slurm_job_id"]))
        if ident.get("hostname"):
            info["vm"].add(str(ident["hostname"]))
        node = ((data.get("machine") or {}).get("uname") or {}).get("node") if isinstance(data.get("machine"), dict) else None
        if node:
            info["vm"].add(str(node))
        host = data.get("host_profile") or data.get("host")
        if host:
            self.hosts.add(str(host))
        if data.get("watermark") or data.get("run_class") == "smoke":
            self.has_smoke_watermark = True
        # A run whose threads outnumber its pinned cores measures
        # oversubscription, not scaling. SIMD-Spielman's MT encoder adds a
        # spin-waiting coordinator thread: count it unless the block records
        # that it deliberately shares a worker core (8 workers on 8 cores).
        pin = data.get("pinning")
        if isinstance(pin, dict) and isinstance(pin.get("affinity_cores_expanded"), list):
            threads = int(pin.get("threads") or 1)
            cores = len(pin["affinity_cores_expanded"])
            extra = data.get("extra") if isinstance(data.get("extra"), dict) else {}
            is_simd = block_family(meta["block"]) == "simd" or "coordinator_shares_worker_core" in extra
            effective = threads + 1 if (is_simd and threads > 1) else threads
            if effective > cores and not (is_simd and extra.get("coordinator_shares_worker_core") is True):
                self.pinning_errors.append(
                    f"{meta['block_rel']}: {threads} threads"
                    + (" + coordinator" if effective > threads else "")
                    + f" pinned to {cores} cores ({pin.get('cores')})")

    def _impl_of(self, item: Dict, meta: Dict) -> str:
        if item.get("schema_version") == "steps-bench-v1":
            return "steps"
        impl = item.get("implementation", "")
        norm = meta["file_rel"]
        if impl == "lcpc-ligero-rs" or item.get("params") in ("ligero-rs", "rs-ligero") or item.get("field") == "Mont127-RS":
            return "ligero"
        if impl == "lcpc-original-upstream" or "/lcpc_orig/" in norm:
            return "lcpc_orig"
        if impl == "simd-spielman" or "/simd_spielman" in norm:
            return "simd"
        if impl in ("polycommit", "arkworks-poly-commit-brakedown") or "/polycommit/" in norm:
            return "polycommit"
        return "this_work"

    def _record_meta(self, meta: Dict, item: Dict, path: str, impl: str) -> Dict:
        m = dict(meta)
        m["path"] = path
        m["impl"] = impl
        family = block_family(meta["block"])
        if family is None:
            family = self._infer_family(impl, item)
        m["family"] = family
        m["arch"] = ("aarch64" if (str(family).startswith("arm_") or item.get("arch") == "aarch64"
                                   or "xcng" in meta["machine"]) else "x86_64")
        if impl == "lcpc_orig":
            variant, recorded = infer_build_variant(item, path)
            m["variant"], m["variant_recorded"] = variant, recorded
        else:
            m["variant"], m["variant_recorded"] = None, None
        if not m["session"]:
            m["session"] = str(item.get("session_id") or "local")
        rec_block = item.get("block_id")
        if rec_block and meta["block"] and not str(meta["block"]).startswith(str(rec_block)):
            self.block_id_mismatches.append(f"{meta['file_rel']}: block_id={rec_block}")
        return m

    @staticmethod
    def _infer_family(impl: str, item: Dict) -> str:
        """Family for records outside a block directory (legacy/local layouts)."""
        if impl == "this_work":
            if item.get("params") == "simd-spielman":
                return "tw_b"
            return "slice_sweep" if item.get("slice_mode") == "explicit" else "tw_a"
        return {"lcpc_orig": "lcpc_a", "simd": "simd", "polycommit": "polycommit",
                "ligero": "ligero", "steps": "steps"}.get(impl, "unknown")

    def _cell_key(self, r: Dict) -> Tuple:
        """The configuration a record measures (everything except its session)."""
        m = r["_meta"]
        if m["impl"] == "steps":
            return ("steps", m["machine"], str(r.get("tag")), str(r.get("mode", "a")), r.get("field"),
                    r.get("params"), int(r.get("log_n", 0)), int(r.get("threads", 1)))
        return (m["impl"], m["machine"], m["family"], m.get("variant"), m.get("simd_patch"), r.get("field"),
                r.get("params"), r.get("shape") or "brakedown", r.get("slice_mode"), r.get("slices"),
                int(r.get("log_n", 0)), int(r.get("threads", 1)))

    def _categorize(self, item: Dict):
        m = item["_meta"]
        status = item.get("status")
        samples = tuple(self._extract_samples(item))
        dedup_key = (m["session"], m["run"], m["block"], self._cell_key(item), samples)
        if samples and dedup_key in self._seen_keys:
            self.exact_duplicates_dropped += 1
            return
        self._seen_keys.add(dedup_key)
        if m["session"]:
            self.session_ids.add(m["session"])
        verdict = self._block_failed(m, item) if m["block"] else None
        if verdict == "interrupted":
            self.interrupted_block_records[m["block_rel"]] += 1
        elif verdict == "failed":
            self.failed_block_records[m["block_rel"]] += 1

        if m["impl"] == "steps":
            tag = str(item.get("tag") or "unknown_step")
            if tag != "HEAD" and not tag.startswith("step/"):
                tag = "step/" + tag.removeprefix("step_")
            self.reports_steps[tag].append(item)
            return
        if (status is not None and str(status).lower() not in MEASURED_STATUSES) or not samples:
            self.unmeasured.append(item)
            return
        if m["arch"] == "aarch64":
            self.reports_arm.append(item)
        elif m["impl"] == "ligero":
            self.reports_ligero_rs.append(item)
        elif m["impl"] == "lcpc_orig":
            self.reports_lcpc_orig.append(item)
        elif m["impl"] == "simd":
            self.reports_simd_spielman.append(item)
        elif m["impl"] == "polycommit":
            self.reports_polycommit.append(item)
        else:
            self.reports_this_work.append(item)

    # ------------------------------------------------------------------
    # Statistics engine (06 §3)
    # ------------------------------------------------------------------

    def _extract_samples(self, report: Dict, op_name: str = "encode_matrix") -> List[float]:
        if report.get("schema_version") == "steps-bench-v1":
            samples = [safe_float(s) for s in report.get("samples_ns") or []]
            samples = [s for s in samples if s is not None and s > 0]
            if samples:
                return samples
            med = safe_float(report.get("median_ns"))
            return [med] if med else []
        meas = (report.get("measurements") or {}).get(op_name) or {}
        samples = [safe_float(s) for s in meas.get("samples_ns") or []]
        samples = [s for s in samples if s is not None and s > 0]
        if not samples:
            med = safe_float((meas.get("summary") or {}).get("median_ns"))
            if med and med > 0:
                samples = [med]
        return samples

    def select(self, pool: Iterable[Dict], **flt) -> List[Dict]:
        """Filters records. Keys: impl, family, variant, machine, arch (meta) or record fields."""
        meta_keys = {"impl", "family", "variant", "machine", "arch", "session"}
        out = []
        for r in pool:
            ok = True
            for k, v in flt.items():
                if v is None:
                    continue
                actual = r["_meta"].get(k) if k in meta_keys else r.get(k)
                if k == "shape" and actual is None:
                    actual = "brakedown"
                if k in ("log_n", "threads", "slices") and actual is not None:
                    actual = int(actual)
                if isinstance(v, (set, frozenset, list, tuple)):
                    if actual not in v:
                        ok = False
                        break
                elif actual != v:
                    ok = False
                    break
            if ok:
                out.append(r)
        return out

    def stat(self, records: List[Dict], op: str = "encode_matrix", label: str = "") -> Dict[str, Any]:
        """Per-session median, mean of session medians, min-max over all samples."""
        empty = {"mean": None, "min": None, "max": None, "sessions": [], "session_medians": {},
                 "session_samples": {}, "diverged": False, "rel_diff": None, "records": [], "machine": None}
        with_samples = [r for r in records if self._extract_samples(r, op)]
        if not with_samples:
            return empty
        keys = {self._cell_key(r) for r in with_samples}
        if len(keys) > 1:
            self.cell_errors.append((label or "?", f"heterogeneous cell pools {len(keys)} configurations: "
                                     + "; ".join(str(k) for k in sorted(keys, key=str)[:3])))
        by_session: Dict[str, List[Dict]] = defaultdict(list)
        for r in sorted(with_samples, key=lambda x: x["_meta"]["path"]):
            by_session[r["_meta"]["session"]].append(r)
        session_medians, session_samples, used = {}, {}, []
        for sid, recs in sorted(by_session.items()):
            if len(recs) > 1:
                self.cell_errors.append((label or "?", f"duplicate: session {sid} has {len(recs)} records ("
                                         + ", ".join(r["_meta"]["block_rel"] for r in recs) + ")"))
            chosen = recs[0]
            samples = self._extract_samples(chosen, op)
            session_samples[sid] = samples
            session_medians[sid] = median(samples)
            used.append(chosen)
        meds = list(session_medians.values())
        rel = (max(meds) - min(meds)) / min(meds) if len(meds) >= 2 and min(meds) > 0 else None
        all_samples = [s for ss in session_samples.values() for s in ss]
        for r in used:
            m = r["_meta"]
            self._used.add((m["machine"], m["run"], m["session_dir"]))
            self._used_records.append(r)
        st = {"mean": sum(meds) / len(meds), "min": min(all_samples), "max": max(all_samples),
              "sessions": sorted(session_medians), "session_medians": session_medians,
              "session_samples": session_samples, "diverged": rel is not None and rel > DIVERGENCE_THRESHOLD,
              "rel_diff": rel, "records": used, "machine": used[0]["_meta"]["machine"]}
        return st

    def scalar_stat(self, records: List[Dict], getter) -> Optional[Dict[str, Any]]:
        """Mean over sessions of a per-record scalar (setup time, RSS)."""
        per_session = {}
        for r in sorted(records, key=lambda x: x["_meta"]["path"]):
            v = getter(r)
            if v is not None and r["_meta"]["session"] not in per_session:
                per_session[r["_meta"]["session"]] = v
        if not per_session:
            return None
        vals = list(per_session.values())
        return {"mean": sum(vals) / len(vals), "min": min(vals), "max": max(vals), "sessions": sorted(per_session)}

    # ---- cell formatting -------------------------------------------------

    def _expected_sessions(self, st: Dict) -> Optional[List[str]]:
        if not self.selection or not st.get("machine"):
            return None
        return self.selection.expected_sessions(st["machine"])

    def missing_text(self, selector: str) -> str:
        if self.selection:
            om = self.selection.omission_for(selector)
            if om:
                self.todo_cells[selector] = f"{om['kind']}: {om['reason']}"
                return "TODO (pending)" if om["kind"] == "pending" else "omitted (documented)"
        return "-"

    def _session_marker(self, selector: str, sessions: List[str], expected: Optional[List[str]],
                        what: str = "") -> str:
        """' [s2 only]' when fewer sessions than expected contributed; registers the gap."""
        if not expected:
            return ""
        missing = [s for s in expected if s not in sessions]
        if not missing:
            return ""
        om = self.selection.omission_for(selector) if self.selection else None
        if om:
            self.todo_cells[selector] = f"{om['kind']}: {om['reason']} (missing {', '.join(missing)}{what})"
        else:
            self.missing_cell_errors.append((selector, f"missing sessions {missing} (have {sessions}){what}"))
        return f" [{'+'.join(sessions) or 'no session'} only]"

    def _markers(self, selector: str, st: Dict) -> str:
        text = ""
        if st["diverged"]:
            text += f" D{st['rel_diff'] * 100:.1f}%"
        return text + self._session_marker(selector, st["sessions"], self._expected_sessions(st))

    def derived_marker(self, selector: str, *stats: Optional[Dict]) -> str:
        """Session marker for a cell derived from several cells (ratio, best-of-T, ...).

        The derived value is only two-session evidence if every input was
        measured in every expected session; otherwise the cell carries
        '[sX only]' with the sessions common to all inputs.
        """
        present = [s for s in stats if s and s.get("mean") is not None]
        if not present:
            return ""
        common = sorted(set.intersection(*(set(s["sessions"]) for s in present)))
        expected = None
        for s in present:
            expected = expected or self._expected_sessions(s)
        return self._session_marker(selector, common, expected, " in an input of this derived cell")

    def value_cell(self, table: str, row_key: str, column: str, st: Dict, unit: str = "ms",
                   with_range: bool = True) -> str:
        selector = f"{table}|{row_key}|{column}"
        if st is None or st.get("mean") is None:
            return self.missing_text(selector)
        self.cell_log[selector] = st
        text = format_ns(st["mean"], unit)
        if with_range:
            text += f" [{format_ns(st['min'], unit)}-{format_ns(st['max'], unit)}]"
        return text + self._markers(selector, st)

    def speedup_cell(self, table: str, row_key: str, column: str, base: Dict, tw: Dict) -> str:
        """Speedup = base / tw (ratio of mean session medians) with the per-session range."""
        selector = f"{table}|{row_key}|{column}"
        if not base or not tw or base.get("mean") is None or tw.get("mean") is None:
            return self.missing_text(selector)
        ratio = base["mean"] / tw["mean"]
        common = [s for s in tw["sessions"] if s in base["sessions"]]
        text = f"{ratio:.2f}x"
        if len(common) >= 2:
            per = [base["session_medians"][s] / tw["session_medians"][s] for s in common]
            text += f" ({min(per):.2f}-{max(per):.2f})"
        return text + self.derived_marker(selector, base, tw)

    def ratio_cell(self, table: str, row_key: str, column: str, num: Optional[Dict], den: Optional[Dict],
                   fmt: str = "{:.2f}x") -> str:
        """num/den of two cells' means (no per-session range), with the derived session marker."""
        selector = f"{table}|{row_key}|{column}"
        if not num or not den or not num.get("mean") or not den.get("mean"):
            return self.missing_text(selector)
        return fmt.format(num["mean"] / den["mean"]) + self.derived_marker(selector, num, den)

    def best_value_cell(self, table: str, row_key: str, column: str, best: Optional[Dict], suffix: str = "") -> str:
        """A value picked from several cells (best-of-T): its mean plus session markers."""
        selector = f"{table}|{row_key}|{column}"
        if not best or best.get("mean") is None:
            return self.missing_text(selector)
        self.cell_log[selector] = best
        return format_ns(best["mean"]) + suffix + self._markers(selector, best)

    def significance(self, tw: Dict, base: Dict, selector: str = "") -> str:
        """'faster' only with non-overlapping ranges in every common session."""
        if not tw or not base or tw.get("mean") is None or base.get("mean") is None:
            return self.missing_text(selector) if selector else "-"
        common = [s for s in tw["sessions"] if s in base["sessions"]]
        pairs = ([(tw["session_samples"][s], base["session_samples"][s]) for s in common] if common else
                 [([x for v in tw["session_samples"].values() for x in v],
                   [x for v in base["session_samples"].values() for x in v])])
        mark = self.derived_marker(selector, tw, base) if selector else ""
        if all(max(t) < min(b) for t, b in pairs):
            return "faster" + mark
        if all(min(t) > max(b) for t, b in pairs):
            return "slower" + mark
        return "comparable" + mark

    @staticmethod
    def _dims(r: Dict) -> str:
        d = r.get("dimensions") or {}
        return f"{d.get('n_rows')}x{d.get('n_cols')}" if isinstance(d, dict) and d.get("n_rows") else "?"

    def ns_per_mac_cell(self, table: str, row_key: str, column: str, st: Dict) -> str:
        selector = f"{table}|{row_key}|{column}"
        macs = {r.get("total_edge_row_macs") for r in st.get("records") or []}
        if st.get("mean") and len(macs) == 1 and (next(iter(macs)) or 0) > 0:
            return f"{st['mean'] / next(iter(macs)):.2f}" + self.derived_marker(selector, st)
        return self.missing_text(selector)

    @staticmethod
    def rustflags_notes(records: List[Dict]) -> List[str]:
        """Original-LCPC build flags as recorded (effective_rustflags), per build variant."""
        by_variant: Dict[str, Set[str]] = defaultdict(set)
        for r in records:
            if r["_meta"]["impl"] != "lcpc_orig":
                continue
            flags = r.get("effective_rustflags")
            by_variant[r["_meta"].get("variant") or "?"].add(
                "not recorded" if flags is None else (f"'{flags}'" if flags else "'' (empty: Cargo defaults)"))
        if not by_variant:
            return []
        return ["Original LCPC effective RUSTFLAGS (record field effective_rustflags): "
                + "; ".join(f"{v}: {' / '.join(sorted(f))}" for v, f in sorted(by_variant.items())) + "."]

    def arm_flags_notes(self, records: List[Dict]) -> List[str]:
        """Arm build-flag caveats derived from the recorded effective_rustflags of the original-LCPC records."""
        lcpc = [r for r in records if r["_meta"]["impl"] == "lcpc_orig"]
        native = sorted({r["_meta"].get("variant") or "?" for r in lcpc
                         if "target-cpu=native" in str(r.get("effective_rustflags") or "")})
        unrecorded = [r for r in lcpc if r.get("effective_rustflags") is None]
        notes = []
        if native:
            notes.append("Build flags differ across rows on Arm: the recorded flags of original LCPC "
                         + ", ".join(native) + " include target-cpu=native, while the fork's .cargo/config.toml sets "
                         "target-cpu=native for x86-64 targets only, so This Work's Arm build sets no target-cpu.")
        if unrecorded:
            forks = sorted({f for r in unrecorded for f in self.session_info.get(
                (r["_meta"]["machine"], r["_meta"]["run"], r["_meta"]["session_dir"]), {}).get("fork", set())})
            notes.append(f"{len(unrecorded)} original-LCPC Arm records do not record their build flags "
                         f"(harness at fork {'/'.join(forks) or '?'}); their flags are those of that commit's "
                         "reproduce/lcpc_orig.sh and cannot be confirmed from the records.")
        return notes

    def kdhr26_entry(self, table: str, log_n: int, machine: Optional[str]) -> Tuple[Optional[Dict], str]:
        """[KDHR26] entry keyed by (reference CPU for the machine, log2 N)."""
        cpu = (self.selection.kdhr26_cpu(machine) if (self.selection and machine) else None) or "EPYC 9754"
        for entry in self.kdhr26_data.get(table, {}).get("entries", []):
            if entry.get("log_n") == log_n and entry.get("cpu", cpu) == cpu:
                return entry, cpu
        return None, cpu

    # ------------------------------------------------------------------
    # Provenance
    # ------------------------------------------------------------------

    def _get_provenance_text(self) -> str:
        used = sorted(self._used)
        parts = []
        for machine, run, sess in used:
            info = self.session_info.get((machine, run, sess), {})
            desc = []
            if info.get("fork"):
                desc.append("fork " + "/".join(sorted(info["fork"])))
            if info.get("zone"):
                desc.append("/".join(sorted(info["zone"])))
            if info.get("vm"):
                desc.append("node " + "/".join(sorted(info["vm"])))
            label = "/".join(p for p in (machine, run, sess) if p) or "local"
            parts.append(label + (f" ({', '.join(desc)})" if desc else ""))
        recs = self._used_records
        lcpc = sorted({str(r.get("upstream_commit"))[:7] for r in recs if r["_meta"]["impl"] == "lcpc_orig" and r.get("upstream_commit")})
        simd = sorted({str(r.get("upstream_commit"))[:7] for r in recs if r["_meta"]["impl"] == "simd" and r.get("upstream_commit")})
        simd_patch = sorted({str(r["_meta"].get("simd_patch")) for r in recs
                             if r["_meta"]["impl"] == "simd" and r["_meta"].get("simd_patch")})
        poly = sorted({f"{str(r.get('upstream_commit'))[:7]} patch {str(r.get('patch_sha256'))[:12]}"
                       for r in recs if r["_meta"]["impl"] == "polycommit"})
        base = []
        if lcpc:
            base.append("LCPC " + "/".join(lcpc))
        if simd:
            base.append("SIMD-Spielman " + "/".join(simd) + (" params.patch " + "/".join(simd_patch) if simd_patch else ""))
        if poly:
            base.append("PolyCommit " + "/".join(poly))
        sel = (f"selection {os.path.basename(self.selection.path)} sha256 {self.selection.sha256[:12]}"
               if self.selection else "UNSELECTED (all records under " + ", ".join(self.result_dirs) + "; not paper evidence)")
        watermark = " [LOCAL SMOKE - NOT A RESULT]" if (self.is_smoke or self.has_smoke_watermark) else ""
        data = "; ".join(parts) if parts else "no measured records"
        return (f"Provenance: {sel}; data: {data}"
                + (f"; baselines: {'; '.join(base)}" if base else "") + f".{watermark}")

    # ------------------------------------------------------------------
    # Writers
    # ------------------------------------------------------------------

    def _begin(self, table: str):
        self._current_table = table
        self._used = set()
        self._used_records = []

    def _write_artefact(self, name: str, caption: str, label: str, headers: List[str],
                        rows: List[List[Any]], notes: Optional[List[str]] = None,
                        col_align: Optional[str] = None, key_cols: int = 1):
        """Writes a LaTeX booktabs table and a CSV with provenance footers."""
        provenance = self._get_provenance_text()
        notes = list(notes or [])
        if any("D" in str(c) and re.search(r" D\d+\.\d%", str(c)) for row in rows for c in row):
            notes.append("D<x>%: the two session medians differ by more than 5%; see csv/session_divergence.csv.")
        if any(re.search(r"\[[^\]]+ only\]", str(c)) for row in rows for c in row):
            notes.append("[sX only]: measured in fewer sessions than planned; see csv/todo_cells.csv.")
        if (self.is_smoke or self.has_smoke_watermark) and name.startswith("table_"):
            notes.append("LOCAL SMOKE - NOT A RESULT. Do not copy these values to paper/.")

        csv_path = os.path.join(self.csv_dir, f"{name}.csv")
        with open(csv_path, "w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            writer.writerow([f"# {caption}"])
            writer.writerow([f"# {provenance}"])
            writer.writerow([f"# Key columns: {key_cols}"])
            for note in notes:
                writer.writerow([f"# Note: {note}"])
            writer.writerow(headers)
            for row in rows:
                writer.writerow(row)
        print(f"[INFO] Wrote CSV: {csv_path}")

        def tex(cell: Any) -> str:
            text = escape_latex(str(cell))
            # Session-divergence marker " D6.1%" -> " $\Delta$6.1\%".
            return re.sub(r" D(\d+\.\d\\%)", r" $\\Delta$\1", text, count=1)

        tex_path = os.path.join(self.latex_dir, f"{name}.tex")
        col_spec = col_align or ("l" + "r" * (len(headers) - 1))
        table_env = "table*" if len(headers) > 6 else "table"
        with open(tex_path, "w", encoding="utf-8") as f:
            f.write("% Auto-generated by analyze.py. Do not edit by hand.\n")
            f.write("% Needs \\usepackage{booktabs,threeparttable}.\n")
            f.write(f"\\begin{{{table_env}}}[t]\n\\centering\n")
            f.write(f"\\caption{{{escape_latex(caption)}}}\n\\label{{{label}}}\n")
            f.write("\\begin{threeparttable}\n")
            f.write(f"\\begin{{tabular}}{{{col_spec}}}\n\\toprule\n")
            f.write(" & ".join(escape_latex(h) for h in headers) + " \\\\\n\\midrule\n")
            for row in rows:
                f.write(" & ".join(tex(cell) for cell in row) + " \\\\\n")
            f.write("\\bottomrule\n\\end{tabular}\n\\begin{tablenotes}\\footnotesize\n")
            for note in notes:
                f.write(f"\\item {escape_latex(note)}\n")
            f.write(f"\\item \\textbf{{{escape_latex(provenance)}}}\n")
            f.write("\\end{tablenotes}\n\\end{threeparttable}\n")
            f.write(f"\\end{{{table_env}}}\n")
        print(f"[INFO] Wrote LaTeX: {tex_path}")

    def _write_plain_csv(self, name: str, headers: List[str], rows: List[List[Any]], comment: str = ""):
        path = os.path.join(self.csv_dir, f"{name}.csv")
        with open(path, "w", newline="", encoding="utf-8") as f:
            writer = csv.writer(f)
            if comment:
                writer.writerow([f"# {comment}"])
            writer.writerow(headers)
            writer.writerows(rows)
        print(f"[INFO] Wrote CSV: {path}")

    # ------------------------------------------------------------------
    # Artefacts
    # ------------------------------------------------------------------

    def generate_all(self):
        """Generates T0-T12 and F1-F3, then the per-cell statistics reports."""
        for gen in (self.generate_T0_correctness, self.generate_T1_headline_encoding,
                    self.generate_T2_simd_spielman, self.generate_T3_steps_breakdown,
                    self.generate_T4_slice_sweep, self.generate_T5_field_comparison,
                    self.generate_T6_pcs_breakdown, self.generate_T7_verifier_encoding,
                    self.generate_T8_setup_memory, self.generate_T9_orion_shape,
                    self.generate_T10_brakedown_vs_rs, self.generate_T11_codegen_evidence,
                    self.generate_T12_arm_portability):
            gen()
        if HAS_MATPLOTLIB:
            self.generate_F1_steps_breakdown()
            self.generate_F2_llc_scaling()
            self.generate_F3_thread_scaling()
        else:
            print("[WARN] Matplotlib not installed; skipping F1-F3 figure generation.")
        self.write_cell_reports()

    def write_cell_reports(self):
        rows, div_rows = [], []
        for label, st in sorted(self.cell_log.items()):
            if st.get("mean") is None:
                continue
            meds = "; ".join(f"{s}={format_ns(st['session_medians'][s], 'ms', 3)}" for s in st["sessions"])
            rel = f"{st['rel_diff'] * 100:.1f}" if st["rel_diff"] is not None else ""
            blocks = "; ".join(sorted({r["_meta"]["block_rel"] for r in st["records"]}))
            row = [label, len(st["sessions"]), meds, format_ns(st["mean"], "ms", 3),
                   format_ns(st["min"], "ms", 3), format_ns(st["max"], "ms", 3), rel, blocks]
            rows.append(row)
            if st["diverged"]:
                div_rows.append(row)
        headers = ["Cell", "Sessions", "Session medians (ms)", "Mean of medians (ms)", "Min (ms)",
                   "Max (ms)", "Session rel. diff (%)", "Blocks"]
        self._write_plain_csv("cell_statistics", headers, rows,
                              "Every table cell: per-session medians, mean of session medians, min-max over samples.")
        self._write_plain_csv("session_divergence", headers, div_rows,
                              "Cells whose session medians differ by more than 5% (flagged for a re-run).")
        self._write_plain_csv("todo_cells", ["Cell", "Status"], sorted(self.todo_cells.items()),
                              "Cells that are pending re-measurement or documented omissions (selection.json).")

    # ---- T0 ---------------------------------------------------------------
    def generate_T0_correctness(self):
        self._begin("T0")
        headers = ["Gate / Invariant", "Evaluation Target", "Oracle / Reference", "Commit / Tag", "Status"]
        # Test-gated rows: reproduce/this_work.sh (and steps.sh per tag) runs
        # `cargo test --workspace` before timing and refuses to benchmark on a
        # failure, so every timed This Work block / step tag in the selection is
        # evidence that the suite passed for that build. Derived from the data.
        tw_recs = self.reports_this_work + [r for r in self.reports_arm if r["_meta"]["impl"] == "this_work"]
        tw_blocks = {(r["_meta"]["machine"], r["_meta"]["run"], r["_meta"]["session_dir"], r["_meta"]["block"])
                     for r in tw_recs}
        forks = sorted({f for (mc, rn, sd, _) in tw_blocks
                        for f in self.session_info.get((mc, rn, sd), {}).get("fork", set())})
        step_tags = sorted(t for t, recs in self.reports_steps.items() if any(self._extract_samples(r) for r in recs))
        for r in tw_recs:
            m = r["_meta"]
            self._used.add((m["machine"], m["run"], m["session_dir"]))

        def gate_status(row_key: str, tags_from: Optional[str]) -> str:
            tags = [t for t in step_tags if tags_from is None or t == "HEAD" or t >= tags_from]
            if not tw_blocks:
                return self.missing_text(f"T0|{row_key}|Status")
            return (f"PASS (test gate before {len(tw_blocks)} timed blocks @ {'/'.join(forks) or '?'}"
                    + (f" and {len(tags)} step tags" if tags else "") + ")")

        def verification_status(row_key: str, recs: List[Dict]) -> str:
            flags = [((r.get("correctness") or {}).get("proof_verification_passed"),
                      (r.get("correctness") or {}).get("direct_encoder_root_matches_commit")) for r in recs]
            flags = [f for f in flags if f != (None, None)]
            if not flags:
                return self.missing_text(f"T0|{row_key}|Status")
            bad = sum(1 for p, d in flags if p is not True or d is not True)
            return (f"FAIL ({bad} of {len(flags)} records)" if bad else
                    f"PASS ({len(flags)} records: proof verifies, direct encoding matches the commitment)")

        gated = [
            ("Bit-exact prover encoding (mode a)", "SdigCode6 (Mont127)", "Original LCPC (f0e2116)", "step/01..HEAD", "step/01"),
            ("Bit-exact encoding (mode b)", "Mont127, Mers127, Fp2M61", "SIMD-Spielman tv/ files", "step/02..HEAD", "step/02"),
            ("Horizontal slice invariance", "s in {0 (adaptive), 2, 4, 8, 16}", "s = 1 non-sliced baseline", "step/07..HEAD", "step/07"),
            ("Expander thread invariance", "T in {1, 2, 4, 7, 8} x s in {1, 4}", "T = 1 single-threaded baseline", "step/08..HEAD", "step/08"),
            ("Field differential tests", "Mont127, Mers127, Fp2M61", "num-bigint big-integer oracle", "step/02..HEAD", "step/02"),
        ]
        rows = [[name, target, oracle, scope, gate_status(name, since)] for name, target, oracle, scope, since in gated]
        lcpc_recs = self.reports_lcpc_orig + [r for r in self.reports_arm if r["_meta"]["impl"] == "lcpc_orig"]
        rows.append(["PCS end-to-end verification", "Commitment & proof verification (This Work records)",
                     "Upstream Ligero/Brakedown PCS", "@ " + ("/".join(forks) or "-"),
                     verification_status("PCS end-to-end verification", tw_recs)])
        rows.append(["Original LCPC PCS verification", "Commitment & proof verification (original LCPC records)",
                     "Its own verifier", "LCPC " + ("/".join(sorted({str(r.get("upstream_commit"))[:7] for r in lcpc_recs
                                                                       if r.get("upstream_commit")})) or "-"),
                     verification_status("Original LCPC PCS verification", lcpc_recs)])
        for r in lcpc_recs:
            m = r["_meta"]
            self._used.add((m["machine"], m["run"], m["session_dir"]))
        # Vector-instruction audit rows are derived from the selected codegen
        # reports (one per architecture), never hard-coded.
        by_arch: Dict[str, List[Dict]] = defaultdict(list)
        for g in self.codegen_groups():
            by_arch[g["arch"]].append(g)
            for r in g["reports"]:
                m = r["_meta"]
                self._used.add((m["machine"], m["run"], m["session_dir"]))
        for arch in sorted(by_arch) or ["(none)"]:
            gs = by_arch.get(arch, [])
            statuses = sorted({st for g in gs for st in g["effective"]})
            if not gs:
                status = self.missing_text("T0|Vector-instruction audit|Status")
            elif all(g["ok"] for g in gs):
                status = "PASS" if statuses == ["PASS"] else "PASS (incl. PASS on re-check, see T11)"
            else:
                documented = all(g["ok"] or (self.selection and self.selection.omission_for(g["selector"])) for g in gs)
                status = "/".join(statuses) + (" (documented, see T11)" if documented else "")
            n_rep = sum(len(g["reports"]) for g in gs)
            arith = "/".join(str(g["vec_arith"]) for g in gs) or "-"
            rows.insert(len(gated), [f"Vector-instruction audit ({arch})", f"Hot kernels of the measured binaries ({n_rep} reports)",
                            f"check_codegen.py: zero vector arithmetic (found {arith})",
                            "@ " + "/".join(sorted({g["fork"] for g in gs})) if gs else "-", status])
        notes = ["Test-gated rows: reproduce/this_work.sh runs cargo test --workspace before every This Work block and "
                 "steps.sh before every step tag, and neither times a build whose tests fail; the status counts the timed "
                 "blocks and step tags in this selection (Commit / Tag = the first tag that has the test).",
                 "PCS rows: the correctness fields recorded with every measurement (proof verification and direct "
                 "encoding vs. the committed root).",
                 "The vector-instruction audit rows are derived from the selected check_codegen.py reports (T11); a FAIL "
                 "written by an older classifier whose complete stored hot-kernel disassembly re-checks without vector "
                 "arithmetic is 'PASS on re-check'."]
        self._write_artefact("table_T0_correctness", "Correctness Gates and Verification Summary for Measured Binaries",
                             "tab:correctness_gates", headers, rows, notes, col_align="lllll", key_cols=1)

    # ---- T1 ---------------------------------------------------------------
    def generate_T1_headline_encoding(self):
        """This Work (adaptive headline blocks only) vs original LCPC per build variant vs PolyCommit."""
        self._begin("T1")
        T = "T1"
        variant_cols = []
        for v in PRIMARY_LCPC_VARIANTS:
            variant_cols += [f"LCPC {v} (ms)", f"Speedup vs LCPC {v}", f"Significance vs {v}"]
        headers = (["log2(N)", "T", "k x n (TW/LCPC)", "This Work (ms)", "TW ns/MAC"] + variant_cols
                   + ["PolyCommit k x n", "PolyCommit sec=80 (ms)", "Speedup vs PolyCommit", "Significance vs PolyCommit"])
        tw_pool = self.select(self.reports_this_work, family="tw_a", params="lcpc-sdig6", shape="brakedown")
        lcpc_pool = self.select(self.reports_lcpc_orig, family="lcpc_a", params="lcpc-sdig6")
        poly_pool = self.select(self.reports_polycommit, family="polycommit")
        keys = sorted({(int(r["log_n"]), int(r.get("threads", 1))) for r in tw_pool + lcpc_pool + poly_pool})
        rows = []
        dims_mismatch = []
        for log_n, threads in keys:
            if threads not in (1, 8):
                continue
            rk = f"{log_n},{threads}"
            tw_recs = self.select(tw_pool, log_n=log_n, threads=threads, slice_mode="adaptive")
            tw = self.stat(tw_recs, label=f"{T}|{rk}|This Work")
            tw_dims = sorted({self._dims(r) for r in tw_recs})
            lcpc_dims = sorted({self._dims(r) for r in self.select(lcpc_pool, log_n=log_n, threads=threads)})
            dims = ("/".join(tw_dims) or "-") + " / " + ("/".join(lcpc_dims) or "-")
            if tw_dims and tw_dims == lcpc_dims:
                dims = "/".join(tw_dims) + " (both)"
            if tw_dims and lcpc_dims and tw_dims != lcpc_dims:
                dims_mismatch.append(f"2^{log_n} T={threads}: This Work {'/'.join(tw_dims)}, LCPC {'/'.join(lcpc_dims)}")
            ns_mac = self.ns_per_mac_cell(T, rk, "TW ns/MAC", tw)
            row = [log_n, threads, dims, self.value_cell(T, rk, "This Work (ms)", tw), ns_mac]
            for v in PRIMARY_LCPC_VARIANTS:
                recs = self.select(lcpc_pool, variant=v, log_n=log_n, threads=threads)
                st = self.stat(recs, label=f"{T}|{rk}|LCPC {v}")
                row += [self.value_cell(T, rk, f"LCPC {v} (ms)", st),
                        self.speedup_cell(T, rk, f"Speedup vs LCPC {v}", st, tw),
                        self.significance(tw, st, f"{T}|{rk}|Significance vs {v}")]
            precs = self.select(poly_pool, log_n=log_n, threads=threads)
            pst = self.stat(precs, label=f"{T}|{rk}|PolyCommit")
            pd = (precs[0].get("dimensions") or {}) if precs else {}
            row += [f"{pd.get('n_rows')}x{pd.get('n_cols')}" if precs else self.missing_text(f"{T}|{rk}|PolyCommit k x n"),
                    self.value_cell(T, rk, "PolyCommit sec=80 (ms)", pst),
                    self.speedup_cell(T, rk, "Speedup vs PolyCommit", pst, tw),
                    self.significance(tw, pst, f"{T}|{rk}|Significance vs PolyCommit")]
            rows.append(row)
        notes = [
            "Mode (a) SdigCode6; This Work = blocks full_this_work_mode_a_* only (adaptive slices, Mont127); "
            "original LCPC = blocks full_lcpc_orig_mode_a_* over Ft127 (the same prime), one column group per build variant.",
            "LCPC as-published-as-is: upstream Cargo defaults with default BLAKE3 features; fork-flags-portable: the fork's "
            "rustflags with portable-only BLAKE3. The variants are never pooled.",
            "PolyCommit: same prime, pinned default graph constants, explicit square shape, opening sec_param 80; its codeword "
            "length differs (PolyCommit k x n column). Encoding only.",
            "Cells: mean of per-session medians [min-max over all timed runs]. Speedup = ratio of the means "
            "(range over per-session ratios). 'faster' only with non-overlapping ranges in every common session.",
            "ns/MAC = mean encode time / total edge-row MACs of the same records.",
        ]
        notes.append("k x n column: This Work / original LCPC matrix (rows x codeword length); '(both)' when equal.")
        if dims_mismatch:
            notes.append("LCPC matrix dimensions differ from This Work at: " + "; ".join(dims_mismatch) + ".")
        notes += self.rustflags_notes(self._used_records)
        if self.profile == "smoke" and self.skipped_baselines.get("PolyCommit"):
            notes.append("PolyCommit cells marked SKIPPED for this local smoke: " + self.skipped_baselines["PolyCommit"])
        self._write_artefact("table_T1_headline_encoding",
                             "Headline Prover Encoding Comparison (This Work vs. Original LCPC vs. PolyCommit)",
                             "tab:headline_encoding", headers, rows, notes, key_cols=2)

    # ---- T2 ---------------------------------------------------------------
    def generate_T2_simd_spielman(self):
        """mode (b) This Work vs SIMD-Spielman (T=1, T=8 and 7 workers + coordinator)."""
        self._begin("T2")
        T = "T2"
        headers = ["Field", "T", "log2(N)", "This Work (ms)", "SIMD-Spielman T workers (ms)",
                   "SIMD-Spielman 7 workers + coordinator (ms)", "SIMD Gap Recovered (8 cores)",
                   "SIMD best-of-T incl. T=1 (ms)", "Gap vs SIMD best-of-T", "SIMD MT slower than its T=1",
                   "SIMD params.patch", "[KDHR26] SIMD reported (ms)", "[KDHR26] LCPC/SIMD reported",
                   "Measured LCPC as-published-as-is/SIMD", "Measured LCPC fork-flags-portable/SIMD"]
        tw_pool = self.select(self.reports_this_work, family="tw_b", params="simd-spielman", shape="brakedown")
        simd_pool = self.select(self.reports_simd_spielman, family="simd", params="simd-spielman", shape="brakedown")
        lcpc_pool = self.select(self.reports_lcpc_orig, family="lcpc_a", params="lcpc-sdig6")
        keys = sorted({(r.get("field"), int(r["log_n"]), int(r.get("threads", 1))) for r in tw_pool + simd_pool
                       if int(r.get("threads", 1)) in (1, 8)}, key=lambda k: (str(k[0]), k[2], k[1]))
        rows = []
        cpu_used = set()
        mt_overhead_rows, mixed_patch_rows = [], []
        for field, log_n, threads in keys:
            rk = f"{field},{threads},{log_n}"
            tw_recs = self.select(tw_pool, field=field, log_n=log_n, threads=threads)
            if self.has_smoke_watermark and not tw_recs:
                continue
            tw = self.stat(tw_recs, label=f"{T}|{rk}|This Work")
            mt = self.simd_multithread_cells(T, rk, self.select(simd_pool, field=field, log_n=log_n), threads, tw,
                                             "SIMD-Spielman T workers (ms)", "SIMD-Spielman 7 workers + coordinator (ms)",
                                             "SIMD Gap Recovered (8 cores)")
            simd, best_all = mt["simd"], mt["best_all"]
            simd_text = mt["simd_text"]
            if simd.get("mean") is None and self.profile == "smoke" and self.skipped_baselines.get("SIMD-Spielman"):
                simd_text = "SKIPPED"
            if mt["slower"]:
                mt_overhead_rows.append(f"{field} 2^{log_n}")
            if mt["mixed_patch"]:
                mixed_patch_rows.append(f"{field} 2^{log_n}")
            machine = tw.get("machine") or simd.get("machine")
            entry, cpu = self.kdhr26_entry("table_2", log_n, machine)
            if field != "Mont127":
                rep_ms = rep_ratio = "N/A (KDHR26 Table 2 reports Mont127 only)"
            elif entry is None:
                rep_ms = rep_ratio = f"N/A (no KDHR26 {cpu} entry for 2^{log_n})"
            else:
                cpu_used.add(cpu)
                key_ms = "simd_spielman_1t_ms" if threads == 1 else "simd_spielman_8t_ms"
                key_sp = "lcpc_1t_speedup" if threads == 1 else "lcpc_8t_speedup"
                rep_ms = f"{entry[key_ms]}" if entry.get(key_ms) is not None else f"N/A (not reported for {cpu})"
                rep_ratio = f"{entry[key_sp]}x" if entry.get(key_sp) is not None else f"N/A (not reported for {cpu})"
            measured_ratios = []
            for v in PRIMARY_LCPC_VARIANTS:
                col = ("Measured LCPC as-published-as-is/SIMD" if v == "as-published-as-is"
                       else "Measured LCPC fork-flags-portable/SIMD")
                if field != "Mont127":
                    measured_ratios.append("N/A (original LCPC measured over Ft127/Mont127 prime only)")
                    continue
                lst = self.stat(self.select(lcpc_pool, variant=v, log_n=log_n, threads=threads),
                                label=f"{T}|{rk}|LCPC {v}")
                # At T=8 the SIMD reference is its best-of-T time (incl. T=1), so the
                # ratio is not inflated by SIMD-Spielman's small-N threading overhead.
                measured_ratios.append(self.speedup_cell(T, rk, col, lst, best_all or {}))
            rows.append([field, threads, log_n, self.value_cell(T, rk, "This Work (ms)", tw), simd_text,
                         mt["simd7_text"], mt["gap"], mt["best_text"], mt["gap_best"], mt["slower_text"],
                         mt["patch_text"], rep_ms, rep_ratio] + measured_ratios)
        cpu_text = ", ".join(sorted(cpu_used)) or "EPYC 9754"
        notes = [
            "Mode (b) parameters matching [KDHR26]; This Work = blocks full_this_work_mode_b_*; SIMD-Spielman runs its "
            "upstream slice table (slice_mode 'table').",
            "SIMD Gap Recovered = T_SIMD / T_ThisWork (>1: This Work faster in that configuration). At T=8 SIMD-Spielman's "
            "upstream encoder adds a spin-waiting coordinator thread; on the 8-core host the T=8 blocks let it share a worker "
            "core, and the 7 workers + coordinator blocks use exactly 8 cores. The 8-core gap uses the faster of the two.",
            "SIMD best-of-T also includes SIMD-Spielman's own T=1 time. Where 'SIMD MT slower than its T=1' is yes, the 8-core "
            "gap reflects SIMD-Spielman's multithreading overhead at that size, not a faster encoder; use the best-of-T gap.",
            f"[KDHR26] columns quoted verbatim from Table 2 (page 11) for {cpu_text}, the reference CPU recorded for this "
            "machine in selection.json (Zen 4c; our host is a Zen 5 EPYC 9755). The table compares our measured LCPC/SIMD ratio "
            "with their reported LCPC speedup; at T=8 our ratio uses SIMD-Spielman's best-of-T time.",
        ]
        if mt_overhead_rows:
            notes.append("SIMD-Spielman 8-core time exceeds its T=1 time at: " + ", ".join(mt_overhead_rows) + ".")
        notes += self.simd_patch_notes(mixed_patch_rows)
        if self.profile == "smoke" and self.skipped_baselines.get("SIMD-Spielman"):
            notes.append("SIMD-Spielman cells marked SKIPPED for this local smoke: " + self.skipped_baselines["SIMD-Spielman"])
        self._write_artefact("table_T2_simd_spielman",
                             "Comparison with SIMD-Spielman (AVX-512 IFMA) and Prior Reported Results",
                             "tab:simd_spielman_comparison", headers, rows, notes, key_cols=3)

    def simd_multithread_cells(self, T: str, rk: str, recs: List[Dict], threads: int, tw: Dict,
                               col_t: str, col_7w: str, col_gap: str) -> Dict[str, Any]:
        """SIMD-Spielman cells shared by T2 and T9 for one (field, N) and row thread count.

        At T=8 the 8-core SIMD time is the faster of its T=8 run (coordinator
        sharing a worker core) and its 7 workers + coordinator run; best-of-T
        additionally includes its own T=1 time, and 'MT slower than its T=1'
        flags sizes where SIMD-Spielman's multithreading is a slowdown.
        """
        simd = self.stat(self.select(recs, threads=threads), label=f"{T}|{rk}|SIMD T")
        simd_t1 = simd if threads == 1 else self.stat(self.select(recs, threads=1), label=f"{T}|{rk}|SIMD T=1")
        out: Dict[str, Any] = {"simd": simd, "slower": False, "mixed_patch": False}
        out["simd_text"] = self.value_cell(T, rk, col_t, simd)
        used = [simd]
        if threads == 8:
            simd7 = self.stat(self.select(recs, threads=7), label=f"{T}|{rk}|SIMD 7W")
            out["simd7_text"] = self.value_cell(T, rk, col_7w, simd7)
            cands = [st for st in (simd, simd7) if st.get("mean") is not None]
            best8 = min(cands, key=lambda st: st["mean"]) if cands else None
            pool = cands + ([simd_t1] if simd_t1.get("mean") is not None else [])
            best_all = min(pool, key=lambda st: st["mean"]) if pool else None
            used += [simd7, simd_t1]
        else:
            out["simd7_text"] = "N/A (T=1 row)"
            best8 = simd if simd.get("mean") is not None else None
            best_all = best8
        out["best8"], out["best_all"] = best8, best_all
        out["gap"] = self.speedup_cell(T, rk, col_gap, best8, tw)
        if threads == 1:
            out["best_text"] = out["gap_best"] = out["slower_text"] = "N/A (T=1 row)"
        else:
            which = ""
            if best_all is not None:
                which = " (T=1)" if best_all is simd_t1 else (" (7W+coord)" if best_all is not simd else " (T=8)")
            out["best_text"] = self.best_value_cell(T, rk, "SIMD best-of-T incl. T=1 (ms)", best_all, which)
            out["gap_best"] = self.speedup_cell(T, rk, "Gap vs SIMD best-of-T", best_all, tw)
            if best8 and simd_t1.get("mean") is not None:
                out["slower"] = best8["mean"] > simd_t1["mean"]
                out["slower_text"] = ("yes" if out["slower"] else "no") + self.derived_marker(
                    f"{T}|{rk}|SIMD MT slower than its T=1", best8, simd_t1)
            else:
                out["slower_text"] = self.missing_text(f"{T}|{rk}|SIMD MT slower than its T=1")
        patches = []
        for st, label in ((simd_t1, "T=1"), (simd, f"T={threads}"), (used[1] if threads == 8 else None, "7W")):
            if st and st.get("records") and (threads == 8 or label == "T=1"):
                ps = sorted({str(r["_meta"].get("simd_patch") or "?")[:8] for r in st["records"]})
                patches.append((label, "/".join(ps)))
        distinct = {p for _, p in patches}
        if not patches:
            out["patch_text"] = self.missing_text(f"{T}|{rk}|SIMD params.patch")
        elif len(distinct) == 1:
            out["patch_text"] = next(iter(distinct))
        else:
            out["mixed_patch"] = True
            out["patch_text"] = "MIXED " + ", ".join(f"{l}:{p}" for l, p in patches)
        return out

    def simd_patch_notes(self, mixed_rows: List[str]) -> List[str]:
        notes = ["SIMD params.patch: sha256 prefix of the hash-checked patch applied to SIMD-Spielman in the blocks "
                 "used by the row (part of the cell key; records of different patches are never pooled). "
                 "12f29de2 and d9cf97f0 differ only by a pinThread(THREADS) call added at the start of the benchmark "
                 "mains, which pins the spin-waiting coordinator (main) thread after the workers "
                 "(external/simd_spielman/params.patch); the encoder code is unchanged."]
        if mixed_rows:
            notes.append("WARNING: rows comparing SIMD-Spielman runs built with different params.patch versions "
                         "(best-of-T and 'MT slower than its T=1' across patches): " + ", ".join(mixed_rows) + ".")
        return notes

    # ---- T3 ---------------------------------------------------------------
    def _steps_rows(self, mode: str) -> Tuple[List[str], Dict[str, Dict[int, Dict]], Dict[str, Set[str]], Dict[str, str], List[int]]:
        tags = sorted(self.reports_steps, key=lambda t: (t == "HEAD", t))
        values: Dict[str, Dict[int, Dict]] = {}
        fields: Dict[str, Set[str]] = {}
        reasons: Dict[str, str] = {}
        sizes = sorted({int(r.get("log_n", 0)) for t in tags for r in self.reports_steps[t]
                        if str(r.get("mode", "a")) == mode})
        for tag in tags:
            recs = [r for r in self.reports_steps[tag] if str(r.get("mode", "a")) == mode]
            values[tag] = {}
            fields[tag] = {str(r.get("field")) for r in recs if r.get("field") and self._extract_samples(r)}
            for r in recs:
                if not self._extract_samples(r) and r.get("reason"):
                    reasons[tag] = str(r["reason"])
            for n in sizes:
                values[tag][n] = self.stat([r for r in recs if int(r.get("log_n", 0)) == n],
                                           label=f"T3|{mode},{tag}|N=2^{n}")
        return tags, values, fields, reasons, sizes

    def generate_T3_steps_breakdown(self):
        """cumulative per-technique breakdown, mode (a) and mode (b), T=1."""
        self._begin("T3")
        T = "T3"
        technique = {
            "step/01-modernize": "Upstream modernize & workspace harness",
            "step/02-fields": "Custom field trait & fast Montgomery/Mersenne",
            "step/03-column-major": "Batched column-major matrix representation",
            "step/04-r2l": "R2L evaluation order (no transposed graphs)",
            "step/05-lazy": "Expander-level lazy reduction",
            "step/06-blocking": "Multi-edge memory cache blocking",
            "step/07-slicing": "Matrix horizontal LLC slicing",
            "step/08-threads": "Expander-level multithreading",
            "step/09-codegen": "Scalar codegen & compiler vectorization verify",
            "HEAD": "Measured HEAD",
        }
        all_sizes = sorted({int(r.get("log_n", 0)) for recs in self.reports_steps.values() for r in recs})
        ref = 20 if 20 in all_sizes else (all_sizes[len(all_sizes) // 2] if all_sizes else 20)
        headers = ["Mode", "Step Tag", "Field", "Optimization Technique"] + [f"N=2^{n} (ms)" for n in all_sizes] + \
                  [f"Step Speedup (N=2^{ref})", f"Cumulative Speedup (N=2^{ref})"]
        rows = []
        notes_extra = []
        na_reasons = set()
        for mode in ("a", "b"):
            tags, values, fields, reasons, sizes = self._steps_rows(mode)
            if not sizes:
                continue
            na_reasons.update(f"Mode ({mode}) N/A rows: {reasons[t]}" for t in tags if not fields[t] and t in reasons)
            prev = first = None
            for tag in tags:
                rk = f"{mode},{tag}"
                field_text = "/".join(sorted(fields[tag])) or "-"
                if not fields[tag]:
                    na = ("N/A (mode (b) requires step/04-r2l)" if mode == "b" else "N/A (not measured at this tag)")
                    rows.append([mode, tag, "N/A (not applicable)", technique.get(tag, "Incremental optimization")]
                                + [na for _ in all_sizes] + [na, na])
                    continue
                cells = [self.value_cell(T, rk, f"N=2^{n} (ms)", values[tag].get(n) or {}, with_range=False)
                         if n in sizes else "N/A (size not run in this mode)" for n in all_sizes]
                cur_st = values[tag].get(ref) or {}
                cur = cur_st.get("mean")
                step_txt = "baseline" if prev is None else self.ratio_cell(T, rk, f"Step Speedup (N=2^{ref})", prev, cur_st)
                cum_txt = "baseline" if first is None else self.ratio_cell(T, rk, f"Cumulative Speedup (N=2^{ref})",
                                                                         first, cur_st)
                if cur:
                    prev = cur_st
                    first = first or cur_st
                rows.append([mode, tag, field_text, technique.get(tag, "Incremental optimization")] + cells + [step_txt, cum_txt])
            changed = [t for t in tags if fields[t] and fields[t] != fields[tags[-1]]]
            if changed:
                notes_extra.append(f"Mode ({mode}): {', '.join(changed)} measured over "
                                   f"{'/'.join(sorted(set().union(*(fields[t] for t in changed))))}, later tags over "
                                   f"{'/'.join(sorted(fields[tags[-1]]))}; the following step ratio includes that field change.")
        notes = [
            "Evaluated at T=1 with steps.sh: each tag's mode is taken from the block named for it (full_steps_mode_a / "
            "full_steps_mode_b); both blocks time both modes, the other copy is excluded in selection.json.",
            f"Measured sizes: {', '.join('2^%d' % n for n in all_sizes)}.",
            "Mode (b) uses SIMD-Spielman parameters over Mers127; it is unavailable before step/04-r2l.",
            "The per-step breakdown is order-dependent; optimizations interact non-linearly.",
        ] + notes_extra + sorted(na_reasons) + self.gate_step_codegen()
        self._write_artefact("table_T3_steps_breakdown", "Cumulative Technique Breakdown Across Tagged Commits",
                             "tab:steps_breakdown", headers, rows, notes, key_cols=2)

    # ---- T4 ---------------------------------------------------------------
    def generate_T4_slice_sweep(self):
        """explicit slice sweep vs the adaptive model."""
        self._begin("T4")
        T = "T4"
        headers = ["log2(N)", "T", "s=1 (ms)", "Best s*", "Best (ms)", "Adaptive s", "Sweep at adaptive s (ms)",
                   "Headline adaptive (ms)", "Adaptive Gap (%)", "Gap <= 10%", "Slicing Speedup (s=1 / best)"]
        sweep = self.select(self.reports_this_work, family="slice_sweep", params="lcpc-sdig6", shape="brakedown")
        headline = self.select(self.reports_this_work, family="tw_a", params="lcpc-sdig6", shape="brakedown",
                               slice_mode="adaptive")
        rows, verdicts = [], []
        for log_n, threads in sorted({(int(r["log_n"]), int(r.get("threads", 1))) for r in sweep}):
            rk = f"{log_n},{threads}"
            recs = self.select(sweep, log_n=log_n, threads=threads)
            by_s = {s: self.stat(self.select(recs, slices=s), label=f"{T}|{rk}|s={s}")
                    for s in sorted({int(r["slices"]) for r in recs})}
            measured = {s: st for s, st in by_s.items() if st.get("mean") is not None}
            if not measured:
                continue
            best_s = min(measured, key=lambda s: measured[s]["mean"])
            adapt = {int(r["adaptive_slices"]) for r in recs if r.get("adaptive_slices") is not None}
            adapt_s = next(iter(adapt)) if len(adapt) == 1 else None
            head = self.stat(self.select(headline, log_n=log_n, threads=threads), label=f"{T}|{rk}|headline adaptive")
            gap_txt = verdict = "-"
            if adapt_s in measured:
                gap = (measured[adapt_s]["mean"] - measured[best_s]["mean"]) / measured[best_s]["mean"] * 100.0
                gap_txt = f"{gap:.1f}%" + self.derived_marker(f"{T}|{rk}|Adaptive Gap (%)", measured[adapt_s],
                                                              measured[best_s])
                verdict = "yes" if gap <= 10.0 else "no"
                verdicts.append(verdict)
            else:
                gap_txt = verdict = self.missing_text(f"{T}|{rk}|Adaptive Gap (%)")
            s1 = by_s.get(1) or {}
            spd = self.ratio_cell(T, rk, "Slicing Speedup (s=1 / best)", s1, measured[best_s])
            rows.append([log_n, threads, self.value_cell(T, rk, "s=1 (ms)", s1, with_range=False), best_s,
                         self.value_cell(T, rk, "Best (ms)", measured[best_s], with_range=False),
                         adapt_s if adapt_s is not None else self.missing_text(f"{T}|{rk}|Adaptive s"),
                         self.value_cell(T, rk, "Sweep at adaptive s (ms)", measured.get(adapt_s) or {}, with_range=False),
                         self.value_cell(T, rk, "Headline adaptive (ms)", head, with_range=False),
                         gap_txt, verdict, spd])
        swept = sorted({int(r["slices"]) for r in sweep})
        notes = [
            "Blocks full_slice_sweep_* (explicit slices s in {" + ", ".join(map(str, swept)) + "}), Mont127, mode (a). "
            "Adaptive s = the model's choice recorded in the sweep records; the headline column is the separately measured "
            "full_this_work_mode_a_* block.",
            "Adaptive Gap = (T(s_adapt) - T(s*)) / T(s*) within the sweep. "
            + (f"Gap <= 10% in {verdicts.count('yes')} of {len(verdicts)} rows." if verdicts else
               "Gap not evaluated (no sweep at the adaptive s)."),
        ]
        self._write_artefact("table_T4_slice_sweep", "LLC Slice Sweep and Adaptive Cache Model Validation",
                             "tab:slice_sweep", headers, rows, notes, key_cols=2)

    # ---- T5 ---------------------------------------------------------------
    def generate_T5_field_comparison(self):
        """field microbenchmarks + mode (b) encodings at T=1 (same parameters for every field)."""
        self._begin("T5")
        T = "T5"
        sizes = (20, 24)
        headers = ["Field", "Security (bits)", "Mul (ns)", "MAC (ns)", "Reduce (ns)"] + \
                  [f"Encode N=2^{n} T=1 (ms)" for n in sizes] + [f"Cost N=2^{n} (ns/MAC)" for n in sizes]
        micro = [fb for fb in self.field_benchmarks if fb["_meta"]["family"] in ("tw_a", "tw_b", "unknown", None)
                 or not self.selection]
        pool = self.select(self.reports_this_work, family="tw_b", params="simd-spielman", shape="brakedown", threads=1)
        rows = []
        for f in ("Mont127", "Mers127", "Fp2M61", "Ft127"):
            rk = f
            ops = []
            for op in ("mul", "mac", "reduce"):
                per_session: Dict[str, List[float]] = defaultdict(list)
                for fb in micro:
                    if fb.get("field") == f and safe_float((fb.get(op) or {}).get("ns_per_op")):
                        per_session[fb["_meta"]["session"]].append(float(fb[op]["ns_per_op"]))
                        m = fb["_meta"]
                        self._used.add((m["machine"], m["run"], m["session_dir"]))
                meds = [median(v) for v in per_session.values()]
                ops.append(f"{sum(meds) / len(meds):.2f}" if meds else self.missing_text(f"{T}|{rk}|{op}"))
            enc, cost = [], []
            for n in sizes:
                if f == "Ft127":
                    enc.append("N/A (test-only oracle field; no mode (b) encoder)")
                    cost.append("N/A (test-only oracle field; no mode (b) encoder)")
                    continue
                if self.has_smoke_watermark and n == 24:
                    enc.append("N/A (N=2^24 omitted from local smoke)")
                    cost.append("N/A (N=2^24 omitted from local smoke)")
                    continue
                st = self.stat(self.select(pool, field=f, log_n=n), label=f"{T}|{rk}|encode 2^{n}")
                enc.append(self.value_cell(T, rk, f"Encode N=2^{n} T=1 (ms)", st))
                cost.append(self.ns_per_mac_cell(T, rk, f"Cost N=2^{n} (ns/MAC)", st))
            rows.append([f, 122 if f == "Fp2M61" else 127] + ops + enc + cost)
        notes = [
            "Microbenchmarks: per session the median over the field_microbenchmarks records of the headline blocks, then "
            "the mean over sessions (ns per operation).",
            "Encodings: mode (b) SIMD-Spielman parameters for every field (blocks full_this_work_mode_b_*), T=1, adaptive "
            "slices; cost = mean encode time / total edge-row MACs of the same records.",
            "Security caveat: Fp2M61 is F_{(2^61-1)^2}, 122 bits rather than 127.",
            "Ft127 is LCPC's field, kept as a test-only oracle in the fork.",
        ]
        self._write_artefact("table_T5_field_comparison", "Field Arithmetic Microbenchmarks and Encoding Comparison",
                             "tab:field_comparison", headers, rows, notes, key_cols=1)

    # ---- T6 ---------------------------------------------------------------
    @staticmethod
    def _hash_label(r: Dict) -> str:
        mode = str(r.get("hash_mode") or "")
        if mode == "portable-only":
            return "BLAKE3 portable"
        if mode.startswith("as-is"):
            return "BLAKE3 default features (runtime SIMD dispatch)"
        return mode or "hash unknown"

    def _impl_rows(self, family_tw: str, family_lcpc: str, pool_tw: List[Dict], pool_lcpc: List[Dict]):
        yield "This Work", self.select(pool_tw, family=family_tw, params="lcpc-sdig6", shape="brakedown", slice_mode="adaptive")
        for v in PRIMARY_LCPC_VARIANTS:
            yield f"Original LCPC {v}", self.select(pool_lcpc, family=family_lcpc, params="lcpc-sdig6", variant=v)

    @staticmethod
    def _t6_variant_effects(means: Dict[Tuple[str, int, int, str], Optional[float]]) -> List[str]:
        """Per (T, log2 N): BLAKE3-backend and compiler-flag ratios between the four LCPC build variants.

        Computed only from T6 cell means; a ratio is reported only when both
        of its cells were measured.
        """
        def ratio(num_variant, den_variant, threads, log_n, op):
            num = means.get((f"Original LCPC {num_variant}", threads, log_n, op))
            den = means.get((f"Original LCPC {den_variant}", threads, log_n, op))
            return f"{num / den:.2f}x" if num and den else None

        out = []
        keys = sorted({(t, n) for (label, t, n, _op) in means if label.startswith("Original LCPC ")})
        for threads, log_n in keys:
            parts = []
            for what, op in (("hash", "hash_matrix_decomposition"), ("commit", "commit_total")):
                pairs = [(f, ratio(f"{f}-portable", f"{f}-as-is", threads, log_n, op))
                         for f in ("as-published", "fork-flags")]
                got = [f"{r} with {f.replace('-', ' ')}" for f, r in pairs if r]
                if got:
                    parts.append(f"backend on {what} " + ", ".join(got))
            for what, op in (("encode", "encode_matrix"), ("commit", "commit_total")):
                pairs = [(b, ratio(f"fork-flags-{b}", f"as-published-{b}", threads, log_n, op))
                         for b in ("as-is", "portable")]
                got = [f"{r} with {'default-features' if b == 'as-is' else 'portable'} BLAKE3" for b, r in pairs if r]
                if got:
                    parts.append(f"flags on {what} " + ", ".join(got))
            if parts:
                out.append(f"T={threads} 2^{log_n}: " + "; ".join(parts) + ".")
        return out

    def generate_T6_pcs_breakdown(self):
        """commit (encode/hash), prove, verify, proof size; per implementation and LCPC variant."""
        self._begin("T6")
        T = "T6"
        headers = ["Implementation", "T", "log2(N)", "Hashing", "Commit (ms)", "Encode (ms)", "Hash (ms)",
                   "Enc/Hash (%)", "Prove (ms)", "Verify (ms)", "Proof Size (B)"]
        rows, regress = [], []
        means: Dict[Tuple[str, int, int, str], Optional[float]] = {}
        groups = [(label, pool, (20, 24, 28)) for label, pool in
                  self._impl_rows("tw_a", "lcpc_a", self.reports_this_work, self.reports_lcpc_orig)]
        groups += [(f"Original LCPC {v}", self.select(self.reports_lcpc_orig, family="lcpc_a", params="lcpc-sdig6",
                                                      variant=v), SECONDARY_LCPC_SIZES)
                   for v in SECONDARY_LCPC_VARIANTS]
        groups.append(("This Work Ft127", self.select(self.reports_this_work, family="tw_ft127", params="lcpc-sdig6",
                                                      shape="brakedown", slice_mode="adaptive"), FT127_SIZES))
        for label, pool, sizes in groups:
            for threads in (1, 8):
                for log_n in sizes:
                    recs = self.select(pool, threads=threads, log_n=log_n)
                    rk = f"{label},{threads},{log_n}"
                    if not recs and not (self.selection and self.selection.omission_for(f"{T}|{rk}|Commit (ms)")):
                        continue
                    sts = {op: self.stat(recs, op, label=f"{T}|{rk}|{op}") for op in
                           ("commit_total", "encode_matrix", "hash_matrix_decomposition", "prove", "verify")}
                    for op, st in sts.items():
                        means[(label, threads, log_n, op)] = st.get("mean")
                    enc, hsh = sts["encode_matrix"].get("mean"), sts["hash_matrix_decomposition"].get("mean")
                    split = f"{enc / (enc + hsh) * 100:.1f} / {hsh / (enc + hsh) * 100:.1f}" if enc and hsh else \
                        self.missing_text(f"{T}|{rk}|Enc/Hash (%)")
                    proof = {r.get("proof_size_bytes") for r in recs if r.get("proof_size_bytes")}
                    rows.append([label, threads, log_n,
                                 "/".join(sorted({self._hash_label(r) for r in recs})) or self.missing_text(f"{T}|{rk}|Hashing"),
                                 self.value_cell(T, rk, "Commit (ms)", sts["commit_total"], with_range=False),
                                 self.value_cell(T, rk, "Encode (ms)", sts["encode_matrix"], with_range=False),
                                 self.value_cell(T, rk, "Hash (ms)", sts["hash_matrix_decomposition"], with_range=False),
                                 split,
                                 self.value_cell(T, rk, "Prove (ms)", sts["prove"], with_range=False),
                                 self.value_cell(T, rk, "Verify (ms)", sts["verify"], with_range=False),
                                 "/".join(str(p) for p in sorted(proof)) if proof else self.missing_text(f"{T}|{rk}|Proof Size (B)")])
        for (label, threads, log_n, op), v in means.items():
            if label.startswith("This Work") or op not in ("prove", "verify"):
                continue
            tw = means.get(("This Work", threads, log_n, op))
            if tw and v and tw > v:
                regress.append(f"{op} T={threads} 2^{log_n} vs {label.replace('Original LCPC ', '')} ({tw / v:.2f}x slower)")
        notes = [
            "Mode (a) only. This Work = full_this_work_mode_a_* (adaptive); original LCPC rows are labelled by build variant "
            "and BLAKE3 backend (Hashing column): the hash columns are like-for-like only between rows with the same backend.",
            "Cells: mean of per-session medians of each operation.",
            "Original LCPC as-published-portable and fork-flags-as-is are measured at 2^20 and 2^24 only, to separate "
            "the BLAKE3 backend from the compiler flags.",
            "This Work Ft127 = full_this_work_ft127_* (2^20 and 2^24): the fork over original LCPC's field Ft127 with "
            "the headline parameters, for a same-field comparison with the original LCPC rows.",
        ]
        effects = self._t6_variant_effects(means)
        if effects:
            notes.append("BLAKE3 backend vs compiler flags (ratios of the cell means above). Backend = BLAKE3 portable / "
                         "default features with the same flags; flags = fork flags / as published with the same "
                         "backend. " + " ".join(effects))
        if regress:
            notes.append("This Work is slower than original LCPC in: " + "; ".join(sorted(regress)) + ".")
        self._write_artefact("table_T6_pcs_breakdown",
                             "End-to-End PCS Breakdown: Commitment, Proving, Verification, and Proof Size",
                             "tab:pcs_breakdown", headers, rows, notes, key_cols=3)

    # ---- T7 ---------------------------------------------------------------
    def generate_T7_verifier_encoding(self):
        """verifier-side encoding of 1 and 2 vectors, T=1."""
        self._begin("T7")
        T = "T7"
        headers = ["Mode", "Field", "log2(N)", "k", "This Work v=1 (us)", "This Work v=2 (us)",
                   "SIMD-Spielman v=2 (us)", "SIMD / This Work (v=2)", "[KDHR26] Table 6 (us)", "[KDHR26] / This Work (v=2)"]
        rows = []
        cpus = set()
        for mode, family, params in (("a", "tw_a", "lcpc-sdig6"), ("b", "tw_b", "simd-spielman")):
            pool = self.select(self.reports_this_work, family=family, params=params, shape="brakedown", threads=1,
                               field="Mont127")
            for log_n in sorted({int(r["log_n"]) for r in pool}):
                rk = f"{mode},Mont127,{log_n}"
                recs = self.select(pool, log_n=log_n)
                v1 = self.stat(recs, "verifier_encode_1", label=f"{T}|{rk}|v1")
                v2 = self.stat(recs, "verifier_encode_2", label=f"{T}|{rk}|v2")
                if v2.get("mean") is None:
                    v2 = self.stat(recs, "verifier_encode", label=f"{T}|{rk}|v2")
                k = (recs[0].get("dimensions") or {}).get("n_rows", "-") if recs else "-"
                if mode == "a":
                    simd_txt = ratio_simd = "N/A (SIMD-Spielman runs mode (b) parameters only)"
                    kd_txt = kd_ratio = "N/A (KDHR26 Table 6 uses mode (b) parameters)"
                else:
                    simd_recs = self.select(self.reports_simd_spielman, family="simd", field="Mont127", shape="brakedown",
                                            log_n=log_n, threads=1)
                    simd = self.stat(simd_recs, "verifier_encode_2", label=f"{T}|{rk}|SIMD v2")
                    simd_txt = self.value_cell(T, rk, "SIMD-Spielman v=2 (us)", simd, unit="us", with_range=False)
                    if simd.get("mean") is None and self.profile == "smoke" and self.skipped_baselines.get("SIMD-Spielman"):
                        simd_txt = "SKIPPED"
                    ratio_simd = self.speedup_cell(T, rk, "SIMD / This Work (v=2)", simd, v2)
                    entry, cpu = self.kdhr26_entry("table_6", log_n, v2.get("machine"))
                    if entry and entry.get("simd_spielman_us") is not None and v2.get("mean"):
                        cpus.add(cpu)
                        kd_txt = f"{entry['simd_spielman_us']:.1f}"
                        kd_ratio = (f"{entry['simd_spielman_us'] * 1000.0 / v2['mean']:.2f}x"
                                    + self.derived_marker(f"{T}|{rk}|[KDHR26] / This Work (v=2)", v2))
                    else:
                        kd_txt = kd_ratio = f"N/A (no KDHR26 {cpu} entry for 2^{log_n})"
                rows.append([mode, "Mont127", log_n, k,
                             self.value_cell(T, rk, "This Work v=1 (us)", v1, unit="us", with_range=False),
                             self.value_cell(T, rk, "This Work v=2 (us)", v2, unit="us", with_range=False),
                             simd_txt, ratio_simd, kd_txt, kd_ratio])
        notes = [
            "Verifier-side encoding of 1 (v=1) and 2 (v=2) vectors of length k, T=1, from the headline blocks "
            "(mode (a): full_this_work_mode_a_*, mode (b): full_this_work_mode_b_*). SIMD-Spielman: its T=1 blocks.",
            "[KDHR26] values quoted verbatim from Table 6 (page 14) for " + (", ".join(sorted(cpus)) or "EPYC 9754")
            + "; compared with mode (b) only.",
        ]
        if self.profile == "smoke" and self.skipped_baselines.get("SIMD-Spielman"):
            notes.append("SIMD-Spielman verifier cells marked SKIPPED for this local smoke: " + self.skipped_baselines["SIMD-Spielman"])
        self._write_artefact("table_T7_verifier_encoding", "Verifier-Side Small-Batch Encoding Latency Comparison",
                             "tab:verifier_encoding", headers, rows, notes, key_cols=3)

    # ---- T8 ---------------------------------------------------------------
    def generate_T8_setup_memory(self):
        """setup time and peak RSS, T=1."""
        self._begin("T8")
        T = "T8"
        headers = ["Implementation", "log2(N)", "Setup (ms)", "Encode (ms)", "Setup / Encode", "Peak RSS (MB)"]

        def setup_ns(r):
            v = r.get("setup_ns")
            if isinstance(v, dict):
                vals = [safe_float(x) for x in v.values()]
                vals = [x for x in vals if x is not None]
                return sum(vals) if vals else None
            v = safe_float(v)
            return v if v and v > 0 else None

        def rss(r):
            v = safe_float(r.get("peak_rss_bytes"))
            return v if v and v > 0 else None

        rows = []
        for label, pool in self._impl_rows("tw_a", "lcpc_a", self.reports_this_work, self.reports_lcpc_orig):
            for log_n in (20, 24, 28):
                recs = self.select(pool, threads=1, log_n=log_n)
                rk = f"{label},{log_n}"
                if not recs and not (self.selection and self.selection.omission_for(f"{T}|{rk}|Setup (ms)")):
                    continue
                enc = self.stat(recs, label=f"{T}|{rk}|encode")
                su = self.scalar_stat(recs, setup_ns)
                mem = self.scalar_stat(recs, rss)
                ratio = self.ratio_cell(T, rk, "Setup / Encode", su, enc) if su else self.missing_text(f"{T}|{rk}|Setup / Encode")
                rows.append([label, log_n,
                             (format_ns(su["mean"]) + self.derived_marker(f"{T}|{rk}|Setup (ms)", dict(su, machine=enc.get("machine"))))
                             if su else self.missing_text(f"{T}|{rk}|Setup (ms)"),
                             self.value_cell(T, rk, "Encode (ms)", enc, with_range=False), ratio,
                             (format_bytes_to_mb(mem["max"]) + self.derived_marker(f"{T}|{rk}|Peak RSS (MB)",
                                                                                   dict(mem, machine=enc.get("machine"))))
                             if mem else self.missing_text(f"{T}|{rk}|Peak RSS (MB)")])
        simd_flagged, simd_res, simd_floor, simd_unrecorded = [], set(), set(), []
        simd_partial: Dict[str, List[str]] = defaultdict(list)    # sessions -> sizes lacking setup/RSS there
        for log_n in (20, 24, 28):
            recs = self.select(self.reports_simd_spielman, family="simd", threads=1, shape="brakedown", field="Mont127",
                               params="simd-spielman", log_n=log_n)
            if not recs:
                continue
            rk = f"SIMD-Spielman,{log_n}"
            enc = self.stat(recs, label=f"{T}|{rk}|encode")
            # simd_spielman.sh derives setup from GNU time (process wall time
            # minus the printed encode samples) and records its resolution and
            # floor; values below the floor are kept but flagged.
            su = self.scalar_stat(recs, setup_ns)
            mem = self.scalar_stat(recs, rss)
            below = any(r.get("setup_ns_below_floor") is True for r in recs if setup_ns(r) is not None)
            simd_res.update(int(r["setup_ns_resolution_ns"]) for r in recs if safe_float(r.get("setup_ns_resolution_ns")))
            simd_floor.update(int(r["setup_ns_floor_ns"]) for r in recs if safe_float(r.get("setup_ns_floor_ns")))
            if below:
                simd_flagged.append(f"2^{log_n}")
            # Sessions whose encode records carry no setup/RSS (harness version
            # that exposed encoding only). A partial gap makes the cells
            # '[sX only]', which needs an omission (derived_marker below); while
            # no session recorded setup/RSS the cells are 'N/A (not recorded)'
            # and a pre-wired omission for the gap is credited here, so it is
            # not reported as stale before the missing session lands.
            have = set(su["sessions"]) if su else set()
            have &= set(mem["sessions"]) if mem else set()
            gap = [sid for sid in enc.get("sessions", []) if sid not in have]
            if gap:
                if have:
                    simd_partial["/".join(gap)].append(f"2^{log_n}")
                else:
                    simd_unrecorded.append(f"2^{log_n}")
                if not have and self.selection:
                    for col in ("Setup (ms)", "Setup / Encode", "Peak RSS (MB)"):
                        self.selection.omission_for(f"{T}|{rk}|{col}")
            na = "N/A (not recorded, see note)"
            setup_txt = ((format_ns(su["mean"]) + (" (below floor)" if below else "")
                          + self.derived_marker(f"{T}|{rk}|Setup (ms)", dict(su, machine=enc.get("machine"))))
                         if su else na)
            ratio = self.ratio_cell(T, rk, "Setup / Encode", su, enc) if su else na
            rss_txt = ((format_bytes_to_mb(mem["max"]) + self.derived_marker(f"{T}|{rk}|Peak RSS (MB)",
                                                                             dict(mem, machine=enc.get("machine"))))
                       if mem else na)
            rows.append(["SIMD-Spielman", log_n, setup_txt,
                         self.value_cell(T, rk, "Encode (ms)", enc, with_range=False), ratio, rss_txt])
        if self.profile == "smoke" and self.skipped_baselines.get("SIMD-Spielman"):
            rows.append(["SIMD-Spielman", 20, "SKIPPED", "SKIPPED", "SKIPPED", "SKIPPED"])
        notes = [
            "T=1. This Work: full_this_work_mode_a_* (adaptive); original LCPC by build variant; SIMD-Spielman: mode (b), Mont127.",
            "Setup = graph sampling and encoder initialisation + input/row-buffer initialisation + verifier-encoder "
            "initialisation (mean over sessions); peak RSS = VmHWM, maximum over sessions.",
            "SIMD-Spielman: setup = GNU time elapsed wall clock of its benchmark process minus the sum of its printed "
            "encode samples (includes process start, graph generation, input generation and output); peak RSS = GNU "
            "time maximum resident set size. Not like-for-like with the in-process setup timers of the other rows.",
        ]
        if simd_res:
            notes.append("SIMD-Spielman setup resolution: GNU time reports elapsed time in steps of "
                         + "/".join(f"{v / 1e6:g} ms" for v in sorted(simd_res)) + "; values below the floor of "
                         + ("/".join(f"{v / 1e6:g} ms" for v in sorted(simd_floor)) or "10x the resolution")
                         + " are marked '(below floor)'" + (f" (at {', '.join(simd_flagged)})" if simd_flagged else "")
                         + " and are not meaningful as setup measurements.")
        if simd_unrecorded or simd_partial:
            where = ([("at " + ", ".join(simd_unrecorded))] if simd_unrecorded else []) + [
                f"in {sessions} at {', '.join(sizes)} (those cells show the other sessions only)"
                for sessions, sizes in sorted(simd_partial.items())]
            notes.append("SIMD-Spielman setup/RSS not recorded " + "; ".join(where) + ": those blocks were run by a "
                         "harness version that exposed encoding only.")
        self._write_artefact("table_T8_setup_memory", "One-Time Setup Cost and Peak Memory Overhead",
                             "tab:setup_memory", headers, rows, notes, key_cols=2)

    # ---- T9 ---------------------------------------------------------------
    def generate_T9_orion_shape(self):
        """Orion shape (128 rows), mode (b)."""
        self._begin("T9")
        T = "T9"
        headers = ["Field", "T", "log2(N)", "This Work Orion (ms)", "SIMD-Spielman Orion T workers (ms)",
                   "SIMD-Spielman Orion 7 workers + coordinator (ms)", "SIMD / This Work",
                   "SIMD best-of-T incl. T=1 (ms)", "Gap vs SIMD best-of-T", "SIMD MT slower than its T=1",
                   "SIMD params.patch", "[KDHR26] Table 3 1T (ms)", "This Work Orion / Brakedown"]
        tw_pool = self.select(self.reports_this_work, family="tw_b", params="simd-spielman")
        simd_pool = self.select(self.reports_simd_spielman, family="simd", params="simd-spielman", shape="orion")
        failed_simd = self.select([r for r in self.unmeasured if r["_meta"]["impl"] == "simd"], shape="orion")
        keys = sorted({(r.get("field"), int(r.get("threads", 1)), int(r["log_n"]))
                       for r in self.select(tw_pool, shape="orion") + simd_pool + failed_simd
                       if int(r.get("threads", 1)) in (1, 8)}, key=lambda k: (str(k[0]), k[1], k[2]))
        rows, cpus = [], set()
        mt_overhead_rows, mixed_patch_rows = [], []
        for field, threads, log_n in keys:
            rk = f"{field},{threads},{log_n}"
            tw_recs = self.select(tw_pool, shape="orion", field=field, threads=threads, log_n=log_n)
            if self.has_smoke_watermark and not tw_recs:
                continue
            tw = self.stat(tw_recs, label=f"{T}|{rk}|TW orion")
            brake = self.stat(self.select(tw_pool, shape="brakedown", field=field, threads=threads, log_n=log_n),
                              label=f"{T}|{rk}|TW brakedown")
            mt = self.simd_multithread_cells(T, rk, self.select(simd_pool, field=field, log_n=log_n), threads, tw,
                                             "SIMD-Spielman Orion T workers (ms)",
                                             "SIMD-Spielman Orion 7 workers + coordinator (ms)", "SIMD / This Work")
            simd_txt = mt["simd_text"]
            if mt["simd"].get("mean") is None and self.profile == "smoke" and self.skipped_baselines.get("SIMD-Spielman"):
                simd_txt = "SKIPPED"
            if mt["slower"]:
                mt_overhead_rows.append(f"{field} 2^{log_n}")
            if mt["mixed_patch"]:
                mixed_patch_rows.append(f"{field} 2^{log_n}")
            if threads != 1:
                kd = "N/A (KDHR26 Table 3 is single-threaded)"
            elif field != "Fp2M61":
                kd = "N/A (KDHR26 Table 3 reports Fp2M61 only)"
            else:
                entry, cpu = None, "EPYC 9754"
                machine = tw.get("machine")
                cpu = (self.selection.kdhr26_cpu(machine) if self.selection and machine else None) or cpu
                key = {"EPYC 9754": "simd_spielman_epyc_ms", "i7-11800H": "simd_spielman_i7_ms",
                       "Xeon Gold 6530": "simd_spielman_xeon_ms"}.get(cpu)
                for e in self.kdhr26_data.get("table_3", {}).get("entries", []):
                    if e.get("log_n") == log_n:
                        entry = e
                kd = f"{entry[key]}" if entry and key and entry.get(key) is not None else f"N/A (no KDHR26 {cpu} entry for 2^{log_n})"
                if entry and key and entry.get(key) is not None:
                    cpus.add(cpu)
            ratio = self.ratio_cell(T, rk, "This Work Orion / Brakedown", tw, brake)
            rows.append([field, threads, log_n, self.value_cell(T, rk, "This Work Orion (ms)", tw), simd_txt,
                         mt["simd7_text"], mt["gap"], mt["best_text"], mt["gap_best"], mt["slower_text"],
                         mt["patch_text"], kd, ratio])
        notes = [
            "Orion matrix shape: 128 rows, N/128 columns; mode (b) parameters; This Work = full_this_work_mode_b_* blocks.",
            "[KDHR26] Table 3 (single-threaded) reports Fp2M61 only; quoted verbatim for " + (", ".join(sorted(cpus)) or "EPYC 9754") + ".",
            "At T=8 the SIMD / This Work ratio uses the faster of SIMD-Spielman's T=8 and 7 workers + coordinator runs "
            "(8 cores). SIMD best-of-T also includes its own T=1 time; where 'SIMD MT slower than its T=1' is yes, use the "
            "best-of-T gap (as in T2).",
        ]
        if mt_overhead_rows:
            notes.append("SIMD-Spielman 8-core Orion time exceeds its T=1 time at: " + ", ".join(mt_overhead_rows) + ".")
        notes += self.simd_patch_notes(mixed_patch_rows)
        if failed_simd:
            # Status and reason come from the records themselves ("omitted" with
            # omission_reason from --max-orion-logn, or "failed" with error).
            by_status: Dict[Tuple[str, str], Set[str]] = defaultdict(set)
            for r in failed_simd:
                reason = r.get("omission_reason") or r.get("error") or r.get("failure_reason") or "no reason recorded"
                by_status[(str(r.get("status")), str(reason))].add(f"{r.get('field')} T={r.get('threads')} 2^{r.get('log_n')}")
            for (status, reason), cases in sorted(by_status.items()):
                notes.append(f"SIMD-Spielman Orion-shape records with status '{status}' ({reason}): "
                             + ", ".join(sorted(cases)) + ".")
        self._write_artefact("table_T9_orion_shape", "Orion Matrix Shape Encoding Comparison (128 Rows)",
                             "tab:orion_shape", headers, rows, notes, key_cols=3)

    # ---- T10 --------------------------------------------------------------
    def generate_T10_brakedown_vs_rs(self):
        """RS (Ligero) vs Brakedown encoding in the same library, T=1, at the sizes RS was measured."""
        self._begin("T10")
        T = "T10"
        headers = ["log2(N)", "N", "RS field", "RS k x n (rate)", "Brakedown field", "Brakedown k x n (rate)",
                   "Brakedown Encoding (ms)", "Reed-Solomon Ligero (ms)", "RS / Brakedown Ratio"]
        rs_pool = self.select(self.reports_ligero_rs, threads=1)
        tw_pool = self.select(self.reports_this_work, family="tw_a", params="lcpc-sdig6", shape="brakedown",
                              slice_mode="adaptive", threads=1)

        def dims(recs):
            if not recs:
                return "-"
            d = recs[0].get("dimensions") or {}
            try:
                rate = f"1/{d['n_cols'] / d['n_per_row']:.2f}"
            except (KeyError, TypeError, ZeroDivisionError):
                rate = "?"
            return f"{d.get('n_rows')}x{d.get('n_cols')} ({rate})"

        rows = []
        for log_n in sorted({int(r["log_n"]) for r in rs_pool}):
            rk = f"{log_n}"
            rs_recs = self.select(rs_pool, log_n=log_n)
            tw_recs = self.select(tw_pool, log_n=log_n)
            rs = self.stat(rs_recs, label=f"{T}|{rk}|RS")
            tw = self.stat(tw_recs, label=f"{T}|{rk}|Brakedown")
            rows.append([log_n, 1 << log_n, "/".join(sorted({str(r.get("field")) for r in rs_recs})), dims(rs_recs),
                         "/".join(sorted({str(r.get("field")) for r in tw_recs})) or "-", dims(tw_recs),
                         self.value_cell(T, rk, "Brakedown Encoding (ms)", tw),
                         self.value_cell(T, rk, "Reed-Solomon Ligero (ms)", rs),
                         self.speedup_cell(T, rk, "RS / Brakedown Ratio", rs, tw)])
        notes = [
            "RS: the fork's lcpc-ligero-pc encoding (FFT path not optimized by this work), blocks full_ligero_rs_*. "
            "Brakedown: full_this_work_mode_a_* (adaptive), T=1.",
            "Not like-for-like: the RS driver runs LCPC's Ft127 field (ff-derived arithmetic) while Brakedown runs the "
            "fork's Mont127 (same prime, custom lazy arithmetic), and the matrix shapes and code rates differ (see the "
            "k x n columns). The ratio therefore mixes field arithmetic, shape and rate with the code family.",
        ]
        self._write_artefact("table_T10_brakedown_vs_rs", "Brakedown vs. Reed-Solomon (Ligero) Encoding in the Same Library",
                             "tab:brakedown_vs_rs", headers, rows, notes, key_cols=1)

    # ---- T11 --------------------------------------------------------------
    CODEGEN_FAMILIES = ("tw_a", "tw_b", "tw_ft127", "arm_this_work", "slice_sweep", "tw_threads", "ligero")
    PASS_STATUSES = ("PASS", "PASS on re-check")

    def _recheck(self, report: Dict) -> Optional[Dict[str, Any]]:
        cache = self.__dict__.setdefault("_recheck_cache", {})
        if id(report) not in cache:
            cache[id(report)] = recheck_codegen_report(report)
        return cache[id(report)]

    def effective_codegen_status(self, report: Dict) -> Tuple[str, str, Optional[Dict[str, Any]]]:
        """(own status, effective status, re-check) of one codegen report.

        A FAIL written by an older classifier counts as "PASS on re-check" when
        the current check_codegen.py, applied to the report's complete stored
        hot-kernel disassembly, finds no vector arithmetic. Anything else keeps
        the report's own status; a report without matched hot kernels is
        NO_HOT_KERNELS (not scalar evidence).
        """
        summary = (report.get("summary") or {}).get("hot_kernels") or {}
        vec = int(summary.get("vector_arithmetic_count", report.get("vector_arithmetic_count", 0)) or 0)
        own = str(report.get("status") or ("PASS" if vec == 0 else "FAIL"))
        if own == "PASS" and not report.get("hot_kernels_checked"):
            own = "NO_HOT_KERNELS"
        recheck = self._recheck(report) if own == "FAIL" else None
        effective = own
        if (own == "FAIL" and recheck and recheck["complete"] and recheck["arith"] == 0
                and report.get("classifier_version") != recheck["classifier"]):
            effective = "PASS on re-check"
        return own, effective, recheck

    def codegen_groups(self) -> List[Dict[str, Any]]:
        """Selected codegen reports of the measured This Work binaries, grouped per (arch, fork commit, summary).

        Status comes from each report ("PASS", "FAIL", "NO_HOT_KERNELS"),
        re-checked with the current classifier (effective_codegen_status);
        vector arithmetic and vector moves (zeroing/copy data movement) are
        counted separately, as check_codegen.py reports them (interface c).
        """
        groups: Dict[Tuple, List[Dict]] = defaultdict(list)
        for cr in self.codegen_reports:
            m = cr["_meta"]
            if self.selection and m["family"] not in self.CODEGEN_FAMILIES:
                continue
            fork = "/".join(sorted(self.session_info.get((m["machine"], m["run"], m["session_dir"]), {}).get("fork", set()))) or "?"
            summary = (cr.get("summary") or {}).get("hot_kernels") or {}
            groups[(str(cr.get("arch", "x86_64")), fork, json.dumps(summary, sort_keys=True))].append(cr)
        out = []
        seen_keys: Dict[Tuple[str, str], int] = defaultdict(int)
        for (arch, fork, _), reports in sorted(groups.items()):
            s = (reports[0].get("summary") or {}).get("hot_kernels") or {}
            vec = int(s.get("vector_arithmetic_count", 0) or 0)
            per = [self.effective_codegen_status(r) for r in reports]
            statuses = sorted({own for own, _, _ in per})
            effective = sorted({eff for _, eff, _ in per})
            seen_keys[(arch, fork)] += 1
            label = f"{arch} @ {fork}" + (f" #{seen_keys[(arch, fork)]}" if seen_keys[(arch, fork)] > 1 else "")
            key = f"{label},hot_kernels (aggregate)"
            out.append({"arch": arch, "fork": fork, "label": label, "reports": reports, "summary": s, "vec_arith": vec,
                        "vec_moves": int(s.get("vector_moves_count", 0) or 0), "statuses": statuses,
                        "effective": effective, "recheck": next((rc for _, _, rc in per if rc), None),
                        "selector": f"T11|{key}|Scalar Status",
                        "ok": all(e in self.PASS_STATUSES for e in effective)})
        return out

    @staticmethod
    def recheck_codegen_hits(report: Dict) -> Optional[Dict[str, int]]:
        """Re-classifies a report's stored vector-arithmetic hits with the current check_codegen.py rules."""
        rc = recheck_codegen_report(report)
        if rc is None:
            return None
        counts: Dict[str, int] = defaultdict(int)
        for kind, n in rc["hits_reclassified"].items():
            counts["arith" if kind == "VEC_ARITH" else ("moves" if kind == "VEC_MOVE" else "scalar")] += n
        return dict(counts)

    def step_codegen_summary(self) -> Dict[str, Dict[str, Any]]:
        """Per step tag: own/effective statuses and hot kernels checked of the steps blocks' codegen reports."""
        out: Dict[str, Dict[str, Any]] = {}
        for cr in self.step_codegen_reports:
            tag = cr["_meta"]["tag"]
            own, eff, _ = self.effective_codegen_status(cr)
            info = out.setdefault(tag, {"own": set(), "effective": set(), "checked": set(), "reports": 0})
            info["own"].add(own)
            info["effective"].add(eff)
            info["checked"].add(int(cr.get("hot_kernels_checked") or 0))
            info["reports"] += 1
        return out

    def gate_step_codegen(self) -> List[str]:
        """Step-tag codegen reports in the FAIL gate (interface c); T3 selectors document exceptions."""
        notes = []
        for tag, info in sorted(self.step_codegen_summary().items()):
            bad = sorted(e for e in info["effective"] if e not in self.PASS_STATUSES + ("NO_HOT_KERNELS",))
            selector = f"T3|codegen,{tag}|Scalar Status"
            if bad:
                om = self.selection.omission_for(selector) if self.selection else None
                if om:
                    self.todo_cells[selector] = f"{om['kind']}: {om['reason']}"
                else:
                    self.codegen_errors.append(f"{selector}: step codegen status {'/'.join(bad)} "
                                               f"({info['reports']} reports)")
            if "PASS on re-check" in info["effective"]:
                notes.append(f"{tag}: codegen report FAIL under an older classifier, PASS when its stored hot-kernel "
                             "disassembly is re-checked with the current check_codegen.py.")
        no_hot = sorted(t for t, i in self.step_codegen_summary().items()
                        if "NO_HOT_KERNELS" in i["effective"] or i["checked"] == {0})
        if no_hot:
            notes.append("Codegen check matched no hot-kernel symbol at " + ", ".join(no_hot) + " (NO_HOT_KERNELS: "
                         "check_codegen.py's hot-kernel patterns name functions introduced by later steps), so the "
                         "scalar status of those tags is not established by the check; the steps harness runs the "
                         "check non-strictly for the rowwise/batched API stages.")
        return notes

    def generate_T11_codegen_evidence(self):
        """codegen evidence of the measured binaries, one row per (arch, fork commit) build."""
        self._begin("T11")
        T = "T11"
        headers = ["Architecture", "Hot Kernel / Symbol", "Reports", "Total Insns", "mul / mulx", "adc / adcx",
                   "Vector Arith", "Vector Moves", "Re-check (current classifier)", "Scalar Status"]
        rows, notes_extra = [], []
        for g in self.codegen_groups():
            cr = g["reports"][0]
            s = g["summary"]
            for r in g["reports"]:
                m = r["_meta"]
                self._used.add((m["machine"], m["run"], m["session_dir"]))
            status = "/".join(g["statuses"])
            rc = g["recheck"]
            if g["effective"] != g["statuses"]:
                status = f"{status} in report; " + "/".join(g["effective"])
            if not g["ok"]:
                om = self.selection.omission_for(g["selector"]) if self.selection else None
                if om:
                    self.todo_cells[g["selector"]] = f"{om['kind']}: {om['reason']}"
                    status += " (documented)"
                    notes_extra.append(f"{g['label']}: status {'/'.join(g['effective'])} documented in "
                                       f"selection.json ({om['kind']}): {om['reason']}")
                else:
                    self.codegen_errors.append(f"{g['selector']}: codegen status {'/'.join(g['effective'])} "
                                               f"({g['vec_arith']} vector arithmetic instructions in hot kernels)")
            if rc is None and g["vec_arith"]:
                rc = self._recheck(cr)
            if not g["vec_arith"]:
                recheck_txt = "N/A (no stored hits)"
            elif rc is None:
                recheck_txt = "N/A (check_codegen.py not importable)"
            else:
                recheck_txt = (f"{rc['arith']} arith, {rc['moves']} moves in {rc['lines']} stored lines"
                               + ("" if rc["complete"] else " (incomplete listing)"))
                if "PASS on re-check" in g["effective"]:
                    kinds = ", ".join(f"{n} {k}" for k, n in sorted(rc["hits_reclassified"].items()))
                    ex = "; ".join(f"'{e}'" for e in rc["examples"])
                    notes_extra.append(
                        f"{g['label']}: the reports (classifier {cr.get('classifier_version') or 'v1, unversioned'}) "
                        f"count {g['vec_arith']} hot-kernel instructions as vector arithmetic; {rc['classifier']} "
                        f"re-classifies them as {kinds}" + (f", e.g. {ex}" if ex else "")
                        + f", and finds {rc['arith']} vector arithmetic and {rc['moves']} vector moves in all "
                        f"{rc['lines']} stored hot-kernel instructions.")
            rows.append([g["label"], "hot_kernels (aggregate)", len(g["reports"]),
                         s.get("total_instructions", 0), s.get("scalar_mulx_count", 0) + s.get("scalar_mul_count", 0),
                         s.get("scalar_adc_count", 0), g["vec_arith"], g["vec_moves"], recheck_txt, status])
            hot = [sym for sym in cr.get("symbols", []) if sym.get("is_hot_kernel")]
            names = [str(sym.get("symbol_demangled") or sym.get("symbol_raw") or "unknown") for sym in hot]
            shorts = [short_symbol(n) for n in names]
            for sym, name, short in zip(hot, names, shorts):
                st = sym.get("scalar_stats", {})
                label = short if shorts.count(short) == 1 else name
                rows.append([g["label"], label, 1,
                             st.get("total", len(sym.get("disassembly_lines", []))),
                             st.get("mul", 0) + st.get("mulx", 0), st.get("adc", 0),
                             len(sym.get("vector_arithmetic", [])), len(sym.get("vector_moves", [])),
                             "N/A (no stored hits)" if not sym.get("vector_arithmetic") else "see aggregate row",
                             sym.get("status", "PASS") + ("; PASS on re-check" if sym.get("status") == "FAIL"
                                                          and "PASS on re-check" in g["effective"] else "")])
        if not rows:
            rows.append(["-", "-", 0, "-", "-", "-", "-", "-", "-", self.missing_text(f"{T}|-,-|Scalar Status")])
        notes = [
            "Disassembly inspection by scripts/check_codegen.py of the benchmark binaries; identical reports from different "
            "blocks of the same build are counted in 'Reports'. Vector Arith = vector arithmetic in the hot kernels (a "
            "failure); Vector Moves = vector-register data movement used for zeroing/copying (reported, not a failure).",
            "Scalar Status is each report's own verdict; Re-check re-classifies the complete stored hot-kernel "
            "disassembly of FAIL reports with the check_codegen.py in this checkout, and a FAIL written by an older "
            "classifier whose re-check finds no vector arithmetic is shown as 'PASS on re-check'.",
            "Symbol names are shown without module paths (full names where that would be ambiguous).",
            "The fork's .cargo/config.toml builds with -C no-vectorize-loops -C no-vectorize-slp; a FAIL row means the "
            "check did not confirm scalar hot kernels for that build, so its code must not be described as scalar until "
            "re-checked.",
        ] + notes_extra
        self._write_artefact("table_T11_codegen_evidence", "Compiler Codegen Inspection and Scalar Disassembly Verification",
                             "tab:codegen_evidence", headers, rows, notes, col_align="llrrrrrrll", key_cols=2)

    # ---- T12 --------------------------------------------------------------
    def generate_T12_arm_portability(self):
        """Arm (SoC xcng) This Work vs original LCPC per build variant."""
        self._begin("T12")
        T = "T12"
        headers = ["log2(N)", "Threads", "This Work Arm (ms)"]
        for v in PRIMARY_LCPC_VARIANTS:
            headers += [f"Original LCPC {v} Arm (ms)", f"Arm Speedup vs {v}"]
        tw_pool = self.select(self.reports_arm, impl="this_work", params="lcpc-sdig6", shape="brakedown", slice_mode="adaptive")
        lcpc_pool = self.select(self.reports_arm, impl="lcpc_orig", params="lcpc-sdig6")
        arm_sizes = sorted({int(r["log_n"]) for r in tw_pool + lcpc_pool}) or [18, 22, 26]
        no_arm = self.has_smoke_watermark and not self.reports_arm
        rows = []
        for log_n in arm_sizes:
            for threads in (1, 8):
                rk = f"{log_n},{threads}"
                if no_arm:
                    rows.append([log_n, threads] + ["N/A (local smoke)"] * (len(headers) - 2))
                    continue
                tw = self.stat(self.select(tw_pool, log_n=log_n, threads=threads), label=f"{T}|{rk}|TW")
                row = [log_n, threads, self.value_cell(T, rk, "This Work Arm (ms)", tw)]
                for v in PRIMARY_LCPC_VARIANTS:
                    st = self.stat(self.select(lcpc_pool, variant=v, log_n=log_n, threads=threads), label=f"{T}|{rk}|LCPC {v}")
                    row += [self.value_cell(T, rk, f"Original LCPC {v} Arm (ms)", st),
                            self.speedup_cell(T, rk, f"Arm Speedup vs {v}", st, tw)]
                rows.append(row)
        if no_arm:
            notes = ["Local smoke does not run on Arm; these cells are explicitly not applicable and contain no Arm measurements."]
        else:
            notes = [
                "SoC cluster xcng (Neoverse-N1), mode (a); This Work: arm_this_work_* (adaptive slices); original LCPC: arm_lcpc_orig_* "
                "over Ft127, one column group per build variant.",
            ]
            notes += self.rustflags_notes(self._used_records)
            notes += self.arm_flags_notes(self._used_records)
            pending = sorted({sel for sel in self.todo_cells if sel.startswith(f"{T}|")})
            if pending:
                reasons = sorted({self.todo_cells[sel] for sel in pending})
                notes.append("TODO (pending) cells: " + " | ".join(reasons))
        self._write_artefact("table_T12_arm_portability", "Arm Architecture Portability Benchmark (SoC xcng, Neoverse-N1)",
                             "tab:arm_portability", headers, rows, notes, key_cols=2)

    # ---- Figures -------------------------------------------------------------
    def _save_figure(self, fig, name: str, data_headers: List[str], data_rows: List[List[Any]]):
        if self.is_smoke or self.has_smoke_watermark:
            fig.text(0.5, 0.5, "LOCAL SMOKE - NOT A RESULT", fontsize=20, color="red", alpha=0.25,
                     ha="center", va="center", rotation=25)
        fig.tight_layout()
        pdf_path = os.path.join(self.plots_dir, f"{name}.pdf")
        png_path = os.path.join(self.plots_dir, f"{name}.png")
        fig.savefig(pdf_path)
        fig.savefig(png_path, dpi=300)
        plt.close(fig)
        self._write_plain_csv(f"{name}_data", data_headers, data_rows, self._get_provenance_text())
        print(f"[INFO] Wrote figure: {pdf_path} and {png_path}")

    def generate_F1_steps_breakdown(self):
        self._begin("F1")
        fig, ax = plt.subplots(figsize=(8.5, 4.8))
        data = []
        width = 0.38
        for i, mode in enumerate(("a", "b")):
            tags, values, fields, _, sizes = self._steps_rows(mode)
            if not sizes:
                continue
            n = 20 if 20 in sizes else max(sizes)
            ys = [((values[t].get(n) or {}).get("mean") or 0.0) / 1e6 for t in tags]
            xs = [x + (i - 0.5) * width for x in range(len(tags))]
            ax.bar(xs, ys, width=width, label=f"mode ({mode}) N=2^{n}")
            data += [[mode, t, n, "/".join(sorted(fields[t])), f"{y:.3f}"] for t, y in zip(tags, ys)]
            ax.set_xticks(range(len(tags)))
            ax.set_xticklabels([t.replace("step/", "") for t in tags], rotation=35, ha="right", fontsize=9)
        ax.set_ylabel("Encode latency (ms, mean of session medians)")
        ax.set_title("Per-technique latency across tagged commits (T=1)")
        ax.grid(True, linestyle=":", alpha=0.5, axis="y")
        if data:
            ax.legend()
        else:
            ax.text(0.5, 0.5, "Steps data pending", ha="center", va="center", color="gray")
        self._save_figure(fig, "figure_F1_steps_breakdown", ["Mode", "Tag", "log2(N)", "Field", "Latency (ms)"], data)

    def generate_F2_llc_scaling(self):
        self._begin("F2")
        fig, ax = plt.subplots(figsize=(7.5, 4.5))
        data = []
        series = [("Mont127 mode (a) adaptive", self.select(self.reports_this_work, family="tw_a", params="lcpc-sdig6",
                                                            shape="brakedown", slice_mode="adaptive", threads=1)),
                  ("Mont127 mode (a) s=1", self.select(self.reports_this_work, family="slice_sweep", params="lcpc-sdig6",
                                                       shape="brakedown", slices=1, threads=1))]
        for f in ("Mont127", "Mers127", "Fp2M61"):
            series.append((f"{f} mode (b) adaptive", self.select(self.reports_this_work, family="tw_b", params="simd-spielman",
                                                                 shape="brakedown", field=f, threads=1)))
        for name, pool in series:
            xs, ys = [], []
            for n in sorted({int(r["log_n"]) for r in pool}):
                st = self.stat(self.select(pool, log_n=n), label=f"F2|{name}|{n}")
                if st.get("mean"):
                    xs.append(n)
                    ys.append(st["mean"] / 1e6)
                    data.append([name, n, f"{st['mean'] / 1e6:.3f}", len(st["sessions"])])
            if xs:
                ax.plot(xs, ys, marker="o", linestyle="--" if "s=1" in name else "-", label=name)
        ax.set_xlabel(r"Polynomial size ($\log_2 N$)")
        ax.set_ylabel("Encoding latency (ms)")
        ax.set_yscale("log")
        ax.set_title("Scaling with N and the LLC effect (T=1)")
        ax.grid(True, which="both", linestyle=":", alpha=0.5)
        if data:
            ax.legend(fontsize=8)
        self._save_figure(fig, "figure_F2_llc_scaling", ["Series", "log2(N)", "Latency (ms)", "Sessions"], data)

    def generate_F3_thread_scaling(self):
        self._begin("F3")
        fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11, 4.5))
        data = []
        pools = [("This Work", self.select(self.reports_this_work, family="tw_threads", params="lcpc-sdig6", shape="brakedown"))]
        for v in PRIMARY_LCPC_VARIANTS:
            pools.append((f"LCPC {v}", self.select(self.reports_lcpc_orig, family="lcpc_threads", variant=v)))
        plotted = False
        for name, pool in pools:
            for n in sorted({int(r["log_n"]) for r in pool}):
                base = self.stat(self.select(pool, log_n=n, threads=1), label=f"F3|{name}|{n}|1")
                if not base.get("mean"):
                    continue
                ts, sp, ef = [], [], []
                for t in sorted({int(r.get("threads", 1)) for r in self.select(pool, log_n=n)}):
                    st = self.stat(self.select(pool, log_n=n, threads=t), label=f"F3|{name}|{n}|{t}")
                    if st.get("mean"):
                        ts.append(t)
                        sp.append(base["mean"] / st["mean"])
                        ef.append(base["mean"] / st["mean"] / t)
                        data.append([name, n, t, f"{st['mean'] / 1e6:.3f}", f"{sp[-1]:.2f}", f"{ef[-1]:.2f}",
                                     len(st["sessions"])])
                ax1.plot(ts, sp, marker="o", label=f"{name} 2^{n}")
                ax2.plot(ts, ef, marker="s", label=f"{name} 2^{n}")
                plotted = True
        if plotted:
            tmax = max(int(r[2]) for r in data)
            ax1.plot([1, tmax], [1, tmax], linestyle=":", color="gray", label="Ideal")
            ax1.legend(fontsize=7)
            ax2.legend(fontsize=7)
        else:
            ax1.text(0.5, 0.5, "Thread-scaling data pending", ha="center", va="center", color="gray")
        ax1.set_xlabel("Threads (T)")
        ax1.set_ylabel("Speedup S(T) = T(1)/T(T)")
        ax1.grid(True, linestyle=":", alpha=0.5)
        ax2.set_xlabel("Threads (T)")
        ax2.set_ylabel("Parallel efficiency S(T)/T")
        ax2.set_ylim(0.0, 1.1)
        ax2.grid(True, linestyle=":", alpha=0.5)
        self._save_figure(fig, "figure_F3_thread_scaling",
                          ["Series", "log2(N)", "T", "Latency (ms)", "Speedup", "Efficiency", "Sessions"], data)

    # ------------------------------------------------------------------
    # Smoke coverage (local dry run)
    # ------------------------------------------------------------------

    def _has_measurement(self, report: Dict, op_name: str) -> bool:
        return bool(self._extract_samples(report, op_name))

    def _has_case(self, reports: List[Dict], *, field: Optional[Set[str]] = None,
                  params: Optional[str] = None, shape: Optional[str] = None,
                  log_n: Optional[int] = None, threads: Optional[int] = None,
                  slices: Optional[int] = None, operation: Optional[str] = "encode_matrix") -> bool:
        for report in reports:
            if field is not None and report.get("field", "Mont127") not in field:
                continue
            if params is not None and report.get("params") != params:
                continue
            if shape is not None and report.get("shape", "brakedown") not in (shape, None):
                continue
            if log_n is not None and int(report.get("log_n", -1)) != log_n:
                continue
            if threads is not None and int(report.get("threads", 1)) != threads:
                continue
            if slices is not None and int(report.get("slices") or 1) != slices:
                continue
            if operation is not None and not self._has_measurement(report, operation):
                continue
            return True
        return False

    def _check_smoke_coverage(self) -> List[str]:
        """Checks the representative smoke matrix, not the full paper campaign."""
        errors = []

        def require(condition: bool, label: str):
            if not condition:
                errors.append(f"Smoke coverage missing: {label}")

        require(self._has_case(self.reports_this_work, field={"Mont127"}, params="lcpc-sdig6", shape="brakedown",
                               log_n=20, threads=1), "T1 This Work mode (a), Mont127, logN=20, T=1")
        require(self._has_case(self.reports_lcpc_orig, field={"Ft127", "Mont127"}, params="lcpc-sdig6", shape="brakedown",
                               log_n=20, threads=1), "T1 original LCPC mode (a), logN=20, T=1")
        require(self._has_case(self.reports_polycommit, field={"Mont127"}, log_n=20, threads=1),
                "T1 PolyCommit Mont127 logN=20, T=1")
        require(any(r.get("patched") is True and len(str(r.get("patch_sha256", ""))) == 64 and int(r.get("sec_param", 0)) > 0
                    for r in self.reports_polycommit if int(r.get("log_n", -1)) == 20 and int(r.get("threads", 1)) == 1),
                "T1 PolyCommit patched-source hash and opening sec_param")
        require(self._has_case(self.reports_this_work, field={"Mont127"}, params="simd-spielman", shape="brakedown",
                               log_n=20, threads=1), "T2 This Work mode (b), Mont127, logN=20, T=1")
        require(self._has_case(self.reports_simd_spielman, field={"Mont127"}, params="simd-spielman", shape="brakedown",
                               log_n=20, threads=1), "T2 SIMD-Spielman, Mont127, logN=20, T=1")
        step_tags = {tag for tag, reports in self.reports_steps.items()
                     if any(r.get("mode", "a") == "a" and int(r.get("log_n", -1)) == 20 and self._extract_samples(r)
                            for r in reports)}
        require("step/01-modernize" in step_tags and "HEAD" in step_tags,
                "T3/F1 successful mode (a) step/01-modernize and HEAD records at logN=20")
        require(all(self._has_case(self.reports_this_work, field={"Mont127"}, params="lcpc-sdig6", shape="brakedown",
                                   log_n=20, threads=1, slices=s) for s in (1, 2)),
                "T4 two explicit slice counts (s=1 and s=2) at logN=20")
        micro_by_field = {str(r.get("field")): r for r in self.field_benchmarks}
        for field_name in ("Mont127", "Mers127", "Fp2M61", "Ft127"):
            micro = micro_by_field.get(field_name, {})
            require(all((safe_float((micro.get(op) or {}).get("ns_per_op")) or 0) > 0 for op in ("mul", "mac", "reduce")),
                    f"T5 {field_name} mul/mac/reduce microbenchmarks")
        for field_name in ("Mont127", "Mers127", "Fp2M61"):
            require(self._has_case(self.reports_this_work, field={field_name}, params="simd-spielman", shape="brakedown",
                                   log_n=20, threads=1), f"T5 {field_name} mode (b) encode sample at logN=20")
        pcs_ops = ("commit_total", "encode_matrix", "hash_matrix_decomposition", "prove", "verify")
        for label, reports, fields in (("This Work", self.reports_this_work, {"Mont127"}),
                                       ("original LCPC", self.reports_lcpc_orig, {"Ft127", "Mont127"})):
            matching = [r for r in reports if r.get("field", "Mont127") in fields and r.get("params") == "lcpc-sdig6"
                        and int(r.get("log_n", -1)) == 20 and int(r.get("threads", 1)) == 1]
            require(any(all(self._has_measurement(r, op) for op in pcs_ops) and (safe_float(r.get("proof_size_bytes")) or 0) > 0
                        for r in matching), f"T6 {label} complete PCS record at logN=20, T=1")
        for params in ("lcpc-sdig6", "simd-spielman"):
            matching = [r for r in self.reports_this_work if r.get("field") == "Mont127" and r.get("params") == params
                        and r.get("shape", "brakedown") == "brakedown" and int(r.get("log_n", -1)) == 20
                        and int(r.get("threads", 1)) == 1]
            require(any(self._has_measurement(r, "verifier_encode_1") and (self._has_measurement(r, "verifier_encode_2")
                                                                         or self._has_measurement(r, "verifier_encode"))
                        for r in matching), f"T7 This Work verifier v=1 and v=2, params={params}")
        require(self._has_case(self.reports_simd_spielman, field={"Mont127"}, params="simd-spielman", shape="brakedown",
                               log_n=20, threads=1, operation="verifier_encode_2"), "T7 SIMD-Spielman verifier v=2 at logN=20")
        require(self._has_case(self.reports_this_work, field={"Mont127"}, params="simd-spielman", shape="orion",
                               log_n=20, threads=1), "T9 This Work mode (b) Orion sample at logN=20")
        require(self._has_case(self.reports_simd_spielman, field={"Mont127"}, params="simd-spielman", shape="orion",
                               log_n=20, threads=1), "T9 SIMD-Spielman Orion sample at logN=20")
        require(self._has_case(self.reports_ligero_rs, log_n=20, threads=1), "T10 Ligero RS encode sample at logN=20")
        require(any(cr.get("status") == "PASS" and cr.get("hot_kernels_checked") for cr in self.codegen_reports),
                "T11 passing hot-kernel codegen report")
        for label, reports, fields in (("This Work", self.reports_this_work, {"Mont127"}),
                                       ("original LCPC", self.reports_lcpc_orig, {"Ft127", "Mont127"})):
            require(any(r.get("field", "Mont127") in fields and r.get("params") == "lcpc-sdig6"
                        and int(r.get("log_n", -1)) == 20 and int(r.get("threads", 1)) == 1
                        and self._has_measurement(r, "encode_matrix")
                        and r.get("setup_ns") is not None and r.get("peak_rss_bytes") is not None for r in reports),
                    f"T8 {label} setup, encoding, and peak-RSS record at logN=20")
        tw_threads = {int(r.get("threads", 1)) for r in self.reports_this_work if r.get("field") == "Mont127"
                      and r.get("params") == "lcpc-sdig6" and int(r.get("log_n", -1)) == 20}
        lcpc_threads = {int(r.get("threads", 1)) for r in self.reports_lcpc_orig if r.get("params") == "lcpc-sdig6"
                        and int(r.get("log_n", -1)) == 20}
        require(1 in tw_threads and len(tw_threads) > 1 and 1 in lcpc_threads and len(lcpc_threads) > 1,
                "F3 This Work and original LCPC at T=1 and a multithreaded configuration")
        return errors

    # ------------------------------------------------------------------
    # --check
    # ------------------------------------------------------------------

    def _check_table_cells(self, allow_patterns: List[str]) -> List[str]:
        errors = []
        for path in sorted(glob.glob(os.path.join(self.csv_dir, "table_T*_*.csv"))):
            tab_id = os.path.basename(path).split("_")[1]
            with open(path, "r", newline="", encoding="utf-8") as source:
                records = list(csv.reader(source))
            key_cols = 1
            for row in records:
                if row and row[0].startswith("# Key columns:"):
                    key_cols = int(row[0].split(":")[1])
            table_rows = [(i, row) for i, row in enumerate(records, start=1)
                          if row and any(c.strip() for c in row) and not row[0].lstrip().startswith("#")]
            if not table_rows:
                continue
            _, headers = table_rows[0]
            for line_number, row in table_rows[1:]:
                if len(row) > len(headers):
                    errors.append(f"Malformed artefact row: {tab_id} {path}:{line_number} has {len(row)} cells, expected {len(headers)}")
                full_key = ",".join(c.strip() for c in row[:key_cols])
                first_key = row[0].strip() if row else ""
                for ci, header in enumerate(headers):
                    value = row[ci] if ci < len(row) else ""
                    reason = cell_missing_reason(value, header, row)
                    if reason is None:
                        continue
                    sel_full = f"{tab_id}|{full_key}|{header.strip()}"
                    sel_first = f"{tab_id}|{first_key}|{header.strip()}"
                    hit = matching_patterns((sel_full, sel_first), allow_patterns)
                    if hit:
                        self.matched_allow.update(hit)
                        continue
                    errors.append(f"Missing artefact cell {sel_full}={value!r} ({reason}) at {path}:{line_number}")
        return errors

    def _check_expected_rows(self, allow_patterns: List[str]) -> List[str]:
        """selection.json expected_rows: every listed key combination must be a row of its table."""
        if not self.selection or not self.selection.expected_rows:
            return []
        errors = []
        for table, specs in sorted(self.selection.expected_rows.items()):
            paths = sorted(glob.glob(os.path.join(self.csv_dir, f"table_{table}_*.csv")))
            if not paths:
                errors.append(f"Expected rows for {table}: table not generated")
                continue
            with open(paths[0], "r", newline="", encoding="utf-8") as f:
                data = [r for r in csv.reader(f) if r and not r[0].lstrip().startswith("#")]
            if not data:
                continue
            headers = [h.strip() for h in data[0]]
            for spec in specs:
                errors.extend(self._check_expected_spec(table, spec, headers, data, paths[0], allow_patterns))
        return errors

    def _check_expected_spec(self, table: str, spec: Dict[str, List[Any]], headers: List[str],
                             data: List[List[str]], path: str, allow_patterns: List[str]) -> List[str]:
        import itertools
        errors = []
        cols = list(spec)
        missing_cols = [c for c in cols if c not in headers]
        if missing_cols:
            return [f"Expected rows for {table}: no column(s) {missing_cols} in {path}"]
        idx = [headers.index(c) for c in cols]
        present = {tuple(r[i].strip() if i < len(r) else "" for i in idx) for r in data[1:]}
        for combo in itertools.product(*(spec[c] for c in cols)):
            key = tuple(str(v) for v in combo)
            if key in present:
                continue
            selector = f"{table}|{','.join(key)}|(row)"
            om = self.selection.omission_for(selector)
            if om:
                self.todo_cells[selector] = f"{om['kind']}: {om['reason']} (row absent)"
                continue
            hit = matching_patterns((selector,), allow_patterns)
            if hit:
                self.matched_allow.update(hit)
                continue
            errors.append(f"Missing expected row {selector} ({', '.join(cols)} = {', '.join(key)}) in {path}")
        return errors

    def warn_unmatched_allow(self, allow_patterns: List[str]) -> List[str]:
        """Warns about --allow selectors that excused nothing (stale or mistyped, like omissions)."""
        unmatched = [p for p in dict.fromkeys(allow_patterns) if p not in self.matched_allow]
        for pattern in unmatched:
            print(f"[WARN] --allow selector matched no missing cell (stale or mistyped?): {pattern}")
        return unmatched

    def warn_unmatched_omissions(self) -> List[str]:
        """Warns about selection.json omission patterns that matched no cell (stale or mistyped)."""
        if not self.selection:
            return []
        unmatched = self.selection.unmatched_omission_patterns()
        for pattern in unmatched:
            print(f"[WARN] selection.json omission pattern matched no cell (data landed or pattern stale?): {pattern}")
        return unmatched

    def check_latex_compiles(self) -> List[str]:
        """Compiles every generated LaTeX table with pdflatex (skipped with a warning if not installed)."""
        tex_paths = sorted(glob.glob(os.path.join(self.latex_dir, "table_T*_*.tex")))
        build_dir = self.latex_build_dir or os.path.join(self.out_dir, "latex_build")
        try:
            compiled, errors = compile_latex_tables(tex_paths, build_dir)
        except FileNotFoundError:
            print("[WARN] pdflatex not found: generated LaTeX tables were not compile-checked.")
            return []
        print(f"[INFO] pdflatex compiled {len(compiled)} of {len(tex_paths)} tables in {build_dir}.")
        return [f"LaTeX table does not compile: {e}" for e in errors]

    def check_all_artefacts(self, allow_patterns: Optional[List[str]] = None) -> bool:
        allow = list(allow_patterns or [])
        omission_patterns = self.selection.omission_patterns() if self.selection else []
        errors = []
        for i in range(13):
            prefix = f"table_T{i}"
            tex = glob.glob(os.path.join(self.latex_dir, f"{prefix}_*.tex"))
            csvs = glob.glob(os.path.join(self.csv_dir, f"{prefix}_*.csv"))
            if not tex or os.path.getsize(tex[0]) == 0:
                errors.append(f"Missing or empty LaTeX table for {prefix} in {self.latex_dir}")
            if not csvs:
                errors.append(f"Missing CSV table for {prefix} in {self.csv_dir}")
            else:
                with open(csvs[0], "r", encoding="utf-8") as f:
                    lines = [l for l in f if l.strip() and not l.startswith("#")]
                if len(lines) < 2:
                    errors.append(f"CSV table has no data rows: {csvs[0]}")
        errors.extend(self._check_table_cells(allow + omission_patterns))
        errors.extend(self._check_expected_rows(allow))
        # Missing values (fewer sessions than expected) may be documented;
        # integrity errors never are (interface d).
        for selector, message in self.missing_cell_errors:
            hit = matching_patterns((selector,), allow)
            self.matched_allow.update(hit)
            if hit or selector_matches(selector, omission_patterns):
                continue
            errors.append(f"Cell {selector}: {message}")
        errors.extend(f"Cell {selector}: integrity error (not excusable by omissions): {message}"
                      for selector, message in self.cell_errors)
        errors.extend(f"Interrupted block (partial output of a run that never completed; re-run it or exclude it): "
                      f"{block} ({count} records)" for block, count in sorted(self.interrupted_block_records.items()))
        errors.extend(f"Oversubscribed run: {e}" for e in self.pinning_errors)
        errors.extend(f"Codegen report not PASS and not documented in selection.json omissions: {e}"
                      for e in self.codegen_errors)
        if self.compile_latex:
            errors.extend(self.check_latex_compiles())
        for name in ("figure_F1_steps_breakdown", "figure_F2_llc_scaling", "figure_F3_thread_scaling"):
            for ext in ("pdf", "png"):
                p = os.path.join(self.plots_dir, f"{name}.{ext}")
                if not os.path.isfile(p) or os.path.getsize(p) == 0:
                    errors.append(f"Missing or empty figure: {p}")

        smoke = self.has_smoke_watermark or self.profile == "smoke"
        if smoke:
            errors.extend(self._check_smoke_coverage())
        elif not self.selection:
            errors.append("Paper artefacts need a result selection: run with --selection (default results/selection.json) "
                          "instead of --results-dir alone.")

        if self.todo_cells:
            print(f"[INFO] {len(self.todo_cells)} cells are pending or documented omissions (csv/todo_cells.csv):")
            for selector, status in sorted(self.todo_cells.items()):
                print(f"  - {selector}: {status}")
        self.warn_unmatched_omissions()
        self.warn_unmatched_allow(allow)
        if errors:
            print("[FAIL] Artefact validation failed with errors:")
            for err in errors:
                print(f"  - {err}")
            return False
        print("[PASS] All required evaluation artefacts (T0-T12, F1-F3) verified.")
        return True


def main():
    parser = argparse.ArgumentParser(description="Analyze benchmark results and generate paper evaluation artefacts.")
    parser.add_argument("results_dir_positional", nargs="?",
                        help="Result directory (positional form; also accepts --results-dir)")
    parser.add_argument("--results-dir", nargs="+", dest="results_dir_option",
                        help="Directories to scan. Without --selection every record found is analyzed (not paper evidence).")
    parser.add_argument("--selection", default=None,
                        help=f"Result selection file (default when no --results-dir is given: {DEFAULT_SELECTION})")
    parser.add_argument("--out", "--out-dir", dest="out_dir", default="./analysis",
                        help="Output directory for generated artefacts")
    parser.add_argument("--kdhr26-json", default=os.path.join(SCRIPT_DIR, "kdhr26_reported.json"),
                        help="Path to committed kdhr26_reported.json citation data")
    parser.add_argument("--smoke", action="store_true", help="Mark outputs with LOCAL SMOKE watermark")
    parser.add_argument("--check", action="store_true", help="Fail if any required artefact or cell is missing")
    parser.add_argument("--allow", action="append", type=parse_allow_pattern, default=[],
                        help="Allow a documented missing cell selector TABLE|ROW|COLUMN (ROW = the key columns joined "
                             "by commas, or the first column); repeat as needed")
    parser.add_argument("--latex-build-dir", default=None,
                        help="Where --check compiles each generated LaTeX table with pdflatex (default <out>/latex_build)")
    parser.add_argument("--no-latex-compile", action="store_true",
                        help="With --check, do not compile the generated LaTeX tables (default: compile if pdflatex exists)")
    parser.add_argument("--self-test", action="store_true", help="Run the built-in self-test and exit")
    args = parser.parse_args()

    if args.self_test:
        run_self_test()
        return

    results_dirs = args.results_dir_option or ([args.results_dir_positional] if args.results_dir_positional else [])
    selection_path = args.selection or (None if results_dirs else DEFAULT_SELECTION)
    selection = None
    if selection_path:
        try:
            selection = Selection(selection_path)
        except (OSError, ValueError, KeyError) as e:
            parser.error(f"cannot load selection {selection_path}: {e}")
        if not results_dirs:
            results_dirs = [selection.root]
        print(f"[INFO] Using selection {selection.path} (sha256 {selection.sha256[:12]}); results root {selection.root}")
    elif not results_dirs:
        parser.error("provide a result directory or a selection")
    else:
        print("[WARN] No --selection: analyzing every record under " + ", ".join(results_dirs)
              + " (per-session / smoke output, not paper evidence).")

    analyzer = BenchmarkAnalyzer(args.out_dir, args.kdhr26_json, is_smoke=args.smoke, selection=selection,
                                 compile_latex=not args.no_latex_compile, latex_build_dir=args.latex_build_dir)
    analyzer.load_results(results_dirs)
    analyzer.generate_all()

    if args.check:
        if not analyzer.check_all_artefacts(args.allow):
            sys.exit(1)
    else:
        analyzer.warn_unmatched_omissions()
    print(f"[INFO] Analysis completed. Artefacts saved in {args.out_dir}")


if __name__ == "__main__":
    main()
