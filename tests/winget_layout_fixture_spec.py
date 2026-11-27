"""Portable preflight of the files assembled by the native WinGet runner.

Stop before any native command; exercise the retained fixture's two layout
entry paths with Python and sh. This does not validate PowerShell behavior.
"""

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "winget_native", ROOT / "tests/winget_layout_native_spec.py")
NATIVE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NATIVE)


class BeforeNativeCommand(Exception):
    pass


class WinGetFixturePreflight(unittest.TestCase):
    def test_fixture_layout_entry_paths_use_the_local_descriptor(self):
        bash = shutil.which("bash")
        self.assertIsNotNone(bash)
        original = ROOT / "scripts/packaging/rmux-package-layout.json"
        original_bytes = original.read_bytes()
        for case in NATIVE.CASES[:4]:
            name, changed_path, changed_alias, _, _ = case
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary) / "native-fixtures"
                argv = ["winget_layout_native_spec.py", "--powershell", sys.executable,
                        "--bash", bash, "--work-dir", str(work)]
                # Only the Windows admission check is simulated. The runner
                # constructs its real fixture, then stops at its first child.
                runner = SimpleNamespace(
                    run=unittest.mock.Mock(side_effect=BeforeNativeCommand),
                    DEVNULL=subprocess.DEVNULL,
                    TimeoutExpired=subprocess.TimeoutExpired)
                with patch.object(NATIVE, "os", SimpleNamespace(name="nt", environ=os.environ)), \
                        patch.object(NATIVE, "CASES", (case,)), \
                        patch.object(NATIVE, "subprocess", runner), patch.object(sys, "argv", argv):
                    with self.assertRaises(BeforeNativeCommand):
                        NATIVE.main()
                fixture = work / name
                packaging = fixture / "scripts/packaging"
                expected_path = NATIVE.CHANGED_PATH if changed_path else NATIVE.CANONICAL_PATH
                expected_alias = NATIVE.CHANGED_ALIAS if changed_alias else "rmux"
                environment = dict(os.environ, RMUX_PYTHON=sys.executable,
                                   PYTHONDONTWRITEBYTECODE="1")
                # PowerShell's helper selects this dispatcher; execute that
                # Python boundary directly, without claiming a native result.
                helper = (packaging / "layout-tool.ps1").read_text(encoding="utf-8")
                self.assertIn('Join-Path $PSScriptRoot "layout_dispatch.py"', helper)
                entries = (["sh", str(packaging / "layout")],
                           [sys.executable, str(packaging / "layout_dispatch.py")])
                for entry in entries:
                    for arguments, expected in [
                        (["nested-exe", "--package-name", NATIVE.PACKAGE], expected_path),
                        (["command-alias", "--consumer", "winget"], expected_alias),
                    ]:
                        outcome = subprocess.run(
                            [*entry, *arguments], cwd=fixture, env=environment,
                            capture_output=True, text=True, timeout=15)
                        self.assertEqual(outcome.returncode, 0, outcome.stderr)
                        self.assertEqual(outcome.stdout.strip(), expected)
                self.assertEqual(original.read_bytes(), original_bytes)


if __name__ == "__main__":
    unittest.main()
