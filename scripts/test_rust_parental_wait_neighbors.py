"""Frozen neighbors at the same native Rust helper, without public Cargo."""
import json
import os
from pathlib import Path
import secrets
import subprocess
import unittest
from unittest.mock import patch


class RustWaitNeighbors(unittest.TestCase):
    def setUp(self):
        self.root = Path(os.environ['RMUX_F7_AUDIT'])
        self.fixture = Path(os.environ['RMUX_F7_NEIGHBORS'])
        self.marker = self.root / ('child-marker-' + secrets.token_hex(16))
        self.env = dict(os.environ, RMUX_F7_MARKER=str(self.marker))

    def launch(self, mode='', env=None, image=None, code=0):
        # Reach Rust's own rejection boundary even for deliberately bad inputs.
        # Python's installed guard would reject before the fixture could run.
        popen = subprocess.Popen
        original = popen.__mro__[1] if getattr(popen, '_rmux_native_waits', False) else popen
        with patch.object(subprocess, 'Popen', original):
            return subprocess.run([str(self.fixture), str(image or self.fixture), str(code), mode],
                                  env=self.env if env is None else env, capture_output=True)

    def refused(self, mode='', env=None, image=None):
        result = self.launch(mode, env, image)
        self.assertEqual(result.returncode, 86)
        self.assertIn(b'launch-error:', result.stderr)
        self.assertFalse(self.marker.exists(), 'unqualified Rust child executed')

    def test_effective_child_manifest_or_digest_cannot_be_dropped(self):
        for mode in ('drop-manifest', 'drop-digest'):
            with self.subTest(mode=mode):
                self.refused(mode)

    def test_adverse_head_and_session_refused_before_child(self):
        for mode in ('adverse-head', 'adverse-session'):
            with self.subTest(mode=mode):
                self.refused(mode)

    def test_mutated_manifest_digest_refused_before_child(self):
        self.refused(env=dict(self.env, RMUX_NATIVE_INPUT_MANIFEST_SHA256='0' * 64))

    def test_missing_or_mutated_retained_image_refused_before_child(self):
        manifest = json.loads(Path(self.env['RMUX_NATIVE_INPUT_MANIFEST']).read_text())
        obj = Path(manifest['files'][str(self.fixture)]['object'])
        original = obj.read_bytes()
        obj.chmod(0o600)
        backup = obj.with_name(obj.name + '.fixture-backup')
        try:
            obj.rename(backup)
            self.refused()
            backup.rename(obj)
            obj.write_bytes(b'mutated retained image')
            self.refused()
        finally:
            if backup.exists():
                backup.rename(obj)
            obj.write_bytes(original)
            obj.chmod(0o400)

    def test_launch_error_and_no_manifest_preserve_existing_semantics(self):
        absent = self.root / 'never-created.exe'
        self.refused(image=absent)
        env = {k: v for k, v in self.env.items() if k not in
               ('RMUX_NATIVE_INPUT_MANIFEST', 'RMUX_NATIVE_INPUT_MANIFEST_SHA256')}
        self.refused(env=env, image=absent)
        for code in (0, 7):
            with self.subTest(code=code):
                result = self.launch(env=env, code=code)
                self.assertEqual(result.returncode, code)
                self.assertEqual(result.stdout, b'')
                self.assertEqual(result.stderr, b'')
                self.assertTrue(self.marker.exists())
                self.marker.unlink()

    def test_stdout_stderr_larger_than_pipe_capacity_are_drained(self):
        result = self.launch(env=dict(self.env, RMUX_F7_BYTES=str(256 * 1024)), code=7)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, b'o' * (256 * 1024))
        self.assertEqual(result.stderr, b'e' * (256 * 1024))
        self.assertTrue(self.marker.exists())

    def test_distinct_births_are_never_joined_by_pid(self):
        # The existing adversarial native trace has several lives of one PID.
        import test_native_input_collector_windows_evidence as evidence
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(evidence.WindowsNativeEvidence)
        selected = [test for test in suite if 'reused' in test.id()]
        self.assertEqual(len(selected), 1)
        result = unittest.TestResult()
        selected[0].run(result)
        self.assertTrue(result.wasSuccessful(), (result.errors, result.failures))


if __name__ == '__main__':
    unittest.main(verbosity=2)
