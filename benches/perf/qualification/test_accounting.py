#!/usr/bin/env python3
"""Finite synthetic R5-B controls through the production collector; no products."""
import ast
from contextlib import ExitStack
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/bench"))
import bench_unix as b
import qualification as q

EVIDENCE = None


class Fixture:
    def __init__(self, directory, failure=None):
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=False)
        self.out = directory / "artifact.json"
        self.calls = []
        self.cancelled = []
        self.failure = failure

    def factory(self, tool):
        def make(label):
            def operation():
                # Observe the real durable checkpoint from inside the callable.
                saved = json.loads(self.out.read_text())
                assert saved["complete"] is False
                assert saved["attempts"][-1]["status"] == "running"
                assert saved["attempts"][-1]["tool"] == tool
                assert saved["attempts"][-1]["operation"] == label
                self.calls.append((tool, label))
                if self.failure is not None:
                    self.failure(self)
                return float(len(self.calls)) / 10
            operation.cancel = lambda: self.cancelled.append((tool, label))
            return operation
        return {label: make(label) for label in q.population()["tools"][tool]}

    def collect(self, iterations=2, operations=None, tools=None, **extra):
        with ExitStack() as stack:
            stack.enter_context(patch.object(b, "tmux_like_operations", side_effect=lambda a, n: self.factory(a.tool)))
            stack.enter_context(patch.object(b, "zellij_operations", side_effect=lambda exe, n: self.factory("zellij")))
            stack.enter_context(patch.object(b, "screen_operations", side_effect=lambda exe, n: self.factory("screen")))
            stack.enter_context(patch.object(b.shutil, "which", side_effect=lambda name: "/synthetic/" + name))
            stack.enter_context(patch.object(b, "version", return_value="SYNTHETIC, NOT PERFORMANCE"))
            stack.enter_context(patch.object(b, "git", return_value="synthetic-run"))
            for target, replacement in extra.items():
                stack.enter_context(patch.object(b, target, replacement))
            b.collect(self.out, iterations, Path(sys.executable), rmux_layout="synthetic-only",
                      rmux_public_binary=None, rmux_helper_binary=None, rmux_daemon_binary=None,
                      progress_enabled=False, sample_progress=False, only_operations=operations,
                      selected_tools=tools or set(b.TOOLS))

    def artifact(self):
        return json.loads(self.out.read_text())


