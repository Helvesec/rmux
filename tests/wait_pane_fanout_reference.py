#!/usr/bin/env python3
"""Bounded Linux wait-pane fanout journey; no runtime instrumentation/build.

Run once in a new --root with the source/binaries pinned by L2-013.  --self-test
only exercises discriminating fabricated evidence, without starting a product.
"""
import argparse
import signal
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

SOURCE = '4667983ea746d4794a69f5756baaf62cad59c714'
PINS = {'rmux': '1bdb6fc9c2117a6f5dae654db144115b113a5ea3bb4ca6c51c1df7130b082db6',
        'rmux-daemon': 'ea10eed3aa188542df8d19f3e8f870dbc63bd5cd500f439268cc3a23c371edeb'}
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

def ready_sample_valid(sample, waiter):
    return (sample['native']['birth_ticks'] == waiter['identity']['birth_ticks']
            and sample['native']['exe'] == waiter['identity']['exe']
            and sample['native']['state'] == 'S' and sample['wchan'] == 'hrtimer_nanosleep'
            and sample['argv'] == waiter['argv'] and sample['threads'] == [str(waiter['identity']['pid'])]
            and len([v for v in sample['fds'].values() if v.startswith('socket:[')]) == 1
            and all(sample['fds'][k] == waiter['identity']['fds'][k] for k in ['0', '1', '2']))


def check_match(result, target):
    require(result['rc'] == 0 and result['stderr'] == '', 'wait match terminal result differs')
    data = json.loads(result['stdout'])
    require(set(data) == {'schema_version', 'ok', 'condition', 'target', 'pane_exit', 'elapsed_ms'}, 'wait JSON fields differ')
    require(data == dict(schema_version=1, ok=True, condition='next-text', target=target,
                         pane_exit=None, elapsed_ms=data['elapsed_ms']), 'wait match not attributed to expected target')
    require(type(data['elapsed_ms']) is int and data['elapsed_ms'] >= 0, 'wait elapsed value invalid')
    return data


def check_timeout(result, target):
    require(result['rc'] == 1 and result['stderr'] == '', 'timeout terminal differs')
    data = json.loads(result['stdout'])
    require(data == dict(schema_version=1, ok=False, error='timeout', condition='next-text',
                         target=target, timeout_ms=1000), 'timeout JSON differs')
    require(result['elapsed_seconds'] >= 1, 'timeout returned before its real deadline')
    return data


