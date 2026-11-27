#!/usr/bin/env python3
"""Bounded native destroy-unattached reference (D1-D4).
Prepare CONFIG, freeze driver/config/fixtures/pins, then run CONFIG once on Linux.
One private daemon and three detached witness sessions per product. A complete
immutable tmux oracle gates RMUX. Actual session survivors, option scopes and
kernel exits are measured; pane closure/kill-session is never a scenario trigger.
Only final cleanup uses the qualified private kill-server command.
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

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{session_attached}|#{session_name}|#{window_index}|#{window_name}'
PUBLIC_KEYS = ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'session_attached', 'session_name', 'window_index', 'window_name')
SESSION_FORMAT = '#{session_id}|#{session_name}|#{session_attached}|#{session_windows}'
SESSION_KEYS = ('session_id','session_name','session_attached','session_windows')
KINDS = {label:label for label in ('alpha','beta','gamma')}
CODES = {label:0 for label in KINDS}
CASE_LABELS = {'D1':('alpha','beta','gamma'),'D2':('beta','gamma'),'D3':('beta',),'D4':()}
ENV_KEYS = ('HOME','XDG_CONFIG_HOME','PATH','L6_DESTROY_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM')

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
    try: return _proc(pid)
    except PermissionError as error:
        return {'at':now(),'coherent':False,'read_error':repr(error)}

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



def environment(config, phase):
    root = Path(config['root']) / phase
    return {'HOME': str(root / 'home-empty'), 'XDG_CONFIG_HOME': str(root / 'xdg-empty'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/dash', 'TERM': 'vt100', 'LANG': 'C.UTF-8',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_DESTROY_MARKER': config['tokens'][phase]}

def witness_argv(config, phase, label):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', KINDS[label],
            str(Path(config['root']) / phase / 'markers'), config['tokens'][phase], str(CODES[label])]

def plan(config, phase):
    cwd=str(Path(config['root'])/phase/'cwd')
    return {label:['new-session','-d','-P','-F','#{pane_id}','-s',label,'-n',label,'-x','80','-y','24','-c',cwd,*witness_argv(config,phase,label)] for label in KINDS}

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

def parse_sessions(raw):
    rows=[]
    for line in raw.splitlines():
        fields=line.split('|');require(len(fields)==4,'invalid session inventory')
        row=dict(zip(SESSION_KEYS,fields))
        for key in ('session_attached','session_windows'):
            require(row[key].isdigit(),'invalid session counter');row[key]=int(row[key])
        rows.append(row)
    return sorted(rows,key=lambda row:row['session_name'])

def check_inventory(rows, children):
    labels=[r['session_name'] for r in rows]
    require(len(labels)==len(set(labels)) and set(labels)<=set(children),'extra or duplicated session')
    require(len({r['session_id'] for r in rows})==len(rows),'duplicated session ID')
    for row in rows:
        child=children[row['session_name']]
        equal(row['session_id'],child['public']['session_id'],'session identity replaced')
        equal(row['session_attached'],0,'attached client outside contract')
        equal(row['session_windows'],1,'unexpected window count')
    return labels

def check_scopes(actual, global_value, locals_by_label):
    equal(actual['global'],global_value,'global destroy-unattached')
    equal(actual['exit_empty'],'off','server exit-empty off')
    equal(set(actual['sessions']),set(locals_by_label),'scope coverage')
    for label,local in locals_by_label.items():
        equal(actual['sessions'][label]['local'],'' if local is None else local,'local '+label)
        equal(actual['sessions'][label]['effective'],global_value if local is None else local,'effective '+label)


def oracle_gate(record, sha):
    equal(record['status'],'PASS','complete oracle required')
    equal([c['id'] for c in record['cases']],list(CASE_LABELS),'all oracle cases')
    require(all(c['status']=='PASS' for c in record['cases']),'oracle case failed')
    equal(record['spawn_commands'],3,'exact oracle population')
    equal(record['cleanup']['status'],'PASS','oracle cleanup')
    equal([v['case'] for v in record['inventories']],list(CASE_LABELS),'all oracle inventories')
    for e in record['exits']:
        equal(e['status'],'PASS','helper exit failure')
        check_kernel_exit(e['original_child']['before'],e['identity_after_exit_event'],e['pidfd'])
        require('closed_at' in e['pidfd'],'helper pidfd unclosed')
    require('closed_at' in record['daemon_exit']['pidfd'],'daemon pidfd unclosed')
    return {'at':now(),'status':'PASS','driver_sha256':digest(__file__),'result_sha256':sha,
            'survivors':{i['case']:i['survivors'] for i in record['inventories']},
            'scope_observations':[{k:v for k,v in item.items() if k not in ('at','command_indices')} for item in record['scopes']],
            'mutation_outcomes':[{k:m[k] for k in ('case','args','exit_code','stdout','stderr')} for m in record['mutations']],
            'projected_sequence_satisfied':all(i['survivors']==list(CASE_LABELS[i['case']]) for i in record['inventories'])}


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
    rejected('denied_proc_not_live',lambda:require(complete({'coherent':False,'read_error':'PermissionError'}),'not live'))
    saved=globals()['Path']
    class Denied:
        def __init__(self,*args):pass
        def read_text(self):raise PermissionError(13,'pure denial')
    try:
        globals()['Path']=Denied;value=proc(41)
        require(value['coherent'] is False and 'PermissionError' in value['read_error'],'denial lost');checks.append('proc_denial_preserved')
    finally:globals()['Path']=saved
    children={'alpha':{'public':{'session_id':'$0'}},'beta':{'public':{'session_id':'$1'}}}
    rows=[{'session_id':'$0','session_name':'alpha','session_attached':0,'session_windows':1},{'session_id':'$1','session_name':'beta','session_attached':0,'session_windows':1}]
    equal(check_inventory(rows,children),['alpha','beta'],'positive inventory');checks.append('inventory_exact')
    for key,value in [('session_id','$9'),('session_name','alien'),('session_attached',1),('session_windows',2)]:
        bad=copy.deepcopy(rows);bad[0][key]=value
        rejected('inventory_wrong_'+key,lambda bad=bad:check_inventory(bad,children))
    rejected('inventory_duplicate',lambda:check_inventory(rows+[rows[0]],children))
    scope={'global':'on','exit_empty':'off','sessions':{'beta':{'local':'off','effective':'off'},'gamma':{'local':'','effective':'on'}}}
    check_scopes(scope,'on',{'beta':'off','gamma':None});checks.append('scope_mask_and_inheritance')
    for label,key,value in [('beta','effective','on'),('gamma','effective','off'),('gamma','local','on')]:
        bad=copy.deepcopy(scope);bad['sessions'][label][key]=value
        rejected('bad_scope_'+label+'_'+key,lambda bad=bad:check_scopes(bad,'on',{'beta':'off','gamma':None}))
    saved_scope=copy.deepcopy(scope);bad=copy.deepcopy(scope);bad['sessions']['beta']['local']='on'
    equal(scope,saved_scope,'deep snapshot independence');checks.append('nested_snapshot_independence')
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
    def __init__(self,config,phase,start):
        self.config,self.phase,self.start=config,phase,start
        self.root=Path(config['root'])/phase
        self.binary,self.socket=config[phase],self.root/'s'
        self.env=environment(config,phase)
        self.case_end=start+150;self.cleaning=False
        self.daemon=self.socket_identity=None
        self.owned,self.children={},{}
        self.spawn_count=0;self.handles={};self.exited=set();self.alive=[]
        self.record={'phase':phase,'start':now(),'commands':[],'cases':[],'observations':[],
                     'environment':self.env,'public_samples':[],'profiles':[],'marker_ledgers':[],
                     'exits':[],'inventories':[],'scopes':[],'mutations':[],'held_pidfds':{}}
        self.oracle={} if phase=='oracle' else json.loads((Path(config['root'])/'ORACLE-FROZEN.json').read_text())

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
        require(size < 4 * 1024**2, 'product log cap')
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
            equal(public['session_attached'], '0', 'detached session')
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

    def hold(self,label,child):
        before=child['before'];require(complete(child),'pidfd needs qualified live snapshot')
        fd=os.pidfd_open(before['pid'],0);poller=select.poll();poller.register(fd,select.POLLIN)
        meta={'fd':fd,'opened_at':now(),'original_identity':copy.deepcopy(before),
              'identity_after_open':identity(before['pid']),'events_before_request':poller.poll(0)}
        self.handles[label]=(fd,poller,meta)
        self.record['held_pidfds'][label]=copy.deepcopy(meta)
        require(same(before,meta['identity_after_open']) and not meta['events_before_request'],'pidfd stale or already exited')

    def spawn(self,label):
        require(label not in self.children and self.spawn_count<3,'population cap')
        self.spawn_count+=1
        target=self.command(plan(self.config,self.phase)[label],startup=label=='alpha')
        require(target.startswith('%') and target[1:].isdigit(),'stable pane ID required')
        item=self.observe(label,target)
        if 'daemon' not in self.handles:self.hold('daemon',self.daemon)
        self.hold(label,item['child']);self.alive.append(label)

    def mutate(self,case,args):
        for label in ['daemon',*self.alive]:
            require(not self.handles[label][1].poll(0),'owned process exited before mutation '+label)
        index=len(self.record['commands']);self.command(args,required=False)
        command=self.record['commands'][index]
        item={'case':case,'at':now(),'args':args if args[0]!='source-file' else ['source-file','PRIVATE/live.conf'],
              'command_index':index,**{k:command[k] for k in ('exit_code','stdout','stderr','start','end')}}
        self.record['mutations'].append(copy.deepcopy(item))
        if self.phase=='candidate':
            expected=next(m for m in self.oracle['mutation_outcomes'] if m['case']==case)
            equal({k:item[k] for k in expected},expected,'exact mutation outcome against oracle')
        require(command['exit_code']==0,'mutation failed')
        return index

    def scopes(self,case,point,global_value,locals_by_label):
        item={'case':case,'point':point,'at':now(),'command_indices':[],'sessions':{}}
        self.record['scopes'].append(item)
        def query(args):
            item['command_indices'].append(len(self.record['commands']));return self.command(args)
        item['global']=query(['show-options','-gv','destroy-unattached'])
        item['exit_empty']=query(['show-options','-sv','exit-empty'])
        for label in locals_by_label:
            item['sessions'][label]={'local':query(['show-options','-qv','-t',label,'destroy-unattached']),
                                     'effective':query(['show-options','-Av','-t',label,'destroy-unattached'])}
        check_scopes(item,global_value,locals_by_label)
        if self.phase=='candidate':
            expected=next(s for s in self.oracle['scope_observations'] if s['case']==case and s['point']==point)
            equal({k:v for k,v in item.items() if k not in ('at','command_indices')},expected,'exact option scope oracle')
        return copy.deepcopy(item)

    def inventory_sample(self):
        index=len(self.record['commands']);raw=self.command(['list-sessions','-F',SESSION_FORMAT])
        sample={'at':now(),'command_index':index,'raw':raw,'sessions':parse_sessions(raw),
                'daemon':proc(self.daemon['before']['pid']),'endpoint':self.socket_info(),
                'daemon_pidfd_events':self.handles['daemon'][1].poll(0)}
        require(snapshot_same(self.daemon,sample['daemon']),'daemon generation changed')
        equal(sample['endpoint'],self.socket_identity,'private endpoint replaced')
        require(not sample['daemon_pidfd_events'],'daemon exited with exit-empty off')
        check_inventory(sample['sessions'],self.children)
        return sample

    def record_exit(self,label,case,trigger_index):
        child=self.children[label];fd,poller,meta=self.handles[label]
        event={'label':label,'case':case,'at':now(),'trigger_command_index':trigger_index,
               'original_public':copy.deepcopy(child['public']),'original_child':copy.deepcopy(child['child']),
               'pidfd':copy.deepcopy(meta),'status':'UNPROVEN'}
        self.record['exits'].append(event)
        try:
            event['pidfd'].update(events_after_request=poller.poll(max(1,int(self.remaining()*1000))),observed_at=now())
            event['identity_after_exit_event']=identity(child['child']['before']['pid'])
            check_kernel_exit(child['child']['before'],event['identity_after_exit_event'],event['pidfd'])
            event['status']='PASS';self.exited.add(label)
        finally:
            os.close(fd);event['pidfd']['closed_at']=now()
            self.record['held_pidfds'][label]['closed_at']=event['pidfd']['closed_at'];del self.handles[label]

    def checkpoint(self,case,target=None,trigger_index=None):
        item={'case':case,'at':now(),'samples':[],'status':'UNPROVEN'}
        self.record['inventories'].append(item)
        if target is not None and target in self.handles:
            item['target_exit_poll']={'label':target,'events':self.handles[target][1].poll(max(1,int(min(3,self.remaining())*1000))),'at':now()}
        first=self.inventory_sample();item['samples'].append(copy.deepcopy(first))
        survivors=check_inventory(first['sessions'],self.children)
        require(set(survivors)<=set(self.alive),'destroyed session resurrected')
        removed=[label for label in self.alive if label not in survivors]
        for label in removed:self.record_exit(label,case,trigger_index)
        for label in survivors:
            require(not self.handles[label][1].poll(0),'listed survivor has exited')
            self.observe(label,self.children[label]['target'],True)
        time.sleep(min(.05,self.remaining()))
        second=self.inventory_sample();item['samples'].append(copy.deepcopy(second))
        equal(second['sessions'],first['sessions'],'two inventories unstable')
        for label in survivors:require(not self.handles[label][1].poll(0),'survivor exited during checkpoint')
        if self.phase=='candidate':equal(survivors,self.oracle['survivors'][case],'exact oracle survivor names')
        if case=='D1':equal(survivors,list(KINDS),'initial three detached sessions')
        if case=='D2':require({'beta','gamma'}<=set(survivors),'local alpha changed neighbors')
        if case=='D3':require('beta' in survivors,'global on bypassed local beta off')
        self.alive=survivors
        item.update(survivors=survivors,removed=removed,status='PASS')
        return copy.deepcopy(item)

    def execute(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh fixture required')
        self.profiles()
        for case in CASE_LABELS:
            self.case_end=min(time.monotonic()+20,self.start+150)
            entry={'id':case,'start':now(),'monotonic_start':time.monotonic()}
            self.record['cases'].append(entry)
            try:
                if case=='D1':
                    for label in KINDS:self.spawn(label)
                    self.scopes(case,'before_beta_override','off',{k:None for k in KINDS})
                    self.mutate(case,['set-option','-t','beta','destroy-unattached','off'])
                    self.checkpoint(case)
                    self.scopes(case,'after','off',{'alpha':None,'beta':'off','gamma':None})
                elif case=='D2':
                    self.scopes(case,'before','off',{label:('off' if label=='beta' else None) for label in self.alive})
                    index=self.mutate(case,['set-option','-t','alpha','destroy-unattached','on'])
                    self.checkpoint(case,'alpha',index)
                    self.scopes(case,'after','off',{label:('on' if label=='alpha' else 'off' if label=='beta' else None) for label in self.alive})
                elif case=='D3':
                    self.scopes(case,'before','off',{label:('on' if label=='alpha' else 'off' if label=='beta' else None) for label in self.alive})
                    index=self.mutate(case,['set-option','-g','destroy-unattached','on'])
                    self.checkpoint(case,'gamma',index)
                    self.scopes(case,'after','on',{label:('on' if label=='alpha' else 'off' if label=='beta' else None) for label in self.alive})
                else:
                    self.scopes(case,'before','on',{label:('on' if label=='alpha' else 'off' if label=='beta' else None) for label in self.alive})
                    index=self.mutate(case,['source-file',str(self.root/'live.conf')])
                    self.checkpoint(case,'beta',index)
                    self.scopes(case,'after','on',{label:('on' if label=='alpha' else None) for label in self.alive})
                    entry['all_sessions_gone']=not self.alive
                self.ledger();self.profiles();self.remaining();entry['status']='PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error));raise
            finally:entry.update(end=now(),elapsed_seconds=time.monotonic()-entry['monotonic_start'])

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
                for label in list(self.handles):
                    if label!='daemon':self.record_exit(label,'CLEANUP',index)
                fd,poller,meta=self.handles['daemon']
                item={'at':now(),'trigger_command_index':index,'pidfd':copy.deepcopy(meta),'status':'UNPROVEN'}
                self.record['daemon_exit']=item
                item['pidfd'].update(events_after_request=poller.poll(max(1,int(self.remaining()*1000))),observed_at=now())
                item['identity_after_exit_event']=identity(self.daemon['before']['pid'])
                item['proc_after']=proc(self.daemon['before']['pid'])
                check_kernel_exit(self.daemon['before'],item['identity_after_exit_event'],item['pidfd'])
                item['status']='PASS'
        finally:
            try:self._cleanup_owned()
            finally:
                for label,(fd,poller,meta) in list(self.handles.items()):
                    os.close(fd);closed=now();self.record['held_pidfds'][label]['closed_at']=closed
                    if label=='daemon' and 'daemon_exit' in self.record:self.record['daemon_exit']['pidfd']['closed_at']=closed
                    del self.handles[label]
                self.record['cleanup_total_seconds']=time.monotonic()-begin
        require(self.record['cleanup_total_seconds']<=15,'whole cleanup budget')


def prepare(config):
    root=Path(config['root']);require(root.is_dir(),'fixture root required')
    for phase in ('oracle','candidate'):
        phase_root=root/phase;phase_root.mkdir()
        for name in ('home-empty','xdg-empty','markers','cwd'):(phase_root/name).mkdir()
        (phase_root/'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-option -s exit-empty off\nset-option -g destroy-unattached off\n")
        (phase_root/'live.conf').write_text('set-option -u -t beta destroy-unattached\n')
    write_once(root/'PLAN.json',{'cases':CASE_LABELS,'spawn':{p:plan(config,p) for p in ('oracle','candidate')},
        'environments':{p:environment(config,p) for p in ('oracle','candidate')},'case_seconds':20,'target_exit_poll_seconds':3,
        'policy':'Observe exact oracle survivors and scope/command outcomes, freeze before candidate. Projection differences retained, never silently changed.','helpers_per_product':3,'daemons_per_product':1})
    write_once(root/'PURE-CHECKS.json',pure_checks(config))

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
            require(gate['status'] == 'PASS' and gate['driver_sha256'] == digest(__file__), 'oracle gate not PASS or driver changed')
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
            write_once(root / 'ORACLE-FROZEN.json', oracle_gate(run.record, digest(run.root / 'RESULT.json')))
    lot.update(end=now(), elapsed_seconds=time.monotonic() - start, fd_after=fd_snapshot(), log_bytes=sum(p.stat().st_size for p in root.rglob('*') if p.is_file()))
    lot['status'] = 'PASS' if len(lot['phases']) == 2 and all(p['status'] == 'PASS' for p in lot['phases']) else 'STOPPED'
    if lot['fd_before'] != lot['fd_after'] or lot['elapsed_seconds'] > 180 or lot['log_bytes'] > 8*1024**2:
        lot['status'] = 'UNPROVEN'
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
