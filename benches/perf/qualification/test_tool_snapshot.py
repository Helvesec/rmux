"""File identity controls, including native Windows stat/fstat differences."""
import hashlib
import os
from pathlib import Path
import stat
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'scripts/bench'))
import tool_identity as t


class SnapshotControls(unittest.TestCase):
    @unittest.skipIf(os.name == 'nt', 'Preserve the Unix observation window')
    def test_unix_observes_symlink_target_contents_at_open(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'target'
            target.write_bytes(b'old executable\n')
            target.chmod(0o700)
            link = Path(directory) / 'tool'
            link.symlink_to(target)
            contents = b'new executable contents\n'
            original = Path.open

            def update_before_open(path, *args, **kwargs):
                if path == target and args == ('rb',):
                    with original(target, 'wb') as stream:
                        stream.write(contents)
                return original(path, *args, **kwargs)

            declaration = {'declared': str(link), 'selected': str(link), 'lookup': 'path'}
            with patch.object(Path, 'open', update_before_open):
                result = t.snapshot(declaration)
            self.assertEqual(result['sha256'], hashlib.sha256(contents).hexdigest())

    def test_native_executable_has_a_stable_identity(self):
        path = str(Path(sys.executable).absolute())
        declaration = {'declared': path, 'selected': path, 'lookup': 'path'}
        first = t.snapshot(declaration)
        self.assertEqual(first, t.snapshot(declaration))
        self.assertEqual(first['sha256'], hashlib.sha256(Path(path).read_bytes()).hexdigest())

    def test_windows_cross_api_metadata(self):
        fields = dict(st_dev=11, st_ino=42, st_mode=stat.S_IFREG | 0o666,
                      st_size=123, st_mtime_ns=100, st_ctime_ns=200)
        descriptor = SimpleNamespace(**fields)
        path_fields = dict(fields, st_mode=stat.S_IFREG | 0o777, st_ctime_ns=50)
        with patch.object(t.os, 'name', 'nt'):
            self.assertTrue(t.same_open_file(descriptor, SimpleNamespace(**path_fields)))
            for key, value in [('st_dev', 12), ('st_ino', 43), ('st_size', 124),
                               ('st_mtime_ns', 101), ('st_mode', stat.S_IFDIR | 0o777)]:
                with self.subTest(field=key):
                    changed = dict(path_fields, **{key: value})
                    self.assertFalse(t.same_open_file(descriptor, SimpleNamespace(**changed)))
        with patch.object(t.os, 'name', 'posix'):
            self.assertFalse(t.same_open_file(descriptor, SimpleNamespace(**path_fields)))

    def mutation_is_rejected(self, mutate):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'tool.cmd'
            path.write_bytes(b'original bytes\n')
            path.chmod(0o700)
            declaration = {'declared': str(path), 'selected': str(path), 'lookup': 'path'}
            original = t.hashlib.file_digest
            def digest(stream, algorithm):
                result = original(stream, algorithm)
                mutate(path)
                return result
            with patch.object(t.hashlib, 'file_digest', side_effect=digest):
                with self.assertRaisesRegex(t.IdentityError, 'changed during identity read'):
                    t.snapshot(declaration)

    def test_write_during_read_is_rejected(self):
        self.mutation_is_rejected(lambda path: path.write_bytes(b'changed and longer bytes\n'))

    @unittest.skipIf(os.name == 'nt', 'Windows does not permit removing an open file')
    def test_unix_changed_descriptor_rejects_before_stat_of_removed_path(self):
        def change_and_remove(path):
            path.write_bytes(b'changed and longer bytes\n')
            path.unlink()
        self.mutation_is_rejected(change_and_remove)

    @unittest.skipIf(os.name == 'nt', 'Windows does not permit replacing an open file')
    def test_path_replacement_during_read_is_rejected(self):
        def replace(path):
            replacement = path.with_suffix('.replacement')
            replacement.write_bytes(path.read_bytes())
            replacement.chmod(0o700)
            replacement.replace(path)
        self.mutation_is_rejected(replace)

    def test_path_timestamp_change_is_rejected(self):
        def change(path):
            before = path.stat()
            os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns + 2_000_000_000))
        self.mutation_is_rejected(change)


if __name__ == '__main__':
    unittest.main()
