"""Exercise Unix packaging with a mutating signer; no native signature claim."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/packaging"))
import notices


@unittest.skipUnless(os.name == "posix", "Unix package entrypoint")
class PackageSigning(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.target = self.root / "target"
        self.binaries = self.target / "aarch64-apple-darwin/debug"
        self.binaries.mkdir(parents=True)
        for name in ("rmux", "rmux-full", "rmux-daemon"):
            path = self.binaries / name
            path.write_bytes(b"fixture payload, never executed\n")
            path.chmod(0o755)
        self.original = notices.payloads(self.binaries)
        notice = "RMUX third-party notices\nfixture attribution\n"
        notices.atomic(notices.cache_path(self.binaries), json.dumps(dict(
            schema=1, inputs=notices.policy_inputs(), payloads=self.original,
            notice=notice, notice_sha256=hashlib.sha256(notice.encode()).hexdigest(),
            logs=[])).encode())
        completions = self.binaries / "completions"
        completions.mkdir()
        names = subprocess.check_output([
            str(ROOT / "scripts/packaging/layout"), "source-names",
            "--variant", "unix-archive", "--kind", "completion"], text=True)
        for name in names.splitlines():
            (completions / name).write_text("# fixture completion\n")
        tools = self.root / "bin"
        tools.mkdir()
        signer = tools / "codesign"
        signer.write_text('''#!/bin/sh
if [ "$1" = --force ]; then
  printf 'signed fixture\n' >> "$4"
else
  test "${FAIL_VERIFY:-0}" != 1 || exit 17
fi
''')
        signer.chmod(0o755)
        self.env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                        CARGO_TARGET_DIR=str(self.target), TMPDIR=str(self.root),
                        RMUX_PACKAGE_CODESIGN_ADHOC="1", RMUX_PACKAGE_STRIP_TINY="0")

    def package(self):
        return subprocess.run([
            "bash", str(ROOT / "scripts/package-unix.sh"), "--configuration", "debug",
            "--target", "aarch64-apple-darwin", "--skip-build", "--allow-stale-binary",
            "--output-dir", str(self.root / "dist")], env=self.env,
            text=True, capture_output=True, timeout=30)

    def test_signed_archive_preserves_notice_inputs_and_hashes_final_payloads(self):
        for signing in ("1", "0", "1"):
            with self.subTest(signing=signing):
                self.env["RMUX_PACKAGE_CODESIGN_ADHOC"] = signing
                result = self.package()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(notices.payloads(self.binaries), self.original)
                notices.verified(self.binaries, True)
                archive, = (self.root / "dist").glob("*.tar.gz")
                with tarfile.open(archive) as package:
                    files = {member.name.split("/", 1)[1]: package.extractfile(member).read()
                             for member in package.getmembers() if member.isfile()}
                metadata = json.loads(files["share/rmux/artifact-metadata.json"])
                for prefix in ("binary", "helper_binary", "daemon_binary"):
                    payload = files[metadata[prefix + "_path"]]
                    self.assertEqual(payload.endswith(b"signed fixture\n"), signing == "1")
                    self.assertEqual(hashlib.sha256(payload).hexdigest(), metadata[prefix + "_sha256"])
                self.assertIn("share/licenses/rmux/THIRD-PARTY-NOTICES.txt", files)
                sidecar = Path(str(archive) + ".NOTICE.txt").read_text()
                self.assertIn("SHA256: " + notices.sha(archive), sidecar)

    def test_verification_failure_does_not_publish_or_mutate_cargo_inputs(self):
        self.env["FAIL_VERIFY"] = "1"
        self.assertNotEqual(self.package().returncode, 0)
        self.assertEqual(notices.payloads(self.binaries), self.original)
        self.assertFalse(list((self.root / "dist").glob("*.tar.gz")))

    def test_stale_notice_inputs_are_still_rejected(self):
        (self.binaries / "rmux").write_bytes(b"tampered input")
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("third-party notice does not match", result.stderr)
        self.assertFalse(list((self.root / "dist").glob("*.tar.gz")))


if __name__ == "__main__":
    unittest.main()
