"""Acceptance coverage for the reusable maintainer publication procedure."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


class PublicationRunbookTest(unittest.TestCase):
    def test_runbook_is_an_english_procedure_for_any_release(self):
        text = (ROOT / "docs/release/publication-runbook.md").read_text(encoding="utf-8")
        forbidden = (
            r"\bsponsor\b", r"\bD\d+(?:-bis)?\b", r"\bL\d+\.\d+\b",
            r"\b\d{4}-\d{2}-\d{2}\b",
            r"\b\d{1,2}\s+(?:October|November)\b",
            r"\b(?:En attente|répétition|livraison|décision)\b",
            r"\bv?\d+\.\d+\.\d+\b",
        )
        violations = [match.group(0) for pattern in forbidden
                      for match in re.finditer(pattern, text, re.IGNORECASE)]
        self.assertEqual(violations, [], "internal planning escaped into the runbook")
        self.assertIn("maintainer", text)
        self.assertIn("## Candidate rehearsal", text)
        self.assertIn("## Release publication", text)


if __name__ == "__main__":
    unittest.main()
