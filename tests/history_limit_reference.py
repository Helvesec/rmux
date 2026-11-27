#!/usr/bin/env python3
"""Bounded native Linux history-limit reference (HISTORY2 prospective grid mutation).

Usage: python3 -I tests/history_limit_reference.py prepare CONFIG.json
       python3 -I tests/history_limit_reference.py run CONFIG.json

CONFIG supplies absolute private root, oracle, candidate, daemon and Python paths,
plus their SHA256 pins. Prepare creates fresh fixtures and pure observer controls.
Before run, write root/FROZEN-NATIVE.json with an `artifacts` list of path/SHA256
pairs covering this file, CONFIG, prepared inputs, binaries and pure controls.
Run is single-use, stops at the first failure and gates RMUX on immutable tmux PASS.
Only read-only CLI capture-pane of owned panes; no build, input injection, attach, desktop capture or debugger.
The witness mode is an ordinary pane program; it reports its actual process state
and acknowledges fresh filesystem challenges. Only private processes are observed.
"""
import base64
import copy
import ctypes
import datetime
import hashlib
import json
import os
from pathlib import Path
import secrets
import select
import shlex
import signal
import stat
import subprocess
import sys
import time
import traceback

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}'
KINDS = {label: label for label in ('a20', 'a40', 'b20', 'a40b', 'a30', 'b30')}
CODES = {label: 0 for label in KINDS}
ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_HISTORY_MARKER', 'BASH_ENV', 'ENV',
            'RMUX_DISABLE_TMUX_SHIM')

def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

class Unproven(RuntimeError):
    pass

class Failed(RuntimeError):
    pass

def require(ok, message):
    if not ok:
        raise Unproven(message)

def equal(actual, expected, message):
    if actual != expected:
        raise Failed(f'{message}: {actual!r} != {expected!r}')

def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

def write_once(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())

def atomic_once(path, value):
    path = Path(path)
    require(not path.exists(), 'immutable marker already exists')
    temp = path.with_suffix(path.suffix + '.tmp')
    write_once(temp, value)
    os.rename(temp, path)

def identity(pid):
    try:
        raw = Path('/proc', str(pid), 'stat').read_text()
        fields = raw.rsplit(')', 1)[1].split()
        return {'pid': int(pid), 'start_ticks': fields[19], 'state': fields[0],
                'ppid': int(fields[1]), 'stat_raw': raw, 'at': now()}
    except (FileNotFoundError, ProcessLookupError):
        return None

def same(a, b):
    return bool(a and b and all(a[k] == b[k] for k in ('pid', 'start_ticks')))

def proc(pid):
    before = identity(pid)
    if before is None:
        return None
    result = {'before': before, 'at': now()}
    p = Path('/proc', str(pid))
    try:
        raw = (p / 'cmdline').read_bytes()
        result.update(exe=os.readlink(p / 'exe'), exe_sha256=digest(p / 'exe'),
                      cmdline_base64=base64.b64encode(raw).decode(),
                      argv=[v.decode('utf-8', errors='surrogateescape') for v in raw[:-1].split(b'\0')],
                      fd0=os.readlink(p / 'fd/0'), cwd=os.readlink(p / 'cwd'))
    except (FileNotFoundError, ProcessLookupError) as error:
        result['read_error'] = repr(error)
    result['after'] = identity(pid)
    result['coherent'] = same(before, result['after']) and 'read_error' not in result
    return result

def complete(p):
    return bool(p and p['coherent'] and p['after']['state'] != 'Z')

def snapshot_same(a, b):
    return bool(complete(a) and complete(b) and same(a['before'], b['before']) and
                all(a[k] == b[k] for k in ('exe', 'exe_sha256', 'cmdline_base64', 'fd0', 'cwd')) and
                a['before']['ppid'] == b['before']['ppid'])

def fd_snapshot():
    result = {}
    for fd in os.listdir('/proc/self/fd'):
        try:
            result[fd] = os.readlink('/proc/self/fd/' + fd)
        except FileNotFoundError:
            pass
    return result

def line(label, index):
    return f'HIST|{label}|{index:04d}\n'.encode('ascii')

def payload(label):
    return b''.join(line(label, i) for i in range(80))

def inventory(raw, label, metrics):
    equal((metrics['width'], metrics['height']), (80, 12), 'geometry')
    require(raw.endswith(b'\n'), 'capture framing newline absent')
    rows = raw.split(b'\n')[:-1]  # framing delimiter only; blank rows are retained
    nonblank = [row for row in rows if row]
    require(nonblank, 'empty capture inventory')
    indices = []
    for row in nonblank:
        prefix = ('HIST|' + label + '|').encode('ascii')
        require(row.startswith(prefix), 'foreign capture line')
        suffix = row[len(prefix):]
        require(len(suffix) == 4 and suffix.isdigit(), 'invalid line index')
        index = int(suffix)
        equal(row + b'\n', line(label, index), 'exact line bytes')
        indices.append(index)
    require(0 <= indices[0] <= 79, 'line index range')
    equal(indices, list(range(indices[0], 80)), 'contiguous suffix through final line79')
    equal(rows, nonblank + [b''] * (len(rows) - len(nonblank)), 'interior blank rows')
    equal(len(rows), metrics['history_size'] + metrics['height'], 'logical row inventory')
    require(0 <= metrics['history_size'] <= metrics['history_limit'], 'history size outside limit')
    return {'indices': indices, 'first': indices[0], 'last': indices[-1],
            'nonblank_rows': len(nonblank), 'blank_rows': len(rows) - len(nonblank),
            'rows': len(rows), 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}

