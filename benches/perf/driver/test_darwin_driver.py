#!/usr/bin/env python3
"""Native, functional R5-A canaries. No speed assertions or benchmark report."""

import ast
import errno
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/bench"))
import darwin_driver as driver
import bench_unix as bench


def fds():
    result = set()
    for name in os.listdir("/dev/fd"):
        try:
            os.fstat(int(name))
        except OSError:
            continue
        result.add(int(name))
    return result


class NativeDriver(unittest.TestCase):
    def run_child(self, script, **kwargs):
        return driver.run([sys.executable, "-B", "-c", script],
                          env=bench.clean_env(), **kwargs)

    def assert_reaped(self, pid):
        with self.assertRaises(ChildProcessError):
            os.waitpid(pid, os.WNOHANG)

    def test_old_polling_canary_new_native_wait(self):
        calls = []
        real_sleep = time.sleep
        with patch.object(time, "sleep", side_effect=lambda delay: (calls.append(delay), real_sleep(delay))):
            subprocess.run(["/bin/sleep", "0.02"], timeout=2, check=True)
        self.assertTrue(calls, "the old timeout wait must exercise its polling canary")
        waits = []
        native_waitpid = os.waitpid

        def observed_waitpid(pid, flags):
            waits.append((pid, flags))
            self.assertGreater(pid, 0)
            self.assertEqual(flags, 0, "the real driver must use blocking, owned waitpid")
            return native_waitpid(pid, flags)

        with patch.object(time, "sleep", side_effect=AssertionError("polling in native driver")), \
                patch.object(os, "waitpid", side_effect=observed_waitpid):
            result = driver.run(["/bin/sleep", "0.02"], env=bench.clean_env(), timeout=2)
        self.assertEqual(waits, [(result.pid, 0)])
        self.assert_reaped(result.pid)

    def test_immediate_exit_and_nonzero_preserve_output(self):
        for _ in range(20):
            result = driver.run(["/usr/bin/true"], env=bench.clean_env())
            self.assertEqual(result.returncode, 0)
            self.assert_reaped(result.pid)
        with self.assertRaises(subprocess.CalledProcessError) as raised:
            self.run_child("import sys; print('out'); print('err',file=sys.stderr); sys.exit(7)")
        self.assertEqual(raised.exception.returncode, 7)
        self.assertEqual(raised.exception.stdout, b"out\n")
        self.assertEqual(raised.exception.stderr, b"err\n")
        self.assert_reaped(raised.exception.pid)

    def test_timeout_kills_owned_child_and_keeps_failure(self):
        with self.assertRaises(subprocess.TimeoutExpired) as raised:
            self.run_child("import time; print('before-timeout',flush=True); time.sleep(30)", timeout=0.2)
        self.assertEqual(raised.exception.stdout, b"before-timeout\n")
        self.assertEqual(raised.exception.timeout, 0.2)
        self.assert_reaped(raised.exception.pid)
        with self.assertRaises(ProcessLookupError):
            os.kill(raised.exception.pid, 0)

    def test_unrelated_child_is_not_collected_or_signalled(self):
        other = os.posix_spawn("/bin/sh", ["/bin/sh", "-c", "exit 23"], bench.clean_env())
        try:
            with self.assertRaises(subprocess.TimeoutExpired):
                driver.run(["/bin/sleep", "30"], env=bench.clean_env(), timeout=0.05)
        finally:
            pid, status = os.waitpid(other, 0)
        self.assertEqual(pid, other)
        self.assertEqual(os.waitstatus_to_exitcode(status), 23)

    def assert_process_exited(self, pid):
        queue = select.kqueue()
        try:
            try:
                queue.control([select.kevent(pid, filter=select.KQ_FILTER_PROC,
                                             flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                             fflags=select.KQ_NOTE_EXIT)], 0, 0)
            except ProcessLookupError:
                return
            events = queue.control(None, 1, 1)
            if not events:
                os.kill(pid, signal.SIGKILL)  # clean up only the observed fixture on failure
            self.assertTrue(events, "fixture descendant survived group cleanup")
            self.assertTrue(events[0].fflags & select.KQ_NOTE_EXIT)
        finally:
            queue.close()

    def test_failed_child_does_not_leave_its_background_process(self):
        with self.assertRaises(subprocess.CalledProcessError) as raised:
            driver.run(["/bin/sh", "-c", "sleep 30 & printf '%s' \"$!\"; exit 7"],
                       env=bench.clean_env())
        self.assertEqual(raised.exception.returncode, 7)
        self.assert_reaped(raised.exception.pid)
        self.assert_process_exited(int(raised.exception.stdout))

    def test_timeout_also_terminates_background_process(self):
        with self.assertRaises(subprocess.TimeoutExpired) as raised:
            driver.run(["/bin/sh", "-c", "sleep 30 & printf '%s' \"$!\"; wait"],
                       env=bench.clean_env(), timeout=0.1)
        self.assert_reaped(raised.exception.pid)
        self.assert_process_exited(int(raised.exception.stdout))

    def test_launch_errors_and_invalid_timeout_have_no_fd_leak(self):
        initial = fds()
        with self.assertRaises(FileNotFoundError):
            driver.run(["/example-no-such-executable"], env=bench.clean_env())
        with tempfile.NamedTemporaryFile() as nonexecutable:
            with self.assertRaises(PermissionError):
                driver.run([nonexecutable.name], env=bench.clean_env())
        for timeout in (0, -1, float("nan"), float("inf")):
            with self.assertRaises(ValueError):
                driver.run(["/usr/bin/true"], env=bench.clean_env(), timeout=timeout)
        self.assertEqual(fds(), initial)

    def test_spawn_error_after_preparation_closes_resources(self):
        initial = fds()
        with patch.object(os, "posix_spawn", side_effect=PermissionError(errno.EACCES, "canary")):
            with self.assertRaises(PermissionError):
                driver.run(["/usr/bin/true"], env=bench.clean_env())
        self.assertEqual(fds(), initial)

    def test_external_reaper_is_rejected_before_spawn(self):
        with patch.object(signal, "getsignal", return_value=signal.SIG_IGN), \
                patch.object(os, "posix_spawn") as spawn:
            with self.assertRaisesRegex(RuntimeError, "exclusive collection"):
                driver.run(["/usr/bin/true"], env=bench.clean_env())
            spawn.assert_not_called()

    def test_event_error_cleans_child(self):
        children = []
        spawn = os.posix_spawn

        def observe_spawn(*args, **kwargs):
            pid = spawn(*args, **kwargs)
            children.append(pid)
            return pid

        with patch.object(os, "posix_spawn", side_effect=observe_spawn), \
                patch.object(driver, "_event", side_effect=RuntimeError("event canary")):
            with self.assertRaisesRegex(RuntimeError, "event canary"):
                driver.run(["/bin/sleep", "30"], env=bench.clean_env())
        self.assertEqual(len(children), 1)
        self.assert_reaped(children[0])

    def test_output_larger_than_pipe_capacity_does_not_deadlock(self):
        result = self.run_child("import os; os.write(1,b'x'*1048576); os.write(2,b'y'*1048576)", timeout=5)
        self.assertEqual(result.stdout, b"x" * 1048576)
        self.assertEqual(result.stderr, b"y" * 1048576)

    def test_inheritable_descriptor_does_not_reach_child(self):
        import fcntl
        with tempfile.TemporaryFile() as stream:
            fd = fcntl.fcntl(stream.fileno(), fcntl.F_DUPFD, 256)
            try:
                os.set_inheritable(fd, True)
                code = (f"import os,sys\ntry: os.fstat({fd})\n"
                        "except OSError: sys.exit(0)\nelse: sys.exit(42)\n")
                self.run_child(code)
                os.fstat(fd)  # the parent's descriptor remains intact
            finally:
                os.close(fd)

    def test_repeated_success_and_timeout_no_parent_fd_leak(self):
        initial = fds()
        for _ in range(10):
            driver.run(["/usr/bin/true"], env=bench.clean_env())
            with self.assertRaises(subprocess.TimeoutExpired):
                driver.run(["/bin/sleep", "5"], env=bench.clean_env(), timeout=0.01)
        self.assertEqual(fds(), initial)