def self_test():
    match = dict(rc=0, stderr='', stdout=json.dumps(dict(schema_version=1, ok=True,
                 condition='next-text', target='s:%0', pane_exit=None, elapsed_ms=12)))
    timeout = dict(rc=1, stderr='', elapsed_seconds=1.01, stdout=json.dumps(dict(
                   schema_version=1, ok=False, error='timeout', condition='next-text',
                   target='s:%0', timeout_ms=1000)))
    check_match(match, 's:%0'); check_timeout(timeout, 's:%0')
    invalid = [lambda: check_match(match, 's:%1'), lambda: check_timeout(match, 's:%0'),
               lambda: check_timeout(timeout, 's:%1'),
               lambda: check_timeout(dict(timeout, elapsed_seconds=0.9), 's:%0'),
               lambda: check_match(timeout, 's:%0')]
    w = {'identity': dict(pid=1, birth_ticks='10', exe='/bin/rmux', fds={'0':'null','1':'pipe:1','2':'pipe:2'}),
         'argv':['/bin/rmux','-N','wait-pane']}
    sample = dict(native=dict(birth_ticks='10', exe='/bin/rmux', state='S'), wchan='hrtimer_nanosleep',
                  argv=w['argv'], threads=['1'], fds={**w['identity']['fds'],'3':'socket:[1]'})
    require(ready_sample_valid(sample,w), 'valid readiness rejected')
    for key,value in [('wchan','unix_stream_read_generic'),('threads',['1','2']),('argv',['/bin/other'])]:
        bad=copy.deepcopy(sample);bad[key]=value
        require(not ready_sample_valid(bad,w), 'invalid readiness accepted')
    for check in invalid:
        try: check()
        except (AssertionError,ValueError): continue
        raise AssertionError('invalid fabricated result accepted')
    print(json.dumps(dict(at=utc(), positive_controls=3, negative_rejections=8, product_runs=0)))


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
        (root / 'ready.json').write_text(json.dumps({'identity': process(os.getpid()), 'argv': sys.argv, 'environment': {k: os.environ.get(k) for k in ['HOME', 'XDG_CONFIG_HOME', 'RMUX_DISABLE_TMUX_SHIM']}, 'at': utc()}))
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
        self.env.update(RMUX_DISABLE_TMUX_SHIM='1', TERM='xterm-256color', LC_ALL='C', HOME=str(self.root / 'home'), XDG_CONFIG_HOME=str(self.root / 'xdg'))

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

    def new_app(self, label, session, window, index=None):
        require(len(self.apps) < 2, 'application bound')
        folder = self.root / label
        folder.mkdir()
        os.mkfifo(folder / 'control.fifo', 0o600)
        self.apps[label] = folder
        command = ['new-session', '-d', '-s', session, '-n', window, '-x', '120', '-y', '40'] if index is None else ['new-window', '-d', '-t', f'{session}:{index}', '-n', window]
        self.command(*command, '--', '/usr/bin/python3', str(Path(__file__).resolve()), '--application', str(folder))
        self.until(lambda: (folder / 'ready.json').exists(), 'application readiness missing')
        self.command('set-window-option', '-t', session + ':' + window, 'automatic-rename', 'off')
        self.command('set-window-option', '-t', session + ':' + window, 'remain-on-exit', 'on')
        ident = self.identity(session + ':' + window + '.0')
        ready = json.loads((folder / 'ready.json').read_text())
        require(ready['identity']['pid'] == int(ident['pane_pid']) and ready['identity']['birth_ticks'] == ident['native']['birth_ticks'], 'app receipt/native identity differs')
        require(ready['environment'] == {'HOME': str(self.root / 'home'), 'XDG_CONFIG_HOME': str(self.root / 'xdg'), 'RMUX_DISABLE_TMUX_SHIM': '1'}, 'application environment mismatch')
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
        require(sum(o['p'].poll() is None for o in self.observers) < 3, 'observer concurrency bound')
        require(not self.observers, 'only one healthy reader allowed')
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
        observer['end_monotonic'] = time.monotonic()
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
            try:
                if observer.get('kind') == 'wait' and observer['p'].poll() is None:
                    self.cancel_waiter(observer)
                elif observer['p'].poll() is None:
                    rc, err = self.finish_observer(observer, close=True)
                    require(rc == 0 and not err, 'cleanup stream reader abnormal exit')
                elif 'end' not in observer:
                    self.finish_observer(observer)
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

    def emit(self, label, token):
        self.send(label, 'emit', token)
        def acknowledged():
            return any(e.get('event') == 'emitted' and e.get('token') == token
                       for e in map(json.loads, (self.apps[label] / 'events.jsonl').read_text().splitlines()))
        self.until(acknowledged, 'application emission receipt absent')

    def healthy(self, observer, reference, purpose):
        marker = self.challenge('H', observer, purpose)
        current = self.identity(reference['pane_id'])
        require(same_identity(reference, current), 'healthy pane changed')
        return marker

    def waiter(self, label, target, needle, expected_target, timeout='10s'):
        require(sum(o['p'].poll() is None for o in self.observers) < 3, 'observer concurrency bound')
        require(sum(o.get('kind') == 'wait' for o in self.observers) < 6, 'waiter count bound')
        require(timeout in ['1s', '10s'], 'unqualified timeout')
        argv = [self.binary, '-N', '-S', str(self.socket), '-f', str(self.config),
                'wait-pane', '-t', target, '--next-text', needle, '--timeout', timeout, '--json']
        launch_at = utc(); launched = time.monotonic()
        p = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        native = process(p.pid)
        require(native and digest(native['exe']) == PINS['rmux'], 'waiter executable differs')
        o = dict(label=label, kind='wait', p=p, identity=native, argv=argv, start=utc(), stdout=b'', stderr=b'',
                 expected_target=expected_target, needle=needle, launch_at=launch_at, launch_monotonic=launched, pipes={'stdout':p.stdout, 'stderr':p.stderr})
        self.observers.append(o)
        self.event('waiter_start', label=label, identity=native, argv=argv, expected_target=expected_target)
        return o

    def wait_ready(self, waiter):
        samples = []
        def ready():
            require(waiter['p'].poll() is None and not waiter['stdout'] and not waiter['stderr'], 'waiter completed before transition readiness')
            pid = waiter['identity']['pid']; root = Path('/proc') / str(pid)
            native = process(pid)
            require(native and native['birth_ticks'] == waiter['identity']['birth_ticks'], 'waiter identity lost before readiness')
            sample = dict(at=utc(), native=native, wchan=(root/'wchan').read_text().strip(),
                          argv=[x.decode() for x in (root/'cmdline').read_bytes().split(b'\0') if x],
                          threads=sorted(x.name for x in (root/'task').iterdir()),
                          fds={x.name:os.readlink(x) for x in (root/'fd').iterdir()})
            if ready_sample_valid(sample, waiter):
                samples.append(sample)
            return len(samples) >= 2
        self.until(ready, 'UNPROVEN: source-qualified waiter polling state not observed', seconds=3)
        self.event('waiter_readiness', label=waiter['label'], samples=samples, source='wait.rs:482-516; common.rs:406')
        return samples

    def waiter_result(self, waiter):
        self.finish_observer(waiter)
        result = {'rc':waiter['rc'], 'stdout':waiter['stdout'].decode(), 'stderr':waiter['stderr'].decode(),
                  'pid':waiter['identity']['pid'], 'birth_ticks':waiter['identity']['birth_ticks'],
                  'start':waiter['start'], 'end':waiter['end'], 'launch_at':waiter['launch_at'],
                  'elapsed_seconds':waiter['end_monotonic'] - waiter['launch_monotonic']}
        self.event('waiter_result', label=waiter['label'], result=result)
        return result

    def cancel_waiter(self, waiter):
        native = process(waiter['identity']['pid'])
        require(native and native['birth_ticks'] == waiter['identity']['birth_ticks'], 'cancel identity differs')
        status = dict(line.split(':',1) for line in (Path('/proc')/str(native['pid'])/'status').read_text().splitlines() if ':' in line)
        flags = {key:int(status[key].strip(),16) for key in ['SigCgt','SigIgn','SigBlk']}
        require(all(value & (1 << (signal.SIGTERM - 1)) == 0 for value in flags.values()), 'SIGTERM disposition not qualified')
        fd = os.pidfd_open(native['pid'])
        try:
            current = process(native['pid'])
            require(current and current['birth_ticks'] == native['birth_ticks'], 'cancel PID birth changed')
            self.event('owned_waiter_cancel', label=waiter['label'], identity=current, signal='SIGTERM', dispositions=flags)
            signal.pidfd_send_signal(fd, signal.SIGTERM)
        finally:
            os.close(fd)
        result = self.waiter_result(waiter)
        require(result['rc'] == -signal.SIGTERM and result['stdout'] == result['stderr'] == '', 'cancel terminal result differs')
        return result

    def run_cases(self):
        a = self.new_app('A', 'l2-f', 'output')
        h = self.new_app('H', 'l2-f', 'healthy', index=1)
        healthy = self.observe('healthy', h['pane_id'])
        self.healthy(healthy, h, 'SETUP_H')
        target = 'l2-f:' + a['pane_id']

        def finish_case(purpose):
            after = self.identity(a['pane_id'])
            require(same_identity(a, after), 'waiter transition changed application identity')
            marker = self.healthy(healthy, h, purpose)
            return dict(before=a, after=after, healthy_marker=marker)

        def pair(prefix, needle, short=False):
            first = self.waiter(prefix+'-first', a['pane_id'], needle, target, '1s' if short else '10s')
            second = self.waiter(prefix+'-second', a['pane_id'], needle, target)
            ready = {o['label']:self.wait_ready(o) for o in [first, second]}
            # Recheck both after the second readiness barrier without adding waiters.
            require(all(o['p'].poll() is None and not o['stdout'] and not o['stderr']
                        for o in [first,second]), 'pair not simultaneously pending')
            self.event('pair_ready', labels=[first['label'],second['label']], pane=a)
            return first, second, ready

        def fresh(needle):
            # The envelope is created after readiness; its matching search nonce was
            # necessarily fixed when the public wait commands were launched.
            token = 'NEW_' + needle + '_' + uuid.uuid4().hex
            self.event('fresh_challenge_created', needle=needle, token=token)
            self.emit('A',token)
            return token

        def fanout():
            needle = 'L2F_F1_' + uuid.uuid4().hex
            old = 'OLD_' + needle
            self.emit('A',old)
            capture = self.command('capture-pane','-p','-t',a['pane_id'])
            require(old in capture['stdout'], 'stale matching output not visible before subscriptions')
            self.event('stale_output_visible', token=old, needle=needle)
            first, second, ready = pair('F1',needle)
            token = fresh(needle)
            results = {o['label']:self.waiter_result(o) for o in [first,second]}
            parsed = {label:check_match(result,target) for label,result in results.items()}
            return dict(**finish_case('F1_H_AFTER'), old_marker=old, search_nonce=needle,
                        fresh_marker=token, readiness=ready, results=results, parsed=parsed)
        self.case('F1-FANOUT',fanout)

        def cancel():
            needle = 'L2F_F2_' + uuid.uuid4().hex
            first, second, ready = pair('F2',needle)
            cancelled = self.cancel_waiter(first)
            pending = self.wait_ready(second)
            self.event('sibling_pending_after_cancel', label=second['label'], pane=self.identity(a['pane_id']))
            token = fresh(needle)
            result = self.waiter_result(second); parsed = check_match(result,target)
            return dict(**finish_case('F2_H_AFTER'), search_nonce=needle, readiness=ready,
                        cancelled=cancelled, sibling_pending=pending, fresh_marker=token, result=result, parsed=parsed)
        self.case('F2-CANCEL-SIBLING',cancel)

        def timeout():
            needle = 'L2F_F3_' + uuid.uuid4().hex
            first, second, ready = pair('F3',needle,short=True)
            expired = self.waiter_result(first); timeout_json = check_timeout(expired,target)
            still_a = self.identity(a['pane_id'])
            require(same_identity(a,still_a), 'timeout coincided with lost application identity')
            pending = self.wait_ready(second)
            self.event('sibling_pending_after_timeout', label=second['label'], pane=still_a)
            token = fresh(needle)
            result = self.waiter_result(second); parsed = check_match(result,target)
            return dict(**finish_case('F3_H_AFTER'), search_nonce=needle, readiness=ready,
                        expired=expired, timeout_json=timeout_json, sibling_pending=pending,
                        fresh_marker=token, result=result, parsed=parsed)
        self.case('F3-TIMEOUT-SIBLING',timeout)


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
