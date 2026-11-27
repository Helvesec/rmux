"""Historical evidence stays byte-exact when only its Git locator is published."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import provenance_archive


class ProvenanceArchiveTests(unittest.TestCase):
    def setUp(self):
        self.path = ROOT / "docs/validation/reference-applicability.json"
        # The published policy is the bare Git locator: no public projection of
        # the archived receipts is shipped, and the bytes stay exact through it.
        self.locator = json.loads(self.path.read_bytes())

    def decode(self, locator):
        return provenance_archive.decode(json.dumps(locator).encode(), ROOT)

    def test_original_receipts_and_authority_are_byte_exact(self):
        original = subprocess.check_output([
            "git", "-C", str(ROOT), "show",
            "c0272d4b5a9e59d8109e7d4b5ec09f5d4312e200:docs/validation/reference-applicability.json",
        ])
        self.assertEqual(self.decode(self.locator), original)
        self.assertEqual(provenance_archive.read(self.path, ROOT), original)

    def test_changed_digest_length_and_object_are_rejected(self):
        for key, value in (("sha256", "0" * 64), ("bytes", 0),
                           ("git_blob", "0" * 40), ("git_blob", "HEAD:file")):
            with self.subTest(field=key, value=value):
                changed = copy.deepcopy(self.locator)
                changed[key] = value
                with self.assertRaises(ValueError):
                    self.decode(changed)

    def test_unknown_locator_fields_are_rejected(self):
        self.locator["unexpected"] = True
        with self.assertRaises(ValueError):
            self.decode(self.locator)

    def test_public_receipt_changes_cannot_hide_behind_the_original_blob(self):
        original = json.loads(self.decode(self.locator))
        document = provenance_archive.public_projection(original)
        document["historical_source"] = copy.deepcopy(self.locator)
        self.assertEqual(json.loads(self.decode(document)), original)
        document["binding"]["review"]["job_id"] = "changed-authority"
        with self.assertRaisesRegex(ValueError, "projection differs"):
            self.decode(document)

    def test_regular_references_are_not_reinterpreted(self):
        raw = b'{"schema": 1, "message": "ordinary data"}\n'
        self.assertEqual(provenance_archive.decode(raw, ROOT), raw)


if __name__ == "__main__":
    unittest.main()
