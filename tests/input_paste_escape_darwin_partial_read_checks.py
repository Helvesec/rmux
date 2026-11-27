#!/usr/bin/env python3
"""Deterministic calibration observer checks; no native PTY or product proof."""
from contextlib import ExitStack
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import input_paste_escape_darwin as m


PAYLOAD = b'raw\n\xc3\xa9\t\x1b'
AUDIT = Path(__file__).resolve().parents[1] / 'target' / 'tmp' / 'paste-partial-checks'


class CalibrationPartialReadChecks(unittest.TestCase):
    def observe(self, initial, deliveries, expected_error=None):
        """Schedule cumulative sink snapshots and virtual notification times."""
        AUDIT.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=AUDIT) as directory:
            root = Path(directory) / 'calibration'
            clock = [0.0]
            pending = list(deliveries)
            waits = []
            queue = MagicMock()
            process = MagicMock(pid=123, wait=MagicMock(return_value=0))
            watcher = MagicMock()
            watcher.poll_exit.return_value = True
            ready = MagicMock(path=root / 'ready', challenge='synthetic')
            readiness = MagicMock()
            readiness.__enter__.return_value = ready

            def launch(*args, **kwargs):
                m.save(root / 'client-start.json', {'identity': {'pid': 124}})
                m.save(root / 'application-start.json', {'identity': {'pid': 124}})
                m.save(root / 'client-end.json', {'code': 0})
                return process

            def inject(fd, data):
                self.assertEqual((fd, data), (10, PAYLOAD))
                (root / 'application-input.bin').write_bytes(initial)
                return len(data)

            def control(changes, maximum, timeout):
                if changes is not None:
                    return []
                waits.append(timeout)
                stop = clock[0] + timeout
                if pending and pending[0][0] <= stop:
                    clock[0], snapshot = pending.pop(0)
                    if snapshot is None:
                        # The producer has ended with no more input bytes.
                        m.save(root / 'application-end.json', {'at': 'synthetic EOF'})
                    else:
                        (root / 'application-input.bin').write_bytes(snapshot)
                    return []
                clock[0] = stop
                return []

            queue.control.side_effect = control
            with ExitStack() as stack:
                replacements = {
                    'identity': lambda pid: {'pid': pid},
                    'raw_tty': lambda *args: {'raw': True},
                    'drain': lambda *args: None,
                    'drained_exit': lambda *args: None,
                    'write_fifo': lambda *args: None,
                    'native.Readiness': lambda: readiness,
                    'native.FixtureProcess': lambda pid: watcher,
                    'select.kqueue': lambda: queue,
                    'select.kevent': MagicMock(),
                    'subprocess.Popen': launch,
                    'termios.tcgetattr': lambda fd: ['synthetic unchanged attributes'],
                    'time.monotonic': lambda: clock[0],
                    'os.mkfifo': lambda *args: None,
                    'os.open': lambda *args: 12,
                    'os.close': lambda *args: None,
                    'os.openpty': lambda: (10, 11),
                    'os.set_blocking': lambda *args: None,
                    'os.ttyname': lambda fd: '/synthetic/tty',
                    'os.read': lambda *args: b'd',
                    'os.write': inject,
                }
                for name, value in replacements.items():
                    stack.enter_context(patch('input_paste_escape_darwin.' + name, value))
                kill = stack.enter_context(patch.object(m.os, 'kill'))
                if expected_error is None:
                    result = m.calibration(root)
                    self.assertTrue(result['pass'])
                    self.assertEqual(result['received_hex'], PAYLOAD.hex())
                    kill.assert_called_once_with(124, m.signal.SIGHUP)
                else:
                    with self.assertRaises(expected_error):
                        m.calibration(root)
                    kill.assert_not_called()
                    result = json.loads((root / 'calibration.json').read_text())
                    self.assertFalse(result.get('pass', False))
                ready.wait.assert_called_once_with(5)
                self.assertTrue(all(0 < timeout <= 5 for timeout in waits))
                self.assertLessEqual(clock[0], 5)
                process.wait.assert_called()
                queue.close.assert_called_once()
            return clock[0], waits

    def test_calibration_requires_complete_exact_delivery_within_five_seconds(self):
        cases = [
            ('complete', PAYLOAD, [], None, 0),
            ('4+4', PAYLOAD[:4], [(1, PAYLOAD)], None, 1),
            ('split UTF-8', PAYLOAD[:5], [(1, PAYLOAD)], None, 1),
            ('delayed fragments', b'', [(1, PAYLOAD[:4]), (4.999, PAYLOAD)], None, 4.999),
            ('empty deadline', b'', [], TimeoutError, 5),
            ('truncated deadline', PAYLOAD[:4], [], TimeoutError, 5),
            ('EOF after prefix', PAYLOAD[:4], [(1, None)], TimeoutError, 5),
            ('empty EOF', b'', [(1, None)], TimeoutError, 5),
            ('wrong complete bytes', b'X' + PAYLOAD[1:], [], AssertionError, 0),
            ('wrong later bytes', PAYLOAD[:4], [(1, PAYLOAD[:-1] + b'X')], AssertionError, 1),
            ('excess bytes', PAYLOAD + b'X', [], AssertionError, 0),
            ('delayed excess', PAYLOAD[:4], [(1, PAYLOAD + b'X')], AssertionError, 1),
        ]
        for label, initial, deliveries, error, elapsed in cases:
            with self.subTest(label=label):
                observed, waits = self.observe(initial, deliveries, error)
                self.assertEqual(observed, elapsed)
                if elapsed > 1:
                    self.assertEqual(waits[0], 5)
                    if len(waits) > 1:
                        self.assertEqual(waits[1], 4)


if __name__ == '__main__':
    unittest.main()