def unchanged_history(actual, original):
    equal(actual['raw_base64'], original['raw_base64'], 'existing capture bytes changed')
    equal(actual['inventory'], original['inventory'], 'existing logical inventory changed')
    for key in ('width', 'height', 'history_size'):
        equal(actual['metrics'][key], original['metrics'][key], 'existing history ' + key)


# Prospective HISTORY2 values, pinned before any native execution. These are
# exact suffixes and actual grid limits, including changes to existing panes.
HISTORY_SPEC = {'H1:a20': ['a20', 50, 19, 20], 'H2:a40': ['a40', 32, 37, 40], 'H2:b20': ['b20', 50, 19, 20], 'H2:old-a20': ['a20', 50, 19, 40], 'H3:a40b': ['a40b', 32, 37, 40], 'H3:global-old-a20': ['a20', 50, 19, 40], 'H3:global-old-a40': ['a40', 32, 37, 40], 'H3:global-old-b20': ['b20', 50, 19, 30], 'H3:a30': ['a30', 39, 30, 30], 'H3:b30': ['b30', 39, 30, 30], 'H3:unset-old-a20': ['a20', 50, 19, 30], 'H3:unset-old-a40': ['a40', 39, 30, 30], 'H3:unset-old-b20': ['b20', 50, 19, 30], 'H3:unset-old-a40b': ['a40b', 39, 30, 30]}

def expected_capture(key):
    require(key in HISTORY_SPEC, 'unregistered history observation')
    label, first, size, limit = HISTORY_SPEC[key]
    raw = b''.join(line(label, i) for i in range(first, 80)) + b'\n'
    metrics = {'width': 80, 'height': 12, 'history_size': size, 'history_limit': limit}
    return {'raw_base64': base64.b64encode(raw).decode(), 'metrics': metrics,
            'inventory': inventory(raw, label, metrics)}

def check_capture(actual, key):
    equal(actual, expected_capture(key), 'prospective exact history at ' + key)

def check_transition(actual, original, key):
    check_capture(actual, key)
    label = HISTORY_SPEC[key][0]
    if key in ('H3:unset-old-a40', 'H3:unset-old-a40b'):
        check_capture(original, 'H2:a40' if label == 'a40' else 'H3:a40b')
        removed = b''.join(line(label, i) for i in range(32, 39))
        equal(base64.b64decode(original['raw_base64']),
              removed + base64.b64decode(actual['raw_base64']),
              'only the seven pre-registered oldest lines removed')
        equal(original['metrics']['history_size'] - actual['metrics']['history_size'],
              7, 'exact history reduction')
    else:
        unchanged_history(actual, original)

def oracle_gate(record, result_sha256):
    equal(record['status'], 'PASS', 'complete oracle required')
    equal([c['id'] for c in record['cases']], ['H1', 'H2', 'H3'], 'complete oracle cases')
    require(all(c['status'] == 'PASS' for c in record['cases']), 'oracle case failed')
    captures = {c['key']: c['final'] for c in record['captures']}
    equal(list(captures), list(HISTORY_SPEC), 'complete ordered capture keys')
    equal(len(record['captures']), len(HISTORY_SPEC), 'no duplicate capture keys')
    require(all(c['status'] == 'PASS' for c in record['captures']), 'oracle capture failed')
    for key, value in captures.items():
        check_capture(value, key)
    equal([e['label'] for e in record['exits']], list(KINDS), 'all six normal exits')
    require(all(e['status'] == 'PASS' for e in record['exits']), 'oracle exit failed')
    equal(record['cleanup']['status'], 'PASS', 'oracle cleanup')
    return {'at': now(), 'status': 'PASS', 'expected_kinds': KINDS,
            'result_sha256': result_sha256, 'driver_sha256': digest(__file__),
            'captures': captures}

def witness(kind, root, token, exit_code):
    root = Path(root)
    equal(int(exit_code), 0, 'fixed helper normal exit')
    require(kind in KINDS, 'unknown helper kind')
    own = identity(os.getpid())
    stem = f"{own['pid']}-{own['start_ticks']}"
    emission = None
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'exit_code': 0, 'identity': identity(os.getpid()),
                'cwd': os.getcwd(), 'argv': sys.argv, 'emission': emission,
                'environment': {k: os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root / ('born-' + stem + '.json'), report(None))
    data = payload(kind)
    sent = 0
    while sent < len(data):
        written = os.write(1, data[sent:])
        require(written > 0, 'short helper emission')
        sent += written
    emission = {'completed_at': now(), 'bytes': sent, 'lines': 80,
                'sha256': hashlib.sha256(data).hexdigest()}
    atomic_once(root / ('emitted-' + stem + '.json'), report(None))
    end = time.monotonic() + 180
    while time.monotonic() < end:
        for path in sorted(root.glob('request-' + stem + '-*.json')):
            ack = root / path.name.replace('request-', 'ack-', 1)
            if not ack.exists():
                request = json.loads(path.read_text())
                require(same(request['identity'], own), 'challenge identity mismatch')
                atomic_once(ack, report(request['challenge']))
        release = root / ('exit-' + stem + '.json')
        if release.exists():
            request = json.loads(release.read_text())
            require(same(request['identity'], own), 'exit request identity mismatch')
            equal(request['exit_code'], 0, 'fixed helper exit code')
            atomic_once(root / ('exit-intent-' + stem + '.json'), report(request['challenge']))
            os._exit(0)
        time.sleep(.02)
    os._exit(124)

def environment(config, phase):
    root = Path(config['root']) / phase
    return {'HOME': str(root / 'home-empty'), 'XDG_CONFIG_HOME': str(root / 'xdg-empty'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/dash', 'TERM': 'vt100', 'LANG': 'C.UTF-8',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_HISTORY_MARKER': config['tokens'][phase]}

def witness_argv(config, phase, label):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', KINDS[label],
            str(Path(config['root']) / phase / 'markers'), config['tokens'][phase], str(CODES[label])]

