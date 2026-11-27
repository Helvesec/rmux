#!/usr/bin/env python3
"""Bounded Linux public slow-reader isolation; no runtime instrumentation/build.

Run once in a new --root with the source/binaries pinned by L2-011.  --self-test
only exercises discriminating fabricated evidence, without starting a product.
"""
import argparse
import array
import fcntl
import termios
import copy
import ctypes
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time
import traceback
import uuid
from datetime import datetime, timezone

SOURCE = '7069540a0ff0ed8d9ddc8c3607120ac81114aaa8'
PINS = {'rmux': '19e3befa7c03bfb948d39754b574d0da44b94b4d8980a18ef37d06f051abbac4',
        'rmux-daemon': '583810735bcaca378fc6ee9af579fd5e3f32486867adda9cbe0059579f852a1c'}
FIELDS = ['session_id', 'session_name', 'window_id', 'window_index', 'window_name',
          'pane_id', 'pane_index', 'pane_pid', 'pane_generation', 'pane_tty', 'pid', 'socket_path']
FORMAT = '|'.join('#{' + x + '}' for x in FIELDS)
STABLE = ['session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_generation', 'pane_tty', 'pid', 'socket_path']
LIMIT = 16 * 1024 * 1024


BURST_BYTES = 262144
CHUNK_BYTES = 4096
CHUNK_PAUSE = 0.005
PAYLOAD_LIMIT = 8 * 1024 * 1024

def utc():
    return datetime.now(timezone.utc).isoformat()


