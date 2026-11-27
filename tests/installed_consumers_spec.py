"""Targeted provenance/rejection checks; real compiler canaries run in the gate."""
import io
import json
from pathlib import Path
import sys
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from installed_consumers import Runner, job_object_cells, runtime
from installed_consumer_packages import check_lock, file_hash, unpack, verify_install


class InstalledConsumersSpec(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def archive(self, extras=None):
        archive = self.root / "sample.crate"
        files = {"sample-1.0.0/Cargo.toml": b'[package]\nname="sample"\nversion="1.0.0"\n',
                 "sample-1.0.0/src/lib.rs": b"pub fn retained() {}\n"}
        files.update(extras or {})
        with tarfile.open(archive, "w:gz") as output:
            for name, data in files.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                output.addfile(member, io.BytesIO(data))
        return archive

    def test_job_object_receipt_requires_all_three_installed_cases(self):
        cases = job_object_cells()
        self.assertEqual(cases, (
            "daemon-survives-launcher", "pane-descendant-cleanup", "parent-denies-breakaway",
        ))

    def test_job_object_receipt_refuses_missing_duplicate_and_failed_cells(self):
        from installed_job_objects import validate_cells
        records = [{"id": cell, "status": "pass"} for cell in job_object_cells()]
        validate_cells(records)
        for invalid in (records[:-1], records + [records[0]],
                        [dict(record, status="incomplete") for record in records]):
            with self.assertRaises(ValueError):
                validate_cells(invalid)

    def test_job_object_fixture_or_wrong_sha_never_becomes_installed_proof(self):
        from installed_job_objects import require_package
        head = "a" * 40
        metadata = {"git_commit": head, "artifact_kind": "windows-package-binary",
                    "configuration": "release", "release_artifact": True, "rmux_version": "0.10.0"}
        require_package(metadata, head, "signed-local", "")
        with self.assertRaisesRegex(ValueError, "verified hosted provenance"):
            require_package(metadata, head, "rc", "v0.10.0-rc.1")
        for wrong in (dict(metadata, configuration="debug"),
                      dict(metadata, release_artifact=False), dict(metadata, git_commit="b" * 40)):
            with self.assertRaisesRegex(ValueError, "identity"):
                require_package(wrong, head, "rc", "v0.10.0-rc.1")
        for ref in ("v0.10.0", "v0.10.0-rc.0", "v0.9.0-rc.1"):
            with self.assertRaisesRegex(ValueError, "RC authority"):
                require_package(metadata, head, "rc", ref)
        with self.assertRaisesRegex(ValueError, "authority"):
            require_package(metadata, head, "fixture", "v0.10.0-rc.1")

    @unittest.skipIf(sys.platform == "win32", "native mode requires an artifact plan")
    def test_windows_mode_reaches_native_authority_check_without_pythonpath(self):
        source = Path(__file__).resolve().parents[1]
        import os
        env = {key: value for key, value in os.environ.items() if key != "PYTHONPATH"}
        result = subprocess.run([sys.executable, str(source / "scripts/installed_consumers.py"),
            "--windows-job-objects", str(self.root / "absent.json")], env=env,
            capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires native Windows", result.stderr)
        self.assertNotIn("ModuleNotFoundError", result.stderr)

    def test_installed_embedded_commit_and_platform_must_match_head(self):
        from installed_job_objects import require_embedded_head
        binary = self.root / "image"
        head = "a" * 40
        marker = f"RMUX_BUILD_IDENTITY_V1:{head}:{'b' * 64}:windows:END".encode()
        binary.write_bytes(marker)
        require_embedded_head(binary, head)
        for payload in (b"unmarked", marker.replace(head.encode(), b"c" * 40),
                        marker.replace(b"windows", b"linux"), marker + marker.replace(head.encode(), b"c" * 40)):
            binary.write_bytes(payload)
            with self.assertRaisesRegex(ValueError, "embedded commit/platform"):
                require_embedded_head(binary, head)

    @unittest.skipUnless(sys.platform == "win32", "native Windows handles")
    def test_native_collector_job_guard_terminates_its_auxiliary_descendant(self):
        # Auxiliary collector coverage only: this is never an installed RMUX cell.
        from installed_job_objects_windows import Native, Job
        import os, time
        native = Native()
        job = Job(native, False)
        witness = self.root / "child.pid"
        code = ("import subprocess,sys; from pathlib import Path; "
                "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); "
                f"Path({str(witness)!r}).write_text(str(child.pid))")
        descendant = None
        try:
            with (self.root / "launcher.log").open("wb") as log:
                self.assertEqual(job.launch([sys.executable, "-c", code], os.environ.copy(), self.root, log), 0)
            descendant = native.process(int(witness.read_text()))
            self.assertFalse(descendant.exited())
            job.close()
            self.assertTrue(descendant.exited(5))
        finally:
            job.close()
            if descendant is not None:
                descendant.terminate()
                descendant.close()

    def test_archive_bytes_and_checksum_are_preserved(self):
        archive = self.archive()
        root = unpack(archive, self.root / "vendor", "sample", "1.0.0")
        self.assertEqual(json.loads((root / ".cargo-checksum.json").read_text())["package"], file_hash(archive))
        verify_install(root.parent)
        (root / "src/lib.rs").unlink()
        with self.assertRaisesRegex(ValueError, "bytes changed"):
            verify_install(root.parent)

    def test_escaping_member_is_rejected(self):
        archive = self.archive({"sample-1.0.0/../escape": b"bad"})
        with self.assertRaisesRegex(ValueError, "unsafe"):
            unpack(archive, self.root / "vendor", "sample", "1.0.0")

    def test_identity_mismatch_is_rejected(self):
        archive = self.archive({"sample-1.0.0/Cargo.toml": b'[package]\nname="other"\nversion="1.0.0"\n'})
        with self.assertRaisesRegex(ValueError, "identity"):
            unpack(archive, self.root / "vendor", "sample", "1.0.0")

    def test_new_registry_pin_is_rejected(self):
        original, consumer = self.root / "original.lock", self.root / "consumer.lock"
        data = '[[package]]\nname="dep"\nversion="1.0.0"\nsource="registry+example"\nchecksum="aaa"\n'
        original.write_text(data)
        consumer.write_text(data)
        check_lock(consumer, original, [])
        consumer.write_text(data.replace('"aaa"', '"bbb"'))
        with self.assertRaisesRegex(ValueError, "locked identity"):
            check_lock(consumer, original, [])

    def runner(self):
        runner = Runner.__new__(Runner)
        runner.output = self.root
        (self.root / "logs").mkdir(exist_ok=True)
        runner.env = {"PATH": "/usr/bin:/bin"}
        runner.receipt = {"commands": []}
        return runner

    def test_negative_control_requires_nonzero_and_specific_diagnostic(self):
        runner = self.runner()
        source = self.root / "source.rs"
        valid = json.dumps({"$message_type": "diagnostic", "level": "error",
            "code": {"code": "E0432"}, "message": "unresolved import `PaneWidget`",
            "spans": [{"is_primary": True, "file_name": str(source)}]})
        for command in ([sys.executable, "-c", "print('E0432 PaneWidget')"],
                        [sys.executable, "-c", "raise SystemExit(1)"]):
            with self.assertRaisesRegex(ValueError, "unexpected result"):
                runner.run("wrong-rejection", command, reject="PaneWidget", diagnostic="E0432",
                           diagnostic_source=source)
        runner.run("valid-rejection", [sys.executable, "-c", f"import sys; print({valid!r}, file=sys.stderr); sys.exit(1)"],
                   reject="PaneWidget", diagnostic="E0432", diagnostic_source=source)

    @unittest.skipIf(sys.platform == "win32", "Linux Cargo consumer uses process groups")
    def test_timeout_never_becomes_expected_rejection(self):
        runner = self.runner()
        with self.assertRaises(subprocess.TimeoutExpired):
            runner.run("timeout", [sys.executable, "-c", "import time; time.sleep(5)"],
                       timeout=0.05, reject="PaneWidget", diagnostic="E0432",
                       diagnostic_source=self.root / "source.rs")
        self.assertEqual(runner.receipt["commands"][0]["status"], "incomplete")

    def test_runtime_passes_owned_cli_for_projection_and_cleans_up(self):
        # Exercise the real runtime with simulated consumers, daemon and /proc.
        # This checks the fixture's argv boundary, not native Rust execution.
        consumers = ("rmux-sdk/no-default", "rmux-sdk/all-features", "ratatui-rmux/default")
        for index, consumer in enumerate(consumers):
            for outcome in ("success", "projection-error", "cleanup-error"):
                with self.subTest(consumer=consumer, outcome=outcome):
                    runner = self.runner()
                    runner.output = self.root.resolve() / f"{index}-{outcome}"
                    (runner.output / "logs").mkdir(parents=True)
                    runner.receipt["runtime"] = []
                    daemon_binary = runner.output / "owned-rmux"
                    daemon_binary.write_bytes(b"exact private daemon and CLI")
                    binary = runner.output / consumer.replace("/", "-")
                    socket = runner.output / f"s{index}/s"
                    child = Mock(pid=1938)
                    child.poll.return_value = None
                    events = []

                    def launch(command, **kwargs):
                        self.assertEqual(command, [str(daemon_binary), "--__internal-daemon", str(socket)])
                        socket.touch()
                        return child

                    def run(label, command, timeout):
                        if label == f"runtime-{index}":
                            self.assertEqual(timeout, 45)
                            self.assertEqual(command, [binary, socket, daemon_binary])
                            # The shared fixture requires argv[2] for record_panes
                            # and uses its presence to enter projection().
                            events.append(("projection", command[2]))
                            if outcome != "success":
                                raise ValueError("projection failed")
                            events.extend(("session-kill", "shutdown"))
                            socket.unlink()
                            child.poll.return_value = 0
                        elif label == f"cleanup-{index}":
                            self.assertEqual(command, [daemon_binary, "-S", socket, "kill-server"])
                            events.append("kill-server")
                            if outcome == "cleanup-error":
                                raise ValueError("cleanup failed")
                            socket.unlink()
                            child.poll.return_value = 0
                        else:
                            self.assertEqual(label, f"daemon-config-{index}")
                            self.assertEqual(command, [daemon_binary, "-S", socket,
                                                      "set-option", "-s", "exit-empty", "off"])

                    def kill():
                        events.append("forced-kill")
                        socket.unlink()
                        child.poll.return_value = -9

                    child.kill.side_effect = kill
                    child.wait.return_value = 0
                    with patch("installed_consumers.subprocess.Popen", side_effect=launch), \
                         patch("installed_consumers.Path", return_value=daemon_binary) as proc_path, \
                         patch.object(runner, "run", side_effect=run) as commands:
                        if outcome == "success":
                            runtime(runner, daemon_binary, binary, index)
                        else:
                            with self.assertRaisesRegex(ValueError, "projection failed|cleanup failed"):
                                runtime(runner, daemon_binary, binary, index)
                    proc_path.assert_called_once_with("/proc/1938/exe")
                    self.assertEqual(events[0], ("projection", daemon_binary))
                    self.assertEqual(events[1:], {
                        "success": ["session-kill", "shutdown"],
                        "projection-error": ["kill-server"],
                        "cleanup-error": ["kill-server", "forced-kill"],
                    }[outcome])
                    self.assertEqual(commands.call_count, 2 if outcome == "success" else 3)
                    child.wait.assert_called_once_with(**({} if outcome == "cleanup-error" else {"timeout": 10}))
                    self.assertEqual(child.kill.call_count, int(outcome == "cleanup-error"))
                    self.assertFalse(socket.exists())
                    record = runner.receipt["runtime"][0]
                    self.assertEqual(record["status"], "pass" if outcome == "success" else "fail")
                    self.assertEqual(record.get("forced_daemon_cleanup", False), outcome == "cleanup-error")
                    persisted = json.loads((runner.output / "receipt.json").read_text())["runtime"][0]
                    # A cleanup exception currently skips the final save; it
                    # must still propagate and never leave a persisted PASS.
                    self.assertEqual(persisted["status"],
                                     "incomplete" if outcome == "cleanup-error" else record["status"])
                    if outcome == "success":
                        self.assertEqual(record["daemon_exit"], 0)
                    else:
                        self.assertEqual(record["error"], "projection failed")

    def test_frozen_witnesses_match_reviewed_hashes(self):
        source = Path(__file__).resolve().parents[1]
        manifest = json.loads((source / "tests/fixtures/installed-consumers/frozen.json").read_text())
        for path, digest in manifest["files"].items():
            self.assertEqual(file_hash(source / path), digest, path)


if __name__ == "__main__":
    unittest.main()
