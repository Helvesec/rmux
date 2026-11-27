#!/usr/bin/env python3
"""Bounded Linux public-stream identity journey; no runtime instrumentation/build.

Run once in a new --root with the source/binaries pinned by L2-009.  --self-test
only exercises discriminating fabricated evidence, without starting a product.
"""
import argparse
import base64
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


def validate_rename(a, b, delivered, healthy):
    require(same_identity(a, b), 'rename replaced identity')
    require(b['session_name'] == 'l2-k-r' and b['window_name'] == 'renamed', 'rename names not committed')
    require(delivered and healthy, 'rename lost target or healthy output')


def validate_recreate(a, b, old_output, forbidden, rc, stderr, delivered, healthy):
    require(a['pane_id'] != b['pane_id'], 'recreation reused stable pane identity')
    require((a['pane_pid'], a['native']['birth_ticks']) != (b['pane_pid'], b['native']['birth_ticks']), 'recreation reused native identity')
    require(all(a[x] == b[x] for x in ['session_name', 'window_name', 'window_index', 'pane_index']), 'recreation did not reuse the requested name/index')
    require(forbidden not in old_output, 'old observer received replacement data')
    require((rc, stderr) in [(0, b''), (1, b'subscription not found\n')], 'old observer unexpected terminal result')
    require(delivered and healthy, 'recreation lost replacement or healthy output')


def validate_cancel(a, b, rc, stderr, delivered, healthy):
    require(rc == 0 and stderr == b'', 'reader cancellation was not normal')
    require(same_identity(a, b), 'cancellation changed pane identity')
    require(delivered and healthy, 'cancellation impaired a healthy reader')


def self_test():
    a = {x: '1' for x in FIELDS}
    a.update(session_name='l2-k-r', window_name='renamed', native={'birth_ticks': '10'})
    b = copy.deepcopy(a)
    b.update(pane_id='%2', pane_pid='20', native={'birth_ticks': '20'})
    validate_rename(a, a, True, True)
    validate_recreate(a, b, b'A', b'B', 0, b'', True, True)
    validate_recreate(a, b, b'A', b'B', 1, b'subscription not found\n', True, True)
    validate_cancel(b, b, 0, b'', True, True)
    bad = copy.deepcopy(a); bad['pane_generation'] = '999'
    attacks = [lambda: validate_rename(a, bad, True, True),
               lambda: validate_rename(a, a, False, True),
               lambda: validate_recreate(a, a, b'A', b'B', 0, b'', True, True),
               lambda: validate_recreate(a, b, b'A-B', b'B', 0, b'', True, True),
               lambda: validate_recreate(a, b, b'A', b'B', 1, b'access denied\n', True, True),
               lambda: validate_cancel(b, b, -15, b'', True, True),
               lambda: validate_cancel(b, b, 0, b'', True, False)]
    for attack in attacks:
        try:
            attack()
        except AssertionError:
            continue
        raise AssertionError('fabricated invalid evidence accepted')
    print(json.dumps({'at': utc(), 'positive_controls': 4, 'discriminating_rejections': len(attacks), 'product_runs': 0}))


