"""Native FIFO cancellation regressions; evidence stays in target-audit.

Run after cargo build --locked --bins. RMUX_TEST_BINARY selects a pre-fix binary.
The legacy stdin and terminal file routes isolate the snapshot reader from the
separate SourceFileInputV1 cancellation defect.
"""
import errno
import os
import pty
import subprocess
import time
import unittest

import test_snapshot_file_reads as fixture


class SnapshotFifoCancellation(fixture.SnapshotFileReads):
    def test_fifo_restores_valid_snapshots(self):
        for suffix in ('', '-v2'):
            with self.subTest(version=suffix or 'v1'):
                payload = self.success(self.run_cli(
                    'export-window-snapshot' + suffix, '-t', 'probe:0')).encode()
                fifo = self.directory / ('valid' + suffix + '.fifo')
                os.mkfifo(fifo, 0o600)
                pending = subprocess.Popen(
                    [str(fixture.BINARY), '-S', str(self.socket), 'source-file', '-'],
                    env=self.env, stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                pending.stdin.write(
                    f'restore-window-snapshot{suffix} -t probe:0 {fifo}\n'.encode())
                pending.stdin.close()
                pending.stdin = None
                try:
                    writer = self.open_writer(fifo)
                    # Restore fixtures are small enough for one pipe write.
                    try:
                        self.assertEqual(os.write(writer, payload), len(payload))
                    finally:
                        os.close(writer)
                    out, err = pending.communicate(timeout=5)
                    self.assertEqual((pending.returncode, out, err), (0, b'', b''))
                finally:
                    if pending.poll() is None:
                        pending.terminate()
                        pending.communicate(timeout=5)

    def open_writer(self, fifo):
        deadline = time.monotonic() + 3
        while True:
            try:
                return os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
            except OSError as error:
                if error.errno != errno.ENXIO or time.monotonic() >= deadline:
                    raise
                time.sleep(.02)

    def assert_reader_cancelled(self, fifo, writer):
        if writer is None:
            # Do this only once: opening and closing a writer would itself
            # release a broken blocking open, masking the cancellation bug.
            time.sleep(.3)
            try:
                probe = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
            except OSError as error:
                if error.errno == errno.ENXIO:
                    return
                raise
            else:
                os.close(probe)
                self.fail('snapshot FIFO open survived client disconnection')
        deadline = time.monotonic() + 3
        while True:
            try:
                os.write(writer, b' ')
            except BrokenPipeError:
                return
            if time.monotonic() >= deadline:
                self.fail('snapshot FIFO reader survived client disconnection')
            time.sleep(.02)

    def check_cancellation(self, suffix, route, phase, cancel):
        fifo = self.directory / 'snapshot.fifo'
        os.mkfifo(fifo, 0o600)
        code = ('set-environment -g RESTORE_ENTERED yes\n'
                f'restore-window-snapshot{suffix} -t probe:0 {fifo}\n'
                'set-environment -g RESTORE_FINISHED yes\n')
        terminal = []
        writer = None
        pending = None
        try:
            argv = [str(fixture.BINARY), '-S', str(self.socket), 'source-file']
            if route == 'stdin':
                pending = subprocess.Popen(
                    argv + ['-'], env=self.env, cwd=self.directory,
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                pending.stdin.write(code.encode())
                pending.stdin.close()
                pending.stdin = None
            else:
                terminal = list(pty.openpty())
                pending = subprocess.Popen(
                    argv + [self.config(code)], env=self.env, cwd=self.directory,
                    stdin=terminal[1], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            deadline = time.monotonic() + 5
            while self.run_cli('show-environment', '-g', 'RESTORE_ENTERED')[0] != 0:
                self.assertIsNone(pending.poll(), 'source exited before restore')
                self.assertLess(time.monotonic(), deadline, 'source did not start')
                time.sleep(.02)
            if phase == 'read':
                writer = self.open_writer(fifo)
                os.write(writer, b'{')
            # Leave both the pre-writer open and the incomplete read pending.
            time.sleep(.2)
            self.assertIsNone(pending.poll(), 'restore must wait for FIFO data/EOF')
            self.assertEqual(self.run_cli('list-sessions', '-F', '#{session_name}'),
                             (0, 'probe\n', ''))
            if cancel == 'disconnect':
                pending.terminate()  # The exact client created above.
                pending.communicate(timeout=5)
                self.assert_reader_cancelled(fifo, writer)
                self.assertEqual(self.run_cli('show-environment', '-g', 'RESTORE_FINISHED')[0], 1)
            self.success(self.run_cli('kill-server'))
            try:
                self.daemon.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.fail('daemon survived successful kill-server with snapshot FIFO pending')
            self.assertEqual(self.daemon.returncode, 0)
            pending.communicate(timeout=5)
        finally:
            # Release a broken pre-fix reader only after the failing assertion;
            # preserve every FIFO and log, and reap only children owned here.
            if self.daemon.poll() is None:
                if writer is None:
                    try:
                        writer = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                    except OSError as error:
                        if error.errno != errno.ENXIO:
                            raise
                if writer is not None:
                    try:
                        os.write(writer, b'{}')
                    except BrokenPipeError:
                        pass
            if writer is not None:
                os.close(writer)
            if pending is not None:
                if pending.poll() is None:
                    pending.terminate()
                pending.communicate(timeout=5)
            for descriptor in terminal:
                os.close(descriptor)


def cancellation_case(suffix, route, phase, cancel):
    def test(self):
        self.check_cancellation(suffix, route, phase, cancel)
    return test


for version, suffix in (('v1', ''), ('v2', '-v2')):
    for route in ('stdin', 'terminal'):
        for phase in ('open', 'read'):
            for cancel in ('disconnect', 'shutdown'):
                name = f'test_{version}_{route}_{phase}_{cancel}'
                setattr(SnapshotFifoCancellation, name,
                        cancellation_case(suffix, route, phase, cancel))


if __name__ == '__main__':
    unittest.main(verbosity=2)
