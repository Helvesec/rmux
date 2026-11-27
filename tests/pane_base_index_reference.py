#!/usr/bin/env python3
"""Bounded Linux pane-base-index reference: P1 global, P2 local, P3 numeric input,
P4 source-file unset and split. Uses a fresh full tmux oracle before RMUX.

Usage: python3 -I tests/pane_base_index_reference.py prepare|run CONFIG.json
CONFIG provides root, oracle, candidate, daemon, python, bash, hashes and tokens.
FROZEN-NATIVE.json pins this file, inputs, binaries and pure controls before run.
The standalone observer is derived from integrated L6-019, without importing020.
Only private helpers receive literal fresh hex nonces and Enter through send-keys.
Each helper reads its own canonical PTY and records actual bytes, separately from
filesystem liveness challenges. Private child births exit through a fresh release
request; held pidfds attest exit only, never nonparent numeric status.
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
import termios

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{pane_index}|#{window_index}|#{window_width}|#{window_height}'
FIELDS = ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid',
          'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'pane_index', 'window_index', 'window_width', 'window_height')
KINDS = {v: v for v in ('alpha1', 'alpha2', 'beta1', 'beta2', 'alpha3')}
CODES = {v: 0 for v in KINDS}
EXIT_ORDER = ('alpha3', 'alpha2', 'alpha1', 'beta2', 'beta1')
EXPECTED = {'P1': {'alpha1': 4, 'alpha2': 5},
            'P2': {'alpha1': 7, 'alpha2': 8, 'beta1': 4, 'beta2': 5},
            'P3': {'alpha1': 7, 'alpha2': 8, 'beta1': 4, 'beta2': 5},
            'P4': {'alpha1': 4, 'alpha2': 5, 'alpha3': 6, 'beta1': 4, 'beta2': 5}}

ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_PANEINDEX_MARKER', 'BASH_ENV', 'ENV',
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
                      fd0=os.readlink(p / 'fd/0'), cwd=os.readlink(p / 'cwd'), signal_status_raw=(p / 'status').read_text())
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

def witness(kind, root, token, exit_code):
    root = Path(root)
    exit_code = int(exit_code)
    require(kind in KINDS and exit_code == 0, 'unplanned witness')
    tty = termios.tcgetattr(0)
    require(bool(tty[3] & termios.ICANON) and bool(tty[0] & termios.ICRNL), 'canonical PTY with CR-to-NL required')
    tty_state = {'icanon': bool(tty[3] & termios.ICANON), 'icrnl': bool(tty[0] & termios.ICRNL)}
    own = identity(os.getpid())
    stem = f"{own['pid']}-{own['start_ticks']}"
    received, buffer = [], b''
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'exit_code': exit_code, 'identity': identity(os.getpid()),
                'cwd': os.getcwd(), 'argv': sys.argv, 'tty': tty_state,
                'received': list(received), 'pending_bytes_base64': base64.b64encode(buffer).decode(),
                'environment': {k: os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root / ('born-' + stem + '.json'), report(None))
    end = time.monotonic() + 180
    while time.monotonic() < end:
        while select.select([0], [], [], 0)[0]:
            data = os.read(0, 4096)
            require(data, 'stdin EOF before release')
            buffer += data
            require(len(buffer) <= 4096 and len(received) < 8, 'stdin cap')
            while b'\n' in buffer:
                line, buffer = buffer.split(b'\n', 1)
                require(len(line) == 48 and all(c in b'0123456789abcdef' for c in line), 'unplanned input')
                received.append(line.decode('ascii'))
                record = report(None)
                record['stdin_bytes_base64'] = base64.b64encode(line + b'\n').decode()
                atomic_once(root / (f'stdin-{stem}-{len(received)}.json'), record)
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
            equal(request['exit_code'], exit_code, 'fixed witness exit intent')
            atomic_once(root / ('exit-intent-' + stem + '.json'), report(request['challenge']))
            os._exit(exit_code)
        time.sleep(.02)
    os._exit(124)

def environment(config, phase):
    root = Path(config['root']) / phase
    return {'HOME': str(root / 'home-empty'), 'XDG_CONFIG_HOME': str(root / 'xdg-empty'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/dash', 'TERM': 'vt100', 'LANG': 'C.UTF-8',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_PANEINDEX_MARKER': config['tokens'][phase]}

def witness_argv(config, phase, label):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', KINDS[label],
            str(Path(config['root']) / phase / 'markers'), config['tokens'][phase], str(CODES[label])]

def plan(config, phase):
    cwd = str(Path(config['root']) / phase / 'cwd')
    def argv(label): return witness_argv(config, phase, label)
    def session(name, label):
        return ['new-session', '-d', '-s', name, '-x', '80', '-y', '24', '-c', cwd, *argv(label)]
    def split(target, label):
        return ['split-window', '-d', '-P', '-F', '#{pane_id}', '-t', target, '-c', cwd, *argv(label)]
    return [('P1', 'alpha1', session('alpha', 'alpha1'), 'alpha:0.4'),
            ('P1', 'alpha2', split('alpha:0.4', 'alpha2'), 'PRINTED_PANE_ID'),
            ('P2', 'beta1', session('beta', 'beta1'), 'beta:0.4'),
            ('P2', 'beta2', split('beta:0.4', 'beta2'), 'PRINTED_PANE_ID'),
            ('P4', 'alpha3', split('alpha:0.5', 'alpha3'), 'PRINTED_PANE_ID')]

def stable_public(a, b):
    return {k: v for k, v in a.items() if k != 'pane_index'} == {k: v for k, v in b.items() if k != 'pane_index'}

def check_indices(rows, children, expected):
    equal(len(rows), len(expected), 'exact pane inventory')
    require(len({r['pane_id'] for r in rows}) == len(rows), 'duplicate pane ID')
    require(len({(r['window_id'], r['pane_index']) for r in rows}) == len(rows), 'duplicate window pane index')
    require(len({r['session_id'] for r in rows}) <= 2 and len({r['window_id'] for r in rows}) <= 2, 'session/window cap')
    for label, index in expected.items():
        original = children[label]['public']
        found = [r for r in rows if r['pane_id'] == original['pane_id']]
        require(len(found) == 1, 'missing stable pane ID')
        row = found[0]
        require(stable_public(original, row), 'pane attribution changed')
        equal(row['pane_index'], str(index), 'visible pane index ' + label)
        equal((row['window_index'], row['window_width'], row['window_height']), ('0', '80', '24'), 'fixed window geometry/index')
        equal(row['pane_dead'], '0', 'pane still alive')

def delivery_snapshot(received):
    return copy.deepcopy(received)

def check_delivery(received, before, target_label, nonce):
    equal(set(received), set(before), 'delivery population')
    require(target_label in before and all(nonce not in v for v in before.values()), 'unknown target or stale input nonce')
    for label in before:
        equal(received[label], before[label] + ([nonce] if label == target_label else []), 'actual input delivery ' + label)


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



def check_cleanup(result, owned):
    actual = {(v['before']['pid'], v['before']['start_ticks']) for v in result['identities_after']}
    expected = {(v['pid'], v['start_ticks']) for v in owned.values()}
    equal(actual, expected, 'cleanup identities complete')
    require(all(not same(v['before'], v['after']) for v in result['identities_after']), 'cleanup surviving birth')
    require(result['all_tracked_exited'] and result['socket_after'] is None and not result['owned_descendants_remaining'], 'cleanup incomplete')

def signal_owned(old, signum):
    # Fallback cleanup and timed-out own clients also use qualified pidfds only.
    current = identity(old['pid'])
    require(same(old, current) and current['state'] != 'Z', 'signal target no longer owns birth')
    fd = os.pidfd_open(old['pid'], 0)
    event = {'before': old, 'signal': int(signum), 'fd': fd, 'opened_at': now()}
    try:
        event['after_open'] = identity(old['pid'])
        require(same(old, event['after_open']), 'signal pidfd stale birth')
        signal.pidfd_send_signal(fd, signum, None, 0)
        event['sent_at'] = now()
    finally:
        os.close(fd)
        event['closed_at'] = now()
    return event

def oracle_gate(record, sha):
    equal(record['status'], 'PASS', 'complete oracle required')
    equal([c['id'] for c in record['cases']], list(EXPECTED), 'complete oracle cases')
    require(all(c['status'] == 'PASS' for c in record['cases']), 'oracle case failed')
    equal([e['label'] for e in record['exits']], list(EXIT_ORDER), 'complete oracle exit coverage')
    require(all(e['status'] == 'PASS' for e in record['exits']), 'oracle exit failure')
    equal(record['spawn_commands'], 5, 'complete oracle child coverage')
    equal(len(record['deliveries']), 3, 'oracle numeric/ID deliveries')
    require(all(d['status'] == 'PASS' for d in record['deliveries']), 'oracle target effect incomplete')
    equal(record['cleanup']['status'], 'PASS', 'oracle cleanup')
    return {'at': now(), 'status': 'PASS', 'expected_kinds': KINDS, 'expected': EXPECTED,
            'driver_sha256': digest(__file__), 'result_sha256': sha,
            'deliveries': [{'case': d['case'], 'address_kind': d['address_kind'], 'target_label': d['target_label']} for d in record['deliveries']]}

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
        require(len(commands) == 5 and len({p[1] for p in commands}) == 5, 'pane population')
        require('BASH_ENV' not in environment(config, phase), 'startup contamination')
    row = {'daemon_pid': 40, 'socket': '/s', 'session_id': '$0', 'window_id': '@0', 'pane_id': '%0',
           'pane_pid': 41, 'pane_dead': '0', 'pane_dead_status': '', 'pane_dead_signal': '',
           'pane_index': '4', 'window_index': '0', 'window_width': '80', 'window_height': '24'}
    children = {'alpha1': {'public': row}}
    check_indices([row], children, {'alpha1': 4}); checks.append('positive_index')
    rejected('wrong_index', lambda: check_indices([{**row, 'pane_index': '5'}], children, {'alpha1': 4}))
    rejected('duplicate_pane', lambda: check_indices([row, row], {**children, 'alpha2': children['alpha1']}, {'alpha1': 4, 'alpha2': 5}))
    row2 = {**row, 'pane_id': '%1', 'pane_pid': 42}
    rejected('duplicate_index', lambda: check_indices([row, row2], {**children, 'alpha2': {'public': row2}}, {'alpha1': 4, 'alpha2': 5}))
    rejected('wrong_stable_identity', lambda: check_indices([{**row, 'pane_pid': 42}], children, {'alpha1': 4}))
    old = {'alpha1': [], 'alpha2': []}; delivered = {'alpha1': [], 'alpha2': ['nonce']}
    check_delivery(delivered, old, 'alpha2', 'nonce'); checks.append('positive_exclusive_delivery')
    saved = delivery_snapshot(delivered)
    delivered['alpha3'] = []
    delivered['alpha2'].append('later')
    equal(saved, {'alpha1': [], 'alpha2': ['nonce']}, 'delivery snapshot must not alias later state')
    checks.append('delivery_snapshot_immutable_after_population_and_input_change')
    delivered = {'alpha1': [], 'alpha2': ['nonce']}
    rejected('incorrect_target', lambda: check_delivery({'alpha1': ['nonce'], 'alpha2': []}, old, 'alpha2', 'nonce'))
    rejected('broadcast_input', lambda: check_delivery({'alpha1': ['nonce'], 'alpha2': ['nonce']}, old, 'alpha2', 'nonce'))
    rejected('stale_input_nonce', lambda: check_delivery(delivered, delivered, 'alpha2', 'nonce'))
    rejected('wrong_input_nonce', lambda: check_delivery({'alpha1': [], 'alpha2': ['old']}, old, 'alpha2', 'nonce'))
    rejected('missing_target_evidence', lambda: check_delivery({'alpha2': ['nonce']}, old, 'alpha2', 'nonce'))
    clean = {'identities_after': [{'before': child['before'], 'after': None}], 'all_tracked_exited': True, 'socket_after': None, 'owned_descendants_remaining': []}
    check_cleanup(clean, {41: child['before']});checks.append('cleanup_exact')
    rejected('cleanup_missing_identity', lambda: check_cleanup({**clean, 'identities_after': []}, {41: child['before']}))
    rejected('cleanup_surviving_birth', lambda: check_cleanup({**clean, 'identities_after': [{'before': child['before'], 'after': child['before']}]}, {41: child['before']}))
    rejected('cleanup_socket_present', lambda: check_cleanup({**clean, 'socket_after': {}}, {41: child['before']}))
    rejected('cleanup_false_flag', lambda: check_cleanup({**clean, 'all_tracked_exited': False}, {41: child['before']}))
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
        self.received = {}
        self.all_nonces = set()
        self.record = {'phase': phase, 'start': now(), 'commands': [], 'cases': [], 'observations': [],
                       'environment': self.env, 'public_samples': [], 'profiles': [], 'marker_ledgers': [], 'exits': [], 'inventories': [], 'deliveries': []}

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

    def command(self, args, startup=False, required=True):
        self.remaining()
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup:
            argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        begin = now()
        p = subprocess.Popen(argv, cwd=self.root, env=self.env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        born, timed_out = identity(p.pid), False
        try:
            out, err = p.communicate(timeout=self.remaining())
        except subprocess.TimeoutExpired:
            timed_out = True
            if same(born, identity(p.pid)):
                signal_owned(born, signal.SIGKILL)
            out, err = p.communicate(timeout=2)
        self.record['commands'].append({'argv': argv, 'start': begin, 'end': now(), 'identity': born,
                                        'exit_code': p.returncode, 'stdout': out, 'stderr': err, 'timeout': timed_out})
        require(not timed_out, 'command timeout')
        require(len(out) + len(err) < 65536, 'command output cap')
        size = len(json.dumps(self.record)) + sum(p.stat().st_size for p in self.root.rglob('*') if p.is_file())
        require(size < 3500000, 'product log cap leaving combined lot below8MiB')
        if required:
            require(p.returncode == 0, 'command failed ' + repr(args) + ': ' + err)
        return out.strip()

    def public(self, target):
        fields = self.command(['display-message', '-p', '-t', target, FORMAT]).split('|')
        require(len(fields) == len(FIELDS), 'invalid public identity')
        public = dict(zip(FIELDS, fields))
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
        nonce = self.fresh_nonce()
        public, daemon = self.public(target)
        own = identity(public['pane_pid'])
        require(own, 'public child absent')
        self.owned[own['pid']] = own
        stem = f"{own['pid']}-{own['start_ticks']}"
        markers = self.root / 'markers'
        born_path = markers / ('born-' + stem + '.json')
        born = self.wait_json(born_path)
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
            equal(ack['tty'], {'icanon': True, 'icrnl': True}, 'helper PTY mode')
            equal(ack['pending_bytes_base64'], '', 'unexpected partial input')
            if reobserve:
                old = self.children[label]
                require(snapshot_same(old['child'], second) and stable_public(old['public'], public), 'existing child mutated')
            else:
                self.children[label] = item
            item['status'] = 'PASS'
            return item
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise

    def fresh_nonce(self):
        nonce = secrets.token_hex(24)
        require(nonce not in self.all_nonces, 'duplicate nonce')
        self.all_nonces.add(nonce)
        return nonce

    def options(self, target, effective, local):
        equal(self.command(['show-options', '-wgv', 'pane-base-index']), '4', 'global window pane-base-index')
        equal(self.command(['show-options', '-wAv', '-t', target, 'pane-base-index']), str(effective), 'effective window pane-base-index')
        equal(self.command(['show-options', '-wqv', '-t', target, 'pane-base-index']), '' if local is None else str(local), 'local window pane-base-index')
        equal(self.command(['show-options', '-gv', 'base-index']), '0', 'fixed base-index')
        equal(self.command(['show-options', '-gv', 'renumber-windows']), 'off', 'fixed renumber-windows')

    def inventory(self, case):
        samples = []
        self.record['inventories'].append({'case': case, 'expected': EXPECTED[case], 'samples': samples})
        for _ in range(2):
            rows = self.pane_listing()
            samples.append({'at': now(), 'rows': rows})
            check_indices(rows, self.children, EXPECTED[case])
            time.sleep(min(.03, self.remaining()))
        equal(samples[0]['rows'], samples[1]['rows'], 'stable pane inventory')

    def delivery(self, case, target, address_kind):
        nonce = self.fresh_nonce()
        label = 'alpha2'
        child = self.children[label]
        old = copy.deepcopy(self.received)
        public, _ = self.public(target)
        require(stable_public(child['public'], public), 'numeric/ID address resolved to incorrect pane')
        equal(public['pane_index'], str(EXPECTED[case][label]), 'current numeric index')
        entry = {'case': case, 'at': now(), 'target': target, 'address_kind': address_kind,
                 'target_label': label, 'nonce': nonce, 'public_before': public, 'received_before': old, 'status': 'UNPROVEN'}
        self.record['deliveries'].append(entry)
        self.command(['send-keys', '-t', target, '-l', nonce])
        self.command(['send-keys', '-t', target, 'Enter'])
        own = child['child']['before'];stem = f"{own['pid']}-{own['start_ticks']}"
        path = self.root / 'markers' / (f'stdin-{stem}-{len(old[label])+1}.json')
        receipt = self.wait_json(path)
        entry.update(receipt=receipt, receipt_path=str(path))
        check_marker(receipt, child['child'], child['expected'], None)
        equal(base64.b64decode(receipt['stdin_bytes_base64']), (nonce + '\n').encode(), 'actual PTY bytes')
        observed = {}
        entry['observation_indices'] = []
        for name, original in self.children.items():
            index = len(self.record['observations'])
            item = self.observe(name, original['public']['pane_id'], True)
            entry['observation_indices'].append(index)
            observed[name] = item['ack']['received']
        entry['received_after'] = observed
        check_delivery(observed, old, label, nonce)
        self.received = delivery_snapshot(observed)
        after, _ = self.public(target)
        equal(after, public, 'target stable after actual delivery')
        entry.update(public_after=after, status='PASS', end=now())

    def pane_listing(self):
        output = self.command(['list-panes', '-a', '-F', FORMAT])
        rows = []
        for line in output.splitlines():
            fields = line.split('|')
            require(len(fields) == len(FIELDS), 'invalid pane listing')
            row = dict(zip(FIELDS, fields))
            row['daemon_pid'], row['pane_pid'] = int(row['daemon_pid']), int(row['pane_pid'])
            require(row['daemon_pid'] == self.daemon['before']['pid'] and row['socket'] == str(self.socket),
                    'listing escaped private daemon')
            rows.append(row)
        require(len({row['pane_id'] for row in rows}) == len(rows), 'duplicate pane identity')
        return rows

    def exit_witness(self, label):
        child = self.children[label]
        before = child['child']['before']
        record = {'label': label, 'at': now(), 'numeric_status_claimed': False,
                  'expected_exit_code': CODES[label], 'original_public': child['public'],
                  'original_child': child['child'], 'status': 'UNPROVEN'}
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
            challenge = self.fresh_nonce()
            request = {'identity': before, 'challenge': challenge, 'exit_code': CODES[label], 'issued_at': now()}
            stem = f"{before['pid']}-{before['start_ticks']}"
            release = self.root / 'markers' / ('exit-' + stem + '.json')
            record.update(request=request, request_path=str(release))
            atomic_once(release, request)
            intent_path = self.root / 'markers' / ('exit-intent-' + stem + '.json')
            intent = self.wait_json(intent_path)
            record.update(intent=intent, intent_path=str(intent_path))
            check_marker(intent, child['child'], child['expected'], challenge)
            equal(intent['received'], self.received[label], 'final exclusive input ledger')
            equal(intent['pending_bytes_base64'], '', 'no partial input at exit')
            events = poller.poll(max(1, int(self.remaining() * 1000)))
            record['pidfd'].update(events_after_request=events, observed_at=now())
            require(any(event_fd == fd and mask & (select.POLLIN | select.POLLHUP) for event_fd, mask in events),
                    'no kernel child-exit event')
            after = identity(before['pid'])
            record['identity_after_exit_event'] = after
            require(not same(before, after) or after['state'] == 'Z', 'exited child still live')
            record['status'] = 'PASS'
            return record
        except Exception as error:
            record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise
        finally:
            if fd is not None:
                os.close(fd)
                record['pidfd']['closed_at'] = now()

    def spawn(self, label):
        rows = [row for row in plan(self.config, self.phase) if row[1] == label]
        require(len(rows) == 1 and self.spawn_count < 5, 'pane child budget or plan mismatch')
        case, _, args, target = rows[0]
        self.spawn_count += 1
        output = self.command(args, startup=label == 'alpha1')
        if target == 'PRINTED_PANE_ID':
            require(output.startswith('%') and output[1:].isdigit(), 'new split pane ID absent')
            target = output
        observed = self.observe(label, target)
        equal(observed['public']['pane_index'], str(EXPECTED[case][label]), 'new pane actual index')
        equal(observed['ack']['received'], [], 'new helper starts without input')
        self.received[label] = []
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
        for case in EXPECTED:
            self.case_end = min(time.monotonic() + 20, self.start + 150)
            entry = {'id': case, 'start': now(), 'monotonic_start': time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case == 'P1':
                    self.spawn('alpha1')
                    self.options('alpha:0', 4, None)
                    self.spawn('alpha2')
                elif case == 'P2':
                    self.spawn('beta1'); self.spawn('beta2')
                    self.command(['set-window-option', '-t', 'alpha:0', 'pane-base-index', '7'])
                    self.options('alpha:0', 7, 7); self.options('beta:0', 4, None)
                    for label, child in self.children.items():
                        self.observe(label, child['public']['pane_id'], True)
                elif case == 'P3':
                    self.delivery(case, 'alpha:0.8', 'numeric')
                else:
                    self.command(['source-file', str(self.root / 'live.conf')])
                    self.options('alpha:0', 4, None); self.options('beta:0', 4, None)
                    for label, child in self.children.items():
                        self.observe(label, child['public']['pane_id'], True)
                    self.delivery(case, 'alpha:0.5', 'numeric')
                    self.delivery(case, self.children['alpha2']['public']['pane_id'], 'stable-pane-ID')
                    self.spawn('alpha3')
                    self.observe('alpha2', self.children['alpha2']['public']['pane_id'], True)
                self.inventory(case)
                self.ledger(); self.profiles(); self.remaining()
                entry['status'] = 'PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
                raise
            finally:
                entry.update(end=now(), elapsed_seconds=time.monotonic() - entry['monotonic_start'])
        self.case_end = min(time.monotonic() + 15, self.start + 150)
        for label in EXIT_ORDER:
            self.exit_witness(label)
        self.ledger()

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
                    result['actions'].append(signal_owned(old, sig))
            reap_for(1)
        result['identities_after'] = [{'before': old, 'after': identity(pid)} for pid, old in self.owned.items()]
        result['all_tracked_exited'] = all(not same(v['before'], v['after']) for v in result['identities_after'])
        endpoint = self.socket_info()
        if endpoint and result['all_tracked_exited'] and endpoint == self.socket_identity:
            self.socket.unlink()
            result['stale_owned_socket_removed'] = endpoint
        result.update(socket_after=self.socket_info(), owned_descendants_remaining=descendants(os.getpid()), end=now(), elapsed_seconds=time.monotonic() - start)
        self.profiles()
        check_cleanup(result, self.owned)
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
        (phase_root / 'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-window-option -g pane-base-index 4\nset-option -g base-index 0\nset-option -g renumber-windows off\nset-window-option -g remain-on-exit off\n")
        (phase_root / 'live.conf').write_text('set-window-option -u -t alpha:0 pane-base-index\n')
    write_once(root / 'PLAN.json', {phase: {'commands': plan(config, phase), 'environment': environment(config, phase),
                                          'expected_kinds': KINDS, 'expected_exit_intents': CODES, 'expected_indices': EXPECTED, 'send_targets': ['alpha:0.8', 'alpha:0.5', 'alpha2 stable paneID']}
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
    lot['log_bytes_before_lot'] = sum(p.stat().st_size for p in root.rglob('*') if p.is_file())
    if lot['log_bytes_before_lot'] + len(json.dumps(lot, indent=2)) + 1 > 8 * 1024**2:
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
