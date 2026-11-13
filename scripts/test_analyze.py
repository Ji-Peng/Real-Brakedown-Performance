#!/usr/bin/env python3
# Copyright 2026 RealBrakedownPerf Authors
# Licensed under Apache-2.0.

"""Unit tests for scripts/analyze.py (stdlib unittest; no benchmark data needed).

Run: python3 scripts/test_analyze.py
The fixtures build a miniature results tree (machine/run/session/block) with
synthetic records, so grouping, de-duplication, LCPC variant inference and the
selection rules are tested without touching code/results/.
"""

import contextlib
import csv
import io
import json
import lzma
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze  # noqa: E402

MACHINE = "soc-test"
RUN = "2026-01-01-abc"


def tw_record(session, log_n=20, threads=1, samples=(100, 101, 102, 103, 104), slice_mode="adaptive", slices=1,
              params="lcpc-sdig6", field="Mont127", shape="brakedown"):
    scale = 1_000_000
    return {"schema_version": "lcpc-bench-v1", "implementation": "lcpc-fork", "field": field, "params": params,
            "shape": shape, "log_n": log_n, "threads": threads, "slices": slices, "slice_mode": slice_mode,
            "adaptive_slices": 1, "session_id": session, "total_edge_row_macs": 1000,
            "dimensions": {"n_rows": 1 << (log_n // 2), "n_per_row": 1 << (log_n // 2), "n_cols": 1762},
            "measurements": {"encode_matrix": {"samples_ns": [s * scale for s in samples]}}}


def lcpc_record(session, samples, log_n=20, threads=1):
    return {"schema_version": "lcpc-bench-v1", "implementation": "lcpc-original-upstream", "field": "Ft127",
            "params": "lcpc-sdig6", "log_n": log_n, "threads": threads, "session_id": session,
            "dimensions": {"n_rows": 1024, "n_per_row": 1024, "n_cols": 1762},
            "measurements": {"encode_matrix": {"samples_ns": [s * 1_000_000 for s in samples]}}}


def simd_record(session, threads=1, log_n=20, samples=(50,) * 5, field="Mont127", shape="brakedown", status="ok",
                **extra):
    r = {"schema_version": "lcpc-bench-v1", "implementation": "simd-spielman", "field": field,
         "params": "simd-spielman", "shape": shape, "log_n": log_n, "threads": threads, "slices": 1,
         "slice_mode": "table", "session_id": session, "status": status,
         "measurements": {"encode_matrix": {"samples_ns": [x * 1_000_000 for x in samples]}}}
    if status != "ok":
        r["measurements"] = {}
    r.update(extra)
    return r


def codegen_report(status="PASS", arch="x86_64", arith=(), moves=()):
    return {"status": status, "arch": arch, "hot_kernels_checked": 1, "hot_kernel_violations": 1 if arith else 0,
            "summary": {"hot_kernels": {"vector_arithmetic_count": len(arith), "vector_moves_count": len(moves),
                                        "scalar_mul_count": 3, "scalar_mulx_count": 0, "scalar_adc_count": 2,
                                        "total_instructions": 40}},
            "symbols": [{"symbol_raw": "k", "symbol_demangled": "encode_column_block", "is_hot_kernel": True,
                         "status": "FAIL" if arith else "PASS",
                         "vector_arithmetic": [{"addr": "1", "insn": i} for i in arith],
                         "vector_moves": [{"addr": "2", "insn": i} for i in moves],
                         "scalar_stats": {"total": 40, "mul": 3, "mulx": 0, "adc": 2}}]}


class Fixture:
    """A temporary results root with helpers to add sessions, blocks and files."""

    def __init__(self):
        self.root = tempfile.mkdtemp(prefix="rbd-analyze-test-")
        self.out = os.path.join(self.root, "_out")

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def session(self, sid, machine=MACHINE, run=RUN):
        d = os.path.join(self.root, machine, run, sid)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "schedule.json"), "w") as f:
            json.dump({"profile": "full", "session_id": sid, "blocks": []}, f)
        return d

    def write(self, sid, block, relpath, data, machine=MACHINE, run=RUN):
        d = os.path.join(self.session(sid, machine, run), block, os.path.dirname(relpath))
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, os.path.basename(relpath)), "w") as f:
            json.dump(data, f)

    def selection(self, exclude=(), omissions=(), sessions=("s1", "s2"), expected=("s1", "s2")):
        path = os.path.join(self.root, "selection.json")
        with open(path, "w") as f:
            json.dump({"schema_version": "rbd-selection-v1", "results_root": ".",
                       "machines": {MACHINE: {"expected_sessions": list(expected), "kdhr26_reference_cpu": "EPYC 9754",
                                              "runs": [{"run_dir": RUN, "sessions": list(sessions)}]}},
                       "exclude": list(exclude), "omissions": list(omissions)}, f)
        return analyze.Selection(path)

    def analyzer(self, selection=None, roots=None):
        with contextlib.redirect_stdout(io.StringIO()):
            a = analyze.BenchmarkAnalyzer(self.out, "/nonexistent/kdhr26.json", selection=selection)
            a.load_results(roots or [self.root])
        return a


class PureFunctionTests(unittest.TestCase):
    def test_variant_inference_from_file_names(self):
        cases = {
            "lcpc-sdig6-plain-release-blake3-as-is.json": "as-published-as-is",
            "lcpc-sdig6-plain-release-blake3-portable-only.json": "as-published-portable",
            "lcpc-sdig6-fork-release-blake3-as-is.json": "fork-flags-as-is",
            "lcpc-sdig6-fork-release-blake3-portable-only.json": "fork-flags-portable",
        }
        for name, variant in cases.items():
            self.assertEqual(analyze.infer_build_variant({}, "/x/" + name), (variant, False))
        self.assertEqual(analyze.infer_build_variant({"build_variant": "as-published-portable"}, "a.json"),
                         ("as-published-portable", True))
        self.assertEqual(analyze.infer_build_variant({}, "other.json"), (None, False))

    def test_block_families(self):
        self.assertEqual(analyze.block_family("full_this_work_mode_a_t8_n16-18-20-22-24-26"), "tw_a")
        self.assertEqual(analyze.block_family("full_this_work_mode_b_mers127_t1_n28"), "tw_b")
        self.assertEqual(analyze.block_family("full_slice_sweep_s128_t1_n20-24-26"), "slice_sweep")
        self.assertEqual(analyze.block_family("full_this_work_threads_t4_n20-24-26"), "tw_threads")
        self.assertEqual(analyze.block_family("full_lcpc_orig_threads_t2_n20-24-26"), "lcpc_threads")
        self.assertEqual(analyze.block_family("full_lcpc_orig_mode_a_t1_n28"), "lcpc_a")
        self.assertEqual(analyze.block_family("full_steps_mode_b_n16-20-24"), "steps")
        self.assertEqual(analyze.block_family("arm_this_work_t1_n18-22-26"), "arm_this_work")
        self.assertIsNone(analyze.block_family("something_else"))

    def test_cell_missing_rules(self):
        self.assertIsNone(analyze.cell_missing_reason("N/A (KDHR26 reports Mont127 only)", "x", ["1"]))
        self.assertEqual(analyze.cell_missing_reason("N/A", "x", ["1"]), "N/A")
        self.assertEqual(analyze.cell_missing_reason("TODO (pending)", "x", ["1"]), "TODO (pending)")
        self.assertEqual(analyze.cell_missing_reason("omitted (documented)", "x", ["1"]), "omitted (documented)")
        self.assertIsNone(analyze.cell_missing_reason("1.00 [0.9-1.1] D6.0%", "x", ["1"]))