def plan(config, phase):
    cwd = str(Path(config['root']) / phase / 'cwd')
    def argv(label):
        return witness_argv(config, phase, label)
    def session(name, label):
        return ['new-session', '-d', '-s', name, '-n', label, '-x', '80', '-y', '12', '-c', cwd, *argv(label)]
    def window(name, label):
        return ['new-window', '-d', '-t', name, '-n', label, '-c', cwd, *argv(label)]
    return [('H1', 'a20', session('alpha', 'a20'), 'alpha:a20.0'),
            ('H2', 'a40', window('alpha', 'a40'), 'alpha:a40.0'),
            ('H2', 'b20', session('beta', 'b20'), 'beta:b20.0'),
            ('H3', 'a40b', window('alpha', 'a40b'), 'alpha:a40b.0'),
            ('H3', 'a30', window('alpha', 'a30'), 'alpha:a30.0'),
            ('H3', 'b30', window('beta', 'b30'), 'beta:b30.0')]

def check_child(first, second, public, daemon, expected, ancestry):
    require(snapshot_same(first, second), 'two child snapshots incoherent')
    require(public['pane_pid'] == first['before']['pid'], 'public pane PID mismatch')
    require(complete(daemon) and public['daemon_pid'] == daemon['before']['pid'], 'public daemon mismatch')
    parent = first['before']['ppid']
    for ancestor in ancestry:
        require(complete(ancestor) and ancestor['before']['pid'] == parent, 'broken ancestry')
        parent = ancestor['before']['ppid']
    require(ancestry and snapshot_same(ancestry[-1], daemon), 'not linked to private daemon')
    require(first['fd0'].startswith('/dev/pts/'), 'no PTY stdin')
    for field in ('exe', 'exe_sha256', 'argv', 'cwd'):
        equal(first[field], expected[field], 'child ' + field)
    equal(base64.b64decode(first['cmdline_base64']),
          b'\0'.join(v.encode() for v in expected['argv']) + b'\0', 'raw argv bytes')

def check_marker(marker, child, expected, challenge):
    require(same(marker['identity'], child['before']), 'marker PID/birth mismatch')
    for field in ('kind', 'token', 'cwd', 'environment', 'exit_code'):
        equal(marker[field], expected[field], 'marker ' + field)
    equal(marker['argv'], expected['argv'][2:], 'witness actual sys.argv')
    equal(marker['challenge'], challenge, 'fresh challenge echo')

