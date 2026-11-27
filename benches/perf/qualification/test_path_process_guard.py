#!/usr/bin/env python3
"""The path fixture permits provenance reads while keeping host/process bans."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import test_tool_paths as paths


class ProcessGuardControls(unittest.TestCase):
    def command(self, *arguments):
        return ["git", "-C", str(paths.REPOSITORY), *arguments]

    def test_exact_provenance_reads_are_forwarded(self):
        commands = paths.PROVENANCE_COMMANDS | {
            ("show", "a" * 40 + ":" + path) for path in paths.HISTORY_PATHS
        }
        for command in sorted(commands):
            with self.subTest(command=command), patch.object(paths, "POPEN") as popen:
                argv = self.command(*command)
                options = {"stdout": subprocess.PIPE, "stderr": subprocess.PIPE}
                self.assertIs(paths.provenance_popen(argv, **options), popen.return_value)
                popen.assert_called_once_with(argv, **options)

    def test_neighbor_commands_are_rejected_before_launch(self):
        commands = [
            ["rmux", "-V"], ["tmux", "-V"], ["sh", "-c", "true"],
            self.command("status"), self.command("fetch"),
            self.command("cat-file", "blob", "0" * 40),
            self.command("cat-file", "-p", paths.POPULATION_BLOB),
            self.command("rev-list", "--full-history", "--all"),
            self.command("rev-parse", "HEAD"),
            self.command("show", "HEAD:" + paths.HISTORY_PATHS[0]),
            self.command("show", "a" * 40 + ":other-file"),
            self.command("show", "a" * 39 + ":" + paths.HISTORY_PATHS[0]),
            self.command("log", "--full-history", "--format=%H", "-66", "HEAD", "--", paths.HISTORY_PATHS[0]),
            self.command("log", "--full-history", "--format=%H", "-65", "HEAD", "--", "other-file"),
            ["git", "-C", "/other-repository", "cat-file", "blob", paths.POPULATION_BLOB],
            self.command("cat-file", "blob", paths.POPULATION_BLOB, "--extra"),
            "git log", [], [None],
        ]
        for argv in commands:
            with self.subTest(argv=argv), patch.object(paths, "POPEN") as popen:
                with self.assertRaisesRegex(AssertionError, "host lookup or process launch"):
                    paths.provenance_popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                popen.assert_not_called()

    def test_launch_overrides_are_rejected(self):
        argv = self.command("cat-file", "blob", paths.POPULATION_BLOB)
        for extra in ({"shell": True}, {"executable": "sh"}, {"env": {}},
                      {"cwd": "/"}, {"preexec_fn": lambda: None}):
            with self.subTest(extra=extra), patch.object(paths, "POPEN") as popen:
                with self.assertRaises(AssertionError):
                    paths.provenance_popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **extra)
                popen.assert_not_called()

    def test_offline_guard_has_no_provenance_exception(self):
        with patch.object(paths, "POPEN") as popen:
            for args in ((), ("tmux",), (self.command("cat-file", "blob", paths.POPULATION_BLOB),)):
                with self.subTest(args=args), self.assertRaises(AssertionError):
                    paths.forbidden(*args)
            popen.assert_not_called()

    def test_offline_validation_rejects_injected_process(self):
        saved_errors = paths.t.saved_errors
        attempts = []

        def injected(*args, **kwargs):
            # Collection and ordinary validation run before the offline block.
            if isinstance(Path.stat, Mock):
                attempts.append("offline")
                subprocess.run([sys.executable, "-c", "pass"], check=True)
            return saved_errors(*args, **kwargs)

        with tempfile.TemporaryDirectory() as directory:
            with patch.object(paths, "EVIDENCE", Path(directory)), \
                    patch.object(paths.t, "saved_errors", side_effect=injected):
                result = unittest.TestResult()
                paths.PathControls(
                    "test_offline_validation_ignores_review_cwd_and_tool_files"
                ).run(result)
        self.assertEqual(attempts, ["offline"])
        self.assertEqual(result.testsRun, 1)
        self.assertEqual(result.errors, [])
        self.assertEqual(len(result.failures), 1)
        self.assertIn("Pure path control attempted host lookup or process launch",
                      result.failures[0][1])


if __name__ == "__main__":
    unittest.main(verbosity=2)
