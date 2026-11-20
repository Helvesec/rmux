"""INT331: published R2 provenance must pass the unchanged hygiene contract."""
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]


class PinProofPortabilityTests(unittest.TestCase):
    def test_versioned_proof_publication_passes_exact_worktree_hygiene(self):
        result = subprocess.run(["bash", "scripts/check-worktree-hygiene.sh"], cwd=ROOT,
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stdout.strip(), "worktree-hygiene=ok")


if __name__ == "__main__":
    unittest.main()
