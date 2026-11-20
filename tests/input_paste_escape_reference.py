#!/usr/bin/env python3
"""Native paste/Escape contract; freeze tmux 3.7b observations before RMUX.

Real client/application PTYs and the accepted G6 lifecycle helper are used.
The 1500 ms bound is functional liveness, not escape-time parameter parity.
"""
import argparse
import fcntl
import importlib.util
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
import tty

HELPER = Path(__file__).with_name('g6_selection_copy_reference.py')
spec = importlib.util.spec_from_file_location('g6_native', HELPER)
g6 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g6)
assert g6.sha(HELPER) == '389da8879380ee4d2f91afa241aa058b4d06b438e018f5351e93062e9a40e2dd'
BODY = 'alpha\ncafé\tomega'.encode()
WRAPPED = b'\x1b[200~' + BODY + b'\x1b[201~'
FRAGMENTS = [b'\x1b[20', b'0~alpha\ncaf\xc3', b'\xa9\tomega\x1b[20', b'1~']
EXPECTED = {'P1': WRAPPED, 'P2': BODY, 'P3': WRAPPED, 'E1': b'\x1bx'}
BOUND_NS = 1_500_000_000


def application(root, aware):
    root = Path(root)
    before = termios.tcgetattr(0)
    tty.setraw(0)
    g6.save(root / 'application-start.json', {
        'at': g6.now(), 'identity': g6.process_identity(os.getpid()),
        'tty': os.ttyname(0), 'bracketed_aware': aware,
    })
    signal.signal(signal.SIGHUP, lambda *_: sys.exit(0))
    stop = time.monotonic() + 30
    try:
        while not (root / 'emit').exists():
            if time.monotonic() >= stop:
                raise RuntimeError('application readiness timeout')
            time.sleep(.005)
        (root / 'application-input.bin').write_bytes(b'')
        (root / 'application-reads.jsonl').write_text('')
        output = (b'\x1b[?2004h' if aware else b'\x1b[?2004l')
        output += b'INPUT READY\r\n\x1b]2;INPUT-READY\x07'
        os.write(1, output)
        g6.save(root / 'application-ready.json', {'at': g6.now(), 'output_hex': output.hex()})
        with (root / 'application-input.bin').open('ab', buffering=0) as sink:
            while time.monotonic() < stop:
                if select.select([0], [], [], .02)[0]:
                    data = os.read(0, 65536)
                    if not data:
                        break
                    event = {'read_ns': time.monotonic_ns(), 'hex': data.hex()}
                    # Write the timestamp first: observing sink bytes then proves
                    # the corresponding timestamp is available to the parent.
                    with (root / 'application-reads.jsonl').open('a') as log:
                        log.write(json.dumps(event) + '\n')
                    sink.write(data)
    finally:
        try:
            termios.tcsetattr(0, termios.TCSANOW, before)
        except termios.error:
            pass
        g6.save(root / 'application-end.json', {'at': g6.now()})


