"""Consumer cwd and child environment at the actual Popen input boundary."""
import os
from pathlib import Path
import unittest
from unittest.mock import patch

import candidate_identity as guard
import native_input_windows_process as process
import test_native_input_collector_consumer as fixtures


class ConsumerLaunchBoundary(unittest.TestCase):
    setUp = fixtures.ConsumerRetention.setUp
    assert_receipt = fixtures.ConsumerRetention.assert_receipt
    run_action = fixtures.ConsumerRetention.run_action

    def launch(self, ticket, cwd, executable=None):
        env = dict(os.environ, RMUX_IDENTITY_TARGET=str(self.target),
                   RMUX_IDENTITY_DOC_SESSION=ticket['session'])
        argv = [executable or ticket['cargo'], 'check', '--locked', '--offline']
        with patch.dict(os.environ, {'RMUX_IDENTITY_DOC_SESSION': 'b' * 64}), \
                patch.object(guard, 'generation', return_value=ticket) as generation:
            paths = process.generated_inputs(argv, env, cwd)
            return paths, generation.call_args

    def test_child_session_and_cwd_retain_exact_project_population(self):
        ticket = self.run_action()
        paths, call = self.launch(ticket, self.directory)
        self.assertEqual(set(paths), {self.manifest, self.code, self.lock})
        self.assertEqual(call.kwargs, {'token': ticket['session']})

    def test_foreign_cargo_cwd_is_refused(self):
        ticket = self.run_action()
        with self.assertRaises(guard.IdentityError):
            self.launch(ticket, self.source)

    def test_git_validation_does_not_reenter_consumer_validation(self):
        ticket = self.run_action()
        git = self.source / 'git-fixture'
        git.write_text('never executed')
        paths, call = self.launch(ticket, self.directory, str(git))
        self.assertEqual(paths, ())
        self.assertIsNone(call)


if __name__ == '__main__':
    unittest.main()
