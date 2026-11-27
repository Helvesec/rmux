"""L09.3: the global manifest is a checked projection, never another registry."""
import json
from pathlib import Path
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
FAMILIES = {"commands", "options", "formats", "wire", "capabilities",
            "packages", "terminals", "personalities"}


class GlobalSurfaceAcceptance(unittest.TestCase):
    def test_all_eight_registries_project_to_a_deterministic_classified_manifest(self):
        argv = [sys.executable, "-B", "scripts/surface_manifest.py", "--export"]
        first = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(first.returncode, 0, first.stderr)
        second = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(first.stdout, second.stdout)
        manifest = json.loads(first.stdout)
        self.assertEqual(manifest["platform"], "linux")
        entries = manifest["entries"]
        self.assertEqual({entry["family"] for entry in entries}, FAMILIES)
        ids = [(entry["family"], entry["id"]) for entry in entries]
        self.assertEqual(ids, sorted(set(ids)))
        for entry in entries:
            self.assertIn(entry["classification"], {"stable", "post-1.0", "absent"})
            self.assertTrue(entry["sources"])
            for source in entry["sources"]:
                self.assertTrue((ROOT / source).is_file(), source)
            if entry["classification"] == "stable":
                self.assertTrue(entry["conformance"], entry)
        checked = subprocess.run([*argv[:-1], "--check"], cwd=ROOT,
                                 capture_output=True, text=True)
        self.assertEqual(checked.returncode, 0, checked.stderr)


if __name__ == "__main__":
    unittest.main()
