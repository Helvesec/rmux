#!/usr/bin/env python3
"""Native idle-popup status regression; observes PTY cells with pyte 0.8.2.

Run once with tmux 3.7b, then pass its JSON as --oracle to the candidate.
Only the private server, attach, and popup spawned here are controlled.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time


def rows(cols, lines):
    return [(f'idle-{row:02}|' + 'X' * cols)[:cols] for row in range(lines)]


def producer(root):
    previous = None
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline and not (root / 'exit').exists():
        size = os.get_terminal_size(1)
        if size != previous:
            frame = '\x1b[0m\x1b[2J' + ''.join(
                f'\x1b[{row + 1};1H{text}'
                for row, text in enumerate(rows(*size)))
            os.write(1, (frame + '\x1b[8;12H\x1b[?25h').encode())
            previous = size
        time.sleep(.02)


class Run:
    def __init__(self, binary, root):
        import pyte
        self.binary, self.root = str(Path(binary).resolve()), root.resolve()
        self.socket = Path.home() / 'Projects' / f'.popup-status-{os.getpid()}.sock'
        assert not self.socket.exists(), 'private socket already exists'
        self.env = dict(os.environ, TERM='xterm-256color', LC_ALL='C', SHELL='/bin/sh')
        self.env.pop('TMUX', None)
        self.env.pop('RMUX', None)
        self.master = self.client = None
        self.popups = []
        self.raw = bytearray()
        class CaptureScreen(pyte.Screen):
            # A private DSR query draws no cells; preserve its raw bytes.
            def report_device_status(self, mode, **kwargs):
                if not kwargs.get('private'):
                    super().report_device_status(mode)
        self.screen = CaptureScreen(225, 54)
        self.stream = pyte.ByteStream(self.screen)
        self.record = {'binary': self.binary, 'surfaces': {}, 'idle_samples': 0}

    def command(self, *args, check=True):
        return subprocess.run([self.binary, '-S', str(self.socket), *args],
                              env=self.env, capture_output=True, timeout=5, check=check)

    def pump(self, seconds=.05):
        deadline = time.monotonic() + seconds
        quiet_at = time.monotonic()
        # Sample a drained terminal frame, never a partial PTY write. Resize
        # and tmux's status repaint can span several reads.
        while time.monotonic() < max(deadline, quiet_at + .03):
            assert time.monotonic() < deadline + 3, 'output never quiesced'
            if not select.select([self.master], [], [], .01)[0]:
                continue
            data = os.read(self.master, 65536)
            assert data, 'attach EOF'
            quiet_at = time.monotonic()
            self.raw.extend(data)
            assert len(self.raw) < 4 * 1024 * 1024, 'capture bound'
            self.stream.feed(data)
        assert self.client.poll() is None, 'attach exited'

    def until(self, predicate, label):
        deadline = time.monotonic() + 6
        while not predicate():
            assert time.monotonic() < deadline, label
            self.pump()

    def snapshot(self):
        # Include style and cursor, not only text. pyte chars are named tuples.
        return {'rows': self.screen.display,
                'cells': [[list(self.screen.buffer[y][x]) for x in range(self.screen.columns)]
                          for y in range(self.screen.lines)],
                'cursor': [self.screen.cursor.x, self.screen.cursor.y, self.screen.cursor.hidden]}

    def expected(self):
        return (self.screen.display == rows(self.screen.columns, self.screen.lines)
                and [self.screen.cursor.x, self.screen.cursor.y, self.screen.cursor.hidden]
                == [11, 7, False])

    def idle(self, key):
        self.until(self.expected, 'popup initial surface absent: ' + key)
        before = self.snapshot()
        self.record['surfaces'][key] = before
        start = time.monotonic()
        while time.monotonic() - start < 3.2:
            self.pump(.1)
            self.record['idle_samples'] += 1
            assert self.snapshot() == before, 'status tick changed popup surface/cursor: ' + key

    def resize(self, cols, lines):
        self.screen.resize(lines=lines, columns=cols)
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack('HHHH', lines, cols, 0, 0))
        os.kill(self.client.pid, signal.SIGWINCH)

    def run(self):
        self.root.mkdir(parents=True, exist_ok=False)
        config = self.root / 'tmux.conf'
        config.write_text('set -g status-interval 1\nset -g status-left STATUS\n'
                          "set -g status-right '%S'\nset -g status-right-length 10\n")
        subprocess.run([self.binary, '-f', str(config), '-S', str(self.socket),
                        'new-session', '-d', '-s', 'idle', '-x', '225', '-y', '54',
                        'printf BASE; sleep 90'], env=self.env, check=True, timeout=8,
                       capture_output=True)
        assert self.command('show', '-gv', 'status-right').stdout.strip() == b'%S'
        assert self.command('show', '-gv', 'status-interval').stdout.strip() == b'1'
        self.master, slave = pty.openpty()
        tty = os.ttyname(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 54, 225, 0, 0))
        self.client = subprocess.Popen([self.binary, '-S', str(self.socket), 'attach', '-t', 'idle'],
                                       env=self.env, stdin=slave, stdout=slave, stderr=slave,
                                       start_new_session=True)
        os.close(slave)
        self.until(lambda: 'STATUS' in self.screen.display[-1], 'status not rendered')
        for position in ('bottom', 'top'):
            self.command('set', '-g', 'status-position', position)
            self.pump(.2)
            status_row = -1 if position == 'bottom' else 0
            previous = self.screen.display[status_row]
            self.until(lambda: self.screen.display[status_row] != previous,
                       'idle status clock did not tick before popup')
            folder = self.root / position
            folder.mkdir()
            command = shlex.join([sys.executable, str(Path(__file__).resolve()),
                                  '--producer', str(folder)])
            log = (folder / 'popup.log').open('wb')
            popup = subprocess.Popen([self.binary, '-S', str(self.socket), 'display-popup',
                                      '-c', tty, '-B', '-w', '100%', '-h', '100%', command],
                                     env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            self.popups.append((popup, log))
            self.idle(position + '-initial')
            self.resize(200, 50)
            self.idle(position + '-resized')
            self.resize(225, 54)
            self.idle(position + '-restored')
            self.command('display-popup', '-C', '-c', tty)
            popup.wait(timeout=5)
            self.record.setdefault('popup_close_exits', []).append(popup.returncode)
            self.until(lambda: 'STATUS' in self.screen.display[status_row],
                       'status not restored after close')
            previous = self.screen.display[status_row]
            self.until(lambda: self.screen.display[status_row] != previous,
                       'status ticks did not resume after close')
        self.record['status'] = 'PASS'

    def cleanup(self):
        if self.socket.exists():
            self.command('kill-server', check=False)
        if self.master is not None:
            os.close(self.master)
            self.master = None
        if self.client:
            self.client.wait(timeout=5)
        for popup, log in self.popups:
            popup.wait(timeout=5)
            log.close()
        if self.root.exists():
            (self.root / 'capture.bin').write_bytes(self.raw)
            (self.root / 'result.json').write_text(json.dumps(self.record))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--producer', type=Path)
    parser.add_argument('--binary')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--oracle', type=Path)
    args = parser.parse_args()
    if args.producer:
        producer(args.producer)
        return
    run = Run(args.binary, args.output)
    try:
        run.run()
        if args.oracle:
            oracle = json.loads(args.oracle.read_text())
            assert oracle['status'] == 'PASS', 'oracle did not pass'
            assert run.record['surfaces'] == oracle['surfaces'], 'tmux surface mismatch'
    except Exception as error:
        run.record.update(status='FAIL', error=str(error), final=run.snapshot())
        raise
    finally:
        run.cleanup()
    print('PASS: six full-height idle surfaces, resize, close, restored status ticks')


if __name__ == '__main__':
    main()
