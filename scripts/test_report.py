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

"""Unit and regression tests for paper_tables.py, claim_times.py, report.sh, and README claims map."""

import csv
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
CODE_DIR = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPT_DIR))

import claim_times  # noqa: E402
import paper_tables  # noqa: E402


def write_csv_file(path, title, prov, header, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(f"# {title}\n")
        if prov:
            f.write(f"# Provenance: {prov}\n")
        w = csv.writer(f, lineterminator="\n")
        w.writerow(header)
        w.writerows(rows)


class TestReportPipeline(unittest.TestCase):
    def test_archived_matches_paper_reference(self):
        ref_dir = SCRIPT_DIR / "paper_reference"
        ref_derived = ref_dir / "derived.csv"
        ref_claims = ref_dir / "claims.csv"
        self.assertTrue(ref_derived.is_file(), f"Missing {ref_derived}")
        self.assertTrue(ref_claims.is_file(), f"Missing {ref_claims}")

        with tempfile.TemporaryDirectory(prefix="rbd-test-report-") as tmp:
            tmp_path = pathlib.Path(tmp)
            env = dict(os.environ)
            env["MPLCONFIGDIR"] = str(tmp_path / "mpl")
            ana_dir = tmp_path / "analysis"
            pap_dir = tmp_path / "paper"
            claims_out = tmp_path / "claims.csv"

            res_ana = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_DIR / "analyze.py"),
                    "--results-dir", str(CODE_DIR / "results"),
                    "--selection", str(CODE_DIR / "results" / "selection.json"),
                    "--out-dir", str(ana_dir),
                    "--no-latex-compile",
                    "--check",
                ],
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(res_ana.returncode, 0, f"analyze.py failed:\n{res_ana.stdout}\n{res_ana.stderr}")

            res_pt = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT_DIR / "paper_tables.py"),
                    "--csv-dir", str(ana_dir / "csv"),
                    "--results-dir", str(CODE_DIR / "results"),
                    "--selection", str(CODE_DIR / "results" / "selection.json"),
                    "--out-dir", str(pap_dir),
                    "--claims-out", str(claims_out),
                    "--strict",
                ],
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(res_pt.returncode, 0, f"paper_tables.py failed:\n{res_pt.stdout}\n{res_pt.stderr}")

            self.assertEqual(
                (pap_dir / "derived.csv").read_text(encoding="utf-8"),
                ref_derived.read_text(encoding="utf-8"),
                "Regenerated derived.csv differs from scripts/paper_reference/derived.csv",
            )
            self.assertEqual(
                claims_out.read_text(encoding="utf-8"),
                ref_claims.read_text(encoding="utf-8"),
                "Regenerated claims.csv differs from scripts/paper_reference/claims.csv",
            )
            with open(claims_out, "r", encoding="utf-8", newline="") as f:
                rows = list(csv.DictReader(f))
            self.assertEqual(len(rows), 12)
            for r in rows:
                self.assertEqual(r["status"], "PASS", f"Expected PASS for {r['claim_id']}, got {r}")

            for stem in (
                "tab_platforms", "tab_headline", "tab_steps", "tab_simd", "tab_pcs",
                "tab_correctness", "tab_codegen", "tab_fields", "tab_slices", "tab_arm", "tab_setup",
            ):
                self.assertTrue((pap_dir / f"{stem}.tex").is_file(), f"Missing {stem}.tex")

    def test_lenient_partial_quick_fixture(self):
        with tempfile.TemporaryDirectory(prefix="rbd-test-lenient-") as tmp:
            tmp_path = pathlib.Path(tmp)
            csv_dir = tmp_path / "csv"
            res_dir = tmp_path / "results"
            out_dir = tmp_path / "paper"
            claims_out = tmp_path / "claims.csv"

            # Create minimal manifest so tab_platforms works for test-host.
            mf_dir = res_dir / "test-host" / "2026-11-21-abcdef123456" / "quick1" / "quick_this_work_mode_a_t1"
            mf_dir.mkdir(parents=True)
            (mf_dir / "manifest.json").write_text(
                json.dumps({
                    "cpu": {"model_name": "Test CPU", "lscpu": "Flags: avx2\n"},
                    "cache_topology": [{"level": 3, "type": "Unified", "size": "32M"}],
                    "memory": {"MemTotal": "65536000 kB"},
                    "pinning": {"affinity_cores_expanded": [0, 1, 2, 3]},
                    "frequency_turbo": {"scaling_governor": "performance"},
                    "rust_toolchain": {"rustc_verbose": {"release": "1.99.1", "LLVM version": "20.1.0"}},
                }),
                encoding="utf-8",
            )
            sel_path = res_dir / "selection.json"
            sel_path.write_text(
                json.dumps({
                    "schema_version": "rbd-selection-v1",
                    "mode": "quick",
                    "mt_threads": 4,
                    "machines": {
                        "test-host": {
                            "role": "x86_64",
                            "runs": [{"run_dir": "2026-11-21-*", "sessions": ["quick1"]}],
                        }
                    },
                }),
                encoding="utf-8",
            )

            prov = "test-host/2026-11-21-abcdef123456/quick1 (fork abcdef1)"
            write_csv_file(
                csv_dir / "table_T0_correctness.csv",
                "Table T0", prov,
                ["Gate / Invariant", "Evaluation Target", "Oracle / Reference", "Commit / Tag", "Status"],
                [["Bit-exact prover encoding (mode a)", "SdigCode6 (Mont127)", "Original LCPC (f0e2116)", "HEAD", "PASS (1 block)"]],
            )
            write_csv_file(
                csv_dir / "table_T1_headline_encoding.csv",
                "Table T1", prov,
                [
                    "log2(N)", "T", "Matrix Shape", "This Work (ms)",
                    "LCPC as-published-as-is (ms)", "Speedup vs LCPC as-published-as-is",
                    "Significance vs as-published-as-is",
                    "LCPC fork-flags-portable (ms)", "Speedup vs LCPC fork-flags-portable",
                    "Significance vs fork-flags-portable",
                    "PolyCommit (ms)", "Speedup vs PolyCommit", "Significance vs PolyCommit",
                    "TW ns/MAC",
                ],
                [[
                    "20", "1", "1024x1762 (both)", "50.00",
                    "350.00", "7.00x", "n/a",
                    "300.00", "6.00x", "n/a",
                    "200.00", "4.00x", "n/a",
                    "4.50",
                ]],
            )
            for stem in (
                "table_T2_simd_spielman", "table_T3_steps_breakdown", "table_T4_slice_sweep",
                "table_T5_field_comparison", "table_T6_pcs_breakdown", "table_T7_verifier_encoding",
                "table_T8_setup_memory", "table_T9_orion_shape", "table_T10_brakedown_vs_rs",
                "table_T11_codegen_evidence", "table_T12_arm_portability",
            ):
                write_csv_file(
                    csv_dir / f"{stem}.csv", stem, None,
                    ["Col1", "Col2"],
                    [["not run (claim not selected)", "not run (claim not selected)"]],
                )
            write_csv_file(csv_dir / "todo_cells.csv", "TODO", None, ["Cell", "Status"], [])
            write_csv_file(csv_dir / "session_divergence.csv", "DIV", None, ["Table", "Cell"], [])

            hard_ok = paper_tables.run_pipeline(
                csv_dir=csv_dir,
                results_dir=res_dir,
                selection_path=sel_path,
                out_dir=out_dir,
                strict=False,
                claims_out=claims_out,
            )
            self.assertTrue(hard_ok)
            hosts_tex = (out_dir / "hosts.tex").read_text(encoding="utf-8")
            self.assertIn(r"\renewcommand{\xmach}{\sw{test-host}\xspace}", hosts_tex)

            with open(claims_out, "r", encoding="utf-8", newline="") as f:
                by_id = {r["claim_id"]: r for r in csv.DictReader(f)}
            self.assertEqual(by_id["C0"]["status"], "PASS")
            self.assertEqual(by_id["C2"]["status"], "PASS")
            for cid in ("C1", "C3", "C4", "C5", "C6", "C7", "C8", "C9", "C10", "C11"):
                self.assertEqual(by_id[cid]["status"], "NOT-RUN", f"{cid}: {by_id[cid]}")
                self.assertIn("not run", by_id[cid]["observed"].lower())

            with open(out_dir / "paper_tables_status.csv", "r", encoding="utf-8", newline="") as f:
                st = {r["table"]: r["status"] for r in csv.DictReader(f)}
            self.assertEqual(st["tab_headline"], "generated")
            self.assertEqual(st["tab_correctness"], "generated")
            self.assertEqual(st["tab_simd"], "skipped")
            self.assertEqual(st["tab_arm"], "skipped")

    def test_hard_claim_failure_returns_false(self):
        with tempfile.TemporaryDirectory(prefix="rbd-test-hardfail-") as tmp:
            tmp_path = pathlib.Path(tmp)
            csv_dir = tmp_path / "csv"
            res_dir = tmp_path / "results"
            out_dir = tmp_path / "paper"
            sel_path = res_dir / "selection.json"
            res_dir.mkdir(parents=True)
            sel_path.write_text(json.dumps({"machines": {}}), encoding="utf-8")

            write_csv_file(
                csv_dir / "table_T0_correctness.csv", "T0", None,
                ["Gate / Invariant", "Evaluation Target", "Oracle / Reference", "Commit / Tag", "Status"],
                [["Bit-exact prover encoding (mode a)", "SdigCode6", "Original LCPC", "HEAD", "FAIL"]],
            )
            write_csv_file(
                csv_dir / "table_T11_codegen_evidence.csv", "T11", None,
                [
                    "Architecture", "Hot Kernel / Symbol", "Reports", "Total Insns",
                    "mul / mulx", "adc / adcx", "Vector Arith", "Vector Moves",
                    "Re-check (current classifier)", "Scalar Status",
                ],
                [["x86_64 @ deadbeef", "hot_kernels (aggregate)", "1", "100", "20", "10", "2", "0", "FAIL", "FAIL"]],
            )
            hard_ok = paper_tables.run_pipeline(
                csv_dir=csv_dir,
                results_dir=res_dir,
                selection_path=sel_path,
                out_dir=out_dir,
                strict=False,
            )
            self.assertFalse(hard_ok)
            with open(out_dir / "claims.csv", "r", encoding="utf-8", newline="") as f:
                by_id = {r["claim_id"]: r for r in csv.DictReader(f)}
            self.assertEqual(by_id["C0"]["status"], "FAIL")
            self.assertEqual(by_id["C1"]["status"], "FAIL")

    def test_claim_times_archived(self):
        rows, totals = claim_times.compute_claim_times(CODE_DIR / "results", CODE_DIR)
        self.assertEqual(len(rows), 12)
        for r in rows:
            self.assertEqual(r["quick_time"], "TODO(measure)")
        self.assertGreater(totals["x86_mean_seconds"], 30000)
        self.assertGreater(totals["arm_primary_seconds"], 3000)

    def test_readme_claims_table_matches_run_sh(self):
        readme = (CODE_DIR / "README.md").read_text(encoding="utf-8")
        begin_mark = "<!-- BEGIN CLAIMS TABLE -->"
        end_mark = "<!-- END CLAIMS TABLE -->"
        self.assertIn(begin_mark, readme, "README.md missing <!-- BEGIN CLAIMS TABLE -->")
        self.assertIn(end_mark, readme, "README.md missing <!-- END CLAIMS TABLE -->")
        start = readme.index(begin_mark) + len(begin_mark)
        end = readme.index(end_mark)
        readme_block = readme[start:end].strip()

        res = subprocess.run(
            ["sh", str(CODE_DIR / "reproduce" / "run.sh"), "--list-claims", "--markdown"],
            cwd=str(CODE_DIR),
            capture_output=True,
            text=True,
            check=True,
        )
        generated = res.stdout.strip()
        self.assertEqual(readme_block, generated, "README.md claims table is out of sync with reproduce/run.sh --list-claims --markdown")

    def test_make_selection_and_report_c0_and_c0_c4_pass(self):
        with tempfile.TemporaryDirectory(prefix="rbd-test-c0-") as tmp:
            tmp_path = pathlib.Path(tmp)
            env = dict(os.environ)
            env["MPLCONFIGDIR"] = str(tmp_path / "mpl")
            host = "test-x86"
            run_id = "2026-11-21-abcdef123456"
            sess = "quick1"
            out_root = tmp_path / "out"
            pf_path = out_root / host / "preflight.json"
            pf_path.parent.mkdir(parents=True)
            pf_path.write_text(json.dumps({
                "cpu_model": "Test x86 CPU",
                "dropped_stages": {},
                "claims": [
                    {"id": f"C{i}", "status": "runnable" if i != 11 else "skipped",
                     "reason": "" if i != 11 else "needs aarch64"}
                    for i in range(12)
                ],
            }), encoding="utf-8")
            tests_dir = out_root / host / "tests"
            tests_dir.mkdir(parents=True)
            (tests_dir / "test-stamp.json").write_text(json.dumps({
                "schema_version": "rbd-test-stamp-v1",
                "status": "passed",
                "key": "k",
                "commit": "abcdef123456",
                "rustc": "rustc 1.99.1",
                "host": host,
                "arch": "x86_64",
            }), encoding="utf-8")

            # 1) --claims C0 alone
            sel_c0 = out_root / "selection-c0.json"
            subprocess.run([
                sys.executable, str(SCRIPT_DIR / "make_selection.py"),
                "--results-root", str(out_root), "--host-label", host,
                "--run", run_id, "--session", sess, "--mode", "quick",
                "--arch", "x86_64", "--claims", "C0", "--preflight", str(pf_path),
                "--mt-threads", "4", "--out", str(sel_c0),
            ], check=True, capture_output=True, text=True)
            ana_c0 = tmp_path / "ana-c0"
            res = subprocess.run([
                sys.executable, str(SCRIPT_DIR / "analyze.py"),
                "--results-dir", str(out_root), "--selection", str(sel_c0),
                "--out-dir", str(ana_c0), "--no-latex-compile", "--check",
            ], env=env, capture_output=True, text=True, check=False)
            self.assertEqual(res.returncode, 0, f"C0 analyze --check failed:\n{res.stdout}\n{res.stderr}")
            pap_c0 = tmp_path / "pap-c0"
            claims_c0 = tmp_path / "claims-c0.csv"
            hard_ok = paper_tables.run_pipeline(
                csv_dir=ana_c0 / "csv", results_dir=out_root, selection_path=sel_c0,
                out_dir=pap_c0, strict=False, claims_out=claims_c0,
            )
            self.assertTrue(hard_ok)
            with open(claims_c0, "r", encoding="utf-8", newline="") as f:
                by_id = {r["claim_id"]: r for r in csv.DictReader(f)}
            self.assertEqual(by_id["C0"]["status"], "PASS")
            self.assertNotIn("claim not selected", by_id["C0"]["observed"])

            # 2) --claims C0,C4 with step tags
            sess_dir = out_root / host / run_id / sess
            sess_dir.mkdir(parents=True)
            (sess_dir / "schedule.json").write_text(json.dumps({
                "profile": "quick", "session_id": sess, "mt_threads": 4, "blocks": [],
            }), encoding="utf-8")
            step_tags = [
                "step/01-modernize", "step/02-fields", "step/03-column-major",
                "step/04-r2l", "step/05-lazy", "step/06-blocking",
                "step/07-slicing", "step/08-threads", "step/09-codegen", "HEAD",
            ]
            for idx, tag in enumerate(step_tags):
                ms_a = 500 - idx * 40
                tag_dir = sess_dir / "quick_steps_mode_a_n16-20" / "steps" / tag.replace("/", "_")
                tag_dir.mkdir(parents=True)
                for n in (16, 20):
                    (tag_dir / f"a_n{n}.json").write_text(json.dumps({
                        "schema_version": "steps-bench-v1", "tag": tag, "commit": "abcdef123456",
                        "mode": "a", "field": "Ft127" if idx == 0 else "Mont127",
                        "params": "lcpc-sdig6", "log_n": n, "threads": 1,
                        "session_id": sess, "samples_ns": [ms_a * 1_000_000] * 2,
                    }), encoding="utf-8")
                if idx >= 3:
                    ms_b = 300 - (idx - 3) * 20
                    tag_dir_b = sess_dir / "quick_steps_mode_b_n16-20" / "steps" / tag.replace("/", "_")
                    tag_dir_b.mkdir(parents=True)
                    for n in (16, 20):
                        (tag_dir_b / f"b_n{n}.json").write_text(json.dumps({
                            "schema_version": "steps-bench-v1", "tag": tag, "commit": "abcdef123456",
                            "mode": "b", "field": "Mers127", "params": "simd-spielman",
                            "log_n": n, "threads": 1, "session_id": sess,
                            "samples_ns": [ms_b * 1_000_000] * 2,
                        }), encoding="utf-8")

            sel_c04 = out_root / "selection-c04.json"
            subprocess.run([
                sys.executable, str(SCRIPT_DIR / "make_selection.py"),
                "--results-root", str(out_root), "--host-label", host,
                "--run", run_id, "--session", sess, "--mode", "quick",
                "--arch", "x86_64", "--claims", "C0,C4", "--preflight", str(pf_path),
                "--mt-threads", "4", "--out", str(sel_c04),
            ], check=True, capture_output=True, text=True)
            ana_c04 = tmp_path / "ana-c04"
            res = subprocess.run([
                sys.executable, str(SCRIPT_DIR / "analyze.py"),
                "--results-dir", str(out_root), "--selection", str(sel_c04),
                "--out-dir", str(ana_c04), "--no-latex-compile", "--check",
            ], env=env, capture_output=True, text=True, check=False)
            self.assertEqual(res.returncode, 0, f"C0,C4 analyze --check failed:\n{res.stdout}\n{res.stderr}")
            pap_c04 = tmp_path / "pap-c04"
            claims_c04 = tmp_path / "claims-c04.csv"
            hard_ok = paper_tables.run_pipeline(
                csv_dir=ana_c04 / "csv", results_dir=out_root, selection_path=sel_c04,
                out_dir=pap_c04, strict=False, claims_out=claims_c04,
            )
            self.assertTrue(hard_ok)
            with open(claims_c04, "r", encoding="utf-8", newline="") as f:
                by_id = {r["claim_id"]: r for r in csv.DictReader(f)}
            self.assertEqual(by_id["C0"]["status"], "PASS")
            self.assertEqual(by_id["C4"]["status"], "PASS")


if __name__ == "__main__":
    unittest.main()