class NativeReadiness(unittest.TestCase):
    def signal(self, ready, frame=None):
        with open(ready.path, "wb", buffering=0) as stream:
            stream.write(frame if frame is not None else (ready.challenge + "\n").encode())

    def test_fresh_producer_signal_and_single_use(self):
        with driver.Readiness() as ready:
            self.assertEqual(ready.path.stat().st_mode & 0o777, 0o600)
            driver.run(["/bin/sh", "-c", ready.shell_signal], env=bench.clean_env())
            ready.wait(1)
            with self.assertRaisesRegex(RuntimeError, "already consumed"):
                ready.wait(1)
        self.assertFalse(ready.path.exists())

    def test_wait_really_blocks_until_producer_signals(self):
        with driver.Readiness() as ready:
            errors = []
            finished = threading.Event()

            def reader():
                try:
                    ready.wait(2)
                except BaseException as error:
                    errors.append(error)
                finally:
                    finished.set()

            thread = threading.Thread(target=reader)
            thread.start()
            try:
                self.assertFalse(finished.wait(0.05), "readiness returned before any producer signal")
                self.signal(ready)
                self.assertTrue(finished.wait(2))
            finally:
                thread.join(3)
            self.assertFalse(thread.is_alive())
            self.assertFalse(errors, errors)

    def test_stale_false_and_extra_readiness_are_terminal(self):
        with driver.Readiness() as previous:
            stale = (previous.challenge + "\n").encode()
        for frame in (stale, b"ready\n", b"", b"x" * 4096):
            with self.subTest(frame=frame[:10]), driver.Readiness() as ready:
                self.signal(ready, frame)
                with self.assertRaises((ValueError, TimeoutError)):
                    ready.wait(0.03)
                self.signal(ready)
                with self.assertRaisesRegex(RuntimeError, "already consumed"):
                    ready.wait(1)
        with driver.Readiness() as ready:
            self.signal(ready, (ready.challenge + "\nextra").encode())
            with self.assertRaises(ValueError):
                ready.wait(1)

    def test_cached_stdout_is_not_readiness(self):
        with driver.Readiness() as ready:
            driver.run(["/bin/echo", ready.challenge], env=bench.clean_env())
            with self.assertRaises(TimeoutError):
                ready.wait(0.03)

    def test_partial_frame_and_missing_suffix(self):
        with driver.Readiness() as ready:
            self.signal(ready, ready.challenge[:10].encode())
            with self.assertRaises(TimeoutError):
                ready.wait(0.03)

    def test_readiness_timeout_and_fd_cleanup(self):
        initial = fds()
        for _ in range(10):
            with driver.Readiness() as ready:
                with self.assertRaises(TimeoutError):
                    ready.wait(0.005)
        self.assertEqual(fds(), initial)


