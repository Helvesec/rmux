"""Failed cleanup must preserve the publication error and prevent stale reuse."""
from contextlib import contextmanager
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py')


def load_guard():
    spec = importlib.util.spec_from_file_location('guard', SCRIPT)
    guard = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(guard)
    return guard, guard.support('candidate_identity_cache')


@contextmanager
def constant_clock(guard):
    stamp = guard.stamp
    windows = guard.support('candidate_identity_windows')
    with patch.object(guard, 'stamp', lambda info: (*stamp(info)[:4], 1, 1)), \
            patch.object(windows, 'change_time', return_value=1):
        yield


class StampCacheCleanup(unittest.TestCase):
    def test_secondary_cleanup_errors_preserve_original_and_force_fresh_reuse(self):
        for operation in ('unlink', 'lstat'):
            for denied in ('json', 'marshal', 'temporary-json', 'temporary-marshal'):
                with self.subTest(operation=operation, denied=denied), \
                        tempfile.TemporaryDirectory() as temporary:
                    guard, cache = load_guard()
                    root = Path(temporary).resolve()
                    binary = root / 'binary'
                    binary.write_bytes(b'before')
                    primary = OSError(5, 'marshal publication failed')
                    secondary = PermissionError(13, 'cleanup refused')
                    replace, lstat, unlink = os.replace, os.lstat, os.unlink
                    failing = False
                    attempts = []

                    def publish(source, destination):
                        nonlocal failing
                        if str(destination).endswith('.marshal'):
                            # Also cover failure reported after the rename took effect.
                            if denied == 'marshal':
                                replace(source, destination)
                            binary.write_bytes(b'after!')
                            failing = True
                            raise primary
                        replace(source, destination)

                    def refuse(action, path, *args, **kwargs):
                        name = Path(path).name
                        kind = ('temporary-' if name.startswith('.') else '')
                        kind += 'marshal' if name.endswith('.marshal') else 'json'
                        if failing:
                            attempts.append(kind)
                            if action == operation and kind == denied:
                                raise secondary
                        return (lstat if action == 'lstat' else unlink)(path, *args, **kwargs)

                    with constant_clock(guard), patch.object(cache.os, 'replace', publish), \
                            patch.object(cache.os, 'lstat', lambda *a, **k: refuse('lstat', *a, **k)), \
                            patch.object(cache.os, 'unlink', lambda *a, **k: refuse('unlink', *a, **k)):
                        try:
                            cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                                         lambda: cache.read_bytes(guard, binary).decode())
                        except BaseException as error:
                            caught = error
                        else:
                            self.fail('failed publication returned success')
                    with self.subTest(check='original exception'):
                        self.assertIs(caught, primary)
                    with self.subTest(check='both destinations attempted'):
                        self.assertTrue({'json', 'marshal'}.issubset(attempts))
                    # A new module has no memory of the failure. The disk state must
                    # still forbid reuse even when stamps hide the byte mutation.
                    fresh_guard, fresh_cache = load_guard()
                    calls = []

                    def recalculate():
                        calls.append(None)
                        return fresh_cache.read_bytes(fresh_guard, binary).decode()

                    with constant_clock(fresh_guard):
                        result = fresh_cache.cached(fresh_guard, 'built', binary, root,
                                                    'a' * 40, root, recalculate)
                    with self.subTest(check='durable invalidation'):
                        self.assertEqual(result, 'after!')
                        self.assertEqual(calls, [None])
                    self.assertEqual(cache._observations, [])


if __name__ == '__main__':
    unittest.main()
