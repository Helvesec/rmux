"""A publisher entering during cleanup must retain its replacement inode."""
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

from test_candidate_stamp_cache_cleanup import load_guard


class StampCacheConcurrency(unittest.TestCase):
    def test_publisher_between_cleanup_lstat_and_unlink_preserves_both_formats(self):
        for suffix in ('.json', '.marshal'):
            with self.subTest(suffix=suffix), tempfile.TemporaryDirectory() as temporary:
                guard, cache = load_guard()
                root = Path(temporary).resolve()
                binary = root / 'binary'
                binary.write_bytes(b'qualified')
                replace, lstat, reusable = os.replace, os.lstat, cache.reusable
                owner = threading.get_ident()
                failed = False
                triggered = False
                started, finished = threading.Event(), threading.Event()
                primary = OSError(5, 'publication refused after replacement')
                outcomes, identities, threads = [], {}, []

                def verify(calculate):
                    return cache.cached(guard, 'built', binary, root, 'a' * 40, root, calculate)

                def competing_publisher():
                    started.set()
                    try:
                        outcomes.append(verify(lambda: cache.read_bytes(guard, binary).decode()))
                        for path in (root / 'identity-cache').glob('*'):
                            if path.suffix in ('.json', '.marshal'):
                                info = lstat(path)
                                identities[path] = (info.st_dev, info.st_ino)
                    except BaseException as error:
                        outcomes.append(error)
                    finally:
                        finished.set()

                def cold_competitor(*args, **kwargs):
                    if threading.get_ident() != owner:
                        return cache._MISSING
                    return reusable(*args, **kwargs)

                def publish(source, destination):
                    nonlocal failed
                    replace(source, destination)
                    if threading.get_ident() == owner and str(destination).endswith('.marshal'):
                        failed = True
                        raise primary

                def at_ownership_check(path, *args, **kwargs):
                    nonlocal triggered
                    info = lstat(path, *args, **kwargs)
                    if (threading.get_ident() == owner and failed and not triggered
                            and Path(path).suffix == suffix and not Path(path).name.startswith('.')):
                        triggered = True
                        thread = threading.Thread(target=competing_publisher)
                        threads.append(thread)
                        thread.start()
                        self.assertTrue(started.wait(5), 'competitor did not enter')
                        # Without serialization B replaces I with J before this
                        # returns stat(I). With serialization B waits until unlink.
                        finished.wait(1)
                    return info

                with patch.object(cache.os, 'replace', publish), \
                        patch.object(cache.os, 'lstat', at_ownership_check), \
                        patch.object(cache, 'reusable', cold_competitor):
                    with self.assertRaises(OSError) as caught:
                        verify(lambda: cache.read_bytes(guard, binary).decode())
                    for thread in threads:
                        thread.join(10)
                        self.assertFalse(thread.is_alive(), 'publisher remained blocked')
                self.assertIs(caught.exception, primary)
                self.assertTrue(triggered, 'ownership boundary was not exercised')
                self.assertEqual(outcomes, ['qualified'])
                self.assertEqual(len(identities), 2)
                for path, identity in identities.items():
                    self.assertTrue(path.exists(), 'cleanup erased the competing inode')
                    info = path.stat()
                    self.assertEqual((info.st_dev, info.st_ino), identity)
                self.assertEqual(verify(lambda: self.fail('competing proof was not reusable')),
                                 'qualified')
                self.assertEqual(cache._observations, [])


if __name__ == '__main__':
    unittest.main()
