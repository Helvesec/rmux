"""Publication I/O failures must not leave an owned stale proof reusable."""
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py')


class StampCachePublishIO(unittest.TestCase):
    @contextmanager
    def fixture(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        cache = guard.support('candidate_identity_cache')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'before')
            stamp = guard.stamp
            windows = guard.support('candidate_identity_windows')
            with patch.object(guard, 'stamp', lambda info: (*stamp(info)[:4], 1, 1)), \
                    patch.object(windows, 'change_time', return_value=1):
                yield guard, cache, root, binary

    def test_publication_errors_remove_owned_proofs_and_force_recalculation(self):
        for phase in ('json', 'marshal'):
            for published in (False, True):
                for error_type in (OSError, PermissionError):
                    with self.subTest(phase=phase, published=published, error=error_type.__name__), \
                            self.fixture() as (guard, cache, root, binary):
                        error = error_type(13, 'publication refused')
                        if error_type is PermissionError:
                            error.winerror = 32
                        replace = os.replace

                        def publish(source, destination):
                            if str(destination).endswith('.' + phase):
                                if published:
                                    replace(source, destination)
                                binary.write_bytes(b'after!')
                                raise error
                            replace(source, destination)

                        def verify(calculate):
                            return cache.cached(guard, 'built', binary, root, 'a' * 40, root, calculate)

                        with patch.object(cache.os, 'replace', publish), \
                                self.assertRaises(error_type) as caught:
                            verify(lambda: cache.read_bytes(guard, binary).decode())
                        self.assertIs(caught.exception, error)
                        leftovers = list((root / 'identity-cache').iterdir())
                        calculations = []

                        def recalculate():
                            calculations.append(None)
                            return cache.read_bytes(guard, binary).decode()

                        self.assertEqual(verify(recalculate), 'after!', 'stale proof was reused')
                        self.assertEqual(calculations, [None])
                        self.assertEqual(leftovers, [], 'failed publisher left owned files')
                        self.assertEqual(cache._observations, [])

    def test_publication_error_preserves_concurrent_inodes_in_either_format(self):
        for concurrent in (('json',), ('marshal',), ('json', 'marshal')):
            for error_type in (OSError, PermissionError):
                with self.subTest(concurrent=concurrent, error=error_type.__name__), \
                        self.fixture() as (guard, cache, root, binary):
                    replace = os.replace
                    error = error_type(13, 'publication refused')
                    if error_type is PermissionError:
                        error.winerror = 32
                    identities = {}

                    def publish(source, destination):
                        replace(source, destination)
                        if str(destination).endswith('.marshal'):
                            binary.write_bytes(b'after!')
                            directory = root / 'identity-cache'
                            original = next(directory.glob('*.json'))
                            proof = json.loads(original.read_bytes())
                            proof['record'] = 'after!'
                            for suffix in concurrent:
                                target = original.with_suffix('.' + suffix)
                                temporary = directory / ('competitor-' + suffix)
                                data = (json.dumps(proof).encode() if suffix == 'json'
                                        else cache.marshal.dumps(proof))
                                temporary.write_bytes(data)
                                replace(temporary, target)
                                info = target.stat()
                                identities[target] = (info.st_dev, info.st_ino)
                            raise error

                    with patch.object(cache.os, 'replace', publish), \
                            self.assertRaises(error_type) as caught:
                        cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                                     lambda: cache.read_bytes(guard, binary).decode())
                    self.assertIs(caught.exception, error)
                    self.assertEqual(set((root / 'identity-cache').iterdir()), set(identities))
                    for path, identity in identities.items():
                        info = path.stat()
                        self.assertEqual((info.st_dev, info.st_ino), identity)
                    self.assertEqual(cache.cached(
                        guard, 'built', binary, root, 'a' * 40, root,
                        lambda: self.fail('concurrent proof was discarded')), 'after!')
                    self.assertEqual(cache._observations, [])


if __name__ == '__main__':
    unittest.main()
