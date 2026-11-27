"""A gates-and-soak recipe alone cannot qualify the final release artifacts."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from qualification_recipe import default_manifest


class FinalSmokesRequiredTests(unittest.TestCase):
    def test_q1_refuses_gates_and_soak_without_final_artifact_manifest(self):
        metadata = {"workspace_members": ["fixture"], "packages": [
            {"id": "fixture", "name": "fixture", "features": {}}]}
        # Metadata is the only external operation; no Cargo, gate or soak runs.
        with patch("qualification_recipe.subprocess.check_output",
                   return_value=json.dumps(metadata)):
            with self.assertRaisesRegex(ValueError, "final artifact"):
                default_manifest(Path(__file__).resolve().parents[1], "a" * 40, "Q1")


if __name__ == "__main__":
    unittest.main()
