"""Acceptance of the documented Cargo entry point (run from the repository)."""
import pathlib
import subprocess
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


class RegistryDocsEntrypoint(unittest.TestCase):
    def test_cargo_xtask_exposes_generation_and_verification(self):
        result = subprocess.run(
            ["cargo", "xtask", "--help"], cwd=ROOT, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cargo xtask", result.stdout)
        self.assertIn("registry-docs --output-dir <path> [--check]", result.stdout)


if __name__ == "__main__":
    unittest.main()
