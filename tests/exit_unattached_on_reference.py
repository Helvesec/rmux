#!/usr/bin/env python3
"""Linux exit-unattached with exit-empty ON, one oracle and one candidate lot.
prepare CONFIG creates fixtures/pure controls; run CONFIG oracle|candidate is
single use per product. Complete oracle A+B gates candidate. First candidate
mismatch stops before B. No runtime changes, attached client or native retry.
Twenty-second observation is followed by a separate bounded proof transition.
OBSERVED.json is immutable before cleanup; cleanup exits never count as effects.
"""
import base64
import copy
import ctypes
import datetime
import hashlib
import json
import math
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

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{session_attached}|#{session_name}|#{window_index}|#{window_name}'
PUBLIC_KEYS = ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'session_attached', 'session_name', 'window_index', 'window_name')
KINDS={'alpha':'alpha'}
CODES={'alpha':0}
GENERATIONS=('A','B')
ENV_KEYS=('HOME','XDG_CONFIG_HOME','PATH','L6_EXITON_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM')
SESSION_FORMAT='#{session_id}|#{session_name}|#{session_attached}|#{session_windows}'

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
    try:return _proc(pid)
    except PermissionError as error:return {'at':now(),'coherent':False,'read_error':repr(error)}

def complete(p):
    return bool(p and p.get('coherent') and p.get('after') and p['before']['state'] not in ('X','Z') and p['after']['state'] not in ('X','Z'))

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



def environment(config, phase, gen):
    root = Path(config['root']) / phase / gen
    return {'HOME': str(root / 'home-empty'), 'XDG_CONFIG_HOME': str(root / 'xdg-empty'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/dash', 'TERM': 'vt100', 'LANG': 'C.UTF-8',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_EXITON_MARKER': config['tokens'][phase][gen]}

def witness_argv(config, phase, gen, label):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', KINDS[label],
            str(Path(config['root']) / phase / gen / 'markers'), config['tokens'][phase][gen], str(CODES[label])]

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
    require(not same(before, after) or after['state'] in ('X','Z'), 'exited child still live')


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

def compare(actual,expected):
    equal(actual,expected,'native effect differs from frozen oracle')

def oracle_gate(record,sha):
    equal(record['status'],'PASS','oracle must be complete')
    equal([g['generation'] for g in record['generations']],list(GENERATIONS),'both oracle generations')
    for g in record['generations']:
        equal(g['status'],'PASS','oracle generation');equal(g['cleanup']['status'],'PASS','oracle cleanup')
        equal(g['spawn_commands'],1,'one helper per generation')
        equal({e['label'] for e in g['exits']},{'alpha','daemon'},'all owned exits')
        for e in g['exits']:
            check_kernel_exit(e['original_identity'],e['identity_after_exit_event'],e['pidfd'])
            require('closed_at' in e['pidfd'],'closed exit pidfd required')
    return {'at':now(),'status':'PASS','driver_sha256':digest(__file__),'result_sha256':sha,
            'generations':{g['generation']:g['comparison'] for g in record['generations']}}

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
    for state in ('X','Z'):
        bad=copy.deepcopy(child);bad['after']['state']=state
        rejected('nonlive_'+state,lambda bad=bad:require(complete(bad),'not live'))
    saved=globals()['Path']
    class Denied:
        def __init__(self,*args):pass
        def read_text(self):raise PermissionError(13,'pure denial')
    try:
        globals()['Path']=Denied;value=proc(41)
        require(not complete(value) and 'PermissionError' in value['read_error'],'denial must not become live');checks.append('actual_denial_preserved')
    finally:globals()['Path']=saved
    expected_effect={'daemon':'exited','helper':'exited','listener':'absent'}
    compare(copy.deepcopy(expected_effect),expected_effect);checks.append('effect_exact')
    rejected('surviving_daemon_mismatch',lambda:compare({**expected_effect,'daemon':'alive_after_20s'},expected_effect))
    rejected('helper_survival_mismatch',lambda:compare({**expected_effect,'helper':'alive'},expected_effect))
    rejected('listener_still_responds',lambda:compare({**expected_effect,'listener':'responding'},expected_effect))
    values={'nested':{'before':child}}
    independent=copy.deepcopy(values);independent['nested']['before']['before']['start_ticks']='changed'
    equal(values['nested']['before']['before']['start_ticks'],'410','deep copy');checks.append('deep_snapshot_independent')
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
    return {'at': now(), 'status': 'PASS', 'checks': checks, 'driver_sha256': digest(__file__), 'product_runs': 0}



