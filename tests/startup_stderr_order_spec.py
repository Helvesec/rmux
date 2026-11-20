"""Execute the Windows callers' actual assertion blocks on synthetic Output.

This portable regression does not start rmux or claim Windows native coverage.
Run with RUSTC and CARGO_TARGET_DIR pointing to the chosen compiler/private target.
"""

import json
import os
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "tests/internal_daemon_windows.rs"
HELPER = ROOT / "tests/support/startup_stderr.rs"


class StartupStderrOrder(unittest.TestCase):
    def check_caller(self, name, end_marker, unexpected_code):
        # Extract unchanged executable statements, not a reimplementation of
        # their ordering. Missing/ambiguous boundaries must fail the test.
        source = SOURCE.read_text()
        signature = f"fn {name}("
        self.assertEqual(source.count(signature), 1)
        function = source.split(signature, 1)[1].split("\n#[test]", 1)[0]
        self.assertIn("let output = ", function)
        after_command = function.split("let output = ", 1)[1].split("?;", 1)[1]
        self.assertEqual(after_command.count(end_marker), 1)
        assertions = after_command.split(end_marker, 1)[0]
        self.assertEqual(assertions.count("assert_startup_output("), 1)
        self.assertIn("output.status.", assertions)

        target = Path(os.environ["CARGO_TARGET_DIR"]) / "startup-stderr-order"
        target.mkdir(parents=True, exist_ok=True)
        probe = target / f"{name}.rs"
        binary = target / (name + (".exe" if os.name == "nt" else ""))
        probe.write_text(
            f"#[path = {json.dumps(str(HELPER))}] mod startup_stderr;\n"
            "use startup_stderr::assert_startup_output;\n"
            "use std::process::{ExitStatus, Output};\n"
            "fn main() {\n"
            "    #[cfg(unix)] use std::os::unix::process::ExitStatusExt;\n"
            "    #[cfg(windows)] use std::os::windows::process::ExitStatusExt;\n"
            f"    #[cfg(unix)] let status = ExitStatus::from_raw({unexpected_code} << 8);\n"
            f"    #[cfg(windows)] let status = ExitStatus::from_raw({unexpected_code}u32);\n"
            "    let output = Output { status, stdout: Vec::new(), stderr: vec![255] };\n"
            + assertions
            + "\n}\n"
        )
        build = subprocess.run(
            [os.environ.get("RUSTC", "rustc"), "--edition=2021", str(probe), "-o", str(binary)],
            capture_output=True,
            timeout=60,
        )
        self.assertEqual(build.returncode, 0, build.stderr.decode(errors="replace"))
        result = subprocess.run([str(binary)], capture_output=True, timeout=10)
        (target / f"{name}.stderr").write_bytes(result.stderr)
        self.assertNotEqual(result.returncode, 0, "unexpected status must still fail")
        raw = b"startup stderr raw=[255]\n"
        self.assertEqual(result.stderr.count(raw), 1, result.stderr)
        self.assertLess(result.stderr.index(raw), result.stderr.index(b"panicked at"))
        # The exit check, rather than the later stderr check, must reject Output.
        expected_code = 1 - unexpected_code
        self.assertIn(f"left: Some({unexpected_code})".encode(), result.stderr)
        self.assertIn(f"right: Some({expected_code})".encode(), result.stderr)

    def test_new_daemon_direct_and_queued_attach(self):
        self.check_caller(
            "attach_session_cleans_up_only_daemon_started_by_command_windows",
            "wait_for_daemon_process_absent(&socket_path)?;",
            unexpected_code=0,
        )

    def test_preexisting_daemon_direct_and_queued_attach(self):
        self.check_caller(
            "attach_session_preserves_preexisting_empty_windows_daemon_and_state",
            "        assert!(\n            daemon.try_wait()?",
            unexpected_code=0,
        )

    def test_captured_start_server(self):
        self.check_caller(
            "start_server_with_captured_output_returns_after_empty_windows_daemon_exits",
            "    let daemon_pid = ",
            unexpected_code=1,
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
