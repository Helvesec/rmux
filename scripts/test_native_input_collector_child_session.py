"""Frozen R2-115 F2 acceptance: child opt-out cannot bypass an active consumer."""
import os
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import candidate_identity as guard
import native_input_windows_process as process
import test_native_input_collector_consumer as fixtures


class ChildNativeSession(unittest.TestCase):
    setUp = fixtures.ConsumerRetention.setUp
    assert_receipt = fixtures.ConsumerRetention.assert_receipt
    run_action = fixtures.ConsumerRetention.run_action

    def assert_child_refused(self, mutation):
        ticket = self.run_action()
        retained = Path(ticket['native_inputs']['resolved']['files'][str(self.manifest)]['object'])
        retained.chmod(0o600)
        if mutation == 'missing':
            retained.unlink()
        else:
            retained.write_bytes(b'changed retained manifest\n')
        env = dict(os.environ, RMUX_IDENTITY_TARGET=str(self.target),
                   RMUX_IDENTITY_HEAD=ticket['commit'], RMUX_IDENTITY_SOURCE=ticket['source'],
                   RMUX_IDENTITY_DOC_SESSION=ticket['session'],
                   CARGO_TARGET_DIR=str(self.directory / 'target'))
        # Only the child's manifest opt-in is removed. Parent and digest survive.
        del env['RMUX_NATIVE_INPUT_MANIFEST']
        self.assertEqual(os.environ['RMUX_NATIVE_INPUT_MANIFEST'], 'qualified')
        self.assertEqual(env['RMUX_NATIVE_INPUT_MANIFEST_SHA256'], 'manifest-sha')
        original = subprocess.Popen
        execute = original._execute_child
        # Exercise install's real Popen subclass and real OS launch boundary.
        # The fixture Cargo image is Python, so a regressing launch is harmless
        # and is waited before the frozen assertion reports failure.
        with patch.object(process, 'os', SimpleNamespace(**(vars(os) | {'name': 'nt'}))), \
                patch.object(subprocess, 'Popen', original), \
                patch.object(original, '_execute_child', autospec=True, side_effect=execute) as launched, \
                patch.object(guard, 'generation', return_value=ticket):
            process.install()
            with self.assertRaises((guard.IdentityError, RuntimeError, FileNotFoundError)):
                with subprocess.Popen([ticket['cargo'], 'check', '--locked', '--offline'],
                                      env=env, cwd=self.directory,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) as child:
                    child.wait()
            self.assertEqual(launched.call_count, 0, 'consumer reached OS process creation')

    def test_child_manifest_removed_with_missing_copy_refused_before_popen(self):
        self.assert_child_refused('missing')

    def test_child_manifest_removed_with_mutated_copy_refused_before_popen(self):
        self.assert_child_refused('mutated')


if __name__ == '__main__':
    unittest.main()
