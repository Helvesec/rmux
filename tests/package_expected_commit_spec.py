"""An explicitly empty SHA cannot disable a package's requested provenance."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import unittest

import unix_installer_spec as fixtures


@unittest.skipUnless(os.name == "posix", "requires the Unix package verifier")
class ExpectedCommit(unittest.TestCase):
    def setUp(self):
        fixture = fixtures.InstallerCase(methodName="runTest")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        tree = fixture.archive("rmux-expected-commit")
        self.commit = "1234567890abcdef1234567890abcdef12345678"
        metadata = dict(artifact_kind="unix-package-binary", package_layout="rmux-package-v3",
                        git_commit=self.commit, configuration="release", release_artifact=True,
                        git_dirty=False, skip_build=False)
        for field, path in (("binary", "bin/rmux"), ("helper_binary", "libexec/rmux/rmux"),
                            ("daemon_binary", "bin/rmux-daemon")):
            metadata[field + "_sha256"] = hashlib.sha256((tree / path).read_bytes()).hexdigest()
        (tree / "share/rmux/artifact-metadata.json").write_text(json.dumps(metadata) + "\n")
        files = sorted(p for p in tree.rglob("*") if p.is_file() and p.name != "SHA256SUMS.txt")
        (tree / "SHA256SUMS.txt").write_text("".join(
            f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(tree)}\n" for p in files))
        self.archive = fixture.root / (tree.name + ".tar.gz")
        with tarfile.open(self.archive, "w:gz") as output:
            output.add(tree, arcname=tree.name)
        (fixture.root / "SHA256SUMS.txt").write_text(
            f"{hashlib.sha256(self.archive.read_bytes()).hexdigest()}  {self.archive.name}\n")

    def verify(self, *options):
        return subprocess.run([str(fixtures.ROOT / "scripts/verify-package.sh"),
                               str(self.archive), *options], capture_output=True, text=True)

    def test_explicit_empty_commit_is_refused(self):
        for release in ((), ("--require-release-artifact",)):
            for pins in (("",), (self.commit, ""), ("", self.commit)):
                with self.subTest(release=bool(release), pins=pins):
                    options = [arg for pin in pins for arg in ("--expected-commit", pin)]
                    result = self.verify(*release, *options)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("--expected-commit requires a non-empty value", result.stderr)

    def test_absent_correct_and_wrong_commits_keep_their_contract(self):
        for release in ((), ("--require-release-artifact",)):
            for pin, accepted in ((None, True), (self.commit, True), ("0" * 40, False)):
                with self.subTest(release=bool(release), pin=pin):
                    options = () if pin is None else ("--expected-commit", pin)
                    result = self.verify(*release, *options)
                    self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
                    if not accepted:
                        self.assertIn("source provenance", result.stderr)

    def test_missing_commit_value_is_refused(self):
        result = self.verify("--expected-commit")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("--expected-commit requires a value", result.stderr)
