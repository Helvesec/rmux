"""Neighbor checks for the document gate and campaign renderer (synthetic data)."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import release_docs_allowlist as docs

ROOT = Path(__file__).resolve().parents[1]


class DocumentNeighbors(unittest.TestCase):
    def test_negation_does_not_hide_a_separate_positive_claim(self):
        for text in ("Windows 10 is not qualified. Windows 10 is fully qualified.",
                     "Windows 10 is fully supported, but speed is not qualified.",
                     "RMUX is <strong>10x faster</strong> than tmux."):
            with self.subTest(text=text):
                self.assertTrue(docs.claim_errors(text, "README.md"))
        self.assertEqual(docs.claim_errors("Windows 10 is not qualified.", "README.md"), [])

    def test_gate_accepts_release_prose_and_rejects_unlisted_docs_forms_and_future_urls(self):
        with tempfile.TemporaryDirectory(prefix="rmux-docs-neighbors-") as directory:
            repo = Path(directory)
            subprocess.run(["git", "clone", "--shared", "--quiet", "--no-checkout",
                            str(ROOT), str(repo)], check=True)
            paths = ["README.md", "docs/README.md", "docs/benchmarks.md",
                     "scripts/check-worktree-hygiene.sh", "scripts/release_docs_allowlist.py",
                     "scripts/source-inventory/declarations.json", docs.POLICY]
            policy = json.loads((ROOT / docs.POLICY).read_text())
            for path in paths + policy["issue_forms"]:
                target = repo / path
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / path, target)
            subprocess.run(["git", "-C", str(repo), "add", "."], check=True)

            def gate():
                return subprocess.run([sys.executable, "scripts/release_docs_allowlist.py"],
                                      cwd=repo, capture_output=True, text=True)

            result = gate()
            self.assertEqual(result.returncode, 0, result.stderr)
            for path in ("docs/unlisted.md", ".github/ISSUE_TEMPLATE/unlisted.yml",
                         "docs/benchmarks/unlisted.csv"):
                target = repo / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("unlisted\n")
                subprocess.run(["git", "-C", str(repo), "add", path], check=True)
                result = gate()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(path, result.stderr)
                subprocess.run(["git", "-C", str(repo), "rm", "-q", "--cached", path], check=True)
                target.unlink()
            policy["external_guides"]["unix_installation"]["url"] = "https://rmux.io/docs/future/"
            (repo / docs.POLICY).write_text(json.dumps(policy))
            self.assertIn("pending URL must be null", gate().stderr)


class GuideSourceAssertions(unittest.TestCase):
    """A checkout digest assertion and a historical Git ref bind different bytes."""

    def setUp(self):
        audit = ROOT / ".rmux-audit" / "docs-digest-tests"
        audit.mkdir(parents=True, exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(directory.cleanup)
        self.repo = Path(directory.name)
        subprocess.run(["git", "clone", "--shared", "--quiet", "--no-checkout",
                        str(ROOT), str(self.repo)], check=True)
        self.policy = json.loads((ROOT / docs.POLICY).read_text(encoding="utf-8"))
        paths = ["README.md", "docs/README.md", "docs/benchmarks.md", docs.POLICY,
                 "scripts/release_docs_allowlist.py", "scripts/source-inventory/declarations.json",
                 *self.policy["issue_forms"],
                 *(guide["source_path"] for guide in self.policy["external_guides"].values())]
        for path in paths:
            target = self.repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / path, target)
        self.git("add", ".")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args])

    def assert_gate(self, expected, diagnostic=None):
        (self.repo / docs.POLICY).write_text(json.dumps(self.policy), encoding="utf-8")
        result = subprocess.run([sys.executable, "scripts/release_docs_allowlist.py"],
                                cwd=self.repo, capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        if diagnostic:
            self.assertIn(diagnostic, result.stderr)

    def assert_checkout_digests(self):
        for guide in self.policy["external_guides"].values():
            guide["source_sha256"] = hashlib.sha256(
                (self.repo / guide["source_path"]).read_bytes()).hexdigest()

    def test_explicit_digest_is_checked_for_every_guide(self):
        self.assert_gate(0)
        self.assert_checkout_digests()
        self.assert_gate(0)
        for name, guide in self.policy["external_guides"].items():
            original = guide["source_sha256"]
            for digest, diagnostic in (("0" * 64, "source snapshot differs"),
                                       ("latest", "needs an exact source digest"),
                                       (None, "needs an exact source digest"),
                                       (True, "needs an exact source digest")):
                with self.subTest(guide=name, digest=digest):
                    guide["source_sha256"] = digest
                    try:
                        self.assert_gate(1, f"{name} {diagnostic}")
                    finally:
                        guide["source_sha256"] = original
        self.assert_gate(0)

    def test_digest_rejects_changed_missing_untracked_and_symlink_sources(self):
        self.assert_checkout_digests()
        self.assert_gate(0)
        for name, guide in self.policy["external_guides"].items():
            path = guide["source_path"]
            target = self.repo / path
            original = target.read_bytes()
            for mutation in ("changed", "missing", "untracked", "symlink", "directory"):
                with self.subTest(guide=name, mutation=mutation):
                    try:
                        if mutation == "changed":
                            target.write_bytes(original + b"\n# stale source\n")
                        elif mutation == "untracked":
                            self.git("rm", "--quiet", "--cached", path)
                        else:
                            target.unlink()
                            if mutation == "symlink":
                                copy = self.repo / "snapshot-copy"
                                copy.write_bytes(original)
                                target.symlink_to(copy)
                            elif mutation == "directory":
                                target.mkdir()
                        self.assert_gate(1, f"{name} source snapshot differs")
                    finally:
                        if target.is_symlink():
                            target.unlink()
                        elif target.is_dir():
                            target.rmdir()
                        target.write_bytes(original)
                        self.git("add", path)
        self.assert_gate(0)

    def test_ref_only_contract_does_not_assert_current_checkout_bytes(self):
        self.assert_gate(0)
        for guide in self.policy["external_guides"].values():
            target = self.repo / guide["source_path"]
            target.write_bytes(target.read_bytes() + b"\n# current checkout only\n")
        self.assert_gate(0)
        for guide in self.policy["external_guides"].values():
            (self.repo / guide["source_path"]).unlink()
        self.assert_gate(0)

    def test_digest_does_not_replace_ref_or_publication_url_checks(self):
        self.assert_checkout_digests()
        self.assert_gate(0)
        for name, guide in self.policy["external_guides"].items():
            original = dict(guide)
            for field, value in (("source_ref", "main"), ("source_ref", "f" * 40),
                                 ("source_ref", self.git("rev-parse", "HEAD:README.md").decode().strip()),
                                 ("url", "https://rmux.io/docs/future/")):
                with self.subTest(guide=name, field=field, value=value):
                    guide[field] = value
                    try:
                        self.assert_gate(1)
                    finally:
                        guide.update(original)
            guide.update(status="published", url="https://rmux.io/docs/" + guide["source_ref"] + "/" + name)
        self.assert_gate(0)
        for name, guide in self.policy["external_guides"].items():
            original = guide["source_sha256"]
            with self.subTest(published_guide=name):
                guide["source_sha256"] = "0" * 64
                try:
                    self.assert_gate(1, f"{name} source snapshot differs")
                finally:
                    guide["source_sha256"] = original
        self.assert_gate(0)


class PublicationPhase(unittest.TestCase):
    """Synthetic receipts in real local Git objects; never publication evidence."""

    def setUp(self):
        audit = ROOT / ".rmux-audit" / "docs-publication-tests"
        audit.mkdir(parents=True, exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(directory.cleanup)
        self.repo = Path(directory.name)
        subprocess.run(["git", "clone", "--shared", "--quiet", "--no-checkout",
                        str(ROOT), str(self.repo)], check=True)
        self.policy = json.loads((ROOT / docs.POLICY).read_text())
        paths = ["README.md", "docs/README.md", "docs/benchmarks.md", docs.POLICY,
                 "scripts/release_docs_allowlist.py", "scripts/source-inventory/declarations.json",
                 ".github/release/schemas/publication-receipt-predicate.schema.json"]
        for path in paths + self.policy["issue_forms"]:
            target = self.repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / path, target)
        self.git("add", ".")
        self.source = self.git("rev-parse", "HEAD")
        self.url = "https://github.com/Helvesec/rmux/releases/tag/v0.11.0"
        self.pending = dict(status="pending", url=None, source_ref=None,
                            receipt_ref=None, receipt_path=None)
        self.policy["release"] = dict(self.pending)
        self.tag = self.git("mktag", content=(
            f"object {self.source}\ntype commit\ntag v0.11.0\n"
            "tagger Synthetic <fixture@example.invalid> 1700000000 +0000\n\n"
            "Synthetic publication phase fixture, not a signed release.\n"))
        self.receipt = {
            "schema_version": 1,
            "predicate_type": "https://rmux.io/attestations/release-publication-receipt/v1",
            "status": "downstream-authorized", "downstream_authority": True,
            "repository_id": 1239918790, "source_git_sha": self.source,
            "release": {"id": 1, "ref": "v0.11.0", "kind": "stable",
                        "intent_id": "synthetic-fixture", "tag_object_sha": self.tag,
                        "immutable": True, "created_at": "2023-11-14T22:13:20Z",
                        "published_at": "2023-11-14T22:14:20Z"},
            "candidate": {
                "schema_version": 1, "status": "shadow-non-authoritative",
                "repository_id": 1239918790, "source_git_sha": self.source,
                "candidate_run_id": 1, "candidate_run_attempt": 1,
                "manifest_run_id": 2, "manifest_run_attempt": 1,
                "manifest_workflow_id": 316223904,
                "manifest_workflow_path": ".github/workflows/release-shadow.yml",
                "manifest_artifact_id": 1, "manifest_artifact_digest": "sha256:" + "c" * 64,
                "manifest_sha256": "c" * 64,
                "manifest_created_at": "2023-11-14T21:00:00Z",
                "manifest_expires_at": "2023-11-15T21:00:00Z"},
            "policy_audit": {
                "schema_version": 1, "status": "shadow-non-authoritative",
                "repository_id": 1239918790, "source_git_sha": self.source,
                "candidate_run_id": 1, "release_intent_id": "synthetic-fixture",
                "policy_audit_run_id": 3, "policy_audit_run_attempt": 1,
                "predicate_artifact_id": 2, "predicate_artifact_digest": "sha256:" + "d" * 64,
                "predicate_sha256": "d" * 64, "emitted_at": "2023-11-14T22:00:00Z",
                "expires_at": "2023-11-15T22:00:00Z",
                "app_id": 4344532, "installation_id": 147749910,
                "workflow_id": 316435346, "workflow_path": ".github/workflows/release-promote.yml",
                "release_policy_sha256": "e" * 64, "reference_sha256": "f" * 64},
            "authorization": {
                "identity": {"run_id": 3, "run_attempt": 1, "workflow_id": 316435346,
                             "workflow_path": ".github/workflows/release-promote.yml"},
                "predicate_sha256": "a" * 64, "envelope_sha256": "b" * 64,
                "attestation": {"attestation_id": "synthetic-fixture",
                                "bundle_file": "SHA256SUMS.sigstore.json",
                                "bundle_sha256": "c" * 64},
                "bundle_artifact": {"artifact_id": 3,
                                    "name": "rmux-promotion-authorization-" + self.source,
                                    "archive_digest": "sha256:" + "d" * 64,
                                    "size_in_bytes": 1}},
            "receipt": {"run_id": 1, "run_attempt": 1, "workflow_id": 316435347,
                        "workflow_path": ".github/workflows/release-receipt.yml"},
            "verified_at": "2023-11-14T22:15:20Z",
            "asset_count": 2, "sha256sums_sha256": "a" * 64,
            "assets": [{"id": 1, "name": "rmux-0.11.0-synthetic.tar.gz",
                        "role": "archive", "size": 1, "digest": "sha256:" + "b" * 64},
                       {"id": 2, "name": "SHA256SUMS", "role": "checksums",
                        "size": 1, "digest": "sha256:" + "a" * 64}],
        }

    def git(self, *args, content=None):
        return subprocess.check_output(
            ["git", "-C", str(self.repo), "-c", "user.name=Synthetic",
             "-c", "user.email=fixture@example.invalid", *args],
            input=content, text=True).strip()

    def publish(self):
        path = "publication-receipt-predicate.json"
        (self.repo / path).write_text(json.dumps(self.receipt))
        self.git("add", path)
        tree = self.git("write-tree")
        ref = self.git("commit-tree", tree, "-p", self.source,
                       content="Synthetic receipt fixture; no publication.\n")
        self.policy["release"] = dict(status="published", url=self.url,
                                      source_ref=self.source, receipt_ref=ref, receipt_path=path)

    def gate(self):
        (self.repo / docs.POLICY).write_text(json.dumps(self.policy))
        return subprocess.run([sys.executable, "scripts/release_docs_allowlist.py"],
                              cwd=self.repo, capture_output=True, text=True)

    def assert_gate(self, expected):
        result = self.gate()
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        if expected:
            self.assertNotIn("Traceback", result.stderr)

    def add_link(self, path):
        target = self.repo / path
        target.write_text(target.read_text() + f"\n[Release]({self.url})\n")

    def test_pending_and_published_at_both_indexes(self):
        self.assert_gate(0)
        for path in ("README.md", "docs/README.md"):
            with self.subTest(path=path):
                original = (self.repo / path).read_text()
                self.policy["release"] = dict(self.pending)
                self.add_link(path)
                self.assert_gate(1)
                self.publish()
                self.assert_gate(0)
                (self.repo / path).write_text(original)

    def test_pending_and_phase_must_be_explicit(self):
        for field, value in (("status", "unknown"), ("url", self.url),
                             ("source_ref", self.source), ("receipt_ref", self.source),
                             ("receipt_path", "receipt.json")):
            with self.subTest(field=field):
                self.policy["release"] = dict(self.pending, **{field: value})
                self.assert_gate(1)
        del self.policy["release"]
        self.assert_gate(1)

    def test_published_rejects_each_missing_receipt_field_at_both_indexes(self):
        original = json.dumps(self.receipt)
        schema = json.loads((ROOT / ".github/release/schemas/"
                             "publication-receipt-predicate.schema.json").read_text())
        self.assertEqual(set(self.receipt), set(schema["required"]))
        for path in ("README.md", "docs/README.md"):
            original_index = (self.repo / path).read_text()
            self.add_link(path)
            for field in schema["required"]:
                with self.subTest(path=path, missing=field):
                    self.receipt = json.loads(original)
                    del self.receipt[field]
                    self.publish()
                    self.assert_gate(1)
            (self.repo / path).write_text(original_index)
        self.receipt = json.loads(original)
        self.publish()
        self.assert_gate(0)

    def test_published_rejects_unknown_receipt_fields_at_both_indexes(self):
        original = json.dumps(self.receipt)
        for path in ("README.md", "docs/README.md"):
            original_index = (self.repo / path).read_text()
            self.add_link(path)
            for field in ("unknown", "attestation_id", "receipt_bundle_artifact_id"):
                with self.subTest(path=path, extra=field):
                    self.receipt = json.loads(original)
                    self.receipt[field] = "synthetic-extra"
                    self.publish()
                    self.assert_gate(1)
            (self.repo / path).write_text(original_index)
        self.receipt = json.loads(original)
        self.publish()
        self.assert_gate(0)

    def test_published_requires_real_immutable_bindings(self):
        self.publish()
        valid = dict(self.policy["release"])
        blob = self.git("rev-parse", f"{self.source}:README.md")
        for field, value in (("url", None), ("url", self.url + "-rc.1"),
                             ("url", self.url.replace("Helvesec", "someone")),
                             ("receipt_path", None), ("receipt_path", "absent.json"),
                             ("receipt_path", "docs"), ("receipt_path", "README.md"),
                             ("source_ref", "HEAD"), ("source_ref", "f" * 40),
                             ("source_ref", blob), ("receipt_ref", None),
                             ("receipt_ref", "HEAD"), ("receipt_ref", "f" * 40),
                             ("receipt_ref", blob)):
            with self.subTest(field=field, value=value):
                self.policy["release"] = dict(valid, **{field: value})
                self.assert_gate(1)
        self.policy["release"] = valid
        self.assert_gate(0)

    def test_receipt_identity_and_publication_must_agree(self):
        original = json.dumps(self.receipt)
        cases = [((), "status", "disarmed-non-authoritative"),
                 ((), "downstream_authority", False), ((), "repository_id", 1),
                 ((), "predicate_type", "unrelated"), ((), "source_git_sha", "f" * 40),
                 (("candidate",), "source_git_sha", "f" * 40),
                 (("release",), "immutable", False), (("release",), "ref", "v0.11.0-rc.1"),
                 (("release",), "kind", "rc"), (("release",), "tag_object_sha", self.source),
                 (("release",), "tag_object_sha", "f" * 40),
                 (("receipt",), "workflow_path", ".github/workflows/release-promotion-simulation.yml"),
                 ((), "assets", []), ((), "asset_count", 0)]
        for parents, field, value in cases:
            with self.subTest(parents=parents, field=field):
                self.receipt = json.loads(original)
                node = self.receipt
                for parent in parents:
                    node = node[parent]
                node[field] = value
                self.publish()
                self.assert_gate(1)
        self.receipt = json.loads(original)
        self.publish()
        self.assert_gate(0)

    def test_published_preserves_disclosures_and_claim_checks(self):
        self.publish()
        path = self.repo / "README.md"
        original = path.read_text()
        for disclosure in ("Windows 10 is not qualified", "Speed is not qualified",
                           "Kitty Keyboard is disabled by default", "Web Share is experimental",
                           "Floating panes are experimental", "submission pending"):
            with self.subTest(disclosure=disclosure):
                path.write_text(original.replace(disclosure, "removed"))
                self.assert_gate(1)
        for claim in ("Windows 10 is fully qualified.", "RMUX is faster than tmux."):
            with self.subTest(claim=claim):
                path.write_text(original + "\n" + claim)
                self.assert_gate(1)
        path.write_text(original)
        self.assert_gate(0)

    def test_every_read_decodes_utf8_under_a_non_utf8_locale(self):
        """W12: a cp1252/ASCII locale must not decide how release sources are decoded."""
        canary = "W12 canary é’”"
        for path in ("README.md", "docs/README.md", "docs/benchmarks.md"):
            target = self.repo / path
            target.write_bytes(target.read_bytes() + f"\n{canary}\n".encode("utf-8"))
        for path in ["scripts/source-inventory/declarations.json",
                     ".github/release/schemas/publication-receipt-predicate.schema.json",
                     *self.policy["issue_forms"]]:
            target = self.repo / path
            document = json.loads(target.read_bytes().decode("utf-8"))
            document["w12_encoding_canary"] = canary
            target.write_bytes(json.dumps(document, ensure_ascii=False).encode("utf-8"))
        self.git("add", ".")
        env = dict(os.environ, PYTHONUTF8="0", PYTHONCOERCECLOCALE="0",
                   LC_ALL="C", LANG="C", PYTHONIOENCODING="utf-8")

        def legacy_gate():
            policy = dict(self.policy, w12_encoding_canary=canary)
            (self.repo / docs.POLICY).write_bytes(
                json.dumps(policy, ensure_ascii=False).encode("utf-8"))
            return subprocess.run([sys.executable, "scripts/release_docs_allowlist.py"],
                                  cwd=self.repo, env=env, capture_output=True,
                                  encoding="utf-8")

        probe = subprocess.run([sys.executable, "-c",
                                "import locale; print(locale.getpreferredencoding(False))"],
                               env=env, capture_output=True, text=True, check=True)
        self.assertNotIn(probe.stdout.strip().lower().replace("-", ""), ("utf8", "cp65001"))
        self.publish()
        result = legacy_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("release-docs-allowlist=ok", result.stdout)
        self.policy["release"] = dict(self.policy["release"], status="unknown")
        result = legacy_gate()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertIn("release phase or versioned URL is invalid", result.stderr)


class DeliveredDocumentAllowlist(unittest.TestCase):
    """The committed tree, not a fixture subset, must satisfy the document gate."""
    DELIVERED = "docs/program-status.md"

    def gate(self, repo):
        return subprocess.run([sys.executable, "scripts/release_docs_allowlist.py"],
                              cwd=repo, capture_output=True, text=True)

    def test_committed_tree_documents_are_allowlisted_and_unknown_paths_refused(self):
        # Entry used by gate-unix-fast.sh, release-review-gate.sh, CI and the recipe.
        result = self.gate(ROOT)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(docs.check(), [])
        declarations = json.loads((ROOT / "scripts/source-inventory/declarations.json").read_text())
        self.assertIn(self.DELIVERED, declarations["files"]["documentation"])
        with tempfile.TemporaryDirectory(prefix="rmux-docs-delivered-") as directory:
            repo = Path(directory)
            subprocess.run(["git", "clone", "--shared", "--quiet", "--no-checkout",
                            str(ROOT), str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "read-tree", "HEAD"], check=True)
            policy = json.loads((ROOT / docs.POLICY).read_text())
            for path in ["README.md", "docs/README.md", "docs/benchmarks.md",
                         "scripts/release_docs_allowlist.py", docs.POLICY,
                         "scripts/source-inventory/declarations.json", *policy["issue_forms"]]:
                target = repo / path
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / path, target)
            result = self.gate(repo)
            self.assertEqual(result.returncode, 0, result.stderr)
            refused = (("docs/program-status-unlisted.md", "document absent from"),
                       ("docs/program-status-unlisted.json", "document absent from"),
                       ("docs/benchmarks/program-status.md", "benchmark results belong"),
                       (".github/ISSUE_TEMPLATE/program-status.yml", "issue form absent"))
            for path, diagnostic in refused:
                with self.subTest(path=path):
                    changed = json.loads(json.dumps(declarations))
                    changed["files"]["documentation"].append(path)
                    (repo / "scripts/source-inventory/declarations.json").write_text(
                        json.dumps(changed) if path.startswith("docs/benchmarks/")
                        else json.dumps(declarations))
                    target = repo / path
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text("unlisted\n")
                    subprocess.run(["git", "-C", str(repo), "add", path], check=True)
                    result = self.gate(repo)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(f"{path}: {diagnostic}", result.stderr)
                    subprocess.run(["git", "-C", str(repo), "rm", "-q", "--cached", path], check=True)
                    target.unlink()
            withdrawn = json.loads(json.dumps(declarations))
            withdrawn["files"]["documentation"].remove(self.DELIVERED)
            (repo / "scripts/source-inventory/declarations.json").write_text(json.dumps(withdrawn))
            result = self.gate(repo)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f"{self.DELIVERED}: document absent from", result.stderr)

    def test_inventory_binds_the_delivered_document_blob(self):
        sys.path.insert(0, str(ROOT / "scripts/source-inventory"))
        try:
            from model import validate_declarations
        finally:
            sys.path.pop(0)
        declarations = json.loads((ROOT / "scripts/source-inventory/declarations.json").read_text())
        blob = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse",
                                        "HEAD:" + self.DELIVERED], text=True).strip()
        files = {self.DELIVERED: {"blob": blob}}
        self.assertEqual(validate_declarations(declarations, files)[self.DELIVERED], "documentation")
        stale = json.loads(json.dumps(declarations))
        stale["documents"][self.DELIVERED]["source_blobs"] = ["0" * 40]
        with self.assertRaisesRegex(ValueError, "stale document declaration: " + self.DELIVERED):
            validate_declarations(stale, files)
        missing = json.loads(json.dumps(declarations))
        del missing["documents"][self.DELIVERED]
        with self.assertRaisesRegex(ValueError, "undocumented role/owner/consumer: " + self.DELIVERED):
            validate_declarations(missing, files)


class CampaignNeighbors(unittest.TestCase):
    def test_campaign_omits_compatibility_columns_but_preserves_raw_evidence(self):
        # Deliberately synthetic fixture, not performance or platform qualification.
        with tempfile.TemporaryDirectory(prefix="rmux-campaign-test-") as directory:
            repo = Path(directory)
            payload = {"platform": {"id": "windows", "name": "Windows"},
                       "git": {"commit": "a" * 40}, "tools": ["rmux", "tmux", "screen"],
                       "tool_environments": {"rmux": "native", "tmux": "wsl", "screen": "wsl"},
                       "operations": [{"id": "list_sessions_default", "metrics": {
                           tool: {"p50_ms": 1, "samples_ms": [1]} for tool in ("rmux", "tmux")}}]}
            source = repo / "synthetic.json"
            source.write_text(json.dumps(payload))
            args = [sys.executable, str(ROOT / "scripts/bench/render.py"), str(source),
                    "--output", str(repo / "report.md"), "--asset-dir", str(repo / "assets")]
            subprocess.run(args + ["--native-only"], check=True)
            output = (repo / "report.md").read_text()
            self.assertNotIn("src=", output, "exported campaigns must not reference source-only images")
            header = next(line for line in output.splitlines() if "<th " in line)
            self.assertIn(">rmux</th>", header)
            self.assertNotIn("WSL", header)
            self.assertNotIn("vs ", header)
            self.assertNotIn(">tmux</th>", header)
            self.assertIn(",tmux,", (repo / "assets/windows.csv").read_text())
            # Diagnostic mode retains the existing honest compatibility labels.
            subprocess.run(args, check=True)
            self.assertIn(">tmux (WSL)</th>", (repo / "report.md").read_text())
            refused = subprocess.run(args + ["--output", str(ROOT / "docs/forbidden-benchmark.md")],
                                     capture_output=True, text=True)
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn("outside source content", refused.stderr)
            self.assertFalse((ROOT / "docs/forbidden-benchmark.md").exists())


if __name__ == "__main__":
    unittest.main()
