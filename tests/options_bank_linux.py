#!/usr/bin/env python3
"""Private options adapter: Linux defaults and native Unix per-option consumers."""
import datetime
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import resource
import select
import signal
import socket
import subprocess
import struct
import sys
import tempfile
import time
import termios
import traceback
import tty

# Explicit sibling imports also work for the isolated pane helper (-I).
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def identity(pid):
    if sys.platform == 'darwin':
        from options_bank_macos import identity as observe
        return observe(pid)
    try:
        raw = Path('/proc', str(pid), 'stat').read_text()
        fields = raw.rsplit(')', 1)[1].split()
        return {'pid': pid, 'start_ticks': fields[19], 'ppid': int(fields[1]), 'state': fields[0]}
    except (FileNotFoundError, ProcessLookupError):
        return None


def same(a, b):
    return bool(a and b and all(a[k] == b[k] for k in ('pid', 'start_ticks')))


def children(pid, proc=Path('/proc')):
    if sys.platform == 'darwin' and proc == Path('/proc'):
        from options_bank_macos import children as observe
        return observe(pid)
    result = set()
    for task in (proc / str(pid) / 'task').iterdir():
        try:
            result.update(map(int, (task / 'children').read_text().split()))
        except FileNotFoundError:
            pass
    return sorted(result)


def describe(pid):
    if sys.platform == 'darwin':
        from options_bank_macos import describe as observe
        return observe(pid)
    before = identity(pid)
    base = Path('/proc', str(pid))
    record = {'identity': before, 'exe': os.readlink(base / 'exe'), 'exe_sha256': sha(base / 'exe'),
              'argv': [x.decode() for x in (base / 'cmdline').read_bytes().split(b'\0') if x],
              'cwd': os.readlink(base / 'cwd'), 'fd0': os.readlink(base / 'fd/0')}
    require(same(before, identity(pid)), 'incoherent process identity')
    return record


def fds():
    if sys.platform == 'darwin':
        from options_bank_macos import fds as observe
        return observe()
    result = {}
    for item in Path('/proc/self/fd').iterdir():
        try:
            result[item.name] = os.readlink(item)
        except FileNotFoundError:
            pass
    return result


def open_watch(pid):
    if sys.platform == 'darwin':
        from options_bank_macos import open_watch as retain
        return retain(pid)
    fd = os.pidfd_open(pid)
    poll = select.poll()
    poll.register(fd, select.POLLIN)
    return fd, poll


def close_watch(fd, poll):
    from_poll = type(poll).__module__ == 'options_bank_macos'
    if from_poll:
        poll.close()
    else:
        os.close(fd)


def send_signal(fd, poll, birth, signum):
    if sys.platform == 'darwin':
        from options_bank_macos import send_signal as send
        send(fd, poll, birth, signum)
    else:
        signal.pidfd_send_signal(fd, signum)


def native_os():
    return {'linux': 'linux', 'darwin': 'macos'}[sys.platform]


def exit_evidence(poll):
    return ({'exit_method': 'kqueue-NOTE_EXIT', 'native_exit': True}
            if type(poll).__module__ == 'options_bank_macos' else {'pidfd_exit': True})


def valid_tty(path, os_name):
    import re
    pattern = r'/dev/ttys[0-9a-f]+' if os_name == 'macos' else r'/dev/pts/[0-9]+'
    return isinstance(path, str) and re.fullmatch(pattern, path) is not None


def helper(root, token):
    root = Path(root)
    raw_config = root / 'raw-input.json'
    raw_input = (root / 'backspace.conf').is_file() or raw_config.exists()
    if raw_input:
        tty.setraw(0)
    if raw_config.exists():
        settings = load(raw_config)
        require(settings['token'] == token, 'raw input observer token')
        os.write(1, bytes.fromhex(settings['request_hex']))
    save(root / 'helper-born.json', {'token': token, 'process': describe(os.getpid()), 'at': now()})
    geometry = (root / 'geometry-enabled').exists()
    inputs = 0
    while not (root / 'helper-stop').exists():
        if geometry:
            from options_geometry_linux import helper_sample
            helper_sample(root, token)
        if not raw_input:
            time.sleep(.02)
            continue
        if select.select([0], [], [], .02)[0]:
            data = os.read(0, 65536)
            require(bool(data), 'pane input closed before helper stop')
            if raw_config.exists() and data == b'\xff':
                os.write(1, bytes.fromhex(settings['request_hex']))
            os.write(1, data.hex().encode() + b'\r\n')
            save(root / f'input-{inputs}.json', {'hex': data.hex(), 'at': now()})
            inputs += 1
    require((root / 'helper-stop').read_text() == token, 'helper stop token')
    save(root / 'helper-ended.json', {'token': token, 'process': describe(os.getpid()), 'planned_exit_code': 0, 'not_wait_status': True, 'at': now()})


