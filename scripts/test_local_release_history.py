"""Frozen local acceptance for the next release's version and protocol history."""

import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class LocalReleaseHistoryTests(unittest.TestCase):
    def test_011_versions_and_protocol_history(self):
        manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        workspace = manifest.split("[workspace.package]", 1)[1].split("[", 1)[0]
        self.assertRegex(workspace, r'(?m)^version = "0\.11\.0"$')
        versions = subprocess.run(
            ["bash", "scripts/check-release-versions.sh"], cwd=ROOT,
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(versions.returncode, 0, versions.stdout + versions.stderr)
        changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
        sections = dict(re.findall(
            r"(?ms)^## ([^\n]+)\n(.*?)(?=^## |\Z)", changelog
        ))
        self.assertEqual(next(iter(sections)), "0.11.0")
        self.assertIn("wire version 9", sections["0.11.0"])
        self.assertNotIn("wire version 9", sections["0.10.0"])
        self.assertIn("wire\n  version 8", sections["0.10.0"])
        checked = subprocess.run(
            ["python3", "scripts/check-changelog-release.py"], cwd=ROOT,
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)


class LocalReleaseReceiptTests(unittest.TestCase):
    def test_failed_retry_retires_the_previous_pass_manifest(self):
        # Exercise the real wrapper with synthetic packaging stages: no RMUX
        # binary is built or executed, and the fixture has its own Git history.
        audit = ROOT / ".rmux-audit" / "local-release-receipt-tests"
        audit.mkdir(parents=True, exist_ok=True)
        for configuration in ("release", "debug"):
            for failure in ("package", "verify", "source"):
                with self.subTest(configuration=configuration, failure=failure):
                    repo = Path(tempfile.mkdtemp(dir=audit)).resolve()
                    self.addCleanup(shutil.rmtree, repo)
                    scripts = repo / "scripts"
                    scripts.mkdir()
                    (repo / ".gitignore").write_text("/target/\n")
                    shutil.copy2(ROOT / "scripts/release-local.sh", scripts)
                    stages = {
                        "package-unix.sh": '''#!/usr/bin/env bash
set -euo pipefail
[ "${RMUX_TEST_RELEASE_FAILURE:-}" != package ] || exit 23
output="$4"
mkdir -p "$output"
printf 'synthetic archive\\n' > "$output/rmux.tar.gz"
printf 'package=%s/rmux.tar.gz\\nsha256=%s\\n' "$output" "$RMUX_TEST_PACKAGE_SHA"
''',
                        "verify-package.sh": '''#!/usr/bin/env bash
set -euo pipefail
[ "${RMUX_TEST_RELEASE_FAILURE:-}" != verify ] || exit 24
if [ "${RMUX_TEST_RELEASE_FAILURE:-}" = source ]; then
  printf '# source changed\\n' >> .gitignore
fi
printf 'sha256=%s\\ngit_commit=%s\\n' "$RMUX_TEST_PACKAGE_SHA" "$(git rev-parse HEAD)"
''',
                    }
                    for name, content in stages.items():
                        path = scripts / name
                        path.write_text(content)
                        path.chmod(0o755)
                    def git(*args):
                        return subprocess.check_output(["git", "-C", str(repo), *args],
                                                       text=True).strip()
                    git("init", "-q")
                    git("add", ".")
                    git("-c", "user.name=Release Test", "-c", "user.email=release@example.invalid",
                        "commit", "-qm", "synthetic release stages")
                    head = git("rev-parse", "HEAD")
                    digest = hashlib.sha256(b"synthetic archive\n").hexdigest()
                    environment = dict(os.environ, RMUX_TEST_PACKAGE_SHA=digest,
                                       RMUX_TEST_RELEASE_FAILURE="")
                    command = ["bash", str(scripts / "release-local.sh"),
                               "--configuration", configuration]
                    accepted = subprocess.run(command, cwd=repo, env=environment,
                                              capture_output=True, text=True)
                    self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)
                    artifacts = repo / "target/release-artifacts"
                    manifest, = artifacts.glob("release_automation_*.txt")
                    summary, = artifacts.glob("*/summary.json")
                    receipt = json.loads(summary.read_text())
                    self.assertEqual(receipt["git_commit"], head)
                    self.assertEqual(receipt["package_sha256"], digest)
                    self.assertIn("\nPASS\n", manifest.read_text())
                    environment["RMUX_TEST_RELEASE_FAILURE"] = failure
                    rejected = subprocess.run(command, cwd=repo, env=environment,
                                              capture_output=True, text=True)
                    self.assertNotEqual(rejected.returncode, 0)
                    self.assertFalse(summary.exists(), "failed retry retained its old summary")
                    self.assertFalse(manifest.exists(),
                                     "failed retry retained a stale PASS manifest for the same HEAD")


if __name__ == "__main__":
    unittest.main()