class AdapterContract(unittest.TestCase):
    def test_population_and_measured_arguments_are_preserved(self):
        frozen = json.loads(Path(__file__).with_name("population.json").read_text())
        self.assertEqual([label for label, _ in bench.OPERATIONS], frozen["operations"])
        self.assertEqual(len(bench.OPERATIONS), 33)
        render = ast.parse((ROOT / "scripts/bench/render.py").read_text())
        visible = next(n.value for n in render.body if isinstance(n, ast.Assign)
                       and isinstance(n.targets[0], ast.Name) and n.targets[0].id == "VISIBLE_OPERATION_ORDER")
        self.assertEqual(ast.literal_eval(visible), frozen["public_operations"])
        self.assertEqual(len(frozen["public_operations"]), 10)
        for node in ast.parse((ROOT / "scripts/bench/bench_unix.py").read_text()).body:
            if not isinstance(node, ast.FunctionDef) or node.name not in frozen["measured_arguments_ast"]:
                continue
            calls = []
            for call in ast.walk(node):
                if isinstance(call, ast.Call) and isinstance(call.func, ast.Name):
                    if call.func.id == "timed":
                        calls.append(ast.dump(call.args[0]))
                    if call.func.id == "with_session":
                        calls.append(ast.dump(call.args[2]))
            self.assertEqual(calls, frozen["measured_arguments_ast"][node.name], node.name)

    def test_rmux_tmux_share_native_launch_and_no_polling(self):
        launches = []
        spawn = os.posix_spawn

        def observe(*args, **kwargs):
            launches.append(args[0])
            return spawn(*args, **kwargs)

        with patch.object(os, "posix_spawn", side_effect=observe), \
                patch.object(time, "sleep", side_effect=AssertionError("polling")):
            for tool in ("rmux", "tmux"):
                adapter = bench.Adapter(tool, "/usr/bin/true")
                bench.timed(adapter.command("fixture", ["list-sessions"]))
        self.assertEqual(launches, ["/usr/bin/true", "/usr/bin/true"])

    def test_failed_sample_retains_attempts_and_does_not_retry(self):
        attempts = []
        calls = []

        def operation():
            calls.append(1)
            if len(calls) == 1:
                return 1.0
            raise subprocess.CalledProcessError(7, ["fixture"], b"out", b"err")

        value, error = bench.measure(operation, 3, label="fixture", tool="rmux",
                                    progress_enabled=False, sample_progress=False, attempts=attempts)
        self.assertIsNone(value)
        self.assertIn("CalledProcessError", error)
        self.assertEqual(len(calls), 2)
        self.assertEqual([a["status"] for a in attempts], ["pass", "failed"])
        self.assertEqual(attempts[-1]["returncode"], 7)
        self.assertEqual(attempts[-1]["stderr"], "err")

    def test_collector_failure_remains_incomplete_and_raises(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "failure.json"
            with self.assertRaises(RuntimeError):
                bench.collect(out, 3, Path("/usr/bin/false"), rmux_layout="fixture",
                              rmux_public_binary=None, rmux_helper_binary=None, rmux_daemon_binary=None,
                              progress_enabled=False, sample_progress=False,
                              only_operations={"list_commands"}, selected_tools={"rmux"})
            payload = json.loads(out.read_text())
            self.assertFalse(payload["complete"])
            self.assertEqual(len(payload["attempts"]), 1)
            self.assertEqual(payload["attempts"][0]["returncode"], 1)
            self.assertEqual(payload["operations"][0]["id"], "list_commands")


if __name__ == "__main__":
    if sys.platform != "darwin":
        raise SystemExit("This qualification requires native Darwin; no portable PASS or skip.")
    unittest.main(verbosity=2)
