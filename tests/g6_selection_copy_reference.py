#!/usr/bin/env python3
"""G6 selection/copy through real client and application PTYs; tmux oracle first.

Each invocation owns a new evidence directory and ten dedicated server sockets.
No debugger, floating pane, split, user config, or existing server is involved.
Run on Linux; only Python's standard library and the two supplied binaries are used.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time
import traceback
import tty

CONTENT = b'zero one two three\r\nalpha beta gamma\r\nomega sigma tau\r\n'
REPORTS = {
    'press': b'\x1b[<0;5;2M', 'drag': b'\x1b[<32;10;3M',
    'release': b'\x1b[<0;10;3m', 'drag_far': b'\x1b[<32;15;3M',
    'release_far': b'\x1b[<0;15;3m', 'click_release': b'\x1b[<0;5;2m',
}
CASES = [
    ('C1-copy', ['press', 'drag', 'release'], b'a beta gamma\nomega sig'),
    ('C2-extend', ['press', 'drag', 'drag_far', 'release_far'], b'a beta gamma\nomega sigma ta'),
    ('C3-application', ['press', 'drag', 'release'], None),
    ('C4-click', ['press', 'click_release'], None),
    ('C5-mouse-off', ['press', 'drag', 'release'], None),
]
PINS = {
    'tmux': '032198096ed82c1473b41c62a2ee83f16d9796a3f6a26f6830637591c24418a7',
    'rmux': '1d04dff46ff8093ea2d7a8566872c0c4a19e6af110c43528c0631fcb73532f7c',
    'rmux-daemon': '79d960968668590b6af668e83b5af037c73ccf18a5aa21ccdd8b47333192c12b',
}


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')


def process_identity(pid):
    try:
        stat = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        if stat[0] == 'Z':
            return None
        return {'pid': pid, 'birth_ticks': stat[19],
                'executable': os.readlink(f'/proc/{pid}/exe')}
    except (FileNotFoundError, ProcessLookupError):
        return None


def same_process(identity):
    current = process_identity(identity['pid'])
    return current is not None and current['birth_ticks'] == identity['birth_ticks']


def terminal_attributes(attributes):
    return [*attributes[:6], [x.hex() if isinstance(x, bytes) else x for x in attributes[6]]]


def application(root, mouse):
    """The actual pane program: emit text, request mouse modes, record raw input."""
    root = Path(root)
    before = termios.tcgetattr(0)
    tty.setraw(0)
    save(root / 'application-start.json', {'at': now(), 'identity': process_identity(os.getpid()),
                                          'tty': os.ttyname(0), 'mouse': mouse})
    signal.signal(signal.SIGHUP, lambda *_: sys.exit(0))
    try:
        stop = time.monotonic() + 30
        while not (root / 'emit').exists():
            if time.monotonic() > stop:
                raise RuntimeError('application emission not requested')
            time.sleep(.01)
        if mouse:
            os.write(1, b'\x1b[?1002h\x1b[?1006h')
        os.write(1, CONTENT + b'\x1b]2;G6-READY\x07')
        (root / 'application-input.bin').write_bytes(b'')
        save(root / 'application-ready.json', {'at': now(), 'content_hex': CONTENT.hex()})
        with (root / 'application-input.bin').open('ab', buffering=0) as log:
            while time.monotonic() < stop:
                if select.select([0], [], [], .05)[0]:
                    data = os.read(0, 65536)
                    if not data:
                        break
                    log.write(data)
    finally:
        try:
            termios.tcsetattr(0, termios.TCSANOW, before)
        except termios.error:
            pass
        save(root / 'application-end.json', {'at': now()})


class Case:
    def __init__(self, binary, kind, root, spec, timeout):
        self.binary = str(Path(binary).resolve())
        self.kind = kind
        self.root = root.resolve()
        self.root.mkdir(parents=True, exist_ok=False)
        self.socket = self.root / 's'
        assert len(os.fsencode(self.socket)) < 100, self.socket
        self.name, self.reports, self.expected = spec
        self.deadline = time.monotonic() + timeout
        self.env = dict(os.environ, TERM='xterm-256color', LC_ALL='C',
                        RMUX_DISABLE_TMUX_FALLBACK='1')
        for key in ('TMUX', 'RMUX', 'RMUX_SOCKET', 'RMUX_SOCKET_PATH'):
            self.env.pop(key, None)
        self.record = {'case': self.name, 'kind': kind, 'start': now(),
                       'binary': self.binary, 'binary_sha256': sha(self.binary),
                       'commands': [], 'checks': [], 'steps': [], 'cleanup': {}}
        self.client = None
        self.master = self.slave = None
        self.identities = []
        self.raw = bytearray()
        self.started = False
        self.cleaning = False

    def remaining(self):
        return max(.1, min(4, self.deadline - time.monotonic()))

    def q(self, *args, check=True):
        argv = [self.binary, '-S', str(self.socket), '-f', '/dev/null', *args]
        r = subprocess.run(argv, env=self.env, capture_output=True,
                           timeout=4 if self.cleaning else self.remaining())
        self.record['commands'].append({'at': now(), 'argv': argv, 'code': r.returncode,
                                        'stdout_hex': r.stdout.hex(),
                                        'stderr': r.stderr.decode(errors='replace')})
        self.pump()
        if check and r.returncode:
            raise AssertionError(self.record['commands'][-1])
        return r

    def text(self, *args, check=True):
        return self.q(*args, check=check).stdout.decode().rstrip('\n')

    def equal(self, actual, expected, label):
        item = {'label': label, 'actual': actual, 'expected': expected, 'pass': actual == expected}
        self.record['checks'].append(item)
        if not item['pass']:
            raise AssertionError(item)

    def pump(self):
        if self.master is None:
            return
        while select.select([self.master], [], [], 0)[0]:
            try:
                data = os.read(self.master, 65536)
            except OSError:
                break
            if not data:
                break
            self.raw.extend(data)
            if len(self.raw) > 2 * 1024 * 1024:
                raise RuntimeError('client evidence output cap exceeded')

    def until(self, predicate, label):
        end = min(self.deadline, time.monotonic() + 4)
        while time.monotonic() < end:
            self.pump()
            if predicate():
                return
            time.sleep(.02)
        raise AssertionError('observation timeout: ' + label)

    def fmt(self, expression):
        return self.text('display-message', '-p', '-t', 'g6:0.0', expression)

    def mode(self):
        return self.fmt('#{pane_mode}|#{pane_in_mode}')

    def start(self):
        command = shlex.join([sys.executable, str(Path(__file__).resolve()), '--application',
                              str(self.root)] + (['--application-mouse'] if self.name == 'C3-application' else []))
        self.started = True
        self.q('new-session', '-d', '-s', 'g6', '-x', '20', '-y', '5', command)
        self.q('set-option', '-g', 'status', 'off')
        self.q('set-option', '-g', 'mouse', 'off' if self.name == 'C5-mouse-off' else 'on')
        identity = self.fmt('#{pid}|#{socket_path}|#{version}').split('|')
        self.equal(str(Path(identity[1]).resolve()), str(self.socket), 'dedicated server socket')
        server = process_identity(int(identity[0]))
        assert server is not None
        self.identities.append(server)
        server_hash = sha(server['executable'])
        permitted = [PINS[self.kind]] + ([PINS['rmux-daemon']] if self.kind == 'rmux' else [])
        self.equal(server_hash in permitted, True, 'server executable is a pinned supplied binary')
        self.record['server'] = {**server, 'sha256': server_hash, 'version': identity[2]}
        self.until(lambda: (self.root / 'application-start.json').exists(), 'pane application started')
        app = json.loads((self.root / 'application-start.json').read_text())
        self.identities.append(app['identity'])
        self.record['application'] = app
        pane_pid = int(self.fmt('#{pane_pid}'))
        pane = process_identity(pane_pid)
        assert pane is not None
        self.identities.append(pane)
        self.record['pane_process'] = pane
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 5, 20, 0, 0))
        self.original_termios = termios.tcgetattr(self.slave)
        self.record['termios_before'] = terminal_attributes(self.original_termios)
        self.client_tty = os.ttyname(self.slave)

        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        argv = [self.binary, '-S', str(self.socket), '-f', '/dev/null', 'attach-session', '-t', 'g6']
        self.client = subprocess.Popen(argv, env=self.env, stdin=self.slave,
                                       stdout=self.slave, stderr=self.slave, preexec_fn=child_setup)
        self.record['client'] = {'argv': argv, 'pid': self.client.pid, 'tty': self.client_tty}
        self.until(lambda: self.client_tty in self.text('list-clients', '-F', '#{client_tty}'), 'client attached')
        self.equal(self.fmt('#{pane_width}|#{pane_height}'), '20|5', 'pane geometry')
        clients = self.text('list-clients', '-F', '#{client_pid}|#{client_tty}|#{client_width}|#{client_height}')
        self.record['client']['list_clients'] = clients
        self.equal(len(clients.splitlines()), 1, 'exactly one client')
        self.equal(clients.split('|')[-2:], ['20', '5'], 'client geometry')
        self.equal(self.client_tty in clients, True, 'owned client tty')
        self.equal(self.client.poll(), None, 'attached client remains live')
        self.record['termios_attached'] = terminal_attributes(termios.tcgetattr(self.slave))
        self.equal(termios.tcgetattr(self.slave) != self.original_termios, True, 'client changed terminal mode')
        (self.root / 'emit').touch()
        self.until(lambda: self.fmt('#{pane_title}') == 'G6-READY', 'real PTY content processed')
        captured = self.text('capture-pane', '-p', '-t', 'g6:0.0')
        self.equal(captured.splitlines()[:3], CONTENT.decode().splitlines(), 'native pane content')
        self.equal(self.mode(), '|0', 'no initial copy mode')
        self.equal(self.q('show-buffer', check=False).returncode != 0, True, 'no initial buffer')

    def run_inputs(self):
        for name in self.reports:
            data = REPORTS[name]
            os.write(self.master, data)
            active = self.name in ('C1-copy', 'C2-extend') and name in ('drag', 'drag_far')
            expected_mode = 'copy-mode|1' if active else '|0'
            # The positive transition is observable. For no-mode paths, a short drain
            # interval precedes querying so that merely observing the old state cannot pass.
            if not active:
                time.sleep(.1)
            self.until(lambda: self.mode() == expected_mode, 'mode after ' + name)
            current = self.mode()
            buffer = self.q('show-buffer', check=False)
            self.record['steps'].append({'at': now(), 'report': name, 'input_hex': data.hex(), 'mode': current,
                                         'buffer_code': buffer.returncode, 'buffer_stdout_hex': buffer.stdout.hex(),
                                         'application_input_hex': (self.root / 'application-input.bin').read_bytes().hex()})
        if self.name == 'C3-application':
            wanted = b''.join(REPORTS[x] for x in self.reports)
            self.until(lambda: (self.root / 'application-input.bin').read_bytes() == wanted,
                       'application received the three exact mouse reports')
        time.sleep(.1)
        result = self.q('show-buffer', check=False)
        if self.expected is None:
            self.equal(result.returncode != 0, True, 'buffer absent')
            buffer_hex = None
        else:
            self.equal(result.returncode, 0, 'show-buffer success')
            self.equal(result.stdout.hex(), self.expected.hex(), 'exact copied buffer bytes')
            buffer_hex = result.stdout.hex()
        received = (self.root / 'application-input.bin').read_bytes()
        if self.name in ('C1-copy', 'C2-extend'):
            self.equal(received.hex(), '', 'copy reports not handed to application')
        elif self.name == 'C3-application':
            self.equal(received.hex(), b''.join(REPORTS[x] for x in self.reports).hex(), 'application reports exact')
        self.record['outcome'] = {'buffer_hex': buffer_hex, 'application_input_hex': received.hex(),
                                  'final_mode': self.mode(),
                                  'step_modes': [x['mode'] for x in self.record['steps']]}

    def finish(self):
        self.cleaning = True
        cleanup = self.record['cleanup']
        try:
            if self.client is not None:
                self.q('detach-client', '-s', 'g6', check=False)
                try:
                    code = self.client.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    self.client.terminate()
                    code = self.client.wait(timeout=2)
                    cleanup['forced_client_termination'] = True
                self.pump()
                cleanup['client_exit_code'] = code
                cleanup['termios_after'] = terminal_attributes(termios.tcgetattr(self.slave))
                cleanup['termios_restored'] = termios.tcgetattr(self.slave) == self.original_termios
            if self.started:
                cleanup['kill_server_code'] = self.q('kill-server', check=False).returncode
            end = time.monotonic() + 4
            while time.monotonic() < end and any(same_process(x) for x in self.identities):
                time.sleep(.03)
            cleanup['owned_processes_gone'] = all(not same_process(x) for x in self.identities)
            cleanup['tracked_processes'] = self.identities
            cleanup['process_observations_after'] = [process_identity(x['pid']) for x in self.identities]
            # A filesystem socket left by a dead server is removed only after every
            # tracked process is gone, and only inside this newly created fixture.
            cleanup['socket_left_by_server'] = self.socket.exists()
            if cleanup['owned_processes_gone'] and self.socket.exists():
                self.socket.unlink()
            cleanup['socket_absent_after_owned_cleanup'] = not self.socket.exists()
            cleanup['binary_unchanged'] = sha(self.binary) == self.record['binary_sha256']
            cleanup['pass'] = (cleanup.get('client_exit_code') == 0 and cleanup.get('termios_restored')
                               and cleanup['owned_processes_gone'] and cleanup['binary_unchanged']
                               and cleanup['socket_absent_after_owned_cleanup']
                               and not cleanup.get('forced_client_termination', False))
        except Exception:
            cleanup['error'] = traceback.format_exc()
            cleanup['pass'] = False
        finally:
            self.pump()
            (self.root / 'client-output.bin').write_bytes(self.raw)
            self.record['client_output'] = {'bytes': len(self.raw), 'sha256': hashlib.sha256(self.raw).hexdigest()}
            for fd in (self.master, self.slave):
                if fd is not None:
                    os.close(fd)
            self.record['end'] = now()
            save(self.root / 'receipt.json', self.record)

    def execute(self):
        try:
            self.start()
            self.run_inputs()
            self.record['behavior_pass'] = True
        except Exception:
            self.record['behavior_pass'] = False
            self.record['error'] = traceback.format_exc()
        finally:
            self.finish()
        self.record['pass'] = self.record['behavior_pass'] and self.record['cleanup']['pass']
        save(self.root / 'receipt.json', self.record)
        return self.record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rmux')
    parser.add_argument('--tmux')
    parser.add_argument('--root', type=Path)
    parser.add_argument('--case-timeout', type=float, default=20)
    parser.add_argument('--application')
    parser.add_argument('--application-mouse', action='store_true')
    args = parser.parse_args()
    if args.application:
        application(args.application, args.application_mouse)
        return 0
    assert args.rmux and args.tmux and args.root
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    summary = {'start': now(), 'driver_sha256': sha(__file__), 'oracle': [], 'rmux': [],
               'source_sha': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
               'contract': 'L2-G6-SELECTION-COPY-01', 'pass': False}
    for kind, binary in [('tmux', args.tmux), ('rmux', args.rmux)]:
        assert sha(binary) == PINS[kind], (kind, 'unexpected binary')
        for index, spec in enumerate(CASES):
            result = Case(binary, kind, root / kind / str(index + 1), spec, args.case_timeout).execute()
            key = 'oracle' if kind == 'tmux' else 'rmux'
            summary[key].append(result)
            print(json.dumps({'kind': kind, 'case': spec[0], 'pass': result['pass'],
                              'outcome': result.get('outcome'), 'error': result.get('error'),
                              'cleanup': result['cleanup']}), flush=True)
            save(root / 'summary.json', summary)
            if not result['pass']:
                summary['stop_reason'] = kind + ': first failed case; no expectation changed or blind repeat'
                summary['end'] = now()
                save(root / 'summary.json', summary)
                return 1
        if kind == 'tmux':
            save(root / 'ORACLE-FROZEN.json', {'at': now(), 'binary': str(Path(binary).resolve()),
                                             'sha256': sha(binary), 'cases': summary['oracle']})
            summary['oracle_sha256'] = sha(root / 'ORACLE-FROZEN.json')
        else:
            summary['outcomes_equal'] = all(a['outcome'] == b['outcome'] for a, b in zip(summary['oracle'], summary['rmux']))
            if not summary['outcomes_equal']:
                summary['end'] = now()
                save(root / 'summary.json', summary)
                return 1
    summary['pass'] = True
    summary['end'] = now()
    save(root / 'summary.json', summary)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