def pure_checks(config):
    expected = {'exe': '/python', 'exe_sha256': 'pin', 'argv': ['/python', '-I', '/driver', 'witness', 'C'],
                'cwd': '/cwd', 'kind': 'C', 'token': 'fresh', 'exit_code': 7, 'environment': {'HOME': '/empty'}}
    def fake(pid, parent):
        own = {'pid': pid, 'ppid': parent, 'start_ticks': str(pid * 10), 'state': 'S'}
        return {**expected, 'before': own, 'after': dict(own), 'coherent': True, 'fd0': '/dev/pts/9',
                'cmdline_base64': base64.b64encode(b'\0'.join(v.encode() for v in expected['argv']) + b'\0').decode()}
    child, daemon = fake(41, 40), fake(40, 1)
    public = {'pane_pid': 41, 'daemon_pid': 40}
    marker = {**expected, 'argv': expected['argv'][2:], 'identity': child['before'], 'challenge': 'nonce'}
    check_child(child, copy.deepcopy(child), public, daemon, expected, [daemon])
    check_marker(marker, child, expected, 'nonce')
    checks = ['positive_exact_child_and_challenge']
    def rejected(name, fn):
        try:
            fn()
        except (Unproven, Failed):
            checks.append(name)
        else:
            raise AssertionError('insensitive observer: ' + name)
    for field, value in [('exe', '/wrong'), ('exe_sha256', 'wrong'), ('argv', ['interpreted']), ('cwd', '/wrong'), ('fd0', 'pipe:1')]:
        bad = {**child, field: value}
        rejected('wrong_child_' + field, lambda bad=bad: check_child(bad, bad, public, daemon, expected, [daemon]))
    stale = copy.deepcopy(child)
    stale['before']['start_ticks'] = 'old'
    rejected('stale_birth', lambda: check_child(child, stale, public, daemon, expected, [daemon]))
    rejected('wrong_pane', lambda: check_child(child, child, {**public, 'pane_pid': 42}, daemon, expected, [daemon]))
    rejected('wrong_ancestry', lambda: check_child(child, child, public, daemon, expected, [fake(50, 1)]))
    for field, value in [('kind', 'A'), ('token', 'old'), ('challenge', 'stale'), ('cwd', '/bad'), ('argv', ['bad']), ('environment', {})]:
        rejected('wrong_marker_' + field, lambda field=field, value=value: check_marker({**marker, field: value}, child, expected, 'nonce'))
    rejected('wrong_marker_birth', lambda: check_marker({**marker, 'identity': {'pid': 41, 'start_ticks': 'stale'}}, child, expected, 'nonce'))
    for phase in ('oracle', 'candidate'):
        commands = plan(config, phase)
        require(len(commands) == 6 and len({p[1] for p in commands}) == 6, 'pane population')
        require(commands[0][2][6:10] == ['-x', '80', '-y', '12'], 'explicit geometry')
        require('BASH_ENV' not in environment(config, phase), 'startup contamination')
    good = b''.join(line('a20', i) for i in range(60, 80)) + b'\n'
    metrics = {'width': 80, 'height': 12, 'history_size': 9, 'history_limit': 20}
    inventory(good, 'a20', metrics)
    checks.append('positive_exact_raw_inventory')
    for name, raw in [('foreign_line', good.replace(b'HIST|a20|0060', b'WRONG|a20|0060')),
                      ('gap', good.replace(line('a20', 65), b'')),
                      ('duplicate', good.replace(line('a20', 65), line('a20', 64))),
                      ('missing_final', good.replace(line('a20', 79), b'')),
                      ('trailing_blank_removed', good[:-1]),
                      ('interior_blank', good.replace(line('a20', 65), b'\n'))]:
        rejected('capture_' + name, lambda raw=raw: inventory(raw, 'a20', metrics))
    rejected('capture_wrong_size', lambda: inventory(good, 'a20', {**metrics, 'history_size': 10}))
    rejected('capture_wrong_geometry', lambda: inventory(good, 'a20', {**metrics, 'width': 79}))
    prior = {'raw_base64': base64.b64encode(good).decode(), 'metrics': metrics, 'inventory': inventory(good, 'a20', metrics)}
    changed_limit = copy.deepcopy(prior)
    changed_limit['metrics']['history_limit'] = 40
    unchanged_history(changed_limit, prior)
    checks.append('old_history_limit_only_change_allowed')
    changed_bytes = {**changed_limit, 'raw_base64': base64.b64encode(good[:-1]).decode()}
    rejected('old_history_changed_bytes', lambda: unchanged_history(changed_bytes, prior))
    for key in ('history_size', 'width'):
        bad = copy.deepcopy(changed_limit)
        bad['metrics'][key] += 1
        rejected('old_history_changed_' + key, lambda bad=bad: unchanged_history(bad, prior))
    for key in HISTORY_SPEC:
        check_capture(expected_capture(key), key)
    checks.append('all_prospective_captures_exact')
    for label, original_key in (('a40', 'H2:a40'), ('a40b', 'H3:a40b')):
        key = 'H3:unset-old-' + label
        original = expected_capture(original_key)
        after = expected_capture(key)
        check_transition(after, original, key)
        checks.append('exact_seven_line_truncation_' + label)
        for delta in (-1, 1):
            first = 39 + delta
            raw = b''.join(line(label, i) for i in range(first, 80)) + b'\n'
            metrics = {**after['metrics'], 'history_size': 30 - delta}
            if delta < 0:
                metrics['history_limit'] = 40
            bad = {'raw_base64': base64.b64encode(raw).decode(), 'metrics': metrics,
                   'inventory': inventory(raw, label, metrics)}
            rejected('truncation_missing_or_added_line_' + label + '_' + str(delta),
                     lambda bad=bad, key=key: check_capture(bad, key))
        bad_limit = copy.deepcopy(after)
        bad_limit['metrics']['history_limit'] = 31
        rejected('wrong_actual_grid_limit_' + label, lambda: check_capture(bad_limit, key))
    # Gate serialization must retain every capture field consumed by the candidate.
    fake_record = {'status': 'PASS', 'cases': [{'id': c, 'status': 'PASS'} for c in ('H1', 'H2', 'H3')],
                   'captures': [{'key': k, 'final': expected_capture(k), 'status': 'PASS'} for k in HISTORY_SPEC],
                   'exits': [{'label': k, 'status': 'PASS'} for k in KINDS], 'cleanup': {'status': 'PASS'}}
    gate = oracle_gate(fake_record, 'synthetic-pure-check-only')
    equal(gate['captures'], {k: expected_capture(k) for k in HISTORY_SPEC}, 'gate retains exact captures')
    checks.append('oracle_gate_complete_capture_serialization')
    fake_record['captures'].pop()
    rejected('oracle_gate_missing_final_old_grid', lambda: oracle_gate(fake_record, 'synthetic'))
    return {'at': now(), 'status': 'PASS', 'checks': checks, 'driver_sha256': digest(__file__), 'product_runs': 0}

