#!/usr/bin/env python3
"""Native bounded R5A-R2-CLEANUP-001 regressions, with no performance claim."""

from datetime import datetime, timezone
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/bench"))
import bench_unix as bench
import darwin_driver as driver
from test_darwin_driver import fds

EVIDENCE = []


class Cleanup(unittest.TestCase):
    def case(self, mode, operation="list_sessions_default", *, batch=False, setup=False, collect=False):
        before_fds = fds()
        report = {"mode": mode, "operation": operation,
                  "started_at": datetime.now(timezone.utc).isoformat(), "launches": []}
        queues = {}
        real_run = driver.run
        with tempfile.TemporaryDirectory(prefix="r5-cleanup-", dir="/tmp") as directory:
            state = Path(directory) / "state.json"
            fixture = Path(directory) / "cli.py"
            source = Path(__file__).with_name("cleanup_fixture.py").read_text()
            fixture.write_text(f"#!{sys.executable}\n" + source.split("\n", 1)[1])
            fixture.chmod(0o700)
            env = {**bench.clean_env(), "R5_FIXTURE_STATE": str(state),
                   "R5_FIXTURE_MODE": mode}
            adapter = bench.Adapter("rmux", fixture, cleanup_timeout=0.3)

            def observed_run(command, **kwargs):
                entry = {"command": command[3], "arguments": command[4:]}
                report["launches"].append(entry)
                try:
                    result = real_run(command, **kwargs)
                except Exception as error:
                    entry.update(bench.failure_details(error))
                    raise
                entry.update(returncode=result.returncode,
                             stdout=result.stdout.decode(errors="replace"),
                             stderr=result.stderr.decode(errors="replace"))
                if command[3] in ("new-session", "start-server") and state.exists():
                    pid = json.loads(state.read_text())["pid"]
                    if pid not in queues:
                        queue = select.kqueue()
                        queue.control([select.kevent(pid, filter=select.KQ_FILTER_PROC,
                                                     flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                                     fflags=select.KQ_NOTE_EXIT)], 0, 0)
                        queues[pid] = queue
                return result

            def small_setup(a, socket):
                if setup:
                    bench.quiet(a.command(socket, ["new-window", "-d"]))

            attempts = []
            try:
                with patch.object(bench, "clean_env", return_value=env), \
                        patch.object(driver, "run", side_effect=observed_run), \
                        patch.object(bench, "setup_list_windows_20", side_effect=small_setup), \
                        patch.object(bench, "setup_list_panes_80", side_effect=small_setup):
                    action = (getattr(bench, operation)(adapter, 1) if batch
                              else lambda: getattr(bench, operation)(adapter))
                    iterations = 1 if mode == "success" else 3
                    report["requested_iterations"] = iterations
                    if collect:
                        out = Path(directory) / "incomplete.json"
                        with patch.object(bench, "Adapter", return_value=adapter), \
                                patch.object(bench, "version", return_value="synthetic fixture"), \
                                self.assertRaises(RuntimeError) as raised:
                            bench.collect(out, iterations, fixture, rmux_layout="synthetic fixture",
                                          rmux_public_binary=None, rmux_helper_binary=None,
                                          rmux_daemon_binary=None, progress_enabled=False,
                                          sample_progress=False, only_operations={operation},
                                          selected_tools={"rmux"})
                        report["collector_artifact"] = json.loads(out.read_text())
                        self.assertFalse(report["collector_artifact"]["complete"])
                        self.assertEqual(report["collector_artifact"]["operations"][0]["metrics"], {})
                        attempts = report["collector_artifact"]["attempts"]
                        result, error = None, str(raised.exception)
                    else:
                        result, error = bench.measure(action, iterations, label=operation, tool="rmux",
                                                      progress_enabled=False, sample_progress=False,
                                                      attempts=attempts)
                report["returned_metric"] = result is not None
                report["error"] = error
                # Do not publish elapsed values from a functional canary.
                report["attempts"] = [{k: v for k, v in row.items() if k != "elapsed_ms"}
                                      for row in attempts]
                self.assertFalse(adapter._daemons)
            finally:
                report["daemons"] = []
                for pid, queue in queues.items():
                    events = queue.control(None, 1, 0.3)
                    exited = bool(events and events[0].ident == pid
                                  and events[0].fflags & select.KQ_NOTE_EXIT)
                    record = {"pid": pid, "candidate_exit_observed": exited,
                              "test_rescue_used": False}
                    if not exited:
                        record["test_rescue_used"] = True
                        try:
                            os.kill(pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        record["rescue_exit_observed"] = bool(queue.control(None, 1, 2))
                    queue.close()
                    report["daemons"].append(record)
                report["completed_at"] = datetime.now(timezone.utc).isoformat()
                EVIDENCE.append(report)
        self.assertEqual(fds(), before_fds)
        self.assertTrue(all(d["candidate_exit_observed"] for d in report["daemons"]), report)
        return report

    def assert_cleanup_failure(self, report, *, primary=None, cleanup_code=7):
        self.assertFalse(report["returned_metric"])
        self.assertEqual(len(report["attempts"]), 1)
        attempt = report["attempts"][0]
        self.assertEqual(attempt["status"], "failed")
        if primary is not None:
            self.assertEqual(attempt["returncode"], primary)
            self.assertIn("primary", attempt["stderr"])
            attempt = attempt["cleanup_error"]
        self.assertEqual(attempt["error_type"], "FixtureCleanupError")
        cleanup = attempt["cleanup"]
        if cleanup_code is not None:
            self.assertEqual(cleanup["command_error"]["returncode"], cleanup_code)
            self.assertIn("R2 intentional cleanup failure", cleanup["command_error"]["stderr"])
        if report["daemons"]:
            self.assertFalse(cleanup["exit_observed"])
            self.assertTrue(cleanup["forced_cleanup_exit_observed"])
        self.assertEqual(sum(r["command"] == "kill-server" for r in report["launches"]), 1)

    def test_successful_operation_cleanup7_alive_is_rejected(self):
        self.assert_cleanup_failure(self.case("cleanup_fail"))

    def test_initial_absence_then_successful_cleanup(self):
        report = self.case("success", "new_session_cold_sh")
        self.assertTrue(report["returned_metric"])
        kills = [r["returncode"] for r in report["launches"] if r["command"] == "kill-server"]
        self.assertEqual(kills, [1, 0])

    def test_successful_cleanup(self):
        self.assertTrue(self.case("success")["returned_metric"])

    def test_double_failure_preserves_primary(self):
        self.assert_cleanup_failure(self.case("double_fail"), primary=9)

    def test_collector_publishes_incomplete_artifact_on_cleanup_failure(self):
        self.assert_cleanup_failure(self.case("cleanup_fail", collect=True))

    def test_zero_status_but_surviving_daemon_is_rejected(self):
        self.assert_cleanup_failure(self.case("false_success"), cleanup_code=None)

    def test_nonzero_cleanup_is_retained_even_if_daemon_exits(self):
        report = self.case("cleanup_fail_after_exit")
        self.assertFalse(report["returned_metric"])
        cleanup = report["attempts"][0]["cleanup"]
        self.assertEqual(cleanup["command_error"]["returncode"], 7)
        self.assertTrue(cleanup["exit_observed"])
        self.assertNotIn("forced_cleanup_attempted", cleanup)

    def test_absence_diagnostic_requires_exact_socket_and_exit_evidence(self):
        command = ["fixture", "-L", "owned", "kill-server"]
        result = subprocess.CompletedProcess(command, 1, b"", b"no server running on /fixture/owned\n")
        self.assertFalse(bench.Adapter._absent(result))
        self.assertTrue(bench.Adapter._absent(result, exited=True))
        result.stderr = b"no server running on /fixture/someone-else\n"
        self.assertFalse(bench.Adapter._absent(result, exited=True))
        result.stderr = b"error connecting to /fixture/owned (Permission denied)\n"
        self.assertFalse(bench.Adapter._absent(result, exited=True))

    def test_initial_cleanup7_is_not_absence(self):
        self.assert_cleanup_failure(self.case("initial_fail", "new_session_cold_sh"))

    def test_primary_failure_with_successful_cleanup(self):
        report = self.case("operation_fail")
        self.assertFalse(report["returned_metric"])
        attempt = report["attempts"][0]
        self.assertEqual(attempt["returncode"], 9)
        self.assertNotIn("cleanup_error", attempt)

    def test_operation_itself_retires_fixture(self):
        report = self.case("success", "kill_server")
        self.assertTrue(report["returned_metric"])

    def test_batched_double_failures_preserve_primary(self):
        for operation in ("batched_list_windows_20", "batched_list_panes_80"):
            with self.subTest(operation=operation):
                self.assert_cleanup_failure(self.case("double_fail", operation, batch=True), primary=9)

    def test_batched_setup_failures_preserve_primary(self):
        for operation in ("batched_list_windows_20", "batched_list_panes_80"):
            with self.subTest(operation=operation):
                self.assert_cleanup_failure(self.case("setup_fail", operation, batch=True, setup=True), primary=10)


if __name__ == "__main__":
    if sys.platform != "darwin":
        raise SystemExit("native Darwin required")
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(Cleanup)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    out = os.environ.get("R5_CLEANUP_EVIDENCE")
    if out:
        Path(out).write_text(json.dumps({"passed": result.wasSuccessful(),
                                       "performance_evidence": False, "cases": EVIDENCE}, indent=2) + "\n")
    raise SystemExit(0 if result.wasSuccessful() else 1)
