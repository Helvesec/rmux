#!/usr/bin/env python3
"""Darwin lifecycle adapter for the unchanged canonical popup reference.

The canonical producer, popup/resize/close cases, exact surface comparison and
stalled timeout gate are reused. This observes a private PTY, not Ghostty pixels.
Only already used Darwin PID/birth, text image and NOTE_EXIT primitives apply.
"""
import base64
import importlib.util
import json
import os
from pathlib import Path
import select
import signal
import sys
import time
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(ROOT / 'tests'), str(ROOT / 'scripts/bench')]
import darwin_driver as native
import input_paste_escape_darwin as darwin
import default_shell_darwin as metadata
import popup_reference as canonical

B = canonical.B
CANONICAL_SHA = '139a95efff395a1755879f9a6876b35da4db03b457c754a122f5fc19c21c46af'
CANONICAL_PATH = ROOT / 'tests/popup_reference.py'
B.equal(B.digest(CANONICAL_PATH), CANONICAL_SHA, 'accepted canonical fixture changed')


def identity(pid):
    value = darwin.bsd(pid)
    if value is None:
        return None
    birth = [value.start_sec, value.start_usec]
    return {'pid': pid, 'ppid': value.ppid, 'birth': birth,
            'start_ticks': f'{birth[0]}:{birth[1]}',
            'state': 'Z' if value.status == 5 else 'T' if value.status == 4 else 'S',
            'darwin_status': value.status,
            'start_ticks_encoding': 'Darwin seconds:microseconds, not Linux ticks'}


def proc(pid):
    before = identity(pid)
    if before is None:
        return None
    observed = darwin.identity(pid)
    args = metadata.procargs(pid)
    after = identity(pid)
    return {'before': before, 'after': after, 'at': B.now(),
            'exe': observed['executable'] if observed else None,
            'exe_sha256': observed['sha256'] if observed else None,
            'coherent': bool(observed and B.same(before, after) and
                             observed['birth'] == before['birth']),
            'argv': args['argv'], 'cwd': None,
            'cmdline_base64': args['argv_base64'], 'fd0': None,
            'argv_observation': args,
            'limit': 'Existing owned KERN_PROCARGS2/image/birth snapshot; environment discarded; no full exec history'}


def snapshot_same(a, b):
    return bool(B.complete(a) and B.complete(b) and B.same(a['before'], b['before'])
                and a['exe'] == b['exe'] and a['exe_sha256'] == b['exe_sha256'])


def environment(config, phase):
    env = ORIGINAL_ENVIRONMENT(config, phase)
    env.update(SHELL='/bin/sh', LANG='C', LC_ALL='C', PYTHONDONTWRITEBYTECODE='1')
    return env


ORIGINAL_ENVIRONMENT = B.environment
ORIGINAL_ABSENT = canonical.owned_child_absent
B.identity, B.proc, B.snapshot_same, B.environment = identity, proc, snapshot_same, environment


def owned_child_absent(own, receipt, identity=identity, waitpid=os.waitpid, parent=None):
    # The canonical default argument was bound on import to Linux identity.
    return ORIGINAL_ABSENT(own, receipt, identity=identity, waitpid=waitpid, parent=parent)


canonical.owned_child_absent = owned_child_absent
# Canonical producer argv re-enters this OS bootstrap; its producer function
# and all case bodies remain the accepted functions, without copied variants.
canonical.__file__ = __file__


class Owned(dict):
    def __init__(self, run):
        super().__init__()
        self.run = run

    def __setitem__(self, pid, birth):
        B.require(birth is not None and pid == birth['pid'], 'owned birth missing')
        if pid not in self:
            self.run.persist('owned-birth', {'identity': birth,
                'qualified_daemon': self.run.daemon,
                'endpoint': getattr(self.run, 'socket_identity', None)})
            self.run.watches[pid] = native.FixtureProcess(pid)
            row = {'at': B.now(), 'identity': birth}
            try:
                row['process'] = proc(pid) if birth['state'] != 'Z' else None
            except (OSError, metadata.Unproven) as error:
                row['observation_gap'] = repr(error)
            self.run.record['process_observations'].append(row)
            self.run.persist('process-observation', row)
        elif not B.same(self[pid], birth):
            raise B.Unproven('owned PID reused')
        super().__setitem__(pid, birth)
        live = sum(B.same(old, identity(p)) for p, old in self.items())
        self.run.record['max_known_live_processes'] = max(
            self.run.record.get('max_known_live_processes', 0), live)
        # One daemon, at most eight other known helpers in this sequential recipe.
        B.require(live <= 9, 'known helper bound exceeded')


