#!/usr/bin/env python3
"""Darwin base-index B1--B4 adapter; Linux020 is an author input contract.

Read-only API bundle comes from default_shell_darwin.py and is pinned before
import. Only acquired011/012 APIs are reused;013 remains separate. Each lot
is prospectively frozen; candidate requires complete immutable oracle PASS.
Only private endpoint processes are inspected. No runtime or shell shim change.
"""
import base64
import ctypes
import datetime
import hashlib
import importlib
import json
import os
from pathlib import Path
import resource
import secrets
import signal
import shlex
import stat
import sys
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode = True
D = A = None
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{session_name}|#{window_index}|#{window_name}|#{pane_tty}'
PUBLIC_KEYS = ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'session_name', 'window_index', 'window_name', 'pane_tty')
WINDOW_FORMAT = '#{session_id}|#{session_name}|#{window_id}|#{window_index}|#{window_name}|#{window_panes}'
WINDOW_KEYS = ('session_id', 'session_name', 'window_id', 'window_index', 'window_name', 'window_panes')
KINDS = {label: label for label in ('a4', 'a7', 'b4', 'a12', 'a5')}
CODES = {label: 0 for label in KINDS}
INDICES = {'a4': 4, 'a7': 7, 'b4': 4, 'a12': 12, 'a5': 5}
CASE_LABELS = {'B1': ('a4',), 'B2': ('a4', 'a7', 'b4'),
               'B3': ('a4', 'a7', 'b4', 'a12'), 'B4': tuple(KINDS)}
EXIT_ORDER = ('a4', 'a7', 'a12', 'a5', 'b4')


ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_BASEINDEX_MARKER', 'BASH_ENV', 'ENV',
            'RMUX_DISABLE_TMUX_SHIM', 'L5_BASEINDEX_API')

FILE_CAP = 4096
RECORD_CAP = 720 * 1024
STDOUT_CAP = 256 * 1024
LOT_CAP = 2 * 1024**2

def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

class Unproven(RuntimeError): pass
class Failed(RuntimeError): pass

def require(ok, message):
    if not ok: raise Unproven(message)

def equal(actual, expected, message):
    if actual != expected: raise Failed(f'{message}: {actual!r} != {expected!r}')

def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

def pin(path):
    path = Path(path)
    return {'path': str(path), 'bytes': path.stat().st_size, 'sha256': digest(path)}

def write_once(path, value, cap=RECORD_CAP):
    raw = (json.dumps(value, indent=2) + '\n').encode()
    require(len(raw) <= cap, 'output exceeds pre-write cap')
    with Path(path).open('xb') as stream:
        stream.write(raw); stream.flush(); os.fsync(stream.fileno())

def atomic_once(path, value):
    path = Path(path)
    require(not path.exists(), 'immutable marker already exists')
    temp = path.with_suffix(path.suffix + '.tmp')
    write_once(temp, value, FILE_CAP)
    os.rename(temp, path)

def bind_api(config):
    global A, D, Unproven, Failed
    entry = config['adapter_pin']
    require(pin(entry['path']) == entry, '012 adapter provenance changed')
    sys.path.insert(0, str(Path(entry['path']).parent))
    A = importlib.import_module('default_command_darwin')
    require(str(Path(A.__file__).resolve()) == entry['path'], 'wrong 012 module imported')
    A.bind_api(config['api_pins']); D = A.D
    Unproven, Failed = D.Unproven, D.Failed

def stem(own):
    return str(own['pid']) + '-' + '-'.join(map(str, own['birth']))