class InputCase(g6.Case):
    def remaining(self):
        if time.monotonic() >= self.deadline:
            raise TimeoutError('native case deadline reached')
        return super().remaining()

    def start(self):
        command = shlex.join([
            sys.executable, str(Path(__file__).resolve()), '--application', str(self.root),
            *(['--application-aware'] if self.name in ('P1', 'P3') else []),
        ])
        self.started = True
        self.q('new-session', '-d', '-s', 'g6', '-x', '20', '-y', '5', command)
        self.q('set-option', '-g', 'status', 'off')
        self.q('set-option', '-s', 'escape-time', '50')
        self.equal(self.text('show-options', '-s', '-v', 'escape-time'), '50', 'escape-time setup')
        pid, socket, version = self.fmt('#{pid}|#{socket_path}|#{version}').split('|')
        self.equal(str(Path(socket).resolve()), str(self.socket), 'dedicated socket')
        server = g6.process_identity(int(pid))
        assert server is not None
        server_hash = g6.sha(server['executable'])
        permitted = [g6.PINS[self.kind]] + ([g6.PINS['rmux-daemon']] if self.kind == 'rmux' else [])
        self.equal(server_hash in permitted, True, 'pinned server executable')
        self.identities.append(server)
        self.record['server'] = {**server, 'version': version, 'sha256': server_hash}
        self.until(lambda: (self.root / 'application-start.json').exists(), 'application raw tty')
        app = json.loads((self.root / 'application-start.json').read_text())
        self.identities.append(app['identity'])
        pane = g6.process_identity(int(self.fmt('#{pane_pid}')))
        assert pane is not None
        self.identities.append(pane)
        self.record['application'] = app
        self.record['pane_process'] = pane
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 5, 20, 0, 0))
        self.original_termios = termios.tcgetattr(self.slave)
        self.record['termios_before'] = g6.terminal_attributes(self.original_termios)
        self.client_tty = os.ttyname(self.slave)

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        argv = [self.binary, '-S', str(self.socket), '-f', '/dev/null', 'attach-session', '-t', 'g6']
        self.client = subprocess.Popen(argv, env=self.env, stdin=self.slave,
                                       stdout=self.slave, stderr=self.slave, preexec_fn=setup)
        self.until(lambda: self.client_tty in self.text('list-clients', '-F', '#{client_tty}'), 'client attached')
        clients = self.text('list-clients', '-F', '#{client_pid}|#{client_tty}|#{client_width}|#{client_height}')
        self.record['client'] = {'argv': argv, 'pid': self.client.pid,
                                 'identity': g6.process_identity(self.client.pid),
                                 'tty': self.client_tty, 'list_clients': clients}
        self.equal(len(clients.splitlines()), 1, 'one attached client')
        self.equal(clients.split('|')[-2:], ['20', '5'], 'client geometry')
        self.equal(self.fmt('#{pane_width}|#{pane_height}'), '20|5', 'pane geometry')
        self.record['termios_attached'] = g6.terminal_attributes(termios.tcgetattr(self.slave))
        self.equal(termios.tcgetattr(self.slave) != self.original_termios, True, 'client terminal changed')
        (self.root / 'emit').touch()
        self.until(lambda: self.fmt('#{pane_title}') == 'INPUT-READY', 'pane processed application modes')
        self.equal(self.text('capture-pane', '-p', '-t', 'g6:0.0').splitlines()[0], 'INPUT READY', 'native content')
        self.equal(self.mode(), '|0', 'ordinary input route')
        if self.name == 'E1':
            self.record['settle_start_ns'] = time.monotonic_ns()
            time.sleep(.7)
            self.pump()
        self.record['escape_time_at_input'] = self.text('show-options', '-s', '-v', 'escape-time')
        self.equal(self.record['escape_time_at_input'], '50', 'escape-time readback before input')
        self.record['readiness_ns'] = time.monotonic_ns()

    def received(self):
        return (self.root / 'application-input.bin').read_bytes()

    def reads(self):
        return [json.loads(line) for line in (self.root / 'application-reads.jsonl').read_text().splitlines()]

    def inject(self, data):
        self.remaining()
        before = time.monotonic_ns()
        count = os.write(self.master, data)
        after = time.monotonic_ns()
        self.record.setdefault('writes', []).append({'before_ns': before, 'after_ns': after, 'hex': data.hex()})
        self.equal(count, len(data), 'complete terminal write')
        return before

    def expect_by(self, expected, sent_ns, label):
        while time.monotonic_ns() - sent_ns <= BOUND_NS:
            self.remaining()
            self.pump()
            if len(self.received()) >= len(expected):
                self.equal(self.received().hex(), expected.hex(), label + ' exact bytes')
                read_ns = self.reads()[-1]['read_ns']
                self.equal(read_ns - sent_ns <= BOUND_NS, True, label + ' delivered within 1500ms')
                return read_ns
            time.sleep(.002)
        self.equal(self.received().hex(), expected.hex(), label + ' deadline bytes')
        raise TimeoutError(label + ' delivery deadline')

    def run_inputs(self):
        if self.name == 'E1':
            sent = self.inject(b'\x1b')
            escape_read = self.expect_by(b'\x1b', sent, 'isolated Escape')
            self.record['before_following_key_hex'] = self.received().hex()
            self.record['escape_read_ns'] = escape_read
            time.sleep(.15)
            sent = self.inject(b'x')
            self.equal(sent - escape_read >= 150_000_000, True, 'next key follows Escape by at least150ms')
            self.expect_by(EXPECTED[self.name], sent, 'following ordinary key')
        elif self.name == 'P3':
            for index, fragment in enumerate(FRAGMENTS):
                sent = self.inject(fragment)
                if index < len(FRAGMENTS) - 1:
                    time.sleep(.005)
            self.expect_by(EXPECTED[self.name], sent, 'fragmented paste')
        else:
            sent = self.inject(WRAPPED)
            self.expect_by(EXPECTED[self.name], sent, 'paste')
        settled_at = time.monotonic_ns()
        time.sleep(.15)
        self.pump()
        self.equal(self.received().hex(), EXPECTED[self.name].hex(), 'no additional bytes after150ms')
        self.record['quiet_observation_ns'] = [settled_at, time.monotonic_ns()]
        reads = self.reads()
        self.equal(self.mode(), '|0', 'ordinary route retained')
        self.record['outcome'] = {
            'received_hex': self.received().hex(), 'mode': self.mode(),
            'first_delivery_ms': (reads[0]['read_ns'] - self.record['writes'][0]['before_ns']) / 1e6,
            'last_delivery_from_last_write_ms': (reads[-1]['read_ns'] - self.record['writes'][-1]['before_ns']) / 1e6,
            'reads': reads,
        }
        if self.name == 'E1':
            self.record['outcome']['escape_before_next_write'] = reads[0]['read_ns'] < self.record['writes'][1]['before_ns']
            self.equal(self.record['outcome']['escape_before_next_write'], True, 'Escape completed independently')

    def finish(self):
        super().finish()
        enable = self.raw.rfind(b'\x1b[?2004h')
        disable = self.raw.rfind(b'\x1b[?2004l')
        restored = disable > enable >= 0
        cleanup = self.record['cleanup']
        cleanup['outer_mode_2004'] = {'last_enable_offset': enable, 'last_disable_offset': disable, 'restored': restored}
        cleanup['pass'] = cleanup['pass'] and restored
        g6.save(self.root / 'receipt.json', self.record)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rmux')
    parser.add_argument('--tmux')
    parser.add_argument('--root', type=Path)
    parser.add_argument('--case-timeout', type=float, default=20)
    parser.add_argument('--run-timeout', type=float, default=180)
    parser.add_argument('--application')
    parser.add_argument('--application-aware', action='store_true')
    args = parser.parse_args()
    if args.application:
        application(args.application, args.application_aware)
        return 0
    assert args.rmux and args.tmux and args.root
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    stop = time.monotonic() + args.run_timeout
    summary = {'start': g6.now(), 'contract': 'L2-INPUT-PASTE-ESC-01',
               'timing_semantics': 'I003-T1 open; 1500ms functional bound, not parameter parity',
               'driver_sha256': g6.sha(__file__), 'helper_sha256': g6.sha(HELPER),
               'source_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
               'oracle': [], 'rmux': [], 'pass': False}
    for kind, binary in [('tmux', args.tmux), ('rmux', args.rmux)]:
        assert g6.sha(binary) == g6.PINS[kind], 'binary pin differs'
        for name in EXPECTED:
            # Reserve bounded cleanup time from the full-run envelope.
            budget = min(args.case_timeout, stop - time.monotonic() - 12)
            if budget <= 0:
                summary['stop_reason'] = 'full run budget exhausted'
                summary['end'] = g6.now()
                g6.save(root / 'summary.json', summary)
                return 1
            if kind == 'rmux':
                assert g6.sha(root / 'ORACLE-FROZEN.json') == summary['oracle_sha256']
            result = InputCase(binary, kind, root / kind / name, (name, [], EXPECTED[name]), budget).execute()
            summary['oracle' if kind == 'tmux' else 'rmux'].append(result)
            g6.save(root / 'summary.json', summary)
            print(json.dumps({'kind': kind, 'case': name, 'pass': result['pass'],
                              'outcome': result.get('outcome'), 'error': result.get('error'),
                              'cleanup_pass': result['cleanup']['pass'],
                              'outer_2004': result['cleanup'].get('outer_mode_2004')}), flush=True)
            if not result['pass']:
                summary['stop_reason'] = kind + '/' + name + ' failed; no expectations changed'
                summary['end'] = g6.now()
                g6.save(root / 'summary.json', summary)
                return 1
        if kind == 'tmux':
            g6.save(root / 'ORACLE-FROZEN.json', {'at': g6.now(), 'cases': summary['oracle'],
                                               'tmux_sha256': g6.PINS['tmux']})
            summary['oracle_sha256'] = g6.sha(root / 'ORACLE-FROZEN.json')
    summary['semantic_outcomes_equal'] = all(
        a['outcome']['received_hex'] == b['outcome']['received_hex']
        and a['outcome']['mode'] == b['outcome']['mode']
        for a, b in zip(summary['oracle'], summary['rmux']))
    summary['pass'] = summary['semantic_outcomes_equal']
    summary['end'] = g6.now()
    g6.save(root / 'summary.json', summary)
    return 0 if summary['pass'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