class Run:

    def __init__(self, contract, phase, root):
        self.contract, self.phase, self.root = contract, phase, root
        self.binary = contract['native_binaries'][phase]['path']
        self.socket = root / 'socket'
        self.start = time.monotonic()
        self.deadline = self.start + 255
        self.handles, self.children = {}, {}
        self.master = None
        self.pty_bytes = bytearray()
        self.record = {'phase': phase, 'native_os': native_os(), 'source': contract['native_source'],
                       'binary': contract['native_binaries'][phase], 'recipe_sha256': contract['default_recipe_sha256'],
                       'driver': pin(__file__), 'started': now(), 'commands': [], 'processes': {}, 'closure': {'status': 'FAIL'}}
        self.per_option = contract['default_recipe']['kind'] == 'per-option-v1'
        self.registry_profile = self.per_option or contract['default_recipe']['kind'] in ('registry-global-default-v1', 'registry-global-default-v2')
        self.env = {'HOME': str(root / 'home'), 'XDG_CONFIG_HOME': str(root / 'xdg'),
                    'PATH': '/usr/bin:/bin', 'SHELL': '/bin/bash', 'TERM': 'xterm',
                    'LANG': 'en_US.UTF-8' if native_os() == 'macos' else 'C.UTF-8',
                    'RMUX_DISABLE_TMUX_SHIM': '1', 'RMUX_DISABLE_TMUX_FALLBACK': '1'}
        if self.registry_profile:
            self.env.update(contract['default_recipe']['environment'])
            self.record['environment'] = dict(self.env)

    def remaining(self):
        require(self.deadline > time.monotonic(), 'lot deadline')
        return self.deadline - time.monotonic()

    def drain(self):
        streams = [(self.master, self.pty_bytes)] if self.master is not None else []
        streams += [(value[0], value[1]) for value in getattr(self, 'auxiliary_clients', {}).values()]
        for master, output in streams:
            while select.select([master], [], [], 0)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                output.extend(data)
                require(len(output) < 1048576, 'PTY log cap')

    def hold(self, label, pid, expected_hash=None):
        birth = identity(pid)
        require(birth is not None, 'process disappeared before retention')
        fd, poll = open_watch(pid)
        self.handles[label] = (fd, poll, birth)
        require(same(birth, identity(pid)) and not poll.poll(0), 'retained birth changed/exited')
        observed = describe(pid)
        self.record['processes'][label] = observed
        if expected_hash is not None:
            require(observed['exe_sha256'] == expected_hash, 'owned executable mismatch: ' + label)

    def signal(self, label, signum):
        fd, poll, birth = self.handles[label]
        current = identity(birth['pid'])
        require(same(birth, current) and not poll.poll(0), 'stale signal target')
        send_signal(fd, poll, birth, signum)
        self.record.setdefault('recovery_signals', []).append({'label': label, 'birth': birth, 'signal': signum, 'at': now()})

    def command(self, role, args, check=True, source_text=None, client_observer=None):
        self.remaining()
        require(len(self.record['commands']) < (1024 if self.registry_profile else 128), 'per product observation cap')
        argv = [self.binary, '-N', '-S', str(self.socket), *args]
        begin = now()
        p = subprocess.Popen(argv, cwd=self.root, env=self.env,
                             stdin=subprocess.PIPE if source_text is not None else subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        birth = identity(p.pid)
        fd, poll = open_watch(p.pid)
        screen_observation = {}
        try:
            require(same(birth, identity(p.pid)), 'client birth changed')
            if client_observer:
                try:
                    from options_bank_linux_screen import capture, retain_consumer, inject, prompt_ready
                    if 'before_input' in client_observer:
                        prompt_ready(self, args)
                        inject(self, client_observer['before_input'])
                    capture(self, client_observer, screen_observation)
                    if 'consumer_argv' in client_observer:
                        retain_consumer(self, client_observer)
                    os.write(self.master, bytes.fromhex(client_observer['dismiss_input']))
                except Exception:
                    if p.poll() is None:
                        send_signal(fd, poll, birth, signal.SIGKILL)
                    out, err = p.communicate(timeout=2)
                    self.record.setdefault('failed_commands', []).append(dict(
                        role=role, argv=argv, pid=p.pid, birth=birth, started=begin, ended=now(),
                        exit_code=p.returncode, after=identity(p.pid), wait_method='Popen.communicate',
                        stdout=out.decode(), stderr=err.decode(),
                        stdout_sha256=hashlib.sha256(out).hexdigest(), stderr_sha256=hashlib.sha256(err).hexdigest()))
                    raise
            try:
                out, err = p.communicate(input=source_text.encode() if source_text is not None else None,
                                         timeout=min(10, self.remaining()))
            except subprocess.TimeoutExpired:
                send_signal(fd, poll, birth, signal.SIGKILL)
                p.communicate(timeout=2)
                raise RuntimeError('owned client command timed out: ' + role)
        finally:
            close_watch(fd, poll)
        require(len(out) + len(err) < 131072, 'command output cap')
        out, err = out.decode(), err.decode()
        item = {'role': role, 'argv': argv, 'pid': p.pid, 'birth': birth, 'started': begin, 'ended': now(),
                'exit_code': p.returncode, 'after': identity(p.pid), 'wait_method': 'Popen.communicate', 'stdout': out, 'stderr': err,
                'stdout_sha256': hashlib.sha256(out.encode()).hexdigest(),
                'stderr_sha256': hashlib.sha256(err.encode()).hexdigest()}
        self.record['commands'].append(item)
        item.update(screen_observation)
        if source_text is not None:
            item['source_text'] = source_text
        self.drain()
        if check:
            require(p.returncode == 0, f'command {role} exit {p.returncode}: {err}')
        return out, len(self.record['commands']) - 1

    def wait(self, predicate, seconds=5):
        end = min(time.monotonic() + seconds, self.deadline)
        while time.monotonic() < end:
            self.drain()
            if predicate():
                return
            time.sleep(.025)
        require(False, 'bounded readiness/exit wait failed')

    def start_server(self):
        from options_bank_server_effects import CONFIGURATIONS
        from options_bank_linux_raw import prepare as prepare_raw
        for name in ('home', 'xdg'):
            (self.root / name).mkdir()
        conf = self.root / 'startup.conf'
        conf.write_text(self.contract['default_recipe']['startup'])
        self.record['startup'] = pin(conf)
        if self.per_option:
            prepare_raw(self)
        if self.per_option and 'backspace' in self.contract['default_recipe']['names']:
            config = self.root / 'backspace.conf'
            config.write_text('set-option -s backspace C-h\n')
            self.record['configuration'] = pin(config)
        self.record['server_configurations'] = {}
        for name, content in CONFIGURATIONS.items():
            if self.per_option and name in self.contract['default_recipe']['names']:
                config = self.root / (name + '.conf')
                config.write_text(content)
                self.record['server_configurations'][name] = pin(config)
        if self.per_option and set(self.contract['default_recipe']['names']) & set(GEOMETRY_OPTIONS):
            from options_geometry_linux import prepare
            prepare(self)
        self.daemon_log = (self.root / 'daemon.log').open('xb')
        argv = [self.binary, '-D', '-S', str(self.socket), '-f', str(conf)]
        daemon = subprocess.Popen(argv, cwd=self.root, env=self.env, stdin=subprocess.DEVNULL,
                                  stdout=self.daemon_log, stderr=subprocess.STDOUT,
                                  preexec_fn=lambda: resource.setrlimit(resource.RLIMIT_FSIZE, (1048576, 1048576)))
        self.children['daemon'] = daemon
        # Popen returns after exec; retain the direct foreground server immediately.
        self.hold('daemon', daemon.pid, self.record['binary']['sha256'])
        self.wait(lambda: self.socket.exists())
        self.record['endpoint'] = {'path': str(self.socket), 'device': self.socket.stat().st_dev,
                                   'inode': self.socket.stat().st_ino}
        self.record['daemon_argv'] = argv

    def start_pane(self):
        args = [self.contract['python']['path'], '-I', str(Path(__file__).resolve()),
                'helper', str(self.root), self.contract['default_recipe']['token']]
        self.command('new-session', ['new-session', '-d', '-s', 'bank', '-n', 'bank',
                                    '-x', '80', '-y', '24', '-c', str(self.root), *args])
        self.wait(lambda: (self.root / 'helper-born.json').exists())
        born = load(self.root / 'helper-born.json')
        pid = born['process']['identity']['pid']
        self.hold('helper', pid, self.contract['python']['sha256'])
        proc = self.record['processes']['helper']
        require(proc['argv'] == args and born['token'] == self.contract['default_recipe']['token'], 'helper identity/token')
        require(proc['identity']['ppid'] == self.children['daemon'].pid, 'helper is not direct own daemon child')
        raw, _ = self.command('public-inventory', ['list-panes', '-a', '-F',
                                 '#{pid}|#{socket_path}|#{session_name}|#{window_id}|#{pane_id}|#{pane_pid}'])
        rows = raw.splitlines()
        require(len(rows) == 1, 'one owned pane required')
        fields = rows[0].split('|')
        require(fields[0] == str(self.children['daemon'].pid) and fields[1] == str(self.socket)
                and fields[2] == 'bank' and fields[5] == str(pid), 'public/retained identity mismatch')
        self.record['public'] = fields

    def cleanup(self):
        cleanup_started = now()
        cleanup_start = time.monotonic()
        self.deadline = min(self.start + 300, time.monotonic() + 45)
        errors = []
        # Inspect only the owned foreground server's immediate children. An
        # unexpected child is retained as unresolved ownership, never ignored.
        if 'daemon' in self.handles and not self.handles['daemon'][1].poll(0):
            parent = self.children['daemon'].pid
            known = {handle[2]['pid'] for handle in self.handles.values()}
            for pid in children(parent):
                if pid in known:
                    continue
                birth = identity(pid)
                try:
                    fd, poll = open_watch(pid)
                except ProcessLookupError:
                    self.record.setdefault('closed_unregistered_children', []).append({'birth': birth, 'after': identity(pid)})
                    continue
                try:
                    if poll.poll(0):
                        self.record.setdefault('closed_unregistered_children', []).append({'birth': birth, **exit_evidence(poll), 'after': identity(pid)})
                        continue
                    observed = describe(pid)
                except (FileNotFoundError, ProcessLookupError):
                    require(bool(poll.poll(0)), 'unregistered child lost its image while alive')
                    self.record.setdefault('closed_unregistered_children', []).append({'birth': birth, **exit_evidence(poll), 'after': identity(pid)})
                    continue
                finally:
                    close_watch(fd, poll)
                self.record.setdefault('unregistered_children', []).append(observed)
                expected = [self.contract['python']['path'], '-I', str(Path(__file__).resolve()),
                            'helper', str(self.root), self.contract['default_recipe']['token']]
                if observed['argv'] == expected and 'helper' not in self.handles:
                    self.hold('helper', pid, self.contract['python']['sha256'])
                else:
                    errors.append('unqualified owned descendant: ' + str(pid))
        if 'helper' in self.handles:
            (self.root / 'helper-stop').write_text(self.contract['default_recipe']['token'])
            try:
                self.wait(lambda: self.handles['helper'][1].poll(0), 5)
            except Exception as error:
                errors.append(str(error))
        try:
            if 'daemon' in self.handles and (getattr(self, 'terminal_exit', False) or not self.handles['daemon'][1].poll(0) and self.socket.exists()):
                self.command('closure-kill-server', ['kill-server'], check=False)
        except Exception as error:
            errors.append(str(error))
        exits = {}
        for label in ('attach', *[key for key in self.handles if key.startswith('attach-')], 'helper', *[key for key in self.handles if key.startswith('consumer-')], 'daemon'):
            if label not in self.handles:
                continue
            fd, poll, birth = self.handles[label]
            try:
                if not poll.poll(1500):
                    self.signal(label, signal.SIGTERM)
                    require(poll.poll(2000), 'owned process did not close: ' + label)
                status = None
                if label in self.children:
                    status = self.children[label].wait(timeout=1)
                else:
                    try:
                        waited_pid, raw_status = os.waitpid(birth['pid'], os.WNOHANG)
                        status = raw_status if waited_pid == birth['pid'] else None
                    except ChildProcessError:
                        pass
                self.wait(lambda: not same(birth, identity(birth['pid'])))
                after = identity(birth['pid'])
                require(not same(birth, after), 'owned birth remains: ' + label)
                exits[label] = {'birth': birth, **exit_evidence(poll), 'wait_status': status,
                                'after': after}
            except Exception as error:
                errors.append(label + ': ' + str(error))
                exits[label] = {'birth': birth, 'owner': 'L6', 'unclosed': True}
            finally:
                close_watch(fd, poll)
        self.drain()
        if self.master is not None:
            os.close(self.master)
            self.master = None
        for label, (master, output) in getattr(self, 'auxiliary_clients', {}).items():
            os.close(master)
            path = self.root / (label + '.pty')
            path.write_bytes(output)
            self.record.setdefault('auxiliary_streams', {})[label] = pin(path)
        self.auxiliary_clients = {}
        if hasattr(self, 'daemon_log'):
            self.daemon_log.close()
        probe = socket.socket(socket.AF_UNIX)
        try:
            code = probe.connect_ex(str(self.socket))
        finally:
            probe.close()
        require(code in (errno.ENOENT, errno.ECONNREFUSED), 'private endpoint remains live')
        self.record['closure'] = {'status': 'PASS' if not errors else 'FAIL', 'errors': errors,
                                  'exits': exits, 'endpoint_connect_errno': code, 'started': cleanup_started, 'elapsed_seconds': time.monotonic()-cleanup_start, 'ended': now()}
        if hasattr(self, 'closure_files'):
            from options_bank_linux_history import capture_files
            self.record['closure_files'] = capture_files(self, self.closure_files, 'closure-file')

    def attach(self, label='attach'):
        if self.master is not None:
            self.drain()
            os.close(self.master)
        self.master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        def tty_session():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        argv = [self.binary, '-N', '-S', str(self.socket), 'attach-session', '-t', 'bank']
        try:
            process = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave,
                                       cwd=self.root, env=self.env, preexec_fn=tty_session)
        finally:
            os.close(slave)
        self.children[label] = process
        self.hold(label, process.pid, self.record['binary']['sha256'])
        self.current_attachment = label
        self.record['attach_argv'] = argv
        before = len(self.pty_bytes)
        self.wait(lambda: len(self.pty_bytes) > before)


    def execute(self):
        from options_bank import default_plan
        from options_bank_linux_screen import requires_client, capture, retain_consumer, inject, event
        self.start_server()
        self.start_pane()
        plan, cells = (per_option_plan(load(self.contract['registry']['path']), self.contract,
                                     self.contract['default_recipe']['names']) if self.per_option
                       else default_plan(self.contract))
        if self.per_option and ('message-limit' in cells or requires_client(plan)):
            self.attach()
        indices = []
        for command in plan:
            if 'add_client' in command:
                from options_bank_linux_screen import add_client
                add_client(self, command['add_client'])
            if 'closure_files' in command:
                self.closure_files = command['closure_files']
            if command.get('raw_mark'):
                time.sleep(.15)
                self.raw_mark = len(list(self.root.glob('input-*.json')))
            if 'reattach' in command:
                self.attach('attach-' + command['reattach'])
            if command.get('client_mark'):
                self.drain()
                self.client_mark = len(self.pty_bytes)
            if 'client_input' in command:
                inject(self, command['client_input'])
            if 'wait_file' in command:
                path = self.root / command['wait_file']
                self.wait(lambda: path.exists() and path.read_bytes() == command['expected_file_bytes'].encode())
            if 'wait_pane_exit' in command:
                from options_bank_linux_windows import await_pane_exit
                await_pane_exit(self, command)
            if command.get('settle_terminal'):
                time.sleep(.15)
            if 'input_index' in command:
                self.wait(lambda: (self.root / f"input-{command['input_index']}.json").exists())
            if 'expected_pty_size' in command: time.sleep(.3)
            if command.get('expected_daemon_alive') is False:
                self.terminal_exit = True
                self.wait(lambda: bool(self.handles['daemon'][1].poll(0)))
            args = [self.record.get('attached_client', '').split('|')[-1] if arg == '$client'
                    else self.record['processes'][self.current_attachment]['fd0'] if arg == '$attached' else arg
                    for arg in command['argv']]
            out, index = self.command(command['role'], args, check=not command.get('error', False),
                                      source_text=command.get('source_text'),
                                      client_observer=command if 'dismiss_input' in command else None)
            if any(key in command for key in ('client_screen', 'client_control')) and 'dismiss_input' not in command:
                capture(self, command, self.record['commands'][index])
            if 'client_input' in command:
                self.record['commands'][index]['client_input'] = command['client_input']
            if '$attached' in command['argv']:
                self.record['commands'][index]['attached_label'] = self.current_attachment
            if 'client_event' in command:
                event(self, command, self.record['commands'][index])
            if 'expected_daemon_alive' in command:
                alive = not bool(self.handles['daemon'][1].poll(0))
                self.record['commands'][index]['daemon_alive'] = alive
                require(alive == command['expected_daemon_alive'], 'native daemon lifetime consumer')
            if command.get('raw_observation'):
                from options_bank_linux_raw import observe as observe_raw
                observe_raw(self, self.record['commands'][index])
            if 'files_expected' in command:
                from options_bank_linux_history import capture_files
                self.record['commands'][index]['file_observations'] = capture_files(self, command['files_expected'], 'file-' + str(index))
            if 'wait_file' in command:
                self.record['commands'][index]['file_observer'] = pin(path)
            if command['role'] == 'inventory-client':
                self.record['attached_client'] = out.strip()
            if 'expected_pty_size' in command:
                from options_geometry_linux import sample
                self.record['commands'][index]['pty_size'] = sample(self)
            indices.append(index)
            if self.per_option and 'dismiss_input' not in command and (command['role'].endswith(':create-consumer') or 'consumer_argv' in command):
                retain_consumer(self, command)
            if 'consumer_effect' in command:
                process = self.record['processes']['consumer-' + command['consumer_effect']]
                require(self.record['commands'][index]['stdout'] == str(process['identity']['pid']) + '\n',
                        'public launch consumer identity: ' + command['role'])
        self.record['cells'] = {
            cell: [indices[index] for index in spec['indices']]
            for cell, spec in cells.items()
        }
        if self.per_option:
            self.record['option_specs'] = cells
            self.record['observations'] = per_option_observations(
                [self.record['commands'][index] for index in indices], plan, cells, self.record['native_os'])



