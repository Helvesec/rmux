#!/usr/bin/env python3
"""An explicit Git epoch selects pins without relaxing the current reader."""
import json
import subprocess
import unittest

from test_accounting import ROOT, q


class PopulationEpochTests(unittest.TestCase):
    def test_explicit_epoch_and_scoped_import_preserve_current_default(self):
        projection = json.loads(q.MANIFEST.read_bytes())
        original = json.loads(q.population_bytes())
        historical = projection["source_commit"]
        current = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"]).decode().strip()
        expected_current = q.population()
        self.assertEqual(q.population(epoch=historical), original)
        self.assertEqual(q.population(epoch=current), expected_current)
        with q.population_epoch(historical):
            self.assertEqual(q.population(), original)
            with q.population_epoch(current):
                self.assertEqual(q.population(), expected_current)
            self.assertEqual(q.population(), original)
        self.assertEqual(q.population(), expected_current)
        with self.assertRaisesRegex(RuntimeError, "scope sentinel"):
            with q.population_epoch(historical):
                raise RuntimeError("scope sentinel")
        self.assertEqual(q.population(), expected_current)
        for invalid in ("HEAD", "", "f" * 40, historical.upper(), historical[:12], 1):
            with self.subTest(epoch=invalid), self.assertRaises(q.QualificationError):
                q.population(epoch=invalid)
        self.assertEqual(q.population(), expected_current)


if __name__ == "__main__":
    unittest.main()
