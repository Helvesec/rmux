"""Deterministic publication/reader interleavings; no product verdict."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import options_bank_linux as adapter


class PublicationAcceptance(unittest.TestCase):
    def setUp(self):
        audit = Path(__file__).resolve().parents[1] / 'target' / 'tmp'
        audit.mkdir(parents=True, exist_ok=True)
        self.folder = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)

    def test_helper_birth_is_invisible_until_complete(self):
        destination = self.root / 'helper-born.json'
        observed = []
        original = json.dump

        def interrupted(value, stream, **kwargs):
            if 'process' in value and 'planned_exit_code' not in value:
                # Exercise the real helper writer, scheduling a reader at the
                # empty file and flushed partial-JSON boundaries, without sleep.
                for fragment in ('', '{'):
                    stream.write(fragment)
                    stream.flush()
                    try:
                        observed.append(adapter.load(destination))
                    except (FileNotFoundError, json.JSONDecodeError) as error:
                        observed.append(type(error).__name__)
                stream.seek(0)
                stream.truncate()
            original(value, stream, **kwargs)

        birth = {'identity': dict(pid=123, start_ticks='456', ppid=122)}
        (self.root / 'helper-stop').write_text('owned')
        with mock.patch.object(adapter, 'describe', return_value=birth), \
                mock.patch.object(adapter.json, 'dump', side_effect=interrupted):
            adapter.helper(self.root, 'owned')
        self.assertEqual(observed, ['FileNotFoundError', 'FileNotFoundError'])
        self.assertEqual(adapter.load(destination)['process'], birth)
        self.assertEqual(adapter.load(self.root / 'helper-ended.json')['token'], 'owned')
        self.assertEqual(sorted(p.name for p in self.root.iterdir()),
                         ['helper-born.json', 'helper-ended.json', 'helper-stop'])

    def test_sibling_publications_hide_partial_json(self):
        for name in ('helper-ended.json', 'input-0.json', 'RESULT.json',
                     'RECOVERY-OWNER.json', 'PER-OPTION.json'):
            with self.subTest(name=name):
                destination = self.root / name
                observed = []

                def interrupted(value, stream, **kwargs):
                    stream.write('{')
                    stream.flush()
                    observed.append(destination.exists())
                    stream.write('"token": "owned"}')

                with mock.patch.object(adapter.json, 'dump', side_effect=interrupted):
                    adapter.save(destination, {'token': 'owned'})
                self.assertEqual(observed, [False])
                self.assertEqual(adapter.load(destination), {'token': 'owned'})

    def test_failed_serialization_is_not_published_and_propagates(self):
        destination = self.root / 'helper-born.json'
        with self.assertRaises(TypeError):
            adapter.save(destination, {'unsupported': object()})
        self.assertFalse(destination.exists())
        self.assertEqual(list(self.root.iterdir()), [])

    def test_existing_publication_is_never_overwritten(self):
        destination = self.root / 'helper-born.json'
        destination.write_bytes(b'original receipt\n')
        with self.assertRaises(FileExistsError):
            adapter.save(destination, {'token': 'replacement'})
        self.assertEqual(destination.read_bytes(), b'original receipt\n')
        self.assertEqual(list(self.root.iterdir()), [destination])

    def test_missing_and_invalid_files_still_raise(self):
        path = self.root / 'helper-born.json'
        with self.assertRaises(FileNotFoundError):
            adapter.load(path)
        for raw in ('', '{', '{"token":'):
            path.write_text(raw)
            with self.subTest(raw=raw), self.assertRaises(json.JSONDecodeError):
                adapter.load(path)

    def test_absent_birth_wait_does_not_retain_a_pid(self):
        run = adapter.Run.__new__(adapter.Run)
        run.root = self.root
        run.contract = {'python': {'path': '/fixture/python'},
                        'default_recipe': {'token': 'owned'}}
        run.command, run.hold = mock.Mock(), mock.Mock()

        def wait(predicate):
            self.assertFalse(predicate())
            raise TimeoutError('still absent')

        run.wait = wait
        with self.assertRaisesRegex(TimeoutError, 'still absent'):
            run.start_pane()
        run.hold.assert_not_called()

    def test_reused_birth_cannot_be_retained_or_signalled(self):
        birth = dict(pid=123, start_ticks='456', ppid=122)
        changed = dict(birth, start_ticks='789')
        run = adapter.Run.__new__(adapter.Run)
        run.handles, run.record = {}, {'processes': {}}
        watch = mock.Mock(poll=mock.Mock(return_value=[]))
        with mock.patch.object(adapter, 'identity', side_effect=[birth, changed]), \
                mock.patch.object(adapter, 'open_watch', return_value=(9, watch)), \
                mock.patch.object(adapter, 'describe') as describe:
            with self.assertRaisesRegex(RuntimeError, 'retained birth changed/exited'):
                run.hold('helper', birth['pid'])
            describe.assert_not_called()
        with mock.patch.object(adapter, 'identity', return_value=changed), \
                mock.patch.object(adapter, 'send_signal') as send:
            with self.assertRaisesRegex(RuntimeError, 'stale signal target'):
                run.signal('helper', 15)
            send.assert_not_called()


if __name__ == '__main__':
    unittest.main()