from options_geometry_linux import OPTIONS as GEOMETRY_OPTIONS


PER_OPTION_EFFECTS = {
    'base-index': ('-g', ['list-windows', '-t', 'bank', '-F', '#{window_name}|#{window_index}'], 'effect|7\n'),
    'pane-base-index': ('-gw', ['list-panes', '-t', 'bank:effect', '-F', '#{pane_index}'], '7\n'),
    'history-limit': ('-g', ['list-panes', '-t', 'bank:effect', '-F', '#{history_limit}'], '7\n'),
}


def launch_option_plan(plan, name, entry):
    """Observe the process launched by a session's command and shell defaults."""
    begin, obligations = len(plan), {}
    command = name == 'default-command'
    first, second = ('exec /bin/sleep 239', 'exec /bin/sleep 238') if command else ('/bin/bash', '/bin/sh')
    probe = 'if [ -n "${BASH_VERSION-}" ]; then exec /bin/sleep 239; else exec /bin/sleep 238; fi'
    def add(role, argv, expected=None, **extra):
        index = len(plan)
        row = dict(role=name + ':' + role, argv=argv, error=False, **extra)
        if expected is not None:
            row['expected_stdout'] = expected
        plan.append(row)
        return index
    def set_value(role, value, flags='-g'):
        return add(role, ['set-option', *([flags] if flags else []), *(['-t', 'bank'] if flags != '-g' else []), name, value])
    def read(role, value=None, flags='-gv'):
        return add(role, ['show-options', flags, *(['-t', 'bank'] if flags != '-gv' else []), name],
                   None if value is None else value + '\n')
    def effect(role, value, split=False):
        label = name + '-' + role
        argv = ['-bash'] if command and not value else ['/bin/sleep', '239' if value == first else '238']
        exe = '/bin/bash' if argv == ['-bash'] else '/bin/sleep'
        launch = ['split-window', '-d', '-t', 'bank:bank'] if split else ['new-window', '-d', '-t', 'bank:', '-n', 'effect']
        if not command:
            launch.append(probe)
        target = 'bank:bank.1' if split else 'bank:effect'
        create = add('create-consumer' if role == 'effect' else role + '-create', launch,
                     consumer_argv=argv, consumer_exe=exe, consumer_label=label)
        observe = add(role, ['display-message', '-p', '-t', target, '#{pane_pid}'], consumer_effect=label)
        close = add(role + '-close', ['kill-pane' if split else 'kill-window', '-t', target])
        return [create, observe, close]
    obligations['default'] = [read('default')]
    add('setup-shell', ['set-option', '-g', 'default-shell', '/bin/bash'])
    obligations['replace'] = [set_value('replace', first), read('value', first),
                              add('named', ['show-options', '-g', name], name + ' "' + first + '"\n' if command else name + ' ' + first + '\n')]
    obligations['effect'] = effect('effect', first)
    obligations['scope'] = [set_value('local', second, ''), read('local-value', second, '-v'), read('global-value', first)]
    obligations['scope'] += effect('local-effect', second)
    invalid = add('invalid-target', ['set-option', '-t', 'missing-launch-target', name, first])
    plan[invalid]['error'] = True
    obligations['scope'].append(invalid)
    only = add('only-existing', ['set-option', '-o', '-t', 'bank', name, first])
    plan[only]['error'] = True
    obligations['only-if-unset'] = [only, read('only-existing-value', second, '-v')]
    obligations['quiet'] = [add('only-existing-quiet', ['set-option', '-oq', '-t', 'bank', name, first]),
                            read('quiet-value', second, '-v')]
    obligations['unset'] = [add('unset', ['set-option', '-u', '-t', 'bank', name]), read('inherited', first, '-Av')]
    obligations['unset'] += effect('inherited-effect', first)
    obligations['only-if-unset'] += [add('only-absent', ['set-option', '-o', '-t', 'bank', name, second]), read('only-absent-value', second, '-v')]
    add('clear-local', ['set-option', '-u', '-t', 'bank', name])
    obligations['format'] = [add('format', ['set-option', '-gF', name, '#{default-shell}' if not command else 'exec /bin/sleep #{e|+|:230,9}']), read('formatted-value', first)]
    obligations['append'] = [set_value('append-base', '' if command else '/usr/bin/ssh'),
                             add('append', ['set-option', '-ga', name, first if command else '-keygen']),
                             read('appended-value', first if command else '/usr/bin/ssh-keygen')]
    obligations['unset'] += [add('restore-default', ['set-option', '-gu', name]), read('reset-value', '' if command else '/bin/sh')]
    obligations['unset'] += effect('reset-effect', '' if command else second)
    source_text = 'set-option -g ' + name + " '" + second + "'\n"
    obligations['source'] = [add('source', ['source-file', '-'], source_text=source_text), read('source-value', second)]
    obligations['source'] += effect('source-effect', second, split=True)
    add('final-reset', ['set-option', '-gu', name])
    return {'id': entry['id'], 'indices': list(range(begin, len(plan))), 'obligations': obligations, 'status': 'REQUIRED'}


