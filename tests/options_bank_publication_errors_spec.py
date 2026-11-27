"""Publication failure and competing-writer boundaries for the Unix bank."""
import errno
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import options_bank_linux as adapter


class PublicationErrors(unittest.TestCase):
    def setUp(self):
        audit = Path(__file__).resolve().parents[1] / 'target' / 'tmp'
        audit.mkdir(parents=True, exist_ok=True)
        self.folder = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.destination = self.root / 'helper-born.json'

    def test_publication_error_propagates_without_a_receipt_or_temporary(self):
        for code in (errno.EACCES, errno.ENOSPC, errno.EIO):
            error = OSError(code, 'injected publication failure')
            with self.subTest(code=code), \
                    mock.patch.object(adapter.os, 'link', side_effect=error), \
                    self.assertRaises(OSError) as raised:
                adapter.save(self.destination, {'token': 'owned'})
            self.assertIs(raised.exception, error)
            self.assertEqual(list(self.root.iterdir()), [])

    def test_complete_closed_bytes_exist_before_publication(self):
        link = adapter.os.link
        value = {'token': 'owned', 'process': {'pid': 123, 'start_ticks': '456'}}
        expected = (json.dumps(value, indent=2, sort_keys=True) + '\n').encode()

        def publish(source, destination):
            self.assertEqual(Path(source).parent, self.root)
            self.assertFalse(self.destination.exists())
            self.assertEqual(Path(source).read_bytes(), expected)
            link(source, destination)

        with mock.patch.object(adapter.os, 'link', side_effect=publish):
            adapter.save(self.destination, value)
        self.assertEqual(self.destination.read_bytes(), expected)
        self.assertEqual(list(self.root.iterdir()), [self.destination])

    def test_competing_publication_wins_without_being_overwritten(self):
        link = adapter.os.link

        def publish(source, destination):
            self.destination.write_text('{"winner": true}\n')
            link(source, destination)

        with mock.patch.object(adapter.os, 'link', side_effect=publish), \
                self.assertRaises(FileExistsError):
            adapter.save(self.destination, {'winner': False})
        self.assertEqual(adapter.load(self.destination), {'winner': True})
        self.assertEqual(list(self.root.iterdir()), [self.destination])

    def test_dangling_symlink_is_not_followed_or_replaced(self):
        target = self.root / 'foreign'
        self.destination.symlink_to(target)
        with self.assertRaises(FileExistsError):
            adapter.save(self.destination, {'token': 'owned'})
        self.assertTrue(self.destination.is_symlink())
        self.assertFalse(target.exists())
        self.assertEqual(list(self.root.iterdir()), [self.destination])


if __name__ == '__main__':
    unittest.main()
