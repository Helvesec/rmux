"""Native first-report regression; run after cargo build --locked --bin rmux.

RMUX_TEST_BINARY selects a reference binary. Evidence stays in target-audit.
"""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[3]
BINARY = Path(os.environ.get('RMUX_TEST_BINARY', REPO / 'target/debug/rmux'))


class ProgramStatusFirstReport(unittest.TestCase):
    def setUp(self):
        audit = REPO / 'target-audit/first-report'
        audit.mkdir(parents=True, exist_ok=True)
        self.directory = Path(tempfile.mkdtemp(prefix='', dir=audit))
        self.socket = self.directory / 's'
        self.trace = []
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith('RMUX') and key not in ('TMUX', 'TMUX_PANE')}
        self.env.update(SHELL='/bin/sh', RMUX_DISABLE_TMUX_FALLBACK='1')
        self.log = (self.directory / 'daemon.log').open('wb')
        self.daemon = subprocess.Popen(
            [str(BINARY), '--__internal-daemon', str(self.socket)], env=self.env,
            stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log)
        self.addCleanup(self.close)
        self.wait_for(self.socket.exists, 'daemon socket')

    def close(self):
        if self.daemon.poll() is None:
            self.daemon.terminate()
            try:
                self.daemon.wait(5)
            except subprocess.TimeoutExpired:
                self.daemon.kill()  # Only this test's private daemon.
                self.daemon.wait(5)
        self.log.close()
        (self.directory / 'trace.json').write_text(json.dumps(
            {'binary': str(BINARY), 'daemon_pid': self.daemon.pid,
             'commands': self.trace}, indent=2))

    def cli(self, *args):
        result = subprocess.run(
            [str(BINARY), '-S', str(self.socket), '-f', '/dev/null', *args],
            env=self.env, stdin=subprocess.DEVNULL, capture_output=True,
            text=True, timeout=10)
        self.trace.append({'argv': args, 'code': result.returncode,
                           'stdout': result.stdout, 'stderr': result.stderr})
        self.assertEqual(result.returncode, 0, self.trace[-1])
        return result.stdout

    def wait_for(self, predicate, description):
        deadline = time.monotonic() + 8
        while not predicate():
            self.assertIsNone(self.daemon.poll(), 'daemon exited')
            self.assertLess(time.monotonic(), deadline, description)
            time.sleep(.02)

    def send_line(self, text):
        self.cli('send-keys', '-t', 'first:0.0', '-l', text)
        self.cli('send-keys', '-t', 'first:0.0', 'Enter')

    def test_single_report_without_query_survives_foreground_and_format_reads(self):
        # The emitter writes one report, never queries and never reports again.
        emitter = self.directory / 'emitter.py'
        emitter.write_text(
            "import os, sys\n"
            "os.write(1, b'\\x1b]7501;state=blocked:app=first:msg=V0FJVElORw==\\x07'"
            " b'FIRST-WRITTEN\\n')\n"
            "sys.stdin.readline()\n")
        self.cli('new-session', '-d', '-s', 'first', '/bin/sh')
        self.send_line("stty -echo; printf '\\nSHELL-READY\\n'")
        self.wait_for(lambda: '\nSHELL-READY\n' in self.cli(
            'capture-pane', '-p', '-t', 'first:0.0'), 'shell ready')
        # Observe the shell lifetime before an ordinary external-program launch.
        self.cli('display-message', '-p', '-t', 'first:0.0',
                 '#{pane_program_status_json}')
        self.send_line(f'{shlex.quote(sys.executable)} {shlex.quote(str(emitter))}')
        self.wait_for(lambda: 'FIRST-WRITTEN' in self.cli(
            'capture-pane', '-p', '-t', 'first:0.0'), 'single report consumed')
        for _ in range(3):
            # Reading formats resamples foreground state. It must keep this
            # first report while the new program is alive and waiting on stdin.
            status, dead = self.cli(
                'display-message', '-p', '-t', 'first:0.0',
                '#{pane_program_status_json}|#{pane_dead}').strip().split('|')
            value = json.loads(status)
            self.assertEqual(dead, '0')
            self.assertEqual(len(value['records']), 1, value)
            self.assertEqual(value['summary']['state'], 'blocked', value)
            self.assertEqual(value['summary']['app'], 'first', value)
            self.assertEqual(value['summary']['message'], 'WAITING', value)
            time.sleep(.05)


if __name__ == '__main__':
    unittest.main(verbosity=2)
