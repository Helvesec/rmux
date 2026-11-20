"""Byte-contract regression for the focused removal canary; no compiler required."""

from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import public_api_cfg_canaries as canaries


class CopyComplete(Exception):
    """Stop at the first compiler operation, after the real copy/hash checks."""


class MaterializationSpec(unittest.TestCase):
    def test_frozen_and_single_token_mutant_preserve_lf_and_crlf_bytes(self):
        immutable = (canaries.FIXTURES / "baseline.rs").read_bytes()
        marker = b"pub fn combination_only"
        self.assertEqual(immutable.count(marker), 1)
        prefix, suffix = immutable.split(marker)
        for newline in (b"\n", b"\r\n"):
            with self.subTest(newline=newline), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                fixture = root / "baseline.rs"
                baseline = immutable.replace(b"\n", newline)
                fixture.write_bytes(baseline)
                runner = Mock(output=root / "output", cargo="unused-cargo")
                runner.document.side_effect = CopyComplete
                with patch.object(canaries, "FIXTURES", root):
                    with self.assertRaises(CopyComplete):
                        canaries.removal(runner)
                self.assertEqual(fixture.read_bytes(), baseline)
                self.assertEqual((runner.output / "frozen/src/lib.rs").read_bytes(), baseline)
                expected = (prefix + b"fn combination_only" + suffix).replace(b"\n", newline)
                self.assertEqual((runner.output / "mutant/src/lib.rs").read_bytes(), expected)
                self.assertEqual(runner.run.call_count, 2)
                runner.document.assert_called_once()

    def test_missing_or_duplicate_mutation_token_fails_before_commands(self):
        for baseline in (b"pub fn retained() {}\n", b"pub fn combination_only() {}\n" * 2):
            with self.subTest(baseline=baseline), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "baseline.rs").write_bytes(baseline)
                runner = Mock(output=root / "output", cargo="unused-cargo")
                with patch.object(canaries, "FIXTURES", root):
                    with self.assertRaisesRegex(RuntimeError, "exactly one combination_only export"):
                        canaries.removal(runner)
                runner.run.assert_not_called()
                self.assertEqual((root / "baseline.rs").read_bytes(), baseline)


if __name__ == "__main__":
    unittest.main()
