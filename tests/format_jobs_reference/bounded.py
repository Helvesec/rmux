"""Finite Linux profile of the existing format reference harness.

The contract pins both binaries, source, harness and exact declarative commands.
Oracle output is frozen before candidate execution. No expected product values
are generated from the candidate registry. Legacy Mac scenarios are unchanged.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import resource
import select
import signal
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone

from support import Client, sha, save


def now():
    return datetime.now(timezone.utc).isoformat()


def pin(path):
    path = Path(path)
    return dict(path=str(path), bytes=path.stat().st_size, sha256=sha(path))


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def authenticate(value):
    require(pin(value['path']) == value, 'artifact authentication: ' + value['path'])


def identity(pid):
    try:
        p = Path('/proc') / str(pid)
        f = (p / 'stat').read_text().rsplit(')', 1)[1].split()
        return dict(pid=pid, ppid=int(f[1]), state=f[0], start_ticks=f[19],
                    executable=os.path.realpath(p / 'exe'))
    except (FileNotFoundError, ProcessLookupError):
        return None


def same(a, b):
    return bool(a and b and (a['pid'], a['start_ticks']) == (b['pid'], b['start_ticks']))


def expand(value, root):
    if isinstance(value, str):
        return value.replace('@ROOT@', str(root))
    if isinstance(value, list):
        return [expand(x, root) for x in value]
    if isinstance(value, dict):
        return {k: expand(v, root) for k, v in value.items()}
    return value


class UnknownEvidence(RuntimeError):
    """A legacy observation lacks facts needed for a bounded conclusion."""


def read_pinned(ref):
    data = Path(ref['path']).read_bytes()
    require(len(data) == ref['bytes'] and hashlib.sha256(data).hexdigest() == ref['sha256'],
            'frozen plan authentication: ' + ref['path'])
    return json.loads(data)


def planned_processes(contract):
    declarations = [('daemon', 'binary')]
    declarations += [(name, 'shell') for name in contract['startup_helpers']]
    for step in contract['steps']:
        if step.get('helper'):
            declarations.append((step['helper'], 'shell'))
        if step['kind'] == 'attach':
            declarations.append(('attached-client', 'binary'))
    names = [name for name, _ in declarations]
    require(all(isinstance(name, str) and name and
                all(ch.isalnum() or ch in '-_' for ch in name) for name in names),
            'invalid planned process name')
    require(len(names) == len(set(names)), 'duplicate planned process name')
    require(len(names) <= 1 + contract['limits']['helpers_per_phase'], 'planned helper cap')
    return dict(declarations)


def planned_observations(contract):
    steps = [step for step in contract['steps']
             if step['kind'] in ('observe', 'delivery', 'status-job', 'route')]
    ids = [step['id'] for step in steps]
    require(ids and all(isinstance(x, str) and x for x in ids)
            and len(ids) == len(set(ids)), 'ordered unique planned observation IDs')
    return steps


def raw_command(command, policy):
    require(type(command.get('exit')) is int and not command.get('timeout'),
            'missing exit or timed out observation')
    raw = {}
    for key in ('stdout', 'stderr'):
        require(isinstance(command.get(key), str), 'missing raw ' + key)
        raw[key] = base64.b64decode(command[key], validate=True)
    require(type(policy.get('exit')) is int and isinstance(policy.get('stderr'), str),
            'explicit success/error policy required')
    require(command['exit'] == policy['exit'] and command['stderr'] == policy['stderr'],
            'unexpected exit/stderr')
    if policy['exit'] != 0:
        require(isinstance(policy.get('stdout'), str) and command['stdout'] == policy['stdout'],
                'explicit expected error stdout required')
    return raw


def load_reader_plan(ref):
    plan = read_pinned(ref)
    require(plan.get('schema') == 1, 'reader plan schema')
    contract = read_pinned(plan['contract'])
    if 'independent_corpus' in contract:
        from routes import validate_contract
        validate_contract(contract)
    planned_processes(contract)
    steps = planned_observations(contract)
    require(set(plan['roots']) == {'oracle', 'candidate'} and
            len(set(plan['roots'].values())) == 2 and
            all(Path(x).is_absolute() for x in plan['roots'].values()), 'frozen phase roots')
    require(list(plan['policies']) == [step['id'] for step in steps], 'ordered per-case policies')
    for step in steps:
        policy = plan['policies'][step['id']]
        require(type(policy.get('exit')) is int and isinstance(policy.get('stderr'), str),
                'explicit per-case success/error policy')
        base64.b64decode(policy['stderr'], validate=True)
        if policy['exit'] != 0:
            require(isinstance(policy.get('stdout'), str), 'explicit expected error stdout')
            base64.b64decode(policy['stdout'], validate=True)
        if not plan.get('archive'):
            require(step.get('success') == policy, 'corpus policy binding')
    if plan.get('archive'):
        require(set(plan.get('launches', {})) == {'oracle', 'candidate'},
                'archive launch identities missing')
        for phase, launch in plan['launches'].items():
            argv = read_pinned(launch)['argv']
            expected = [contract['python']['path'], '-B',
                        contract['harness']['tests/format_jobs_reference.py']['path'],
                        'bounded-linux', plan['contract']['path'], phase, plan['roots'][phase]]
            require(argv[:7] == expected, 'archive launch identity')
    require(set(plan['executables']) == {'oracle', 'candidate', 'shell'},
            'qualified executable paths')
    return plan, contract


def birth_key(born):
    if not isinstance(born, dict) or 'pid' not in born or 'start_ticks' not in born:
        raise UnknownEvidence('process birth facts missing')
    if not (type(born.get('pid')) is int and born['pid'] > 0
            and isinstance(born.get('start_ticks'), str) and born['start_ticks'].isdigit()
            and int(born['start_ticks']) > 0):
        raise UnknownEvidence('process birth facts uninterpretable')
    return born['pid'], born['start_ticks']


def validate_population(plan, contract, phase, receipt):
    expected = planned_processes(contract)
    processes = receipt['processes']
    if set(expected) - set(processes):
        raise UnknownEvidence('missing planned process birth/closure')
    require(set(processes) == set(expected), 'unexpected planned process')
    population = receipt.get('process_population', {})
    require(population.get('expected') == sorted(expected) and
            population.get('observed') == sorted(expected), 'population receipt binding')
    keys, pids, closure = set(), set(), {}
    daemon_record = processes['daemon']
    require(isinstance(daemon_record, dict), 'malformed daemon process record')
    daemon = daemon_record.get('birth')
    birth_key(daemon)
    for name, role in expected.items():
        proc = processes[name]
        born = proc.get('birth')
        key = birth_key(born)
        require(key not in keys and key[0] not in pids, 'aliased process birth')
        keys.add(key); pids.add(key[0])
        tool = contract['binaries'][phase] if role == 'binary' else contract['shell']
        executable = plan['executables'][phase if role == 'binary' else 'shell']
        if 'executable' not in born or 'executable' not in proc:
            raise UnknownEvidence('process executable identity missing')
        require(proc.get('executable') == tool and born.get('executable') == executable,
                'process tool identity')
        require(type(born.get('ppid')) is int and born['ppid'] > 0, 'parent identity')
        if role == 'shell':
            require(born['ppid'] == daemon['pid'], 'helper parent identity')
            marker = proc.get('marker', {})
            require(marker.get('path') == str(Path(plan['roots'][phase]) / (name + '.pid')),
                    'helper marker identity')
            # Marker bytes are independently bound by the phase receipt; do not
            # perform any live process lookup while reading archived evidence.
            expected_marker = (str(born['pid']) + '\n').encode()
            require(marker.get('bytes') == len(expected_marker) and
                    marker.get('sha256') == hashlib.sha256(expected_marker).hexdigest(),
                    'helper marker/birth binding')
        require(proc.get('pidfd_exit') is True and not same(born, proc.get('after')),
                'process closure evidence')
        closure[name] = 'INDIRECT_PIDFD_EXIT'
    require(type(receipt.get('daemon_wait')) is int, 'daemon direct wait missing')
    closure['daemon'] = 'DIRECT_WAIT_AND_PIDFD_EXIT'
    clients = receipt.get('clients', [])
    attached = [step for step in contract['steps'] if step['kind'] == 'attach']
    require(len(clients) == len(attached), 'attached client population')
    for client, step in zip(clients, attached):
        born = processes['attached-client']['birth']
        prefix = [contract['binaries'][phase]['path'], '-N', '-S',
                  str(Path(plan['roots'][phase]) / 'socket')]
        launch_prefix = qualified_argv(contract, phase, prefix, check_context=False)
        require(client.get('pid') == born['pid'] and
                client.get('identity') == dict(pid=born['pid'], path=prefix[0],
                                               start_ticks=born['start_ticks']) and
                client.get('argv') == launch_prefix + ['attach-session', '-t', step['session']] and
                client.get('control') is False and type(client.get('returncode')) is int,
                'attached client direct wait/identity')
        require(born['ppid'] == daemon['ppid'], 'client collector parent identity')
        closure['attached-client'] = 'DIRECT_WAIT_AND_PIDFD_EXIT'
    require(type(receipt.get('endpoint_connect_errno')) is int and
            receipt['endpoint_connect_errno'] in (2, 111), 'endpoint closure evidence')
    require(receipt.get('closure', {}).get('status') == 'PASS' and
            receipt['closure'].get('errors') == [], 'closure summary')
    return closure


def phase_observations(plan, contract, phase, receipt):
    if plan.get('archive') and any(key not in receipt for key in
            ('phase', 'native_os', 'source', 'binary', 'binary_after', 'contract', 'processes')):
        raise UnknownEvidence('archived phase identity facts missing')
    require(receipt.get('phase') == phase and receipt.get('native_os') == 'linux',
            'phase identity')
    require(receipt.get('source') == contract['source'], 'frozen source identity')
    tool = contract['binaries'][phase]
    require(receipt.get('binary') == tool and receipt.get('binary_after') == tool,
            'frozen tool identity')
    require(receipt.get('contract') == plan['contract'], 'frozen contract identity')
    require(receipt.get('status') == 'EXECUTED', 'phase not executed')
    root = Path(plan['roots'][phase])
    prefix = [tool['path'], '-N', '-S', str(root / 'socket')]
    require(receipt.get('endpoint', {}).get('path') == str(root / 'socket'),
            'frozen endpoint identity')
    startup = expand(contract['startup'], root).encode()
    require(receipt.get('startup') == dict(path=str(root / 'startup.conf'),
            bytes=len(startup), sha256=hashlib.sha256(startup).hexdigest()), 'startup identity')
    require(receipt.get('daemon_argv') == [tool['path'], '-D', '-S', str(root / 'socket'),
            '-f', str(root / 'startup.conf')], 'daemon command identity')
    closure = validate_population(plan, contract, phase, receipt)
    steps = planned_observations(contract)
    observations = receipt['observations']
    require([x.get('id') for x in observations] == [x['id'] for x in steps],
            'exact ordered frozen observation population')
    values, last = [], -1
    for step, obs in zip(steps, observations):
        kind = step['kind']
        if kind == 'route':
            from routes import observation_value
            value, last = observation_value(plan, contract, phase, receipt, step, obs, last)
            values.append((value, obs['command']))
            continue
        if kind == 'status-job':
            raise UnknownEvidence('status-job cold/warm bytes not established by legacy effect labels')
        allowed = {'id', 'command', 'kind'}
        if kind == 'delivery':
            allowed |= {'delivery', 'required', 'expansion_status'}
        require(not set(obs) - allowed and obs.get('kind', kind) == kind,
                'observation kind')
        if not plan.get('archive'):
            require(obs.get('kind') == kind, 'explicit observation kind')
        index = obs.get('command')
        require(type(index) is int and last < index < len(receipt['commands']),
                'ordered observation command index')
        last = index
        command = receipt['commands'][index]
        if plan.get('archive') and any(key not in command for key in ('argv', 'label', 'exit', 'stdout', 'stderr')):
            raise UnknownEvidence('archived command identity/raw facts missing')
        args = step['argv'] if kind == 'observe' else ['capture-pane', '-p', '-t', step['target']]
        require(command.get('argv') == prefix + expand(args, root) and
                command.get('label') == step['id'], 'expanded observation argv/target')
        raw = raw_command(command, plan['policies'][step['id']])
        if kind == 'delivery':
            actual = next((line for line in raw['stdout'].splitlines()
                           if line.startswith(step['prefix'].encode())), None)
            require(actual is not None and obs.get('delivery', '').encode() == actual and
                    obs.get('required') == step['line'] and
                    obs.get('expansion_status') == ('PASS' if actual == step['line'].encode()
                                                   else 'FAIL'), 'raw delivery binding')
            value = actual
        else:
            value = (command['exit'], raw['stdout'], raw['stderr'])
        values.append((value, index))
    if 'independent_corpus' in contract:
        from routes import validate_values
        validate_values(contract, steps, values)
    return values, closure


def qualified_argv(contract, phase, argv, check_context=True):
    guard = contract.get('candidate_identity') if phase == 'candidate' else None
    if not guard:
        return argv
    if check_context:
        for key in ('script', 'python'):
            authenticate(guard[key])
        require(all(os.environ.get('RMUX_IDENTITY_' + key.upper()) == guard[key]
                    for key in ('source', 'head', 'target')), 'qualified launch context differs')
    return [guard['python']['path'], guard['script']['path'], 'exec', *argv]


class LinuxPhase:
    def __init__(self, contract, phase, root):
        planned_processes(contract)  # Validate before creating directories or launching anything.
        planned_observations(contract)
        self.contract, self.phase, self.root = contract, phase, root
        self.binary = contract['binaries'][phase]['path']
        self.socket = root / 'socket'
        self.deadline = time.monotonic() + contract['limits']['seconds'] - 60
        self.env = expand(contract['environment'], root)
        if phase == 'candidate' and 'candidate_identity' in contract:
            self.env = {**os.environ, **self.env}
        self.held, self.daemon, self.log = {}, None, None
        self.clients, self.server = [], None
        self.out, self.end = root, self.deadline
        self.prefix = [self.binary, '-N', '-S', str(self.socket)]
        self.execution_prefix = qualified_argv(contract, phase, self.prefix)
        self.produced = 0
        self.r = dict(schema=1, phase=phase, source=contract['source'],
                      binary=contract['binaries'][phase], started=now(),
                      commands=[], observations=[], processes={}, clients=[], assertions=[], status='FAIL')
        for name in ('home', 'xdg'):
            (root / name).mkdir()

    def require(self, condition, description, **details):
        self.r['assertions'].append(dict(description=description, passed=bool(condition), **details))
        require(condition, description)

    def until(self, fn, description, timeout=2):
        return self.wait(fn, description, timeout)

    def remaining(self):
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, 'execution deadline; cleanup reserved')
        return remaining

    def wait(self, fn, reason, seconds=3):
        stop = time.monotonic() + min(seconds, self.remaining())
        while time.monotonic() < stop:
            value = fn()
            if value:
                return value
            time.sleep(.01)
        raise RuntimeError('timeout: ' + reason)

    def hold(self, name, pid, expected):
        planned = planned_processes(self.contract)
        require(name in planned, 'unplanned held name')
        require(name not in self.held and name not in self.r['processes'], 'duplicate held name')
        role = planned[name]
        require(expected == (self.contract['binaries'][self.phase] if role == 'binary'
                             else self.contract['shell']), 'held tool declaration')
        require(len(self.held) < 1 + self.contract['limits']['helpers_per_phase'], 'helper cap')
        def executed_identity():
            value = identity(pid)
            return value if value and value['executable'] == os.path.realpath(expected['path']) else None
        born = self.wait(executed_identity, 'owned executable after exec')
        require(born and born['executable'] == os.path.realpath(expected['path']), 'owned executable')
        require(sha(born['executable']) == expected['sha256'], 'owned binary SHA')
        key = birth_key(born)
        require(all(key != birth_key(other) and pid != other['pid']
                    for other, _ in self.held.values()), 'duplicate held process birth')
        fd = os.pidfd_open(pid)
        try:
            require(same(born, identity(pid)), 'birth changed')
        except Exception:
            os.close(fd)
            raise
        self.held[name] = (born, fd)
        self.r['processes'][name] = dict(birth=born, executable=expected)
        return born

    def q(self, args, label=None, check=True, cleanup=False):
        require(len(self.r['commands']) < self.contract['limits']['commands_per_phase'], 'command cap')
        timeout = 5 if cleanup else min(5, self.remaining())
        argv = [self.binary, '-N', '-S', str(self.socket), *expand(args, self.root)]
        r = dict(argv=argv, started=now(), label=label)
        self.r['commands'].append(r)
        launch_argv = qualified_argv(self.contract, self.phase, argv)
        r['launch_argv'] = launch_argv
        child = subprocess.Popen(launch_argv, env={**os.environ, **self.env} if
                                 'candidate_identity' in self.contract else self.env, cwd=self.root,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE)
        r['birth'] = identity(child.pid)
        try:
            out, err = child.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            child.kill()
            out, err = child.communicate(timeout=1)
            r['timeout'] = True
        self.produced += len(out) + len(err)
        r.update(ended=now(), exit=child.returncode,
                 waited_pid=child.pid, parent_pid=os.getpid(),
                 gone_after_wait=not Path('/proc', str(child.pid)).exists(),
                 stdout=base64.b64encode(out).decode(), stderr=base64.b64encode(err).decode())
        require(self.produced <= self.contract['limits']['bytes_per_phase'], 'produced byte cap')
        require(len(out) + len(err) <= 262144, 'command byte cap')
        if check:
            require(child.returncode == 0 and not r.get('timeout'), 'command failed: ' + str(args))
        return out

    def start(self):
        planned_processes(self.contract)
        config = self.root / 'startup.conf'
        config.write_text(expand(self.contract['startup'], self.root))
        self.r['startup'] = pin(config)
        argv = [self.binary, '-D', '-S', str(self.socket), '-f', str(config)]
        self.r['daemon_argv'] = argv
        self.log = (self.root / 'daemon.log').open('xb')
        def limits():
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
            resource.setrlimit(resource.RLIMIT_FSIZE, (1048576, 1048576))
        launch_argv = qualified_argv(self.contract, self.phase, argv)
        self.r['daemon_launch_argv'] = launch_argv
        self.daemon = subprocess.Popen(launch_argv, env={**os.environ, **self.env} if
                                      'candidate_identity' in self.contract else self.env, cwd=self.root,
                                       stdin=subprocess.DEVNULL, stdout=self.log,
                                       stderr=subprocess.STDOUT, preexec_fn=limits)
        self.server = self.hold('daemon', self.daemon.pid, self.contract['binaries'][self.phase])
        self.wait(self.socket.exists, 'owned endpoint')
        st = self.socket.stat()
        self.r['endpoint'] = dict(path=str(self.socket), device=st.st_dev, inode=st.st_ino)
        self.q(['display-message', '-p', '#{pid}'], 'startup-boundary')
        for name in self.contract['startup_helpers']:
            self.register_helper(name)

    def register_helper(self, name):
        marker = self.root / (name + '.pid')
        self.wait(marker.exists, 'helper marker ' + name)
        pid = int(marker.read_text().strip())
        born = self.hold(name, pid, self.contract['shell'])
        require(born['ppid'] == self.daemon.pid, 'helper direct parent')
        self.r['processes'][name]['marker'] = pin(marker)

    def run(self):
        self.start()
        for step in self.contract['steps']:
            kind = step['kind']
            if kind == 'command':
                self.q(step['argv'], step.get('id'), check=step.get('check', True))
                if step.get('helper'):
                    self.register_helper(step['helper'])
            elif kind == 'route':
                from routes import execute
                execute(self, step)
            elif kind == 'file':
                f = self.root / step['name']
                require(f.parent == self.root and not f.exists(), 'fresh fixture file')
                f.write_text(expand(step['text'], self.root))
            elif kind == 'observe':
                index = len(self.r['commands'])
                self.q(step['argv'], step['id'], check=False)
                self.r['observations'].append(dict(id=step['id'], kind=kind, command=index))
            elif kind == 'attach':
                client = Client(self, step['session'], False, rows=16, cols=40)
                self.hold('attached-client', client.p.pid, self.contract['binaries'][self.phase])
                expected = (str(client.p.pid) + '|' + step['session'] + '|').encode()
                self.wait(lambda: self.q(['list-clients', '-F', '#{client_pid}|#{client_session}|#{client_tty}'], 'attached-client-identity').startswith(expected), 'registered owned client')
            elif kind == 'key':
                require(len(self.clients) == 1, 'one owned attached client')
                client = self.clients[0]
                data = bytes.fromhex(step['hex'])
                require(os.write(client.master, data) == len(data), 'PTY key delivery')
                self.r.setdefault('key_inputs', []).append(dict(at=now(), pid=client.p.pid,
                                                               data_hex=step['hex'], id=step['id']))
            elif kind == 'status-job':
                require(len(self.clients) == 1, 'job owner is attached client')
                self.q(['set-option', '-g', 'status', 'on'])
                self.q(['set-option', '-g', 'status-interval', '0'])
                self.q(['set-option', '-g', 'status-left-length', '35'])
                self.q(['set-option', '-g', 'status-right', ''])
                self.q(['set-option', '-g', 'status-left', step['format']])
                self.register_helper(step['helper'])
                client = self.clients[0]
                token = step['token'].encode()
                cold = all(token not in drain.raw for drain in client.drains)
                require(cold, 'job output absent before owned release')
                (self.root / step['release']).write_text('release')
                self.wait(lambda: any(token in drain.raw for drain in client.drains),
                          'job completes on existing terminal consumer', 3)
                self.r['observations'].append(dict(id=step['id'], kind=kind, effect=dict(cold=True, warm=step['token'])))
            elif kind == 'delivery':
                expected = step['line'].encode()
                prefix = step['prefix'].encode()
                def delivered():
                    raw = self.q(['capture-pane', '-p', '-t', step['target']], step['id'])
                    return next((line for line in raw.splitlines() if line.startswith(prefix)), None)
                raw = self.wait(delivered, 'actual delivered line ' + step['id'])
                self.r['observations'].append(dict(id=step['id'], kind=kind, delivery=raw.decode('utf-8'),
                                                  required=step['line'],
                                                  expansion_status='PASS' if raw == expected else 'FAIL',
                                                  command=len(self.r['commands']) - 1))
            else:
                raise RuntimeError('unknown declarative step: ' + kind)
        self.r['status'] = 'EXECUTED'

    def close(self):
        errors = []
        start = time.monotonic()
        try:
            if self.daemon and self.daemon.poll() is None:
                self.q(['kill-server'], 'owned-close', check=False, cleanup=True)
        except Exception as exc:
            errors.append(str(exc))
        for client in reversed(self.clients):
            client.close()
            self.produced += sum(x['bytes'] for x in client.record['captures'])
        for name, (born, fd) in reversed(list(self.held.items())):
            poll = select.poll()
            poll.register(fd, select.POLLIN)
            if not poll.poll(1000):
                signal.pidfd_send_signal(fd, signal.SIGTERM)
            if not poll.poll(1000):
                signal.pidfd_send_signal(fd, signal.SIGKILL)
            exited = bool(poll.poll(1000))
            self.r['processes'][name].update(pidfd_exit=exited, after=identity(born['pid']))
            if not exited:
                errors.append('owned process did not exit: ' + name)
            os.close(fd)
        if self.daemon:
            self.r['daemon_wait'] = self.daemon.wait(timeout=2)
        endpoint = socket.socket(socket.AF_UNIX)
        try:
            endpoint.settimeout(.1)
            rc = endpoint.connect_ex(str(self.socket))
            self.r['endpoint_connect_errno'] = rc
            if rc == 0:
                errors.append('owned endpoint still accepts connections')
        finally:
            endpoint.close()
        if self.log:
            self.log.close()
        if self.produced > self.contract['limits']['bytes_per_phase']:
            errors.append('total produced bytes exceeded including terminal')
        try:
            expected = set(planned_processes(self.contract))
            births = [birth_key(born) for born, _ in self.held.values()]
            if len(set(births)) != len(births) or len({x[0] for x in births}) != len(births):
                errors.append('aliased held process birth')
        except (RuntimeError, KeyError, TypeError) as exc:
            expected = set()
            errors.append(str(exc))
        self.r['process_population'] = dict(expected=sorted(expected), observed=sorted(self.held))
        if expected != set(self.held):
            errors.append('incomplete planned process population; no whole closure credit')
        self.r.update(ended=now(), produced_bytes=self.produced,
                      closure=dict(status='PASS' if not errors else 'FAIL', errors=errors,
                                   elapsed_seconds=time.monotonic() - start))
        self.r['binary_after'] = pin(self.binary)
        if errors or self.r['binary_after'] != self.r['binary']:
            self.r['status'] = 'FAIL'
        save(self.root / 'RESULT.json', self.r)


def compare(plan_ref, oracle, candidate):
    plan, contract = load_reader_plan(plan_ref)
    left, left_closure = phase_observations(plan, contract, 'oracle', oracle)
    right, right_closure = phase_observations(plan, contract, 'candidate', candidate)
    rows = [dict(id=step['id'], kind=step['kind'],
                 status='PASS' if a[0] == b[0] else 'FAIL',
                 oracle_command=a[1], candidate_command=b[1])
            for step, a, b in zip(planned_observations(contract), left, right)]
    return dict(rows=rows, passed=sum(x['status'] == 'PASS' for x in rows),
                failed=sum(x['status'] == 'FAIL' for x in rows), plan=plan_ref,
                closure=dict(oracle=left_closure, candidate=right_closure),
                whole_criterion_closed=False)


def reader_main(argv):
    """Pure entrypoints: freeze prospective policy, or compare existing receipts."""
    p = argparse.ArgumentParser(description=reader_main.__doc__)
    sub = p.add_subparsers(dest='mode', required=True)
    freeze = sub.add_parser('plan')
    freeze.add_argument('contract', type=Path)
    freeze.add_argument('sha256')
    freeze.add_argument('oracle_root', type=Path)
    freeze.add_argument('candidate_root', type=Path)
    freeze.add_argument('out', type=Path)
    check = sub.add_parser('compare')
    check.add_argument('plan', type=Path)
    check.add_argument('sha256')
    check.add_argument('oracle', type=Path)
    check.add_argument('candidate', type=Path)
    check.add_argument('out', type=Path)
    args = p.parse_args(argv)
    require(not args.out.exists(), 'fresh reader output')
    if args.mode == 'plan':
        ref = pin(args.contract)
        require(ref['sha256'] == args.sha256, 'external contract SHA')
        contract = read_pinned(ref)
        planned_processes(contract)
        steps = planned_observations(contract)
        result = dict(schema=1, contract=ref, archive=False,
                      roots=dict(oracle=str(args.oracle_root.resolve()),
                                 candidate=str(args.candidate_root.resolve())),
                      policies={step['id']: step['success'] for step in steps},
                      executables={**{phase: os.path.realpath(item['path'])
                                      for phase, item in contract['binaries'].items()},
                                   'shell': os.path.realpath(contract['shell']['path'])})
        # Validate structure using the same gate before any native caller can use it.
        save(args.out, result)
        load_reader_plan(pin(args.out))
        print(json.dumps(pin(args.out)))
        return 0
    ref = pin(args.plan)
    require(ref['sha256'] == args.sha256, 'external reader plan SHA')
    try:
        plan, _ = load_reader_plan(ref)
        if plan.get('archive'):
            if set(plan.get('receipts', {})) != {'oracle', 'candidate'}:
                raise UnknownEvidence('archived receipt authentication missing')
            require(str(args.oracle) == plan['receipts']['oracle']['path'] and
                    str(args.candidate) == plan['receipts']['candidate']['path'],
                    'archived receipt path identity')
            # Authenticate the exact bytes subsequently parsed, not a separate read.
            oracle = read_pinned(plan['receipts']['oracle'])
            candidate = read_pinned(plan['receipts']['candidate'])
        else:
            oracle = json.loads(args.oracle.read_text())
            candidate = json.loads(args.candidate.read_text())
        result = compare(ref, oracle, candidate)
        result.update(status='RECOMPUTED', reader=pin(__file__),
                      archived_input_authentication=bool(plan.get('archive')),
                      new_native_execution=False)
        code = 0
    except UnknownEvidence as exc:
        result, code = dict(status='UNKNOWN', reason=str(exc), whole_criterion_closed=False), 2
    except (RuntimeError, KeyError, TypeError, ValueError) as exc:
        result, code = dict(status='INVALID', reason=str(exc), whole_criterion_closed=False), 1
    save(args.out, result)
    print(json.dumps({k: v for k, v in result.items() if k not in ('rows', 'closure')}))
    return code


def main(argv):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('contract', type=Path)
    p.add_argument('phase', choices=['oracle', 'candidate'])
    p.add_argument('out', type=Path)
    p.add_argument('--oracle', type=Path)
    p.add_argument('--reader-plan', type=Path, required=True)
    p.add_argument('--reader-sha256', required=True)
    args = p.parse_args(argv)
    require(sys.platform == 'linux', 'Linux profile only')
    plan_ref = pin(args.reader_plan)
    require(plan_ref['sha256'] == args.reader_sha256, 'external reader plan SHA')
    reader, contract = load_reader_plan(plan_ref)
    require(not reader.get('archive'), 'archive plans cannot launch products')
    require(reader['contract'] == pin(args.contract), 'native contract binding')
    require(reader['roots'][args.phase] == str(args.out.resolve()), 'native root binding')
    for item in [*contract['binaries'].values(), contract['shell'], *contract['harness'].values()]:
        authenticate(item)
    if args.phase == 'candidate':
        require(args.oracle is not None, 'frozen oracle required')
        frozen = json.loads(args.oracle.read_text())
        authenticate(frozen['receipt'])
        require(frozen['contract'] == pin(args.contract), 'same frozen contract')
    args.out.mkdir(parents=True, exist_ok=False)
    run = LinuxPhase(contract, args.phase, args.out.resolve())
    run.r['contract'] = pin(args.contract)
    run.r['native_os'] = sys.platform
    try:
        run.run()
    except Exception as exc:
        run.r['error'] = str(exc)
    finally:
        run.close()
    if run.r['status'] == 'EXECUTED' and 'independent_corpus' in contract:
        phase_observations(reader, contract, args.phase, run.r)
    if args.phase == 'candidate' and run.r['status'] == 'EXECUTED':
        paired = compare(plan_ref, json.loads(Path(frozen['receipt']['path']).read_text()), run.r)
        save(args.out / 'COMPARISON.json', paired)
        if paired['failed']:
            return 1
    print(json.dumps(dict(status=run.r['status'], closure=run.r['closure'],
                         commands=len(run.r['commands']), observations=len(run.r['observations']))))
    return 0 if run.r['status'] == 'EXECUTED' else 1
