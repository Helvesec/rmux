"""Cold byte witnesses survive serialization, publication races and nested reuse."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py')


class StampCacheBoundaries(unittest.TestCase):
    def guard(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        return guard, guard.support('candidate_identity_cache')

    def test_mutation_while_serializing_or_publishing_refuses_both_formats(self):
        for phase in ('json', 'marshal', 'replace-json', 'replace-marshal',
                      'winerror5-json', 'winerror5-marshal'):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temporary:
                guard, cache = self.guard()
                root = Path(temporary).resolve()
                binary = root / 'binary'
                binary.write_bytes(b'before')
                module = json if phase == 'json' else cache.marshal
                dumps, replace = module.dumps, os.replace

                def serialize(*args, **kwargs):
                    data = dumps(*args, **kwargs)
                    binary.write_bytes(b'after!')
                    return data

                def publish(source, destination):
                    replace(source, destination)
                    if str(destination).endswith('.' + phase.split('-')[-1]):
                        binary.write_bytes(b'after!')
                        if phase.startswith('winerror5'):
                            error = PermissionError(13, 'Access is denied', str(destination))
                            error.winerror = 5
                            raise error

                mutation = (patch.object(module, 'dumps', serialize)
                            if phase in ('json', 'marshal')
                            else patch.object(cache.os, 'replace', publish))
                with mutation, self.assertRaises(guard.IdentityError):
                    cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                                 lambda: {'bytes': cache.read_bytes(guard, binary).decode()})
                self.assertEqual(list((root / 'identity-cache').iterdir()), [])
                self.assertEqual(cache._observations, [])

    def test_same_stamp_observed_and_nested_reused_evidence_is_rechecked(self):
        for phase in ('observed', 'reused'):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temporary:
                guard, cache = self.guard()
                root = Path(temporary).resolve()
                binary, dependency = root / 'binary', root / 'dependency'
                binary.write_bytes(b'binary')
                dependency.write_bytes(b'before')
                stamp = guard.stamp
                windows = guard.support('candidate_identity_windows')
                with patch.object(guard, 'stamp', lambda info: (*stamp(info)[:4], 1, 1)), \
                        patch.object(windows, 'change_time', return_value=1):
                    def inner():
                        return cache.cached(guard, 'built', dependency, root, 'a' * 40, root,
                                            lambda: cache.read_bytes(guard, dependency).decode())
                    if phase == 'reused':
                        self.assertEqual(inner(), 'before')

                    def calculate():
                        if phase == 'reused':
                            self.assertEqual(inner(), 'before')
                        else:
                            cache.observe(guard, dependency)
                        dependency.write_bytes(b'after!')
                        return {}

                    with self.assertRaises(guard.IdentityError):
                        cache.cached(guard, 'built', binary, root, 'a' * 40, root, calculate)
                # The earlier nested proof remains separate; no outer proof escaped.
                self.assertEqual(len(list((root / 'identity-cache').glob('*.json'))),
                                 int(phase == 'reused'))
                self.assertEqual(cache._observations, [])

    def test_disappearance_during_read_is_an_identity_refusal(self):
        guard, cache = self.guard()
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary).resolve() / 'binary'
            binary.write_bytes(b'before')
            read = Path.read_bytes

            def remove(path):
                data = read(path)
                path.unlink()
                return data

            with patch.object(Path, 'read_bytes', remove), self.assertRaises(guard.IdentityError):
                cache.read_bytes(guard, binary)


if __name__ == '__main__':
    unittest.main()