def per_option_plan(registry, contract, names):
    """A selected recipe never stands in for the rest of the frozen registry."""
    from options_bank import inventory_gate, REGISTRY_STARTUP_V1
    from options_bank_server_effects import CONFIGURATIONS, server_option_plan
    from options_bank_linux_terminal import TERMINAL_OPTIONS, terminal_plan
    from options_bank_linux_copy import OPTIONS as COPY_OPTIONS, plan as copy_plan
    from options_bank_linux_windows import OPTIONS as WINDOW_OPTIONS, plan as windows_plan
    from options_bank_linux_title import OPTIONS as TITLE_OPTIONS, plan as title_plan
    from options_bank_linux_status import OPTIONS as STATUS_OPTIONS, plan as status_plan
    from options_bank_linux_ui import OPTIONS as UI_OPTIONS, plan as ui_plan
    from options_bank_linux_commands import OPTIONS as COMMAND_OPTIONS, plan as commands_plan
    from options_bank_linux_layout import OPTIONS as LAYOUT_OPTIONS, plan as layout_plan
    from options_bank_linux_keys import OPTIONS as KEY_OPTIONS, plan as keys_plan
    from options_bank_linux_alerts import OPTIONS as ALERT_OPTIONS, plan as alerts_plan
    from options_bank_linux_controls import OPTIONS as CONTROL_OPTIONS, plan as controls_plan
    from options_bank_linux_lifetime import OPTIONS as LIFETIME_OPTIONS, plan as lifetime_plan
    from options_bank_linux_timers import OPTIONS as TIMER_OPTIONS, plan as timers_plan
    from options_bank_linux_raw import OPTIONS as RAW_OPTIONS, plan as raw_plan
    from options_bank_linux_applications import OPTIONS as APPLICATION_OPTIONS, plan as applications_plan
    from options_bank_linux_history import OPTIONS as HISTORY_OPTIONS, plan as history_plan
    inventory_gate(registry, contract)
    if contract.get('default_recipe', {}).get('kind') == 'per-option-v1':
        recipe = contract['default_recipe']
        require(recipe['startup'] == REGISTRY_STARTUP_V1 and recipe['environment'] == {'SHELL': '/bin/bash'}
                and recipe['names'] == names, 'per-option setup/selection')
    require(isinstance(names, list) and names and len(names) == len(set(names)), 'option selection')
    entries = {entry['name']: entry for entry in registry['options']}
    require(set(names) <= set(entries), 'unknown/renamed option')
    plan = [{'role': 'inventory-' + flags, 'argv': ['show-options', flags], 'error': False}
            for flags in ('-s', '-g', '-gw', '-pA')]
    cells = {}
    for name in names:
        if name in HISTORY_OPTIONS:
            cells[name] = history_plan(plan, entries[name], contract)
            continue
        if name in APPLICATION_OPTIONS:
            cells[name] = applications_plan(plan, entries[name])
            continue
        if name in RAW_OPTIONS:
            cells[name] = raw_plan(plan, entries[name])
            continue
        if name in TIMER_OPTIONS:
            cells[name] = timers_plan(plan, entries[name])
            continue
        if name in LIFETIME_OPTIONS:
            cells[name] = lifetime_plan(plan, entries[name])
            continue
        if name in CONTROL_OPTIONS:
            cells[name] = controls_plan(plan, entries[name])
            continue
        if name in ALERT_OPTIONS:
            cells[name] = alerts_plan(plan, entries[name])
            continue
        if name in KEY_OPTIONS:
            cells[name] = keys_plan(plan, entries[name])
            continue
        if name in LAYOUT_OPTIONS:
            cells[name] = layout_plan(plan, entries[name])
            continue
        if name in COMMAND_OPTIONS:
            cells[name] = commands_plan(plan, entries[name])
            continue
        if name in UI_OPTIONS:
            cells[name] = ui_plan(plan, entries[name])
            continue
        if name in STATUS_OPTIONS:
            cells[name] = status_plan(plan, entries[name])
            continue
        if name in TITLE_OPTIONS:
            cells[name] = title_plan(plan, entries[name])
            continue
        if name in CONFIGURATIONS:
            cells[name] = server_option_plan(plan, entries[name])
            continue
        if name in GEOMETRY_OPTIONS:
            from options_geometry_linux import plan as geometry_plan
            cells[name] = geometry_plan(plan, entries[name])
            continue
        if name == 'backspace':
            cells[name] = backspace_plan(plan, entries[name])
            continue
        if name == 'codepoint-widths':
            from options_bank_linux_terminal import codepoint_widths_plan
            cells[name] = codepoint_widths_plan(plan, entries[name])
            continue
        if name in TERMINAL_OPTIONS:
            cells[name] = terminal_plan(plan, entries[name])
            continue
        if name in COPY_OPTIONS:
            cells[name] = copy_plan(plan, entries[name])
            continue
        if name in WINDOW_OPTIONS:
            cells[name] = windows_plan(plan, entries[name])
            continue
        if name in ('default-command', 'default-shell'):
            cells[name] = launch_option_plan(plan, name, entries[name])
            continue
        if name == 'message-limit':
            cells[name] = message_limit_plan(plan, entries[name], contract)
            continue
        if name not in PER_OPTION_EFFECTS:
            cells[name] = {'id': entries[name]['id'], 'indices': [], 'obligations': {},
                           'status': 'FAIL_MISSING'}
            continue
        flags, effect, expected = PER_OPTION_EFFECTS[name]
        local = '-w' if flags == '-gw' else ''
        target = ['-t', 'bank:bank' if local else 'bank']
        begin = len(plan)
        obligations = {}
        def add(role, argv, expected=None, error=False):
            index = len(plan)
            row = {'role': name + ':' + role, 'argv': argv, 'error': error}
            if expected is not None:
                row['expected_stdout'] = expected
            plan.append(row)
            return index
        def read(role, scope, value=None):
            return add(role, ['show-options', scope + 'v' if scope else '-v',
                             *(target if scope != flags else []), name], value)
        obligations['default'] = [read('default', flags)]
        obligations['replace'] = [add('replace', ['set-option', flags, name, '7']),
                                  read('value', flags, '7\n'),
                                  add('named', ['show-options', flags, name], name + ' 7\n')]
        obligations['scope'] = [add('local', ['set-option', *( [local] if local else []), *target, name, '3']),
                                read('local-value', local, '3\n'), read('global-value', flags, '7\n'),
                                add('invalid-target', ['set-option', *([local] if local else []), '-t', 's3083-missing-target', name, '7'], error=True)]
        obligations['only-if-unset'] = [add('only-existing', ['set-option', '-o', *([local] if local else []), *target, name, '9'], error=True),
                                       read('only-existing-value', local, '3\n')]
        obligations['unset'] = [add('unset', ['set-option', '-u', *([local] if local else []), *target, name]),
                                add('inherited', ['show-options', '-wAv' if local else '-Av', *target, name], '7\n')]
        obligations['only-if-unset'] += [add('only-absent', ['set-option', '-o', *([local] if local else []), *target, name, '5']),
                                        read('only-absent-value', local, '5\n')]
        obligations['quiet'] = [add('only-existing-quiet', ['set-option', '-o', '-q', *([local] if local else []), *target, name, '9']),
                                read('only-existing-quiet-value', local, '5\n'),
                                add('quiet', ['set-option', '-q', *([local] if local else []), *target, name, '6']),
                                read('quiet-value', local, '6\n')]
        obligations['format'] = [add('format', ['set-option', flags, '-F', name, '#{e|+|:3,4}']),
                                 read('formatted-value', flags, '7\n')]
        add('clear-local', ['set-option', '-u', *([local] if local else []), *target, name])
        obligations['effect'] = [add('create-consumer', ['new-window', '-d', '-t', 'bank:', '-n', 'effect', '/bin/sleep', '240']),
                                 add('effect', effect)]
        if name == 'base-index':
            plan[-1]['expected_lines'] = ['bank|0', 'effect|7']
        else:
            plan[-1]['expected_stdout'] = expected
        add('close-consumer', ['kill-window', '-t', 'bank:effect'])
        add('restore-default', ['set-option', '-u', flags, name])
        cells[name] = {'id': entries[name]['id'], 'indices': list(range(begin, len(plan))),
                       'obligations': obligations, 'status': 'REQUIRED'}
    if contract.get('native_os', 'linux') == 'macos':
        from options_bank_macos import consumer_paths
        consumer_paths(plan, contract)
    return plan, cells