class Run:
    def __init__(self, config, phase, start):
        self.config, self.phase, self.start = config, phase, start
        self.root = Path(config['root']) / phase
        self.binary, self.socket = config[phase], self.root / 's'
        self.env = environment(config, phase)
        self.case_end = start + 150
        self.cleaning = False
        self.daemon = self.socket_identity = None
        self.owned, self.children = {}, {}
        self.spawn_count = 0
        self.record = {'phase': phase, 'start': now(), 'commands': [], 'cases': [], 'observations': [],
                       'environment': self.env, 'public_samples': [], 'profiles': [], 'marker_ledgers': [], 'exits': [], 'captures': []}
        self.capture_bytes = 0
        self.original_captures = {}
        self.oracle_captures = {} if phase == 'oracle' else json.loads((Path(config['root']) / 'ORACLE-FROZEN.json').read_text())['captures']

    def remaining(self):
        left = min(self.case_end, self.start + (180 if self.cleaning else 150)) - time.monotonic()
        require(left > 0, 'time budget exhausted')
        return left

    def socket_info(self):
        try:
            value = self.socket.stat()
            return {'path': str(self.socket), 'device': value.st_dev, 'inode': value.st_ino, 'socket': stat.S_ISSOCK(value.st_mode)}
        except FileNotFoundError:
            return None

    def profiles(self):
        info = {name: sorted(p.name for p in (self.root / name).iterdir()) for name in ('home-empty', 'xdg-empty')}
        self.record['profiles'].append({'at': now(), 'entries': info})
        require(all(not value for value in info.values()), 'private HOME/XDG not empty')

    def command(self, args, startup=False, required=True, raw=False):
        self.remaining()
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup:
            argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        begin = now()
        p = subprocess.Popen(argv, cwd=self.root, env=self.env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=False)
        born, timed_out = identity(p.pid), False
        try:
            out, err = p.communicate(timeout=self.remaining())
        except subprocess.TimeoutExpired:
            timed_out = True
            if same(born, identity(p.pid)):
                p.kill()
            out, err = p.communicate(timeout=2)
        self.record['commands'].append({'argv': argv, 'start': begin, 'end': now(), 'identity': born,
                                        'exit_code': p.returncode, 'stdout': out.decode('utf-8'), 'stderr': err.decode('utf-8'), 'stdout_base64': base64.b64encode(out).decode(), 'stderr_base64': base64.b64encode(err).decode(), 'timeout': timed_out})
        require(not timed_out, 'command timeout')
        require(len(out) + len(err) < 65536, 'command output cap')
        size = len(json.dumps(self.record)) + sum(p.stat().st_size for p in self.root.rglob('*') if p.is_file())
        require(size < 16 * 1024**2, 'product log cap')
        if required:
            require(p.returncode == 0, 'command failed ' + repr(args) + ': ' + err.decode('utf-8'))
        return out if raw else out.decode('utf-8').strip()

    def public(self, target):
        fields = self.command(['display-message', '-p', '-t', target, FORMAT]).split('|')
        require(len(fields) == 9, 'invalid public identity')
        public = dict(zip(('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_dead', 'pane_dead_status', 'pane_dead_signal'), fields))
        public['daemon_pid'], public['pane_pid'] = int(public['daemon_pid']), int(public['pane_pid'])
        daemon, endpoint = proc(public['daemon_pid']), self.socket_info()
        self.record['public_samples'].append({'at': now(), 'target': target, 'public': public, 'daemon': daemon, 'endpoint': endpoint})
        require(public['socket'] == str(self.socket) and complete(daemon), 'public daemon/socket invalid')
        require(daemon['exe_sha256'] == self.config['hashes'][self.phase if self.phase == 'oracle' else 'daemon'], 'daemon image mismatch')
        require(endpoint and endpoint['socket'], 'not a private socket')
        if self.daemon:
            require(snapshot_same(self.daemon, daemon) and endpoint == self.socket_identity, 'daemon/endpoint changed')
        else:
            self.daemon, self.socket_identity = daemon, endpoint
        self.owned[daemon['before']['pid']] = daemon['before']
        return public, daemon

    def wait_json(self, path):
        while not path.exists():
            time.sleep(min(.02, self.remaining()))
        require(path.stat().st_size < 65536, 'marker size cap')
        return json.loads(path.read_text())

    def expected(self, label):
        kind = KINDS[label]
        return {'kind': kind, 'exit_code': CODES[label], 'token': self.config['tokens'][self.phase],
                'exe': str(Path(self.config['python']).resolve()), 'exe_sha256': self.config['hashes']['python'],
                'argv': witness_argv(self.config, self.phase, label), 'cwd': str(self.root / 'cwd'),
                'environment': {k: self.env.get(k) for k in ENV_KEYS}}

    def observe(self, label, target, reobserve=False):
        public, daemon = self.public(target)
        own = identity(public['pane_pid'])
        require(own, 'public child absent')
        self.owned[own['pid']] = own
        stem = f"{own['pid']}-{own['start_ticks']}"
        markers = self.root / 'markers'
        born_path = markers / ('born-' + stem + '.json')
        born = self.wait_json(born_path)
        self.wait_json(markers / ('emitted-' + stem + '.json'))
        first = proc(own['pid'])
        expected = self.expected(label)
        observation_index = len(self.record['observations'])
        item = {'label': label, 'target': target, 'reobserve': reobserve, 'at': now(), 'expected': expected,
                'public': public, 'first': first, 'born': born, 'born_path': str(born_path),
                'status': 'UNPROVEN'}
        self.record['observations'].append(item)
        try:
            check_marker(born, first, expected, None)
            equal(public['pane_dead'], '0', 'living pane flag')
            equal(public['pane_dead_status'], '', 'living pane exit status')
            equal(public['pane_dead_signal'], '', 'living pane exit signal')
            nonce = secrets.token_hex(24)
            request = {'identity': own, 'challenge': nonce, 'issued_at': now()}
            request_path = markers / (f'request-{stem}-{observation_index}.json')
            item.update(request=request, request_path=str(request_path))
            atomic_once(request_path, request)
            ack_path = markers / request_path.name.replace('request-', 'ack-', 1)
            ack = self.wait_json(ack_path)
            second = proc(own['pid'])
            item.update(ack=ack, ack_path=str(ack_path), child=second)
            public_after, daemon_after = self.public(target)
            item['public_after'] = public_after
            equal(public_after, public, 'public attribution across challenge')
            ancestry, parent = [], second['before']['ppid']
            item['ancestry'] = ancestry
            for _ in range(8):
                ancestor = proc(parent)
                ancestry.append(ancestor)
                require(complete(ancestor), 'missing own ancestor')
                if parent == daemon['before']['pid']:
                    break
                parent = ancestor['before']['ppid']
            check_child(first, second, public, daemon_after, expected, ancestry)
            check_marker(ack, second, expected, nonce)
            if reobserve:
                old = self.children[label]
                require(snapshot_same(old['child'], second) and old['public'] == public, 'existing child mutated')
            else:
                self.children[label] = item
            item['status'] = 'PASS'
            return item
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise

    def options(self, target, global_value, effective, local):
        equal(self.command(['show-options', '-gv', 'history-limit']), str(global_value), 'global history-limit')
        equal(self.command(['show-options', '-Av', '-t', target, 'history-limit']), str(effective), 'effective session history-limit')
        equal(self.command(['show-options', '-qv', '-t', target, 'history-limit']), '' if local is None else str(local), 'local session history-limit')
        equal(self.command(['show-options', '-gv', 'default-command']), '', 'explicit argv guard')

    def capture(self, label, target, key, unchanged=False):
        child = self.children[label]
        latest = next(item for item in reversed(self.record['observations']) if item['label'] == label)
        own = child['child']['before']
        emitted_path = self.root / 'markers' / f"emitted-{own['pid']}-{own['start_ticks']}.json"
        emitted = self.wait_json(emitted_path)
        check_marker(emitted, child['child'], child['expected'], None)
        emission = emitted['emission']
        equal(emission['bytes'], len(payload(label)), 'complete emission bytes')
        equal(emission['lines'], 80, 'complete emission lines')
        equal(emission['sha256'], hashlib.sha256(payload(label)).hexdigest(), 'emission digest')
        equal(latest['ack']['emission'], emission, 'post-emission challenge receipt')
        require(emission['completed_at'] <= latest['request']['issued_at'] <= latest['ack']['at'], 'challenge precedes emission')
        item = {'key': key, 'label': label, 'target': target, 'at': now(), 'public': child['public'],
                'identity': own, 'emitted': emitted, 'emitted_path': str(emitted_path),
                'challenge': latest['request']['challenge'], 'samples': [], 'status': 'UNPROVEN'}
        self.record['captures'].append(item)
        previous = None
        try:
            while True:
                self.remaining()
                raw = self.command(['capture-pane', '-p', '-t', target, '-S', '-', '-E', '-'], raw=True)
                self.capture_bytes += len(raw)
                require(self.capture_bytes <= 65536, 'captured text budget')
                values = self.command(['display-message', '-p', '-t', target,
                    '#{pane_width}|#{pane_height}|#{history_size}|#{history_limit}']).split('|')
                require(len(values) == 4 and all(value.isdigit() for value in values), 'history format API absent')
                metrics = dict(zip(('width', 'height', 'history_size', 'history_limit'), map(int, values)))
                sample = {'at': now(), 'raw_base64': base64.b64encode(raw).decode(), 'metrics': metrics}
                item['samples'].append(sample)
                equal((metrics['width'], metrics['height']), (80, 12), 'capture geometry')
                if line(label, 79)[:-1] not in raw.split(b'\n'):
                    time.sleep(min(.04, self.remaining()))
                    continue
                inv = inventory(raw, label, metrics)
                stable = {'raw_base64': sample['raw_base64'], 'metrics': metrics, 'inventory': inv}
                if previous == stable:
                    item['final'] = stable
                    if unchanged:
                        check_transition(stable, self.original_captures[label], key)
                    else:
                        check_capture(stable, key)
                        self.original_captures[label] = stable
                    if self.phase == 'candidate':
                        require(key in self.oracle_captures, 'capture oracle gate missing key')
                        equal(stable, self.oracle_captures[key], 'exact oracle captured bytes/logical history')
                    item['status'] = 'PASS'
                    return item
                previous = stable
                time.sleep(min(.04, self.remaining()))
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise

    def old_history(self, label, key):
        target = next(row[3] for row in plan(self.config, self.phase) if row[1] == label)
        self.observe(label, target, True)
        self.capture(label, target, key, True)

    def pane_listing(self):
        output = self.command(['list-panes', '-a', '-F', FORMAT])
        rows = []
        for line in output.splitlines():
            fields = line.split('|')
            require(len(fields) == 9, 'invalid pane listing')
            row = dict(zip(('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid',
                            'pane_dead', 'pane_dead_status', 'pane_dead_signal'), fields))
            row['daemon_pid'], row['pane_pid'] = int(row['daemon_pid']), int(row['pane_pid'])
            require(row['daemon_pid'] == self.daemon['before']['pid'] and row['socket'] == str(self.socket),
                    'listing escaped private daemon')
            rows.append(row)
        require(len({row['pane_id'] for row in rows}) == len(rows), 'duplicate pane identity')
        return rows

    def exit_witness(self, label, kept):
        child = self.children[label]
        before = child['child']['before']
        record = {'label': label, 'at': now(), 'expected_kept': kept,
                  'expected_exit_code': CODES[label], 'original_public': child['public'],
                  'original_child': child['child'], 'pane_samples': [], 'status': 'UNPROVEN'}
        self.record['exits'].append(record)
        fd = None
        try:
            current = proc(before['pid'])
            record['before_pidfd'] = current
            require(snapshot_same(child['child'], current), 'child changed before exit request')
            fd = os.pidfd_open(before['pid'], 0)
            record['pidfd'] = {'fd': fd, 'opened_at': now(), 'identity_after_open': identity(before['pid'])}
            require(same(before, record['pidfd']['identity_after_open']), 'pidfd birth mismatch')
            poller = select.poll()
            poller.register(fd, select.POLLIN)
            record['pidfd']['events_before_request'] = poller.poll(0)
            require(not record['pidfd']['events_before_request'], 'child already exited before release')
            challenge = secrets.token_hex(24)
            request = {'identity': before, 'challenge': challenge, 'exit_code': CODES[label], 'issued_at': now()}
            stem = f"{before['pid']}-{before['start_ticks']}"
            release = self.root / 'markers' / ('exit-' + stem + '.json')
            record.update(request=request, request_path=str(release))
            atomic_once(release, request)
            intent_path = self.root / 'markers' / ('exit-intent-' + stem + '.json')
            intent = self.wait_json(intent_path)
            record.update(intent=intent, intent_path=str(intent_path))
            check_marker(intent, child['child'], child['expected'], challenge)
            events = poller.poll(max(1, int(self.remaining() * 1000)))
            record['pidfd'].update(events_after_request=events, observed_at=now())
            require(any(event_fd == fd and mask & (select.POLLIN | select.POLLHUP) for event_fd, mask in events),
                    'no kernel child-exit event')
            after = identity(before['pid'])
            record['identity_after_exit_event'] = after
            require(not same(before, after) or after['state'] == 'Z', 'exited child still live')
            previous = None
            while True:
                self.remaining()
                rows = self.pane_listing()
                current = identity(before['pid'])
                sample = {'at': now(), 'rows': rows, 'child_identity': current}
                record['pane_samples'].append(sample)
                require(not same(before, current) or current['state'] == 'Z', 'dead pane attributes a living child birth')
                matches = [row for row in rows if row['pane_id'] == child['public']['pane_id']]
                ready = not matches if not kept else bool(matches and matches[0]['pane_dead'] == '1')
                if kept and not matches:
                    raise Failed('remain-on-exit on pane disappeared')
                if kept and ready:
                    row = matches[0]
                    for field in ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid'):
                        equal(row[field], child['public'][field], 'dead pane identity ' + field)
                    equal(row['pane_dead_status'], str(CODES[label]), 'actual public exit status')
                    equal(row['pane_dead_signal'], '', 'normal exit must have no signal')
                stable = matches[0] if kept and ready else ('ABSENT' if ready else None)
                if ready and previous == stable:
                    record['status'] = 'PASS'
                    return record
                previous = stable
                time.sleep(min(.05, self.remaining()))
        except Exception as error:
            record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise
        finally:
            if fd is not None:
                os.close(fd)
                record['pidfd']['closed_at'] = now()

    def spawn(self, label):
        rows = [row for row in plan(self.config, self.phase) if row[1] == label]
        require(len(rows) == 1 and self.spawn_count < 6, 'pane child budget or plan mismatch')
        case, _, args, target = rows[0]
        self.spawn_count += 1
        self.command(args, startup=label == 'a20')
        observed = self.observe(label, target)
        self.capture(label, target, case + ':' + label)
        return observed

    def ledger(self):
        entries = [{'path': str(p), 'data': json.loads(p.read_text())}
                   for p in sorted((self.root / 'markers').glob('born-*.json'))]
        self.record['marker_ledgers'].append({'at': now(), 'entries': entries})
        equal(len(entries), self.spawn_count, 'unexpected witness execution count')
        equal({entry['path'] for entry in entries}, {c['born_path'] for c in self.children.values()},
              'unattributed witness marker')

    def execute(self):
        require(not self.socket.exists() and not list((self.root / 'markers').iterdir()), 'fixture is not fresh')
        self.profiles()
        for case in ('H1', 'H2', 'H3'):
            self.case_end = min(time.monotonic() + 20, self.start + 150)
            entry = {'id': case, 'start': now(), 'monotonic_start': time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case == 'H1':
                    self.spawn('a20')
                    self.options('alpha', 20, 20, None)
                elif case == 'H2':
                    self.command(['set-option', '-t', 'alpha', 'history-limit', '40'])
                    self.options('alpha', 20, 40, 40)
                    self.spawn('a40')
                    self.spawn('b20')
                    self.options('beta', 20, 20, None)
                    self.old_history('a20', 'H2:old-a20')
                else:
                    self.command(['set-option', '-g', 'history-limit', '30'])
                    self.options('alpha', 30, 40, 40)
                    self.options('beta', 30, 30, None)
                    self.spawn('a40b')
                    for label in ('a20', 'a40', 'b20'):
                        self.old_history(label, 'H3:global-old-' + label)
                    self.command(['source-file', str(self.root / 'live.conf')])
                    self.options('alpha', 30, 30, None)
                    self.spawn('a30')
                    self.spawn('b30')
                    for label in ('a20', 'a40', 'b20', 'a40b'):
                        self.old_history(label, 'H3:unset-old-' + label)
                    for label in KINDS:
                        self.exit_witness(label, True)
                self.ledger()
                self.profiles()
                self.remaining()
                entry['status'] = 'PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
                raise
            finally:
                entry.update(end=now(), elapsed_seconds=time.monotonic() - entry['monotonic_start'])

    def cleanup(self):
        start = time.monotonic()
        self.cleaning = True
        self.case_end = min(start + 15, self.start + 180)
        result = {'start': now(), 'actions': [], 'reaped': [], 'ownership_discovery': []}
        self.record['cleanup'] = result
        def descendants(pid, seen=None):
            seen = set() if seen is None else seen
            children = set()
            try:
                for task in Path('/proc', str(pid), 'task').iterdir():
                    try:
                        children.update(int(v) for v in (task / 'children').read_text().split())
                    except FileNotFoundError:
                        pass
            except FileNotFoundError:
                pass
            children -= seen
            seen.update(children)
            return list(children) + [p for c in children for p in descendants(c, seen)]
        for pid in descendants(os.getpid()):
            own = identity(pid)
            if own:
                result['ownership_discovery'].append(own)
                self.owned[pid] = own
        if self.daemon:
            daemon, endpoint = proc(self.daemon['before']['pid']), self.socket_info()
            result.update(daemon_before_kill_server=daemon, endpoint_before_kill_server=endpoint)
            if snapshot_same(self.daemon, daemon) and endpoint == self.socket_identity:
                try:
                    self.command(['kill-server'], required=False)
                    result['actions'].append('qualified private kill-server')
                except Exception as error:
                    result['kill_server_error'] = repr(error)
        def reap_for(seconds):
            end = min(time.monotonic() + seconds, self.case_end)
            while time.monotonic() < end:
                try:
                    while True:
                        pid, status = os.waitpid(-1, os.WNOHANG)
                        if not pid:
                            break
                        result['reaped'].append({'pid': pid, 'status': status})
                except ChildProcessError:
                    pass
                if all(not same(old, identity(pid)) for pid, old in self.owned.items()):
                    break
                time.sleep(.02)
        reap_for(2)
        for sig in (signal.SIGTERM, signal.SIGKILL):
            for pid, old in self.owned.items():
                current = identity(pid)
                if same(old, current) and current['state'] != 'Z':
                    os.kill(pid, sig)
                    result['actions'].append({'signal': int(sig), 'before': old, 'requalified': current})
            reap_for(1)
        result['identities_after'] = [{'before': old, 'after': identity(pid)} for pid, old in self.owned.items()]
        result['all_tracked_exited'] = all(not same(v['before'], v['after']) for v in result['identities_after'])
        endpoint = self.socket_info()
        if endpoint and result['all_tracked_exited'] and endpoint == self.socket_identity:
            self.socket.unlink()
            result['stale_owned_socket_removed'] = endpoint
        result.update(socket_after=self.socket_info(), owned_descendants_remaining=descendants(os.getpid()), end=now(), elapsed_seconds=time.monotonic() - start)
        self.profiles()
        require(result['all_tracked_exited'] and result['socket_after'] is None and not result['owned_descendants_remaining'], 'cleanup incomplete')
        require(result['elapsed_seconds'] <= 15, 'cleanup budget')
        result['status'] = 'PASS'

def prepare(config):
    root = Path(config['root'])
    require(root.is_dir(), 'fixture root must exist')
    for phase in ('oracle', 'candidate'):
        phase_root = root / phase
        phase_root.mkdir()
        for name in ('home-empty', 'xdg-empty', 'markers', 'cwd'):
            (phase_root / name).mkdir()
        (phase_root / 'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-window-option -g remain-on-exit on\nset-option -g history-limit 20\n")
        (phase_root / 'live.conf').write_text('set-option -u -t alpha history-limit\n')
    write_once(root / 'PLAN.json', {phase: {'commands': plan(config, phase), 'environment': environment(config, phase),
                                          'expected_kinds': KINDS, 'expected_exit_codes': CODES}
                                   for phase in ('oracle', 'candidate')})
    write_once(root / 'PURE-CHECKS.json', pure_checks(config))

def execute_lot(config, config_path):
    root = Path(config['root'])
    frozen = json.loads((root / 'FROZEN-NATIVE.json').read_text())
    artifacts = {v['path']: v['sha256'] for v in frozen['artifacts']}
    mandatory = [str(Path(__file__).resolve()), str(Path(config_path).resolve()), str(root / 'PLAN.json'), str(root / 'PURE-CHECKS.json')]
    mandatory += [config[k] for k in ('oracle', 'candidate', 'daemon', 'python')]
    mandatory += [str(root / p / f) for p in ('oracle', 'candidate') for f in ('startup.conf', 'live.conf')]
    require(all(p in artifacts for p in mandatory), 'freeze misses required artifacts')
    for path, sha in artifacts.items():
        require(digest(path) == sha, 'frozen artifact changed ' + path)
    for key, sha in config['hashes'].items():
        require(digest(config[key]) == sha, 'binary pin mismatch: ' + key)
    pure = json.loads((root / 'PURE-CHECKS.json').read_text())
    require(pure['status'] == 'PASS' and pure['driver_sha256'] == digest(__file__), 'pure controls absent or stale')
    require(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0, 'subreaper unavailable')
    write_once(root / 'STARTED.json', {'at': now(), 'identity': proc(os.getpid())})
    start = time.monotonic()
    lot = {'start': now(), 'phases': [], 'frozen_sha256': digest(root / 'FROZEN-NATIVE.json'), 'fd_before': fd_snapshot()}
    for phase in ('oracle', 'candidate'):
        if phase == 'candidate':
            gate = json.loads((root / 'ORACLE-FROZEN.json').read_text())
            require(gate['status'] == 'PASS' and gate['expected_kinds'] == KINDS and gate['driver_sha256'] == digest(__file__), 'oracle gate not PASS or driver changed')
            require(digest(root / 'oracle/RESULT.json') == gate['result_sha256'], 'oracle proof changed')
        run = Run(config, phase, start)
        try:
            run.execute()
            run.record['status'] = 'PASS'
        except Exception as error:
            run.record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error), traceback=traceback.format_exc())
        finally:
            try:
                run.cleanup()
            except Exception as error:
                run.record.update(status='UNPROVEN', cleanup_error=repr(error))
            run.record.update(end=now(), spawn_commands=run.spawn_count)
            write_once(run.root / 'RESULT.json', run.record)
        lot['phases'].append({'phase': phase, 'status': run.record['status'], 'result_sha256': digest(run.root / 'RESULT.json')})
        print(json.dumps(lot['phases'][-1]), flush=True)
        if run.record['status'] != 'PASS':
            break
        if phase == 'oracle':
            write_once(root / 'ORACLE-FROZEN.json', oracle_gate(run.record, digest(run.root / 'RESULT.json')))
    lot.update(end=now(), elapsed_seconds=time.monotonic() - start, fd_after=fd_snapshot())
    lot['status'] = 'PASS' if len(lot['phases']) == 2 and all(p['status'] == 'PASS' for p in lot['phases']) else 'STOPPED'
    if lot['fd_before'] != lot['fd_after'] or lot['elapsed_seconds'] > 180:
        lot['status'] = 'UNPROVEN'
    write_once(root / 'LOT.json', lot)
    print(json.dumps(lot), flush=True)
    return 0 if lot['status'] == 'PASS' else 1

if __name__ == '__main__':
    if len(sys.argv) == 6 and sys.argv[1] == 'witness':
        witness(*sys.argv[2:6])
    elif len(sys.argv) == 3 and sys.argv[1] in ('prepare', 'run'):
        config = json.loads(Path(sys.argv[2]).read_text())
        if sys.argv[1] == 'prepare':
            prepare(config)
        else:
            sys.exit(execute_lot(config, sys.argv[2]))
    else:
        raise SystemExit(__doc__)
