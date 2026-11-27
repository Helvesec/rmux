#!/usr/bin/env python3
"""Bounded Linux renumber-windows N1-N4 reference against fresh tmux3.7b.

Usage: python3 -I tests/renumber_windows_reference.py prepare|run CONFIG.json
CONFIG pins root, absolute oracle/candidate/daemon/python/bash binaries and tokens.
FROZEN-NATIVE.json pins this source, prepared files, binaries and pure controls.
Standalone observer derived from accepted/integrated020;021 remains untouched.
Before RMUX, complete oracle evidence freezes all intermediate index mappings.
Only private attributed windows are closed by public windowID; held qualified
pidfds prove owned helper exits. Literal nonce input is read from each ownPTY,
separately from filesystem liveness challenges. No numeric exit status inferred.
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

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{session_name}|#{window_index}|#{window_name}|#{window_width}|#{window_height}'
PUBLIC_KEYS = ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'session_name', 'window_index', 'window_name', 'window_width', 'window_height')
WINDOW_FORMAT = '#{session_id}|#{session_name}|#{window_id}|#{window_index}|#{window_name}|#{window_panes}'
WINDOW_KEYS = ('session_id', 'session_name', 'window_id', 'window_index', 'window_name', 'window_panes')
KINDS = {label: label for label in ('a4', 'a6', 'a8', 'b4', 'a10', 'a12', 'a14')}
CODES = {label: 0 for label in KINDS}
CASES = ('N1', 'N2', 'N3', 'N4')
STAGES = {'N1-created': ('a4', 'a6', 'a8', 'b4'),
          'N1-closed6': ('a4', 'a8', 'b4'),
          'N2-set-on': ('a4', 'a8', 'b4'),
          'N2-created10': ('a4', 'a8', 'b4', 'a10'),
          'N2-closed4': ('a8', 'b4', 'a10'),
          'N3-input': ('a8', 'b4', 'a10'),
          'N4-unset': ('a8', 'b4', 'a10'),
          'N4-created12-14': ('a8', 'b4', 'a10', 'a12', 'a14'),
          'N4-closed12': ('a8', 'b4', 'a10', 'a14')}
EXIT_ORDER = ('a6', 'a4', 'a12', 'a8', 'b4', 'a10', 'a14')

ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_RENUMBER_MARKER', 'BASH_ENV', 'ENV',
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
    except (FileNotFoundError, ProcessLookupError, PermissionError) as error:
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
    require(kind in KINDS and exit_code == '0', 'unplanned helper')
    modes = termios.tcgetattr(0)
    tty = {'icanon': bool(modes[3] & termios.ICANON), 'icrnl': bool(modes[0] & termios.ICRNL)}
    equal(tty, {'icanon': True, 'icrnl': True}, 'canonical ownPTY required')
    own = identity(os.getpid()); stem = f"{own['pid']}-{own['start_ticks']}"
    received, pending = [], b''
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'exit_code': 0, 'identity': identity(os.getpid()), 'cwd': os.getcwd(),
                'argv': sys.argv, 'environment': {k: os.environ.get(k) for k in ENV_KEYS},
                'tty': tty.copy(), 'received': copy.deepcopy(received),
                'pending_bytes_base64': base64.b64encode(pending).decode()}
    atomic_once(root / ('born-' + stem + '.json'), report(None))
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        while select.select([0], [], [], 0)[0]:
            data = os.read(0, 4096)
            require(data, 'stdin EOF before product closure')
            pending += data
            require(len(pending) <= 4096 and len(received) < 4, 'helper input cap')
            while b'\n' in pending:
                line, pending = pending.split(b'\n', 1)
                require(len(line) == 48 and all(c in b'0123456789abcdef' for c in line), 'unplanned input')
                received.append(line.decode('ascii'))
                value = report(None)
                value['stdin_bytes_base64'] = base64.b64encode(line + b'\n').decode()
                atomic_once(root / (f'stdin-{stem}-{len(received)}.json'), value)
        for path in sorted(root.glob('request-' + stem + '-*.json')):
            ack = root / path.name.replace('request-', 'ack-', 1)
            if not ack.exists():
                request = json.loads(path.read_text())
                require(same(request['identity'], own), 'challenge birth mismatch')
                atomic_once(ack, report(request['challenge']))
        time.sleep(.02)
    os._exit(124)

def environment(config, phase):
    root = Path(config['root']) / phase
    return {'HOME': str(root / 'home-empty'), 'XDG_CONFIG_HOME': str(root / 'xdg-empty'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/dash', 'TERM': 'vt100', 'LANG': 'C.UTF-8',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_RENUMBER_MARKER': config['tokens'][phase]}

def witness_argv(config, phase, label):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', KINDS[label],
            str(Path(config['root']) / phase / 'markers'), config['tokens'][phase], str(CODES[label])]

def plan(config, phase):
    cwd = str(Path(config['root']) / phase / 'cwd')
    def argv(label): return witness_argv(config, phase, label)
    def session(name, label):
        return ['new-session', '-d', '-s', name, '-n', label, '-x', '80', '-y', '24', '-c', cwd, *argv(label)]
    def window(label, index):
        return ['new-window', '-d', '-t', 'alpha:' + str(index), '-n', label, '-c', cwd, *argv(label)]
    return [('N1', 'a4', session('alpha', 'a4'), 'alpha:a4.0'),
            ('N1', 'a6', window('a6', 6), 'alpha:a6.0'),
            ('N1', 'a8', window('a8', 8), 'alpha:a8.0'),
            ('N1', 'b4', session('beta', 'b4'), 'beta:b4.0'),
            ('N2', 'a10', window('a10', 10), 'alpha:a10.0'),
            ('N4', 'a12', window('a12', 12), 'alpha:a12.0'),
            ('N4', 'a14', window('a14', 14), 'alpha:a14.0')]

def stable_public(a, b):
    return {k: v for k, v in a.items() if k != 'window_index'} == {k: v for k, v in b.items() if k != 'window_index'}

def saved(value):
    return copy.deepcopy(value)

def check_delivery(before, after, label, nonce):
    equal(set(before), set(after), 'exact input population')
    require(label in before and all(nonce not in v for v in before.values()), 'stale input or absent recipient')
    for name, values in before.items():
        equal(after[name], values + ([nonce] if name == label else []), 'exclusive actual stdin ' + name)


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

def check_layout(rows, windows, children):
    equal(len(rows), len(children), 'no missing or extra pane')
    equal(len(windows), len(children), 'no missing or extra window')
    for key in ('window_id', 'pane_id'):
        equal(len({r[key] for r in rows}), len(rows), 'unique public ' + key)
    equal(len({r['window_id'] for r in windows}), len(windows), 'unique listed window IDs')
    equal(len({(r['session_id'], r['window_index']) for r in windows}), len(windows), 'unique session/index')
    logical, sessions = [], {}
    for label, child in children.items():
        original = child['public']
        matches = [r for r in rows if r['pane_id'] == original['pane_id']]
        require(len(matches) == 1, 'expected pane identity missing')
        row = matches[0]
        require(stable_public(row, original), 'public identity changed beyond window index')
        equal(row['session_name'], 'beta' if label == 'b4' else 'alpha', 'window session')
        equal(row['window_name'], label, 'stable window name')
        equal((row['window_width'], row['window_height']), ('80', '24'), 'fixed window geometry')
        if label == 'b4': equal(row['window_index'], 4, 'independent beta index')
        matches = [w for w in windows if w['window_id'] == row['window_id']]
        require(len(matches) == 1, 'expected window identity missing')
        equal(matches[0], {**{k: row[k] for k in WINDOW_KEYS if k != 'window_panes'}, 'window_panes': 1}, 'window/pane inventory coherence')
        name = row['session_name']
        if name in sessions: equal(sessions[name], row['session_id'], 'stable session ID')
        sessions[name] = row['session_id']
        logical.append({'label': label, 'session_name': name, 'window_index': row['window_index']})
    equal(len(set(sessions.values())), len(sessions), 'distinct session IDs')
    require(len(sessions) <= 2, 'session count cap')
    # Sorting by index retains the relative identity order, not just a set of labels.
    return sorted(logical, key=lambda r: (r['session_name'], r['window_index']))

def check_mapping(logical, expected):
    equal(logical, expected, 'exact frozen oracle mapping and order')


def check_kernel_exit(before, after, pidfd):
    require(not pidfd['events_before_request'], 'exit happened before request')
    require(same(before, pidfd['identity_after_open']), 'exit pidfd stale birth')
    require(any(fd == pidfd['fd'] and mask & (select.POLLIN | select.POLLHUP)
                for fd, mask in pidfd['events_after_request']), 'kernel exit event missing')
    require(not same(before, after) or after['state'] == 'Z', 'exited child still live')


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
    equal([c['id'] for c in record['cases']], list(CASES), 'complete oracle cases')
    require(all(c['status'] == 'PASS' for c in record['cases']), 'oracle case failed')
    equal([v['case'] for v in record['layouts']], list(STAGES), 'complete oracle inventories')
    require(all(v['status'] == 'PASS' for v in record['layouts']), 'oracle inventory failed')
    equal([e['label'] for e in record['exits']], list(EXIT_ORDER), 'complete oracle exit coverage')
    for e in record['exits']:
        equal(e['status'], 'PASS', 'oracle exit failure')
        check_kernel_exit(e['original_child']['before'], e['identity_after_exit_event'], e['pidfd'])
        require('closed_at' in e['pidfd'], 'exit pidfd not closed')
    equal(record['spawn_commands'], 7, 'complete oracle child coverage')
    equal(record['cleanup']['status'], 'PASS', 'oracle cleanup')
    equal(len(record['deliveries']), 2, 'oracle actual input coverage')
    require(all(d['status'] == 'PASS' for d in record['deliveries']), 'oracle input failure')
    return {'at': now(), 'status': 'PASS', 'expected_kinds': KINDS, 'driver_sha256': digest(__file__),
            'result_sha256': sha, 'layouts': saved({v['case']: v['logical'] for v in record['layouts']}), 'numeric_target': record['deliveries'][0]['target']}


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
        require(len(commands) == 7 and len({p[1] for p in commands}) == 7, 'bounded birth plan')
        require('BASH_ENV' not in environment(config, phase), 'startup contamination')
    children = {}
    for i, label in enumerate(('a4', 'a8', 'b4')):
        row = {'daemon_pid': 40, 'socket': '/own/s', 'session_id': '$1' if label == 'b4' else '$0',
               'window_id': '@' + str(i), 'pane_id': '%' + str(i), 'pane_pid': 41+i,
               'pane_dead': '0', 'pane_dead_status': '', 'pane_dead_signal': '',
               'session_name': 'beta' if label == 'b4' else 'alpha', 'window_index': 8 if label == 'a8' else 4,
               'window_name': label, 'window_width': '80', 'window_height': '24'}
        children[label] = {'public': row}
    rows = [saved(c['public']) for c in children.values()]
    windows = [{**{k:r[k] for k in WINDOW_KEYS if k!='window_panes'}, 'window_panes':1} for r in rows]
    logical = check_layout(rows, windows, children);check_mapping(logical, saved(logical));checks.append('positive_exact_inventory')
    bad = saved(logical);bad[1]['window_index']=5
    rejected('wrong_oracle_mapping', lambda: check_mapping(bad, logical))
    rejected('wrong_order', lambda: check_mapping(list(reversed(logical)), logical))
    for key in ('window_id', 'pane_id'):
        badrows = saved(rows);badrows[0][key] = rows[1][key]
        rejected('duplicate_'+key, lambda badrows=badrows: check_layout(badrows, windows, children))
    badwindows = saved(windows);badwindows[1]['window_index']=4
    rejected('duplicate_session_index', lambda: check_layout(rows, badwindows, children))
    rejected('missing_window', lambda: check_layout(rows, windows[:-1], children))
    rejected('extra_window', lambda: check_layout(rows, windows+[windows[0]], children))
    before = {'a8': [], 'a10': [], 'b4': []};after={**before,'a10':['nonce']}
    check_delivery(before, after, 'a10', 'nonce');checks.append('exclusive_input_positive')
    rejected('incorrect_target', lambda: check_delivery(before,{**before,'a8':['nonce']},'a10','nonce'))
    rejected('broadcast_input', lambda: check_delivery(before,{**after,'a8':['nonce']},'a10','nonce'))
    rejected('stale_input', lambda: check_delivery(after,after,'a10','nonce'))
    rejected('wrong_nonce', lambda: check_delivery(before,{**before,'a10':['old']},'a10','nonce'))
    snapshot = saved(after);after['new']=[];after['a10'].append('later')
    equal(snapshot, {'a8': [], 'a10': ['nonce'], 'b4': []}, 'nested snapshot independent');checks.append('nested_snapshot_no_alias')
    clean = {'identities_after': [{'before': child['before'], 'after': None}], 'all_tracked_exited': True, 'socket_after': None, 'owned_descendants_remaining': []}
    check_cleanup(clean, {41: child['before']});checks.append('cleanup_exact')
    rejected('cleanup_missing_identity', lambda: check_cleanup({**clean, 'identities_after': []}, {41: child['before']}))
    rejected('cleanup_surviving_birth', lambda: check_cleanup({**clean, 'identities_after': [{'before': child['before'], 'after': child['before']}]}, {41: child['before']}))
    rejected('cleanup_false_socket', lambda: check_cleanup({**clean, 'socket_after': {}}, {41: child['before']}))
    fd = {'fd': 3, 'identity_after_open': child['before'], 'events_before_request': [], 'events_after_request': [[3, 1]]}
    check_kernel_exit(child['before'], None, fd);checks.append('kernel_exit_exact')
    rejected('kernel_exit_missing', lambda: check_kernel_exit(child['before'], None, {**fd, 'events_after_request': []}))
    rejected('kernel_exit_stale', lambda: check_kernel_exit(child['before'], None, {**fd, 'identity_after_open': {'pid':41,'start_ticks':'old'}}))
    rejected('kernel_exit_child_still_live', lambda: check_kernel_exit(child['before'], child['before'], fd))
    from unittest.mock import patch
    class DeniedPath:
        def __truediv__(self, other):return self
        def read_bytes(self):raise PermissionError(13, 'simulated exiting proc access')
    with patch.dict(globals(), {'identity': lambda pid:saved(child['before']), 'Path': lambda *args:DeniedPath()}):
        incomplete = proc(41)
    require(not complete(incomplete) and incomplete['read_error'].startswith('PermissionError'), 'denied snapshot must be incoherent')
    checks.append('exiting_proc_permission_denial_recorded')
    rejected('denied_snapshot_never_live_proof', lambda: check_child(incomplete,incomplete,public,daemon,expected,[daemon]))
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
        self.closed = set()
        self.received = {}
        self.nonces = set()
        self.last_layouts = {}
        self.text_bytes = 0
        self.oracle_layouts = {} if phase == 'oracle' else json.loads((Path(config['root']) / 'ORACLE-FROZEN.json').read_text())['layouts']
        self.record = {'phase': phase, 'start': now(), 'commands': [], 'cases': [], 'observations': [],
                       'environment': self.env, 'public_samples': [], 'profiles': [], 'marker_ledgers': [], 'exits': [], 'layouts': [], 'deliveries': [], 'closures': []}

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
        require(born and born['ppid'] == os.getpid(), 'own client birth unavailable')
        self.owned[p.pid] = born
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
        require(size < 3500000, 'bounded product evidence cap')
        self.text_bytes += len(out.encode()) + len(err.encode())
        require(self.text_bytes <= 120000, 'bounded public text')
        if required:
            require(p.returncode == 0, 'command failed ' + repr(args) + ': ' + err)
        return out.strip()

    def public(self, target):
        public = parse_public(self.command(['display-message', '-p', '-t', target, FORMAT]))

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
        observed = identity(public['pane_pid'])
        require(observed, 'public child absent')
        stem = f"{observed['pid']}-{observed['start_ticks']}"
        markers = self.root / 'markers'
        born_path = markers / ('born-' + stem + '.json')
        born = self.wait_json(born_path)
        first = proc(observed['pid'])
        expected = self.expected(label)
        observation_index = len(self.record['observations'])
        item = {'label': label, 'target': target, 'reobserve': reobserve, 'at': now(), 'expected': expected,
                'public': public, 'first': first, 'born': born, 'born_path': str(born_path),
                'status': 'UNPROVEN'}
        self.record['observations'].append(item)
        try:
            require(complete(first) and same(observed, first['before']), 'observed child birth changed')
            check_marker(born, first, expected, None)
            equal(public['pane_dead'], '0', 'living pane flag')
            equal(public['pane_dead_status'], '', 'living pane exit status')
            equal(public['pane_dead_signal'], '', 'living pane exit signal')
            request = {'identity': observed, 'challenge': nonce, 'issued_at': now()}
            request_path = markers / (f'request-{stem}-{observation_index}.json')
            item.update(request=request, request_path=str(request_path))
            atomic_once(request_path, request)
            ack_path = markers / request_path.name.replace('request-', 'ack-', 1)
            ack = self.wait_json(ack_path)
            second = proc(observed['pid'])
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
                require(snapshot_same(old['child'], second) and stable_public(old['public'], public), 'existing child mutated')
            equal(ack['tty'], {'icanon': True, 'icrnl': True}, 'canonical helper')
            equal(ack['pending_bytes_base64'], '', 'no partial unexpected input')
            # PID/birth observation alone grants no signal or reap authority.
            self.owned[second['before']['pid']] = second['before']
            if not reobserve:
                self.children[label] = item
            item['status'] = 'PASS'
            return item
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise

    def fresh_nonce(self):
        nonce = secrets.token_hex(24)
        require(nonce not in self.nonces, 'duplicate nonce')
        self.nonces.add(nonce)
        return nonce

    def active(self):
        return {k:v for k,v in self.children.items() if k not in self.closed}

    def options(self, target, effective, local):
        equal(self.command(['show-options', '-gv', 'base-index']), '4', 'global base-index')
        equal(self.command(['show-options', '-Av', '-t', target, 'base-index']), '4', 'effective base-index')
        equal(self.command(['show-options', '-gv', 'renumber-windows']), 'off', 'global renumber')
        equal(self.command(['show-options', '-Av', '-t', target, 'renumber-windows']), effective, 'effective renumber')
        equal(self.command(['show-options', '-qv', '-t', target, 'renumber-windows']), local or '', 'local renumber')

    def delivery(self, target, address_kind):
        label = 'a10';nonce = self.fresh_nonce();child = self.children[label]
        before = saved({k:self.received[k] for k in self.active()})
        public, _ = self.public(target)
        require(stable_public(child['public'], public), 'input address resolved to wrong child')
        entry = {'at': now(), 'target': target, 'address_kind': address_kind, 'target_label': label,
                 'nonce': nonce, 'public_before': saved(public), 'received_before': saved(before), 'status': 'UNPROVEN'}
        self.record['deliveries'].append(entry)
        self.command(['send-keys', '-t', target, '-l', nonce]);self.command(['send-keys', '-t', target, 'Enter'])
        own = child['child']['before'];stem = f"{own['pid']}-{own['start_ticks']}"
        path = self.root/'markers'/(f'stdin-{stem}-{len(before[label])+1}.json')
        receipt = self.wait_json(path)
        entry.update(receipt=saved(receipt), receipt_path=str(path))
        check_marker(receipt, child['child'], child['expected'], None)
        equal(base64.b64decode(receipt['stdin_bytes_base64']), (nonce+'\n').encode(), 'real PTY input bytes')
        after = {};entry['observation_indices'] = []
        for name, original in self.active().items():
            entry['observation_indices'].append(len(self.record['observations']))
            obs = self.observe(name, original['public']['pane_id'], True)
            after[name] = saved(obs['ack']['received'])
        check_delivery(before, after, label, nonce)
        entry['received_after'] = saved(after)
        for name, values in after.items():self.received[name] = saved(values)
        after_public, _ = self.public(target)
        equal(public, after_public, 'stable real numeric/ID target')
        entry.update(public_after=saved(after_public), status='PASS', end=now())

    def layout(self, case):
        equal(list(self.active()), list(STAGES[case]), 'exact stage population')
        for label, child in self.active().items():
            obs = self.observe(label, child['public']['pane_id'], True)
            equal(obs['ack']['received'], self.received[label], 'input ledger at inventory')
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
                logical = check_layout(rows, windows, self.active())
                stable = {'panes': sorted(rows, key=lambda r: r['pane_id']), 'windows': sorted(windows, key=lambda r: r['window_id'])}
                if previous == stable:
                    item['logical'] = saved(logical)
                    if case == 'N1-created':
                        equal({r['label']:r['window_index'] for r in logical}, {'a4':4,'a6':6,'a8':8,'b4':4}, 'initial explicit layout')
                    if case == 'N1-closed6':
                        equal({r['label']:r['window_index'] for r in logical}, {'a4':4,'a8':8,'b4':4}, 'off retains gap')
                    if case == 'N2-closed4':
                        alpha = [r for r in logical if r['session_name']=='alpha']
                        equal([r['label'] for r in alpha], ['a8','a10'], 'surviving identity order')
                        equal([r['window_index'] for r in alpha], [4,5], 'renumber begins at base4')
                    if self.phase == 'candidate':
                        require(case in self.oracle_layouts, 'oracle inventory gate missing')
                        check_mapping(logical, self.oracle_layouts[case])
                    self.last_layouts[case] = saved(logical)
                    item['status'] = 'PASS'
                    return item
                previous = stable
                time.sleep(min(.05, self.remaining()))
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise


    def close_owned(self, labels, final=False):
        require(all(label in self.active() for label in labels), 'closure only live owned births')
        group = {'at': now(), 'labels': list(labels), 'final_server': final, 'command': None, 'status': 'UNPROVEN'}
        self.record['closures'].append(group)
        opened = []
        daemon_exit = None
        daemon_poller = None
        try:
            for label in labels:
                child = self.children[label]
                obs = self.observe(label, child['public']['pane_id'], True)
                equal(obs['ack']['received'], self.received[label], 'final owned input ledger')
                before = obs['child']['before']
                current = proc(before['pid'])
                require(snapshot_same(obs['child'], current), 'child changed before pidfd')
                fd = os.pidfd_open(before['pid'], 0)
                e = {'label': label, 'at': now(), 'original_child': saved(obs['child']),
                     'original_public': saved(obs['public']), 'before_pidfd': current,
                     'last_challenge': saved(obs['request']), 'last_ack': saved(obs['ack']),
                     'numeric_status_claimed': False, 'status': 'UNPROVEN',
                     'pidfd': {'fd': fd, 'opened_at': now(), 'identity_after_open': identity(before['pid'])}}
                self.record['exits'].append(e)
                poller = select.poll();poller.register(fd, select.POLLIN)
                opened.append((e, poller))
                e['pidfd']['events_before_request'] = poller.poll(0)
                require(same(before, e['pidfd']['identity_after_open']) and not e['pidfd']['events_before_request'], 'pidfd not live owned birth')
            if final:
                current = proc(self.daemon['before']['pid'])
                require(snapshot_same(self.daemon, current) and self.socket_info()==self.socket_identity, 'final private server attribution')
                args = ['kill-server']
                group['daemon_before'] = current
                daemon_fd = os.pidfd_open(current['before']['pid'], 0)
                daemon_exit = {'before': saved(current['before']), 'fd': daemon_fd, 'opened_at': now(),
                               'identity_after_open': identity(current['before']['pid'])}
                group['daemon_exit'] = daemon_exit
                daemon_poller = select.poll();daemon_poller.register(daemon_fd, select.POLLIN)
                daemon_exit['events_before_request'] = daemon_poller.poll(0)
                require(same(current['before'], daemon_exit['identity_after_open']) and not daemon_exit['events_before_request'], 'daemon pidfd ownership')
            else:
                require(len(labels)==1, 'single private window closure')
                args = ['kill-window', '-t', self.children[labels[0]]['public']['window_id']]
            group['command'] = args
            group['issued_at'] = now();group['command_index'] = len(self.record['commands'])
            self.command(args)
            for e, poller in opened:
                fd = e['pidfd']['fd'];before = e['original_child']['before']
                events = poller.poll(max(1, int(self.remaining()*1000)))
                e['pidfd'].update(events_after_request=events, observed_at=now())
                e['identity_after_exit_event'] = identity(before['pid'])
                check_kernel_exit(before, e['identity_after_exit_event'], e['pidfd'])
                e.update(command=saved(args), command_issued_at=group['issued_at'], status='PASS')
                self.closed.add(e['label'])
            if daemon_exit is not None:
                daemon_exit.update(events_after_request=daemon_poller.poll(max(1,int(self.remaining()*1000))), observed_at=now())
                daemon_exit['after'] = identity(daemon_exit['before']['pid'])
                check_kernel_exit(daemon_exit['before'], daemon_exit['after'], daemon_exit)
            group.update(status='PASS', end=now())
        finally:
            if daemon_exit is not None:
                os.close(daemon_exit['fd']);daemon_exit['closed_at']=now()
            for e, _ in opened:
                os.close(e['pidfd']['fd']);e['pidfd']['closed_at']=now()


    def spawn(self, label):
        rows = [row for row in plan(self.config, self.phase) if row[1] == label]
        require(len(rows) == 1 and self.spawn_count < 7, 'pane child budget or plan mismatch')
        _, _, args, target = rows[0]
        self.spawn_count += 1
        self.command(args, startup=label == 'a4')
        observed = self.observe(label, target)
        equal(observed['public']['window_name'], label, 'new named window')
        equal(observed['ack']['received'], [], 'new helper without input')
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
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()), 'fixture must be fresh')
        self.profiles()
        for case in CASES:
            self.case_end = min(time.monotonic()+20, self.start+150)
            entry = {'id': case, 'start': now(), 'monotonic_start': time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case == 'N1':
                    for label in ('a4','a6','a8','b4'):self.spawn(label)
                    self.options('alpha','off',None);self.options('beta','off',None)
                    self.layout('N1-created');self.close_owned(['a6']);self.layout('N1-closed6')
                elif case == 'N2':
                    self.command(['set-option','-t','alpha','renumber-windows','on'])
                    self.options('alpha','on','on');self.options('beta','off',None)
                    self.layout('N2-set-on')
                    self.spawn('a10');self.layout('N2-created10')
                    self.close_owned(['a4']);self.layout('N2-closed4')
                elif case == 'N3':
                    mapping = self.last_layouts['N2-closed4'] if self.phase=='oracle' else self.oracle_layouts['N2-closed4']
                    index = next(r['window_index'] for r in mapping if r['label']=='a10')
                    self.delivery(f'alpha:{index}.0', 'numeric')
                    self.delivery(self.children['a10']['public']['pane_id'], 'stable-pane-ID')
                    self.layout('N3-input')
                else:
                    self.command(['set-option','-g','renumber-windows','off'])
                    self.command(['source-file',str(self.root/'live.conf')])
                    self.options('alpha','off',None);self.options('beta','off',None)
                    self.layout('N4-unset')
                    self.spawn('a12');self.spawn('a14');self.layout('N4-created12-14')
                    before = saved(self.last_layouts['N4-created12-14'])
                    self.close_owned(['a12']);after = self.layout('N4-closed12')['logical']
                    equal(after,[r for r in before if r['label']!='a12'],'unsetoff retains all surviving gaps')
                self.ledger();self.profiles();self.remaining();entry['status']='PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error));raise
            finally:
                entry.update(end=now(),elapsed_seconds=time.monotonic()-entry['monotonic_start'])
        self.case_end=min(time.monotonic()+15,self.start+150)
        self.close_owned(list(self.active()),final=True)
        self.ledger()


    def cleanup(self):
        start = time.monotonic()
        self.cleaning = True
        self.case_end = min(start + 15, self.start + 180)
        result = {'start': now(), 'actions': [], 'reaped': [], 'ownership_discovery': [],
                  'ownership_roots': saved(list(self.owned.values()))}
        self.record['cleanup'] = result
        def descendants(old, seen=None):
            seen = set() if seen is None else seen
            pid = old['pid']
            if pid in seen or not same(old, identity(pid)):
                return []
            seen.add(pid)
            children = set()
            try:
                for task in Path('/proc', str(pid), 'task').iterdir():
                    try:
                        children.update(int(v) for v in (task / 'children').read_text().split())
                    except FileNotFoundError:
                        pass
            except FileNotFoundError:
                pass
            if not same(old, identity(pid)):
                return []
            attributed = []
            for child_pid in sorted(children - seen):
                child = identity(child_pid)
                if (child and child['ppid'] == pid and same(old, identity(pid))
                        and same(child, identity(child_pid))):
                    attributed.append(child)
                    attributed.extend(descendants(child, seen))
            return attributed
        # Only a verified chain from an already owned birth grants ownership.
        # Subreaper children of this observer may belong to another lot.
        for root in list(self.owned.values()):
            for own in descendants(root):
                if not same(self.owned.get(own['pid']), own):
                    result['ownership_discovery'].append(own)
                    self.owned[own['pid']] = own
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
                for pid, old in list(self.owned.items()):
                    if not same(old, identity(pid)):
                        continue
                    try:
                        reaped, status = os.waitpid(pid, os.WNOHANG)
                        if reaped:
                            equal(reaped, pid, 'reap exact owned child')
                            result['reaped'].append({'pid': reaped, 'status': status})
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
        remaining = {own['pid'] for root in self.owned.values() for own in descendants(root)}
        result.update(socket_after=self.socket_info(), owned_descendants_remaining=sorted(remaining), end=now(), elapsed_seconds=time.monotonic() - start)
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
        (phase_root / 'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-option -g base-index 4\nset-option -g renumber-windows off\n")
        (phase_root / 'live.conf').write_text('set-option -u -t alpha renumber-windows\n')
    write_once(root / 'PLAN.json', {phase: {'commands': plan(config, phase), 'environment': environment(config, phase),
                                          'expected_kinds': KINDS, 'stages': STAGES}
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
            run.record.update(end=now(), spawn_commands=run.spawn_count, text_bytes=run.text_bytes)
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
    lot['public_text_bytes'] = sum(json.loads((root/p['phase']/'RESULT.json').read_text())['text_bytes'] for p in lot['phases'])
    if lot['log_bytes_before_lot'] + len(json.dumps(lot,indent=2)) + 1 > 8*1024**2 or lot['public_text_bytes']>256*1024:
        lot['status']='UNPROVEN'
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
