#!/usr/bin/env python3
"""Historical benchmark accounting through the real importer and collector."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from test_accounting import Fixture, ROOT, q


HISTORICAL_DIGEST = "d2608970b6830d4d8e0688a3828916d380850a5bd41addede3104df4929041c5"


class PopulationArchiveTests(unittest.TestCase):
    def test_original_receipt_and_producer_require_verified_archive(self):
        projection = json.loads(q.MANIFEST.read_bytes())
        original = subprocess.check_output([
            "git", "-C", str(ROOT), "cat-file", "blob",
            projection["historical_source"]["git_blob"],
        ])
        historical_epoch = projection["source_commit"]
        producer_epoch = subprocess.check_output([
            "git", "-C", str(ROOT), "rev-parse", "HEAD",
        ]).decode().strip()
        expected_current = json.loads(original)
        proof = json.loads(q.pin_revisions.proof_bytes(ROOT))
        for revision in proof["revisions"]:
            active = subprocess.run(["git", "-C", str(ROOT), "merge-base", "--is-ancestor",
                                     revision["pin_commit"], producer_epoch], capture_output=True)
            self.assertIn(active.returncode, (0, 1))
            if active.returncode == 0:
                expected_current["preserved_definitions"][revision["function"]]["ast_sha256"] = revision["new_ast_sha256"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = root / "population.json"
            manifest.write_bytes(original)
            with patch.object(q, "MANIFEST", manifest):
                prior = Fixture(root / "prior")
                with q.population_epoch(producer_epoch):
                    prior.collect(iterations=1, operations={"new_session_warm_sh"},
                                  tools={"rmux", "tmux"})
                receipt = prior.artifact()  # Synthetic accounting, never native performance.
                self.assertEqual(receipt["population_sha256"], HISTORICAL_DIGEST)
                self.assertEqual(q.validate_artifact(receipt), [])

                cases = [("verified", projection, None)]
                for name, field, value, error in (
                    ("absent", "git_blob", "0" * 40, "historical Git size differs or is unavailable"),
                    ("corrupt", "sha256", "0" * 64, "historical Git bytes differ or are unavailable"),
                    ("projection", None, None, "public historical projection differs"),
                ):
                    changed = copy.deepcopy(projection)
                    if field:
                        changed["historical_source"][field] = value
                    else:
                        changed["recipe_limits"][0] += " changed"
                    cases.append((name, changed, error))

                for name, document, error in cases:
                    with self.subTest(case=name):
                        manifest.write_text(json.dumps(document))
                        produced = Fixture(root / name)
                        if error is None:
                            self.assertEqual(q.population(epoch=historical_epoch), json.loads(original))
                            self.assertEqual(q.population(), expected_current)
                            self.assertEqual(q.validate_artifact(receipt), [])
                            produced.collect(iterations=1, operations={"new_session_warm_sh"},
                                             tools={"rmux", "tmux"})
                            self.assertEqual(produced.artifact()["population_sha256"], HISTORICAL_DIGEST)
                            self.assertEqual(q.validate_artifact(produced.artifact()), [])
                        else:
                            with self.assertRaisesRegex(q.QualificationError, error):
                                q.population()
                            self.assertTrue(any(error in item for item in q.validate_artifact(receipt)))
                            with self.assertRaisesRegex(q.QualificationError, error):
                                produced.collect(iterations=1, operations={"new_session_warm_sh"},
                                                 tools={"rmux", "tmux"})
                            self.assertFalse(produced.out.exists())


if __name__ == "__main__":
    unittest.main()
