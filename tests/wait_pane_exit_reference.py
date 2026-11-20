#!/usr/bin/env python3
"""Bounded Linux wait-pane exit-metadata journey; no runtime instrumentation/build.

Run once in a new --root with the source/binaries pinned by L2-014.  --self-test
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


def check_exit_result(result, target, expected):
    require(result['rc'] == 0 and result['stderr'] == '', 'pane-exit CLI must complete rc0')
    data = json.loads(result['stdout'])
    require(set(data) == {'schema_version','ok','condition','target','pane_exit','elapsed_ms'}, 'wrong exit schema')
    require(type(data['schema_version']) is int and data['schema_version'] == 1 and data['ok'] is True,
            'invalid schema/version/success')
    require(data['condition'] == 'pane-exit' and data['target'] == target, 'wrong condition or stable target')
    require(type(data['elapsed_ms']) is int and data['elapsed_ms'] >= 0, 'invalid elapsed time')
    metadata = data['pane_exit']
    require(type(metadata) is dict and set(metadata) == {'stale','exit_status','exit_signal'}, 'invalid metadata shape')
    require(type(metadata['stale']) is bool, 'stale is not a Boolean')
    require(all(metadata[k] is None or type(metadata[k]) is int for k in ['exit_status','exit_signal']), 'invalid numeric metadata')
    allowed = [dict(stale=False,exit_status=7,exit_signal=None)] if expected == 'normal' else [
        dict(stale=False,exit_status=None,exit_signal=15)] if expected == 'signal' else [
        dict(stale=False,exit_status=7,exit_signal=None), dict(stale=True,exit_status=None,exit_signal=None)]
    require(metadata in allowed, 'exit metadata does not match prefrozen alternatives')
    return data


def check_intent(intent, nonce, identity, issued_at, kind):
    require(intent['event'] == kind and intent['token'] == nonce and intent['at'] >= issued_at,
            'stale or wrong exit intent')
    require(intent['identity']['pid'] == identity['pid'] and
            intent['identity']['birth_ticks'] == identity['birth_ticks'], 'intent identity mismatch')


def cleanup_valid(report):
    cleanup = report.get('cleanup', {})
    return (cleanup.get('pass') is True and cleanup.get('socket_absent') is True
            and cleanup.get('errors') == [] and cleanup.get('tracked_remaining') == []
            and report.get('fd_restored') is True)


def self_test():
    def result(metadata):
        return dict(rc=0,stderr='',stdout=json.dumps(dict(schema_version=1,ok=True,condition='pane-exit',
                    target='s:%0',elapsed_ms=1,pane_exit=metadata)))
    normal=result(dict(stale=False,exit_status=7,exit_signal=None))
    signal_exit=result(dict(stale=False,exit_status=None,exit_signal=15))
    stale=result(dict(stale=True,exit_status=None,exit_signal=None))
    for value,kind in [(normal,'normal'),(signal_exit,'signal'),(stale,'removed')]:
        check_exit_result(value,'s:%0',kind)
    ident=dict(pid=1,birth_ticks='9')
    intent=dict(event='exit_intent',token='fresh',at='2026-02',identity=ident)
    check_intent(intent,'fresh',ident,'2026-01','exit_intent')
    valid=dict(cleanup=dict(pass_=True,socket_absent=True,errors=[],tracked_remaining=[]),fd_restored=True)
    valid['cleanup']['pass']=valid['cleanup'].pop('pass_')
    require(cleanup_valid(valid),'valid cleanup rejected')
    bad_schema=json.loads(normal['stdout']);bad_schema['schema_version']=2
    invalid=[lambda:check_exit_result(dict(normal,stdout=json.dumps(bad_schema)),'s:%0','normal'),
             lambda:check_exit_result(stale,'s:%0','normal'),
             lambda:check_exit_result(normal,'s:%0','signal'),
             lambda:check_exit_result(signal_exit,'s:%0','normal'),
             lambda:check_exit_result(dict(normal,rc=7),'s:%0','normal'),
             lambda:check_exit_result(normal,'s:%1','normal'),
             lambda:check_intent(intent,'old',ident,'2026-01','exit_intent')]
    for check in invalid:
        try:check()
        except (AssertionError,ValueError):continue
        raise AssertionError('invalid fabricated evidence accepted')
    require(not cleanup_valid({'fd_restored':True}),'missing cleanup accepted')
    print(json.dumps(dict(at=utc(),positive_controls=5,negative_rejections=8,product_runs=0)))


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
                    require(request['op'] in ['emit','exit7','arm'] and len(token) <= 160 and token.isascii() and '\n' not in token, 'invalid fixture token')
                    count += 1
                    if request['op'] in ['exit7','arm']:
                        record({'event':'exit_intent' if request['op']=='exit7' else 'armed',
                                'token':token,'identity':process(os.getpid()),'intent_exit_code':7 if request['op']=='exit7' else None})
                        if request['op'] == 'exit7':
                            os._exit(7)
                        continue
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

    def new_app(self, label, session, window, index=None, remain='on'):
        require(len(self.apps) < 4, 'application bound')
        folder = self.root / label
        folder.mkdir()
        os.mkfifo(folder / 'control.fifo', 0o600)
        self.apps[label] = folder
        command = ['new-session', '-d', '-s', session, '-n', window, '-x', '120', '-y', '40'] if index is None else ['new-window', '-d', '-t', f'{session}:{index}', '-n', window]
        self.command(*command, '--', '/usr/bin/python3', str(Path(__file__).resolve()), '--application', str(folder))
        self.until(lambda: (folder / 'ready.json').exists(), 'application readiness missing')
        self.command('set-window-option', '-t', session + ':' + window, 'automatic-rename', 'off')
        self.command('set-window-option', '-t', session + ':' + window, 'remain-on-exit', remain)
        option = self.command('show-options','-wAv','-t',session + ':' + window,'remain-on-exit')
        require(option['stdout'].strip() == remain, 'fixture remain option differs')
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
        require(sum(o['p'].poll() is None for o in self.observers) < 2, 'observer concurrency bound')
        require(not self.observers, 'one healthy reader only')
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
                                   and cleanup_valid(self.report)
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

    def waiter(self, label, target, expected_target):
        require(sum(o['p'].poll() is None for o in self.observers) < 2, 'observer concurrency bound')
        require(sum(o.get('kind') == 'wait' for o in self.observers) < 3, 'waiter count bound')
        argv = [self.binary, '-N', '-S', str(self.socket), '-f', str(self.config),
                'wait-pane', '-t', target, '--pane-exit', '--timeout', '10s', '--json']
        p = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        native = process(p.pid)
        require(native and digest(native['exe']) == PINS['rmux'], 'waiter executable differs')
        o = dict(label=label, kind='wait', p=p, identity=native, argv=argv, start=utc(), stdout=b'', stderr=b'',
                 expected_target=expected_target, pipes={'stdout':p.stdout, 'stderr':p.stderr})
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
        self.event('waiter_readiness', label=waiter['label'], samples=samples, source='wait.rs:385-424; wait_target.rs:148-258; common.rs:406')
        return samples

    def waiter_result(self, waiter):
        self.finish_observer(waiter)
        result = {'rc':waiter['rc'], 'stdout':waiter['stdout'].decode(), 'stderr':waiter['stderr'].decode(),
                  'pid':waiter['identity']['pid'], 'birth_ticks':waiter['identity']['birth_ticks'],
                  'start':waiter['start'], 'end':waiter['end']}
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

    def pane_rows(self):
        fields = FIELDS + ['pane_dead','pane_dead_status','pane_dead_signal']
        fmt = '|'.join('#{' + k + '}' for k in fields)
        command = self.command('list-panes','-a','-F',fmt)
        rows = []
        for line in command['stdout'].splitlines():
            values = line.split('|')
            require(len(values) == len(fields), 'public exit fields unavailable')
            row = dict(zip(fields,values))
            require(row['socket_path'] == str(self.socket) and row['pid'] == str(self.daemon['pid']),
                    'public listing escaped own daemon')
            rows.append(row)
        self.event('public_pane_rows', rows=rows)
        return rows

    def live_barrier(self, label, ident, purpose):
        token = 'L2E_LIVE_' + purpose + '_' + uuid.uuid4().hex
        self.emit(label,token)
        captures = []
        def visible():
            row = self.command('capture-pane','-p','-t',ident['pane_id'])
            captures.append(row)
            return token in row['stdout']
        self.until(visible,'fresh live challenge not visible')
        current = self.identity(ident['pane_id'])
        require(same_identity(ident,current),'application changed across challenge')
        rows = [r for r in self.pane_rows() if r['pane_id'] == ident['pane_id']]
        require(len(rows)==1 and rows[0]['pane_dead']=='0' and
                rows[0]['pane_dead_status']==rows[0]['pane_dead_signal']=='','helper not publicly live')
        self.event('live_application_barrier',label=label,token=token,identity=current)
        return token

    def release(self, label, ident, signaled):
        own = ident['native']; current = process(own['pid'])
        require(current and current['birth_ticks']==own['birth_ticks'] and current['exe']==own['exe'],
                'release identity changed')
        record = dict(label=label,before=current,opened_at=utc())
        self.report.setdefault('kernel_exits',[]).append(record)
        fd = os.pidfd_open(own['pid'])
        try:
            current = process(own['pid']);record['after_open']=current
            require(current and current['birth_ticks']==own['birth_ticks'],'pidfd birth differs')
            poller=select.poll();poller.register(fd,select.POLLIN)
            record.update(fd=fd,events_before_request=poller.poll(0))
            require(not record['events_before_request'],'process exited before release')
            nonce='L2E_RELEASE_'+uuid.uuid4().hex
            record.update(nonce=nonce,issued_at=utc(),requested='SIGTERM' if signaled else 'exit7')
            self.send(label,'arm' if signaled else 'exit7',nonce)
            intents=[]
            def acknowledged():
                intents[:] = [e for e in map(json.loads,(self.apps[label]/'events.jsonl').read_text().splitlines())
                              if e.get('token')==nonce and e['event'] in ['armed','exit_intent']]
                return bool(intents)
            self.until(acknowledged,'fresh exit intent absent')
            record['intent']=intents[0]
            check_intent(intents[0],nonce,own,record['issued_at'],'armed' if signaled else 'exit_intent')
            if signaled:
                current=process(own['pid'])
                require(current and current['birth_ticks']==own['birth_ticks'],'signal identity changed')
                status=dict(line.split(':',1) for line in (Path('/proc')/str(own['pid'])/'status').read_text().splitlines() if ':' in line)
                flags={k:int(status[k].strip(),16) for k in ['SigCgt','SigIgn','SigBlk']}
                require(all(v & (1 << 14)==0 for v in flags.values()),'SIGTERM not default')
                record.update(signal_identity=current,signal_dispositions=flags,signal_at=utc())
                signal.pidfd_send_signal(fd,signal.SIGTERM)
            def exited():
                record['events_after_request']=poller.poll(0)
                return any(n==fd and mask & (select.POLLIN|select.POLLHUP) for n,mask in record['events_after_request'])
            self.until(exited,'no kernel process-exit event')
            record['observed_at']=utc();record['identity_after_event']=process(own['pid'])
            require(record['identity_after_event'] is None or
                    record['identity_after_event']['birth_ticks']!=own['birth_ticks'] or
                    record['identity_after_event']['state']=='Z','kernel event still living identity')
            return record
        finally:
            os.close(fd);record['closed_at']=utc()

    def public_exit(self, ident, kind):
        samples=[]
        def terminal():
            rows=self.pane_rows()
            matches=[r for r in rows if r['pane_id']==ident['pane_id']]
            samples.append(dict(at=utc(),rows=rows,original_process=process(int(ident['pane_pid']))))
            if kind=='removed':
                return not matches
            require(len(matches)==1,'retained pane disappeared')
            row=matches[0]
            if row['pane_dead']!='1':return False
            require(all(row[k]==ident[k] for k in ['session_id','window_id','pane_id','pane_pid','pid','socket_path']),
                    'retained public identity changed')
            require((row['pane_dead_status'],row['pane_dead_signal']) ==
                    (('','15') if kind=='signal' else ('7','')),'public exit metadata differs')
            return True
        self.until(terminal,'public pane exit state missing')
        self.pump()
        require(terminal(),'public exit state not stable')
        self.until(lambda: process(int(ident['pane_pid'])) is None,'original process not absent/reaped')
        return samples

    def run_cases(self):
        h=self.new_app('H','l2-e','healthy')
        healthy=self.observe('healthy',h['pane_id'])
        self.healthy(healthy,h,'SETUP_H')
        for index,(label,kind) in enumerate([('E1','normal'),('E2','signal'),('E3','removed')],1):
            def action(label=label,kind=kind,index=index):
                a=self.new_app(label,'l2-e',label.lower(),index=index,remain='off' if kind=='removed' else 'on')
                live=self.live_barrier(label,a,label+'_BEFORE_WAIT')
                waiter=self.waiter(label,a['pane_id'],'l2-e:'+a['pane_id'])
                readiness=self.wait_ready(waiter)
                # The live public identity excludes post-exit EOF/grace sleep paths.
                held=self.live_barrier(label,a,label+'_AFTER_READY')
                require(waiter['p'].poll() is None and not waiter['stdout'] and not waiter['stderr'],
                        'pane-exit waiter completed while helper held live')
                kernel=self.release(label,a,kind=='signal')
                result=self.waiter_result(waiter)
                parsed=check_exit_result(result,waiter['expected_target'],kind)
                samples=self.public_exit(a,kind)
                marker=self.healthy(healthy,h,label+'_H_AFTER')
                return dict(before=a,live_before_wait=live,live_after_readiness=held,readiness=readiness,
                            kernel_exit=kernel,result=result,parsed=parsed,public_exit_samples=samples,
                            healthy_marker=marker,exit_status_source='public CLI metadata' if not parsed['pane_exit']['stale'] else 'unknown')
            self.case({'E1':'E1-NORMAL','E2':'E2-SIGNAL','E3':'E3-REMOVED'}[label],action)


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
