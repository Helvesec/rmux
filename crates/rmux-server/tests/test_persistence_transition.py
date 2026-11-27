"""A version-transition label must describe the two effective images."""
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("persistence_acceptance.py")


class PersistenceTransition(unittest.TestCase):
    def test_upgrade_cannot_replay_the_same_image(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            image = root / "rmux"
            marker = root / "executed"
            image.write_text("#!/bin/sh\ntouch '" + str(marker) + "'\nexit 86\n")
            image.chmod(0o755)
            output = root / "fixture"
            result = subprocess.run(
                [sys.executable, "-S", "-B", str(SCRIPT), str(image),
                 str(root / "plugins"), str(output), "clean", "resurrect",
                 str(image), "upgrade"], text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("upgrade requires distinct images", result.stderr)
            self.assertFalse(output.exists(), "invalid stimulus created a native fixture")
            self.assertFalse(marker.exists(), "invalid stimulus executed an image")


if __name__ == "__main__":
    unittest.main()