def application(root):
    """Only emits bounded tokens requested over its private FIFO, then exits."""
    root = Path(root)
    fd = os.open(root / 'control.fifo', os.O_RDWR | os.O_NONBLOCK)
    pending = b''
    end = time.monotonic() + 150
    count = 0
    with (root / 'events.jsonl').open('a', buffering=1) as log:
        def record(value):
            log.write(json.dumps({'at': utc(), **value}) + '\n')
        (root / 'ready.json').write_text(json.dumps({'identity': process(os.getpid()), 'argv': sys.argv, 'at': utc()}))
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
                        record({'event': 'normal_exit', 'pid': os.getpid()})
                        return
                    token = request['token']
                    require(request['op'] == 'emit' and len(token) <= 160 and token.isascii() and '\n' not in token, 'invalid fixture token')
                    count += 1
                    os.write(1, (token + '\n').encode())
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
                   if (pipe := o['pipes'].get(key)) is not None and not pipe.closed}
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
        require(len(self.observers) < 8, 'observer total bound')
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

    def healthy(self, observer, reference, purpose):
        token = self.challenge('H', observer, purpose)
        current = self.identity('l2-k-healthy:healthy.0')
        require(same_identity(reference, current), 'healthy client pane identity changed')
        return token

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

    def run_cases(self):
        h = self.new_app('H', 'l2-k-healthy', 'healthy')
        a = self.new_app('A', 'l2-k-a', 'origin')
        oh = self.observe('healthy', 'l2-k-healthy:healthy.0')
        oa = self.observe('original-A', 'l2-k-a:origin.0')
        self.challenge('H', oh, 'SETUP_H')
        state = {'a': a}
        def rename():
            self.challenge('A', oa, 'K1_BARRIER')
            self.healthy(oh, h, 'K1_H_BEFORE')
            self.command('rename-session', '-t', 'l2-k-a', 'l2-k-r')
            self.command('rename-window', '-t', 'l2-k-r:0', 'renamed')
            after = self.identity('l2-k-r:renamed.0')
            token = self.challenge('A', oa, 'K1_AFTER')
            healthy = self.healthy(oh, h, 'K1_H_AFTER')
            names = self.command('list-sessions', '-F', '#{session_name}')['stdout'].splitlines()
            require('l2-k-a' not in names and 'l2-k-r' in names, 'old session still resolves')
            old = self.command('has-session', '-t', 'l2-k-a', expected=1)
            require(old['stdout'] == '' and old['stderr'] == "can't find session: l2-k-a\n", 'old session absence not established by exact has-session diagnostic')
            validate_rename(a, after, token.encode() in oa['stdout'], bool(healthy))
            state['a'] = after
            return {'before': a, 'after': after, 'marker': token, 'healthy_marker': healthy, 'old_target': old}
        self.case('K1-RENAME', rename)
        def recreate():
            self.challenge('A', oa, 'K2_BARRIER')
            self.healthy(oh, h, 'K2_H_BEFORE')
            self.command('kill-pane', '-t', state['a']['pane_id'])
            b = self.new_app('B', 'l2-k-r', 'renamed')
            forbidden = 'L2K_B_ONLY_' + uuid.uuid4().hex
            self.send('B', 'emit', forbidden)
            rc, err = self.finish_observer(oa)
            self.until(lambda: process(int(a['pane_pid'])) is None, 'removed A process still present')
            ob = self.observe('replacement-B', 'l2-k-r:renamed.0')
            token = self.challenge('B', ob, 'K2_B_FRESH')
            self.until(lambda: forbidden.encode() in ob['stdout'], 'B did not emit forbidden-to-A marker')
            healthy = self.healthy(oh, h, 'K2_H_AFTER')
            validate_recreate(state['a'], b, oa['stdout'], forbidden.encode(), rc, err, bool(token), bool(healthy))
            state.update(b=b, ob=ob, retained=token)
            return {'before': state['a'], 'after': b, 'forbidden_to_A': forbidden, 'old_rc': rc,
                    'old_stderr': err.decode(), 'new_marker': token, 'healthy_marker': healthy}
        self.case('K2-RECREATE', recreate)
        def cancel():
            b, ob = state['b'], state['ob']
            self.challenge('B', ob, 'K3_BARRIER')
            self.healthy(oh, h, 'K3_H_BEFORE')
            rc, err = self.finish_observer(ob, close=True)
            retained = 'L2K_PRESERVED_' + uuid.uuid4().hex
            self.send('B', 'emit', retained)
            fresh = self.observe('new-B-after-cancel', 'l2-k-r:renamed.0')
            token = self.challenge('B', fresh, 'K3_FRESH')
            healthy = self.healthy(oh, h, 'K3_H_AFTER')
            after = self.identity('l2-k-r:renamed.0')
            capture = self.command('capture-pane', '-p', '-t', b['pane_id'])['stdout']
            require(state['retained'] in capture and retained in capture and token in capture, 'B data not retained')
            validate_cancel(b, after, rc, err, token.encode() in fresh['stdout'], bool(healthy))
            return {'before': b, 'after': after, 'cancel_rc': rc, 'cancel_stderr': err.decode(),
                    'new_marker': token, 'preserved_marker': retained, 'healthy_marker': healthy, 'capture': capture}
        self.case('K3-CANCEL', cancel)

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
