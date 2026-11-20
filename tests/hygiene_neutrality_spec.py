#!/usr/bin/env python3
"""Frozen regressions for tracked provenance hygiene and platform boundaries."""

import hashlib
from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[1]
FROZEN_GATES = {
    "cfg-check.sh": "6bb900e1314c870c2ea5ac28c6122761edb84304853c40eb06ef75936abb66a1",
    "check-platform-neutrality.sh": "90aa994a0aecf3564a2d50001f52924d6c9ed2a7079097d8a1364aa3d3a77e95",
    "check-worktree-hygiene.sh": "29bc5d3e75762a7a74f155ebb558d39e5402dfbc17146fca1397ae84ba6029a5",
}


class HygieneNeutralityTests(unittest.TestCase):
    def test_analysis_rules_are_unchanged(self):
        for name, expected in FROZEN_GATES.items():
            with self.subTest(gate=name):
                self.assertEqual(hashlib.sha256((ROOT / "scripts" / name).read_bytes()).hexdigest(), expected)

    def check_gate(self, name):
        result = subprocess.run(
            ["bash", str(ROOT / "scripts" / name)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def test_tracked_provenance_and_runtime_artifacts_are_hygienic(self):
        self.assertIn("worktree-hygiene=ok", self.check_gate("check-worktree-hygiene.sh"))

    def test_os_selection_stays_within_original_platform_budgets(self):
        self.assertIn(
            "Platform-neutral crate boundary check passed.",
            self.check_gate("check-platform-neutrality.sh"),
        )

    def test_windows_gate_cfg_budgets_match_unix_gate(self):
        unix = re.findall(
            r"^check_budget (\S+) (\S+) (\d+)$",
            (ROOT / "scripts" / "cfg-check.sh").read_text(),
            re.M,
        )
        windows = re.findall(
            r"""^\s*@\(\s*(["'])([^"']+)\1\s*,\s*(["'])([^"']+)\3\s*,\s*(\d+)\s*\)\s*,?\s*(?:#.*)?$""",
            (ROOT / "scripts" / "release-review-gate-windows.ps1").read_text(),
            re.M,
        )
        self.assertEqual(len(unix), 9)
        self.assertEqual(
            sorted((crate, path.replace("\\", "/"), budget) for _, crate, _, path, budget in windows),
            sorted(unix),
        )


if __name__ == "__main__":
    unittest.main()
