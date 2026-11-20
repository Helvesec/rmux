"""Local release checks; attestation subprocess results here are test doubles."""
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent / "release"))
import release_publish_security as security


class VerifyReleaseAcceptance(unittest.TestCase):
    def test_attestation_cannot_authorize_payload_manifest_disagreement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subject = root / "output.tar.gz"
            original = b"original release payload"
            subject.write_bytes(original)
            bundle = root / "bundle.json"
            bundle.write_text("{}")
            verifier = root / "gh"
            verifier.write_text("test double, never executed")
            predicate = {"assets": [{
                "name": subject.name, "role": "archive", "size": len(original),
                "sha256": hashlib.sha256(original).hexdigest(),
            }]}
            statement = {
                "subject": [{"name": subject.name, "digest": {
                    "sha256": predicate["assets"][0]["sha256"]}}],
                "predicateType": security.PREDICATE_TYPE, "predicate": predicate,
            }
            result = subprocess.CompletedProcess([], 0, json.dumps([{
                "verificationResult": {"statement": statement}}]).encode(), b"")
            subject.write_bytes(b"altered release payload!")
            with patch.object(security.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(ValueError, "asset bytes differ"):
                    security.verify_promotion_attestation(
                        gh=verifier, assets={subject.name: subject}, bundle=bundle,
                        predicate=predicate, source_git_sha="1" * 40,
                        release_ref="v0.11.0-rc.1",
                    )


if __name__ == "__main__":
    unittest.main()
