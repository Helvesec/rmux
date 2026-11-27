"""Cargo already forwards its qualified runner to rustdoc on current toolchains."""
import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch


class DoctestRunner(unittest.TestCase):
    def test_cargo_runner_is_composed_but_foreign_runner_is_rejected(self):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        source = script.parent.parent
        target = source / '.rmux-audit' / 'doctest-runner-unit'
        target.mkdir(parents=True, exist_ok=True)
        base = [sys.executable, '--test', str(source / 'crates/rmux-sdk/src/lib.rs')]
        runner = ['--test-runtool', sys.executable, '--test-runtool-arg', str(script),
                  '--test-runtool-arg', 'run']
        with patch.object(guard, 'context', return_value=(source, '0' * 40, target)), \
                patch.object(guard, 'source_identity'), \
                patch.object(guard, 'tracked_input', return_value='qualified-test-origin'), \
                patch.dict(os.environ, {'RUSTC_WRAPPER': str(target / 'wrapper')}), \
                patch.object(guard.subprocess, 'call', return_value=0) as process:
            self.assertEqual(guard.rustdoc_wrapper(base + runner), 0)
            args = process.call_args.args[0]
            self.assertEqual(args.count('--test-runtool'), 1)
            self.assertIn('--test-builder-wrapper', args)
            process.reset_mock()
            with self.assertRaises(guard.IdentityError):
                guard.rustdoc_wrapper(base + ['--test-runtool', '/foreign/runner'])
            process.assert_not_called()


if __name__ == '__main__':
    unittest.main()