class LoaderAndStatsTests(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def test_session_median_mean_and_divergence(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1", samples=(100, 101, 102, 103, 150))])
        self.fx.write("s2", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s2", samples=(110, 111, 112, 113, 114))])
        a = self.fx.analyzer(self.fx.selection())
        st = a.stat(a.select(a.reports_this_work, family="tw_a", log_n=20, threads=1), label="t")
        self.assertEqual(st["sessions"], ["s1", "s2"])
        self.assertAlmostEqual(st["session_medians"]["s1"], 102e6)
        self.assertAlmostEqual(st["mean"], (102e6 + 112e6) / 2)
        self.assertEqual((st["min"], st["max"]), (100e6, 150e6))
        self.assertTrue(st["diverged"])          # 112/102 - 1 = 9.8% > 5%
        self.assertEqual(a.cell_errors, [])

    def test_byte_identical_copies_are_dropped(self):
        # The mode-(b) driver writes a combined file and a per-shape file.
        rec = tw_record("s1", params="simd-spielman", field="Mers127")
        self.fx.write("s1", "full_this_work_mode_b_mers127_t1", "tw/combined_T1.json", [rec])
        self.fx.write("s1", "full_this_work_mode_b_mers127_t1", "tw/combined_T1_brakedown.json", [rec])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        self.assertEqual(len(a.reports_this_work), 1)
        self.assertEqual(a.exact_duplicates_dropped, 1)

    def test_different_records_for_one_session_are_duplicates(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_this_work_mode_a_t1_n20_rerun", "tw/a.json", [tw_record("s1", samples=(90, 91, 92, 93, 94))])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        a.stat(a.select(a.reports_this_work, family="tw_a"), label="T1|20,1|This Work")
        self.assertTrue(any("duplicate" in m for _, m in a.cell_errors))

    def test_pooling_different_configurations_is_an_error(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_slice_sweep_s4_t1_n20", "tw/a.json", [tw_record("s1", slice_mode="explicit", slices=4)])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        a.stat(a.reports_this_work, label="pooled")
        self.assertTrue(any("heterogeneous" in m for _, m in a.cell_errors))
        # Restricting to the headline family gives one clean cell.
        a.cell_errors.clear()
        st = a.stat(a.select(a.reports_this_work, family="tw_a"), label="headline")
        self.assertEqual(a.cell_errors, [])
        self.assertAlmostEqual(st["mean"], 102e6)

    def test_lcpc_variants_are_separate_cells(self):
        blk = "full_lcpc_orig_mode_a_t1_n20"
        self.fx.write("s1", blk, "lcpc_orig/lcpc-sdig6-plain-release-blake3-as-is.json", [lcpc_record("s1", (600,) * 5)])
        self.fx.write("s1", blk, "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json", [lcpc_record("s1", (500,) * 5)])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        self.assertEqual(sorted(r["_meta"]["variant"] for r in a.reports_lcpc_orig),
                         ["as-published-as-is", "fork-flags-portable"])
        st = a.stat(a.select(a.reports_lcpc_orig, variant="fork-flags-portable"), label="x")
        self.assertAlmostEqual(st["mean"], 500e6)
        a.stat(a.reports_lcpc_orig, label="pooled")
        self.assertTrue(any("heterogeneous" in m for _, m in a.cell_errors))

    def test_block_dir_wins_over_mistagged_block_id(self):
        rec = tw_record("s1")
        rec["block_id"] = "full_simd_spielman_mont127_t8_n28"
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [rec])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        self.assertEqual(a.reports_this_work[0]["_meta"]["family"], "tw_a")
        self.assertEqual(len(a.block_id_mismatches), 1)

    def test_simd_coordinator_counts_towards_oversubscription(self):
        def manifest(block, threads, cores, shares=None):
            extra = {} if shares is None else {"coordinator_shares_worker_core": shares}
            self.fx.write("s1", block, "manifest.json", {
                "pinning": {"threads": threads, "cores": ",".join(map(str, range(cores))),
                            "affinity_cores_expanded": list(range(cores))}, "extra": extra})
        manifest("full_simd_spielman_mont127_t8_old", 8, 8)            # unpinned coordinator: 9 threads
        manifest("full_simd_spielman_mont127_t8_r", 8, 8, shares=True)  # documented sharing: allowed
        manifest("full_simd_spielman_mont127_t7_r", 7, 8, shares=False) # 7 + 1 = 8: fine
        manifest("full_this_work_mode_a_t8", 8, 1)                      # oversubscribed
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        joined = "\n".join(a.pinning_errors)
        self.assertIn("t8_old", joined)
        self.assertIn("full_this_work_mode_a_t8", joined)
        self.assertNotIn("t8_r", joined)
        self.assertNotIn("t7_r", joined)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def test_uncovered_records_are_refused(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s3", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s3")])
        with self.assertRaises(SystemExit):
            self.fx.analyzer(self.fx.selection(sessions=("s1",)))

    def test_exclude_by_path_and_by_record_fields(self):
        blk = "full_lcpc_orig_mode_a_t1_n20"
        self.fx.write("s1", blk, "lcpc_orig/lcpc-sdig6-plain-release-blake3-as-is.json", [lcpc_record("s1", (600,) * 5)])
        self.fx.write("s1", blk, "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json", [lcpc_record("s1", (500,) * 5)])
        recorded = lcpc_record("s1", (650,) * 5)
        recorded["build_variant"] = "as-published-as-is"
        self.fx.write("s1", "full_lcpc_orig_mode_a_t1_n20_new", "lcpc_orig/lcpc-new.json", [recorded])
        self.fx.write("s1", "full_slice_sweep_s1_t1", "tw/a.json", [tw_record("s1", slice_mode="explicit")])
        sel = self.fx.selection(sessions=("s1",), exclude=[
            {"path": "*", "match": {"implementation": "lcpc-original-upstream", "build_variant": "as-published-*",
                                    "build_variant_recorded": "false"}, "reason": "fork flags"},
            {"path": f"{MACHINE}/{RUN}/*/full_slice_sweep_*", "reason": "superseded"}])
        a = self.fx.analyzer(sel)
        kept = sorted((r["_meta"]["variant"], r["_meta"]["variant_recorded"]) for r in a.reports_lcpc_orig)
        self.assertEqual(kept, [("as-published-as-is", True), ("fork-flags-portable", False)])
        self.assertEqual(a.reports_this_work, [])
        self.assertEqual(a.excluded, {"fork flags": 1, "superseded": 1})

    def test_steps_mode_taken_from_matching_block(self):
        def step(mode, ms):
            return {"schema_version": "steps-bench-v1", "tag": "HEAD", "mode": mode, "field": "Mont127",
                    "params": "lcpc-sdig6", "log_n": 20, "threads": 1, "session_id": "s1",
                    "samples_ns": [ms * 1_000_000] * 5}
        for blk in ("full_steps_mode_a_n20", "full_steps_mode_b_n20"):
            self.fx.write("s1", blk, "steps/HEAD/a.json", step("a", 100 if blk.endswith("a_n20") else 120))
            self.fx.write("s1", blk, "steps/HEAD/b.json", step("b", 70 if blk.endswith("b_n20") else 80))
        sel = self.fx.selection(sessions=("s1",), expected=("s1",), exclude=[
            {"path": "*/full_steps_mode_a*", "match": {"mode": "b"}, "reason": "a-block mode b"},
            {"path": "*/full_steps_mode_b*", "match": {"mode": "a"}, "reason": "b-block mode a"}])
        a = self.fx.analyzer(sel)
        recs = a.reports_steps["HEAD"]
        self.assertEqual(sorted((r["mode"], r["samples_ns"][0] // 1_000_000) for r in recs), [("a", 100), ("b", 70)])

    def test_missing_session_needs_an_omission(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        a = self.fx.analyzer(self.fx.selection())
        st = a.stat(a.select(a.reports_this_work, family="tw_a"))
        text = a.value_cell("T1", "20,1", "This Work (ms)", st)
        self.assertIn("[s1 only]", text)
        self.assertTrue(any("missing sessions" in m for _, m in a.missing_cell_errors))
        self.assertEqual(a.cell_errors, [])  # a missing session is a missing value, not an integrity error

        b = self.fx.analyzer(self.fx.selection(omissions=[
            {"kind": "pending", "reason": "s2 pending", "cells": ["T1|20,1|This Work*"]}]))
        st = b.stat(b.select(b.reports_this_work, family="tw_a"))
        b.value_cell("T1", "20,1", "This Work (ms)", st)
        self.assertEqual(b.missing_cell_errors, [])
        self.assertIn("T1|20,1|This Work (ms)", b.todo_cells)
        self.assertEqual(b.missing_text("T1|22,1|This Work (ms)"), "-")
        self.assertEqual(b.missing_text("T1|20,1|This Work (ms)"), "TODO (pending)")


class TableTests(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def _rows(self, name):
        with open(os.path.join(self.fx.out, "csv", name)) as f:
            rows = [r for r in csv.reader(f) if r and not r[0].startswith("#")]
        return rows[0], rows[1:]

    def test_t1_uses_headline_blocks_and_lcpc_variant_columns(self):
        for sid in ("s1", "s2"):
            self.fx.write(sid, "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record(sid, samples=(100,) * 5)])
            self.fx.write(sid, "full_slice_sweep_s256_t1_n20", "tw/a.json",
                          [tw_record(sid, samples=(160,) * 5, slice_mode="explicit", slices=256)])
            self.fx.write(sid, "full_this_work_threads_t1_n20", "tw/a.json", [tw_record(sid, samples=(130,) * 5)])
            blk = "full_lcpc_orig_mode_a_t1_n20"
            self.fx.write(sid, blk, "lcpc_orig/lcpc-sdig6-plain-release-blake3-as-is.json", [lcpc_record(sid, (600,) * 5)])
            self.fx.write(sid, blk, "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json", [lcpc_record(sid, (500,) * 5)])
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T1_headline_encoding()
        headers, rows = self._rows("table_T1_headline_encoding.csv")
        row = dict(zip(headers, rows[0]))
        self.assertTrue(row["This Work (ms)"].startswith("100.00"))
        self.assertTrue(row["LCPC as-published-as-is (ms)"].startswith("600.00"))
        self.assertTrue(row["LCPC fork-flags-portable (ms)"].startswith("500.00"))
        self.assertTrue(row["Speedup vs LCPC fork-flags-portable"].startswith("5.00x"))
        self.assertEqual(row["Significance vs fork-flags-portable"], "faster")
        self.assertEqual(row["k x n (TW/LCPC)"], "1024x1762 (both)")
        self.assertEqual(a.cell_errors, [])

    def test_generate_all_runs_on_sparse_data_in_both_modes(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_lcpc_orig_mode_a_t1_n20", "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json",
                      [lcpc_record("s1", (500,) * 5)])
        for selection in (self.fx.selection(sessions=("s1",), expected=("s1",)), None):
            a = self.fx.analyzer(selection, roots=[os.path.join(self.fx.root, MACHINE)])
            with contextlib.redirect_stdout(io.StringIO()):
                a.generate_all()
                ok = a.check_all_artefacts([])
            self.assertFalse(ok)  # sparse data: many cells are missing
            for i in range(13):
                self.assertTrue(any(n.startswith(f"table_T{i}_") for n in os.listdir(os.path.join(self.fx.out, "csv"))))

    def test_kdhr26_lookup_is_keyed_by_reference_cpu(self):
        a = self.fx.analyzer(self.fx.selection())
        a.kdhr26_data = {"table_2": {"entries": [
            {"cpu": "EPYC 9754", "log_n": 16, "simd_spielman_1t_ms": 1.45},
            {"cpu": "Xeon Gold 6530", "log_n": 16, "simd_spielman_1t_ms": 1.02}]}}
        entry, cpu = a.kdhr26_entry("table_2", 16, MACHINE)
        self.assertEqual((cpu, entry["simd_spielman_1t_ms"]), ("EPYC 9754", 1.45))

    def test_check_accepts_documented_na_and_composite_selectors(self):
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a._write_artefact("table_T9_test", "c", "l", ["Field", "T", "log2(N)", "Value", "Ref"],
                              [["Mont127", 8, 28, "TODO (pending)", "N/A (single-threaded table)"],
                               ["Mont127", 8, 26, "1.0", "N/A (single-threaded table)"]], key_cols=3)
        errors = a._check_table_cells([])
        self.assertEqual(len(errors), 1)
        self.assertIn("T9|Mont127,8,28|Value", errors[0])
        self.assertEqual(a._check_table_cells(["T9|*,8,28|Value"]), [])
        self.assertEqual(len(a._check_table_cells(["T9|*,8,26|Value"])), 1)


class InterfaceTests(unittest.TestCase):
    """Shared interfaces (a)-(d) with the harness (reproduce/all.sh, check_codegen.py)."""

    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def _check(self, a, allow=()):
        with contextlib.redirect_stdout(io.StringIO()) as out:
            a.generate_all()
            ok = a.check_all_artefacts(list(allow))
        return ok, out.getvalue()

    def test_stages_layout_is_part_of_the_session(self):
        # (a): <session>/schedule.json accumulates stages; <session>/stages/<tag>/ holds per-stage copies.
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_this_work_mode_a_t8_n20", "tw/a.json", [tw_record("s1", threads=8)])
        d = os.path.join(self.fx.root, MACHINE, RUN, "s1")
        with open(os.path.join(d, "schedule.json"), "w") as f:
            # Field names as written by reproduce/all.sh (rbd-session-schedule-v1).
            json.dump({"format": "rbd-session-schedule-v1", "profile": "full", "session_id": "s1", "stages": [
                {"stage": "20261023T000000Z_A", "block_ids": ["full_this_work_mode_a_t1_n20", "full_this_work_mode_a_t8_n20"],
                 "blocks": [{"id": "full_this_work_mode_a_t1_n20", "status": "ok"},
                            {"id": "full_this_work_mode_a_t8_n20", "status": "ok"}], "exit_status": 0},
                {"stage": "20261023T010000Z_B", "block_ids": ["full_this_work_mode_a_t1_n20", "full_this_work_mode_a_t8_n20"],
                 "blocks": [{"id": "full_this_work_mode_a_t1_n20", "status": "not_run"},
                            {"id": "full_this_work_mode_a_t8_n20", "status": "failed", "exit_code": 7}],
                 "exit_status": 1, "failed_blocks": ["full_this_work_mode_a_t8_n20"]}]}, f)
        for tag in ("20261023T000000Z_A", "20261023T010000Z_B"):
            sd = os.path.join(d, "stages", tag)
            os.makedirs(sd)
            with open(os.path.join(sd, "schedule.json"), "w") as f:
                json.dump({"profile": "full", "session_id": "WRONG-stage-copy"}, f)
            with open(os.path.join(sd, "manifest.json"), "w") as f:
                json.dump({"git": {"code_repo_sha": "deadbeef"}}, f)
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        self.assertEqual({r["_meta"]["session"] for r in a.reports_this_work}, {"s1"})
        self.assertEqual({r["_meta"]["session_dir"] for r in a.reports_this_work}, {"s1"})
        self.assertNotIn("WRONG-stage-copy", a.session_ids)
        self.assertEqual(len(a.session_stages[(MACHINE, RUN, "s1")]), 2)
        self.assertEqual(list(a.failed_block_records), [f"{MACHINE}/{RUN}/s1/full_this_work_mode_a_t8_n20"])

    def test_prev_dirs_excluded_by_selection_and_skipped_unselected(self):
        # (b): a re-run moves the old block dir to <out>.prev-<UTC>.
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_this_work_mode_a_t1_n20.prev-20261023T101010Z", "tw/a.json",
                      [tw_record("s1", samples=(90,) * 5)])
        sel = self.fx.selection(sessions=("s1",), expected=("s1",),
                                exclude=[{"path": "*.prev-*", "reason": "earlier attempt"}])
        a = self.fx.analyzer(sel)
        self.assertEqual(len(a.reports_this_work), 1)
        self.assertEqual(a.excluded, {"earlier attempt": 1})
        b = self.fx.analyzer(None, roots=[os.path.join(self.fx.root, MACHINE)])
        self.assertEqual(len(b.reports_this_work), 1)
        self.assertEqual(len(b.skipped_prev_dirs), 1)
        # The real selection file carries the rule.
        real = analyze.Selection(analyze.DEFAULT_SELECTION)
        self.assertTrue(any(ex.get("path") == "*.prev-*" for ex in real.exclude))

    def _codegen_fixture(self, report):
        for sid in ("s1",):
            self.fx.write(sid, "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record(sid)])
            self.fx.write(sid, "full_this_work_mode_a_t1_n20", "this_work/codegen/codegen_report.json", report)

    def test_codegen_report_read_from_xz(self):
        # Committed codegen reports are <name>.json.xz; they load like <name>.json.
        self._codegen_fixture(codegen_report("FAIL", "aarch64", arith=["add v0.2d, v1.2d, v2.2d"],
                                             moves=["movi v0.2d, #0"]))
        d = os.path.join(self.fx.session("s1"), "full_this_work_mode_a_t1_n20", "this_work", "codegen")
        plain = os.path.join(d, "codegen_report.json")
        with open(plain, "rb") as src, lzma.open(plain + ".xz", "wb") as dst:
            dst.write(src.read())
        os.remove(plain)
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        ok, out = self._check(a)
        self.assertFalse(ok)
        self.assertIn("Codegen report not PASS", out)

    def test_codegen_fail_fails_check_unless_documented(self):
        # (c): any selected FAIL report fails --check; T0 derives its status from the reports.
        self._codegen_fixture(codegen_report("FAIL", "aarch64", arith=["add v0.2d, v1.2d, v2.2d"],
                                             moves=["movi v0.2d, #0"]))
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        ok, out = self._check(a)
        self.assertFalse(ok)
        self.assertIn("Codegen report not PASS", out)
        _, rows = TableTests._rows(self, "table_T0_correctness.csv")
        audit = [r for r in rows if r[0].startswith("Vector-instruction audit")]
        self.assertEqual([(r[0], r[-1]) for r in audit], [("Vector-instruction audit (aarch64)", "FAIL")])
        headers, t11 = TableTests._rows(self, "table_T11_codegen_evidence.csv")
        agg = dict(zip(headers, t11[0]))
        self.assertEqual((agg["Vector Arith"], agg["Vector Moves"]), ("1", "1"))

        b = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",), omissions=[
            {"kind": "pending", "reason": "classifier false positive", "cells": ["T11|aarch64 @ *|Scalar Status"]}]))
        ok, out = self._check(b)
        self.assertNotIn("Codegen report not PASS", out)
        _, rows = TableTests._rows(self, "table_T0_correctness.csv")
        audit = [r for r in rows if r[0].startswith("Vector-instruction audit")]
        self.assertEqual(audit[0][-1], "FAIL (documented, see T11)")

    def test_codegen_vector_moves_alone_pass(self):
        self._codegen_fixture(codegen_report("PASS", "x86_64", moves=["vmovdqu %ymm0, (%rdi)"]))
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        ok, out = self._check(a)
        self.assertNotIn("Codegen report not PASS", out)
        _, rows = TableTests._rows(self, "table_T0_correctness.csv")
        audit = [r for r in rows if r[0].startswith("Vector-instruction audit")]
        self.assertEqual([(r[0], r[-1]) for r in audit], [("Vector-instruction audit (x86_64)", "PASS")])

    def test_omissions_never_excuse_integrity_errors(self):
        # (d): a duplicate stays an error even when an omission pattern matches the cell.
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_this_work_mode_a_t1_n20_again", "tw/a.json", [tw_record("s1", samples=(90,) * 5)])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",), omissions=[
            {"kind": "pending", "reason": "broad", "cells": ["T*|*|*"]},
            {"kind": "pending", "reason": "stale", "cells": ["T99|nothing|here"]}]))
        ok, out = self._check(a, allow=["T*|*|*"])
        self.assertFalse(ok)
        self.assertIn("integrity error (not excusable by omissions)", out)
        self.assertIn("omission pattern matched no cell", out)
        self.assertIn("T99|nothing|here", out)
        self.assertNotIn("matched no cell (data landed or pattern stale?): T*|*|*", out)


class DerivedCellTests(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def test_speedup_from_one_session_is_marked(self):
        for sid in ("s1", "s2"):
            self.fx.write(sid, "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record(sid, samples=(100,) * 5)])
        self.fx.write("s2", "full_lcpc_orig_mode_a_t1_n20", "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json",
                      [lcpc_record("s2", (500,) * 5)])
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T1_headline_encoding()
        headers, rows = TableTests._rows(self, "table_T1_headline_encoding.csv")
        row = dict(zip(headers, rows[0]))
        self.assertIn("[s2 only]", row["LCPC fork-flags-portable (ms)"])
        self.assertIn("[s2 only]", row["Speedup vs LCPC fork-flags-portable"])
        self.assertTrue(any("Speedup vs LCPC fork-flags-portable" in sel for sel, _ in a.missing_cell_errors))

    def _simd_tree(self, patch_t1="12f29de2e552", patch_mt="d9cf97f0347f"):
        for sid in ("s1", "s2"):
            for t in (1, 8):
                self.fx.write(sid, f"full_this_work_mode_b_mont127_t{t}_n20", "tw/b.json",
                              [tw_record(sid, threads=t, params="simd-spielman", shape="orion",
                                         samples=(40,) * 5 if t == 1 else (10,) * 5)])
            for t, ms in ((1, 30), (7, 45), (8, 50)):
                blk = f"full_simd_spielman_mont127_t{t}_n20"
                self.fx.write(sid, blk, "simd_spielman_bench.json",
                              [simd_record(sid, threads=t, shape="orion", samples=(ms,) * 5)])
                self.fx.write(sid, blk, "simd_spielman.json",
                              {"status": "complete", "params_patch_sha256": patch_t1 if t == 1 else patch_mt})
            self.fx.write(sid, "full_simd_spielman_mont127_t1_n28", "simd_spielman_bench.json",
                          [simd_record(sid, threads=1, log_n=28, shape="orion", status="omitted",
                                       omission_reason="Orion shape above logN=26 not run (--max-orion-logn)")])

    def test_t9_best_of_t_mt_slower_flag_patch_and_record_status(self):
        self._simd_tree()
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T9_orion_shape()
        with open(os.path.join(self.fx.out, "csv", "table_T9_orion_shape.csv")) as f:
            notes = " ".join(r[0] for r in csv.reader(f) if r and r[0].startswith("# Note"))
        headers, rows = TableTests._rows(self, "table_T9_orion_shape.csv")
        t8 = dict(zip(headers, next(r for r in rows if r[1] == "8" and r[2] == "20")))
        self.assertTrue(t8["SIMD / This Work"].startswith("4.50x"))           # best 8-core: 7W+coord 45 ms / 10 ms
        self.assertTrue(t8["SIMD best-of-T incl. T=1 (ms)"].startswith("30.00 (T=1)"))
        self.assertTrue(t8["Gap vs SIMD best-of-T"].startswith("3.00x"))
        self.assertEqual(t8["SIMD MT slower than its T=1"], "yes")
        self.assertTrue(t8["SIMD params.patch"].startswith("MIXED"))
        self.assertIn("status 'omitted' (Orion shape above logN=26 not run (--max-orion-logn))", notes)
        self.assertIn("WARNING: rows comparing SIMD-Spielman runs built with different params.patch", notes)
        self.assertEqual(a.cell_errors, [])

    def test_simd_patch_is_part_of_the_cell_key(self):
        self._simd_tree()
        # s2's T=1 block used a different patch than s1's: never pooled.
        self.fx.write("s2", "full_simd_spielman_mont127_t1_n20", "simd_spielman.json",
                      {"status": "complete", "params_patch_sha256": "ffffffffffff"})
        a = self.fx.analyzer(self.fx.selection())
        a.stat(a.select(a.reports_simd_spielman, threads=1, log_n=20), label="T9|x|SIMD")
        self.assertTrue(any("heterogeneous" in m for _, m in a.cell_errors))

    def test_t12_shows_effective_rustflags(self):
        rec = lcpc_record("arm1", (500,) * 5, log_n=18)
        rec.update({"build_variant": "fork-flags-portable", "effective_rustflags": "-C target-cpu=native"})
        self.fx.write("arm1", "arm_lcpc_orig_t1_n18", "lcpc_orig/x.json", [rec], machine="soc-xcng-test")
        path = os.path.join(self.fx.root, "selection.json")
        with open(path, "w") as f:
            json.dump({"schema_version": "rbd-selection-v1", "machines": {"soc-xcng-test": {
                "expected_sessions": ["arm1"], "runs": [{"run_dir": RUN, "sessions": ["arm1"]}]}},
                "omissions": [{"kind": "pending", "reason": "Arm This Work blocks failed",
                               "cells": ["T12|*|This Work Arm (ms)", "T12|*|Arm Speedup vs *"]}]}, f)
        a = self.fx.analyzer(analyze.Selection(path))
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T12_arm_portability()
        with open(os.path.join(self.fx.out, "csv", "table_T12_arm_portability.csv")) as f:
            text = f.read()
        self.assertIn("fork-flags-portable: '-C target-cpu=native'", text)
        headers, rows = TableTests._rows(self, "table_T12_arm_portability.csv")
        row = dict(zip(headers, rows[0]))
        self.assertEqual(row["This Work Arm (ms)"], "TODO (pending)")
        self.assertIn("TODO (pending) cells: pending: Arm This Work blocks failed", text)


class LatexTests(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def test_escaping(self):
        e = analyze.escape_latex
        self.assertEqual(e("N=2^{n} (ms)"), "N=$2^{n}$ (ms)")
        self.assertEqual(e("2^28 & 5% of a_b"), r"$2^{28}$ \& 5\% of a\_b")
        self.assertEqual(e("<= 10%"), r"$\le$ 10\%")
        self.assertEqual(e("x^y"), "$x^{y}$")
        self.assertEqual(e("a|b ~ {c}"), r"a\textbar{}b \textasciitilde{} \{c\}")
        self.assertEqual(e("F_{(2^61-1)^2}"), r"F\_\{($2^{61}$-1)\textasciicircum{}2\}")

    @unittest.skipUnless(shutil.which("pdflatex"), "pdflatex not installed")
    def test_every_generated_table_compiles(self):
        for sid in ("s1", "s2"):
            for t in (1, 8):
                self.fx.write(sid, f"full_this_work_mode_a_t{t}_n20", "tw/a.json", [tw_record(sid, threads=t)])
                self.fx.write(sid, f"full_this_work_mode_b_mont127_t{t}_n20", "tw/b.json",
                              [tw_record(sid, threads=t, params="simd-spielman"),
                               tw_record(sid, threads=t, params="simd-spielman", shape="orion")])
                self.fx.write(sid, f"full_simd_spielman_mont127_t{t}_n20", "simd_spielman_bench.json",
                              [simd_record(sid, threads=t), simd_record(sid, threads=t, shape="orion")])
            self.fx.write(sid, "full_lcpc_orig_mode_a_t1_n20", "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json",
                          [lcpc_record(sid, (500,) * 5)])
            self.fx.write(sid, "full_this_work_mode_a_t1_n20", "this_work/codegen/codegen_report.json",
                          codegen_report("PASS", moves=["vmovdqu %ymm0, (%rdi)"]))
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_all()
        texs = sorted(os.path.join(a.latex_dir, n) for n in os.listdir(a.latex_dir) if n.endswith(".tex"))
        self.assertEqual(len(texs), 13)
        compiled, errors = analyze.compile_latex_tables(texs, os.path.join(self.fx.root, "_latex"))
        self.assertEqual(errors, [])
        self.assertEqual(len(compiled), 13)
        # A broken table is reported, not silently accepted.
        bad = os.path.join(self.fx.root, "table_T99_bad.tex")
        with open(bad, "w") as f:
            f.write("\\begin{tabular}{l}\\toprule 2^28 \\\\ \\bottomrule\\end{tabular}\n")
        _, errors = analyze.compile_latex_tables([bad], os.path.join(self.fx.root, "_latex_bad"))
        self.assertEqual(len(errors), 1)



def stage_schedule(path, stages):
    with open(os.path.join(path, "schedule.json"), "w") as f:
        json.dump({"format": "rbd-session-schedule-v1", "profile": "full", "session_id": os.path.basename(path),
                   "stages": stages}, f)


class InterruptedRerunTests(unittest.TestCase):
    """Review 2 item 1: an interrupted re-run leaves untagged partial records in the block directory."""

    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def _check(self, a):
        with contextlib.redirect_stdout(io.StringIO()) as out:
            a.generate_all()
            ok = a.check_all_artefacts([])
        return ok, out.getvalue()

    def test_moved_previous_without_final_status_is_interrupted(self):
        blk = "full_this_work_mode_a_t1_n20"
        self.fx.write("s1", blk, "tw/a.json", [tw_record("s1")])           # partial, untagged
        self.fx.write("s1", blk + ".prev-20261023T000000Z", "tw/a.json", [dict(tw_record("s1"), block_status="ok")])
        stage_schedule(os.path.join(self.fx.root, MACHINE, RUN, "s1"), [
            {"stage": "A", "state": "finished", "exit_status": 0, "blocks": [{"id": blk, "status": "ok"}]},
            {"stage": "B", "state": "interrupted", "exit_status": 143, "failed_blocks": [],
             "blocks": [{"id": blk, "status": "not_run", "moved_previous_to": blk + ".prev-20261023T000000Z"}]}])
        sel = self.fx.selection(sessions=("s1",), expected=("s1",),
                                exclude=[{"path": "*.prev-*", "reason": "earlier attempt"}])
        a = self.fx.analyzer(sel)
        self.assertEqual(dict(a.interrupted_block_records), {f"{MACHINE}/{RUN}/s1/{blk}": 1})
        self.assertEqual(dict(a.failed_block_records), {})
        ok, out = self._check(a)
        self.assertFalse(ok)
        self.assertIn("Interrupted block", out)

    def test_killed_first_run_and_unfinalized_stage_are_interrupted(self):
        self.fx.write("s1", "blk_new_this_work_mode_a_t1", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_this_work_mode_a_t8_n20", "tw/a.json", [tw_record("s1", threads=8)])
        stage_schedule(os.path.join(self.fx.root, MACHINE, RUN, "s1"), [
            {"stage": "A", "state": "interrupted", "exit_status": 143,
             "blocks": [{"id": "blk_new_this_work_mode_a_t1", "status": "not_run"}]},
            {"stage": "B", "state": "running", "exit_status": None,
             "blocks": [{"id": "full_this_work_mode_a_t8_n20", "status": "scheduled"}]}])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        self.assertEqual(sorted(b.rsplit("/", 1)[1] for b in a.interrupted_block_records),
                         ["blk_new_this_work_mode_a_t1", "full_this_work_mode_a_t8_n20"])

    def test_completed_and_tagged_blocks_are_not_interrupted(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [dict(tw_record("s1"), block_status="ok")])
        self.fx.write("s1", "full_this_work_mode_a_t8_n20", "tw/a.json", [tw_record("s1", threads=8)])
        stage_schedule(os.path.join(self.fx.root, MACHINE, RUN, "s1"), [
            {"stage": "A", "state": "finished", "exit_status": 0,
             "blocks": [{"id": "full_this_work_mode_a_t1_n20", "status": "ok"},
                        {"id": "full_this_work_mode_a_t8_n20", "status": "ok"}]},
            # A later stage that did not touch the blocks (not_run, nothing moved).
            {"stage": "B", "state": "interrupted", "exit_status": 143,
             "blocks": [{"id": "full_this_work_mode_a_t1_n20", "status": "not_run"},
                        {"id": "full_this_work_mode_a_t8_n20", "status": "not_run"}]}])
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        self.assertEqual(dict(a.interrupted_block_records), {})
        self.assertEqual(dict(a.failed_block_records), {})


NEW_RUN = "2026-10-23-375a6e1x"


class SupersedeTests(unittest.TestCase):
    """selection.json 'supersede': a newer run replaces every older record of the cells it re-measured."""

    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def _selection(self, supersede=None, runs=None, omissions=()):
        path = os.path.join(self.fx.root, "selection.json")
        with open(path, "w") as f:
            json.dump({"schema_version": "rbd-selection-v1", "machines": {MACHINE: {
                "expected_sessions": ["s1", "s2"],
                "runs": runs or [{"run_dir": RUN, "sessions": ["s1", "s2"]},
                                 {"run_dir": "2026-10-23-375a6e1*", "sessions": ["s1", "s2"]}]}},
                "supersede": supersede if supersede is not None else [
                    {"by": [f"{MACHINE}/2026-10-23-375a6e1*"],
                     "blocks": [f"{MACHINE}/*/*/full_lcpc_orig_mode_a_*", f"{MACHINE}/*/*/full_this_work_threads_*",
                                f"{MACHINE}/*/*/full_lcpc_orig_threads_*"],
                     "reason": "re-measured at 375a6e1"}],
                "omissions": list(omissions)}, f)
        return analyze.Selection(path)

    def _old_tree(self):
        lc = "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json"
        for sid in ("s1", "s2"):
            self.fx.write(sid, "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record(sid, samples=(100,) * 5)])
            self.fx.write(sid, "full_lcpc_orig_mode_a_t1_n20-24", lc,
                          [lcpc_record(sid, (700,) * 5), lcpc_record(sid, (900,) * 5, log_n=24)])
            self.fx.write(sid, "full_this_work_threads_t1_n20", "tw/a.json", [tw_record(sid, samples=(130,) * 5)])
            self.fx.write(sid, "full_this_work_threads_t1_n20", "this_work/codegen/codegen_report.json",
                          codegen_report("PASS"))

    def test_inert_until_the_new_run_exists(self):
        self._old_tree()
        with contextlib.redirect_stdout(io.StringIO()) as out:
            a = analyze.BenchmarkAnalyzer(self.fx.out, "/nonexistent", selection=self._selection())
            a.load_results([self.fx.root])
        self.assertEqual(dict(a.superseded), {})
        self.assertEqual(len(a.reports_lcpc_orig), 4)
        self.assertIn("matches no run directory yet", out.getvalue())

    def test_new_run_supersedes_old_cells_in_all_sessions(self):
        self._old_tree()
        lc = "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json"
        # The re-measurement (same block ids, new run dir); s2 has not landed yet for the threads block.
        for sid in ("s1", "s2"):
            self.fx.write(sid, "full_lcpc_orig_mode_a_t1_n20-24", lc,
                          [lcpc_record(sid, (500,) * 5)], run=NEW_RUN)       # only 2^20 re-measured
        self.fx.write("s1", "full_this_work_threads_t1_n20", "tw/a.json", [tw_record("s1", samples=(120,) * 5)],
                      run=NEW_RUN)
        self.fx.write("s1", "full_this_work_threads_t1_n20", "this_work/codegen/codegen_report.json",
                      codegen_report("PASS"), run=NEW_RUN)
        a = self.fx.analyzer(self._selection())
        # LCPC 2^20: both old sessions superseded; 2^24 (not re-measured) kept.
        lcpc = sorted((r["_meta"]["run"], r["_meta"]["session"], int(r["log_n"])) for r in a.reports_lcpc_orig)
        self.assertEqual(lcpc, sorted([(NEW_RUN, "s1", 20), (NEW_RUN, "s2", 20), (RUN, "s1", 24), (RUN, "s2", 24)]))
        st = a.stat(a.select(a.reports_lcpc_orig, log_n=20), label="T1|20,1|LCPC")
        self.assertAlmostEqual(st["mean"], 500e6)
        self.assertEqual(a.cell_errors, [])
        # Threads: the cell is superseded in every session, so s2 is missing (never mixed across runs).
        thr = a.select(a.reports_this_work, family="tw_threads")
        self.assertEqual([(r["_meta"]["run"], r["_meta"]["session"]) for r in thr], [(NEW_RUN, "s1")])
        # The headline block is not covered by the rule and stays.
        self.assertEqual(len(a.select(a.reports_this_work, family="tw_a")), 2)
        # Codegen reports of the superseded blocks go too.
        self.assertEqual({r["_meta"]["run"] for r in a.codegen_reports}, {NEW_RUN})
        self.assertTrue(any(k.startswith("superseded: re-measured at 375a6e1") for k in a.excluded))
        self.assertTrue(a._fully_superseded(f"{MACHINE}/{RUN}/s1/full_this_work_threads_t1_n20"))
        self.assertFalse(a._fully_superseded(f"{MACHINE}/{RUN}/s1/full_lcpc_orig_mode_a_t1_n20-24"))

    def test_without_supersede_the_rerun_is_a_duplicate(self):
        self._old_tree()
        self.fx.write("s1", "full_lcpc_orig_mode_a_t1_n20-24", "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json",
                      [lcpc_record("s1", (500,) * 5)], run=NEW_RUN)
        a = self.fx.analyzer(self._selection(supersede=[]))
        a.stat(a.select(a.reports_lcpc_orig, log_n=20), label="T1|20,1|LCPC")
        self.assertTrue(any("duplicate" in m for _, m in a.cell_errors))

    def test_real_selection_covers_the_cluster_campaign(self):
        real = analyze.Selection(analyze.DEFAULT_SELECTION)
        for machine, session in (("soc-xcnh", "s1"), ("soc-xcnh", "s2"), ("soc-xcng", "arm1")):
            entry = real.run_entry(machine, "2026-11-14-0123456789ab")
            self.assertIsNotNone(entry, machine)
            self.assertIn(session, entry["sessions"])
        self.assertIsNone(real.run_entry("soc-xcnh", "host-profile-xcnh1.json"))
        self.assertEqual(real.supersede, [])
        self.assertFalse(any(om["kind"] == "pending" for om in real.omissions))
        self.assertEqual(real.expected_sessions("soc-xcnh"), ["s1", "s2"])
        self.assertEqual(real.expected_sessions("soc-xcng"), ["arm1"])
        status, _ = real.classify("soc-xcnh/2026-11-14-0123456789ab/s1/full_this_work_mode_a_t1_n28.prev-20261114T000000Z",
                                  "x.json", {})
        self.assertEqual(status, "exclude")
        status, _ = real.classify("local-i7-11700k/2026-11-14-0123456789ab/s1/full_this_work_mode_a_t1", "x.json", {})
        self.assertEqual(status, "exclude")
        fields = {"implementation": "lcpc-original-upstream", "build_variant": "as-published-as-is",
                  "build_variant_recorded": "true"}
        status, _ = real.classify("soc-xcnh/2026-11-14-0123456789ab/s1/full_lcpc_orig_mode_a_t1_n28", "x.json", fields)
        self.assertEqual(status, "include")

    def test_ft127_blocks_have_their_own_family(self):
        self.assertEqual(analyze.block_family("full_this_work_ft127_t8_n20-24"), "tw_ft127")
        self.assertEqual(analyze.block_family("full_this_work_mode_a_t8_n20-24"), "tw_a")


class CodegenRecheckTests(unittest.TestCase):
    """Review 2 items 2 and 5: re-classified v1 reports, step reports in the gate."""

    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    @staticmethod
    def v1_report(lines, hits):
        r = codegen_report("FAIL", "aarch64", arith=hits)
        r["symbols"][0]["disassembly_lines"] = lines
        r["summary"]["hot_kernels"]["total_instructions"] = len(lines)
        return r

    def _check(self, a):
        with contextlib.redirect_stdout(io.StringIO()) as out:
            a.generate_all()
            ok = a.check_all_artefacts([])
        return ok, out.getvalue()

    def test_v1_branch_false_positive_passes_on_recheck(self):
        lines = ["b332c: b.eq b3334 <f::<typenum::B1>+0x40>", "b3330: movi v0.2d, #0x0", "b3334: mul x1, x2, x3"]
        rep = self.v1_report(lines, ["b.eq b3334 <f::<typenum::B1>+0x40>"])
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "this_work/codegen/codegen_report.json", rep)
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        ok, out = self._check(a)
        self.assertNotIn("Codegen report not PASS", out)
        headers, rows = TableTests._rows(self, "table_T11_codegen_evidence.csv")
        agg = dict(zip(headers, rows[0]))
        self.assertEqual(agg["Scalar Status"], "FAIL in report; PASS on re-check")
        self.assertEqual(agg["Re-check (current classifier)"], "0 arith, 1 moves in 3 stored lines")
        _, t0 = TableTests._rows(self, "table_T0_correctness.csv")
        audit = [r for r in t0 if r[0].startswith("Vector-instruction audit")]
        self.assertEqual(audit[0][-1], "PASS (incl. PASS on re-check, see T11)")
        with open(os.path.join(self.fx.out, "csv", "table_T11_codegen_evidence.csv")) as f:
            self.assertIn("'b.eq b3334 <...>'", f.read())

    def test_real_vector_arithmetic_stays_fail(self):
        lines = ["b332c: b.eq b3334 <f+0x40>", "b3330: add v0.2d, v1.2d, v2.2d"]
        rep = self.v1_report(lines, ["b.eq b3334 <f+0x40>", "add v0.2d, v1.2d, v2.2d"])
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "this_work/codegen/codegen_report.json", rep)
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        ok, out = self._check(a)
        self.assertFalse(ok)
        self.assertIn("Codegen report not PASS", out)

    def test_incomplete_listing_is_not_rechecked_to_pass(self):
        rep = self.v1_report(["b332c: b.eq b3334 <f+0x40>"], ["b.eq b3334 <f+0x40>"])
        rep["summary"]["hot_kernels"]["total_instructions"] = 40     # listing does not cover the kernel
        own, eff, rc = analyze.BenchmarkAnalyzer.effective_codegen_status(
            self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",))), rep)
        self.assertEqual((own, eff, rc["complete"]), ("FAIL", "FAIL", False))

    def test_step_reports_are_gated_and_no_hot_kernels_footnoted(self):
        def step(mode):
            return {"schema_version": "steps-bench-v1", "tag": "step_01-modernize", "mode": mode, "field": "Ft127",
                    "params": "lcpc-sdig6", "log_n": 20, "threads": 1, "session_id": "s1", "samples_ns": [1e8] * 5}
        blk = "full_steps_mode_a_n20"
        self.fx.write("s1", blk, "steps/step_01-modernize/a.json", step("a"))
        none = codegen_report("PASS")
        none["hot_kernels_checked"] = 0
        self.fx.write("s1", blk, "steps/step_01-modernize/codegen_report.json", none)
        self.fx.write("s1", blk, "steps/step_05-lazy/codegen_report.json",
                      codegen_report("FAIL", "x86_64", arith=["vpaddq %ymm0, %ymm1, %ymm2"]))
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        ok, out = self._check(a)
        self.assertIn("T3|codegen,step/05-lazy|Scalar Status: step codegen status FAIL", out)
        with open(os.path.join(self.fx.out, "csv", "table_T3_steps_breakdown.csv")) as f:
            self.assertIn("matched no hot-kernel symbol at step/01-modernize", f.read())
        b = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",), omissions=[
            {"kind": "pending", "reason": "documented", "cells": ["T3|codegen,step/05-lazy|Scalar Status"]}]))
        ok, out = self._check(b)
        self.assertNotIn("step codegen status FAIL", out)

    def test_t11_symbol_names_are_unique(self):
        rep = codegen_report("PASS")
        sym = rep["symbols"][0]
        rep["symbols"] = [dict(sym, symbol_demangled="a::encode::encode_column_block::<x::mont127::Mont127>"),
                          dict(sym, symbol_demangled="a::encode::encode_column_block::<x::mers127::Mers127>"),
                          dict(sym, symbol_demangled="a::f::<x::A>"), dict(sym, symbol_demangled="b::f::<y::A>")]
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "this_work/codegen/codegen_report.json", rep)
        a = self.fx.analyzer(self.fx.selection(sessions=("s1",), expected=("s1",)))
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T11_codegen_evidence()
        _, rows = TableTests._rows(self, "table_T11_codegen_evidence.csv")
        names = [r[1] for r in rows[1:]]
        self.assertEqual(names, ["encode_column_block::<Mont127>", "encode_column_block::<Mers127>",
                                 "a::f::<x::A>", "b::f::<y::A>"])
        self.assertEqual(len({(r[0], r[1]) for r in rows}), len(rows))


class CheckCoverageTests(unittest.TestCase):
    """Expected rows (item 6), --allow / omission bookkeeping (item 7, cross-group)."""

    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def _selection(self, expected_rows, omissions=()):
        path = os.path.join(self.fx.root, "selection.json")
        with open(path, "w") as f:
            json.dump({"schema_version": "rbd-selection-v1", "machines": {MACHINE: {
                "expected_sessions": ["s1"], "runs": [{"run_dir": RUN, "sessions": ["s1"]}]}},
                "expected_rows": expected_rows, "omissions": list(omissions)}, f)
        return analyze.Selection(path)

    def test_missing_expected_rows_are_reported_unless_documented(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        a = self.fx.analyzer(self._selection({"T1": {"log2(N)": [20, 22], "T": [1]}}))
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T1_headline_encoding()
        errors = a._check_expected_rows([])
        self.assertEqual(len(errors), 1)
        self.assertIn("T1|22,1|(row)", errors[0])
        self.assertEqual(a._check_expected_rows(["T1|22,*|*"]), [])
        b = self.fx.analyzer(self._selection({"T1": {"log2(N)": [20, 22], "T": [1]}}, omissions=[
            {"kind": "pending", "reason": "2^22 pending", "cells": ["T1|22,*|*"]}]))
        with contextlib.redirect_stdout(io.StringIO()):
            b.generate_T1_headline_encoding()
        self.assertEqual(b._check_expected_rows([]), [])
        self.assertIn("T1|22,1|(row)", b.todo_cells)

    def test_unmatched_allow_patterns_are_warned_about(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        a = self.fx.analyzer(self._selection({}))
        with contextlib.redirect_stdout(io.StringIO()) as out:
            a.generate_all()
            a.check_all_artefacts(["T1|*|LCPC *", "T99|nothing|here"])
        text = out.getvalue()
        self.assertIn("--allow selector matched no missing cell (stale or mistyped?): T99|nothing|here", text)
        self.assertNotIn("(stale or mistyped?): T1|*|LCPC *", text)

    def test_every_matching_omission_pattern_is_credited(self):
        sel = self._selection({}, omissions=[{"kind": "pending", "reason": "narrow", "cells": ["T1|20,1|x"]},
                                             {"kind": "omitted", "reason": "broad", "cells": ["T1|*|*"]}])
        self.assertEqual(sel.omission_for("T1|20,1|x")["reason"], "narrow")
        self.assertEqual(sel.unmatched_omission_patterns(), [])


class TableNoteTests(unittest.TestCase):
    """Review 2 items 3 and 4: T8 SIMD setup/RSS, T12 flags note from the records."""

    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    def test_t8_reads_simd_setup_rss_and_flags_below_floor(self):
        for sid in ("s1", "s2"):
            for n, setup in ((20, 40_000_000), (24, 2_000_000_000)):
                self.fx.write(sid, f"full_simd_spielman_mont127_t1_n{n}", "simd_spielman_bench.json", [simd_record(
                    sid, log_n=n, setup_ns={"graph_generation_and_process_overhead_ns": setup},
                    peak_rss_bytes=512 * 1024 * 1024, setup_ns_resolution_ns=10_000_000,
                    setup_ns_floor_ns=100_000_000, setup_ns_below_floor=setup < 100_000_000)])
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T8_setup_memory()
        headers, rows = TableTests._rows(self, "table_T8_setup_memory.csv")
        simd = {r[1]: dict(zip(headers, r)) for r in rows if r[0] == "SIMD-Spielman"}
        self.assertEqual(simd["20"]["Setup (ms)"], "40.00 (below floor)")
        self.assertEqual(simd["24"]["Setup (ms)"], "2000.00")
        self.assertEqual(simd["24"]["Peak RSS (MB)"], "512.0")
        self.assertTrue(simd["24"]["Setup / Encode"].startswith("40.00x"))
        with open(os.path.join(self.fx.out, "csv", "table_T8_setup_memory.csv")) as f:
            text = f.read()
        self.assertIn("steps of 10 ms", text)
        self.assertIn("(at 2^20)", text)

    def test_t12_flags_note_follows_the_records(self):
        def tree(flags):
            fx = Fixture()
            rec = lcpc_record("arm1", (500,) * 5, log_n=18)
            rec.update({"build_variant": "fork-flags-portable"})
            if flags is not None:
                rec["effective_rustflags"] = flags
            fx.write("arm1", "arm_lcpc_orig_t1_n18", "lcpc_orig/x.json", [rec], machine="soc-xcng-test")
            path = os.path.join(fx.root, "selection.json")
            with open(path, "w") as f:
                json.dump({"schema_version": "rbd-selection-v1", "machines": {"soc-xcng-test": {
                    "expected_sessions": ["arm1"], "runs": [{"run_dir": RUN, "sessions": ["arm1"]}]}}}, f)
            a = fx.analyzer(analyze.Selection(path))
            with contextlib.redirect_stdout(io.StringIO()):
                a.generate_T12_arm_portability()
            with open(os.path.join(fx.out, "csv", "table_T12_arm_portability.csv")) as f:
                text = f.read()
            fx.cleanup()
            return text
        native = tree("-C target-cpu=native -C no-vectorize-loops")
        self.assertIn("include target-cpu=native", native)
        generic = tree("-C no-vectorize-loops -C no-vectorize-slp")
        self.assertNotIn("target-cpu=native", generic)
        self.assertIn("do not record their build flags", tree(None))


class Review3Tests(unittest.TestCase):
    """Review 3: T6 BLAKE3-backend/flags rows, T8 SIMD session gap, same-boot partner runs, expected_rows lists."""

    def setUp(self):
        self.fx = Fixture()

    def tearDown(self):
        self.fx.cleanup()

    @staticmethod
    def pcs_record(session, encode, hash_, log_n=20, threads=1):
        r = lcpc_record(session, (encode,) * 5, log_n=log_n, threads=threads)
        r["measurements"]["hash_matrix_decomposition"] = {"samples_ns": [hash_ * 1_000_000] * 5}
        r["measurements"]["commit_total"] = {"samples_ns": [(encode + hash_) * 1_000_000] * 5}
        return r

    def test_t6_reports_all_four_variants_and_separates_backend_from_flags(self):
        files = {"as-published-as-is": ("plain", "as-is", 600, 30), "as-published-portable": ("plain", "portable-only", 600, 60),
                 "fork-flags-as-is": ("fork", "as-is", 500, 30), "fork-flags-portable": ("fork", "portable-only", 500, 60)}
        for sid in ("s1", "s2"):
            for v, (prof, hm, enc, hsh) in files.items():
                recs = [self.pcs_record(sid, enc, hsh, log_n=n) for n in (20, 24, 28)]
                for r in recs:
                    r["hash_mode"] = hm
                self.fx.write(sid, "full_lcpc_orig_mode_a_t1_n20-24-28",
                              f"lcpc_orig/lcpc-sdig6-{prof}-release-blake3-{hm}.json", recs)
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T6_pcs_breakdown()
        headers, rows = TableTests._rows(self, "table_T6_pcs_breakdown.csv")
        keys = {(r[0], r[2]) for r in rows}
        for v in ("as-published-portable", "fork-flags-as-is"):
            self.assertIn((f"Original LCPC {v}", "20"), keys)
            self.assertIn((f"Original LCPC {v}", "24"), keys)
            self.assertNotIn((f"Original LCPC {v}", "28"), keys)     # measured at 2^20/2^24 only (stage RH)
        self.assertIn(("Original LCPC fork-flags-portable", "28"), keys)
        with open(os.path.join(self.fx.out, "csv", "table_T6_pcs_breakdown.csv")) as f:
            note = [l for l in f if "BLAKE3 backend vs compiler flags" in l][0]
        # backend: hash portable/as-is = 60/30; flags: encode fork/as-published = 500/600
        self.assertIn("T=1 2^20: backend on hash 2.00x with as published, 2.00x with fork flags", note)
        self.assertIn("flags on encode 0.83x with default-features BLAKE3, 0.83x with portable BLAKE3", note)
        self.assertNotIn("2^28:", note)
        self.assertEqual(a.cell_errors, [])

    def test_t6_effects_need_both_cells(self):
        means = {("Original LCPC fork-flags-portable", 1, 20, "hash_matrix_decomposition"): 60.0,
                 ("Original LCPC fork-flags-as-is", 1, 20, "hash_matrix_decomposition"): None}
        self.assertEqual(analyze.BenchmarkAnalyzer._t6_variant_effects(means), [])

    def test_t8_simd_session_gap_needs_omission_and_is_footnoted(self):
        for sid in ("s1", "s2"):
            extra = dict(setup_ns={"graph_generation_and_process_overhead_ns": 4_000_000_000},
                         peak_rss_bytes=2 ** 30) if sid == "s1" else {}
            self.fx.write(sid, "full_simd_spielman_mont127_t1_n28", "simd_spielman_bench.json",
                          [simd_record(sid, log_n=28, **extra)])
        cells = ["T8|SIMD-Spielman,28|Setup (ms)", "T8|SIMD-Spielman,28|Setup / Encode", "T8|SIMD-Spielman,28|Peak RSS (MB)"]
        a = self.fx.analyzer(self.fx.selection())
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T8_setup_memory()
        self.assertEqual(sorted(s for s, _ in a.missing_cell_errors), sorted(cells))
        b = self.fx.analyzer(self.fx.selection(omissions=[{"kind": "omitted", "reason": "s2 harness", "cells": cells}]))
        with contextlib.redirect_stdout(io.StringIO()):
            b.generate_T8_setup_memory()
        self.assertEqual(b.missing_cell_errors, [])
        headers, rows = TableTests._rows(self, "table_T8_setup_memory.csv")
        row = dict(zip(headers, [r for r in rows if r[0] == "SIMD-Spielman"][0]))
        self.assertEqual(row["Setup (ms)"], "4000.00 [s1 only]")
        with open(os.path.join(self.fx.out, "csv", "table_T8_setup_memory.csv")) as f:
            self.assertIn("not recorded in s2 at 2^28", f.read())

    def test_t8_prewired_gap_omission_is_not_stale_before_the_data_lands(self):
        self.fx.write("s2", "full_simd_spielman_mont127_t1_n28", "simd_spielman_bench.json", [simd_record("s2", log_n=28)])
        cells = ["T8|SIMD-Spielman,28|Setup (ms)", "T8|SIMD-Spielman,28|Setup / Encode", "T8|SIMD-Spielman,28|Peak RSS (MB)"]
        sel = self.fx.selection(omissions=[{"kind": "omitted", "reason": "s2 harness", "cells": cells},
                                           {"kind": "pending", "reason": "s1", "cells": ["T8|SIMD-Spielman,28|Encode (ms)"]}])
        a = self.fx.analyzer(sel)
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T8_setup_memory()
        self.assertEqual(sel.unmatched_omission_patterns(), [])

    def test_partner_runs_warn_when_one_side_comes_from_another_run(self):
        sel_path = os.path.join(self.fx.root, "selection.json")
        with open(sel_path, "w") as f:
            json.dump({"schema_version": "rbd-selection-v1", "machines": {MACHINE: {
                "expected_sessions": ["s1"], "runs": [{"run_dir": RUN, "sessions": ["s1"]},
                                                     {"run_dir": NEW_RUN, "sessions": ["s1"]}]}},
                "supersede": [{"by": [f"{MACHINE}/{NEW_RUN}"], "blocks": [f"{MACHINE}/*/*/full_this_work_threads_*",
                                                                          f"{MACHINE}/*/*/full_lcpc_orig_threads_*"],
                               "reason": "same-boot pairs"}]}, f)
        lc = "lcpc_orig/lcpc-sdig6-fork-release-blake3-portable-only.json"
        self.fx.write("s1", "full_this_work_threads_t1_n20", "tw/a.json", [tw_record("s1")])
        self.fx.write("s1", "full_lcpc_orig_threads_t1_n20", lc, [lcpc_record("s1", (500,) * 5)])
        same = self.fx.analyzer(analyze.Selection(sel_path))
        self.assertEqual(same.partner_run_mismatches, [])
        # The re-run's This Work block landed, its LCPC partner did not (failed block).
        self.fx.write("s1", "full_this_work_threads_t1_n20", "tw/a.json", [tw_record("s1")], run=NEW_RUN)
        with contextlib.redirect_stdout(io.StringIO()) as out:
            a = analyze.BenchmarkAnalyzer(self.fx.out, "/nonexistent", selection=analyze.Selection(sel_path))
            a.load_results([self.fx.root])
        self.assertEqual(len(a.partner_run_mismatches), 1)
        self.assertIn(f"tw_threads from {NEW_RUN}, lcpc_threads from {RUN}", a.partner_run_mismatches[0])
        self.assertIn("Same-boot pair split across runs", out.getvalue())

    def test_expected_rows_accept_a_list_of_specs(self):
        self.fx.write("s1", "full_this_work_mode_a_t1_n20", "tw/a.json", [tw_record("s1")])
        path = os.path.join(self.fx.root, "selection.json")
        with open(path, "w") as f:
            json.dump({"schema_version": "rbd-selection-v1", "machines": {MACHINE: {
                "expected_sessions": ["s1"], "runs": [{"run_dir": RUN, "sessions": ["s1"]}]}},
                "expected_rows": {"T1": [{"log2(N)": [20], "T": [1]}, {"log2(N)": [22], "T": [8]}]}}, f)
        a = self.fx.analyzer(analyze.Selection(path))
        with contextlib.redirect_stdout(io.StringIO()):
            a.generate_T1_headline_encoding()
        errors = a._check_expected_rows([])
        self.assertEqual(len(errors), 1)
        self.assertIn("T1|22,8|(row)", errors[0])
        with open(path, "w") as f:
            json.dump({"schema_version": "rbd-selection-v1", "expected_rows": {"T1": [{}]}}, f)
        with self.assertRaises(ValueError):
            analyze.Selection(path)

    def test_real_selection_expects_the_rh_rows_in_t6(self):
        real = analyze.Selection(analyze.DEFAULT_SELECTION)
        specs = real.expected_rows["T6"]
        rh = [s for s in specs if "Original LCPC fork-flags-as-is" in s["Implementation"]]
        self.assertEqual(len(rh), 1)
        self.assertEqual(sorted(rh[0]["Implementation"]),
                         ["Original LCPC as-published-portable", "Original LCPC fork-flags-as-is"])
        self.assertEqual(rh[0]["log2(N)"], list(analyze.SECONDARY_LCPC_SIZES))
        # Every session measures SIMD-Spielman's setup and peak RSS at 2^28.
        self.assertIsNone(real.omission_for("T8|SIMD-Spielman,28|Peak RSS (MB)"))
        ft127 = [s for s in specs if "This Work Ft127" in s["Implementation"]]
        self.assertEqual(len(ft127), 1)
        self.assertEqual(ft127[0]["log2(N)"], list(analyze.FT127_SIZES))


if __name__ == "__main__":
    unittest.main(verbosity=2)