def digest(path):
    with open(path, 'rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def process(pid):
    root = Path('/proc') / str(pid)
    try:
        stat = (root / 'stat').read_text().rsplit(')', 1)[1].split()
        out = {'pid': pid, 'birth_ticks': stat[19], 'ppid': int(stat[1]), 'state': stat[0], 'at': utc()}
        if stat[0] != 'Z':
            out['exe'] = os.readlink(root / 'exe')
            out['fds'] = {str(n): os.readlink(root / 'fd' / str(n)) for n in (0, 1, 2)}
        end = (root / 'stat').read_text().rsplit(')', 1)[1].split()
        require(end[19] == stat[19], 'PID changed birth during owned identity read')
        return out
    except FileNotFoundError:
        return None


def own_fds():
    result = {}
    for path in Path('/proc/self/fd').iterdir():
        try:
            result[path.name] = os.readlink(path)
        except FileNotFoundError:
            pass
    return result


def same_identity(a, b):
    return all(a[x] == b[x] for x in STABLE) and a['native']['birth_ticks'] == b['native']['birth_ticks']

def validate_blocked(s, baseline):
    require(s['capacity'] == 4096 and s['pending'] == s['capacity'], 'owned pipe not full')
    require(s['paused'] and s['read_bytes'] == baseline, 'slow stdout consumed while paused')
    require(s['rc'] is None and s['stderr_bytes'] == 0, 'slow reader not live and healthy')
    require(s['identity_matches'] and s['reader_stdout'] == s['owned_read_pipe'], 'owned pipe identity mismatch')


def validate_progress(same, delivered, seconds):
    require(same and delivered, 'healthy progress or pane identity failed')
    require(0 <= seconds <= 2, 'public command exceeded two seconds')


def self_test():
    s = dict(capacity=4096, pending=4096, paused=True, read_bytes=80, rc=None,
             stderr_bytes=0, identity_matches=True, reader_stdout='pipe:[1]', owned_read_pipe='pipe:[1]')
    validate_blocked(s, 80)
    validate_progress(True, True, 0.1)
    invalid = []
    for key, value in [('pending', 4095), ('read_bytes', 81), ('identity_matches', False),
                       ('reader_stdout', 'pipe:[2]'), ('rc', 0)]:
        changed = dict(s); changed[key] = value
        invalid.append(lambda sample=changed: validate_blocked(sample, 80))
    invalid += [lambda: validate_progress(True, False, 0.1),
                lambda: validate_progress(True, True, 2.1)]
    for check in invalid:
        try:
            check()
        except AssertionError:
            continue
        raise AssertionError('invalid fabricated evidence accepted')
    print(json.dumps({'at': utc(), 'positive_controls': 2, 'negative_rejections': len(invalid), 'product_runs': 0}))


def application(root):
    """Only emits bounded tokens requested over its private FIFO, then exits."""
    root = Path(root)
    fd = os.open(root / 'control.fifo', os.O_RDWR | os.O_NONBLOCK)
    pending = b''
    end = time.monotonic() + 150
    count = total = bursts = 0
    with (root / 'events.jsonl').open('a', buffering=1) as log:
        def record(value):
            log.write(json.dumps({'at': utc(), **value}) + '\n')
        def write_all(data, purpose):
            nonlocal total
            require(len(data) <= 2 * 1024 * 1024 and total + len(data) <= PAYLOAD_LIMIT, 'application payload bound')
            sent = 0
            while sent < len(data):
                n = os.write(1, data[sent:])
                require(n > 0, 'application output made no progress')
                sent += n; total += n
                record({'event': 'write', 'purpose': purpose, 'bytes': n, 'total_bytes': total})
        (root / 'ready.json').write_text(json.dumps({'identity': process(os.getpid()), 'argv': sys.argv, 'environment': {k: os.environ.get(k) for k in ['HOME', 'XDG_CONFIG_HOME']}, 'at': utc()}))
        try:
            while time.monotonic() < end and count < 80:
                if not select.select([fd], [], [], min(1, max(0, end - time.monotonic())))[0]:
                    continue
                pending += os.read(fd, 8192)
                require(len(pending) <= 8192, 'application input exceeded bound')
                while b'\n' in pending:
                    line, pending = pending.split(b'\n', 1)
                    request = json.loads(line)
                    if request['op'] == 'exit':
                        record({'event': 'normal_exit', 'pid': os.getpid(), 'total_bytes': total})
                        return
                    token = request['token']
                    require(len(token) <= 160 and token.isascii() and '\n' not in token, 'invalid token')
                    if request['op'] == 'burst':
                        require(bursts == 0, 'one fixed burst only')
                        bursts += 1; count += 1
                        record({'event': 'burst_start', 'token': token, 'bytes': BURST_BYTES,
                                'chunk_bytes': CHUNK_BYTES, 'inter_chunk_sleep_seconds': CHUNK_PAUSE})
                        for _ in range(BURST_BYTES // CHUNK_BYTES):
                            write_all(b'Q' * (CHUNK_BYTES - 1) + b'\n', token)
                            time.sleep(CHUNK_PAUSE)
                        record({'event': 'burst_complete', 'token': token, 'bytes': BURST_BYTES, 'total_bytes': total})
                        continue
                    require(request['op'] == 'emit' and len(token) <= 160 and token.isascii() and '\n' not in token, 'invalid fixture token')
                    count += 1
                    write_all((token + '\n').encode(), token)
                    record({'event': 'emitted', 'token': token, 'pid': os.getpid(), 'count': count})
            record({'event': 'deadline_exit', 'pid': os.getpid()})
        finally:
            os.close(fd)


class Journey:
    def __init__(self, binary, root):
        self.binary = str(Path(binary).resolve())
        self.root = Path(root).resolve()
        self.socket = self.root / 'server.sock'
        self.config = self.root / 'empty.conf'
        self.observers = []
        self.apps = {}
        self.identities = []
        self.daemon = None
        self.started = time.monotonic()
        self.deadline = self.started + 180
        self.stage_deadline = self.started + 30
        self.report = {'source': SOURCE, 'start': utc(), 'cases': [], 'commands': [], 'events': [], 'pass': False}
        self.env = {k: os.environ[k] for k in ['PATH', 'HOME', 'USER', 'LANG'] if k in os.environ}
        self.env.update(TERM='xterm-256color', LC_ALL='C', HOME=str(self.root / 'home'), XDG_CONFIG_HOME=str(self.root / 'xdg'))

    def event(self, name, **data):
        self.report['events'].append({'at': utc(), 'monotonic': time.monotonic(), 'event': name, **data})

    def remaining(self, ceiling=5):
        seconds = min(ceiling, self.deadline - time.monotonic(), self.stage_deadline - time.monotonic())
        require(seconds > 0, 'bounded stage deadline exceeded')
        return seconds

    def command(self, *args, expected=0):
        argv = [self.binary, '-S', str(self.socket), '-f', str(self.config), *args]
        start = utc()
        p = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        out, err = p.communicate(timeout=self.remaining())
        row = {'start': start, 'end': utc(), 'argv': argv, 'pid': p.pid, 'rc': p.returncode,
               'stdout': out.decode(errors='replace'), 'stderr': err.decode(errors='replace')}
        self.report['commands'].append(row)
        require(len(out) + len(err) < LIMIT, 'command log bound')
        if expected is not None:
            require(p.returncode == expected, 'command rc mismatch: ' + json.dumps(row))
        return row

    def pump(self, duration=0.025):
        streams = {pipe: (o, key) for o in self.observers for key in ['stdout', 'stderr']
                   if not (key == 'stdout' and o.get('paused', False))
                   and (pipe := o['pipes'].get(key)) is not None and not pipe.closed}
        if not streams:
            time.sleep(min(duration, self.remaining()))
            return
        ready = select.select(list(streams), [], [], min(duration, self.remaining()))[0]
        for pipe in ready:
            o, key = streams[pipe]
            chunk = os.read(pipe.fileno(), 65536)
            if chunk:
                o[key] += chunk
                require(sum(len(x['stdout']) + len(x['stderr']) for x in self.observers) < LIMIT, 'stream logs exceeded bound')
            else:
                pipe.close()
                o['pipes'][key] = None

    def until(self, condition, message, seconds=5):
        end = time.monotonic() + self.remaining(seconds)
        while not condition():
            require(time.monotonic() < end, message)
            self.pump()

    def identity(self, target):
        row = self.command('display-message', '-p', '-t', target, FORMAT)
        parts = row['stdout'].strip().split('|')
        require(len(parts) == len(FIELDS), 'identity format unavailable')
        ident = dict(zip(FIELDS, parts))
        require(all(ident[k] for k in FIELDS), 'identity field unavailable')
        require(ident['socket_path'] == str(self.socket), 'endpoint identity mismatch')
        require(ident['pane_id'].startswith('%') and ident['pane_generation'].isdigit(), 'stable ID/generation unavailable')
        ident['native'] = process(int(ident['pane_pid']))
        require(ident['native'] and ident['native']['state'] != 'Z', 'application process not alive')
        require(ident['native']['fds']['0'] == ident['pane_tty'], 'native/public PTY differs')
        require(ident['native']['exe'] == str(Path('/usr/bin/python3').resolve()), 'application exe mismatch')
        self.event('identity', target=target, identity=ident)
        return ident

    def new_app(self, label, session, window):
        folder = self.root / label
        folder.mkdir()
        os.mkfifo(folder / 'control.fifo', 0o600)
        self.apps[label] = folder
        self.command('new-session', '-d', '-s', session, '-n', window, '-x', '120', '-y', '40', '--',
                     '/usr/bin/python3', str(Path(__file__).resolve()), '--application', str(folder))
        self.until(lambda: (folder / 'ready.json').exists(), 'application readiness missing')
        self.command('set-window-option', '-t', session + ':0', 'automatic-rename', 'off')
        self.command('set-window-option', '-t', session + ':0', 'remain-on-exit', 'on')
        ident = self.identity(session + ':0.0')
        ready = json.loads((folder / 'ready.json').read_text())
        require(ready['identity']['pid'] == int(ident['pane_pid']) and ready['identity']['birth_ticks'] == ident['native']['birth_ticks'], 'app receipt/native identity differs')
        self.identities.append(ident['native'])
        if self.daemon is None:
            self.daemon = process(int(ident['pid']))
            require(self.daemon and digest(self.daemon['exe']) == PINS['rmux-daemon'], 'wrong daemon executable')
            self.report['daemon'] = self.daemon
        require(ident['native']['ppid'] == self.daemon['pid'], 'pane child has unqualified parent')
        return ident

    def send(self, label, op, token=None):
        request = {'op': op}
        if token is not None:
            request['token'] = token
        fd = os.open(self.apps[label] / 'control.fifo', os.O_WRONLY | os.O_NONBLOCK)
        try:
            os.write(fd, (json.dumps(request) + '\n').encode())
        finally:
            os.close(fd)
        self.event('application_request', label=label, request=request)

    def observe(self, label, target):
        require(sum(o['p'].poll() is None for o in self.observers) < 2, 'observer concurrency bound')
        require(len(self.observers) < 6, 'observer total bound')
        argv = [self.binary, '-S', str(self.socket), '-f', str(self.config), 'stream-pane', '-t', target, '--raw']
        p = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        native = process(p.pid)
        require(native and digest(native['exe']) == PINS['rmux'], 'observer binary mismatch')
        o = {'label': label, 'p': p, 'argv': argv, 'identity': native, 'start': utc(), 'stdout': b'', 'stderr': b'',
             'pipes': {'stdout': p.stdout, 'stderr': p.stderr}}
        self.observers.append(o)
        self.event('observer_start', label=label, argv=argv, identity=native)
        return o

    def challenge(self, label, observer, purpose):
        token = 'L2K_' + purpose + '_' + uuid.uuid4().hex
        self.send(label, 'emit', token)
        self.until(lambda: token.encode() in observer['stdout'], 'fresh challenge not delivered: ' + purpose)
        require(observer['p'].poll() is None and not observer['stderr'], 'observer not healthy after challenge')
        self.event('readiness_barrier', observer=observer['label'], app=label, token=token)
        return token

    def finish_observer(self, observer, close=False):
        if close and observer['pipes']['stdout'] is not None:
            observer['pipes']['stdout'].close()
            observer['pipes']['stdout'] = None
            self.event('reader_pipe_closed', observer=observer['label'])
        self.until(lambda: observer['p'].poll() is not None, 'observer did not exit normally')
        while any(observer['pipes'].values()):
            self.pump()
        observer['rc'] = observer['p'].wait(timeout=self.remaining())
        observer['end'] = utc()
        require(process(observer['identity']['pid']) is None, 'observer still present after wait')
        self.event('observer_exit', observer=observer['label'], rc=observer['rc'], stderr=observer['stderr'].decode(errors='replace'))
        return observer['rc'], observer['stderr']

    def case(self, name, action):
        self.stage_deadline = time.monotonic() + 20
        started = time.monotonic()
        row = {'id': name, 'start': utc(), 'pass': False}
        self.report['cases'].append(row)
        try:
            row.update(action())
            row['pass'] = True
        finally:
            row['end'] = utc(); row['seconds'] = time.monotonic() - started

    def cleanup(self):
        self.stage_deadline = min(self.deadline, time.monotonic() + 30)
        out = {'start': utc(), 'pass': False, 'errors': []}
        self.report['cleanup'] = out
        for observer in self.observers:
            if observer['p'].poll() is None:
                try:
                    rc, err = self.finish_observer(observer, close=True)
                    require(rc == 0 and not err, 'cleanup reader abnormal exit')
                except Exception as e:
                    out['errors'].append(str(e))
        for label, folder in self.apps.items():
            try:
                ready = json.loads((folder / 'ready.json').read_text())
                native = process(ready['identity']['pid'])
                if native and native['birth_ticks'] == ready['identity']['birth_ticks']:
                    self.send(label, 'exit')
                    self.until(lambda: process(native['pid']) is None, 'application normal exit not observed')
                    events = [json.loads(x) for x in (folder / 'events.jsonl').read_text().splitlines()]
                    require(any(x['event'] == 'normal_exit' for x in events), 'application normal exit receipt missing')
            except Exception as e:
                out['errors'].append(str(e))
        if self.daemon:
            try:
                current = process(self.daemon['pid'])
                require(current and current['birth_ticks'] == self.daemon['birth_ticks'], 'own daemon identity changed before cleanup')
                require(digest(current['exe']) == PINS['rmux-daemon'], 'daemon hash changed before cleanup')
                self.command('kill-server')
                def daemon_gone():
                    try:
                        os.waitpid(self.daemon['pid'], os.WNOHANG)
                    except ChildProcessError:
                        pass
                    return process(self.daemon['pid']) is None
                self.until(daemon_gone, 'daemon not absent/reaped')
                require(not self.socket.exists(), 'private socket remains')
            except Exception as e:
                out['errors'].append(str(e))
        out['tracked_remaining'] = [p for i in self.identities for p in [process(i['pid'])]
                                    if p and p['birth_ticks'] == i['birth_ticks']]
        out['socket_absent'] = not self.socket.exists()
        out['end'] = utc()
        out['pass'] = not out['errors'] and not out['tracked_remaining'] and out['socket_absent']

    def execute(self):
        require(not self.root.exists(), 'new run root required; native retry forbidden')
        self.root.mkdir(parents=True)
        self.config.write_text('')
        (self.root / 'home').mkdir()
        (self.root / 'xdg').mkdir()
        (self.root / 'LOT-STARTED.json').write_text(json.dumps({'at': utc(), 'pid': os.getpid(), 'source': SOURCE}))
        require(sys.platform == 'linux', 'native Linux only')
        require(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0, 'cannot establish own child subreaper')
        self.report['baseline_fds'] = own_fds()
        self.report['binary_pins'] = {name: digest(Path(self.binary).with_name(name)) for name in PINS}
        require(self.report['binary_pins'] == PINS, 'binary pin mismatch')
        self.report['boot_id'] = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        self.report['driver_sha256'] = digest(__file__)
        try:
            self.run_cases()
        except Exception:
            self.report['failure'] = traceback.format_exc()
        finally:
            try:
                self.cleanup()
            except Exception:
                self.report['cleanup_fatal'] = traceback.format_exc()
            for observer in self.observers:
                for stream in observer['pipes'].values():
                    if stream is not None and not stream.closed:
                        stream.close()
                for key in ['stdout', 'stderr']:
                    (self.root / (observer['label'] + '.' + key + '.bin')).write_bytes(observer[key])
            self.report['observers'] = [{k: v for k, v in o.items() if k not in ['p', 'pipes', 'stdout', 'stderr']} |
                                        {'rc': o['p'].poll(), 'stdout_bytes': len(o['stdout']), 'stderr_bytes': len(o['stderr'])}
                                        for o in self.observers]
            self.report['final_fds'] = own_fds()
            self.report['fd_restored'] = self.report['final_fds'] == self.report['baseline_fds']
            self.report['end'] = utc(); self.report['seconds'] = time.monotonic() - self.started
            self.report['pass'] = (len(self.report['cases']) == 3 and all(c['pass'] for c in self.report['cases'])
                                   and self.report.get('cleanup', {}).get('pass', False) and self.report['fd_restored']
                                   and self.report['seconds'] <= 180 and 'failure' not in self.report)
            (self.root / 'RESULT.json').write_text(json.dumps(self.report, indent=2) + '\n')
        print(json.dumps({'pass': self.report['pass'], 'cases': [(x['id'], x['pass']) for x in self.report['cases']],
                          'seconds': self.report['seconds'], 'result': str(self.root / 'RESULT.json'),
                          'failure': self.report.get('failure'), 'cleanup': self.report.get('cleanup')}))
        return 0 if self.report['pass'] else 1

    def pipe_snapshot(self, observer, purpose):
        pipe = observer['pipes']['stdout']
        require(pipe is not None and not pipe.closed, 'owned pipe descriptor unavailable')
        pending = array.array('i', [0])
        fcntl.ioctl(pipe.fileno(), termios.FIONREAD, pending, True)
        identity = process(observer['identity']['pid'])
        sample = {'at': utc(), 'capacity': fcntl.fcntl(pipe.fileno(), fcntl.F_GETPIPE_SZ),
                  'pending': pending[0], 'read_bytes': len(observer['stdout']), 'paused': observer.get('paused', False),
                  'rc': observer['p'].poll(), 'stderr_bytes': len(observer['stderr']),
                  'identity': identity, 'identity_matches': bool(identity and identity['birth_ticks'] == observer['identity']['birth_ticks']),
                  'reader_stdout': identity.get('fds', {}).get('1') if identity else None,
                  'owned_read_pipe': os.readlink('/proc/self/fd/' + str(pipe.fileno()))}
        if identity:
            sample['wchan'] = (Path('/proc') / str(identity['pid']) / 'wchan').read_text().strip()
        if purpose:
            self.event('owned_pipe_snapshot', purpose=purpose, sample=sample)
        return sample

    def app_events(self):
        return [json.loads(x) for x in (self.apps['A'] / 'events.jsonl').read_text().splitlines()]

    def run_cases(self):
        state = {}
        def fill():
            reference = self.new_app('A', 'l2-q', 'output')
            ready = json.loads((self.apps['A'] / 'ready.json').read_text())
            require(ready['environment'] == {'HOME': str(self.root / 'home'), 'XDG_CONFIG_HOME': str(self.root / 'xdg')}, 'application environment not private')
            healthy = self.observe('healthy', 'l2-q:output.0')
            slow = self.observe('slow', 'l2-q:output.0')
            capacity = fcntl.fcntl(slow['pipes']['stdout'].fileno(), fcntl.F_SETPIPE_SZ, 4096)
            require(capacity == 4096, 'private pipe size unavailable')
            self.challenge('A', healthy, 'Q1_HEALTHY_READY')
            self.challenge('A', slow, 'Q1_SLOW_READY')
            before = self.pipe_snapshot(slow, 'before_pause')
            baseline = len(slow['stdout']); slow['paused'] = True
            self.event('slow_stdout_paused', observer=slow['label'], read_bytes=baseline)
            token = 'L2Q_BURST_' + uuid.uuid4().hex
            self.send('A', 'burst', token)
            self.until(lambda: self.pipe_snapshot(slow, None)['pending'] == capacity,
                       'UNPROVEN: fixed burst did not fill owned pipe', seconds=5)
            first = self.pipe_snapshot(slow, 'first_full')
            validate_blocked(first, baseline)
            self.until(lambda: any(e['event'] == 'burst_complete' and e['token'] == token for e in self.app_events()),
                       'application fixed burst did not finish', seconds=5)
            full = self.pipe_snapshot(slow, 'full_after_burst')
            validate_blocked(full, baseline)
            emitted = sum(e['bytes'] for e in self.app_events() if e['event'] == 'write' and e['purpose'] == token)
            require(emitted == BURST_BYTES, 'burst write accounting mismatch')
            state.update(reference=reference, slow=slow, healthy=healthy, baseline=baseline)
            return {'identity': reference, 'before_pause': before, 'first_full': first, 'full_after_burst': full,
                    'burst_token': token, 'burst_bytes_emitted': emitted, 'healthy_bytes_read': len(healthy['stdout'])}
        self.case('Q1-PIPE-FULL', fill)

        def progress():
            slow, healthy = state['slow'], state['healthy']
            before = self.pipe_snapshot(slow, 'Q2_before'); validate_blocked(before, state['baseline'])
            marker = self.challenge('A', healthy, 'Q2_HEALTHY_FRESH')
            old_deadline = self.stage_deadline
            self.stage_deadline = min(old_deadline, time.monotonic() + 2)
            start = time.monotonic()
            try:
                after = self.identity('l2-q:output.0')
            finally:
                self.stage_deadline = old_deadline
            seconds = time.monotonic() - start
            command = self.report['commands'][-1]
            require(command['rc'] == 0 and command['stderr'] == '', 'independent command abnormal result')
            blocked = self.pipe_snapshot(slow, 'Q2_after'); validate_blocked(blocked, state['baseline'])
            validate_progress(same_identity(state['reference'], after), marker.encode() in healthy['stdout'], seconds)
            return {'before_pipe': before, 'after_pipe': blocked, 'healthy_marker': marker,
                    'public_command': command, 'public_command_seconds': seconds, 'identity': after,
                    'healthy_bytes_read': len(healthy['stdout']), 'slow_bytes_read': len(slow['stdout'])}
        self.case('Q2-HEALTHY-PROGRESS', progress)

        def replace():
            slow, healthy = state['slow'], state['healthy']
            before = self.pipe_snapshot(slow, 'Q3_before_close'); validate_blocked(before, state['baseline'])
            rc, err = self.finish_observer(slow, close=True)
            require(rc == 0 and err == b'', 'slow close not normal')
            fresh = self.observe('replacement', 'l2-q:output.0')
            marker = self.challenge('A', fresh, 'Q3_REPLACEMENT_FRESH')
            healthy_marker = self.challenge('A', healthy, 'Q3_HEALTHY_FRESH')
            after = self.identity('l2-q:output.0')
            require(same_identity(state['reference'], after), 'replacement changed pane identity')
            require(marker.encode() in fresh['stdout'] and healthy_marker.encode() in healthy['stdout'], 'replacement/healthy marker absent')
            return {'before_close_pipe': before, 'cancel_rc': rc, 'cancel_stderr': err.decode(),
                    'replacement_marker': marker, 'healthy_marker': healthy_marker, 'identity': after,
                    'healthy_bytes_read': len(healthy['stdout']), 'replacement_bytes_read': len(fresh['stdout'])}
        self.case('Q3-REPLACE-SLOW', replace)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--application')
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--rmux')
    parser.add_argument('--root')
    args = parser.parse_args()
    if args.application:
        application(args.application)
        return 0
    if args.self_test:
        self_test()
        return 0
    require(args.rmux and args.root, '--rmux and --root required')
    return Journey(args.rmux, args.root).execute()


if __name__ == '__main__':
    sys.exit(main())
