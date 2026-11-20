"""Byte changes must not hide behind identical filesystem timestamps."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py')


class StampCachePublication(unittest.TestCase):
    def test_same_stamp_evidence_is_refused_during_read_and_before_publication(self):
        for phase in ('read', 'publish'):
            for name in ('binary', 'dependency'):
                with self.subTest(phase=phase, evidence=name), tempfile.TemporaryDirectory() as temporary:
                    spec = importlib.util.spec_from_file_location('guard', SCRIPT)
                    guard = importlib.util.module_from_spec(spec)
                    spec.loader.exec_module(guard)
                    cache = guard.support('candidate_identity_cache')
                    root = Path(temporary).resolve()
                    binary, dependency = root / 'binary', root / 'dependency'
                    for path in (binary, dependency):
                        path.write_bytes(b'before')
                    changed = root / name
                    stamp = guard.stamp
                    read = Path.read_bytes

                    def same_clock(info):
                        # Model writes within one native metadata clock tick.
                        return (*stamp(info)[:4], 1, 1)

                    def mutate(path):
                        data = read(path)
                        if path == changed:
                            path.write_bytes(b'after!')
                        return data

                    def calculate():
                        for path in (binary, dependency):
                            if phase == 'read' and path == changed:
                                with patch.object(Path, 'read_bytes', mutate):
                                    cache.read_bytes(guard, path)
                            else:
                                cache.read_bytes(guard, path)
                        if phase == 'publish':
                            changed.write_bytes(b'after!')
                        return {'bytes': 'before'}

                    windows = guard.support('candidate_identity_windows')
                    with patch.object(guard, 'stamp', same_clock), \
                            patch.object(windows, 'change_time', return_value=1):
                        with self.assertRaises(guard.IdentityError):
                            cache.cached(guard, 'built', binary, root, 'a' * 40, root, calculate)
                    self.assertEqual(list((root / 'identity-cache').iterdir()), [])
                    self.assertEqual(cache._observations, [])


if __name__ == '__main__':
    unittest.main()
