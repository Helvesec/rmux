#!/usr/bin/env python3
"""Darwin D1--D4 default-command adapter, paired with Linux DCOMMAND1.

Read-only API bundle comes from default_shell_darwin.py and is pinned before
import. This does not imply acceptance of that separate native proof. Each lot
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
import shlex
import stat
import sys
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode = True
D = None
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}'
EXTRA = ['space separated value', 'literal;$L6_DCOMMAND_MARKER', '*?[abc]', 'quote"and\'quote']
KINDS = {'d1': 'A', 'd2window': 'B', 'd2split': 'B', 'd2session': 'A',
         'd3': 'C', 'd4new': 'A', 'd4respawn': 'B'}
ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_DCOMMAND_MARKER', 'BASH_ENV',
            'ENV', 'RMUX_DISABLE_TMUX_SHIM', 'L5_DCOMMAND_API')
FILE_CAP = 4096
RECORD_CAP = 512 * 1024
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

def bind_api(pins):
    global D, Unproven, Failed
    require(len(pins) == 4, 'API dependency bundle incomplete')
    for item in pins:
        require(pin(item['path']) == item, 'API provenance changed')
    api = next(p for p in pins if p['path'].endswith('/tests/default_shell_darwin.py'))
    sys.path.insert(0, str(Path(api['path']).parent))
    D = importlib.import_module('default_shell_darwin')
    require(str(Path(D.__file__).resolve()) == api['path'], 'wrong imported Darwin API')
    Unproven, Failed = D.Unproven, D.Failed

def stem(own):
    return str(own['pid']) + '-' + '-'.join(map(str, own['birth']))

def witness(kind, root, token):
    config = json.loads(Path(os.environ['L5_DCOMMAND_API']).read_text())
    bind_api(config['api_pins'])
    root = Path(root); own = D.birth(os.getpid()); key = stem(own)
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'identity': D.birth(os.getpid()), 'cwd': os.getcwd(), 'argv': sys.argv,
                'environment': {k: os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root / ('born-' + key + '.json'), report(None))
    end = time.monotonic() + 210
    while time.monotonic() < end:
        requests = sorted(root.glob('request-' + key + '-*.json'))
        require(len(requests) <= 12, 'witness challenge count exceeded')
        for path in requests:
            ack = root / path.name.replace('request-', 'ack-', 1)
            if not ack.exists():
                require(path.stat().st_size <= FILE_CAP, 'request size cap')
                request = json.loads(path.read_text())
                require(D.same(request['identity'], own), 'challenge identity mismatch')
                atomic_once(ack, report(request['challenge']))
        time.sleep(.02)

def environment(config, phase):
    root = Path(config['roots'][phase])
    return {'HOME': str(root / 'home'), 'XDG_CONFIG_HOME': str(root / 'xdg'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/bash', 'TERM': 'vt100', 'LANG': 'C',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_DCOMMAND_MARKER': config['tokens'][phase],
            'L5_DCOMMAND_API': config['config_path']}

def witness_argv(config, phase, kind):
    return [config['pins']['python']['path'], '-I', config['driver'], 'witness', kind,
            str(Path(config['roots'][phase]) / 'markers'), config['tokens'][phase],
            *(EXTRA if kind == 'C' else [])]

def default_command(config, phase, kind):
    return 'exec ' + shlex.join(witness_argv(config, phase, kind))

def plan(config, phase):
    cwd = str(Path(config['roots'][phase]) / 'cwd')
    return [('D1', 'd1', ['new-session', '-d', '-s', 'alpha', '-c', cwd], 'alpha:0.0'),
            ('D2', 'd2window', ['new-window', '-d', '-t', 'alpha', '-n', 'local', '-c', cwd], 'alpha:local.0'),
            ('D2', 'd2split', ['split-window', '-d', '-t', 'alpha:local.0', '-c', cwd], 'alpha:local.1'),
            ('D2', 'd2session', ['new-session', '-d', '-s', 'beta', '-c', cwd], 'beta:0.0'),
            ('D3', 'd3', ['new-window', '-d', '-t', 'alpha', '-n', 'explicit', '-c', cwd,
                          *witness_argv(config, phase, 'C')], 'alpha:explicit.0'),
            ('D4', 'd4new', ['new-window', '-d', '-t', 'alpha', '-n', 'restored', '-c', cwd], 'alpha:restored.0'),
            ('D4', 'd4respawn', ['respawn-pane', '-k', '-t', 'alpha:local.0'], 'alpha:local.0')]


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
    for field in ('kind', 'token', 'cwd', 'environment'):
        equal(marker[field], expected[field], 'marker ' + field)
    equal(marker['argv'], expected['argv'][2:], 'witness actual sys.argv')
    equal(marker['challenge'], challenge, 'fresh challenge echo')


def check_child(first, second, public, daemon, expected):
    require(D.stable(first, second), 'two child snapshots incoherent')
    require(public['pane_pid'] == first['before']['pid'], 'public pane PID mismatch')
    require(public['daemon_pid'] == daemon['before']['pid'], 'public daemon mismatch')
    require(first['before']['ppid'] == daemon['before']['pid'], 'not direct own daemon child')
    fd = first['fd0']
    require(stat.S_ISCHR(fd['mode']) and fd['path'] == public['pane_tty']
            and fd['path'].startswith('/dev/ttys'), 'PTY attribution mismatch')
    equal(first['image']['executable'], expected['exe'], 'child executable')
    equal(first['image']['sha256'], expected['exe_sha256'], 'executable disk hash')
    equal(first['cwd']['path'], expected['cwd'], 'child cwd')
    equal(first['args']['argv'], expected['argv'], 'exact child argv')
    equal(base64.b64decode(first['args']['argv_base64'], validate=True),
          b'\0'.join(os.fsencode(v) for v in expected['argv']) + b'\0', 'raw argv bytes')
    for snap in (first, second):
        equal(D.parse_procargs(base64.b64decode(snap['args']['procargs_prefix_base64'])),
              snap['args'], 'raw procargs prefix fidelity')
        equal(snap['environment'], expected['environment'], 'kernel allowlisted environment')


def command_budget(record):
    require(len(record['commands']) < 64, 'CLI command count cap')
    require(len(json.dumps(record, indent=2).encode()) < 400 * 1024,
            'record reserve exhausted before exec')


def check_cleanup(row):
    require(row.get('pass') and row['all_absent'] and row['socket_absent']
            and row['fds_equal'] and not row['errors'] and not row['rescues'],
            'cleanup incomplete or rescued')
    require(all(p['exit_event'] and not D.same(p['before'], p['after'])
                for p in row['processes']), 'exit/disappearance evidence incomplete')


class Run:
    def __init__(self, frozen):
        D.Run.__init__(self, frozen, None)
        self.config = frozen['config']; self.env = environment(self.config, self.phase)
        self.record.update(environment=self.env, marker_ledgers=[])
        self.children = self.observed

    def remaining(self): return D.Run.remaining(self)
    def socket_info(self): return D.Run.socket_info(self)
    def track(self, row, role): return D.Run.track(self, row, role)
    def public(self, target): return D.Run.public(self, target)

    def checkpoint(self):
        raw = (json.dumps(self.record, indent=2) + '\n').encode()
        require(len(raw) <= RECORD_CAP, 'record cap')
        D.acquired.save(self.root / 'progress.json', self.record)

    def profiles(self, post_shutdown=False):
        row = {'at': now(), 'post_shutdown': post_shutdown,
               'entries': {n: sorted(p.name for p in (self.root / n).iterdir())
                           for n in ('home', 'zdot', 'xdg')}}
        self.record['profiles'].append(row)
        require(all(not values for values in row['entries'].values()), 'private profiles not empty')

    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent, D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own = D.birth(pid)
                if own is None or own['ppid'] != parent['pid'] or pid in self.owned: continue
                role = 'unexpected-pane' if parent['role'] == 'daemon' else 'helper'
                self.track(own, role)
                self.record['helpers'].append({'at': now(), 'parent': parent, 'identity': own, 'role': role})
                require(len(self.record['helpers']) <= 2, 'observed own helper cap')
                require(self.cleaning or role == 'helper', 'unattributed daemon child')

    def command(self, args, startup=False, required=True, check=None):
        if check is not None: required = check
        command_budget(self.record)
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup: argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        entry = {'start': now(), 'argv': argv, 'pre_exec_file_cap': FILE_CAP}
        self.record['commands'].append(entry)
        old = resource.getrlimit(resource.RLIMIT_FSIZE)
        require(old[0] == resource.RLIM_INFINITY or old[0] >= FILE_CAP, 'RLIMIT_FSIZE too small')
        try:
            # Single-threaded observer only. The CLI and its private descendants
            # inherit this kernel limit before posix_spawn; each output file is
            # bounded before both writing and drain. Restore the observer only.
            resource.setrlimit(resource.RLIMIT_FSIZE, (FILE_CAP, old[1]))
            with chdir(self.root):
                result = D.native.run(argv, env=self.env, timeout=self.remaining(), check=False)
            entry.update(pid=result.pid, returncode=result.returncode,
                         stdout=result.stdout.decode(), stderr=result.stderr.decode())
            require(len(result.stdout) < FILE_CAP and len(result.stderr) < FILE_CAP,
                    'CLI output reached inherited cap')
        except BaseException as error:
            entry.update(error=repr(error))
            raise
        finally:
            resource.setrlimit(resource.RLIMIT_FSIZE, old)
            entry['end'] = now()
        if required: require(result.returncode == 0, 'CLI failed: ' + repr(args))
        return result.stdout.decode().strip()

    def wait_json(self, path):
        while not path.exists(): time.sleep(min(.02, self.remaining()))
        require(path.stat().st_size <= FILE_CAP, 'marker size cap')
        return json.loads(path.read_text())

    def expected_for(self, label):
        kind = KINDS[label]; python = self.config['pins']['python']
        return {'kind': kind, 'token': self.config['tokens'][self.phase],
                'exe': python['path'], 'exe_sha256': python['sha256'],
                'argv': witness_argv(self.config, self.phase, kind), 'cwd': str(self.root / 'cwd'),
                'environment': {k: self.env.get(k) for k in ENV_KEYS}}

    def snapshot(self, pid):
        item = D.process(pid, pane=True)
        item['environment'] = read_environment(pid)
        after = D.birth(pid)
        require(D.same(item['after'], after), 'birth changed during environment read')
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

    def attribution(self, label, observed):
        public, original = observed['public'], self.children['d1']['public']
        if label == 'd2session':
            require(public['session_id'] != original['session_id'], 'new session not distinct')
        elif label == 'd2split':
            old = self.children['d2window']['public']
            require(public['session_id'] == old['session_id'] and public['window_id'] == old['window_id']
                    and public['pane_id'] != old['pane_id'], 'split attribution')
        elif label == 'd4respawn':
            old = self.children['d2window']; identity = old['child']['before']
            require(all(public[k] == old['public'][k] for k in ('session_id', 'window_id', 'pane_id')),
                    'respawn pane changed')
            require(not D.same(identity, observed['child']['before']), 'respawn did not replace child')
            watch = self.watches[identity['pid']]; watch.wait(self.remaining())
            after = D.birth(identity['pid'])
            observed.update(old_child_after=after, old_child_exit_event=watch.exited)
            require(not D.same(identity, after), 'old child not absent after respawn')
        elif label != 'd1':
            require(public['session_id'] == original['session_id']
                    and public['window_id'] != original['window_id'], 'new window attribution')

    def cleanup(self):
        D.Run.cleanup(self)
        check_cleanup(self.record['cleanup'])

    def options(self, session, kind, local):
        equal(self.command(['show-options', '-gv', 'default-shell']), '/bin/bash', 'default-shell guard')
        equal(self.command(['show-options', '-gv', 'default-command']), default_command(self.config, self.phase, 'A'), 'global value')
        equal(self.command(['show', '-gv', 'default-command']), default_command(self.config, self.phase, 'A'), 'show alias global value')
        equal(self.command(['show-options', '-Av', '-t', session, 'default-command']), default_command(self.config, self.phase, kind), 'effective value')
        equal(self.command(['show-options', '-qv', '-t', session, 'default-command']), default_command(self.config, self.phase, local) if local else '', 'local value')


    def ledger(self):
        entries = [{'path': str(p), 'data': json.loads(p.read_text())} for p in sorted((self.root / 'markers').glob('born-*.json'))]
        self.record['marker_ledgers'].append({'at': now(), 'entries': entries})
        equal(len(entries), self.spawn_count, 'unexpected witness execution count')
        equal({e['path'] for e in entries}, {c['born_path'] for c in self.children.values()}, 'unattributed witness marker')

    def execute(self):
        require(not self.socket.exists() and not list((self.root / 'markers').iterdir()), 'fixture is not fresh')
        self.profiles()
        for case in ('D1', 'D2', 'D3', 'D4'):
            self.case_end = min(time.monotonic() + 20, self.start + 210)
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


def verify_frozen(frozen, frozen_path):
    require(frozen['phase'] in ('oracle', 'candidate'), 'unknown phase')
    for item in frozen['artifacts']:
        require(pin(item['path']) == item, 'frozen artifact changed: ' + item['path'])
    config = frozen['config']
    mandatory = [str(Path(__file__).resolve()), config['config_path'],
                 *[p['path'] for p in config['pins'].values()],
                 *[p['path'] for p in config['api_pins']],
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
        for label, kind in KINDS.items():
            expected = witness_argv(config, 'oracle', kind)
            equal(gate['expected'][label]['argv'], expected, 'unmodified oracle argv')
            equal(base64.b64decode(gate['expected'][label]['argv_base64']),
                  b'\0'.join(os.fsencode(v) for v in expected) + b'\0', 'unmodified oracle bytes')


def execute(frozen_path):
    frozen = json.loads(frozen_path.read_text()); verify_frozen(frozen, frozen_path)
    bind_api(frozen['config']['api_pins'])
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
                   'comparison': 'Both absolute phase argv lists frozen before oracle; no argv normalization.'})
    print(json.dumps({'status': run.record['status'], 'result': pin(run.root / 'RESULT.json'),
                      'seconds': run.record['seconds']}), flush=True)
    return 0 if run.record['status'] == 'PASS' else 1


if __name__ == '__main__':
    if len(sys.argv) >= 5 and sys.argv[1] == 'witness':
        witness(*sys.argv[2:5])
    elif len(sys.argv) == 3 and sys.argv[1] == '--frozen':
        raise SystemExit(execute(Path(sys.argv[2])))
    else:
        raise SystemExit('Use --frozen FILE.json, or the private witness mode.')