def backspace_plan(plan, entry):
    """Server key mutation consumed by send-keys into the owned raw PTY."""
    begin, obligations, lines = len(plan), {}, []
    def add(role, argv, expected=None, error=False):
        index = len(plan)
        row = {'role': 'backspace:' + role, 'argv': argv, 'error': error}
        if expected is not None:
            row['expected_stdout'] = expected
        plan.append(row)
        return index
    def read(role, value):
        return add(role, ['show-options', '-sv', 'backspace'], value + '\n')
    def effect(role, value):
        sent = add(role + '-send', ['send-keys', '-t', 'bank:bank', 'BSpace'])
        lines.append(value)
        observed = add(role, ['capture-pane', '-p', '-t', 'bank:bank'],
                       '\n'.join(lines + [''] * (24-len(lines))) + '\n')
        plan[-1]['input_index'] = len(lines)-1
        return [sent, observed]
    obligations['default'] = [read('default', 'C-?')]
    obligations['effect'] = effect('default-effect', '7f')
    obligations['replace'] = [add('replace', ['set-option', '-s', 'backspace', 'C-h']),
                              read('value', 'C-h'),
                              add('named', ['show-options', '-s', 'backspace'], 'backspace C-h\n')]
    obligations['effect'] += effect('effect', '08')
    obligations['scope'] = []
    # tmux infers the server scope by name, including with -g/-w/-p.
    for flag, value in (('-g', 'C-?'), ('-w', 'C-h'), ('-p', 'C-?')):
        obligations['scope'] += [add('inferred-scope' + flag, ['set-option', flag, 'backspace', value]),
                                 read('inferred-value' + flag, value)]
    obligations['effect'] += effect('scope-effect', '7f')
    obligations['only-if-unset'] = [add('only-existing', ['set-option', '-so', 'backspace', 'C-h'], error=True),
                                   read('only-existing-value', 'C-?')]
    obligations['quiet'] = [add('only-existing-quiet', ['set-option', '-soq', 'backspace', 'C-h']),
                            read('quiet-value', 'C-?')]
    obligations['format'] = [add('format', ['set-option', '-sF', 'backspace', '#{?session_name,C-h,C-?}']),
                             read('formatted-value', 'C-h')]
    obligations['unset'] = [add('unset', ['set-option', '-su', 'backspace']), read('reset-default', 'C-?')]
    obligations['effect'] += effect('reset-effect', '7f')
    obligations['sibling'] = [add('window-command', ['set-window-option', '-g', 'backspace', 'C-h']),
                              read('window-command-value', 'C-h')]
    obligations['effect'] += effect('window-command-effect', '08')
    obligations['sibling'] += [add('append-key', ['set-option', '-sa', 'backspace', 'C-?']),
                              read('append-key-value', 'C-?'),
                              add('invalid-key', ['set-option', '-s', 'backspace', 'invalid-key'], error=True),
                              read('invalid-key-value', 'C-?')]
    obligations['effect'] += effect('sibling-effect', '7f')
    obligations['configuration'] = [add('configuration', ['source-file', 'backspace.conf']),
                                    read('configuration-value', 'C-h')]
    obligations['effect'] += effect('configuration-effect', '08')
    add('restore-default', ['set-option', '-su', 'backspace'])
    read('restored-default', 'C-?')
    add('create-consumer', ['new-window', '-d', '-t', 'bank:', '-n', 'effect', '/bin/sleep', '240'])
    add('close-consumer', ['kill-window', '-t', 'bank:effect'])
    return {'id': entry['id'], 'indices': list(range(begin, len(plan))),
            'obligations': obligations, 'status': 'REQUIRED'}


