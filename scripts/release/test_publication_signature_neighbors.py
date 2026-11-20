"""Inert signature-boundary checks; these fixtures do not prove cryptography."""

import argparse
from contextlib import ExitStack
from dataclasses import replace
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from release_tag_policy import AllowedSigner, PolicyError, SignerPolicy, parse_message
from test_publication_v1_contract import INVALID, ROOT, VALID, identity, load

SIGNATURE = b"-----BEGIN SSH SIGNATURE-----\ninert\n-----END SSH SIGNATURE-----\n"
SIGNER = AllowedSigner("inert@rmux.invalid", "unused", "SHA256:" + "A" * 43)


def tag_bytes(value):
    return (f"object {value.source_git_sha}\ntype commit\ntag {value.release_ref}\n"
            "tagger Inert <inert@rmux.invalid> 1 +0000\n\n" + value.message()).encode() + SIGNATURE


class PublicationSignatureNeighborsTest(unittest.TestCase):
    def setUp(self):
        self.verifier = load("verify-release-tag")
        self.proof = load("signed-tag-proof")
        self.evidence = load("release_evidence")
        self.policy = SignerPolicy(Path("unused"), True, "", (SIGNER,))
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(patch("subprocess.run", side_effect=AssertionError("external execution")))
        self.stack.enter_context(patch.object(self.verifier, "load_signer_policy", return_value=self.policy))
        self.stack.enter_context(patch.object(self.proof, "load_signer_policy", return_value=self.policy))
        self.crypto = self.stack.enter_context(patch.object(
            self.verifier, "verify_ssh_signature", return_value=SIGNER.principal))
        audit = ROOT / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory(dir=audit)))

    def github_arguments(self, value, raw):
        sha = self.verifier.object_sha(raw)
        ref = {"ref": value.full_ref, "object": {"type": "tag", "sha": sha}}
        tag = {"sha": sha, "tag": value.release_ref,
               "object": {"type": "commit", "sha": value.source_git_sha},
               "message": value.message() + SIGNATURE.decode(),
               "verification": {"verified": True,
                                "payload": raw[:-len(SIGNATURE)].decode(),
                                "signature": SIGNATURE.decode()}}
        ref_path, tag_path = self.root / "ref.json", self.root / "tag.json"
        ref_path.write_text(json.dumps(ref), encoding="utf-8")
        tag_path.write_text(json.dumps(tag), encoding="utf-8")
        return argparse.Namespace(ref_json=ref_path, tag_json=tag_path)

    def test_local_github_and_proof_keep_v1_identity_and_signature_inputs(self):
        for stable in VALID:
            for suffix in ("", "-rc.1", "-rc.99"):
                with self.subTest(ref=stable + suffix):
                    value = identity(stable + suffix)
                    raw = tag_bytes(value)
                    sha = self.verifier.object_sha(raw)
                    with patch.object(self.verifier, "run_git", side_effect=["tag\n", sha, raw, "commit\n"]):
                        local = self.verifier.verify_local(argparse.Namespace(repository=self.root), value)
                    self.assertEqual(local["release_ref"], value.release_ref)
                    verified = self.verifier.verify_github_json(self.github_arguments(value, raw), value)
                    self.crypto.assert_called_with(self.policy, raw[:-len(SIGNATURE)], SIGNATURE)
                    self.assertEqual(local | {"mode": "github-json-verified"}, verified)
                    verification = self.root / "verification.json"
                    verification.write_text(json.dumps(verified), encoding="utf-8")
                    proof = self.proof.expected(argparse.Namespace(
                        verification=verification, verified_at="2026-01-01T00:00:00Z"))
                    candidate = {"candidate_run_id": value.candidate_run_id,
                                 "manifest_artifact_id": value.candidate_manifest_artifact_id,
                                 "manifest_artifact_digest": value.candidate_manifest_artifact_digest,
                                 "manifest_sha256": value.candidate_manifest_sha256}
                    self.assertEqual(self.evidence.validate_signed_tag(
                        proof, release_ref=value.release_ref, source_git_sha=value.source_git_sha,
                        release_intent_id=value.release_intent_id, release_kind=value.release_kind,
                        candidate=candidate, release_policy_sha256=value.release_policy_root_sha256), proof)
                    self.assertEqual(proof["signature"]["signing_principal"], SIGNER.principal)

    def test_invalid_version_cannot_enter_canonical_message_parser(self):
        valid = identity("v1.1.0")
        for ref in INVALID:
            with self.subTest(ref=ref), self.assertRaises(PolicyError):
                message = valid.message().replace("RMUX release v1.1.0", f"RMUX release {ref}")
                if "-rc." in ref:
                    message = message.replace("Kind: stable", "Kind: rc")
                parse_message(message)

    def test_signed_tag_mutations_and_unverified_github_data_are_rejected(self):
        value = identity("v1.99.7-rc.99")
        raw = tag_bytes(value)
        mutations = {
            "wrong source": raw.replace(b"object " + b"a" * 40, b"object " + b"b" * 40),
            "wrong name": raw.replace(b"tag v1.99.7-rc.99", b"tag v1.99.8-rc.99"),
            "not commit": raw.replace(b"type commit", b"type tag"),
            "wrong intent": raw.replace(b"inert-release-contract", b"other-release-contract"),
            "wrong candidate": raw.replace(b"Run-ID: 1", b"Run-ID: 3"),
            "wrong manifest": raw.replace(b"SHA256: " + b"c" * 64, b"SHA256: " + b"e" * 64),
            "duplicate trailer": raw.replace(b"RMUX-Release-Kind: rc\n", b"RMUX-Release-Kind: rc\n" * 2),
            "reordered trailers": raw.replace(
                b"RMUX-Release-Intent-ID: inert-release-contract\nRMUX-Release-Kind: rc",
                b"RMUX-Release-Kind: rc\nRMUX-Release-Intent-ID: inert-release-contract"),
            "missing signature": raw[:-len(SIGNATURE)],
            "duplicate signature": raw + SIGNATURE,
            "CRLF": raw.replace(b"\n", b"\r\n"),
        }
        for label, changed in mutations.items():
            with self.subTest(mutation=label), self.assertRaises(PolicyError):
                self.verifier.verify_raw_tag(changed, self.verifier.object_sha(changed), value)
        with self.assertRaisesRegex(PolicyError, "hash mismatch"):
            self.verifier.verify_raw_tag(raw, "0" * 40, value)
        with patch.object(self.verifier, "run_git", return_value="commit\n"), \
             self.assertRaisesRegex(PolicyError, "annotated tag"):
            self.verifier.verify_local(argparse.Namespace(repository=self.root), value)
        args = self.github_arguments(value, raw)
        data = json.loads(args.tag_json.read_text(encoding="utf-8"))
        data["verification"]["verified"] = False
        args.tag_json.write_text(json.dumps(data), encoding="utf-8")
        with self.assertRaisesRegex(PolicyError, "did not verify"):
            self.verifier.verify_github_json(args, value)
        self.crypto.side_effect = PolicyError("inert signature rejection")
        with self.assertRaisesRegex(PolicyError, "signature rejection"):
            self.verifier.verify_raw_tag(raw, self.verifier.object_sha(raw), value)

    def test_proof_rejects_removed_signer_and_local_only_verification(self):
        value = identity("v1.1.0")
        raw = tag_bytes(value)
        verified = self.verifier.verify_github_json(self.github_arguments(value, raw), value)
        path = self.root / "verification.json"
        path.write_text(json.dumps(verified), encoding="utf-8")
        args = argparse.Namespace(verification=path, verified_at="2026-01-01T00:00:00Z")
        for policy in (replace(self.policy, enabled=False), replace(self.policy, allowed_signers=()),
                       replace(self.policy, allowed_signers=(replace(SIGNER, principal="other@rmux.invalid"),))):
            with self.subTest(policy=policy), \
                 patch.object(self.proof, "load_signer_policy", return_value=policy), \
                 self.assertRaises(ValueError):
                self.proof.expected(args)
        verified["mode"] = "verified"
        path.write_text(json.dumps(verified), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "GitHub-verified"):
            self.proof.expected(args)


if __name__ == "__main__":
    unittest.main()
