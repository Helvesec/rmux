"""Socket isolation and cleanup boundaries for the screen adapter."""
import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "screen_environment", Path(__file__).resolve().parents[1] / "scripts/bench/screen_environment.py"
)
environment = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(environment)


@unittest.skipUnless(os.name == "posix", "GNU screen socket namespace is Unix-only")
class ScreenEnvironment(unittest.TestCase):
    def test_linux_preserves_the_existing_environment(self):
        with patch.object(environment.sys, "platform", "linux"), \
                patch.dict(os.environ, {"SCREENDIR": "/existing/screen"}), \
                patch.object(Path, "mkdir") as mkdir:
            environment.prepare()
            self.assertEqual(os.environ["SCREENDIR"], "/existing/screen")
            mkdir.assert_not_called()

    def test_darwin_uses_one_private_short_directory_and_preserves_the_user_directory(self):
        with tempfile.TemporaryDirectory() as inherited, \
                patch.object(environment.sys, "platform", "darwin"), \
                patch.object(environment, "_directory", None), \
                patch.dict(os.environ, {"SCREENDIR": inherited}):
            sentinel = Path(inherited) / "user-session"
            sentinel.write_text("preserved")
            environment.prepare()
            directory = Path(os.environ["SCREENDIR"])
            try:
                self.assertNotEqual(directory, Path(inherited))
                self.assertEqual(len(os.fsencode(directory)), 9)
                self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o700)
                environment.prepare()
                self.assertEqual(os.environ["SCREENDIR"], str(directory))
                self.assertEqual(sentinel.read_text(), "preserved")
            finally:
                environment._remove_empty(directory, os.getpid())
            self.assertFalse(directory.exists())

    def test_cleanup_preserves_nonempty_directories_and_other_process_ownership(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root) / "sockets"
            directory.mkdir()
            socket = directory / "session"
            socket.write_text("live")
            environment._remove_empty(directory, os.getpid())
            self.assertEqual(socket.read_text(), "live")
            socket.unlink()
            environment._remove_empty(directory, os.getpid() + 1)
            self.assertTrue(directory.exists())
            environment._remove_empty(directory, os.getpid())
            self.assertFalse(directory.exists())


if __name__ == "__main__":
    unittest.main()