class Run:
    def __init__(self,config,phase,gen,start,expected):
        self.config,self.phase,self.gen,self.start,self.expected_oracle=config,phase,gen,start,expected
        self.root=Path(config['root'])/phase/gen
        self.binary,self.socket=config[phase],self.root/'s';self.env=environment(config,phase,gen)
        self.case_end=start+150;self.cleaning=False;self.daemon=self.socket_identity=None
        self.owned,self.children,self.handles={},{},{};self.spawn_count=0
        self.record={'phase':phase,'generation':gen,'start':now(),'commands':[],'observations':[],
            'environment':self.env,'public_samples':[],'profiles':[],'marker_ledgers':[],
            'exits':[],'inventories':[],'options':[],'mutations':[],'held_pidfds':{},'transitions':[],'endpoint_probes':[]}

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
        require(size < 1024**2, 'product log cap')
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
                'argv': witness_argv(self.config, self.phase, self.gen, label), 'cwd': str(self.root / 'cwd'),
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
            equal(public['session_attached'], '0', 'no attached client')
            require(len(self.record['observations']) <= 4, 'four challenges per generation maximum')
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

    def transition(self,name,operation):
        self.case_end=min(time.monotonic()+20,self.start+150)
        item={'name':name,'start':now(),'nominal_budget_seconds':20};self.record['transitions'].append(item)
        begin=time.monotonic()
        try:return operation()
        finally:item.update(end=now(),elapsed_seconds=time.monotonic()-begin)

    def hold(self,label,snapshot):
        require(complete(snapshot),'qualified live snapshot required');before=snapshot['before']
        fd=os.pidfd_open(before['pid'],0);poller=select.poll();poller.register(fd,select.POLLIN)
        meta={'fd':fd,'opened_at':now(),'original_identity':copy.deepcopy(before),'identity_after_open':identity(before['pid']),'events_before_request':poller.poll(0)}
        self.handles[label]=(fd,poller,meta);self.record['held_pidfds'][label]=copy.deepcopy(meta)
        require(same(before,meta['identity_after_open']) and not meta['events_before_request'],'pidfd stale or exited')

    def mutate(self,purpose,args):
        before={label:{'identity':identity(meta['original_identity']['pid']),'events':poller.poll(0)} for label,(fd,poller,meta) in self.handles.items()}
        for label,item in before.items():require(same(item['identity'],self.handles[label][2]['original_identity']) and not item['events'],'identity changed before mutation')
        index=len(self.record['commands']);self.command(args,required=False);command=self.record['commands'][index]
        item={'purpose':purpose,'args':args if args[0]!='source-file' else ['source-file','PRIVATE/live.conf'],'command_index':index,'before':before,
              **{k:command[k] for k in ('start','end','exit_code','stdout','stderr')}}
        self.record['mutations'].append(copy.deepcopy(item));return index

    def options(self,point):
        item={'point':point,'at':now(),'command_indices':[]};self.record['options'].append(item)
        for name in ('exit-empty','exit-unattached'):
            item['command_indices'].append(len(self.record['commands']));item[name]=self.command(['show-options','-sv',name])
        return copy.deepcopy(item)

    def sample(self):
        i=len(self.record['commands']);sessions=self.command(['list-sessions','-F',SESSION_FORMAT])
        j=len(self.record['commands']);panes=self.command(['list-panes','-a','-F',FORMAT])
        item={'at':now(),'command_indices':[i,j],'sessions_raw':sessions,'panes_raw':panes,
              'daemon':proc(self.daemon['before']['pid']),'endpoint':self.socket_info(),
              'pidfd_events':{label:poller.poll(0) for label,(fd,poller,meta) in self.handles.items()}}
        require(snapshot_same(self.daemon,item['daemon']),'daemon changed');equal(item['endpoint'],self.socket_identity,'endpoint replaced')
        require(all(not e for e in item['pidfd_events'].values()),'owned process exited during live inventory')
        child=self.children['alpha']['public']
        equal(sessions,f"{child['session_id']}|alpha|0|1",'one detached session')
        equal([parse_public(line) for line in panes.splitlines()],[child],'exact original pane')
        return item

    def inventory(self,point,challenges):
        entry={'point':point,'at':now(),'samples':[],'challenges':challenges,'status':'UNPROVEN'};self.record['inventories'].append(entry)
        entry['samples'].append(self.sample())
        for _ in range(challenges):self.observe('alpha',self.children['alpha']['target'],True)
        entry['samples'].append(self.sample())
        equal(entry['samples'][0]['sessions_raw'],entry['samples'][1]['sessions_raw'],'stable sessions')
        equal(entry['samples'][0]['panes_raw'],entry['samples'][1]['panes_raw'],'stable panes')
        entry['status']='PASS';return copy.deepcopy(entry)

    def setup(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh fixture')
        self.profiles();self.spawn_count=1
        target=self.command(['new-session','-d','-P','-F','#{pane_id}','-s','alpha','-n','alpha','-x','80','-y','24','-c',str(self.root/'cwd'),*witness_argv(self.config,self.phase,self.gen,'alpha')],startup=True)
        require(target.startswith('%') and target[1:].isdigit(),'pane ID required')
        child=self.observe('alpha',target);self.hold('daemon',self.daemon);self.hold('alpha',child['child'])
        if self.gen=='B':
            index=len(self.record['commands']);self.record['default']=self.command(['show-options','-sv','exit-unattached']);self.record['default_command_index']=index
            require(self.record['default'] in ('off','on'),'invalid default')
            index=self.mutate('establish_off',['set-option','-s','exit-unattached','off'])
            require(self.record['commands'][index]['exit_code']==0,'off override failed')
        values=self.options('before_on');equal(values['exit-empty'],'on','fixed exit-empty on');equal(values['exit-unattached'],'off','initial off')
        self.inventory('before_on',1);self.ledger()

    def record_exit(self,label,cause,trigger_index):
        fd,poller,meta=self.handles[label];before=meta['original_identity']
        event={'label':label,'cause':cause,'at':now(),'trigger_command_index':trigger_index,'original_identity':copy.deepcopy(before),'pidfd':copy.deepcopy(meta),'status':'UNPROVEN'}
        self.record['exits'].append(event)
        try:
            event['pidfd'].update(events_after_request=poller.poll(max(1,int(self.remaining()*1000))),observed_at=now())
            event['identity_after_exit_event']=identity(before['pid']);event['proc_after']=proc(before['pid'])
            check_kernel_exit(before,event['identity_after_exit_event'],event['pidfd']);event['status']='PASS'
        finally:
            os.close(fd);event['pidfd']['closed_at']=now();self.record['held_pidfds'][label]['closed_at']=event['pidfd']['closed_at'];del self.handles[label]

    def endpoint_after_exit(self,cause):
        item={'at':now(),'cause':cause,'pathname':self.socket_info()};self.record['endpoint_probes'].append(item)
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as connection:
            connection.settimeout(min(1,self.remaining()))
            try:connection.connect(str(self.socket));item.update(accepting=True,errno=None)
            except OSError as error:item.update(accepting=False,errno=error.errno,error=repr(error))
        require(not item['accepting'] and item['errno'] in (errno.ENOENT,errno.ECONNREFUSED),'endpoint absence not qualified')
        item.update(listener_absent=True,pathname_absent=item['pathname'] is None);return copy.deepcopy(item)

    def activate(self):
        arguments=['set-option','-s','exit-unattached','on'] if self.gen=='A' else ['source-file',str(self.root/'live.conf')]
        index=self.transition('activation_command',lambda:self.mutate('activate',arguments))
        poll={'start':now(),'nominal_seconds':20,'trigger_command_index':index};self.record['activation_observation']=poll
        begin=time.monotonic();self.case_end=min(begin+20,self.start+150)
        poll['events']=self.handles['daemon'][1].poll(max(1,math.ceil((self.case_end-begin)*1000)))
        poll.update(end=now(),elapsed_seconds=time.monotonic()-begin)
        def prove():
            if poll['events']:
                for label in ('alpha','daemon'):self.record_exit(label,'option_activation',index)
                endpoint=self.endpoint_after_exit('option_activation')
                logical={'daemon':'exited','helper':'exited','listener':'absent'}
            else:
                require(poll['elapsed_seconds']>=20,'incomplete survival observation')
                self.inventory('after_20s',2);values=self.options('after_20s')
                endpoint={'at':now(),'pathname':self.socket_info(),'listener_responding_public_commands':True}
                logical={'daemon':'alive_after_20s','helper':'alive','listener':'responding','exit-empty':values['exit-empty'],'exit-unattached':values['exit-unattached']}
            self.record['effect']={'at':now(),'logical':logical,'endpoint':endpoint}
        self.transition('post_observation_proof',prove)
        self.record['comparison']={'default':self.record.get('default'),'mutations':[{k:m[k] for k in ('purpose','args','exit_code','stdout','stderr')} for m in self.record['mutations']],'effect':self.record['effect']['logical']}
        self.record['status']='PASS'
        if self.phase=='candidate':
            try:compare(self.record['comparison'],self.expected_oracle)
            except Failed as error:self.record.update(status='RUNTIME_MISMATCH',mismatch=str(error),expected=copy.deepcopy(self.expected_oracle))
        self.record['proof_sealed_at']=now()
        write_once(self.root/'OBSERVED.json',copy.deepcopy(self.record))

    def execute(self):
        self.transition('setup',self.setup);self.activate();self.profiles()

    def ledger(self):
        entries = [{'path': str(p), 'data': json.loads(p.read_text())}
                   for p in sorted((self.root / 'markers').glob('born-*.json'))]
        self.record['marker_ledgers'].append({'at': now(), 'entries': entries})
        equal(len(entries), self.spawn_count, 'unexpected witness execution count')
        equal({entry['path'] for entry in entries}, {c['born_path'] for c in self.children.values()},
              'unattributed witness marker')

    def _cleanup_owned(self):
        start = time.monotonic()
        self.cleaning = True
        self.case_end = min(self.cleanup_end, self.start + 180)
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

    def cleanup(self):
        begin=time.monotonic();self.cleaning=True;self.cleanup_end=min(begin+15,self.start+180);self.case_end=self.cleanup_end
        try:
            if self.daemon and snapshot_same(self.daemon,proc(self.daemon['before']['pid'])) and self.socket_info()==self.socket_identity:
                if 'daemon' not in self.handles:self.hold('daemon',self.daemon)
                index=len(self.record['commands']);self.command(['kill-server'])
                for label in ('alpha','daemon'):
                    if label in self.handles:self.record_exit(label,'final_cleanup',index)
                self.endpoint_after_exit('final_cleanup')
        finally:
            try:self._cleanup_owned()
            finally:
                for label,(fd,poller,meta) in list(self.handles.items()):
                    os.close(fd);self.record['held_pidfds'][label]['closed_at']=now();del self.handles[label]
                self.record['cleanup_total_seconds']=time.monotonic()-begin
        require(self.record['cleanup_total_seconds']<=15,'cleanup budget')


def prepare(config):
    root=Path(config['root']);require(root.is_dir(),'fixture root required')
    for phase in ('oracle','candidate'):
        for gen in GENERATIONS:
            p=root/phase/gen;p.mkdir(parents=True)
            for name in ('home-empty','xdg-empty','markers','cwd'):(p/name).mkdir()
            startup="set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-option -s exit-empty on\n"
            if gen=='A':startup+='set-option -s exit-unattached off\n'
            (p/'startup.conf').write_text(startup);(p/'live.conf').write_text('set-option -s exit-unattached on\n')
    write_once(root/'PLAN.json',{'generations':GENERATIONS,'alpha_per_generation':1,'transition_seconds':20,'native_phase_seconds':180,'cleanup_seconds':30,
        'observation':'A full nominal20s daemon pidfd poll, then separate <=20s proof transition if still alive; actual poll scheduling duration retained.',
        'stop':'Candidate first mismatch seals OBSERVED then cleanup then stop before B. No repeat native run.',
        'challenges_per_product_max':8,'child_snapshots_per_product_max':16,'oracle_complete_before_candidate':True})
    write_once(root/'PURE-CHECKS.json',pure_checks(config))


def execute_phase(config,config_path,phase):
    root=Path(config['root']);frozen=json.loads((root/'FROZEN-NATIVE.json').read_text())
    artifacts={v['path']:v['sha256'] for v in frozen['artifacts']}
    mandatory=[str(Path(__file__).resolve()),str(Path(config_path).resolve()),str(root/'PLAN.json'),str(root/'PURE-CHECKS.json')]
    mandatory += [config[k] for k in ('oracle','candidate','daemon','python')]
    mandatory += [str(root/p/g/f) for p in ('oracle','candidate') for g in GENERATIONS for f in ('startup.conf','live.conf')]
    require(all(p in artifacts for p in mandatory),'freeze incomplete')
    for path,sha in artifacts.items():require(digest(path)==sha,'frozen artifact changed '+path)
    pure=json.loads((root/'PURE-CHECKS.json').read_text());require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure controls stale')
    expected={}
    if phase=='candidate':
        gate=json.loads((root/'ORACLE-FROZEN.json').read_text())
        require(gate['status']=='PASS' and gate['driver_sha256']==digest(__file__),'oracle gate invalid')
        require(digest(root/'oracle/RESULT.json')==gate['result_sha256'],'oracle proof changed');expected=gate['generations']
    require(ctypes.CDLL(None,use_errno=True).prctl(36,1,0,0,0)==0,'subreaper unavailable')
    write_once(root/phase/'STARTED.json',{'at':now(),'identity':proc(os.getpid())})
    start=time.monotonic();record={'phase':phase,'start':now(),'generations':[],'fd_before':fd_snapshot(),'frozen_sha256':digest(root/'FROZEN-NATIVE.json')}
    for gen in GENERATIONS:
        run=Run(config,phase,gen,start,expected.get(gen))
        try:run.execute()
        except Exception as error:run.record.update(status='UNPROVEN',error=str(error),traceback=traceback.format_exc())
        finally:
            if not (run.root/'OBSERVED.json').exists():write_once(run.root/'OBSERVED.json',copy.deepcopy(run.record))
            try:run.cleanup()
            except Exception as error:run.record.update(status='UNPROVEN',cleanup_error=repr(error),cleanup_traceback=traceback.format_exc())
            run.record.update(end=now(),spawn_commands=run.spawn_count,observed_sha256=digest(run.root/'OBSERVED.json'))
            write_once(run.root/'RESULT.json',run.record)
        record['generations'].append(copy.deepcopy(run.record))
        if run.record['status']!='PASS':break
    record['status']='PASS' if len(record['generations'])==2 and all(g['status']=='PASS' for g in record['generations']) else record['generations'][-1]['status']
    record.update(end=now(),elapsed_seconds=time.monotonic()-start,fd_after=fd_snapshot())
    if record['fd_before']!=record['fd_after'] or record['elapsed_seconds']>180:record['status']='UNPROVEN'
    write_once(root/phase/'RESULT.json',record)
    if phase=='oracle' and record['status']=='PASS':write_once(root/'ORACLE-FROZEN.json',oracle_gate(record,digest(root/phase/'RESULT.json')))
    bytes_used=sum(p.stat().st_size for p in root.rglob('*') if p.is_file());require(bytes_used<4*1024**2,'cumulative log cap')
    print(json.dumps({'phase':phase,'status':record['status'],'result_sha256':digest(root/phase/'RESULT.json'),'elapsed_seconds':record['elapsed_seconds'],'log_bytes':bytes_used,'generations':len(record['generations'])}),flush=True)
    return 0 if record['status']=='PASS' else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness':witness(*sys.argv[2:6])
    elif len(sys.argv)==3 and sys.argv[1]=='prepare':prepare(json.loads(Path(sys.argv[2]).read_text()))
    elif len(sys.argv)==4 and sys.argv[1]=='run' and sys.argv[3] in ('oracle','candidate'):
        sys.exit(execute_phase(json.loads(Path(sys.argv[2]).read_text()),sys.argv[2],sys.argv[3]))
    else:raise SystemExit(__doc__)
