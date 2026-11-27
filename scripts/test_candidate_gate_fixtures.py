"""Identity fixtures must reach their assertions inside and outside a gate."""
import io
import os
import subprocess
import sys
import unittest
from unittest.mock import patch

import test_candidate_identity_manifest as manifest_tests


class GateFixtureRegression(unittest.TestCase):
    def test_provenance_fixtures_reach_their_positive_and_negative_assertions(self):
        names = ('test_candidate_build_probe', 'test_candidate_empty_metadata',
                 'test_candidate_producer_history')
        suite = unittest.defaultTestLoader.loadTestsFromNames(names)
        output = io.StringIO()
        result = unittest.TextTestRunner(stream=output, verbosity=2).run(suite)
        self.assertTrue(result.wasSuccessful(), output.getvalue())

    def test_standalone_fixture_owns_fresh_context_without_changing_outer_gate(self):
        inherited = dict(RMUX_IDENTITY_HEAD='outer-gate-head',
                         RMUX_IDENTITY_SOURCE='/outer-gate-source',
                         RMUX_IDENTITY_TARGET='/outer-gate-target',
                         CARGO_TARGET_DIR='/outer-gate-target',
                         RUSTC_WRAPPER='/outer-gate-wrapper',
                         CARGO_TARGET_FIXTURE_RUNNER='/outer-gate-runner')
        with patch.dict(os.environ, inherited):
            outer = dict(os.environ)
            fixture = manifest_tests.StandaloneManifestIdentity()
            fixture.setUp()
            env, target = fixture.prepare()
            self.assertEqual(env['RMUX_IDENTITY_HEAD'], fixture.head)
            self.assertEqual(env['RMUX_IDENTITY_SOURCE'], str(fixture.repo))
            self.assertEqual(target, fixture.g.target_directory(
                fixture.repo, fixture.head, target.name))
            self.assertEqual(dict(os.environ), outer)
            # Isolation belongs to the fixture. The production entrypoint
            # must still reject an inherited identity before the scenario.
            rejected = subprocess.run(
                [sys.executable, str(manifest_tests.SCRIPT), 'with-manifest',
                 str(fixture.manifest), '--', sys.executable, '-c',
                 'print("scenario reached")'], cwd=fixture.repo,
                env=outer, capture_output=True, text=True)
            self.assertEqual(rejected.returncode, 86, rejected.stderr)
            self.assertIn('requires a fresh identity context', rejected.stderr)
            self.assertEqual(rejected.stdout, '')


if __name__ == '__main__':
    unittest.main()
