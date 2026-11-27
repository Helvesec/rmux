"""Surface conformance fixtures must run without a pre-existing audit directory."""
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import surface_manifest as manifest


class FreshConformanceFixtures(unittest.TestCase):
    def test_each_fixture_initializes_its_own_parent_on_a_fresh_source(self):
        inputs = set(manifest.project()['inputs'])
        inputs.add('scripts/test_surface_conformance.py')
        cases = (
            'ConformanceRegressions.test_python_class_skip_and_method_skip_are_not_cases',
            'TerminalLinkMutations.test_every_terminal_link_refuses_missing_or_disabled_case',
            'TerminalLinkMutations.test_terminal_conformance_module_must_remain_registered_and_active',
            'TerminalLinkMutations.test_new_terminal_family_requires_its_own_conformance_case',
        )
        for case in cases:
            with self.subTest(case=case), tempfile.TemporaryDirectory(
                    prefix='fresh-surface-') as directory:
                root = Path(directory)
                for name in inputs:
                    target = root / name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(manifest.ROOT / name, target)
                self.assertFalse((root / '.rmux-audit').exists())
                result = subprocess.run(
                    [sys.executable, '-B', '-m', 'unittest',
                     'test_surface_conformance.' + case],
                    cwd=root / 'scripts', capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn('Ran 1 test', result.stderr)
                self.assertTrue((root / '.rmux-audit').is_dir())


if __name__ == '__main__':
    unittest.main()
