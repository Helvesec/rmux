"""Exercise local release-tag validation without contacting GitHub."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EXPECTED_SHA = "a" * 40


class ReleaseRefInputTests(unittest.TestCase):
    def test_ref_verifier_accepts_only_stable_or_canonical_rc_tags(self):
        audit = ROOT / ".rmux-audit" / "release-ref-inputs"
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            fixture = Path(directory)
            trace = fixture / "gh-calls"
            gh = fixture / "gh"
            gh.write_text('''#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$*" >> "$RELEASE_REF_TRACE"
for endpoint in "$@"; do :; done
case "$endpoint" in
  */git/ref/tags/*)
    printf '{"object":{"type":"tag","sha":"%s"}}\\n' "${EXPECTED_SHA//a/b}" ;;
  */git/tags/*)
    printf '{"object":{"type":"commit","sha":"%s"},"verification":{"verified":true}}\\n' "$EXPECTED_SHA" ;;
  */releases/tags/*)
    printf 'HTTP/2.0 404 Not Found\\r\\n\\r\\n{"message":"Not Found"}\\n'
    exit 1 ;;
  *) exit 99 ;;
esac
''', encoding="utf-8")
            gh.chmod(0o755)
            environment = dict(
                os.environ, PATH=str(fixture) + os.pathsep + os.environ["PATH"],
                RELEASE_REF_TRACE=str(trace), EXPECTED_SHA=EXPECTED_SHA,
            )
            valid = ("v0.9.0", "v0.11.0", "v0.11.0-rc.1", "v0.11.0-rc.10")
            invalid = (
                "v00.11.0", "v0.011.0", "v0.11.00", "v0.11.0-rc.0",
                "v0.11.0-rc.01", "v0.11.0-rc.", "v0.11.0-rc.1.2",
                "v0.11.0-..", "v0.11.0.1", "v0.11.0-beta.1",
            )
            for tag in valid + invalid:
                with self.subTest(tag=tag):
                    trace.unlink(missing_ok=True)
                    result = subprocess.run(
                        ["bash", str(ROOT / "scripts/verify-release-ref.sh"),
                         "Helvesec/rmux", tag, EXPECTED_SHA],
                        cwd=ROOT, env=environment, capture_output=True, text=True,
                    )
                    if tag in valid:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn(f"release-ref-ok tag={tag}", result.stdout)
                        self.assertTrue(trace.exists())
                    else:
                        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                        self.assertIn("invalid release tag", result.stderr)
                        self.assertFalse(trace.exists(), "invalid tag reached GitHub")


if __name__ == "__main__":
    unittest.main()
