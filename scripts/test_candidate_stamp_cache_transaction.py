"""Transaction boundaries, failed commit and cleanup diagnostics stay fail-closed."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from test_candidate_stamp_cache_cleanup import constant_clock, load_guard


def held_marker_unlink(cache, release_after):
    """Unlink while a reader in pending_proof holds the commit marker open.

    Native Windows refuses that unlink with WinError 32; elsewhere the same
    sharing rule is injected for the held marker. The reader closes it after
    release_after refusals, or never when release_after is None.
    """
    unlink, refused, reader = os.unlink, [], []

    def windows_unlink(path, *args, **kwargs):
        if Path(cache.stable_path(path)).suffix != '.pending':
            return unlink(path, *args, **kwargs)
        if not reader and not refused:
            reader.append(open(path, 'rb'))
        if not reader:
            return unlink(path, *args, **kwargs)
        try:
            if os.name != 'nt':
                error = PermissionError(13, 'The process cannot access the file because it is '
                                            'being used by another process', str(path))
                error.winerror = 32
                raise error
            unlink(path, *args, **kwargs)
        except PermissionError as error:
            refused.append(getattr(error, 'winerror', None))
            if len(refused) == release_after:
                reader.pop().close()
            raise
        refused.append(None)

    return windows_unlink, refused, reader


class StampCacheTransaction(unittest.TestCase):
    def test_fresh_reader_refuses_either_format_until_commit(self):
        guard, cache = load_guard()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'qualified')
            replace = os.replace
            seen = []

            def publish(source, destination):
                replace(source, destination)
                if Path(destination).suffix in ('.json', '.marshal'):
                    original = Path(destination).with_suffix('.json')
                    binding = json.loads(original.read_bytes())['binding']
                    fresh_guard, fresh_cache = load_guard()
                    result = fresh_cache.reusable(fresh_guard, original, binding)
                    seen.append((Path(destination).suffix, result is fresh_cache._MISSING))

            with patch.object(cache.os, 'replace', publish):
                cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                             lambda: cache.read_bytes(guard, binary).decode())
            self.assertEqual(seen, [('.json', True), ('.marshal', True)])
            self.assertEqual(len(list((root / 'identity-cache').iterdir())), 2)
            self.assertEqual(cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                                         lambda: self.fail('committed proof was not reused')),
                             'qualified')

    def test_failed_commit_with_failed_cleanup_retains_invalidation(self):
        guard, cache = load_guard()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'before')
            unlink = os.unlink
            primary = PermissionError(13, 'commit marker removal denied')
            secondary = PermissionError(13, 'owned proof cleanup denied')
            committing = False
            attempts = []

            def refuse(path, *args, **kwargs):
                nonlocal committing
                path = Path(path)
                if path.suffix == '.pending':
                    committing = True
                    binary.write_bytes(b'after!')
                    raise primary
                if committing:
                    attempts.append(path.name)
                    raise secondary
                return unlink(path, *args, **kwargs)

            with constant_clock(guard), patch.object(cache.os, 'unlink', refuse), \
                    self.assertRaises(PermissionError) as caught:
                cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                             lambda: cache.read_bytes(guard, binary).decode())
            self.assertIs(caught.exception, primary)
            self.assertEqual(len(attempts), 4, 'both proofs and temporaries must be attempted')
            self.assertEqual(caught.exception.cleanup_errors, (secondary,) * 4)
            fresh_guard, fresh_cache = load_guard()
            with constant_clock(fresh_guard):
                self.assertEqual(fresh_cache.cached(
                    fresh_guard, 'built', binary, root, 'a' * 40, root,
                    lambda: fresh_cache.read_bytes(fresh_guard, binary).decode()), 'after!')

    def test_winerror32_on_a_held_commit_marker_keeps_the_published_proof(self):
        # R3468 under emulated Windows sharing: a reader in pending_proof held
        # the marker, os.unlink(marker) got WinError 32 and the failed-commit
        # cleanup erased the verified proof this publisher had just published.
        guard, cache = load_guard()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'qualified')
            unlink, refused, reader = held_marker_unlink(cache, release_after=1)
            with patch.object(cache.os, 'unlink', unlink):
                result = cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                                      lambda: cache.read_bytes(guard, binary).decode())
            self.assertEqual(result, 'qualified')
            self.assertEqual(refused, [32])
            self.assertEqual(reader, [])
            directory = root / 'identity-cache'
            self.assertEqual(len(list(directory.glob('*.json'))), 1)
            self.assertEqual(len(list(directory.glob('*.marshal'))), 1)
            self.assertEqual(sorted(path.name for path in directory.glob('.*')), [])
            fresh_guard, fresh_cache = load_guard()
            self.assertEqual(fresh_cache.cached(fresh_guard, 'built', binary, root, 'a' * 40, root,
                                                lambda: self.fail('committed proof was not reused')),
                             'qualified')

    def test_winerror32_on_a_marker_held_past_the_retries_keeps_the_proof_pending(self):
        guard, cache = load_guard()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'qualified')
            unlink, refused, reader = held_marker_unlink(cache, release_after=None)
            try:
                with patch.object(cache.os, 'unlink', unlink), \
                        patch.object(cache, '_COMMIT_RETRY_DELAYS', (0.001, 0.001)):
                    result = cache.cached(guard, 'built', binary, root, 'a' * 40, root,
                                          lambda: cache.read_bytes(guard, binary).decode())
            finally:
                for stream in reader:
                    stream.close()
            self.assertEqual(result, 'qualified')
            self.assertEqual(refused, [32, 32, 32])
            directory = root / 'identity-cache'
            self.assertEqual(len(list(directory.glob('*.json'))), 1)
            self.assertEqual(len(list(directory.glob('*.marshal'))), 1)
            self.assertEqual(len(list(directory.glob('*.pending'))), 1)
            fresh_guard, fresh_cache = load_guard()
            calculated = []
            self.assertEqual(fresh_cache.cached(
                fresh_guard, 'built', binary, root, 'a' * 40, root,
                lambda: calculated.append(1) or fresh_cache.read_bytes(fresh_guard, binary).decode()),
                'qualified')
            self.assertEqual(calculated, [1], 'an uncommitted proof must not be reused')


if __name__ == '__main__':
    unittest.main()
