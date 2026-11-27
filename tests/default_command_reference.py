#!/usr/bin/env python3
"""Bounded native Linux default-command reference (DCOMMAND1).

Usage: python3 -I tests/default_command_reference.py prepare CONFIG.json
       python3 -I tests/default_command_reference.py run CONFIG.json

CONFIG supplies absolute private root, oracle, candidate, daemon and Python paths,
plus their SHA256 pins. Prepare creates fresh fixtures and pure observer controls.
Before run, write root/FROZEN-NATIVE.json with an `artifacts` list of path/SHA256
pairs covering this file, CONFIG, prepared inputs, binaries and pure controls.
Run is single-use, stops at the first failure and gates RMUX on immutable tmux PASS.
No build, input injection, attach, capture, debugger or product instrumentation.
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
import shlex
import signal
import stat
import subprocess
import sys
import time
import traceback

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}'
EXTRA = ['space separated value', 'literal;$L6_DCOMMAND_MARKER', '*?[abc]', 'quote"and\'quote']
KINDS = {'d1': 'A', 'd2window': 'B', 'd2split': 'B', 'd2session': 'A',
         'd3': 'C', 'd4new': 'A', 'd4respawn': 'B'}
ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_DCOMMAND_MARKER', 'BASH_ENV', 'ENV',
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

def witness(kind, root, token):
    root = Path(root)
    own = identity(os.getpid())
    stem = f"{own['pid']}-{own['start_ticks']}"
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'identity': identity(os.getpid()), 'cwd': os.getcwd(), 'argv': sys.argv,
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
        time.sleep(.02)

def environment(config, phase):
    root = Path(config['root']) / phase
    return {'HOME': str(root / 'home-empty'), 'XDG_CONFIG_HOME': str(root / 'xdg-empty'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/dash', 'TERM': 'vt100', 'LANG': 'C.UTF-8',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_DCOMMAND_MARKER': config['tokens'][phase]}

def witness_argv(config, phase, kind):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', kind,
            str(Path(config['root']) / phase / 'markers'), config['tokens'][phase],
            *(EXTRA if kind == 'C' else [])]

def default_command(config, phase, kind):
    return 'exec ' + shlex.join(witness_argv(config, phase, kind))

def plan(config, phase):
    cwd = str(Path(config['root']) / phase / 'cwd')
    return [('D1', 'd1', ['new-session', '-d', '-s', 'alpha', '-c', cwd], 'alpha:0.0'),
            ('D2', 'd2window', ['new-window', '-d', '-t', 'alpha', '-n', 'local', '-c', cwd], 'alpha:local.0'),
            ('D2', 'd2split', ['split-window', '-d', '-t', 'alpha:local.0', '-c', cwd], 'alpha:local.1'),
            ('D2', 'd2session', ['new-session', '-d', '-s', 'beta', '-c', cwd], 'beta:0.0'),
            ('D3', 'd3', ['new-window', '-d', '-t', 'alpha', '-n', 'explicit', '-c', cwd,
                          *witness_argv(config, phase, 'C')], 'alpha:explicit.0'),
            ('D4', 'd4new', ['new-window', '-d', '-t', 'alpha', '-n', 'restored', '-c', cwd], 'alpha:restored.0'),
            ('D4', 'd4respawn', ['respawn-pane', '-k', '-t', 'alpha:local.0'], 'alpha:local.0')]

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
    for field in ('kind', 'token', 'cwd', 'environment'):
        equal(marker[field], expected[field], 'marker ' + field)
    equal(marker['argv'], expected['argv'][2:], 'witness actual sys.argv')
    equal(marker['challenge'], challenge, 'fresh challenge echo')

def pure_checks(config):
    expected = {'exe': '/python', 'exe_sha256': 'pin', 'argv': ['/python', '-I', '/driver', 'witness', 'C'],
                'cwd': '/cwd', 'kind': 'C', 'token': 'fresh', 'environment': {'HOME': '/empty'}}
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
        require(len(commands) == 7 and len({p[1] for p in commands}) == 7, 'pane population')
        require(commands[-1][2] == ['respawn-pane', '-k', '-t', 'alpha:local.0'], 'respawn must omit command')
        require('BASH_ENV' not in environment(config, phase), 'startup contamination')
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
                       'environment': self.env, 'public_samples': [], 'profiles': [], 'marker_ledgers': []}

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
                p.kill()
            out, err = p.communicate(timeout=2)
        self.record['commands'].append({'argv': argv, 'start': begin, 'end': now(), 'identity': born,
                                        'exit_code': p.returncode, 'stdout': out, 'stderr': err, 'timeout': timed_out})
        require(not timed_out, 'command timeout')
        require(len(out) + len(err) < 65536, 'command output cap')
        size = len(json.dumps(self.record)) + sum(p.stat().st_size for p in self.root.rglob('*') if p.is_file())
        require(size < 16 * 1024**2, 'product log cap')
        if required:
            require(p.returncode == 0, 'command failed ' + repr(args) + ': ' + err)
        return out.strip()

    def public(self, target):
        fields = self.command(['display-message', '-p', '-t', target, FORMAT]).split('|')
        require(len(fields) == 6, 'invalid public identity')
        public = dict(zip(('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid'), fields))
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
        return {'kind': kind, 'token': self.config['tokens'][self.phase],
                'exe': str(Path(self.config['python']).resolve()), 'exe_sha256': self.config['hashes']['python'],
                'argv': witness_argv(self.config, self.phase, kind), 'cwd': str(self.root / 'cwd'),
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

    def options(self, session, kind, local):
        equal(self.command(['show-options', '-gv', 'default-shell']), '/bin/bash', 'default-shell guard')
        equal(self.command(['show-options', '-gv', 'default-command']), default_command(self.config, self.phase, 'A'), 'global value')
        equal(self.command(['show', '-gv', 'default-command']), default_command(self.config, self.phase, 'A'), 'show alias global value')
        equal(self.command(['show-options', '-Av', '-t', session, 'default-command']), default_command(self.config, self.phase, kind), 'effective value')
        equal(self.command(['show-options', '-qv', '-t', session, 'default-command']), default_command(self.config, self.phase, local) if local else '', 'local value')

    def attribution(self, label, observed):
        public, original = observed['public'], self.children['d1']['public']
        if label == 'd2session':
            require(public['session_id'] != original['session_id'], 'new session not distinct')
        elif label == 'd2split':
            old = self.children['d2window']['public']
            require(public['session_id'] == old['session_id'] and public['window_id'] == old['window_id'] and public['pane_id'] != old['pane_id'], 'split attribution')
        elif label == 'd4respawn':
            old = self.children['d2window']
            require(all(public[k] == old['public'][k] for k in ('session_id', 'window_id', 'pane_id')), 'respawn pane changed')
            require(not same(old['child']['before'], observed['child']['before']), 'respawn did not replace child')
            after = identity(old['child']['before']['pid'])
            observed['old_child_after'] = after
            require(not same(old['child']['before'], after) or after['state'] == 'Z', 'old respawn child still live')
        elif label != 'd1':
            require(public['session_id'] == original['session_id'] and public['window_id'] != original['window_id'], 'new window attribution')

    def ledger(self):
        entries = [{'path': str(p), 'data': json.loads(p.read_text())} for p in sorted((self.root / 'markers').glob('born-*.json'))]
        self.record['marker_ledgers'].append({'at': now(), 'entries': entries})
        equal(len(entries), self.spawn_count, 'unexpected witness execution count')
        equal({e['path'] for e in entries}, {c['born_path'] for c in self.children.values()}, 'unattributed witness marker')

    def execute(self):
        require(not self.socket.exists() and not list((self.root / 'markers').iterdir()), 'fixture is not fresh')
        self.profiles()
        for case in ('D1', 'D2', 'D3', 'D4'):
            self.case_end = min(time.monotonic() + 20, self.start + 150)
            entry = {'id': case, 'start': now(), 'monotonic_start': time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case == 'D2':
                    self.command(['set-option', '-t', 'alpha', 'default-command', default_command(self.config, self.phase, 'B')])
                    self.options('alpha', 'B', 'B')
                    self.observe('d1', 'alpha:0.0', True)
                if case == 'D3':
                    self.options('alpha', 'B', 'B')
                if case == 'D4':
                    self.command(['source-file', str(self.root / 'live.conf')])
                    self.options('alpha', 'A', None)
                    self.observe('d2window', 'alpha:local.0', True)
                    self.observe('d2split', 'alpha:local.1', True)
                for _, label, args, target in [p for p in plan(self.config, self.phase) if p[0] == case]:
                    require(self.spawn_count < 7, 'pane population cap')
                    self.spawn_count += 1
                    self.command(args, startup=case == 'D1')
                    observed = self.observe(label, target)
                    self.attribution(label, observed)
                    if label in ('d1', 'd2session'):
                        self.options('alpha' if label == 'd1' else 'beta', 'A', None)
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
        command = default_command(config, phase, 'A')
        # All A arguments have simple path/token characters, so a single quoted
        # tmux string preserves the command as one option value.
        require("'" not in command and '\n' not in command, 'unsafe startup literal')
        (phase_root / 'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command '" + command + "'\n")
        (phase_root / 'live.conf').write_text('set-option -u -t alpha default-command\n')
    write_once(root / 'PLAN.json', {p: {'commands': plan(config, p), 'environment': environment(config, p),
                                     'A': default_command(config, p, 'A'), 'B': default_command(config, p, 'B')} for p in ('oracle', 'candidate')})
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
            require(gate['status'] == 'PASS' and gate['expected_kinds'] == KINDS, 'oracle gate not PASS')
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
            write_once(root / 'ORACLE-FROZEN.json', {'at': now(), 'status': 'PASS', 'expected_kinds': KINDS,
                       'result_sha256': digest(run.root / 'RESULT.json'), 'driver_sha256': digest(__file__)})
    lot.update(end=now(), elapsed_seconds=time.monotonic() - start, fd_after=fd_snapshot())
    lot['status'] = 'PASS' if len(lot['phases']) == 2 and all(p['status'] == 'PASS' for p in lot['phases']) else 'STOPPED'
    if lot['fd_before'] != lot['fd_after'] or lot['elapsed_seconds'] > 180:
        lot['status'] = 'UNPROVEN'
    write_once(root / 'LOT.json', lot)
    print(json.dumps(lot), flush=True)
    return 0 if lot['status'] == 'PASS' else 1

if __name__ == '__main__':
    if len(sys.argv) >= 5 and sys.argv[1] == 'witness':
        witness(*sys.argv[2:5])
    elif len(sys.argv) == 3 and sys.argv[1] in ('prepare', 'run'):
        config = json.loads(Path(sys.argv[2]).read_text())
        if sys.argv[1] == 'prepare':
            prepare(config)
        else:
            sys.exit(execute_lot(config, sys.argv[2]))
    else:
        raise SystemExit(__doc__)