def message_limit_plan(plan, entry, contract):
    from options_bank import RECEIPT_BINDING_V1
    require(contract['receipt_binding'] == RECEIPT_BINDING_V1
            and contract['recipe']['option_id'] == entry['id'], 'message-limit recipe binding')
    begin = len(plan)
    def add(role, argv, expected=None):
        index = len(plan)
        row = {'role': 'message-limit:' + role, 'argv': argv, 'error': False}
        if expected is not None:
            row['expected_stdout'] = expected
        plan.append(row)
        return index
    default = add('default', ['show-options', '-sv', 'message-limit'])
    add('create-consumer', ['new-window', '-d', '-t', 'bank:', '-n', 'effect', '/bin/sleep', '240'])
    plan.append({'role': 'inventory-client', 'argv': ['list-clients', '-F', '#{client_pid}|#{client_tty}'], 'error': False})
    add('reset-limit', ['set-option', '-s', 'message-limit', '1000'])
    for operation in contract['receipt_binding']['operations']:
        argv = [entry['name'] if arg == '$option' else contract['recipe']['markers'].get(arg[1:], arg)
                for arg in operation['argv']]
        add(operation['role'], argv, '1\n' if operation['role'] == 'value' else None)
    after = len(plan)-1
    add('append', ['display-message', '-c', '$client', contract['recipe']['markers']['append']])
    appended = add('append-after', ['show-messages'])
    scope = []
    for flag in ('-g', '-w', '-p'):
        scope += [add('inferred-' + flag, ['set-option', flag, 'message-limit', '1']),
                  add('inferred-value-' + flag, ['show-options', '-sv', 'message-limit'], '1\n')]
    only_quiet = [add('only-existing-quiet', ['set-option', '-soq', 'message-limit', '2']),
                  add('quiet-value', ['show-options', '-sv', 'message-limit'], '1\n')]
    formatted = [add('format', ['set-option', '-sF', 'message-limit', '#{e|+|:0,1}']),
                 add('formatted-value', ['show-options', '-sv', 'message-limit'], '1\n')]
    add('close-consumer', ['kill-window', '-t', 'bank:effect'])
    unset = add('restore-default', ['set-option', '-su', 'message-limit'])
    add('restored-default', ['show-options', '-sv', 'message-limit'])
    return {'id': entry['id'], 'indices': list(range(begin, len(plan))), 'status': 'REQUIRED',
            'markers': contract['recipe']['markers'],
            'obligations': {'default': [default], 'replace': list(range(after-2, after+1)),
                            'effect': [after], 'append': [appended-1, appended], 'unset': [unset, unset+1],
                            'scope': scope, 'only-if-unset': only_quiet, 'quiet': only_quiet, 'format': formatted}}


def per_option_observations(commands, plan, cells, os_name='linux'):
    from options_bank import message_limit_observation
    require(len(commands) == len(plan), 'per-option command population')
    observations = []
    clients = [command for command in commands if command['role'] == 'inventory-client']
    client = clients[0]['stdout'].strip().split('|') if clients else []
    if 'message-limit' in cells:
        require(len(clients) == 1 and len(client) == 2 and client[0].isdigit()
                and int(client[0]) > 0 and valid_tty(client[1], os_name), 'attached client identity')
    by_role = {command['role']: command for command in commands}
    if 'message-limit' in cells:
        require(by_role['message-limit:restored-default']['stdout'] == by_role['message-limit:default']['stdout'],
                'message-limit default not restored')
    active_attachment = 'attach'
    for command, expected in zip(commands, plan):
        if 'reattach' in expected:
            active_attachment = 'attach-' + expected['reattach']
        args = [client[1] if arg == '$client' else command['argv'][4+i] if arg == '$attached' else arg for i, arg in enumerate(expected['argv'])]
        if '$attached' in expected['argv']:
            require(command.get('attached_label') == active_attachment, 'selected attachment binding')
        require(command['role'] == expected['role'] and command['argv'][4:] == args,
                'per-option command binding')
        require(type(command['exit_code']) is int and
                ((command['exit_code'] != 0) if expected['error'] else command['exit_code'] == 0),
                'per-option exit: ' + expected['role'])
        for channel in ('stdout', 'stderr'):
            require(hashlib.sha256(command[channel].encode()).hexdigest() == command[channel + '_sha256'], 'output digest')
        if 'expected_stdout' in expected:
            require(command['stdout'] == expected['expected_stdout'], 'value/consumer: ' + expected['role'])
        if 'expected_lines' in expected:
            require(command['stdout'].splitlines() == expected['expected_lines'], 'real consumer: ' + expected['role'])
        if 'must_differ_from' in expected:
            require(command['stdout'] != by_role[expected['must_differ_from']]['stdout'],
                    'unchanged downstream consumer: ' + expected['role'])
        if 'must_equal_to' in expected:
            require(command['stdout'] == by_role[expected['must_equal_to']]['stdout'],
                    'downstream consumer did not restore: ' + expected['role'])
        if 'expected_pty_size' in expected:
            require(command.get('pty_size') == expected['expected_pty_size'], 'native PTY consumer: ' + expected['role'])
        if 'source_text' in expected:
            require(command.get('source_text') == expected['source_text'], 'source mutation binding')
        if 'client_input' in expected:
            require(command.get('client_input') == expected['client_input'], 'attached client input binding')
        if not expected['error']:
            require(command['stderr'] == '', 'unexpected stderr: ' + expected['role'])
        if expected['role'] in ('message-limit:after', 'message-limit:append-after'):
            normalized = message_limit_observation(by_role['message-limit:before'], command, cells['message-limit']['markers'])
            observations.append({'role': command['role'], **normalized, 'exit_code': command['exit_code']})
        elif expected['role'] == 'message-limit:before':
            # Its two payloads are checked against the frozen markers above.
            continue
        elif not expected['role'].startswith('inventory-'):
            observations.append({key: ('' if key == 'stdout' and 'consumer_effect' in expected else command[key])
                                 for key in ('role', 'stdout', 'exit_code', *(['pty_size'] if 'expected_pty_size' in expected else []))})
            if 'client_screen' in expected:
                require(command.get('screen_observation') == expected['client_screen'], 'screen consumer observation')
                observations[-1]['screen'] = command['screen_observation']
            if 'client_control' in expected:
                require(isinstance(command.get('client_control'), str) and command['client_control'], 'terminal control observation')
                observations[-1]['client_control'] = command['client_control']
                if 'control_differ_from' in expected:
                    require(command['client_control'] != by_role[expected['control_differ_from']]['client_control'], 'unchanged terminal control')
                if 'control_equal_to' in expected:
                    require(command['client_control'] == by_role[expected['control_equal_to']]['client_control'], 'terminal control did not restore')
            if 'client_event' in expected:
                require(command.get('client_event') == expected['client_event'], 'terminal event observation')
                observations[-1]['client_event'] = command['client_event']
            if 'expected_daemon_alive' in expected:
                require(command.get('daemon_alive') is expected['expected_daemon_alive'], 'daemon lifetime observation')
                observations[-1]['daemon_alive'] = command['daemon_alive']
            if expected.get('raw_observation'):
                raw = command['raw_input']
                if 'expected_raw' in expected:
                    require(raw == expected['expected_raw'], 'raw terminal consumer bytes')
                if 'raw_differ_from' in expected:
                    require(raw != by_role[expected['raw_differ_from']]['raw_input'], 'unchanged raw terminal consumer')
                if 'raw_equal_to' in expected:
                    require(raw == by_role[expected['raw_equal_to']]['raw_input'], 'raw terminal consumer did not restore')
                observations[-1]['stdout'] = ''
                observations[-1]['raw_input'] = raw
            if 'files_expected' in expected:
                records = command.get('file_observations', [])
                require(len(records) == len(expected['files_expected']), 'file observer population')
                files = []
                for actual, spec in zip(records, expected['files_expected']):
                    digest = hashlib.sha256(spec['text'].encode()).hexdigest()
                    require(actual['exists'] == spec['exists'] and actual['snapshot']['sha256'] == digest, 'application file observation')
                    files.append(dict(exists=actual['exists'], sha256=actual['snapshot']['sha256']))
                observations[-1]['files'] = files
    require(all(spec['indices'] for spec in cells.values() if spec['status'] == 'REQUIRED'), 'missing option execution')
    return observations