class Accounting(unittest.TestCase):
    def fixture(self, failure=None):
        return Fixture(EVIDENCE / self._testMethodName, failure)

    def test_population_recipes_and_public_mapping(self):
        legacy = json.loads((ROOT / "benches/perf/driver/population.json").read_text())
        data = q.population()
        self.assertEqual([x["id"] for x in data["operations"]], legacy["operations"])
        self.assertEqual(data["public_operations"], legacy["public_operations"])
        self.assertEqual((len(legacy["operations"]), len(legacy["public_operations"])), (33, 10))
        renderer = ast.parse((ROOT / "scripts/bench/render.py").read_text())
        public = next(ast.literal_eval(n.value) for n in renderer.body if isinstance(n, ast.Assign)
                      and isinstance(n.targets[0], ast.Name) and n.targets[0].id == "VISIBLE_OPERATION_ORDER")
        self.assertEqual(public, legacy["public_operations"])
        q.verify_source(Path(b.__file__).read_text(), b.OPERATIONS)
        self.assertEqual(len(data["tools"]["screen"]), 8)
        self.assertEqual(len(data["tools"]["zellij"]), 10)
        self.assertNotIn("split_window_h_detached_sh", data["tools"]["screen"])
        self.assertNotIn("capture_pane_200x50_scrollback_10k", data["tools"]["zellij"])

    def test_success_all_33_all_tools_exact_order_and_cardinality(self):
        f = self.fixture(); f.collect()
        artifact = f.artifact()
        self.assertTrue(artifact["complete"])
        self.assertEqual(q.validate_artifact(artifact), [])
        self.assertEqual(len(f.calls), 2 * (33 + 33 + 10 + 8))
        self.assertEqual(len(artifact["attempts"]), 168)
        self.assertEqual(len({x["attempt_id"] for x in artifact["attempts"]}), 168)
        self.assertEqual(artifact["unattempted"], [])
        # Independent order oracle from the legacy manifest, not q.schedule.
        legacy = json.loads((ROOT / "benches/perf/driver/population.json").read_text())
        for index, label in enumerate(legacy["operations"]):
            selected = [tool for tool, operation in f.calls if operation == label and tool in ("rmux", "tmux")]
            self.assertEqual(selected, ["rmux", "tmux", "tmux", "rmux"] if index % 2 == 0
                             else ["tmux", "rmux", "rmux", "tmux"])
            for tool in ("rmux", "tmux"):
                row = next(row for row in artifact["operations"] if row["id"] == label)
                self.assertEqual(len(row["metrics"][tool]["samples_ms"]), 2)

    def test_odd_iterations_balanced_and_deterministic(self):
        ids = [label for label, _ in b.OPERATIONS]
        first = q.schedule("fixed", ids, ["rmux", "tmux"], 15)
        self.assertEqual(first, q.schedule("fixed", ids, ["tmux", "rmux"], 15))
        starts = [x["tool"] for x in first if x["order"] == 0]
        self.assertLessEqual(abs(starts.count("rmux") - starts.count("tmux")), 1)
        for label in ids:
            starts = [x["tool"] for x in first if x["operation"] == label and x["order"] == 0]
            self.assertEqual(sorted([starts.count("rmux"), starts.count("tmux")]), [7, 8])

    def reject(self, f, expected_exception=Exception):
        with self.assertRaises(expected_exception):
            f.collect(iterations=3, operations={"new_session_warm_sh"}, tools={"rmux", "tmux"})
        artifact = f.artifact()
        self.assertFalse(artifact["complete"])
        self.assertTrue(q.validate_artifact(artifact))
        return artifact

    def test_real_finite_child_nonzero_preserves_status_and_output(self):
        def fail(f):
            subprocess.run([sys.executable, "-B", "-c", "import sys; print('child-cause',file=sys.stderr);sys.exit(7)"],
                           capture_output=True, check=True, timeout=5)
        f = self.fixture(fail); a = self.reject(f)
        self.assertEqual(a["attempts"][0]["returncode"], 7)
        self.assertIn("child-cause", a["attempts"][0]["stderr"])
        self.assertEqual(len(f.calls), 1)
        self.assertEqual(len(a["unattempted"]), 5)

    def test_simulated_timeout_preserves_cause(self):
        def fail(f):
            raise subprocess.TimeoutExpired(["synthetic"], 2, output=b"partial stdout", stderr=b"timeout cause")
        a = self.reject(self.fixture(fail))
        self.assertEqual(a["attempts"][0]["timeout_s"], 2)
        self.assertEqual(a["attempts"][0]["stdout"], "partial stdout")

    def test_partial_write_exception_keeps_attempt_and_partial_data(self):
        def fail(f):
            (f.directory / "partial.txt").write_text("partial evidence\n")
            raise OSError("after partial write")
        f = self.fixture(fail); a = self.reject(f)
        self.assertEqual((f.directory / "partial.txt").read_text(), "partial evidence\n")
        self.assertEqual(a["attempts"][0]["error_type"], "OSError")
        self.assertIn("after partial write", a["attempts"][0]["error"])

    def test_interrupt_is_preserved_and_not_retried(self):
        def fail(f): raise KeyboardInterrupt("synthetic interruption")
        f = self.fixture(fail); a = self.reject(f, KeyboardInterrupt)
        self.assertEqual(a["attempts"][0]["status"], "interrupted")
        self.assertEqual(len(f.calls), 1)

    def test_checkpoint_error_after_partial_artifact_is_retained(self):
        f = self.fixture(); original = b.write_json_atomic; injected = []
        def write(path, artifact):
            original(path, artifact)
            if not injected and artifact["attempts"] and artifact["attempts"][-1]["status"] == "pass":
                injected.append(True)
                raise OSError("checkpoint after partial artifact")
        with patch.object(b, "write_json_atomic", side_effect=write):
            a = self.reject(f)
        self.assertEqual(len(f.calls), 1)
        self.assertEqual(a["attempts"][0]["status"], "failed")
        self.assertIn("checkpoint after partial artifact", a["attempts"][0]["error"])

    def test_portable_sampler_branch_uses_same_accounting_gate(self):
        f = self.fixture()
        with patch.object(b, "darwin_driver", None):
            f.collect(operations={"new_session_warm_sh"}, tools={"rmux", "tmux"})
        a = f.artifact()
        self.assertEqual(a["driver"], "subprocess")
        self.assertTrue(a["complete"])
        self.assertEqual(q.validate_artifact(a), [])

    def test_complete_samples_in_partial_checkpoint_are_still_rejected(self):
        f = self.fixture(); original = b.write_json_atomic; partial = []
        def write(path, artifact):
            original(path, artifact)
            if (not artifact["complete"] and len(artifact["attempts"]) == 4
                    and len(artifact["operations"][0]["metrics"]) == 2):
                partial.append(copy.deepcopy(artifact))
        with patch.object(b, "write_json_atomic", side_effect=write):
            f.collect(operations={"new_session_warm_sh"}, tools={"rmux", "tmux"})
        self.assertTrue(partial)
        for index, artifact in enumerate(partial):
            self.assertTrue(all(row["status"] == "pass" for row in artifact["attempts"]))
            self.assertIn("sampler finalization not established", q.validate_artifact(artifact))
            (f.directory / f"partial-{index}.json").write_text(json.dumps(artifact, indent=2))
        self.assertTrue(f.artifact()["complete"])
        self.assertTrue(f.artifact()["sampling_finished"])
        self.assertTrue(f.artifact()["cleanup_finished"])

    def test_missing_sampler_result(self):
        f = self.fixture()
        def factory(tool): return {label: (lambda: None) for label in q.population()["tools"][tool]}
        with patch.object(f, "factory", side_effect=factory):
            a = self.reject(f)
        self.assertIn("missing or invalid sampler result", a["attempts"][0]["error"])

    def corrupt(self, change):
        f = self.fixture(); original = q.finish_attempt
        def finish(attempt, value):
            result = original(attempt, value); change(attempt); return result
        with patch.object(q, "finish_attempt", side_effect=finish):
            a = self.reject(f)
        return a

    def test_unknown_attempt_id_rejected_by_real_final_gate(self):
        a = self.corrupt(lambda row: row.update(attempt_id="unknown"))
        self.assertTrue(any("unknown attempt" in error for error in a["accounting_errors"]))

    def test_retry_number_rejected_by_real_final_gate(self):
        a = self.corrupt(lambda row: row.update(attempt_number=2))
        self.assertTrue(any("identity/retry" in error for error in a["accounting_errors"]))

    def test_missing_result_field_rejected_by_real_final_gate(self):
        a = self.corrupt(lambda row: row.pop("elapsed_ms"))
        self.assertTrue(any("missing or invalid" in error for error in a["accounting_errors"]))

    def test_duplicate_retry_refused_before_second_launch(self):
        f = self.fixture(); original = q.schedule
        def duplicate(*args):
            result = original(*args);return [result[0], result[0], *result[1:]]
        with patch.object(q, "schedule", side_effect=duplicate):
            a = self.reject(f)
        self.assertEqual(len(f.calls), 1)
        self.assertIn("retry/duplicate", a["sampler_errors"][0]["error"])
        self.assertTrue(q.validate_artifact(a))  # canonical plan, after fault injection removed

    def test_saved_artifact_missing_duplicate_unknown_and_retry(self):
        f = self.fixture(); f.collect(operations={"new_session_warm_sh"}, tools={"rmux", "tmux"})
        original = f.artifact()
        mutants = {}
        a = copy.deepcopy(original); a["attempts"].pop(); mutants["missing"] = a
        a = copy.deepcopy(original); a["attempts"].append(copy.deepcopy(a["attempts"][0])); mutants["duplicate"] = a
        a = copy.deepcopy(original); a["attempts"][0]["operation"] = "unknown"; mutants["unknown"] = a
        a = copy.deepcopy(original); a["attempts"][0]["status"] = "failed"; a["attempts"].append(copy.deepcopy(original["attempts"][0])); mutants["retry"] = a
        a = copy.deepcopy(original); a["attempts"][0], a["attempts"][1] = a["attempts"][1], a["attempts"][0]; mutants["order"] = a
        a = copy.deepcopy(original); a["operations"][0]["metrics"]["rmux"]["samples_ms"].pop(); mutants["metric"] = a
        for label, artifact in mutants.items():
            errors = q.validate_artifact(artifact)
            self.assertTrue(errors, label)
            (f.directory / (label + ".json")).write_text(json.dumps({"synthetic_mutation": True, "rejections": errors, "artifact": artifact}, indent=2))

    def test_cancellation_failure_prevents_complete(self):
        f = self.fixture(); original = f.factory
        def factory(tool):
            result = original(tool)
            def cancel(): raise OSError("sampler cleanup cause")
            result["new_session_warm_sh"].cancel = cancel
            return result
        with patch.object(f, "factory", side_effect=factory):
            a = self.reject(f)
        self.assertTrue(any(row["status"] == "failed" for row in a["sampler_cleanup"]))
        self.assertTrue(any("sampler cancellation" in error for error in a["accounting_errors"]))

    def test_existing_batch_cleanup_is_called_for_both_tools_after_interrupt(self):
        directory = EVIDENCE / self._testMethodName; directory.mkdir()
        out = directory / "artifact.json"
        opened, cleaned, timed = [], [], []
        def session(adapter, socket, **kwargs): opened.append(socket)
        def timing(command, **kwargs):
            timed.append(command[0])
            if len(timed) == 2: raise KeyboardInterrupt("paired batch interruption")
            return 1.0
        with patch.object(b, "session", side_effect=session), \
             patch.object(b, "setup_list_windows_20"), \
             patch.object(b, "timed", side_effect=timing), \
             patch.object(b.Adapter, "cleanup", autospec=True, side_effect=lambda self, socket: cleaned.append(socket)), \
             patch.object(b.shutil, "which", return_value="synthetic-tmux"), \
             patch.object(b, "version", return_value="synthetic"), \
             self.assertRaises(KeyboardInterrupt):
            b.collect(out, 3, Path("synthetic-rmux"), rmux_layout="synthetic-only",
                      rmux_public_binary=None, rmux_helper_binary=None, rmux_daemon_binary=None,
                      progress_enabled=False, sample_progress=False, only_operations={"list_windows_20"},
                      selected_tools={"rmux", "tmux"})
        self.assertEqual(len(opened), 2)
        self.assertCountEqual(cleaned, opened)
        self.assertEqual(len(timed), 2)
        self.assertFalse(json.loads(out.read_text())["complete"])
        (directory / "fixture.json").write_text(json.dumps({"opened": opened, "cleaned": cleaned, "timed": timed}))

    def test_sampler_hard_exit_keeps_durable_running_attempt(self):
        directory = EVIDENCE / self._testMethodName; directory.mkdir()
        run = subprocess.run([sys.executable, "-B", __file__, "--crash-child", str(directory / "child")],
                             capture_output=True, timeout=10)
        (directory / "process.json").write_text(json.dumps({"returncode": run.returncode,
                     "stdout": run.stdout.decode(), "stderr": run.stderr.decode(), "product_execution": False}))
        self.assertEqual(run.returncode, 7)
        artifact = json.loads((directory / "child/artifact.json").read_text())
        self.assertFalse(artifact["complete"])
        self.assertEqual(artifact["attempts"][0]["status"], "running")
        self.assertTrue(q.validate_artifact(artifact))

    def test_optional_absent_tools_remain_explicitly_unavailable(self):
        f = self.fixture()
        f.collect(operations={"new_session_warm_sh"}, tools={"rmux", "zellij", "screen"},
                  **{"shutil": type("Lookup", (), {"which": staticmethod(lambda _: None)})})
        a = f.artifact()
        self.assertTrue(a["complete"])
        self.assertEqual(a["tools"], ["rmux"])
        self.assertEqual(a["unavailable_tools"], ["zellij", "screen"])


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--crash-child":
        f = Fixture(Path(sys.argv[2]), lambda _: os._exit(7))
        f.collect(operations={"new_session_warm_sh"}, tools={"rmux", "tmux"})
    else:
        if len(sys.argv) != 2:
            raise SystemExit("usage: test_accounting.py NEW_EVIDENCE_DIRECTORY")
        EVIDENCE = Path(sys.argv[1]); EVIDENCE.mkdir(parents=True, exist_ok=False)
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Accounting))
        (EVIDENCE / "summary.json").write_text(json.dumps({"tests": result.testsRun,
            "pass": result.wasSuccessful(), "failures": len(result.failures), "errors": len(result.errors),
            "product_runs": 0, "performance_evidence": False,
            "source_sha256": hashlib.sha256(Path(b.__file__).read_bytes()).hexdigest(),
            "qualification_sha256": hashlib.sha256(Path(q.__file__).read_bytes()).hexdigest()}, indent=2))
        raise SystemExit(0 if result.wasSuccessful() else 1)