class DarwinRun(canonical.Run):
    def persist(self, event, value):
        with (self.root / 'OWNED-LIFECYCLE.jsonl').open('a') as stream:
            stream.write(json.dumps({'at': B.now(), 'event': event, 'value': value}) + '\n')
            stream.flush()
            os.fsync(stream.fileno())

    def __init__(self, config, phase):
        super().__init__(config, phase)
        self.watches = {}
        self.record['process_observations'] = []
        self.record['census_samples'] = []
        self.owned = Owned(self)
        self.last_guard = 0
        self.last_census = None
        self.record['resource_samples'] = []
        self.record['display_scope'] = 'private native PTY parsed by pinned pyte; not Ghostty'
        self.record['full_transient_history'] = 'unknown; not required or inferred here'
        self.record['canonical_fixture_sha256'] = CANONICAL_SHA

    def census(self):
        stamp = time.monotonic()
        parents, seen = [os.getpid()], set()
        if self.daemon and B.same(self.daemon['before'], identity(self.daemon['before']['pid'])):
            parents.append(self.daemon['before']['pid'])
        while parents:
            parent = parents.pop()
            if parent in seen:
                continue
            seen.add(parent)
            if parent != os.getpid() and not B.same(self.owned.get(parent), identity(parent)):
                continue
            for pid in metadata.children(parent):
                birth = identity(pid)
                if birth and birth['ppid'] == parent:
                    self.owned[pid] = birth
                    parents.append(pid)
        daemon_pid = self.daemon['before']['pid'] if self.daemon else None
        live = [b for pid, b in self.owned.items()
                if pid != daemon_pid and B.same(b, identity(pid))]
        self.record['max_owned_helpers'] = max(self.record['max_owned_helpers'], len(live))
        self.record['census_samples'].append({'at': B.now(), 'helpers': live,
            'gap_seconds': stamp - self.last_census if self.last_census else None})
        self.last_census = stamp
        B.require(len(live) <= 8, 'owned helper cap including transient commands')

    def guard(self, force=False):
        stamp = time.monotonic()
        if not force and stamp - self.last_guard < 1:
            return
        # These roots belong to this job. Files are persistent, never removed
        # during the lot; ordinary metadata walks do not suspend the product.
        usage = {}
        for name, roots in self.config['measure_paths'].items():
            total, seen = 0, set()
            for root in roots:
                root = Path(root)
                for path in [root, *root.rglob('*')]:
                    st = path.lstat()
                    key = st.st_dev, st.st_ino
                    if key not in seen:
                        seen.add(key)
                        total += st.st_blocks * 512
            usage[name] = total
            B.require(total <= self.config['caps'][name], 'resource cap: ' + name)
        fs = os.statvfs(self.root)
        free = fs.f_bavail * fs.f_frsize
        B.require(free >= 30_000_000_000, 'free space floor')
        self.record['resource_samples'].append({'at': B.now(), 'usage': usage,
            'free_bytes': free, 'gap_seconds': stamp - self.last_guard if self.last_guard else None})
        self.last_guard = stamp

    def pump(self, duration=.02, read=True):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            self.remaining()
            if read and self.master is not None and select.select([self.master], [], [], .005)[0]:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    break
                if not data:
                    break
                self.capture.extend(data)
                B.require(len(self.capture) <= canonical.MAX_CAPTURE, 'PTY capture cap')
                self.stream.feed(data)
            else:
                time.sleep(.005)
        self.guard()
        self.census()
        # Observe only births learned from the private CLI/producer receipts.
        for watch in self.watches.values():
            watch.poll_exit()

    def command(self, args, startup=False, required=True, raw=False):
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup:
            argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        row = {'argv': argv, 'start': B.now(), 'timeout': False}
        B.require(len(self.record['commands']) < 512, 'CLI command cap')
        self.record['commands'].append(row)
        def observe(event, pid):
            birth = identity(pid)
            if event == 'spawn':
                B.require(birth is not None, 'direct CLI launch birth unavailable')
                self.owned[pid] = birth
            else:
                # Darwin may return ESRCH after NOTE_EXIT even before waitpid.
                # The driver still owns this unreaped child and records wait.
                B.require(pid in self.owned, 'direct CLI launch birth not recorded')
                B.require(birth is None or B.same(self.owned[pid], birth), 'CLI birth changed')
            self.census()
            return {'event': event, 'at': B.now(), 'identity': birth,
                    'launch_identity': self.owned[pid],
                    'launch_argv': argv, 'meaning': 'owned launch argv, not guessed current argv'}
        try:
            result = native.run(argv, env=self.env, timeout=min(8, self.remaining()),
                                check=False, observer=observe)
        except Exception as error:
            row.update(error=repr(error), timeout=isinstance(
                error, (TimeoutError, canonical.subprocess.TimeoutExpired)), end=B.now())
            self.persist('command-error', row)
            raise
        row.update(end=B.now(), pid=result.pid, exit_code=result.returncode,
                   stdout=result.stdout.decode(), stderr=result.stderr.decode(),
                   stdout_base64=base64.b64encode(result.stdout).decode(),
                   stderr_base64=base64.b64encode(result.stderr).decode(),
                   direct_child_reaped=True, elapsed_ms=result.elapsed_ms,
                   lifecycle=result.observation)
        self.persist('command-complete', row)
        B.require(len(result.stdout) + len(result.stderr) < 65536, 'command output cap')
        if required:
            B.require(result.returncode == 0, 'command failed: ' + repr(row))
        return result.stdout if raw else result.stdout.decode().strip()

    def cleanup(self):
        start = time.monotonic()
        self.cleaning = True
        self.case_end = min(start + 55, self.start + 180)
        result = {'start': B.now(), 'actions': [], 'direct_waits': [], 'identities': []}
        self.record['cleanup'] = result
        self.persist('cleanup-start', {'deadline_seconds': self.remaining()})
        # Canonical case verdict (including seven-second timeout query) already
        # exists before any cleanup command or signal.
        if self.daemon is None and self.socket_info():
            # Startup may have succeeded before its observation failed. The
            # main entrypoint required a fresh absent private endpoint.
            try:
                public, _ = self.public('alpha')
                pane = identity(public['pane_pid'])
                if pane is not None:
                    self.owned[pane['pid']] = pane
                self.census()
                result['recovered_startup_identity'] = self.daemon
                self.persist('startup-recovered', {'daemon': self.daemon,
                    'pane': pane, 'endpoint': self.socket_identity})
            except Exception as error:
                result['startup_recovery_error'] = repr(error)
                self.persist('startup-recovery-error', result)
        if self.daemon and B.same(self.daemon['before'], identity(self.daemon['before']['pid'])):
            B.require(self.socket_info() == self.socket_identity, 'private endpoint changed')
            try:
                self.command(['kill-server'], required=False)
                result['actions'].append('qualified private kill-server after verdict')
            except Exception as error:
                result['kill_server_error'] = repr(error)
        for process, role in [(self.client, 'attach'), (self.popup_command, 'popup-command')]:
            if process is None:
                continue
            until = time.monotonic() + min(3, self.remaining())
            while process.poll() is None and time.monotonic() < until:
                self.pump(.02)
            if process.poll() is None:
                current = identity(process.pid)
                B.require(B.same(self.owned.get(process.pid), current), 'cleanup direct birth mismatch')
                process.terminate()
                result['actions'].append({'signal': 'TERM', 'identity': current})
            code = process.wait(timeout=min(2, self.remaining()))
            result['direct_waits'].append({'role': role, 'pid': process.pid, 'returncode': code})
            self.persist('cleanup-direct-wait', result['direct_waits'][-1])
        for pid, saved in self.owned.items():
            self.remaining()
            watch = self.watches[pid]
            until = time.monotonic() + min(2, self.remaining())
            while B.same(saved, identity(pid)) and time.monotonic() < until:
                watch.poll_exit()
                time.sleep(.01)
            current = identity(pid)
            if B.same(saved, current):
                os.kill(pid, signal.SIGTERM)
                result['actions'].append({'signal': 'TERM', 'identity': current})
                self.persist('cleanup-signal', result['actions'][-1])
                watch.wait(min(2, self.remaining()))
            watch.poll_exit()
            after = identity(pid)
            result['identities'].append({'before': saved, 'after': after,
                'NOTE_EXIT': watch.exited, 'same_birth_absent': not B.same(saved, after),
                'numeric_indirect_wait_status': None})
            self.persist('cleanup-identity', result['identities'][-1])
            watch.close()
        if self.master is not None:
            os.close(self.master)
            self.master = None
        endpoint = self.socket_info()
        all_absent = all(x['same_birth_absent'] for x in result['identities'])
        if endpoint and all_absent and endpoint == self.socket_identity:
            self.socket.unlink()
            result['owned_stale_endpoint_removed'] = endpoint
        result.update(end=B.now(), seconds=time.monotonic() - start,
                      all_known_births_absent=all_absent, socket_after=self.socket_info())
        self.profiles()
        self.guard(force=True)
        B.require(all_absent and result['socket_after'] is None, 'owned cleanup incomplete')
        B.require('startup_recovery_error' not in result, 'startup recovery failed')
        B.require(result['seconds'] <= 60, 'cleanup limit')
        result['status'] = 'PASS'
        self.persist('cleanup-complete', result)