def per_option_phase(receipt, contract, phase):
    from options_bank import inventory_gate, default_birth, command_time
    from options_bank_server_effects import CONFIGURATIONS
    registry = load(contract['registry']['path'])
    inventory_gate(registry, contract)
    plan, cells = per_option_plan(registry, contract, contract['default_recipe']['names'])
    require(not receipt.get('SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE'), 'synthetic native receipt')
    os_name = contract.get('native_os', 'linux')
    require(os_name in ('linux', 'macos'), 'supported native per-option OS')
    require(receipt['phase'] == phase and receipt['native_os'] == os_name and receipt['source'] == contract['native_source']
            and receipt['binary'] == contract['native_binaries'][phase] and receipt['driver'] == contract['default_driver']
            and receipt['recipe_sha256'] == contract['default_recipe_sha256'], 'per-option receipt identity')
    closure = receipt['closure']
    require(receipt['execution_status'] == 'COMPLETE' and closure['status'] == 'PASS' and closure['errors'] == []
            and type(closure['endpoint_connect_errno']) is int and closure['endpoint_connect_errno'] in ((2, 61) if os_name == 'macos' else (2, 111))
            and receipt['fd_balance']['before'] == receipt['fd_balance']['after'], 'per-option closure')
    consumers = {('consumer-' + row.get('consumer_label', row['role'].split(':')[0])):
                 row for row in plan if 'consumer_argv' in row or row['role'].endswith(':create-consumer')}
    from options_bank_linux_screen import requires_client, verify as verify_screen, verify_event
    attached_client = 'message-limit' in cells or requires_client(plan)
    attachments = {'attach', *('attach-' + row['reattach'] for row in plan if 'reattach' in row),
                   *('attach-' + row['add_client']['label'] for row in plan if 'add_client' in row)} if attached_client else set()
    require(set(closure['exits']) == set(receipt['processes']) ==
            {'daemon', 'helper', *attachments, *consumers}, 'consumer population')
    for label, process in receipt['processes'].items():
        birth, ended = process['identity'], closure['exits'][label]
        default_birth(birth, birth['pid'], label)
        default_birth(ended['birth'], birth['pid'], label + ' exit')
        if os_name == 'macos':
            from options_bank_macos import validate_birth
            validate_birth(birth)
            validate_birth(ended['birth'])
        exited = (ended.get('native_exit') is True and ended.get('exit_method') == 'kqueue-NOTE_EXIT'
                  if os_name == 'macos' else ended.get('pidfd_exit') is True)
        require(same(ended['birth'], birth) and ended['birth']['ppid'] == birth['ppid']
                and exited and ended['after'] is None
                and (type(ended['wait_status']) is int if label == 'daemon' else ended['wait_status'] is None
                     or type(ended['wait_status']) is int), 'consumer birth/exit')
        expected = contract['native_binaries'][phase] if label == 'daemon' or label in attachments else contract['python'] if label == 'helper' else pin(consumers[label].get('consumer_exe', '/bin/sleep'))
        require(process['exe_sha256'] == expected['sha256'], 'consumer executable')
        if label in consumers and 'consumer_argv' in consumers[label]:
            require(process['argv'] == consumers[label].get('consumer_argv', ['/bin/sleep', '240']), 'launch consumer argv')
    commands, prefix = receipt['commands'], [receipt['binary']['path'], '-N', '-S', receipt['public'][1]]
    root = str(Path(receipt['public'][1]).parent)
    require(receipt['startup']['sha256'] == hashlib.sha256(contract['default_recipe']['startup'].encode()).hexdigest()
            and receipt['endpoint']['path'] == receipt['public'][1]
            and receipt['environment'] == {'HOME': root+'/home', 'XDG_CONFIG_HOME': root+'/xdg',
                'PATH': '/usr/bin:/bin', 'SHELL': '/bin/bash', 'TERM': 'xterm',
                'LANG': 'en_US.UTF-8' if os_name == 'macos' else 'C.UTF-8',
                'RMUX_DISABLE_TMUX_SHIM': '1', 'RMUX_DISABLE_TMUX_FALLBACK': '1'}, 'per-option startup/environment')
    require(commands[0]['argv'] == prefix + ['new-session', '-d', '-s', 'bank', '-n', 'bank', '-x', '80', '-y', '24',
                '-c', root, contract['python']['path'], '-I', contract['default_driver']['path'], 'helper', root,
                contract['default_recipe']['token']], 'per-option helper binding')
    daemon, helper = [receipt['processes'][label]['identity'] for label in ('daemon', 'helper')]
    require(receipt['public'][0] == str(daemon['pid']) and receipt['public'][5] == str(helper['pid'])
            and all(proc['identity']['ppid'] == daemon['pid'] for label, proc in receipt['processes'].items() if label != 'daemon' and label not in attachments),
            'per-option process parent/public identity')
    require(len(commands) >= len(plan) + 3 and commands[1]['stdout'] == '|'.join(receipt['public']) + '\n'
            and commands[-1]['argv'] == prefix + ['kill-server'], 'per-option fixture commands')
    previous, final = command_time(receipt['started']), command_time(receipt['ended'])
    require(0 <= (final-previous).total_seconds() <= 300, 'per-option time bound')
    for command in commands:
        default_birth(command['birth'], command['pid'], 'command')
        if os_name == 'macos':
            validate_birth(command['birth'])
        begin, end = command_time(command['started']), command_time(command['ended'])
        require(command['argv'][:4] == prefix and previous <= begin <= end <= final
                and command['birth']['ppid'] == daemon['ppid'] and command['after'] is None
                and command['wait_method'] == 'Popen.communicate', 'command wait/chronology')
        previous = end
    from options_bank_linux_windows import planned_commands
    selected, indices = planned_commands(commands, plan, prefix)
    require(receipt['cells'] == {name: [indices[index] for index in spec['indices']] for name, spec in cells.items()}, 'option cell indices')
    if 'backspace' in cells:
        require(receipt['configuration']['sha256'] == hashlib.sha256(b'set-option -s backspace C-h\n').hexdigest(),
                'backspace configuration input')
    for name in set(cells) & set(CONFIGURATIONS):
        config = receipt['server_configurations'][name]
        require(config['path'] == root + '/' + name + '.conf'
                and config['sha256'] == hashlib.sha256(CONFIGURATIONS[name].encode()).hexdigest(),
                'server configuration input: ' + name)
    if set(cells) & set(GEOMETRY_OPTIONS):
        from options_geometry_linux import verify_samples
        verify_samples(receipt, contract)
    observations = per_option_observations(selected, plan, cells, os_name)
    for command, row in zip(selected, plan):
        if 'files_expected' in row:
            from options_bank_linux_history import verify_files
            verify_files(command['file_observations'], row['files_expected'], root)
        if 'closure_files' in row:
            from options_bank_linux_history import verify_files
            verify_files(receipt['closure_files'], row['closure_files'], root)
        if row.get('raw_observation'):
            from options_bank_linux_raw import verify as verify_raw
            verify_raw(receipt, command, root, contract)
        if '$attached' in row['argv']:
            attached = receipt['processes'][command['attached_label']]
            for index, arg in enumerate(row['argv']):
                if arg == '$attached':
                    require(command['argv'][4+index] == attached['fd0'], 'attached command target')
        if any(key in row for key in ('client_screen', 'client_control')):
            verify_screen(command, row, root)
        if 'client_event' in row:
            verify_event(command, row, root)
        if 'wait_file' in row:
            artifact = command['file_observer']
            require(artifact['path'] == root + '/' + row['wait_file'], 'copy consumer file target')
            authenticate(artifact)
            require(Path(artifact['path']).read_bytes() == row['expected_file_bytes'].encode(), 'copy consumer file bytes')
        if 'consumer_effect' in row:
            require(command['stdout'] == str(receipt['processes']['consumer-' + row['consumer_effect']]['identity']['pid']) + '\n',
                    'public launch consumer identity')
    if attached_client:
        attached = receipt['processes']['attach']
        require(attached['identity']['ppid'] == daemon['ppid']
                and attached['argv'] == receipt['attach_argv'] == prefix + ['attach-session', '-t', 'bank'],
                'attached client birth/target')
        for label in attachments:
            process = receipt['processes'][label]
            auxiliary = next((row['add_client'] for row in plan if 'add_client' in row and label == 'attach-' + row['add_client']['label']), None)
            session = auxiliary['session'] if auxiliary else 'bank'
            require(process['identity']['ppid'] == daemon['ppid'] and process['argv'] == prefix + ['attach-session', '-t', session],
                    'additional attached client birth/target')
            if auxiliary:
                stream = receipt['auxiliary_streams'][label]
                require(stream['path'] == root + '/' + label + '.pty', 'auxiliary terminal stream target')
                authenticate(stream)
        if 'message-limit' in cells:
            clients = next(command['stdout'].strip() for command in commands if command['role'] == 'inventory-client')
            require(clients == receipt['attached_client'] == str(attached['identity']['pid']) + '|' + attached['fd0'],
                    'message-limit attached client binding')
    for entry in registry['options']:
        if entry['name'] in cells and cells[entry['name']]['indices']:
            default = (contract['default_recipe']['environment']['SHELL'] if entry['name'] == 'default-shell'
                       else '\n'.join(entry['default']['values']) if entry['default'].get('kind') == 'array'
                       else entry['default']['value'])
            if entry.get('is_array', False) and entry['default'].get('kind') != 'array':
                import re
                default = '\n'.join(re.split('[' + re.escape(entry['separator']) + ']+', default))
            expected_default = default + '\n' if default or not entry.get('is_array', False) else ''
            require(commands[cells[entry['name']]['indices'][0]+2]['stdout'] == expected_default,
                    'independent option default: ' + entry['name'])
    names = {line.split(' ', 1)[0].split('[', 1)[0].removesuffix('*')
             for command in commands[2:6] for line in command['stdout'].splitlines()}
    require(names == {entry['name'] for entry in registry['options']}, 'independent native option inventory')
    return observations

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def pin(path):
    path = Path(path)
    return {'path': str(path), 'bytes': path.stat().st_size, 'sha256': sha(path)}


