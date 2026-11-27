#!/usr/bin/env python3
"""Bounded popup PTY fixture, reusing the native history harness lifecycle.

One recipe for tmux and RMUX; inherited public endpoint/PID qualification and
cleanup. Captures use the installed independent pyte parser. No product probes.
"""
import ctypes
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time
import traceback

sys.dont_write_bytecode = True
BASE = Path(__file__).with_name('history_limit_reference.py')
spec = importlib.util.spec_from_file_location('history_support', BASE)
B = importlib.util.module_from_spec(spec)
spec.loader.exec_module(B)
COLS, ROWS = 225, 54
MAX_CAPTURE = 6 * 1024 * 1024

def final_rows(label):
    return [(f'FINAL033|{label}|{n:02d}|' + chr(65 + n % 26) * COLS)[:COLS]
            for n in range(ROWS)]

def final_bytes(label):
    return b'\x1b[0m\x1b[2J' + b''.join(
        f'\x1b[{n + 1};1H'.encode() + row.encode() for n, row in enumerate(final_rows(label))) + b'\x1b[8;12H\x1b[?25h'

def burst_bytes(label, count):
    token = (f'BURST039|{label}|'.encode() if label.startswith('replace-') else b'Q')
    return (token * (count // len(token) + 1))[:count]

def replacement_surface(screen):
    return (screen.columns == COLS and screen.lines == ROWS and
            screen.display == final_rows('replace-new') and
            (screen.cursor.x, screen.cursor.y) == (11, 7) and not screen.cursor.hidden)

def producer(root, label, total):
    root = Path(root)
    B.write_once(root / 'born.json', {'process': B.proc(os.getpid()), 'at': B.now()})
    deadline = time.monotonic() + 90
    def wait(name):
        while not (root / name).exists():
            B.require(time.monotonic() < deadline, 'producer deadline')
            time.sleep(.005)
    wait('go')
    final = final_bytes(label)
    remaining = int(total) - len(final)
    writes, sent = [], 0
    for stage in range(4):
        target = remaining * (stage + 1) // 4
        while sent < target:
            count = min(4096, target - sent)
            data = burst_bytes(label, count)
            begin = time.monotonic()
            n = os.write(1, data)
            writes.append({'bytes': n, 'seconds': time.monotonic() - begin})
            sent += n
            if label == 'stalled500':
                time.sleep(.015)
        B.write_once(root / f'stage{stage}.json', {'bytes': sent, 'at': B.now()})
        wait(f'continue{stage}')
    size = os.get_terminal_size(1)
    B.equal((size.columns, size.lines), (COLS, ROWS), 'producer final native PTY geometry')
    offset = 0
    while offset < len(final):
        offset += os.write(1, final[offset:])
    B.write_once(root / 'emitted.json', {'at': B.now(), 'bytes': sent + offset,
        'final_hex': final.hex(), 'final_size': [size.columns, size.lines], 'writes': writes})
    wait('exit')
    B.write_once(root / 'exit-intent.json', {'at': B.now(), 'identity': B.identity(os.getpid()), 'code': 0})

def owned_child_absent(own, receipt, identity=B.identity, waitpid=os.waitpid, parent=None):
    """Reap only a terminated child of this harness, then check the same birth."""
    current = identity(own['pid'])
    receipt.setdefault('wait_status', None)  # The original parent may reap it.
    observations = receipt.setdefault('observations', [])
    key = lambda value: None if value is None else tuple(
        value[k] for k in ('pid', 'start_ticks', 'state', 'ppid'))
    if not observations or key(observations[-1]) != key(current):
        observations.append(current)
    if not B.same(own, current):
        receipt['same_birth_absent'] = True
        return True
    receipt['same_birth_absent'] = False
    if current['state'] not in ('Z', 'X') or current['ppid'] != (os.getpid() if parent is None else parent):
        return False
    try:
        pid, status = waitpid(own['pid'], os.WNOHANG)
    except ChildProcessError:
        receipt['wait_error'] = 'ECHILD; status unknown'
    else:
        if pid:
            B.equal(pid, own['pid'], 'waitpid returned a different child')
            receipt['wait_status'] = {'pid': pid, 'raw': status,
                'exited': os.WIFEXITED(status),
                'exit_code': os.WEXITSTATUS(status) if os.WIFEXITED(status) else None,
                'signaled': os.WIFSIGNALED(status),
                'signal': os.WTERMSIG(status) if os.WIFSIGNALED(status) else None}
    after = identity(own['pid'])
    if key(observations[-1]) != key(after):
        observations.append(after)
    receipt['same_birth_absent'] = not B.same(own, after)
    return receipt['same_birth_absent']

def transport_timeout_events(raw, daemon_pid, attach_pid, elapsed):
    """Recognize the existing product timeout receipt for this owned attach."""
    pattern = (r'time_ms=(\d+) process_pid=(\d+) attach_pid=(\d+) '
               r'session=alpha reason=attach-error '
               r'detail="attach client did not drain server output"')
    events = []
    for line in raw.splitlines(keepends=True):
        if not line.endswith('\n'):
            continue
        match = re.fullmatch(pattern, line[:-1])
        if match and (int(match[2]), int(match[3])) == (daemon_pid, attach_pid):
            B.require(elapsed >= 5, 'premature transport timeout receipt')
            events.append({'time_ms': int(match[1]), 'process_pid': int(match[2]),
                           'attach_pid': int(match[3]), 'raw_line': line})
    return events

class Run(B.Run):
    def __init__(self, config, phase):
        import pyte
        super().__init__(config, phase, time.monotonic())
        self.env['TERM'] = 'xterm-256color'
        self.exit_log = self.root / 'attach-exit.log'
        if phase == 'candidate':
            self.env['RMUX_ATTACH_EXIT_LOG'] = str(self.exit_log)
        self.master = None
        self.client = None
        self.popup_command = None
        class CaptureScreen(pyte.Screen):
            # pyte0.8.2 does not accept private DSR queries. They do not draw
            # cells; preserve their raw bytes and ignore the query in this
            # passive observer, like pyte's private DA handling.
            def report_device_status(self, mode, **kwargs):
                if not kwargs.get('private'):
                    super().report_device_status(mode)
        self.screen = CaptureScreen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.capture = bytearray()
        self.record['rss_samples'] = []
        self.record['surfaces'] = {}
        self.record['max_owned_helpers'] = 0
        self.record['helper_births'] = []

    def pump(self, duration=.02, read=True):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            self.remaining()
            if read and self.master is not None and select.select([self.master], [], [], .005)[0]:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    break
                if not data:
                    break
                self.capture.extend(data)
                B.require(len(self.capture) <= MAX_CAPTURE, 'PTY capture cap')
                self.stream.feed(data)
            else:
                time.sleep(.005)
        if self.daemon:
            pending, seen = [os.getpid()], set()
            while pending:
                pid = pending.pop()
                if pid in seen:
                    continue
                seen.add(pid)
                try:
                    for task in Path('/proc', str(pid), 'task').iterdir():
                        try:
                            pending.extend(map(int, (task / 'children').read_text().split()))
                        except FileNotFoundError:
                            pass
                except FileNotFoundError:
                    pass
            for pid in seen - {os.getpid()}:
                own = B.identity(pid)
                if own and not any(B.same(own, prior) for prior in self.record['helper_births']):
                    self.record['helper_births'].append(own)
                    self.owned[pid] = own
            helpers = len(seen - {os.getpid(), self.daemon['before']['pid']})
            self.record['max_owned_helpers'] = max(self.record['max_owned_helpers'], helpers)
            B.require(helpers <= 8, 'owned helper cap')
            status = Path('/proc', str(self.daemon['before']['pid']), 'status')
            if status.exists():
                self.record['rss_samples'].append({'at': B.now(), 'bytes_read': len(self.capture),
                    'values': [v for v in status.read_text().splitlines() if v.startswith(('VmRSS:', 'VmHWM:'))]})

    def until(self, condition, message, seconds=8):
        end = time.monotonic() + seconds
        while not condition():
            B.require(time.monotonic() < end, message)
            self.pump()

    def mark(self, root, name):
        (root / name).write_text(B.now())

    def resize(self, cols, rows):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        os.kill(self.client.pid, signal.SIGWINCH)
        self.screen.resize(lines=rows, columns=cols)
        self.until(lambda: self.command(['list-clients', '-F', '#{client_width}|#{client_height}']) == f'{cols}|{rows}', 'attached geometry not updated')
        self.pump(.1)

    def snapshot(self, label):
        value = {'rows': self.screen.display, 'cursor': [self.screen.cursor.x, self.screen.cursor.y],
                 'hidden': self.screen.cursor.hidden, 'geometry': [self.screen.columns, self.screen.lines]}
        self.record['surfaces'][label] = value
        total = sum(p.stat().st_size for p in Path(self.config['root']).rglob('*') if p.is_file())
        B.require(total + len(self.capture) < 64 * 1024 * 1024, 'aggregate native log cap')
        (self.root / (label + '.pty.bin')).write_bytes(bytes(self.capture))
        self.record['captures'].append({'key': label, 'bytes': len(self.capture),
            'sha256': B.digest(self.root / (label + '.pty.bin'))})
        return value

    def client_live(self):
        B.require(self.client.poll() is None, 'healthy attached client exited')
        B.equal(self.command(['list-clients', '-F', '#{client_tty}']), self.tty, 'same attached client retained')

    def popup(self, label, total, slow=False, resize=False, close=False):
        folder = self.root / label
        folder.mkdir()
        command = shlex.join([self.config['python'], '-I', str(Path(__file__).resolve()),
                              'producer', str(folder), label, str(total)])
        argv = [self.binary, '-N', '-S', str(self.socket), 'display-popup', '-c', self.tty,
                '-B', '-w', '100%', '-h', '100%', command]
        log = (folder / 'command.log').open('wb')
        self.popup_command = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        born = B.identity(self.popup_command.pid)
        self.owned[born['pid']] = born
        case = {'id': label, 'argv': argv, 'command_identity': born, 'start': B.now(), 'total_limit': total}
        self.record['cases'].append(case)
        try:
            self.until(lambda: (folder / 'born.json').exists(), 'popup producer absent')
            producer_born = json.loads((folder / 'born.json').read_text())['process']
            B.require(B.complete(producer_born), 'producer identity incomplete')
            own = producer_born['before']
            self.owned[own['pid']] = own
            case['producer'] = producer_born
            self.mark(folder, 'go')
            for stage in range(4):
                if slow and stage == 0:
                    self.pump(1, read=False)
                    case['paused_seconds'] = 1
                self.until(lambda: (folder / f'stage{stage}.json').exists(), 'bounded producer stage absent')
                if close and stage == 1:
                    self.command(['display-popup', '-C', '-c', self.tty])
                    case['termination'] = {}
                    self.until(lambda: owned_child_absent(own, case['termination']), 'closed popup producer remains active')
                    self.pump(.2)
                    self.client_live()
                    case.update(status='PASS', closed_at=B.now(), emitted_stages=2)
                    return
                if resize and stage in (1, 2):
                    self.resize(*((200, 50) if stage == 1 else (COLS, ROWS)))
                self.mark(folder, f'continue{stage}')
            self.until(lambda: (folder / 'emitted.json').exists(), 'producer final receipt absent')
            self.until(lambda: self.screen.display == final_rows(label) and
                       (self.screen.cursor.x, self.screen.cursor.y) == (11, 7), 'exact final displayed surface/cursor absent')
            self.client_live()
            case['emission'] = json.loads((folder / 'emitted.json').read_text())
            B.equal(case['emission']['bytes'], total, 'exact bounded output size')
            self.snapshot(label + '-live')
            self.mark(folder, 'exit')
            case['termination'] = {}
            self.until(lambda: owned_child_absent(own, case['termination']), 'EOF producer not reaped')
            self.pump(.3)
            B.equal(self.screen.display, final_rows(label), 'EOF retains final displayed rows')
            self.client_live()
            self.snapshot(label + '-eof')
            self.command(['display-popup', '-C', '-c', self.tty])
            self.pump(.2)
            case['status'] = 'PASS'
        except Exception:
            case['status'] = 'FAIL'
            case['error'] = traceback.format_exc()
            self.snapshot(label + '-failure')
            raise
        finally:
            if case.get('status') == 'PASS':
                case['command_exit'] = self.popup_command.wait(timeout=2)
            elif self.popup_command.poll() is not None:
                case['command_exit'] = self.popup_command.wait()
            log.close()
            case['end'] = B.now()

    def replacement(self):
        case = {'id': 'replacement', 'status': 'RUNNING', 'start': B.now(),
                'internal_generation': 'UNKNOWN', 'publication_count': 'UNKNOWN',
                'commands': [], 'producers': {}, 'delayed_samples': []}
        self.record['cases'].append(case)
        launched, logs = [], []
        def launch(label):
            folder = self.root / label
            folder.mkdir()
            command = shlex.join([self.config['python'], '-I', str(Path(__file__).resolve()),
                                  'producer', str(folder), label, '512000'])
            argv = [self.binary, '-N', '-S', str(self.socket), 'display-popup', '-c', self.tty,
                    '-B', '-w', '100%', '-h', '100%', command]
            log = (folder / 'command.log').open('wb')
            logs.append(log)
            process = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            own = B.identity(process.pid)
            B.require(own is not None, 'popup command birth missing')
            self.owned[own['pid']] = own
            row = {'label': label, 'argv': argv, 'identity': own, 'start': B.now()}
            case['commands'].append(row)
            launched.append((process, row))
            self.popup_command = process
            return folder, process, row
        def birth(folder, label):
            self.until(lambda: (folder / 'born.json').exists(), label + ' producer absent')
            value = json.loads((folder / 'born.json').read_text())['process']
            B.require(B.complete(value), label + ' producer birth incomplete')
            B.equal(value['exe_sha256'], B.digest(self.config['python']), 'producer Python image')
            B.equal(value['argv'], [self.config['python'], '-I', str(Path(__file__).resolve()),
                                  'producer', str(folder), label, '512000'], 'producer exact argv')
            B.require(B.same(value['before'], B.identity(value['before']['pid'])), label + ' producer changed')
            self.owned[value['before']['pid']] = value['before']
            case['producers'][label] = value
            return value['before']
        def final_surface():
            return replacement_surface(self.screen)
        try:
            old, old_command, old_row = launch('replace-old')
            old_birth = birth(old, 'replace-old')
            self.mark(old, 'go')
            for stage in range(3):
                self.until(lambda: (old / f'stage{stage}.json').exists(), 'old burst boundary absent')
                if stage < 2:
                    self.mark(old, f'continue{stage}')
            case['old_boundary'] = json.loads((old / 'stage2.json').read_text())
            B.require(307200 <= case['old_boundary']['bytes'] <= 512000, 'old burst amount outside frozen range')
            self.pump(.3)
            B.require(any('replace-old' in row for row in self.screen.display), 'old labeled burst not visible')
            old_surface = self.snapshot('replace-old-boundary')
            noop, noop_command, noop_row = launch('replace-noop')
            self.until(lambda: noop_command.poll() is not None, 'second popup command did not return')
            noop_row['exit'] = noop_command.wait()
            B.equal(noop_row['exit'], 0, 'second popup no-op command failed')
            self.pump(.3)
            B.require(not (noop / 'born.json').exists(), 'second popup unexpectedly launched a producer')
            B.require(B.same(old_birth, B.identity(old_birth['pid'])), 'second popup replaced old producer')
            B.equal(self.snapshot('replace-old-noop'), old_surface, 'second popup changed old displayed surface')
            self.client_live()
            case['old_live_before_close'] = B.identity(old_birth['pid'])
            B.require(B.same(old_birth, case['old_live_before_close']) and
                      case['old_live_before_close']['state'] not in ('Z', 'X'), 'old producer not live at replacement')
            self.command(['display-popup', '-C', '-c', self.tty])
            case['close_command'] = self.record['commands'][-1]
            new, new_command, new_row = launch('replace-new')
            new_birth = birth(new, 'replace-new')
            B.require(not B.same(old_birth, new_birth), 'replacement reused old producer birth')
            case['old_termination'] = {}
            self.until(lambda: owned_child_absent(old_birth, case['old_termination']), 'old producer survives explicit close')
            self.until(lambda: old_command.poll() is not None, 'old popup command remains active')
            old_row['exit'] = old_command.wait()
            case['old_closed_before_new_output'] = B.now()
            B.require(not (old / 'emitted.json').exists(), 'old producer escaped burst boundary')
            self.client_live()
            self.mark(new, 'go')
            for stage in range(4):
                self.until(lambda: (new / f'stage{stage}.json').exists(), 'new burst boundary absent')
                if stage in (1, 2):
                    self.resize(*((200, 50) if stage == 1 else (COLS, ROWS)))
                    self.snapshot('replace-new-resize' + str(stage))
                self.mark(new, f'continue{stage}')
            self.until(lambda: (new / 'emitted.json').exists(), 'new final receipt absent')
            case['emission'] = json.loads((new / 'emitted.json').read_text())
            B.equal(case['emission']['bytes'], 512000, 'new exact output amount')
            self.until(final_surface, 'new exact final rows/cursor absent')
            self.client_live()
            self.snapshot('replace-new-live')
            self.mark(new, 'exit')
            case['new_termination'] = {}
            self.until(lambda: owned_child_absent(new_birth, case['new_termination']), 'new EOF producer remains active')
            self.pump(.3)
            B.require(final_surface(), 'new EOF surface/cursor changed')
            self.snapshot('replace-new-eof')
            offset = len(self.capture)
            for _ in range(5):
                self.pump(.1)
                B.require(final_surface(), 'old output or changed surface after replacement')
                B.require(not B.same(old_birth, B.identity(old_birth['pid'])), 'old producer reappeared')
                case['delayed_samples'].append({'at': B.now(), 'capture_bytes': len(self.capture),
                                               'rows_equal': True, 'cursor': [11, 7]})
            B.require(not (noop / 'born.json').exists(), 'no-op producer appeared late')
            case['delayed_raw_offset'] = offset
            self.snapshot('replace-new-quiescent')
            self.client_live()
            self.command(['display-popup', '-C', '-c', self.tty])
            self.until(lambda: new_command.poll() is not None, 'new popup command remains active after close')
            new_row['exit'] = new_command.wait()
            self.pump(.2)
            self.client_live()
            case['status'] = 'PASS'
        except Exception:
            case.update(status='FAIL', error=traceback.format_exc())
            self.snapshot('replace-failure')
            raise
        finally:
            for process, row in launched:
                row['exit'] = process.poll()
                row['end'] = B.now()
            for log in logs:
                log.close()
            case['end'] = B.now()

    def stalled(self):
        folder = self.root / 'stalled500'
        folder.mkdir()
        command = shlex.join([self.config['python'], '-I', str(Path(__file__).resolve()),
                              'producer', str(folder), 'stalled500', '512000'])
        argv = [self.binary, '-N', '-S', str(self.socket), 'display-popup', '-c', self.tty,
                '-B', '-w', '100%', '-h', '100%', command]
        with (folder / 'command.log').open('wb') as log:
            self.popup_command = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            own = B.identity(self.popup_command.pid)
            self.owned[own['pid']] = own
            self.until(lambda: (folder / 'born.json').exists(), 'stalled producer absent')
            producer = json.loads((folder / 'born.json').read_text())['process']['before']
            self.owned[producer['pid']] = producer
            self.pump(.3)  # Drain popup startup before starting the pressure clock.
            self.client_live()
            if self.phase == 'candidate':
                B.equal(self.exit_log.read_text() if self.exit_log.exists() else '', '',
                        'healthy/startup control already produced an exit event')
            case = {'id': 'stalled500', 'argv': argv, 'command_identity': own,
                    'producer_identity': producer, 'start': B.now(), 'status': 'RUNNING',
                    'policy': 'existing5s transport timeout; unchanged7s unregistration assertion'}
            self.record['cases'].append(case)
            before = len(self.capture)
            begin = time.monotonic()
            case['monotonic_begin'] = begin
            last_observation = 0.0
            def observe():
                nonlocal last_observation
                elapsed = time.monotonic() - begin
                if self.phase == 'candidate':
                    B.require(B.same(self.daemon['before'], B.identity(self.daemon['before']['pid'])),
                              'owned daemon identity changed')
                    current = B.identity(self.client.pid)
                    B.require(current is None or B.same(self.record['client']['identity'], current),
                              'attached PID reused during observation')
                    raw = self.exit_log.read_text() if self.exit_log.exists() else ''
                    B.require(len(raw.encode()) < 1024 * 1024, 'private exit log cap')
                    events = transport_timeout_events(raw, self.daemon['before']['pid'], self.client.pid, elapsed)
                    if events and 'timeout_observation' not in case:
                        B.equal(len(events), 1, 'unique owned timeout event')
                        case['timeout_observation'] = {'previous_poll_seconds': last_observation,
                            'observed_seconds': elapsed, 'events': events, 'raw_log': raw,
                            'writer_interval': 'source-qualified Tokio5s expiry; exact write start/bytes unknown'}
                last_observation = elapsed
            for stage in range(4):
                self.mark(folder, f'continue{stage}')
            self.mark(folder, 'go')
            while time.monotonic() - begin < 7:
                self.pump(.1, read=False)
                observe()
            B.equal(len(self.capture), before, 'non-reader must not consume bytes')
            case['status'] = 'FAIL'  # A failed public query cannot prove absence.
            clients = self.command(['list-clients', '-F', '#{client_tty}'])
            case.update(pause_seconds=time.monotonic() - begin, read_bytes_during_pause=0,
                        registered_after_pause=clients, status='OBSERVED')
            if self.phase == 'candidate':
                # Preserve a failure at7s even if a later natural exit is observed.
                case['status'] = 'FAIL'
                case['unregistered_by_seven_seconds'] = clients == ''
                case['natural_unregister_seconds'] = time.monotonic() - begin if not clients else None
                if clients and self.config.get('focus_transport'):
                    while clients and time.monotonic() - begin < 12:
                        self.pump(.25, read=False)
                        observe()
                        clients = self.command(['list-clients', '-F', '#{client_tty}'])
                        if not clients:
                            case['natural_unregister_seconds'] = time.monotonic() - begin
                B.equal(case['registered_after_pause'], '', 'truly stalled RMUX client must be unregistered')
                B.require('timeout_observation' in case, 'pending transport write prerequisite unproven')
                case['status'] = 'PASS'
            else:
                self.command(['display-popup', '-C', '-c', self.tty], required=False)
            self.pump(.2)
            self.popup_command.wait(timeout=2)
            case['command_exit'] = self.popup_command.returncode
            case['end'] = B.now()

    def execute(self):
        idle = 'exec /bin/sleep 120' if self.config.get('focus_replacement') else '/bin/sleep 120'
        self.command(['new-session', '-d', '-s', 'alpha', '-x', str(COLS), '-y', str(ROWS), idle], startup=True)
        public, _ = self.public('alpha')
        pane = B.identity(public['pane_pid'])
        self.owned[pane['pid']] = pane
        self.master, slave = pty.openpty()
        self.tty = os.ttyname(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', ROWS, COLS, 0, 0))
        argv = [self.binary, '-N', '-S', str(self.socket), 'attach-session', '-t', 'alpha']
        self.client = subprocess.Popen(argv, env=self.env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.record['client'] = {'argv': argv, 'identity': B.identity(self.client.pid), 'tty': self.tty}
        self.owned[self.client.pid] = self.record['client']['identity']
        self.until(lambda: self.command(['list-clients', '-F', '#{client_tty}']) == self.tty, 'client not registered')
        self.pump(.2)
        if self.config.get('focus_replacement'):
            self.replacement()
            if self.phase == 'candidate':
                golden = json.loads((Path(self.config['root']) / 'ORACLE-FROZEN.json').read_text())
                for key in ('replace-new-live', 'replace-new-eof', 'replace-new-quiescent'):
                    B.equal(self.record['surfaces'][key], golden['surfaces'][key], 'replacement tmux surface ' + key)
                exits = [row['exit'] for row in self.record['cases'][0]['commands']]
                B.equal(exits, golden['replacement_exits'], 'replacement tmux command exits')
            self.record['status'] = 'PASS'
            return
        if self.config.get('focus_transport'):
            self.popup('healthy500', 512000, slow=True)
        else:
            self.popup('burst300', 307200)
            self.popup('slow500', 512000, slow=True, resize=True)
            self.popup('close300', 307200, close=True)
        self.client_live()
        if self.phase == 'candidate':
            golden = json.loads((Path(self.config['root']) / 'ORACLE-FROZEN.json').read_text())
            B.equal(self.record['surfaces'], golden['surfaces'], 'exact independent tmux terminal surfaces/cursors')
        self.stalled()
        self.record['status'] = 'PASS'

    def cleanup(self):
        if self.client and self.client.poll() is None:
            self.command(['detach-client', '-t', self.tty], required=False)
            try:
                self.client.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.client.terminate()
                self.client.wait(timeout=2)
        if self.client:
            self.record['client_exit'] = self.client.returncode
        if self.master is not None:
            os.close(self.master)
            self.master = None
        super().cleanup()
        if self.popup_command:
            self.record['popup_command_exit'] = self.popup_command.poll()


def self_test(root):
    """Exercise real fixture decisions with synthetic processes, never a PTY."""
    from types import SimpleNamespace
    from unittest.mock import patch

    root = Path(root)
    root.mkdir(exist_ok=False)
    checks, scenarios = [], []
    def check(name, passed):
        checks.append({'name': name, 'status': 'PASS' if passed else 'FAIL'})

    def exercise(name, responses, expected_error=None, proof=True, healthy=False,
                 focus=True, early_rejection=False):
        folder = root / name
        folder.mkdir()
        clock, queries = [0.0], []
        births = {pid: {'pid': pid, 'start_ticks': str(pid * 10), 'state': 'S', 'ppid': 1}
                  for pid in (10, 20, 21, 30)}
        obj = object.__new__(Run)
        obj.root, obj.binary, obj.socket = folder, '/not-executed', folder / 's'
        obj.config, obj.env = {'python': '/not-executed'}, {}
        if focus != 'absent':
            obj.config['focus_transport'] = focus
        obj.phase, obj.tty = 'candidate', '/synthetic/tty'
        obj.owned, obj.capture = {}, bytearray()
        obj.start, obj.case_end, obj.cleaning = 0, 150, False
        obj.client = SimpleNamespace(pid=20, poll=lambda: None)
        obj.daemon, obj.exit_log = {'before': births[10]}, folder / 'attach-exit.log'
        obj.record = {'commands': [], 'cases': [], 'client': {'identity': births[20]}}
        class Process:
            def __init__(self, argv, **kwargs):
                self.pid, self.returncode, self.timed_out = 21, 129, False
                if 'display-popup' in argv:
                    B.write_once(folder / 'stalled500/born.json', {'process': {'before': births[30]}})
                    self.response = None
                else:
                    assert 'list-clients' in argv, argv
                    if not healthy and clock[0] < 7:
                        self.response = (0, '/synthetic/tty\n', '')
                    else:
                        self.response = responses[min(len(queries), len(responses) - 1)]
                        queries.append(clock[0])
                    if self.response == 'spawn-failure':
                        raise OSError('synthetic command spawn failure')
                    if self.response != 'timeout':
                        self.returncode = self.response[0]
            def communicate(self, timeout=None):
                if self.response == 'timeout':
                    if not self.timed_out:
                        self.timed_out = True
                        raise subprocess.TimeoutExpired('synthetic list-clients', timeout)
                    return b'', b'synthetic command timeout\n'
                return self.response[1].encode(), self.response[2].encode()
            def kill(self):
                self.returncode = -9
            def wait(self, timeout=None):
                return self.returncode
        def pump(seconds=.02, read=True):
            clock[0] += seconds
            if proof and not read and clock[0] >= (4.8 if proof == 'premature' else 5.4):
                line = ('time_ms=10000 process_pid=10 attach_pid=20 session=alpha '
                        'reason=attach-error detail="attach client did not drain server output"\n')
                if proof == 'wrong-daemon':
                    line = line.replace('process_pid=10', 'process_pid=11')
                if proof == 'duplicate':
                    line *= 2
                obj.exit_log.write_text(line)
        obj.pump = pump
        error = None
        with patch.object(subprocess, 'Popen', Process), \
             patch.object(B, 'identity', lambda pid: births.get(pid)), \
             patch.object(time, 'monotonic', lambda: clock[0]), \
             patch.object(pty, 'openpty', side_effect=AssertionError('self-test must not open a PTY')):
            try:
                obj.client_live() if healthy else obj.stalled()
            except (B.Unproven, B.Failed, OSError) as caught:
                error = str(caught)
        case = obj.record['cases'][0] if obj.record['cases'] else {}
        passed = error is None if expected_error is None else error is not None and expected_error in error
        if not healthy:
            if early_rejection:
                passed = passed and not queries
            else:
                passed = passed and bool(queries) and 7 <= queries[0] < 7.5
            if expected_error is None:
                passed = passed and case.get('status') == 'PASS' and 'timeout_observation' in case
                passed = passed and case.get('read_bytes_during_pause') == 0
            else:
                passed = passed and case.get('status') != 'PASS'
        check(name, passed)
        scenarios.append({'name': name, 'error': error, 'expected_error': expected_error,
                          'case': case, 'public_commands': obj.record['commands'],
                          'query_clock': queries, 'native_processes': 0,
                          'focus_transport': obj.config.get('focus_transport', 'absent'),
                          'timeout_input': proof})

    empty, listed = (0, '', ''), (0, '/synthetic/tty\n', '')
    failed = (1, '', 'synthetic CLI failure\n')
    exercise('successful-empty', [empty])
    exercise('failed-empty', [failed], 'command failed')
    exercise('still-registered', [listed], 'must be unregistered')
    exercise('late-empty-does-not-rescue-seven-seconds', [listed, empty], 'must be unregistered')
    exercise('late-command-failure', [listed, failed], 'command failed')
    exercise('command-timeout', ['timeout'], 'command timeout')
    exercise('command-spawn-failure', ['spawn-failure'], 'synthetic command spawn failure')
    exercise('missing-timeout-premise', [empty], 'pending transport write prerequisite', proof=False)
    exercise('healthy-same-client', [listed], healthy=True)
    exercise('healthy-command-failure', [failed], 'command failed', healthy=True)
    exercise('healthy-missing-client', [empty], 'same attached client retained', healthy=True)

    # The optional campaign selector must never disable a PASS prerequisite.
    for label, focus in [('false', False), ('absent', 'absent')]:
        exercise(label + '-qualified-empty', [empty], focus=focus)
        exercise(label + '-missing-premise', [empty], 'pending transport write prerequisite', proof=False, focus=focus)
        exercise(label + '-failed-empty', [failed], 'command failed', focus=focus)
        exercise(label + '-listed', [listed], 'must be unregistered', focus=focus)
        exercise(label + '-command-timeout', ['timeout'], 'command timeout', focus=focus)
        exercise(label + '-spawn-failure', ['spawn-failure'], 'synthetic command spawn failure', focus=focus)
    for label, focus in [('true', True), ('false', False), ('absent', 'absent')]:
        exercise(label + '-wrong-daemon', [empty], 'pending transport write prerequisite', proof='wrong-daemon', focus=focus)
        exercise(label + '-duplicate', [empty], 'unique owned timeout event', proof='duplicate', focus=focus, early_rejection=True)
        exercise(label + '-premature', [empty], 'premature transport timeout receipt', proof='premature', focus=focus, early_rejection=True)
    B.write_once(root / 'ORACLE-FROZEN.json', {'captures': {}})
    for label, focus in [('true', True), ('false', False), ('absent', 'absent')]:
        config = {'root': str(root), 'candidate': '/not-executed', 'tokens': {'candidate': 'pure'}}
        if focus != 'absent':
            config['focus_transport'] = focus
        candidate = Run(config, 'candidate')
        check(label + '-candidate-timeout-log-enabled',
              candidate.env.get('RMUX_ATTACH_EXIT_LOG') == str(candidate.exit_log))

    own = {'pid': 11, 'start_ticks': '110', 'state': 'S', 'ppid': 7}
    for name, current, after, status, expected, calls in [
        ('live', own, None, None, False, 0),
        ('absent', None, None, None, True, 0),
        ('reused-pid', {**own, 'start_ticks': 'new'}, None, None, True, 0),
        ('foreign-zombie', {**own, 'state': 'Z', 'ppid': 8}, None, None, False, 0),
        ('own-zombie-exit', {**own, 'state': 'Z'}, None, (11, 0), True, 1),
        ('own-zombie-signal', {**own, 'state': 'Z'}, None, (11, 1), True, 1),
        ('not-waitable', {**own, 'state': 'Z'}, own, (0, 0), False, 1),
        ('ECHILD-not-absence', {**own, 'state': 'Z'}, own, ChildProcessError(), False, 1),
    ]:
        values, waited, receipt = iter([current, after]), [], {}
        def wait(pid, flags):
            waited.append(pid)
            if isinstance(status, Exception):
                raise status
            return status
        absent = owned_child_absent(own, receipt, lambda pid: next(values), wait, parent=7)
        check('lifecycle-' + name, absent == expected and len(waited) == calls)
        if status == (11, 1):
            check('lifecycle-exact-signal-status', receipt['wait_status']['signal'] == 1)

    screen = Run({'root': str(root), 'oracle': '/not-executed', 'tokens': {'oracle': 'pure'}}, 'oracle')
    raw = b'\x1b[?6n' + final_bytes('control')
    screen.stream.feed(raw)
    check('surface-all-rows', screen.screen.display == final_rows('control'))
    check('surface-visible-cursor', (screen.screen.cursor.x, screen.screen.cursor.y, screen.screen.cursor.hidden) == (11, 7, False))
    screen.stream.feed(b'\x1b[1;1HZ')
    check('surface-wrong-cell-rejected', screen.screen.display != final_rows('control'))
    for count in (1, 17, 4096):
        check('replacement-burst-size-' + str(count), len(burst_bytes('replace-old', count)) == count)
    check('replacement-burst-distinct-labels', burst_bytes('replace-old', 4096) != burst_bytes('replace-new', 4096))
    check('original-burst-unchanged', burst_bytes('healthy500', 4096) == b'Q' * 4096)
    screen.stream.feed(final_bytes('replace-new'))
    check('replacement-final-exact', replacement_surface(screen.screen))
    screen.stream.feed(final_bytes('replace-old'))
    check('replacement-old-surface-rejected', not replacement_surface(screen.screen))
    screen.stream.feed(final_bytes('replace-new') + b'\x1b[1;1HZ')
    check('replacement-stale-cell-rejected', not replacement_surface(screen.screen))
    screen.stream.feed(final_bytes('replace-new') + b'\x1b[8;13H')
    check('replacement-wrong-cursor-rejected', not replacement_surface(screen.screen))
    screen.stream.feed(final_bytes('replace-new') + b'\x1b[?25l')
    check('replacement-hidden-cursor-rejected', not replacement_surface(screen.screen))
    screen.stream.feed(final_bytes('replace-new'))
    screen.screen.resize(lines=50, columns=200)
    check('replacement-wrong-geometry-rejected', not replacement_surface(screen.screen))
    line = 'time_ms=10000 process_pid=10 attach_pid=20 session=alpha reason=attach-error detail="attach client did not drain server output"\n'
    check('timeout-owned-event', len(transport_timeout_events(line, 10, 20, 5.1)) == 1)
    for name, bad in [('daemon', line.replace('process_pid=10', 'process_pid=11')),
                      ('attach', line.replace('attach_pid=20', 'attach_pid=21')),
                      ('session', line.replace('alpha', 'beta')),
                      ('detail', line.replace('did not drain', 'did drain')),
                      ('truncated', line[:-1])]:
        check('timeout-wrong-' + name, transport_timeout_events(bad, 10, 20, 7) == [])
    try:
        transport_timeout_events(line, 10, 20, 4.99)
    except B.Unproven:
        check('timeout-not-premature', True)
    else:
        check('timeout-not-premature', False)
    result = {'status': 'PASS' if all(c['status'] == 'PASS' for c in checks) else 'FAIL',
              'checks': checks, 'scenarios': scenarios, 'product_runs': 0, 'PTY_runs': 0,
              'source_sha256': B.digest(__file__), 'command_source_sha256': B.digest(BASE)}
    B.write_once(root / 'RESULT.json', result)
    print(json.dumps({'status': result['status'], 'checks': checks}))
    return 0 if result['status'] == 'PASS' else 1


def main(config_path):
    config = json.loads(Path(config_path).read_text())
    for path, digest in config['pins'].items():
        B.equal(B.digest(path), digest, 'frozen tool/source changed: ' + path)
    B.require(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0, 'own subreaper unavailable')
    phase = config['phase']
    root = Path(config['root']) / phase
    B.write_once(root / 'STARTED.json', {'at': B.now(), 'config_sha256': B.digest(config_path), 'runner': B.proc(os.getpid())})
    run = Run(config, phase)
    try:
        run.execute()
    except Exception:
        run.record.update(status='FAIL', error=traceback.format_exc())
    finally:
        try:
            run.cleanup()
        except Exception:
            run.record.update(status='FAIL', cleanup_error=traceback.format_exc())
        B.write_once(root / 'RESULT.json', run.record)
    print(json.dumps({'phase': phase, 'status': run.record.get('status'), 'error': run.record.get('error'),
                      'cleanup': run.record.get('cleanup', {}).get('status')}))
    return 0 if run.record.get('status') == 'PASS' else 1

if __name__ == '__main__':
    if sys.argv[1] == '--self-test':
        sys.exit(self_test(sys.argv[2]))
    elif sys.argv[1] == 'producer':
        producer(*sys.argv[2:])
    else:
        sys.exit(main(sys.argv[1]))