def self_test(root, python_lib):
    sys.path.insert(0, python_lib)
    root = Path(root)
    root.mkdir()
    code = canonical.self_test(root / 'canonical')
    B.equal(code, 0, 'canonical synthetic controls')
    import pyte
    screen = pyte.Screen(225, 54)
    pyte.ByteStream(screen).feed(canonical.final_bytes('darwin-control'))
    B.equal(screen.display, canonical.final_rows('darwin-control'), 'parser exact rows')
    B.equal([screen.cursor.x, screen.cursor.y], [11, 7], 'parser exact cursor')
    from unittest.mock import patch
    from types import SimpleNamespace
    obj = object.__new__(DarwinRun)
    obj.binary, obj.socket, obj.root = '/not-executed', root / 's', root
    obj.env, obj.record = {}, {'commands': []}
    obj.remaining = lambda: 1
    fake = SimpleNamespace(pid=999, returncode=1, stdout=b'', stderr=b'failure',
                           elapsed_ms=0, observation=None)
    rejected = False
    with patch.object(native, 'run', return_value=fake):
        try:
            obj.command(['list-clients'])
        except B.Unproven:
            rejected = True
    B.require(rejected, 'Darwin adapter accepted nonzero empty query')
    for error in (TimeoutError('control'), canonical.subprocess.TimeoutExpired('control', 1),
                  OSError('synthetic spawn failure')):
        with patch.object(native, 'run', side_effect=error):
            try:
                obj.command(['list-clients'])
            except type(error) as observed:
                B.require(observed is error, 'adapter replaced command exception')
            else:
                raise B.Unproven('adapter suppressed command exception')
        B.equal(obj.record['commands'][-1]['timeout'],
                isinstance(error, (TimeoutError, canonical.subprocess.TimeoutExpired)),
                'actual driver timeout metadata')
    obj.record = {'commands': [], 'process_observations': [], 'census_samples': [],
                  'max_owned_helpers': 0}
    obj.watches, obj.daemon, obj.last_census = {}, None, None
    obj.owned = Owned(obj)
    real_run = native.run
    control_argv = [sys.executable, '-c', 'import time; time.sleep(.05)']
    def control_run(_argv, **kwargs):
        return real_run(control_argv, **kwargs)
    with patch.object(native, 'run', side_effect=control_run):
        obj.command(['synthetic-control'])
    direct_control = obj.record
    B.equal(direct_control['commands'][0]['lifecycle']['raw_wait_status'], 0,
            'real short child wait status')
    B.require(direct_control['commands'][0]['lifecycle']['events'][0]['launch_identity'],
              'real short child launch birth')
    for watch in obj.watches.values():
        watch.close()
    # Exercise the imported producer and its real transitive dependencies.
    # Only terminal output/size are synthetic; current argv comes from the OS.
    controls = []
    for label in ('burst300', 'replace-old', 'replace-new'):
        folder = root / label
        folder.mkdir()
        for name in ('go', 'exit', 'continue0', 'continue1', 'continue2', 'continue3'):
            (folder / name).touch()
        output = bytearray()
        def write(fd, data):
            B.equal(fd, 1, 'producer output descriptor')
            output.extend(data)
            return len(data)
        with patch.object(os, 'write', side_effect=write), patch.object(
                os, 'get_terminal_size', return_value=os.terminal_size((225, 54))):
            canonical.producer(str(folder), label, '512000')
        born = json.loads((folder / 'born.json').read_text())['process']
        B.require(B.complete(born), 'real self producer identity incomplete')
        B.equal(born['argv'], sys.orig_argv, 'kernel argv differs from interpreter launch argv')
        B.equal(len(output), 512000, 'full imported producer output')
        B.require(output.endswith(canonical.final_bytes(label)), 'producer final dependency')
        if label.startswith('replace-'):
            B.require(output.startswith(('BURST039|' + label + '|').encode()), 'burst_bytes dependency')
        controls.append({'label': label, 'bytes': len(output), 'producer': born})
    bad = root / 'bad-config.json'
    B.write_once(bad, {'pins': {str(CANONICAL_PATH): 'invalid'}})
    rejected = False
    try:
        main(bad)
    except B.Failed:
        rejected = True
    B.require(rejected, 'actual main accepted changed input')
    common = json.loads((root / 'canonical/RESULT.json').read_text())
    B.write_once(root / 'RESULT.json', {'status': 'PASS', 'canonical_controls': len(common['checks']),
        'parser_all_rows_cursor': True, 'nonzero_empty_query_rejected': True,
        'producer_controls': controls, 'main_bad_pin_rejected': True, 'native_runs': 0,
        'short_child_control': {'actual_argv': control_argv, 'record': direct_control,
                               'product_processes': 0}})
    return 0


