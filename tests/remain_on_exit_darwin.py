#!/usr/bin/env python3
"""Darwin remain-on-exit off/on R1--R4, paired with accepted Linux REMAIN1.

Read-only API bundle comes from default_shell_darwin.py and is pinned before
import. The separate 012 dependency remains under review; no implicit acceptance. Each lot
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
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{pane_tty}'
KINDS = {'anchor': 'anchor', 'r1': 'off', 'r2zero': 'zero', 'r2seven': 'seven',
         'r3': 'seven', 'r4local': 'local', 'r4unset': 'unset', 'r4beta': 'beta'}
CODES = {'anchor': 0, 'r1': 7, 'r2zero': 0, 'r2seven': 7,
         'r3': 7, 'r4local': 7, 'r4unset': 7, 'r4beta': 0}
ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_REMAIN_MARKER', 'BASH_ENV', 'ENV',
            'RMUX_DISABLE_TMUX_SHIM', 'L5_REMAIN_API')

FILE_CAP = 4096
RECORD_CAP = 720 * 1024
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
    config = json.loads(Path(os.environ['L5_REMAIN_API']).read_text())
    bind_api(config)
    root = Path(root)
    exit_code = int(exit_code)
    require(exit_code in (0, 7), 'unplanned witness exit code')
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
        require(len(requests) <= 12, 'witness challenge count cap')
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
            'L6_REMAIN_MARKER': config['tokens'][phase], 'L5_REMAIN_API': config['config_path']}

def witness_argv(config, phase, label):
    return [config['pins']['python']['path'], '-I', config['driver'], 'witness', KINDS[label],
            str(Path(config['roots'][phase]) / 'markers'), config['tokens'][phase], str(CODES[label])]

def plan(config, phase):
    cwd = str(Path(config['roots'][phase]) / 'cwd')
    def argv(label):
        return witness_argv(config, phase, label)
    return [('R1', 'anchor', ['new-session', '-d', '-s', 'alpha', '-c', cwd, *argv('anchor')], 'alpha:0.0'),
            ('R1', 'r1', ['new-window', '-d', '-t', 'alpha', '-n', 'off', '-c', cwd, *argv('r1')], 'alpha:off.0'),
            ('R2', 'r2zero', ['new-window', '-d', '-t', 'alpha', '-n', 'zero', '-c', cwd, *argv('r2zero')], 'alpha:zero.0'),
            ('R2', 'r2seven', ['new-window', '-d', '-t', 'alpha', '-n', 'seven', '-c', cwd, *argv('r2seven')], 'alpha:seven.0'),
            ('R3', 'r3', ['respawn-pane', '-t', 'alpha:seven.0'], 'alpha:seven.0'),
            ('R4', 'r4local', ['split-window', '-d', '-t', 'alpha:0.0', '-c', cwd, *argv('r4local')], 'alpha:0.1'),
            ('R4', 'r4unset', ['split-window', '-d', '-t', 'alpha:0.0', '-c', cwd, *argv('r4unset')], 'alpha:0.1'),
            ('R4', 'r4beta', ['new-session', '-d', '-s', 'beta', '-c', cwd, *argv('r4beta')], 'beta:0.0')]

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


def check_living(public):
    equal([public[k] for k in ('pane_dead', 'pane_dead_status', 'pane_dead_signal')],
          ['0', '', ''], 'living pane public state')


def check_child(first, second, public, daemon, expected):
    check_living(public)
    require(first['before']['status'] != 5 and second['after']['status'] != 5,
            'dead process cannot be live child')
    A.check_child(first, second, public, daemon, expected)


def check_respawn(old, new):
    require(all(new['public'][k] == old['public'][k]
                for k in ('session_id', 'window_id', 'pane_id')), 'respawn changed pane ID')
    require(not D.same(old['child']['before'], new['child']['before']), 'respawn reused child birth')
    equal(new['child']['args']['argv_base64'], old['child']['args']['argv_base64'], 'respawn original raw argv')
    equal(new['child']['cwd'], old['child']['cwd'], 'respawn original cwd')
    require(new['request']['challenge'] != old['request']['challenge'], 'respawn reused nonce')
    check_living(new['public'])


def check_exit(record):
    before = record['original_child']['before']
    require(D.same(before, record['request']['identity']), 'release birth mismatch')
    require(record['watch']['event_before_request'] is False
            and record['watch']['event_after_request'] is True, 'missing fresh kernel NOTE_EXIT')
    after = record['identity_after_exit_event']
    require(not D.same(before, after) or after['status'] == 5, 'exited child is still live')
    require(len(record['pane_samples']) >= 2, 'two public exit samples required')
    outcomes = []
    for sample in record['pane_samples'][-2:]:
        current = sample['child_identity']
        require(not D.same(before, current) or current['status'] == 5, 'dead pane has live birth')
        matches = [p for p in sample['rows'] if p['pane_id'] == record['original_public']['pane_id']]
        if record['expected_kept']:
            require(len(matches) == 1, 'retained pane absent')
            row = matches[0]
            for field in ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid'):
                equal(row[field], record['original_public'][field], 'dead public identity ' + field)
            equal([row[k] for k in ('pane_dead', 'pane_dead_status', 'pane_dead_signal')],
                  ['1', str(record['expected_exit_code']), ''], 'actual public exit status')
            equal(record['actual_public_exit_status'], row['pane_dead_status'], 'observed exit provenance')
            outcomes.append(row)
        else:
            require(not matches, 'off pane not removed')
            equal(record['actual_public_exit_status'], None, 'intent is not observable removed-pane status')
            outcomes.append('ABSENT')
    equal(outcomes[0], outcomes[1], 'two stable public exit samples')


class Run:
    def __init__(self, frozen):
        A.Run.__init__(self, frozen)
        self.env = environment(self.config, self.phase)
        self.record.update(environment=self.env, exits=[])

    def remaining(self):
        end = min(self.case_end, self.start + (180 if self.cleaning else 150))
        left = end - time.monotonic()
        require(left > 0, 'native deadline exceeded')
        return min(left, 5)

    def socket_info(self): return A.Run.socket_info(self)
    def track(self, row, role): return A.Run.track(self, row, role)
    def profiles(self, post_shutdown=False): return A.Run.profiles(self, post_shutdown)
    def wait_json(self, path): return A.Run.wait_json(self, path)
    def ledger(self): return A.Run.ledger(self)

    def checkpoint(self):
        raw = (json.dumps(self.record, indent=2) + '\n').encode()
        require(len(raw) <= RECORD_CAP, 'record cap')
        D.acquired.save(self.root / 'progress.json', self.record)

    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent, D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own = D.birth(pid)
                if own is None or own['ppid'] != parent['pid'] or pid in self.owned: continue
                role = 'unexpected-pane' if parent['role'] == 'daemon' else 'helper'
                self.track(own, role)
                self.record['helpers'].append({'at': now(), 'parent': parent, 'identity': own, 'role': role})
                require(len(self.record['helpers']) <= 1, 'observed own helper cap')
                require(self.cleaning or role == 'helper', 'unattributed daemon child')

    def command(self, args, startup=False, required=True):
        require(len(self.record['commands']) < (182 if self.cleaning else 180), 'CLI count cap')
        cap = RECORD_CAP - 8192 if self.cleaning else 650 * 1024
        require(len(json.dumps(self.record, indent=2).encode()) < cap, 'reserve exhausted before exec')
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup: argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        entry = {'start': now(), 'argv': argv, 'pre_exec_file_cap': FILE_CAP}
        self.record['commands'].append(entry)
        old = resource.getrlimit(resource.RLIMIT_FSIZE)
        require(old[0] == resource.RLIM_INFINITY or old[0] >= FILE_CAP, 'file limit too small')
        try:
            resource.setrlimit(resource.RLIMIT_FSIZE, (FILE_CAP, old[1]))
            with chdir(self.root):
                result = D.native.run(argv, env=self.env, timeout=self.remaining(), check=False)
            entry.update(pid=result.pid, returncode=result.returncode,
                         stdout=result.stdout.decode(), stderr=result.stderr.decode())
            require(len(result.stdout) < FILE_CAP and len(result.stderr) < FILE_CAP, 'CLI output cap')
        except BaseException as error:
            entry['error'] = repr(error); raise
        finally:
            resource.setrlimit(resource.RLIMIT_FSIZE, old); entry['end'] = now()
        if required: require(result.returncode == 0, 'CLI failed: ' + repr(args))
        return result.stdout.decode().strip()

    def parse_public(self, text):
        fields = text.split('|')
        require(len(fields) == 10, 'public identity format incomplete')
        row = dict(zip(('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid',
                        'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'pane_tty'), fields))
        row['daemon_pid'], row['pane_pid'] = int(row['daemon_pid']), int(row['pane_pid'])
        require(row['socket'] == str(self.socket), 'public endpoint differs')
        return row

    def public(self, target):
        public = self.parse_public(self.command(['display-message', '-p', '-t', target, FORMAT]))
        endpoint = self.socket_info(); require(endpoint is not None, 'endpoint absent')
        daemon = D.process(public['daemon_pid']); self.track(daemon['before'], 'daemon')
        key = 'tmux' if self.phase == 'oracle' else 'rmux_daemon'
        equal(daemon['image']['sha256'], self.frozen['pins'][key]['sha256'], 'daemon disk identity')
        if self.daemon is None: self.daemon, self.endpoint = daemon, endpoint
        else: require(D.stable(self.daemon, daemon) and self.endpoint == endpoint, 'daemon or socket changed')
        item = {'at': now(), 'target': target, 'public': public, 'daemon': daemon, 'endpoint': endpoint}
        self.record['public_samples'].append(item)
        return item

    def pane_listing(self):
        rows = [self.parse_public(line) for line in self.command(['list-panes', '-a', '-F', FORMAT]).splitlines()]
        require(all(r['daemon_pid'] == self.daemon['before']['pid'] for r in rows), 'listing left private daemon')
        require(len({r['pane_id'] for r in rows}) == len(rows), 'duplicate pane ID')
        return rows

    def expected_for(self, label):
        python = self.config['pins']['python']
        return {'kind': KINDS[label], 'exit_code': CODES[label], 'token': self.config['tokens'][self.phase],
                'exe': python['path'], 'exe_sha256': python['sha256'],
                'argv': witness_argv(self.config, self.phase, label), 'cwd': str(self.root / 'cwd'),
                'environment': {k: self.env.get(k) for k in ENV_KEYS}}

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
            check_living(public)
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


    def options(self, target, global_value, effective, local):
        equal(self.command(['show-options', '-wgv', 'remain-on-exit']), global_value, 'global remain-on-exit')
        equal(self.command(['show-options', '-wAv', '-t', target, 'remain-on-exit']), effective, 'effective window remain-on-exit')
        equal(self.command(['show-options', '-wqv', '-t', target, 'remain-on-exit']), local or '', 'local window remain-on-exit')
        equal(self.command(['show-options', '-pAv', '-t', target, 'remain-on-exit']), effective, 'effective pane remain-on-exit')
        equal(self.command(['show-options', '-gv', 'default-command']), '', 'explicit argv default-command guard')


    def exit_witness(self, label, kept):
        child = self.children[label]; before = child['child']['before']
        record = {'label': label, 'at': now(), 'expected_kept': kept,
                  'expected_exit_code': CODES[label], 'original_public': child['public'],
                  'original_child': child['child'], 'pane_samples': [], 'status': 'UNPROVEN',
                  'actual_public_exit_status': None,
                  'status_scope': 'public retained pane status' if kept else 'helper intent only; removed pane has no surviving status'}
        self.record['exits'].append(record)
        try:
            current = self.snapshot(before['pid']); record['before_release'] = current
            require(D.stable(child['child'], current), 'child changed before exit request')
            watch = self.watches[before['pid']]
            event_before = watch.poll_exit()
            record['watch'] = {'pid': before['pid'], 'identity': before,
                               'retained_since_live_observation': True, 'event_before_request': event_before}
            require(not event_before, 'child already exited before release')
            challenge = secrets.token_hex(24)
            request = {'identity': before, 'challenge': challenge, 'exit_code': CODES[label], 'issued_at': now()}
            key = stem(before); release = self.root / 'markers' / ('exit-' + key + '.json')
            record.update(request=request, request_path=str(release))
            atomic_once(release, request)
            intent_path = self.root / 'markers' / ('exit-intent-' + key + '.json')
            intent = self.wait_json(intent_path); record.update(intent=intent, intent_path=str(intent_path))
            check_marker(intent, child['child'], child['expected'], challenge)
            watch.wait(self.remaining())
            record['watch'].update(event_after_request=watch.exited, observed_at=now())
            after = D.birth(before['pid']); record['identity_after_exit_event'] = after
            require(watch.exited and (not D.same(before, after) or after['status'] == 5), 'no actual child exit')
            previous = None
            while True:
                self.remaining(); rows = self.pane_listing(); current = D.birth(before['pid'])
                record['pane_samples'].append({'at': now(), 'rows': rows, 'child_identity': current})
                require(not D.same(before, current) or current['status'] == 5, 'dead pane has live birth')
                matches = [row for row in rows if row['pane_id'] == child['public']['pane_id']]
                ready = not matches if not kept else bool(matches and matches[0]['pane_dead'] == '1')
                if kept and not matches: raise Failed('on pane disappeared')
                stable = matches[0] if kept and ready else ('ABSENT' if ready else None)
                if ready and previous == stable:
                    if kept: record['actual_public_exit_status'] = matches[0]['pane_dead_status']
                    check_exit(record); record['status'] = 'PASS'; return record
                previous = stable
                time.sleep(min(.05, self.remaining()))
        except Exception as error:
            record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error)); raise
        finally: self.checkpoint()

    def spawn(self, label):
        rows = [row for row in plan(self.config, self.phase) if row[1] == label]
        require(len(rows) == 1 and self.spawn_count < 8, 'child budget or plan mismatch')
        _, _, args, target = rows[0]; self.spawn_count += 1
        self.command(args, startup=label == 'anchor'); observed = self.observe(label, target)
        if label == 'r3': check_respawn(self.children['r2seven'], observed)
        return observed

    def execute(self):
        require(not self.socket.exists() and not list((self.root / 'markers').iterdir()), 'fixture is not fresh')
        self.profiles()
        for case in ('R1', 'R2', 'R3', 'R4'):
            self.case_end = min(time.monotonic() + 20, self.start + 150)
            entry = {'id': case, 'start': now(), 'monotonic_start': time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case == 'R1':
                    self.spawn('anchor')
                    self.options('alpha:0.0', 'off', 'off', None)
                    self.spawn('r1')
                    self.options('alpha:off.0', 'off', 'off', None)
                    self.exit_witness('r1', False)
                    self.observe('anchor', 'alpha:0.0', True)
                elif case == 'R2':
                    self.command(['set-window-option', '-g', 'remain-on-exit', 'on'])
                    for label, target in [('r2zero', 'alpha:zero.0'), ('r2seven', 'alpha:seven.0')]:
                        self.spawn(label)
                        self.options(target, 'on', 'on', None)
                        self.exit_witness(label, True)
                elif case == 'R3':
                    self.options('alpha:seven.0', 'on', 'on', None)
                    self.spawn('r3')
                    self.options('alpha:seven.0', 'on', 'on', None)
                else:
                    self.command(['set-window-option', '-t', 'alpha:0', 'remain-on-exit', 'off'])
                    self.options('alpha:0.0', 'on', 'off', 'off')
                    self.spawn('r4local')
                    self.options('alpha:0.1', 'on', 'off', 'off')
                    self.exit_witness('r4local', False)
                    self.command(['source-file', str(self.root / 'live.conf')])
                    self.options('alpha:0.0', 'on', 'on', None)
                    self.spawn('r4unset')
                    self.options('alpha:0.1', 'on', 'on', None)
                    self.exit_witness('r4unset', True)
                    self.spawn('r4beta')
                    self.options('beta:0.0', 'on', 'on', None)
                    self.exit_witness('r4beta', True)
                    self.observe('anchor', 'alpha:0.0', True)
                    self.observe('r3', 'alpha:seven.0', True)
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
                require(D.stable(D.process(self.daemon['before']['pid']), self.daemon)
                        and self.socket_info() == self.endpoint, 'cleanup ownership changed')
                self.helpers(); self.command(['kill-server'])
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
        A.check_cleanup(row)


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
        equal(gate['exit_outcomes'], [['r1', False, None], ['r2zero', True, '0'], ['r2seven', True, '7'],
                                     ['r4local', False, None], ['r4unset', True, '7'], ['r4beta', True, '0']],
              'unmodified complete oracle public exit outcomes')
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
                   'exit_outcomes': [[e['label'], e['expected_kept'], e['actual_public_exit_status']] for e in run.record['exits']],
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