def load(path):
    return json.loads(Path(path).read_text())


def save(path, value):
    path = Path(path)
    fd, temporary = tempfile.mkstemp(dir=path.parent, prefix='.' + path.name + '-')
    try:
        with os.fdopen(fd, 'w') as stream:
            json.dump(value, stream, indent=2, sort_keys=True)
            stream.write('\n')
        # Publish complete bytes on the same filesystem, retaining open('x')'s
        # no-clobber contract for existing receipts and competing writers.
        os.link(temporary, path)
    finally:
        os.unlink(temporary)


def require(value, message):
    if not value:
        raise RuntimeError(message)


def authenticate(artifact):
    require(pin(artifact['path']) == artifact, 'artifact pin mismatch: ' + artifact['path'])



def execute(contract_path, phase, output, mode='run'):
    from options_bank import default_phase, default_plan
    require(sys.platform in ('linux', 'darwin') and phase in ('oracle', 'candidate'), 'native Unix product phase required')
    contract = load(contract_path)
    require(contract.get('native_os', 'linux') == native_os(), 'native contract OS mismatch')
    for artifact in contract['artifacts']:
        authenticate(artifact)
    for artifact in (*contract['native_binaries'].values(), contract['python'], contract['default_driver']):
        authenticate(artifact)
    require(pin(__file__) == contract['default_driver'], 'driver pin')
    require(subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=contract['source_path'], text=True).strip()
            == contract['native_source'], 'source HEAD mismatch')
    require(not subprocess.check_output(['git', 'status', '--porcelain'], cwd=contract['source_path'], text=True).strip(),
            'dirty native source')
    per_option = contract['default_recipe']['kind'] == 'per-option-v1'
    require(sys.platform != 'darwin' or per_option, 'macOS port covers per-option recipe only')
    require(mode != 'per-option' or per_option, 'per-option mode requires per-option recipe')
    if per_option:
        require(hashlib.sha256(json.dumps(contract['default_recipe'], sort_keys=True).encode()).hexdigest()
                == contract['default_recipe_sha256'], 'per-option recipe digest')
        per_option_plan(load(contract['registry']['path']), contract, contract['default_recipe']['names'])
    else:
        default_plan(contract)
    require(not (output.parent / 'RECOVERY-OWNER.json').exists(), 'previous closure blocks new product')
    if phase == 'candidate':
        prior = load(output.parent / 'ORACLE-FROZEN.json')
        authenticate(prior['receipt'])
        (per_option_phase if per_option else default_phase)(load(prior['receipt']['path']), contract, 'oracle')
        subprocess.run([sys.executable, str(Path(__file__).resolve().parent.parent / 'scripts/candidate_identity.py'),
                        'check', contract['native_binaries']['candidate']['path']], check=True)
    output.mkdir()
    before = fds()
    run = Run(contract, phase, output)
    try:
        run.execute()
        run.record['execution_status'] = 'COMPLETE'
    except Exception as error:
        run.record.update(execution_status='FAIL', error=str(error), traceback=traceback.format_exc())
    finally:
        try:
            run.cleanup()
        except Exception as error:
            run.record['closure'] = {'status': 'FAIL', 'error': str(error), 'traceback': traceback.format_exc()}
        after = fds()
        run.record['fd_balance'] = {'before': before, 'after': after}
        if before != after:
            run.record['closure']['status'] = 'FAIL'
        run.record.update(ended=now(), elapsed_seconds=time.monotonic()-run.start)
        require(len(json.dumps(run.record).encode()) < 1048576, 'reserved receipt cap')
        if run.master is not None or 'attach' in run.record['processes']:
            (output / 'attach.pty').write_bytes(run.pty_bytes)
            run.record['attach_pty'] = pin(output / 'attach.pty')
        else:
            require(not run.pty_bytes, 'default fixture does not attach a client')
        save(output / 'RESULT.json', run.record)
        if run.record['closure']['status'] != 'PASS':
            save(output.parent / 'RECOVERY-OWNER.json', {'owner': 'L6', 'receipt': pin(output / 'RESULT.json'),
                 'new_lot_allowed': False, 'processes': run.record['processes']})
    if per_option and run.record['execution_status'] == 'COMPLETE' and run.record['closure']['status'] == 'PASS':
        observations = per_option_phase(run.record, contract, phase)
        if phase == 'candidate':
            require(observations == per_option_phase(load(prior['receipt']['path']), contract, 'oracle'), 'oracle/candidate per-option mismatch')
        save(output / 'PER-OPTION.json', {'observations': observations, 'options': run.record['option_specs'],
                                        'whole_criterion_closed': False})
    print(json.dumps({'phase': phase, 'execution': run.record['execution_status'],
                     'closure': run.record['closure']['status'], 'result': pin(output / 'RESULT.json')}))
    return 0 if run.record['execution_status']=='COMPLETE' and run.record['closure']['status']=='PASS' else 1


if __name__ == '__main__':
    if len(sys.argv)==4 and sys.argv[1]=='helper':
        helper(*sys.argv[2:])
    elif len(sys.argv)==5 and sys.argv[1] in ('run', 'per-option'):
        sys.exit(execute(sys.argv[2], sys.argv[3], Path(sys.argv[4]), sys.argv[1]))
    else:
        raise SystemExit('helper DIRECTORY TOKEN | run|per-option CONTRACT PHASE OUTPUT')
