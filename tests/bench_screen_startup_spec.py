"""Native regression: detached screen must execute its measured payload."""
import importlib.util
from pathlib import Path
import shlex
import shutil
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("bench_unix", ROOT / "scripts/bench/bench_unix.py")
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class DetachedScreenStartup(unittest.TestCase):
    def test_detached_session_executes_payload_and_accepts_control(self):
        screen = shutil.which("screen")
        self.assertIsNotNone(screen, "native GNU screen is required")
        with tempfile.TemporaryDirectory(prefix="rmux-screen-startup-") as directory:
            for explicit_program in (True, False):
                with self.subTest(explicit_program=explicit_program):
                    marker = Path(directory) / str(explicit_program)
                    name = bench.screen_session_name("capture_pane_200x50_scrollback_10k")
                    script = f"printf started > {shlex.quote(str(marker))}"
                    program = ["/bin/sh", "-c", script + "; exec /bin/sleep 30"]
                    try:
                        bench.screen_create_session(
                            screen, name, program=program if explicit_program else None
                        )
                        if not explicit_program:
                            bench.quiet(bench.screen_control_command(
                                screen, name, ["-X", "stuff", script + "\n"]
                            ))
                        deadline = time.monotonic() + 2
                        while (not marker.exists() or marker.read_text() != "started") and time.monotonic() < deadline:
                            time.sleep(.01)
                        self.assertTrue(marker.exists(), "screen exited before executing the payload")
                        self.assertEqual(marker.read_text(), "started")
                        bench.captured(bench.screen_control_command(
                            screen, name, ["-Q", "windows"]
                        ))
                    finally:
                        bench.screen_cleanup(screen, name)


if __name__ == "__main__":
    unittest.main()
