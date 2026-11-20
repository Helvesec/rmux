#!/usr/bin/env python3
"""Bounded Darwin default-shell S1--S5 adapter; no runtime changes or GUI input.

Oracle and candidate are separate frozen lots. Only owned endpoint PIDs and
their observed children are queried. Disk hashes are not process-image hashes.
"""
import argparse
import base64
from contextlib import chdir
import ctypes as C
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import stat
import struct
import sys
import time
import traceback

import input_paste_escape_darwin as acquired

native = acquired.native
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}'
TEXT = 'read -r L6_DSH_HOLD'
KINDS = {'s1': 'bash', 's2window': 'zsh', 's2split': 'zsh', 's2session': 'bash',
         's3': 'bash', 's4': 'bash', 's5direct': 'sleep', 's5text': 'bash'}
now, sha = acquired.now, acquired.sha


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


def pin(path):
    path = Path(path)
    return {'path': str(path), 'bytes': path.stat().st_size, 'sha256': sha(path)}


def write_once(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n'); stream.flush(); os.fsync(stream.fileno())


def parse_procargs(raw):
    """Decode argc and exactly its argv; never persist the environment tail."""
    require(len(raw) >= 6, 'short KERN_PROCARGS2 buffer')
    argc = struct.unpack_from('=i', raw)[0]
    require(0 < argc <= 128, 'invalid bounded argc')
    end = raw.find(b'\0', 4)
    require(end > 4, 'missing executable spelling')
    executable = raw[4:end]
    offset = end + 1
    while offset < len(raw) and raw[offset] == 0:
        offset += 1
    begin = offset
    argv = []
    for _ in range(argc):
        end = raw.find(b'\0', offset)
        require(end >= offset, 'truncated argv')
        argv.append(raw[offset:end]); offset = end + 1
    return {'argc': argc, 'exec_spelling': os.fsdecode(executable),
            'argv': [os.fsdecode(value) for value in argv],
            'argv_base64': base64.b64encode(raw[begin:offset]).decode(),
            'procargs_prefix_base64': base64.b64encode(raw[:offset]).decode(),
            'argv_offset': begin, 'prefix_bytes': offset,
            'environment_tail': 'discarded; not recorded'}


class VNodePath(C.Structure):
    _fields_ = [('vnode', acquired.VNode), ('path', C.c_char * 1024)]


class CwdInfo(C.Structure):
    _fields_ = [('cwd', VNodePath), ('root', VNodePath)]


SYSTEM = C.CDLL(None, use_errno=True)
SYSTEM.sysctl.argtypes = [C.POINTER(C.c_int), C.c_uint, C.c_void_p,
                         C.POINTER(C.c_size_t), C.c_void_p, C.c_size_t]
SYSTEM.sysctl.restype = C.c_int
acquired.LIB.proc_listpids.argtypes = [C.c_uint32, C.c_uint32, C.c_void_p, C.c_int]
acquired.LIB.proc_listpids.restype = C.c_int


def procargs(pid):
    mib = (C.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2; pinned Darwin SDK.
    buffer = C.create_string_buffer(1024 * 1024)
    size = C.c_size_t(len(buffer))
    if SYSTEM.sysctl(mib, 3, buffer, C.byref(size), None, 0) != 0:
        raise OSError(C.get_errno(), 'KERN_PROCARGS2 own process')
    require(size.value < len(buffer), 'procargs buffer truncated')
    return parse_procargs(buffer.raw[:size.value])


def cwd(pid):
    value = CwdInfo()
    size = acquired.LIB.proc_pidinfo(pid, 9, 0, C.byref(value), C.sizeof(value))
    require(size == C.sizeof(value), 'PROC_PIDVNODEPATHINFO incomplete')
    info = value.cwd.vnode.info
    return {'path': os.fsdecode(value.cwd.path), 'dev': info.dev, 'ino': info.ino,
            'returned_size': size}


def birth(pid):
    row = acquired.bsd(pid)
    if row is None:
        return None
    return {'pid': row.pid, 'birth': [row.start_sec, row.start_usec],
            'ppid': row.ppid, 'pgid': row.pgid, 'status': row.status}


def same(a, b):
    return bool(a and b and a['pid'] == b['pid'] and a['birth'] == b['birth'])


def process(pid, pane=False):
    before = birth(pid)
    require(before is not None and before['status'] != 5, 'own process unavailable or zombie')
    image = acquired.identity(pid)
    args = procargs(pid)
    location = cwd(pid)
    fd0 = acquired.fd_identity(pid, 0) if pane else None
    after = birth(pid)
    require(same(before, after) and same(before, image) and before['ppid'] == after['ppid'],
            'process changed during observation')
    return {'at': now(), 'before': before, 'after': after, 'image': image,
            'args': args, 'cwd': location, 'fd0': fd0}


def stable(a, b):
    return (same(a['before'], a['after']) and same(b['before'], b['after'])
            and same(a['before'], b['before']) and same(a['after'], b['after'])
            and a['before']['ppid'] == b['before']['ppid']
            and all(a['image'][k] == b['image'][k] for k in ('executable', 'sha256'))
            and a['args']['argv_base64'] == b['args']['argv_base64']
            and a['cwd'] == b['cwd'] and a['fd0'] == b['fd0'])


def children(parent):
    buffer = (C.c_int * 64)()
    count = acquired.LIB.proc_listpids(6, parent, buffer, C.sizeof(buffer))  # PPID_ONLY
    require(0 <= count < C.sizeof(buffer) and count % C.sizeof(C.c_int) == 0,
            'owned children query invalid or truncated')
    return [pid for pid in buffer[:count // C.sizeof(C.c_int)] if pid > 0]


def fd_snapshot():
    result = {}
    for name in os.listdir('/dev/fd'):
        fd = int(name)
        try:
            info = os.fstat(fd)
        except OSError:
            continue
        target = None
        try:
            target = os.fsdecode(fcntl.fcntl(fd, fcntl.F_GETPATH, b'\0' * 1024).split(b'\0', 1)[0])
        except OSError:
            pass  # Pipes/kqueues are identified by fstat, never a guessed pathname.
        result[name] = {'dev': info.st_dev, 'ino': info.st_ino, 'rdev': info.st_rdev,
                        'mode': info.st_mode, 'path': target}
    return result


def environment(root):
    return {'HOME': str(root / 'home'), 'ZDOTDIR': str(root / 'zdot'),
            'XDG_CONFIG_HOME': str(root / 'xdg'), 'PATH': '/usr/bin:/bin',
            'SHELL': '/bin/zsh', 'TERM': 'vt100', 'LANG': 'C', 'LC_ALL': 'C',
            'RMUX_DISABLE_TMUX_FALLBACK': '1'}


def check_profiles(row, uid):
    entries = row['entries']
    require(set(entries) == {'home', 'zdot', 'xdg'}, 'profile inventory incomplete')
    if not any(entries.values()):
        return
    require(row.get('post_shutdown') is True and entries == {'home': [], 'zdot': ['.zsh_history'], 'xdg': []},
            'private startup/profile input appeared')
    history = row['history']
    require(stat.S_ISREG(history['mode']) and history['uid'] == uid and history['bytes'] == 0
            and history['sha256'] == hashlib.sha256(b'').hexdigest(),
            'post-shutdown history is not an owned empty regular file')


def plan():
    return [('S1', 's1', ['new-session', '-d', '-s', 'alpha'], 'alpha:0.0'),
            ('S2', 's2window', ['new-window', '-d', '-t', 'alpha', '-n', 'local'], 'alpha:local.0'),
            ('S2', 's2split', ['split-window', '-d', '-t', 'alpha:local.0'], 'alpha:local.1'),
            ('S2', 's2session', ['new-session', '-d', '-s', 'beta'], 'beta:0.0'),
            ('S3', 's3', ['respawn-pane', '-k', '-t', 'alpha:0.0'], 'alpha:0.0'),
            ('S4', 's4', ['new-window', '-d', '-t', 'alpha', '-n', 'restored'], 'alpha:restored.0'),
            ('S5', 's5direct', ['new-window', '-d', '-t', 'alpha', '-n', 'direct', '/bin/sleep', '120'], 'alpha:direct.0'),
            ('S5', 's5text', ['new-window', '-d', '-t', 'alpha', '-n', 'text', TEXT], 'alpha:text.0')]


def check_child(a, b, public, daemon, label, pins, root, expected=None):
    require(stable(a, b), 'two child snapshots differ')
    require(public['pane_pid'] == a['before']['pid'], 'pane PID attribution differs')
    require(public['daemon_pid'] == daemon['before']['pid'], 'daemon PID attribution differs')
    require(a['before']['ppid'] == daemon['before']['pid'], 'pane not direct child of own daemon')
    require(stat.S_ISCHR(a['fd0']['mode']) and a['fd0']['path'] == public['pane_tty']
            and public['pane_tty'].startswith('/dev/ttys'), 'pane PTY attribution differs')
    equal(a['cwd']['path'], str(root), 'pane cwd')
    kind = KINDS[label]
    require(a['image']['sha256'] in {pins[k]['sha256'] for k in ('bash', 'zsh', 'sleep')},
            'unexpected image; startup redirection cannot be excluded')
    equal(a['image']['executable'], pins[kind]['path'], 'child executable path')
    equal(a['image']['sha256'], pins[kind]['sha256'], 'child executable disk hash')
    args = a['args']
    raw = base64.b64decode(args['argv_base64'], validate=True)
    equal(raw, b'\0'.join(os.fsencode(v) for v in args['argv']) + b'\0', 'raw argv fidelity')
    equal(parse_procargs(base64.b64decode(args['procargs_prefix_base64'])), args, 'procargs prefix fidelity')
    if expected is not None:
        equal(args['argv'], expected['argv'], 'exact oracle argv')
        equal(args['argv_base64'], expected['argv_base64'], 'exact oracle argv bytes')
    elif label == 's5direct':
        equal(args['argv'], ['/bin/sleep', '120'], 'direct argv without shell')
    elif label == 's5text':
        require(len(args['argv']) == 3 and args['argv'][0] and args['argv'][1:] == ['-c', TEXT],
                'oracle textual shell shape differs')
    else:
        require(len(args['argv']) == 1 and args['argv'][0].startswith('-'),
                'oracle implicit login shell shape differs')


class Run:
    def __init__(self, frozen, expected):
        self.frozen, self.expected = frozen, expected
        self.root = Path(frozen['root']); self.phase = frozen['phase']
        self.binary = frozen['pins']['tmux' if self.phase == 'oracle' else 'rmux']['path']
        self.socket = self.root / 's'; self.env = environment(self.root)
        self.start = time.monotonic(); self.case_end = self.start + 20
        self.cleaning = False; self.daemon = None; self.endpoint = None
        self.owned = {}; self.watches = {}; self.observed = {}; self.spawn_count = 0
        self.record = {'phase': self.phase, 'start': now(), 'commands': [], 'cases': [],
                       'observations': [], 'public_samples': [], 'profiles': [],
                       'helpers': [], 'environment': self.env, 'fd_before': fd_snapshot()}

    def remaining(self):
        end = self.start + 240 if self.cleaning else min(self.case_end, self.start + 210)
        left = end - time.monotonic()
        require(left > 0, 'native deadline exceeded')
        return min(left, 5)

    def checkpoint(self):
        raw = json.dumps(self.record, indent=2) + '\n'
        require(len(raw.encode()) < 16 * 1024**2, 'native log budget exceeded')
        acquired.save(self.root / 'progress.json', self.record)

    def profiles(self, post_shutdown=False):
        row = {'at': now(), 'post_shutdown': post_shutdown,
               'entries': {n: sorted(p.name for p in (self.root / n).iterdir())
                                       for n in ('home', 'zdot', 'xdg')}}
        if post_shutdown and row['entries']['zdot'] == ['.zsh_history']:
            path = self.root / 'zdot/.zsh_history'
            info = path.lstat()
            row['history'] = {**pin(path), 'mode': info.st_mode, 'uid': info.st_uid}
        self.record['profiles'].append(row)
        check_profiles(row, os.getuid())

    def socket_info(self):
        try:
            s = self.socket.lstat()
        except FileNotFoundError:
            return None
        require(stat.S_ISSOCK(s.st_mode) and s.st_uid == os.getuid(), 'private endpoint not owned socket')
        return {'path': str(self.socket), 'device': s.st_dev, 'inode': s.st_ino, 'uid': s.st_uid}

    def track(self, row, role):
        pid = row['pid']
        if pid in self.owned:
            require(same(row, self.owned[pid]), 'tracked PID reused')
        else:
            self.owned[pid] = {**row, 'role': role}
            self.watches[pid] = native.FixtureProcess(pid)
        return row

    def helpers(self):
        # Sample only known owned parents. Short-lived helpers can escape sampling.
        for parent in list(self.owned.values()):
            if not same(parent, birth(parent['pid'])):
                continue
            for pid in children(parent['pid']):
                row = birth(pid)
                if row is None or row['ppid'] != parent['pid'] or pid in self.owned:
                    continue
                require(parent['role'] != 'daemon', 'unattributed extra daemon child')
                self.track(row, 'helper')
                self.record['helpers'].append({'at': now(), 'parent': parent, 'identity': row})
                require(len(self.record['helpers']) <= 8, 'observed helper cap exceeded')

    def command(self, args, startup=False, check=True):
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup:
            argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        entry = {'start': now(), 'argv': argv}; self.record['commands'].append(entry)
        self.checkpoint()
        with chdir(self.root):
            result = native.run(argv, env=self.env, timeout=self.remaining(), check=False)
        entry.update(end=now(), pid=result.pid, waitpid_reaped=True, returncode=result.returncode,
                     stdout_hex=result.stdout.hex(), stderr_hex=result.stderr.hex())
        require(len(result.stdout) + len(result.stderr) < 65536, 'command output cap exceeded')
        self.checkpoint()
        if check:
            require(result.returncode == 0, 'command failed: ' + repr(args) + ': ' + repr(result.stderr))
        return result.stdout.decode().strip()

    def public(self, target):
        fields = self.command(['display-message', '-p', '-t', target, FORMAT]).split('|')
        require(len(fields) == 7, 'public identity format incomplete')
        public = dict(zip(('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_tty'), fields))
        public['daemon_pid'] = int(public['daemon_pid']); public['pane_pid'] = int(public['pane_pid'])
        require(public['socket'] == str(self.socket), 'public endpoint differs')
        endpoint = self.socket_info(); require(endpoint is not None, 'endpoint missing')
        row = process(public['daemon_pid'])
        self.track(row['before'], 'daemon')
        key = 'tmux' if self.phase == 'oracle' else 'rmux_daemon'
        equal(row['image']['sha256'], self.frozen['pins'][key]['sha256'], 'daemon disk identity')
        if self.daemon is None:
            self.daemon, self.endpoint = row, endpoint
        else:
            require(stable(self.daemon, row) and endpoint == self.endpoint, 'daemon or endpoint changed')
        item = {'at': now(), 'target': target, 'public': public, 'daemon': row, 'endpoint': endpoint}
        self.record['public_samples'].append(item)
        return item

    def observe(self, label, target, reobserve=False):
        item = {'label': label, 'target': target, 'reobserve': reobserve, 'samples': []}
        self.record['observations'].append(item); previous = None
        while True:
            self.remaining(); pub = self.public(target)
            child = process(pub['public']['pane_pid'], pane=True)
            self.track(child['before'], 'pane')
            sample = {'public': pub, 'child': child}; item['samples'].append(sample)
            self.helpers(); self.checkpoint()
            if previous and stable(previous['child'], child) and previous['public']['public'] == pub['public']:
                check_child(previous['child'], child, pub['public'], pub['daemon'], label,
                            self.frozen['pins'], self.root, self.expected.get(label) if self.expected else None)
                if reobserve:
                    old = self.observed[label]
                    require(stable(old['child'], child) and old['public'] == pub['public'], 'existing child changed')
                item.update(status='PASS', child=child, public=pub['public'])
                if not reobserve:
                    self.observed[label] = item
                return item
            previous = sample
            time.sleep(min(.10, self.remaining()))

    def options(self, session, effective, local):
        equal(self.command(['show-options', '-gv', 'default-shell']), '/bin/bash', 'global shell')
        equal(self.command(['show-options', '-Av', '-t', session, 'default-shell']), effective, 'effective shell')
        equal(self.command(['show-options', '-qv', '-t', session, 'default-shell']), local, 'local shell')
        equal(self.command(['show-options', '-Av', '-t', session, 'default-command']), '', 'default-command')

    def attribution(self, label, obs):
        public = obs['public']; original = self.observed['s1']['public']
        if label == 's2session':
            require(public['session_id'] != original['session_id'], 'new session not distinct')
        elif label == 's2split':
            old = self.observed['s2window']['public']
            require(all(public[k] == old[k] for k in ('session_id', 'window_id'))
                    and public['pane_id'] != old['pane_id'], 'split attribution differs')
        elif label == 's3':
            old = self.observed['s1']
            require(all(public[k] == original[k] for k in ('session_id', 'window_id', 'pane_id')), 'respawn changed pane')
            require(not same(old['child']['before'], obs['child']['before']), 'respawn child not replaced')
            pid = old['child']['before']['pid']; self.watches[pid].wait(self.remaining())
            after = birth(pid); obs['old_child_after'] = after
            obs['old_child_exit_event'] = self.watches[pid].exited
            require(not same(old['child']['before'], after), 'old respawn child not reaped/absent')
        elif label != 's1':
            require(public['session_id'] == original['session_id'] and public['window_id'] != original['window_id'],
                    'new-window attribution differs')

    def execute(self):
        require(not self.socket.exists(), 'private endpoint already present'); self.profiles()
        for case in ('S1', 'S2', 'S3', 'S4', 'S5'):
            self.case_end = min(time.monotonic() + 20, self.start + 210)
            entry = {'id': case, 'start': now()}; self.record['cases'].append(entry); start = time.monotonic()
            try:
                if case == 'S2':
                    self.command(['set-option', '-t', 'alpha', 'default-shell', '/bin/zsh'])
                    self.options('alpha', '/bin/zsh', '/bin/zsh'); self.observe('s1', 'alpha:0.0', True)
                if case == 'S3':
                    self.options('alpha', '/bin/zsh', '/bin/zsh')
                if case == 'S4':
                    self.command(['source-file', str(self.root / 'live.conf')]); self.options('alpha', '/bin/bash', '')
                for _, label, args, target in [row for row in plan() if row[0] == case]:
                    require(self.spawn_count < 8, 'pane child cap exceeded'); self.spawn_count += 1
                    self.command(args, startup=case == 'S1')
                    obs = self.observe(label, target); self.attribution(label, obs)
                    if label == 's1': self.options('alpha', '/bin/bash', '')
                    if label == 's2session': self.options('beta', '/bin/bash', '')
                    if label == 's4': self.observe('s2window', 'alpha:local.0', True)
                self.profiles(); self.remaining(); entry['status'] = 'PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error)); raise
            finally:
                entry.update(end=now(), seconds=time.monotonic() - start); self.checkpoint()

    def cleanup(self):
        self.cleaning = True; stop = min(time.monotonic() + 30, self.start + 240)
        row = {'start': now(), 'errors': [], 'rescues': [], 'processes': []}; self.record['cleanup'] = row
        def attempt(name, fn):
            try: return fn()
            except Exception as error: row['errors'].append({'step': name, 'error': repr(error)})
        if self.daemon is None and self.socket.exists():
            attempt('recover-owned-endpoint', lambda: self.public('alpha:0.0'))
        if self.daemon:
            def close_server():
                require(stable(process(self.daemon['before']['pid']), self.daemon)
                        and self.socket_info() == self.endpoint, 'cleanup ownership changed')
                self.helpers(); self.command(['kill-server'])
            attempt('qualified-kill-server', close_server)
        for pid, old in self.owned.items():
            watched = self.watches[pid]
            attempt('wait-exit-' + str(pid), lambda: watched.wait(max(.01, min(3, stop - time.monotonic()))))
            if not watched.poll_exit() and same(old, birth(pid)):
                row['rescues'].append({'identity': old, 'signal': int(signal.SIGKILL)})
                attempt('rescue-' + str(pid), lambda: watched.retire_after_failure(max(.01, min(2, stop - time.monotonic()))))
            reaped = None
            try:
                collected, status = os.waitpid(pid, os.WNOHANG)
                reaped = {'pid': collected, 'raw_status': status}
            except ChildProcessError:
                reaped = {'ECHILD': True, 'note': 'indirect process; exit and disappearance checked separately'}
            after = birth(pid)
            while same(old, after) and time.monotonic() < stop:
                time.sleep(.02); after = birth(pid)
            row['processes'].append({'before': old, 'after': after, 'exit_event': watched.exited, 'waitpid': reaped})
        row['all_absent'] = all(not same(p['before'], p['after']) and p['exit_event'] for p in row['processes'])
        if row['all_absent'] and self.socket.exists() and self.socket_info() == self.endpoint:
            self.socket.unlink(); row['owned_socket_residue_removed'] = True
        row['socket_absent'] = not self.socket.exists()
        attempt('private-profiles-after', lambda: self.profiles(post_shutdown=row['all_absent']))
        for watched in self.watches.values(): watched.close()
        row['fd_after'] = fd_snapshot(); row['fds_equal'] = row['fd_after'] == self.record['fd_before']
        row['end'] = now()
        row['pass'] = (self.daemon is not None and row['all_absent'] and row['socket_absent']
                       and row['fds_equal'] and not row['errors'] and not row['rescues'] and time.monotonic() <= stop)
        self.checkpoint()


def verify_frozen(frozen):
    require(frozen['phase'] in ('oracle', 'candidate'), 'unknown product phase')
    for path in frozen.get('absent_startup', []):
        require(not Path(path).exists(), 'previously absent startup file appeared: ' + path)
    for name in ('bash', 'zsh', 'sleep', 'tmux', 'rmux', 'rmux_daemon'):
        require(frozen['pins'][name] in frozen['artifacts'], 'binary provenance missing: ' + name)
    for item in frozen['artifacts']:
        require(pin(item['path']) == item, 'frozen artifact changed: ' + item['path'])
    require(frozen['plan'] == json.loads(json.dumps(plan())), 'frozen plan differs')
    require(frozen['environment'] == environment(Path(frozen['root'])), 'environment contaminated')


def execute(frozen_path):
    frozen = json.loads(frozen_path.read_text())
    verify_frozen(frozen)
    expected = None
    if frozen['phase'] == 'candidate':
        gate = json.loads(Path(frozen['oracle_gate']['path']).read_text())
        require(pin(frozen['oracle_gate']['path']) == frozen['oracle_gate'] and gate['status'] == 'PASS', 'oracle gate invalid')
        require(pin(gate['result']['path']) == gate['result'], 'oracle result changed')
        expected = gate['expected']
        require(set(expected) == set(KINDS), 'oracle population incomplete')
    run = Run(frozen, expected)
    write_once(run.root / 'STARTED.json', {'at': now(), 'frozen': pin(frozen_path), 'observer': birth(os.getpid())})
    try:
        run.execute(); run.record['status'] = 'PASS'
    except BaseException as error:
        run.record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=repr(error), traceback=traceback.format_exc())
    finally:
        try: run.cleanup()
        except BaseException as error: run.record.update(cleanup_exception=repr(error), status='UNPROVEN')
        if not run.record.get('cleanup', {}).get('pass'): run.record['status'] = 'UNPROVEN'
        run.record.update(end=now(), seconds=time.monotonic() - run.start, pane_spawn_commands=run.spawn_count,
                          frozen=pin(frozen_path), limits=['Disk path hashes are not process-image hashes.',
                          'Scoped own-child sampling is not exhaustive transient-helper observation.'])
        write_once(run.root / 'RESULT.json', run.record)
    if run.record['status'] == 'PASS' and frozen['phase'] == 'oracle':
        expected = {label: {k: obs['child']['args'][k] for k in ('argv', 'argv_base64')}
                    for label, obs in run.observed.items()}
        write_once(run.root / 'ORACLE-FROZEN.json', {'status': 'PASS', 'at': now(), 'expected': expected,
                   'result': pin(run.root / 'RESULT.json'), 'frozen': pin(frozen_path)})
    print(json.dumps({'status': run.record['status'], 'result': pin(run.root / 'RESULT.json'),
                      'seconds': run.record['seconds']}), flush=True)
    return 0 if run.record['status'] == 'PASS' else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--frozen', type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(execute(args.frozen))
