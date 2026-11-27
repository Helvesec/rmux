#!/usr/bin/env python3
"""Native Linux exit-empty lifecycle reference, E1-E4.
Prepare CONFIG, freeze all inputs, then run CONFIG once. Two owned daemon generations
and three pane witnesses per product. Complete tmux gate precedes RMUX. pidfds prove
exit events, never numeric exit status; waitpid reports only real adopted children.
Endpoint probes distinguish absent listener from a residual filesystem socket.
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
import socket
import errno
import shlex
import signal
import stat
import subprocess
import sys
import time
import traceback

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{session_name}|#{window_index}|#{window_name}'
PUBLIC_KEYS = ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'session_name', 'window_index', 'window_name')
KINDS = {label: label for label in ('alpha', 'beta', 'gamma')}
CODES = {label: 0 for label in KINDS}
CASE_LABELS = {'E1': ('alpha',), 'E2': ('beta',), 'E3': ('beta',), 'E4': ('gamma',)}
GENERATIONS = ('reuse', 'default')
ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_EXITEMPTY_MARKER', 'BASH_ENV', 'ENV', 'RMUX_DISABLE_TMUX_SHIM')

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

def _proc(pid):
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

def proc(pid):
    try:
        return _proc(pid)
    except PermissionError as error:
        return {'at': now(), 'read_error': repr(error), 'coherent': False}

def complete(p):
    return bool(p and p.get('coherent') and p.get('after') and p['after']['state'] not in ('Z', 'X') and p['before']['state'] not in ('Z', 'X'))

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
    require(exit_code == 0, 'unplanned witness exit code')
    own = identity(os.getpid())
    stem = f"{own['pid']}-{own['start_ticks']}"
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'exit_code': exit_code, 'identity': identity(os.getpid()),
                'cwd': os.getcwd(), 'argv': sys.argv,
                'environment': {k: os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root / ('born-' + stem + '.json'), report(None))
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
            equal(request['exit_code'], exit_code, 'fixed witness exit code')
            atomic_once(root / ('exit-intent-' + stem + '.json'), report(request['challenge']))
            os._exit(exit_code)
        time.sleep(.02)
    os._exit(124)



def generation(label):
    return 'default' if label == 'gamma' else 'reuse'

def environment(config, phase, gen):
    root = Path(config['root']) / phase / gen
    return {'HOME': str(root / 'home-empty'), 'XDG_CONFIG_HOME': str(root / 'xdg-empty'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/dash', 'TERM': 'vt100', 'LANG': 'C.UTF-8',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_EXITEMPTY_MARKER': config['tokens'][phase][gen]}

def witness_argv(config, phase, label):
    gen = generation(label)
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', label,
            str(Path(config['root']) / phase / gen / 'markers'), config['tokens'][phase][gen], '0']

def spawn_plan(config, phase, label):
    root = Path(config['root']) / phase / generation(label)
    return ['new-session', '-d', '-P', '-F', '#{pane_id}', '-s', label, '-n', label,
            '-x', '80', '-y', '24', '-c', str(root / 'cwd'), *witness_argv(config, phase, label)]

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

def check_kernel_exit(before, after, pidfd):
    require(not pidfd['events_before_request'], 'exit happened before request')
    require(same(before, pidfd['identity_after_open']), 'exit pidfd stale birth')
    require(any(fd == pidfd['fd'] and mask & (select.POLLIN | select.POLLHUP)
                for fd, mask in pidfd['events_after_request']), 'kernel exit event missing')
    require(not same(before, after) or after['state'] in ('Z', 'X'), 'exited child still live')


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

def check_empty(samples, daemon, endpoint, option):
    equal(len(samples), 2, 'two independent empty-server samples')
    for sample in samples:
        require(snapshot_same(daemon, sample['daemon']), 'empty server generation changed')
        equal(sample['endpoint'], endpoint, 'empty endpoint changed')
        equal(sample['sessions'], '', 'server still has sessions')
        equal(sample['option'], option, 'empty server option')
        require(not sample['pidfd_events'], 'empty daemon exited')

def oracle_gate(record, sha):
    equal(record['status'], 'PASS', 'complete oracle required')
    cases = [c for g in record['generations'] for c in g['cases']]
    equal([c['id'] for c in cases], list(CASE_LABELS), 'all oracle cases')
    require(all(c['status'] == 'PASS' for c in cases), 'oracle case failure')
    equal(sum(g['spawn_commands'] for g in record['generations']), 3, 'helper budget')
    for g in record['generations']:
        equal(g['cleanup']['status'], 'PASS', 'oracle cleanup')
        for e in g['exits']:
            equal(e['status'], 'PASS', 'helper exit')
            check_kernel_exit(e['original_child']['before'], e['identity_after_exit_event'], e['pidfd'])
            require('closed_at' in e['pidfd'], 'helper pidfd unclosed')
        require('closed_at' in g['daemon_pidfd'], 'daemon pidfd unclosed')
    default = cases[-1]['observed']
    return {'at': now(), 'status': 'PASS', 'result_sha256': sha, 'driver_sha256': digest(__file__),
            'default': default, 'endpoint_semantics': 'no accepting listener, raw pathname retained separately'}


def pure_checks(config):
    expected = {'exe': '/python', 'exe_sha256': 'pin', 'argv': ['/python', '-I', '/driver', 'witness', 'C'],
                'cwd': '/cwd', 'kind': 'C', 'token': 'fresh', 'exit_code': 0, 'environment': {'HOME': '/empty'}}
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
        try: fn()
        except (Unproven, Failed): checks.append(name)
        else: raise AssertionError('insensitive observer: ' + name)
    for field, value in [('exe', '/wrong'), ('exe_sha256', 'wrong'), ('argv', ['interpreted']), ('cwd', '/wrong'), ('fd0', 'pipe:1')]:
        bad = {**child, field: value}
        rejected('wrong_child_' + field, lambda bad=bad: check_child(bad, bad, public, daemon, expected, [daemon]))
    stale = copy.deepcopy(child);stale['before']['start_ticks'] = 'old'
    rejected('stale_birth', lambda: check_child(child, stale, public, daemon, expected, [daemon]))
    rejected('wrong_pane', lambda: check_child(child, child, {**public, 'pane_pid': 42}, daemon, expected, [daemon]))
    rejected('wrong_ancestry', lambda: check_child(child, child, public, daemon, expected, [fake(50, 1)]))
    for field, value in [('kind', 'A'), ('token', 'old'), ('challenge', 'stale'), ('cwd', '/bad'), ('argv', ['bad']), ('environment', {})]:
        rejected('wrong_marker_' + field, lambda field=field, value=value: check_marker({**marker, field: value}, child, expected, 'nonce'))
    rejected('wrong_marker_birth', lambda: check_marker({**marker, 'identity': {'pid': 41, 'start_ticks': 'stale'}}, child, expected, 'nonce'))
    for state in ('X', 'Z'):
        bad = copy.deepcopy(child);bad['after']['state'] = state
        rejected('nonlive_' + state, lambda bad=bad: require(complete(bad), 'not live'))
    rejected('denied_proc_not_live', lambda: require(complete({'coherent':False,'read_error':"PermissionError(13, 'denied')"}), 'not live'))
    saved = globals()['Path']
    class DeniedPath:
        def __init__(self, *a): pass
        def read_text(self): raise PermissionError(13, 'pure denied proc')
    try:
        globals()['Path'] = DeniedPath
        value = proc(41)
        require(value['coherent'] is False and 'PermissionError' in value['read_error'], 'denial must be preserved')
        checks.append('actual_proc_permission_denied_preserved')
    finally: globals()['Path'] = saved
    fd = {'fd': 3, 'identity_after_open': child['before'], 'events_before_request': [], 'events_after_request': [[3, 1]]}
    check_kernel_exit(child['before'], None, fd);checks.append('kernel_exit_exact')
    rejected('kernel_exit_missing', lambda: check_kernel_exit(child['before'], None, {**fd, 'events_after_request': []}))
    rejected('kernel_exit_stale', lambda: check_kernel_exit(child['before'], None, {**fd, 'identity_after_open': {'pid':41,'start_ticks':'old'}}))
    rejected('kernel_exit_still_live', lambda: check_kernel_exit(child['before'], child['before'], fd))
    endpoint = {'inode': 1};sample = {'daemon': daemon, 'endpoint':endpoint, 'sessions':'', 'option':'off', 'pidfd_events':[]}
    samples = [copy.deepcopy(sample), copy.deepcopy(sample)]
    check_empty(samples, daemon, endpoint, 'off');checks.append('positive_empty_server')
    for key,value in [('daemon',fake(42,1)),('endpoint',{'inode':2}),('sessions','$0|alpha'),('option','on'),('pidfd_events',[[3,1]])]:
        bad = copy.deepcopy(samples);bad[1][key] = value
        rejected('empty_bad_' + key, lambda bad=bad: check_empty(bad, daemon, endpoint, 'off'))
    saved_sample = copy.deepcopy(samples[0]);samples[1]['daemon']['before']['start_ticks'] = 'changed'
    equal(samples[0], saved_sample, 'nested snapshots independent');checks.append('deep_nested_copy')
    clean = {'identities_after': [{'before': child['before'], 'after': None}], 'all_tracked_exited': True, 'socket_after': None, 'owned_descendants_remaining': []}
    check_cleanup(clean, {41: child['before']});checks.append('cleanup_exact')
    rejected('cleanup_missing_identity', lambda: check_cleanup({**clean, 'identities_after': []}, {41: child['before']}))
    rejected('cleanup_surviving_birth', lambda: check_cleanup({**clean, 'identities_after': [{'before': child['before'], 'after': child['before']}]}, {41: child['before']}))
    rejected('cleanup_false_socket', lambda: check_cleanup({**clean, 'socket_after': {}}, {41: child['before']}))
    for phase in ('oracle', 'candidate'):
        require(len({spawn_plan(config,phase,label)[6] for label in KINDS}) == 3, 'distinct session names')
        for gen in GENERATIONS: require('BASH_ENV' not in environment(config,phase,gen), 'startup contamination')
    return {'at': now(), 'status': 'PASS', 'checks': checks, 'driver_sha256': digest(__file__), 'product_runs': 0}


class Run:
    def __init__(self, config, phase, gen, start):
        self.config, self.phase, self.gen, self.start = config, phase, gen, start
        self.root = Path(config['root']) / phase / gen
        self.binary, self.socket = config[phase], self.root / 's'
        self.env = environment(config, phase, gen)
        self.case_end = start + 150
        self.cleaning = False
        self.daemon = self.socket_identity = None
        self.owned, self.children = {}, {}
        self.spawn_count = 0
        self.daemon_fd = self.daemon_poller = None
        self.record = {'phase':phase, 'generation':gen, 'start':now(), 'commands':[], 'cases':[],
                       'observations':[], 'environment':self.env, 'public_samples':[], 'profiles':[],
                       'marker_ledgers':[], 'exits':[], 'empty_samples':[]}

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
        require(size < 2 * 1024**2, 'product log cap')
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
        return {'kind': kind, 'exit_code': CODES[label], 'token': self.config['tokens'][self.phase][self.gen],
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
                item['status'] = 'PASS'
                self.children[label] = copy.deepcopy(item)
            item['status'] = 'PASS'
            return item
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error))
            raise

    def spawn(self, label):
        require(generation(label) == self.gen and self.spawn_count < (2 if self.gen == 'reuse' else 1), 'helper budget')
        self.spawn_count += 1
        target = self.command(spawn_plan(self.config, self.phase, label), startup=self.spawn_count == 1)
        require(target.startswith('%') and target[1:].isdigit(), 'new-session must return stable pane ID')
        observed = self.observe(label, target)
        self.observe(label, target, True)
        if self.daemon_fd is None:
            before = self.daemon['before']
            self.daemon_fd = os.pidfd_open(before['pid'], 0)
            self.daemon_poller = select.poll();self.daemon_poller.register(self.daemon_fd, select.POLLIN)
            self.record['daemon_pidfd'] = {'fd':self.daemon_fd, 'opened_at':now(),
                'identity_after_open':identity(before['pid']), 'events_before_request':self.daemon_poller.poll(0)}
            require(same(before, self.record['daemon_pidfd']['identity_after_open']), 'daemon pidfd birth')
            require(not self.record['daemon_pidfd']['events_before_request'], 'daemon already exited')
        return observed

    def close_window(self, label):
        child = self.children[label]
        before = child['child']['before']
        record = {'label':label, 'at':now(), 'original_public':copy.deepcopy(child['public']),
                  'original_child':copy.deepcopy(child['child']), 'status':'UNPROVEN'}
        self.record['exits'].append(record)
        fd = None
        try:
            current = proc(before['pid']);record['before_pidfd'] = current
            require(snapshot_same(child['child'], current), 'child changed before closing window')
            fd = os.pidfd_open(before['pid'], 0)
            poller = select.poll();poller.register(fd, select.POLLIN)
            record['pidfd'] = {'fd':fd, 'opened_at':now(), 'identity_after_open':identity(before['pid']), 'events_before_request':poller.poll(0)}
            require(same(before, record['pidfd']['identity_after_open']), 'child pidfd birth')
            require(not record['pidfd']['events_before_request'], 'child already exited')
            record['close_requested_at'] = now()
            self.command(['kill-window', '-t', child['public']['window_id']])
            record['pidfd'].update(events_after_request=poller.poll(max(1,int(self.remaining()*1000))), observed_at=now())
            record['identity_after_exit_event'] = identity(before['pid'])
            check_kernel_exit(before, record['identity_after_exit_event'], record['pidfd'])
            record['status'] = 'PASS'
        except Exception as error:
            record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error));raise
        finally:
            if fd is not None:
                os.close(fd);record['pidfd']['closed_at'] = now()

    def empty_server(self, option):
        samples = []
        self.record['empty_samples'].append(samples)
        for _ in range(2):
            sample = {'at':now(), 'daemon':proc(self.daemon['before']['pid']), 'endpoint':self.socket_info(),
                      'sessions':self.command(['list-sessions','-F','#{session_id}|#{session_name}']),
                      'option':self.command(['show-options','-sv','exit-empty']), 'pidfd_events':self.daemon_poller.poll(0)}
            samples.append(copy.deepcopy(sample))
            time.sleep(min(.05,self.remaining()))
        check_empty(samples,self.daemon,self.socket_identity,option)
        return copy.deepcopy(samples)

    def endpoint_after_exit(self):
        item = {'at':now(), 'path_state':self.socket_info(), 'probe':'AF_UNIX SOCK_STREAM connect, no product/autostart command'}
        self.record.setdefault('endpoint_probes',[]).append(item)
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as conn:
            conn.settimeout(min(1,self.remaining()))
            try:
                conn.connect(str(self.socket));item.update(accepting=True,errno=None)
            except OSError as error:
                item.update(accepting=False,errno=error.errno,error=repr(error))
        require(item['errno'] in (errno.ENOENT,errno.ECONNREFUSED) and not item['accepting'], 'endpoint still reachable or unqualified failure')
        item['listener_absent'] = True
        item['pathname_absent'] = item['path_state'] is None
        return copy.deepcopy(item)

    def daemon_exit(self, limit=None):
        milliseconds = int(min(self.remaining(),limit if limit is not None else self.remaining())*1000)
        events = self.daemon_poller.poll(max(1,milliseconds))
        item = {'at':now(), 'events':events, 'identity_after':identity(self.daemon['before']['pid']), 'proc_after':proc(self.daemon['before']['pid'])}
        self.record.setdefault('daemon_exit_observations',[]).append(item)
        if events:
            self.record['daemon_pidfd'].update(events_after_request=events, observed_at=now())
            check_kernel_exit(self.daemon['before'],item['identity_after'],self.record['daemon_pidfd'])
            item['endpoint'] = self.endpoint_after_exit()
            return True
        return False

    def execute(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()), 'fixture is not fresh')
        self.profiles()
        cases = ('E1','E2','E3') if self.gen == 'reuse' else ('E4',)
        for case in cases:
            self.case_end = min(time.monotonic()+20,self.start+150)
            entry = {'id':case,'start':now(),'monotonic_start':time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case == 'E1':
                    self.spawn('alpha')
                    self.command(['set-option','-s','exit-empty','off'])
                    equal(self.command(['show-options','-sv','exit-empty']),'off','explicit off')
                    self.close_window('alpha');entry['empty'] = self.empty_server('off')
                elif case == 'E2':
                    observed = self.spawn('beta')
                    require(not same(observed['child']['before'],self.children['alpha']['child']['before']),'beta reused alpha birth')
                    entry['same_daemon'] = snapshot_same(self.daemon,self.public(observed['target'])[1])
                    require(entry['same_daemon'],'daemon replaced on reuse')
                    equal(self.command(['show-options','-sv','exit-empty']),'off','reuse keeps off')
                    entry['alpha_birth'] = copy.deepcopy(self.children['alpha']['child']['before'])
                    entry['beta_birth'] = copy.deepcopy(observed['child']['before'])
                elif case == 'E3':
                    self.command(['source-file',str(self.root/'live.conf')])
                    equal(self.command(['show-options','-sv','exit-empty']),'on','source-file enables exit-empty')
                    self.observe('beta',self.children['beta']['target'],True)
                    self.close_window('beta')
                    require(self.daemon_exit(),'explicit on daemon did not exit')
                    entry['listener_absent'] = True
                    entry['pathname_absent'] = self.record['endpoint_probes'][-1]['pathname_absent']
                else:
                    self.spawn('gamma')
                    default = self.command(['show-options','-sv','exit-empty'])
                    require(default in ('on','off'),'default option value invalid')
                    self.close_window('gamma')
                    exited = self.daemon_exit(3)
                    if not exited: entry['empty'] = self.empty_server(default)
                    observed = {'default':default,'behavior':'exited' if exited else 'alive_after_3s'}
                    entry['observed'] = observed
                    if self.phase == 'candidate':
                        gate = json.loads((Path(self.config['root'])/'ORACLE-FROZEN.json').read_text())
                        equal(observed,gate['default'],'fresh default lifecycle against oracle')
                self.ledger();self.profiles();self.remaining();entry['status'] = 'PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error));raise
            finally: entry.update(end=now(),elapsed_seconds=time.monotonic()-entry['monotonic_start'])

    def ledger(self):
        entries = [{'path': str(p), 'data': json.loads(p.read_text())}
                   for p in sorted((self.root / 'markers').glob('born-*.json'))]
        self.record['marker_ledgers'].append({'at': now(), 'entries': entries})
        equal(len(entries), self.spawn_count, 'unexpected witness execution count')
        equal({entry['path'] for entry in entries}, {c['born_path'] for c in self.children.values()},
              'unattributed witness marker')

    def cleanup(self):
        start = time.monotonic()
        self.cleaning = True
        self.case_end = min(start + 7.5, self.start + 180)
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
        require(result['elapsed_seconds'] <= 7.5, 'cleanup budget')
        result['status'] = 'PASS'

    def close_daemon_fd(self):
        if self.daemon_fd is not None:
            os.close(self.daemon_fd)
            self.record['daemon_pidfd']['closed_at'] = now()
            self.daemon_fd = None


def prepare(config):
    root = Path(config['root'])
    require(root.is_dir(), 'fixture root must exist')
    for phase in ('oracle','candidate'):
        for gen in GENERATIONS:
            generation_root = root/phase/gen
            generation_root.mkdir(parents=True)
            for name in ('home-empty','xdg-empty','markers','cwd'): (generation_root/name).mkdir()
            (generation_root/'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\n")
            (generation_root/'live.conf').write_text('set-option -s exit-empty on\n')
    write_once(root/'PLAN.json', {'cases':CASE_LABELS,'spawn':{phase:{label:spawn_plan(config,phase,label) for label in KINDS} for phase in ('oracle','candidate')},
        'case_seconds':20,'default_observation_seconds':3,'generations_per_product':2,'helpers_per_product':3,
        'endpoint':'pidfd exit plus refused/absent private Unix listener; pathname separately recorded before cleanup',
        'environment':{phase:{gen:environment(config,phase,gen) for gen in GENERATIONS} for phase in ('oracle','candidate')}})
    write_once(root/'PURE-CHECKS.json',pure_checks(config))


def execute_lot(config, config_path):
    root = Path(config['root'])
    frozen = json.loads((root/'FROZEN-NATIVE.json').read_text())
    artifacts = {v['path']:v['sha256'] for v in frozen['artifacts']}
    mandatory = [str(Path(__file__).resolve()),str(Path(config_path).resolve()),str(root/'PLAN.json'),str(root/'PURE-CHECKS.json')]
    mandatory += [config[k] for k in ('oracle','candidate','daemon','python')]
    mandatory += [str(root/p/g/f) for p in ('oracle','candidate') for g in GENERATIONS for f in ('startup.conf','live.conf')]
    require(all(p in artifacts for p in mandatory),'freeze misses required artifacts')
    for path,sha in artifacts.items(): require(digest(path)==sha,'frozen artifact changed '+path)
    for key,sha in config['hashes'].items(): require(digest(config[key])==sha,'binary pin mismatch '+key)
    pure = json.loads((root/'PURE-CHECKS.json').read_text())
    require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure controls stale')
    require(ctypes.CDLL(None,use_errno=True).prctl(36,1,0,0,0)==0,'subreaper unavailable')
    write_once(root/'STARTED.json',{'at':now(),'identity':proc(os.getpid())})
    start=time.monotonic()
    lot={'start':now(),'phases':[],'frozen_sha256':digest(root/'FROZEN-NATIVE.json'),'fd_before':fd_snapshot()}
    for phase in ('oracle','candidate'):
        if phase=='candidate':
            gate=json.loads((root/'ORACLE-FROZEN.json').read_text())
            require(gate['status']=='PASS' and gate['driver_sha256']==digest(__file__),'oracle gate invalid')
            require(digest(root/'oracle/RESULT.json')==gate['result_sha256'],'oracle proof changed')
        record={'phase':phase,'start':now(),'generations':[],'status':'UNPROVEN'}
        for gen in GENERATIONS:
            run=Run(config,phase,gen,start)
            try:
                run.execute();run.record['status']='PASS'
            except Exception as error:
                run.record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error),traceback=traceback.format_exc())
            finally:
                try: run.cleanup()
                except Exception as error: run.record.update(status='UNPROVEN',cleanup_error=repr(error),cleanup_traceback=traceback.format_exc())
                finally: run.close_daemon_fd()
                run.record.update(end=now(),spawn_commands=run.spawn_count)
                write_once(run.root/'RESULT.json',run.record)
            record['generations'].append(copy.deepcopy(run.record))
            if run.record['status']!='PASS': break
        record['status']='PASS' if len(record['generations'])==2 and all(g['status']=='PASS' for g in record['generations']) else 'UNPROVEN'
        record['end']=now();write_once(root/phase/'RESULT.json',record)
        lot['phases'].append({'phase':phase,'status':record['status'],'result_sha256':digest(root/phase/'RESULT.json')})
        print(json.dumps(lot['phases'][-1]),flush=True)
        if record['status']!='PASS': break
        if phase=='oracle': write_once(root/'ORACLE-FROZEN.json',oracle_gate(record,digest(root/phase/'RESULT.json')))
    lot.update(end=now(),elapsed_seconds=time.monotonic()-start,fd_after=fd_snapshot())
    lot['status']='PASS' if len(lot['phases'])==2 and all(p['status']=='PASS' for p in lot['phases']) else 'STOPPED'
    total=sum(p.stat().st_size for p in root.rglob('*') if p.is_file())
    lot['log_bytes']=total
    if lot['fd_before']!=lot['fd_after'] or lot['elapsed_seconds']>180 or total>8*1024**2: lot['status']='UNPROVEN'
    write_once(root/'LOT.json',lot);print(json.dumps(lot),flush=True)
    return 0 if lot['status']=='PASS' else 1

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