def witness(kind, root, token, exit_code):
    config = json.loads(Path(os.environ['L5_BASEINDEX_API']).read_text())
    bind_api(config)
    root = Path(root)
    exit_code = int(exit_code)
    require(exit_code == 0, 'unplanned witness exit code')
    own = D.birth(os.getpid())
    key = stem(own)
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'exit_code': exit_code, 'identity': D.birth(os.getpid()),
                'cwd': os.getcwd(), 'argv': sys.argv,
                'environment': {k: os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root / ('born-' + key + '.json'), report(None))
    end = time.monotonic() + 180
    while time.monotonic() < end:
        requests = sorted(root.glob('request-' + key + '-*.json'))
        require(len(requests) <= 16, 'witness challenge cap')
        for path in requests:
            ack = root / path.name.replace('request-', 'ack-', 1)
            if not ack.exists():
                require(path.stat().st_size <= FILE_CAP, 'request size cap')
                request = json.loads(path.read_text())
                require(D.same(request['identity'], own), 'challenge identity mismatch')
                atomic_once(ack, report(request['challenge']))
        release = root / ('exit-' + key + '.json')
        if release.exists():
            require(release.stat().st_size <= FILE_CAP, 'release size cap')
            request = json.loads(release.read_text())
            require(D.same(request['identity'], own), 'exit request identity mismatch')
            equal(request['exit_code'], exit_code, 'fixed witness exit code')
            atomic_once(root / ('exit-intent-' + key + '.json'), report(request['challenge']))
            os._exit(exit_code)
        time.sleep(.02)
    os._exit(124)



