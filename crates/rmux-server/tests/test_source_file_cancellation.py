"""Native source-file lifetime regressions; run after cargo build --locked --bins.

RMUX_TEST_BINARY selects an alternate binary. Fixtures remain in target-audit.
"""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[3]
BINARY = Path(os.environ.get('RMUX_TEST_BINARY', REPO / 'target/debug/rmux'))
AUDIT = REPO / 'target-audit/source-cancellation'


@unittest.skipUnless(os.name == 'posix', 'requires Unix process and socket behavior')
class SourceFileCancellation(unittest.TestCase):
    def setUp(self):
        AUDIT.mkdir(parents=True, exist_ok=True)
        self.directory = Path(tempfile.mkdtemp(prefix='', dir=AUDIT))
        self.socket = self.directory / 's'
        self.release = self.directory / 'release'
        self.children = []
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith('RMUX') and k not in ('TMUX', 'TMUX_PANE')}
        self.env.update(SHELL='/bin/sh', RMUX_DISABLE_TMUX_FALLBACK='1',
                        XDG_RUNTIME_DIR=str(self.directory))
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
        self.success('new-session', '-d', '-s', 'probe', 'sleep 300')

    def close(self):
        # Release failed fixtures before stopping only children owned by this test.
        self.release.write_text('release\n')
        for child in [*self.children, self.daemon]:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(5)
            if child in self.children:
                child.communicate(timeout=5)
        self.log.close()

    def spawn(self, *args, stdin=subprocess.DEVNULL):
        child = subprocess.Popen([str(BINARY), '-S', str(self.socket), *args],
                                 env=self.env, cwd=self.directory, stdin=stdin,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(child)
        return child

    def run_cli(self, *args):
        child = self.spawn(*args)
        out, err = child.communicate(timeout=5)
        return child.returncode, out, err

    def success(self, *args):
        result = self.run_cli(*args)
        self.assertEqual(result[0], 0, result)
        return result[1]

    def wait_for_marker(self):
        deadline = time.monotonic() + 5
        while self.run_cli('show-environment', '-g', 'ENTERED')[0] != 0:
            self.assertLess(time.monotonic(), deadline, 'source never entered')
            time.sleep(.02)

    def source(self, text, route):
        config = self.directory / 'source.conf'
        config.write_text(text)
        if route == 'stdin':
            child = self.spawn('source-file', '-', stdin=subprocess.PIPE)
            child.stdin.write(text.encode())
            child.stdin.close()
            child.stdin = None
        else:
            child = self.spawn('source-file', str(config),
                               stdin=subprocess.PIPE if route == 'pull' else subprocess.DEVNULL)
            if route == 'pull':
                # A foreground consumer reads EOF, then the source reaches its gate.
                child.stdin.close()
                child.stdin = None
        self.wait_for_marker()
        self.assertIsNone(child.poll(), 'source must still be pending')
        return child

    def check_disconnect(self, route):
        text = ('new-pane -dI\n' if route == 'pull' else '')
        text += ('set-environment -g ENTERED yes\nwait-for gate\n'
                 'set-environment -g AFTER_DISCONNECT yes\n')
        child = self.source(text, route)
        child.terminate()
        child.communicate(timeout=5)
        # Give the transport time to observe EOF before making the wait runnable.
        time.sleep(.3)
        self.success('wait-for', '-S', 'gate')
        time.sleep(.3)
        result = self.run_cli('show-environment', '-g', 'AFTER_DISCONNECT')
        self.assertEqual(result[0], 1, result)
        self.assertIn(b'unknown variable', result[2])

    def test_file_disconnect_cancels_without_reading_stdin(self):
        self.check_disconnect('file')

    def test_file_disconnect_cancels_after_reading_stdin(self):
        self.check_disconnect('pull')

    def test_code_stdin_disconnect_still_cancels(self):
        self.check_disconnect('stdin')

    def test_file_shutdown_cancels_foreground_shell(self):
        entered = self.directory / 'shell-entered'
        shell = (f'touch {shlex.quote(str(entered))}; '
                 f'while [ ! -f {shlex.quote(str(self.release))} ]; do sleep 0.05; done')
        source = self.source('set-environment -g ENTERED yes\nrun-shell '
                             + shlex.quote(shell) + '\n', 'file')
        deadline = time.monotonic() + 5
        while not entered.exists():
            self.assertLess(time.monotonic(), deadline, 'foreground shell never entered')
            time.sleep(.02)
        self.success('list-sessions')
        killer = self.spawn('kill-server')
        try:
            out, err = killer.communicate(timeout=3)
        except subprocess.TimeoutExpired:
            self.fail('kill-server blocked on the foreground source-file')
        self.assertEqual((killer.returncode, out, err), (0, b'', b''))
        self.assertEqual(self.daemon.wait(timeout=3), 0)
        source.communicate(timeout=3)
        self.assertFalse(self.release.exists(), 'shutdown must not need shell release')


if __name__ == '__main__':
    unittest.main(verbosity=2)
