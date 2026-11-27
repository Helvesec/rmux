"""A coherent live pane_pid response must never grant cleanup authority."""
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import support


class ProducerOwnershipRegression(unittest.TestCase):
    def test_foreign_live_pane_pid_is_neither_registered_nor_signalled(self):
        # This test process is live and unrelated to the Case creation stub.
        foreign = support.identity(os.getpid())
        self.assertIsNotNone(foreign)
        audit = Path(__file__).resolve().parents[2] / 'target' / 'tmp' / 'fjobs'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as root:
            with patch.dict(os.environ, FORMAT_JOBS_SOCKET_ROOT=root):
                case = support.Case('oracle', '/bin/sleep', 'foreign', root, 3)
            case.handshake = lambda: None
            case.q = lambda *args, **kwargs: str(foreign['pid']) if args[0] == 'display-message' else ''
            with patch.object(support.os, 'kill') as signal_spy:
                try:
                    case.new('s')
                except (support.Failure, support.Skip):
                    pass
                finally:
                    case.close()
            self.assertNotIn(foreign, case.producers,
                             'live identity alone is not producer ownership')
            self.assertFalse(any(call.args[0] == foreign['pid']
                                 for call in signal_spy.call_args_list),
                             'cleanup must never signal the foreign PID')
        self.assertEqual(support.identity(os.getpid()), foreign)


if __name__ == '__main__':
    unittest.main()