def environment(config, phase):
    root = Path(config['roots'][phase])
    return {'HOME': str(root / 'home'), 'XDG_CONFIG_HOME': str(root / 'xdg'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/bash', 'TERM': 'vt100', 'LANG': 'C',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_BASEINDEX_MARKER': config['tokens'][phase], 'L5_BASEINDEX_API': config['config_path']}

def witness_argv(config, phase, label):
    return [config['pins']['python']['path'], '-I', config['driver'], 'witness', KINDS[label],
            str(Path(config['roots'][phase]) / 'markers'), config['tokens'][phase], str(CODES[label])]

def plan(config, phase):
    cwd = str(Path(config['roots'][phase]) / 'cwd')
    def argv(label): return witness_argv(config, phase, label)
    def session(name, label):
        return ['new-session', '-d', '-s', name, '-n', label, '-x', '80', '-y', '24', '-c', cwd, *argv(label)]
    def window(label, target='alpha'):
        return ['new-window', '-d', '-t', target, '-n', label, '-c', cwd, *argv(label)]
    return [('B1', 'a4', session('alpha', 'a4'), 'alpha:a4.0'),
            ('B2', 'a7', window('a7'), 'alpha:a7.0'),
            ('B2', 'b4', session('beta', 'b4'), 'beta:b4.0'),
            ('B3', 'a12', window('a12', 'alpha:12'), 'alpha:a12.0'),
            ('B4', 'a5', window('a5'), 'alpha:a5.0')]


def read_environment(pid):
    """Read only the allowlist from KERN_PROCARGS2; discard all other values."""
    mib = (ctypes.c_int * 3)(1, 49, pid)
    buffer = ctypes.create_string_buffer(1024 * 1024)
    size = ctypes.c_size_t(len(buffer))
    require(D.SYSTEM.sysctl(mib, 3, buffer, ctypes.byref(size), None, 0) == 0,
            'own environment read unavailable')
    require(size.value < len(buffer), 'environment buffer truncated')
    raw = buffer.raw[:size.value]
    prefix = D.parse_procargs(raw)
    result = {key: None for key in ENV_KEYS}
    seen = set()
    for field in raw[prefix['prefix_bytes']:].split(b'\0'):
        key, sep, value = field.partition(b'=')
        if sep and key in {k.encode() for k in ENV_KEYS}:
            key = key.decode()
            require(key not in seen, 'duplicate allowlisted environment key')
            seen.add(key); result[key] = os.fsdecode(value)
    return result


def check_marker(marker, child, expected, challenge):
    require(D.same(marker['identity'], child['before']), 'marker PID/birth mismatch')
    for field in ('kind', 'token', 'cwd', 'environment', 'exit_code'):
        equal(marker[field], expected[field], 'marker ' + field)
    equal(marker['argv'], expected['argv'][2:], 'witness actual sys.argv')
    equal(marker['challenge'], challenge, 'fresh challenge echo')

def parse_public(raw):
    fields = raw.split('|')
    require(len(fields) == len(PUBLIC_KEYS), 'invalid public identity/index')
    row = dict(zip(PUBLIC_KEYS, fields))
    for key in ('daemon_pid', 'pane_pid', 'window_index'):
        require(row[key].isdigit(), 'invalid public number ' + key)
        row[key] = int(row[key])
    return row

def parse_window(raw):
    fields = raw.split('|')
    require(len(fields) == len(WINDOW_KEYS), 'invalid window inventory')
    row = dict(zip(WINDOW_KEYS, fields))
    for key in ('window_index', 'window_panes'):
        require(row[key].isdigit(), 'invalid window number ' + key)
        row[key] = int(row[key])
    return row

def check_index(row, label):
    equal(row['session_name'], 'beta' if label == 'b4' else 'alpha', 'window session')
    equal(row['window_name'], label, 'explicit window name')
    equal(row['window_index'], INDICES[label], 'registered real window index')

def check_layout(rows, windows, children):
    equal(len(rows), len(children), 'no missing or extra pane')
    equal(len(windows), len(children), 'no missing or extra window')
    for key in ('window_id', 'pane_id'):
        equal(len({r[key] for r in rows}), len(rows), 'unique public ' + key)
    equal(len({r['window_id'] for r in windows}), len(windows), 'unique listed window IDs')
    equal(len({(r['session_id'], r['window_index']) for r in windows}), len(windows), 'unique session/index')
    logical = []
    sessions = {}
    for label, child in children.items():
        original = child['public']
        matches = [r for r in rows if r['pane_id'] == original['pane_id']]
        require(len(matches) == 1, 'expected pane identity missing')
        row = matches[0]
        equal(row, original, 'existing public identity/index changed')
        check_index(row, label)
        matches = [w for w in windows if w['window_id'] == row['window_id']]
        require(len(matches) == 1, 'expected window identity missing')
        window = matches[0]
        equal(window, {**{k: row[k] for k in WINDOW_KEYS if k != 'window_panes'}, 'window_panes': 1}, 'window/pane inventory coherence')
        name = row['session_name']
        if name in sessions: equal(sessions[name], row['session_id'], 'stable session ID')
        sessions[name] = row['session_id']
        logical.append({'label': label, 'session_name': name, 'window_name': row['window_name'], 'window_index': row['window_index']})
    equal(len(set(sessions.values())), len(sessions), 'distinct session IDs')
    return sorted(logical, key=lambda r: r['label'])


def check_child(first, second, public, daemon, expected):
    equal([public[k] for k in ('pane_dead', 'pane_dead_status', 'pane_dead_signal')],
          ['0', '', ''], 'living public pane state')
    check_index(public, expected['label'])
    require(first['before']['status'] != 5 and second['after']['status'] != 5, 'dead child presented live')
    A.check_child(first, second, public, daemon, expected)


def check_kernel_exit(before, after, watch):
    require(watch['event_before_request'] is False and watch['event_after_request'] is True,
            'fresh kernel NOTE_EXIT missing')
    require(D.same(before, watch['identity']), 'exit watch stale birth')
    require(not D.same(before, after) or after['status'] == 5, 'exited child still live')


def check_no_extra(helpers):
    require(not helpers, 'additional own child/helper observed')


def check_cleanup(row, owned):
    A.check_cleanup(row)
    expected = {(p['pid'], tuple(p['birth'])) for p in owned.values()}
    actual = {(p['before']['pid'], tuple(p['before']['birth'])) for p in row['processes']}
    equal(actual, expected, 'cleanup identities complete')


def command_budget(record, cleaning=False):
    require(len(record['commands']) < (130 if cleaning else 128), 'CLI count cap')
    cap = RECORD_CAP - 8192 if cleaning else 650 * 1024
    require(len(json.dumps(record, indent=2).encode()) < cap, 'record reserve before exec')
    require(record['stdout_bytes'] + FILE_CAP <= STDOUT_CAP, 'stdout reserve before exec')


class Run:
    def __init__(self, frozen):
        A.Run.__init__(self, frozen)
        self.env = environment(self.config, self.phase)
        self.record.update(environment=self.env, exits=[], layouts=[], stdout_bytes=0, stderr_bytes=0)
        self.oracle_layouts = (json.loads(Path(frozen['oracle_gate']['path']).read_text())['layouts']
                               if self.phase == 'candidate' else {})

    def remaining(self):
        left = min(self.case_end, self.start + (180 if self.cleaning else 150)) - time.monotonic()
        require(left > 0, 'native deadline exceeded')
        return min(left, 5)

    def socket_info(self): return A.Run.socket_info(self)
    def track(self, row, role): return A.Run.track(self, row, role)
    def profiles(self, post_shutdown=False): return A.Run.profiles(self, post_shutdown)
    def wait_json(self, path): return A.Run.wait_json(self, path)
    def ledger(self): return A.Run.ledger(self)

    def checkpoint(self):
        require(len((json.dumps(self.record, indent=2) + '\n').encode()) <= RECORD_CAP, 'record cap')
        D.acquired.save(self.root / 'progress.json', self.record)

    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent, D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own = D.birth(pid)
                if own is None or own['ppid'] != parent['pid'] or pid in self.owned: continue
                self.track(own, 'unexpected-helper')
                self.record['helpers'].append({'at': now(), 'parent': parent, 'identity': own})
        if not self.cleaning: check_no_extra(self.record['helpers'])

    def command(self, args, startup=False, required=True):
        command_budget(self.record, self.cleaning)
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup: argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        entry = {'start': now(), 'argv': argv, 'pre_exec_file_cap': FILE_CAP,
                 'stdout_budget_before': self.record['stdout_bytes']}
        self.record['commands'].append(entry)
        old = resource.getrlimit(resource.RLIMIT_FSIZE)
        require(old[0] == resource.RLIM_INFINITY or old[0] >= FILE_CAP, 'file limit too small')
        try:
            resource.setrlimit(resource.RLIMIT_FSIZE, (FILE_CAP, old[1]))
            with chdir(self.root):
                result = D.native.run(argv, env=self.env, timeout=self.remaining(), check=False)
            entry.update(pid=result.pid, returncode=result.returncode,
                         stdout=result.stdout.decode(), stderr=result.stderr.decode())
            self.record['stdout_bytes'] += len(result.stdout)
            self.record['stderr_bytes'] += len(result.stderr)
            require(len(result.stdout) < FILE_CAP and len(result.stderr) < FILE_CAP, 'CLI output cap')
        except BaseException as error:
            entry['error'] = repr(error); raise
        finally:
            resource.setrlimit(resource.RLIMIT_FSIZE, old); entry['end'] = now()
        if required: require(result.returncode == 0, 'CLI failed: ' + repr(args))
        return result.stdout.decode().strip()

    def public(self, target):
        public = parse_public(self.command(['display-message', '-p', '-t', target, FORMAT]))
        require(public['socket'] == str(self.socket), 'public endpoint differs')
        endpoint = self.socket_info(); require(endpoint is not None, 'endpoint absent')
        daemon = D.process(public['daemon_pid']); self.track(daemon['before'], 'daemon')
        key = 'tmux' if self.phase == 'oracle' else 'rmux_daemon'
        equal(daemon['image']['sha256'], self.frozen['pins'][key]['sha256'], 'daemon disk identity')
        if self.daemon is None: self.daemon, self.endpoint = daemon, endpoint
        else: require(D.stable(self.daemon, daemon) and self.endpoint == endpoint, 'daemon/socket changed')
        item = {'at': now(), 'target': target, 'public': public, 'daemon': daemon, 'endpoint': endpoint}
        self.record['public_samples'].append(item)
        return item

    def expected_for(self, label):
        python = self.config['pins']['python']
        return {'label': label, 'kind': KINDS[label], 'exit_code': CODES[label],
                'token': self.config['tokens'][self.phase], 'exe': python['path'],
                'exe_sha256': python['sha256'], 'argv': witness_argv(self.config, self.phase, label),
                'cwd': str(self.root / 'cwd'), 'environment': {k: self.env.get(k) for k in ENV_KEYS}}

    def snapshot(self, pid):
        item = D.process(pid, pane=True); item['environment'] = read_environment(pid)
        require(D.same(item['after'], D.birth(pid)), 'birth changed during environment read')
        return item

    def observe(self, label, target, reobserve=False):
        pub = self.public(target); public, daemon = pub['public'], pub['daemon']
        own = D.birth(public['pane_pid']); require(own is not None, 'public child absent')
        self.track(own, 'pane')
        key = stem(own); markers = self.root / 'markers'
        item = {'label': label, 'target': target, 'reobserve': reobserve, 'at': now(),
                'expected': self.expected_for(label), 'public': public, 'status': 'UNPROVEN'}
        index = len(self.record['observations']); self.record['observations'].append(item)
        try:
            born_path = markers / ('born-' + key + '.json')
            born = self.wait_json(born_path); first = self.snapshot(own['pid'])
            item.update(first=first, born=born, born_path=str(born_path)); self.checkpoint()
            check_marker(born, first, item['expected'], None)
            nonce = secrets.token_hex(24)
            request = {'identity': own, 'challenge': nonce, 'issued_at': now()}
            request_path = markers / (f'request-{key}-{index}.json')
            item.update(request=request, request_path=str(request_path))
            atomic_once(request_path, request)
            ack_path = markers / request_path.name.replace('request-', 'ack-', 1)
            ack = self.wait_json(ack_path); second = self.snapshot(own['pid'])
            item.update(ack=ack, ack_path=str(ack_path), child=second)
            after = self.public(target); item['public_after'] = after['public']
            equal(after['public'], public, 'public attribution across challenge')
            check_child(first, second, public, after['daemon'], item['expected'])
            check_marker(ack, second, item['expected'], nonce)
            if reobserve:
                old = self.children[label]
                require(D.stable(old['child'], second) and old['public'] == public, 'existing child mutated')
            else: self.children[label] = item
            self.helpers(); item['status'] = 'PASS'
            return item
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise
        finally: self.checkpoint()


    def options(self, target, effective, local):
        equal(self.command(['show-options', '-gv', 'base-index']), '4', 'global base-index')
        equal(self.command(['show-options', '-Av', '-t', target, 'base-index']), str(effective), 'effective session base-index')
        equal(self.command(['show-options', '-qv', '-t', target, 'base-index']), '' if local is None else str(local), 'local session base-index')
        equal(self.command(['show-options', '-gv', 'renumber-windows']), 'off', 'global renumber disabled')
        equal(self.command(['show-options', '-Av', '-t', target, 'renumber-windows']), 'off', 'effective renumber disabled')
        equal(self.command(['show-options', '-gv', 'default-command']), '', 'explicit argv guard')

    def layout(self, case):
        equal(list(self.children), list(CASE_LABELS[case]), 'exact case population')
        item = {'case': case, 'at': now(), 'samples': [], 'status': 'UNPROVEN'}
        self.record['layouts'].append(item)
        previous = None
        try:
            while True:
                raw_panes = self.command(['list-panes', '-a', '-F', FORMAT])
                raw_windows = self.command(['list-windows', '-a', '-F', WINDOW_FORMAT])
                rows = [parse_public(line) for line in raw_panes.splitlines()]
                windows = [parse_window(line) for line in raw_windows.splitlines()]
                sample = {'at': now(), 'raw_panes': raw_panes, 'raw_windows': raw_windows, 'panes': rows, 'windows': windows}
                item['samples'].append(sample)
                logical = check_layout(rows, windows, self.children)
                stable = {'panes': sorted(rows, key=lambda r: r['pane_id']), 'windows': sorted(windows, key=lambda r: r['window_id'])}
                if previous == stable:
                    item['logical'] = logical
                    if self.phase == 'candidate':
                        require(case in self.oracle_layouts, 'oracle inventory gate missing')
                        equal(logical, self.oracle_layouts[case], 'exact oracle window indices')
                    item['status'] = 'PASS'
                    return item
                previous = stable
                time.sleep(min(.05, self.remaining()))
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise



    def exit_witness(self, label):
        child = self.children[label]; before = child['child']['before']
        record = {'label': label, 'at': now(), 'configured_exit_code': 0,
                  'original_public': child['public'], 'original_child': child['child'],
                  'actual_public_exit_status': None, 'status_scope': 'helper intent only, no retained public status',
                  'status': 'UNPROVEN'}
        self.record['exits'].append(record)
        try:
            current = self.snapshot(before['pid']); record['before_release'] = current
            require(D.stable(child['child'], current), 'child changed before release')
            watch = self.watches[before['pid']]
            record['watch'] = {'pid': before['pid'], 'identity': before,
                               'retained_since_live_observation': True,
                               'event_before_request': watch.poll_exit()}
            require(not record['watch']['event_before_request'], 'child already exited')
            challenge = secrets.token_hex(24)
            request = {'identity': before, 'challenge': challenge, 'exit_code': 0, 'issued_at': now()}
            key = stem(before); release = self.root / 'markers' / ('exit-' + key + '.json')
            record.update(request=request, request_path=str(release)); atomic_once(release, request)
            intent_path = self.root / 'markers' / ('exit-intent-' + key + '.json')
            intent = self.wait_json(intent_path); record.update(intent=intent, intent_path=str(intent_path))
            check_marker(intent, child['child'], child['expected'], challenge)
            watch.wait(self.remaining())
            record['watch'].update(event_after_request=watch.exited, observed_at=now())
            record['identity_after_exit_event'] = D.birth(before['pid'])
            check_kernel_exit(before, record['identity_after_exit_event'], record['watch'])
            record['status'] = 'PASS'
        except Exception as error:
            record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error)); raise
        finally: self.checkpoint()

    def spawn(self, label):
        rows = [row for row in plan(self.config, self.phase) if row[1] == label]
        require(len(rows) == 1 and self.spawn_count < 5, 'pane child budget or plan mismatch')
        _, _, args, target = rows[0]
        self.spawn_count += 1
        self.command(args, startup=label == 'a4')
        observed = self.observe(label, target)
        check_index(observed['public'], label)
        return observed



    def execute(self):
        require(not self.socket.exists() and not list((self.root / 'markers').iterdir()), 'fixture is not fresh')
        self.profiles()
        for case in CASE_LABELS:
            self.case_end = min(time.monotonic() + 20, self.start + 150)
            entry = {'id': case, 'start': now(), 'monotonic_start': time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case == 'B1':
                    self.spawn('a4');self.options('alpha', 4, None)
                elif case == 'B2':
                    self.command(['set-option', '-t', 'alpha', 'base-index', '7'])
                    self.options('alpha', 7, 7)
                    self.spawn('a7');self.spawn('b4');self.options('beta', 4, None)
                    self.observe('a4', 'alpha:a4.0', True)
                elif case == 'B3':
                    self.spawn('a12');self.options('alpha', 7, 7)
                    for label in ('a4', 'a7', 'b4'):
                        self.observe(label, self.children[label]['target'], True)
                else:
                    self.command(['source-file', str(self.root / 'live.conf')])
                    self.options('alpha', 4, None);self.options('beta', 4, None)
                    self.spawn('a5')
                    for label in ('a4', 'a7', 'b4', 'a12'):
                        self.observe(label, self.children[label]['target'], True)
                self.ledger();self.layout(case)
                if case == 'B4':
                    for label in EXIT_ORDER:self.exit_witness(label)
                self.profiles();self.remaining();entry['status'] = 'PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
                raise
            finally:
                entry.update(end=now(), elapsed_seconds=time.monotonic() - entry['monotonic_start'])



    def cleanup(self):
        self.cleaning = True; stop = min(time.monotonic() + 30, self.start + 180)
        self.case_end = stop
        row = {'start': now(), 'errors': [], 'rescues': [], 'processes': []}; self.record['cleanup'] = row
        def attempt(name, fn):
            try: return fn()
            except Exception as error: row['errors'].append({'step': name, 'error': repr(error)})
        if self.daemon is None and self.socket.exists():
            attempt('recover-owned-endpoint', lambda: self.public('alpha:0.0'))
        if self.daemon:
            def close_server():
                pid = self.daemon['before']['pid']; watched = self.watches[pid]
                current = D.birth(pid)
                if watched.poll_exit() or not D.same(self.daemon['before'], current):
                    row['daemon_already_exited'] = True
                    return
                try:
                    observed = D.process(pid)
                except Exception:
                    if watched.poll_exit() or not D.same(self.daemon['before'], D.birth(pid)):
                        row['daemon_exited_during_shutdown_qualification'] = True
                        return
                    raise
                endpoint = self.socket_info()
                if endpoint is None:
                    watched.wait(self.remaining())
                    row['daemon_exit_after_socket_disappearance'] = True
                    return
                require(D.stable(observed, self.daemon) and endpoint == self.endpoint,
                        'cleanup ownership changed')
                self.helpers(); self.command(['kill-server'], required=False)
                if self.record['commands'][-1]['returncode'] != 0:
                    watched.wait(self.remaining())
                    row['daemon_exit_during_kill_server'] = True
            attempt('qualified-kill-server', close_server)
        for pid, old in self.owned.items():
            watched = self.watches[pid]
            attempt('wait-exit-' + str(pid), lambda: watched.wait(max(.01, min(3, stop - time.monotonic()))))
            if not watched.poll_exit() and D.same(old, D.birth(pid)):
                row['rescues'].append({'identity': old, 'signal': int(signal.SIGKILL)})
                attempt('rescue-' + str(pid), lambda: watched.retire_after_failure(max(.01, min(2, stop - time.monotonic()))))
            reaped = None
            try:
                collected, status = os.waitpid(pid, os.WNOHANG)
                reaped = {'pid': collected, 'raw_status': status}
            except ChildProcessError:
                reaped = {'ECHILD': True, 'note': 'indirect process; exit and disappearance checked separately'}
            after = D.birth(pid)
            while D.same(old, after) and time.monotonic() < stop:
                time.sleep(.02); after = D.birth(pid)
            row['processes'].append({'before': old, 'after': after, 'exit_event': watched.exited, 'waitpid': reaped})
        row['all_absent'] = all(not D.same(p['before'], p['after']) and p['exit_event'] for p in row['processes'])
        if row['all_absent'] and self.socket.exists() and self.socket_info() == self.endpoint:
            self.socket.unlink(); row['owned_socket_residue_removed'] = True
        row['socket_absent'] = not self.socket.exists()
        attempt('private-profiles-after', lambda: self.profiles(post_shutdown=row['all_absent']))
        for watched in self.watches.values(): watched.close()
        row['fd_after'] = D.fd_snapshot(); row['fds_equal'] = row['fd_after'] == self.record['fd_before']
        row['end'] = now()
        row['pass'] = (self.daemon is not None and row['all_absent'] and row['socket_absent']
                       and row['fds_equal'] and not row['errors'] and not row['rescues'] and time.monotonic() <= stop)
        self.checkpoint()
        check_cleanup(row, self.owned)
        check_no_extra(self.record['helpers'])


def verify_frozen(frozen, frozen_path):
    require(frozen['phase'] in ('oracle', 'candidate'), 'unknown phase')
    for item in frozen['artifacts']:
        require(pin(item['path']) == item, 'frozen artifact changed: ' + item['path'])
    config = frozen['config']
    mandatory = [str(Path(__file__).resolve()), config['config_path'],
                 *[p['path'] for p in config['pins'].values()],
                 *[p['path'] for p in config['api_pins']], config['adapter_pin']['path'],
                 frozen['pure']['path']]
    mandatory += [str(Path(root) / name) for root in config['roots'].values()
                  for name in ('startup.conf', 'live.conf')]
    pinned = {p['path'] for p in frozen['artifacts']}
    require(set(mandatory) <= pinned, 'mandatory provenance missing')
    equal(json.loads(Path(config['config_path']).read_text()), config, 'config provenance')
    equal(frozen['plan'], json.loads(json.dumps(plan(config, frozen['phase']))), 'frozen plan')
    equal(frozen['environment'], environment(config, frozen['phase']), 'frozen environment')
    equal(frozen['pins'], config['pins'], 'frozen binary pins')
    equal(frozen['root'], config['roots'][frozen['phase']], 'frozen private root')
    pure = json.loads(Path(frozen['pure']['path']).read_text())
    require(pure['status'] == 'PASS' and pure['driver_sha256'] == digest(__file__), 'pure controls stale')
    require(not (Path(frozen['root']) / 'STARTED.json').exists(), 'lot already attempted')
    if frozen['phase'] == 'candidate':
        require(pin(frozen['oracle_gate']['path']) == frozen['oracle_gate'], 'oracle gate changed')
        gate = json.loads(Path(frozen['oracle_gate']['path']).read_text())
        require(gate['status'] == 'PASS' and pin(gate['result']['path']) == gate['result'], 'oracle proof invalid')
        require(gate['driver_sha256'] == digest(__file__), 'oracle driver differs')
        require(set(gate['expected']) == set(KINDS), 'oracle population incomplete')
        equal(set(gate['layouts']), set(CASE_LABELS), 'complete oracle layouts')
        for case, labels in CASE_LABELS.items():
            equal(gate['layouts'][case], sorted([{'label': label, 'session_name': 'beta' if label == 'b4' else 'alpha',
                  'window_name': label, 'window_index': INDICES[label]} for label in labels], key=lambda r: r['label']),
                  'frozen oracle index contract')
        for label, kind in KINDS.items():
            expected = witness_argv(config, 'oracle', label)
            equal(gate['expected'][label]['argv'], expected, 'unmodified oracle argv')
            equal(base64.b64decode(gate['expected'][label]['argv_base64']),
                  b'\0'.join(os.fsencode(v) for v in expected) + b'\0', 'unmodified oracle bytes')


def execute(frozen_path):
    frozen = json.loads(frozen_path.read_text()); verify_frozen(frozen, frozen_path)
    bind_api(frozen['config'])
    run = Run(frozen)
    write_once(run.root / 'STARTED.json', {'at': now(), 'frozen': pin(frozen_path),
                                         'observer': D.birth(os.getpid())})
    try:
        run.execute(); run.record['status'] = 'PASS'
    except BaseException as error:
        run.record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN',
                          error=repr(error), traceback=traceback.format_exc())
    finally:
        try: run.cleanup()
        except BaseException as error:
            run.record.update(cleanup_exception=repr(error), status='UNPROVEN')
        run.record.update(end=now(), seconds=time.monotonic() - run.start,
                          pane_spawn_commands=run.spawn_count, frozen=pin(frozen_path),
                          limits=['Disk hashes do not attest the mapped image.',
                                  'Scoped own-child sampling is not exhaustive transient-helper coverage.',
                                  'Indirect children: NOTE_EXIT plus birth disappearance, not author reap.'])
        result_size = len((json.dumps(run.record, indent=2) + '\n').encode())
        other = sum(p.stat().st_size for p in run.root.rglob('*') if p.is_file())
        require(result_size + other < LOT_CAP, 'lot log cap')
        write_once(run.root / 'RESULT.json', run.record)
    if run.record['status'] == 'PASS' and frozen['phase'] == 'oracle':
        expected = {label: {k: obs['child']['args'][k] for k in ('argv', 'argv_base64')}
                    for label, obs in run.children.items()}
        write_once(run.root / 'ORACLE-FROZEN.json', {'at': now(), 'status': 'PASS',
                   'result': pin(run.root / 'RESULT.json'), 'expected': expected,
                   'driver_sha256': digest(__file__), 'frozen': pin(frozen_path),
                   'layouts': {item['case']: item['logical'] for item in run.record['layouts']},
                   'comparison': 'Both absolute phase argv lists frozen before oracle; no argv normalization.'})
    print(json.dumps({'status': run.record['status'], 'result': pin(run.root / 'RESULT.json'),
                      'seconds': run.record['seconds']}), flush=True)
    return 0 if run.record['status'] == 'PASS' else 1


if __name__ == '__main__':
    if len(sys.argv) >= 6 and sys.argv[1] == 'witness':
        witness(*sys.argv[2:6])
    elif len(sys.argv) == 3 and sys.argv[1] == '--frozen':
        raise SystemExit(execute(Path(sys.argv[2])))
    else:
        raise SystemExit('Use --frozen FILE.json, or the private witness mode.')