def main(config_path):
    config = json.loads(Path(config_path).read_text())
    for path, expected in config['pins'].items():
        B.equal(B.digest(path), expected, 'frozen input changed: ' + path)
    sys.path.insert(0, config['python_lib'])
    root = Path(config['root']) / config['phase']
    B.require(not (root / 's').exists(), 'native entrypoint requires fresh private endpoint')
    B.write_once(root / 'STARTED.json', {'at': B.now(), 'config_sha256': B.digest(config_path),
                                     'runner': proc(os.getpid())})
    run = DarwinRun(config, config['phase'])
    try:
        run.guard(force=True)
        run.execute()
        if config.get('focus_replacement'):
            case = run.record['cases'][0]
            B.require(case['old_boundary']['bytes'] < 512000, 'old burst must be incomplete')
            B.equal(len(case['delayed_samples']), 5, 'five delayed observations')
            B.require(all(x['capture_bytes'] == case['delayed_raw_offset']
                          for x in case['delayed_samples']), 'additional bytes after new EOF')
    except BaseException:
        run.record.update(status='FAIL', error=traceback.format_exc())
    finally:
        try:
            B.write_once(root / 'VERDICT-BEFORE-CLEANUP.json', run.record)
        except BaseException:
            run.record.update(status='FAIL', verdict_write_error=traceback.format_exc())
        try:
            run.cleanup()
        except BaseException:
            run.record.update(status='FAIL', cleanup_error=traceback.format_exc())
        run.record['end'] = B.now()
        B.write_once(root / 'RESULT.json', run.record)
    print(json.dumps({'phase': config['phase'], 'status': run.record.get('status'),
                      'cleanup': run.record.get('cleanup', {}).get('status')}))
    return 0 if run.record.get('status') == 'PASS' else 1


if __name__ == '__main__':
    if sys.argv[1] == 'producer':
        canonical.producer(*sys.argv[2:])
    elif sys.argv[1] == '--self-test':
        raise SystemExit(self_test(*sys.argv[2:]))
    else:
        raise SystemExit(main(sys.argv[1]))
