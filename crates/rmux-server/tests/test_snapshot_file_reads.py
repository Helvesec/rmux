"""Native snapshot file regressions; run after cargo build --locked --bins.

RMUX_TEST_BINARY can select a pre-fix binary. Evidence stays in target-audit.
"""
import errno
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[3]
BINARY = Path(os.environ.get('RMUX_TEST_BINARY', REPO / 'target/debug/rmux'))
AUDIT = REPO / 'target-audit/snapshot'


class SnapshotFileReads(unittest.TestCase):
    def setUp(self):
        AUDIT.mkdir(parents=True, exist_ok=True)
        self.directory = Path(tempfile.mkdtemp(prefix='', dir=AUDIT))
        self.socket = self.directory / 's'
        self.env = os.environ.copy()
        for key in ('RMUX', 'TMUX', 'RMUX_PANE', 'TMUX_PANE', 'RMUX_INTERNAL_BINARY_PATH'):
            self.env.pop(key, None)
        self.env.update(XDG_RUNTIME_DIR=str(self.directory),
                        RMUX_DISABLE_TMUX_FALLBACK='1', SHELL='/bin/sh')
        self.log = (self.directory / 'daemon.log').open('wb')
        self.daemon = subprocess.Popen(
            [str(BINARY), '--__internal-daemon', str(self.socket)], env=self.env,
            stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log)
        self.addCleanup(self.close)
        deadline = time.monotonic() + 10
        while not self.socket.exists():
            self.assertIsNone(self.daemon.poll(), 'daemon exited')
            self.assertLess(time.monotonic(), deadline, 'socket not ready')
            time.sleep(.02)
        self.success(self.run_cli('new-session', '-d', '-s', 'probe', 'sleep 300'))

    def close(self):
        if self.daemon.poll() is None:
            self.daemon.terminate()
            try:
                self.daemon.wait(5)
            except subprocess.TimeoutExpired:
                self.daemon.kill()  # Only the exact child launched by this test.
                self.daemon.wait(5)
        self.log.close()

    def spawn(self, *args):
        return subprocess.Popen([str(BINARY), '-S', str(self.socket), *args],
                                env=self.env, cwd=self.directory, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def run_cli(self, *args):
        child = self.spawn(*args)
        try:
            out, err = child.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate()
            raise
        return child.returncode, out.decode(), err.decode()

    def success(self, result):
        self.assertEqual(result[0], 0, result)
        return result[1]

    def config(self, text):
        path = self.directory / 'restore.conf'
        path.write_text(text)
        return str(path)

    def test_fifo_without_producer_keeps_other_connection_responsive(self):
        for suffix in ('', '-v2'):
            command = 'restore-window-snapshot' + suffix
            for route in ('validate', 'source', 'queue'):
                with self.subTest(command=command, route=route):
                    fifo = self.directory / (route + suffix + '.fifo')
                    os.mkfifo(fifo, 0o600)
                    text = f'{command} -t probe:0 {fifo}'
                    config = self.config(text + '\n')
                    argv = {'validate': ('source-file', '-n', config),
                            'source': ('source-file', config),
                            'queue': ('display-message', '-p', 'ready', ';', command,
                                      '-t', 'probe:0', str(fifo))}[route]
                    pending = self.spawn(*argv)
                    time.sleep(.3)
                    completed_without_producer = pending.poll() is not None
                    try:
                        independent = self.run_cli('list-sessions', '-F', '#{session_name}')
                    except subprocess.TimeoutExpired:
                        independent = (-1, '', 'independent connection timed out')
                    # Release even a broken implementation before any assertion.
                    reader = False
                    deadline = time.monotonic() + 3
                    while pending.poll() is None:
                        try:
                            writer = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                        except OSError as error:
                            if error.errno != errno.ENXIO or time.monotonic() >= deadline:
                                pending.kill()
                                break
                            time.sleep(.02)
                        else:
                            os.write(writer, b'{}')
                            os.close(writer)
                            reader = True
                            break
                    out, err = pending.communicate(timeout=5)
                    self.assertEqual(independent, (0, 'probe\n', ''))
                    if route == 'validate':
                        self.assertTrue(completed_without_producer)
                        self.assertFalse(reader, 'validation opened the FIFO')
                        self.assertEqual((pending.returncode, out, err), (0, b'', b''))
                    else:
                        self.assertFalse(completed_without_producer)
                        self.assertTrue(reader, 'execution must actually read the FIFO')
                        self.assertEqual(pending.returncode, 1)
                        self.assertIn(b'invalid RMUX window snapshot', out + err)

    def test_file_created_earlier_in_same_batch(self):
        for suffix in ('', '-v2'):
            original = self.directory / ('original' + suffix + '.json')
            original.write_text(self.success(self.run_cli(
                'export-window-snapshot' + suffix, '-t', 'probe:0')))
            for route in ('source', 'queue'):
                with self.subTest(suffix=suffix, route=route):
                    generated = self.directory / (route + suffix + '.json')
                    copy = f'cp {shlex.quote(str(original))} {shlex.quote(str(generated))}'
                    restore = f'restore-window-snapshot{suffix} -t probe:0 {generated.name}'
                    if route == 'source':
                        result = self.run_cli('source-file', self.config(
                            f'run-shell {shlex.quote(copy)}\n{restore}\n'))
                    else:
                        result = self.run_cli('run-shell', copy, ';',
                                              'restore-window-snapshot' + suffix,
                                              '-t', 'probe:0', generated.name)
                    self.success(result)
                    self.assertEqual(json.loads(generated.read_text()),
                                     json.loads(original.read_text()))

    def test_read_and_decode_errors_are_execution_errors(self):
        bad_utf8 = self.directory / 'bad-utf8'
        bad_utf8.write_bytes(b'\xff')
        bad_json = self.directory / 'bad-json'
        bad_json.write_text('{}')
        for suffix in ('', '-v2'):
            command = 'restore-window-snapshot' + suffix
            for path, diagnostic in ((self.directory / 'missing', 'failed to read'),
                                     (self.directory, 'failed to read'),
                                     (bad_utf8, 'failed to read'),
                                     (bad_json, 'invalid RMUX window snapshot')):
                with self.subTest(command=command, path=path):
                    config = self.config(f'{command} -t probe:0 {path}\n')
                    self.success(self.run_cli('source-file', '-n', config))
                    for argv in (('source-file', config),
                                 ('display-message', '-p', 'ready', ';', command,
                                  '-t', 'probe:0', str(path))):
                        result = self.run_cli(*argv)
                        self.assertEqual(result[0], 1, result)
                        self.assertIn(command + ': ' + diagnostic, result[1] + result[2])
                    self.success(self.run_cli('list-sessions'))
            for operand in ('', '-'):
                config = self.config(f'{command} -t probe:0 {operand}\n')
                self.assertEqual(self.run_cli('source-file', '-n', config)[0], 1)


if __name__ == '__main__':
    unittest.main(verbosity=2)
