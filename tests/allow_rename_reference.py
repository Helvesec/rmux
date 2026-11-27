#!/usr/bin/env python3
"""Linux allow-rename ESC k reference, isolated private own panes.
prepare CONFIG; run CONFIG oracle|candidate, single use each.
Automatic-rename off; window allow-rename queried also at pane inheritance.
Every ESC k is followed in the same write by a fresh visible consumption marker.
First discrepancy seals OBSERVED before cleanup and stops remaining stages.
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
import socket
import errno
import subprocess
import sys
import time
import traceback
import termios
import tempfile

FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{session_attached}|#{session_name}|#{window_index}|#{window_name}|#{pane_width}|#{pane_height}'
PUBLIC_KEYS = ('daemon_pid','socket','session_id','window_id','pane_id','pane_pid','pane_dead','pane_dead_status','pane_dead_signal','session_attached','session_name','window_index','window_name','pane_width','pane_height')
WINDOW_FORMAT = '#{session_id}|#{session_name}|#{window_id}|#{window_index}|#{window_name}|#{window_panes}'
WINDOW_KEYS = ('session_id','session_name','window_id','window_index','window_name','window_panes')
KINDS={x:x for x in ('alpha','beta')}
CODES={x:0 for x in KINDS}
STAGES=('R1','R2','R3_OFF','R3_ON','R4')
ENV_KEYS=('HOME','XDG_CONFIG_HOME','PATH','L6_ALLOWRENAME_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM')

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
                      fd0=os.readlink(p / 'fd/0'), fd1=os.readlink(p / 'fd/1'), cwd=os.readlink(p / 'cwd'), signal_status_raw=(p / 'status').read_text())
    except (FileNotFoundError, ProcessLookupError, PermissionError) as error:
        result['read_error'] = repr(error)
    result['after'] = identity(pid)
    result['coherent'] = same(before, result['after']) and 'read_error' not in result
    return result

def proc(pid):
    try:return _proc(pid)
    except PermissionError as error:return {'coherent':False,'at':now(),'read_error':repr(error)}

def complete(p):
    return bool(p and p.get('coherent') and p.get('after') and p['before']['state'] not in ('X','Z') and p['after']['state'] not in ('X','Z'))

def snapshot_same(a, b):
    return bool(complete(a) and complete(b) and same(a['before'], b['before']) and
                all(a[k] == b[k] for k in ('exe', 'exe_sha256', 'cmdline_base64', 'fd0', 'fd1', 'cwd')) and
                a['before']['ppid'] == b['before']['ppid'])

def fd_snapshot():
    result = {}
    for fd in os.listdir('/proc/self/fd'):
        try:
            result[fd] = os.readlink('/proc/self/fd/' + fd)
        except FileNotFoundError:
            pass
    return result


def check_child(first, second, public, daemon, expected, ancestry):
    require(snapshot_same(first, second), 'two child snapshots incoherent')
    require(public['pane_pid'] == first['before']['pid'], 'public pane PID mismatch')
    require(complete(daemon) and public['daemon_pid'] == daemon['before']['pid'], 'public daemon mismatch')
    parent = first['before']['ppid']
    for ancestor in ancestry:
        require(complete(ancestor) and ancestor['before']['pid'] == parent, 'broken ancestry')
        parent = ancestor['before']['ppid']
    require(ancestry and snapshot_same(ancestry[-1], daemon), 'not linked to private daemon')
    require(first['fd0'].startswith('/dev/pts/') and first['fd1']==first['fd0'], 'one real PTY stdin/stdout required')
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

def parse_window(raw):
    fields = raw.split('|')
    require(len(fields) == len(WINDOW_KEYS), 'invalid window inventory')
    row = dict(zip(WINDOW_KEYS, fields))
    for key in ('window_index', 'window_panes'):
        require(row[key].isdigit(), 'invalid window number ' + key)
        row[key] = int(row[key])
    return row

def check_kernel_exit(before, after, pidfd):
    require(not pidfd['events_before_request'], 'exit happened before request')
    require(same(before, pidfd['identity_after_open']), 'exit pidfd stale birth')
    require(any(fd == pidfd['fd'] and mask & (select.POLLIN | select.POLLHUP)
                for fd, mask in pidfd['events_after_request']), 'kernel exit event missing')
    require(not same(before, after) or after['state'] == 'Z', 'exited child still live')


def check_cleanup(result, owned):
    actual = {(v['before']['pid'], v['before']['start_ticks']) for v in result['identities_after']}
    expected = {(v['pid'], v['start_ticks']) for v in owned.values()}
    equal(actual, expected, 'cleanup identities complete')
    require(all(not same(v['before'], v['after']) for v in result['identities_after']), 'cleanup surviving birth')
    require(result['all_tracked_exited'] and result['socket_after'] is None and not result['owned_descendants_remaining'], 'cleanup incomplete')

# L6-028: cumulative physical log bytes, with bounded closure/publication reserves.
LOG_CAP=4*1024**2
CLOSURE_RESERVE=256*1024
PUBLICATION_RESERVE=256*1024
COMMAND_MARGIN=256*1024
PRODUCTION_RESERVE=CLOSURE_RESERVE+PUBLICATION_RESERVE+COMMAND_MARGIN

class LogBudgetError(Unproven):
    pass

def encoded_document(value):
    return (json.dumps(value,indent=2)+'\n').encode('utf-8')

def physical_log_bytes(root):
    seen=set();total=0;count=0
    for path in Path(root).rglob('*'):
        info=path.lstat()
        if not stat.S_ISREG(info.st_mode):continue
        key=(info.st_dev,info.st_ino)
        if key in seen:continue
        seen.add(key);count+=1;total+=info.st_size
    return total,count

class LogAccounting:
    def __init__(self,root,cap=LOG_CAP):
        self.root,self.cap=Path(root),cap;self.errors=[];self.last=None;self.high_water=0
    def fault(self,point,error):
        if len(self.errors)<8:self.errors.append({'at':now(),'point':point,'error':str(error)})
    def measure(self,documents=(),future_bytes=0,reserve=0):
        require(future_bytes>=0 and reserve>=0,'nonnegative future/reserve')
        physical,count=physical_log_bytes(self.root);pending=sum(len(encoded_document(x)) for x in documents)
        return {'physical_bytes':physical,'physical_files':count,'unpersisted_bytes':pending,'future_bytes':future_bytes,'reserve_bytes':reserve,'total_with_reserve':physical+pending+future_bytes+reserve,'cap':self.cap}
    def check(self,point,documents=(),future_bytes=0,reserve=0,admit=True):
        if admit and self.errors:raise LogBudgetError('logging failure forbids new admission')
        try:
            value=self.measure(documents,future_bytes,reserve);self.last=value
            self.high_water=max(self.high_water,value['physical_bytes']+value['unpersisted_bytes']+future_bytes)
            if value['total_with_reserve']>self.cap:raise LogBudgetError('physical+new+future+reserve exceed cumulative log cap: '+str(value))
        except Exception as error:
            self.fault(point,repr(error))
            if admit:raise LogBudgetError(str(error)) from error
            return False
        return True
    def summary(self):
        return {'status':'FAIL' if self.errors else 'PASS','errors':copy.deepcopy(self.errors),'last':copy.deepcopy(self.last),'high_water_physical_plus_pending':self.high_water,'cap':self.cap,'production_reserve':PRODUCTION_RESERVE,'closure_reserve':CLOSURE_RESERVE,'publication_reserve':PUBLICATION_RESERVE,'command_margin':COMMAND_MARGIN,'new_admission_allowed':not self.errors}

def combined_verdict(primary,logging):
    return 'LOG_BUDGET_FAIL' if primary=='PASS' and logging.errors else primary

def environment(config,phase):
    root=Path(config['root'])/phase
    return {'HOME':str(root/'home-empty'),'XDG_CONFIG_HOME':str(root/'xdg-empty'),'PATH':'/usr/bin:/bin','SHELL':'/bin/dash','TERM':'vt100','LANG':'C.UTF-8','RMUX_DISABLE_TMUX_FALLBACK':'1','RMUX_DISABLE_TMUX_SHIM':'1','L6_ALLOWRENAME_MARKER':config['tokens'][phase]}

def witness_argv(config,phase,label):
    return [config['python'],'-I',str(Path(__file__).resolve()),'witness',label,str(Path(config['root'])/phase/'markers'),config['tokens'][phase],'0']

def payload(name,nonce):
    require(name.isascii() and name.replace('_','').isalnum() and 1<=len(name)<=40,'bounded ASCII name required')
    require(len(nonce)==48 and all(c in '0123456789abcdef' for c in nonce),'fresh nonce shape')
    return b'\x1bk'+name.encode('ascii')+b'\x1b\\\r\n'+('SEEN_'+nonce).encode('ascii')+b'\r\n'

def check_emission(ack,request,child):
    e=ack['emission'];raw=payload(request['name'],request['challenge'])
    equal(base64.b64decode(e['payload_base64']),raw,'exact emitted bytes')
    equal(e['bytes_written'],len(raw),'entire payload written')
    equal(e['fd1_before'],child['fd0'],'output PTY ownership')
    equal(e['fd1_after'],child['fd0'],'output PTY stable')
    require(not e['output_flags'] & termios.OPOST,'output postprocessing must be disabled')
    require(e['isatty'] and request['issued_at']<=e['start']<=e['end']<=ack['at'],'emission ordering')

def consumed(raw,nonce):
    return ('SEEN_'+nonce) in raw.splitlines()

def stable_public(a,b):
    return all(a[k]==b[k] for k in PUBLIC_KEYS if k!='window_name')

def witness(kind,root,token,exit_code):
    root=Path(root);equal(int(exit_code),0,'helper declaration only')
    attributes=termios.tcgetattr(1);attributes[1] &= ~termios.OPOST;termios.tcsetattr(1,termios.TCSANOW,attributes)
    own=identity(os.getpid());stem=f"{own['pid']}-{own['start_ticks']}"
    def report(nonce):return {'at':now(),'kind':kind,'token':token,'challenge':nonce,'exit_code':0,'identity':identity(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,'environment':{k:os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root/('born-'+stem+'.json'),report(None));deadline=time.monotonic()+180
    while time.monotonic()<deadline:
        for path in sorted(root.glob('request-'+stem+'-*.json')):
            ack=root/path.name.replace('request-','ack-',1)
            if ack.exists():continue
            request=json.loads(path.read_text());require(same(request['identity'],own),'request birth')
            emission=None
            if request.get('name') is not None:
                raw=payload(request['name'],request['challenge'])
                emission={'start':now(),'payload_base64':base64.b64encode(raw).decode(),'fd1_before':os.readlink('/proc/self/fd/1'),'isatty':os.isatty(1),'output_flags':termios.tcgetattr(1)[1]}
                require(emission['isatty'],'own output not PTY')
                emission['bytes_written']=os.write(1,raw)
                emission.update(end=now(),fd1_after=os.readlink('/proc/self/fd/1'))
                equal(emission['bytes_written'],len(raw),'short PTY write')
            value=report(request['challenge']);value['emission']=emission;atomic_once(ack,value)
        time.sleep(.02)
    os._exit(124)

def normalize_names(names,configured):
    reverse={v:k for k,v in configured.items()}
    return {label:reverse.get(value,value) for label,value in names.items()}

def compare(actual,expected):equal(actual,expected,'frozen oracle behavior mismatch')

def pure_checks(config):
    expected = {'exe': '/python', 'exe_sha256': 'pin', 'argv': ['/python', '-I', '/driver', 'witness', 'C'],
                'cwd': '/cwd', 'kind': 'C', 'token': 'fresh', 'exit_code': 7, 'environment': {'HOME': '/empty'}}
    def fake(pid, parent):
        own = {'pid': pid, 'ppid': parent, 'start_ticks': str(pid * 10), 'state': 'S'}
        return {**expected, 'before': own, 'after': dict(own), 'coherent': True, 'fd0': '/dev/pts/9', 'fd1': '/dev/pts/9',
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
    rejected('wrong_output_pty',lambda:check_child({**child,'fd1':'pipe:9'},{**child,'fd1':'pipe:9'},public,daemon,expected,[daemon]))
    for state in ('X','Z'):
        bad=copy.deepcopy(child);bad['after']['state']=state
        rejected('notlive_'+state,lambda bad=bad:require(complete(bad),'not live'))
    nonce='a'*48;raw=payload('NAME_1',nonce)
    equal(raw,b'\x1bkNAME_1\x1b\\\r\nSEEN_'+nonce.encode()+b'\r\n','ESC k exact bytes');checks.append('exact_payload')
    require(consumed('SEEN_'+nonce,nonce),'visible marker required');checks.append('positive_consumption')
    rejected('missing_consumption',lambda:require(consumed('old','b'*48),'stale marker'))
    for name in ('','bad name','bad|name','x'*41):rejected('bad_name_'+repr(name),lambda name=name:payload(name,nonce))
    request={'name':'NAME_1','challenge':nonce,'issued_at':'1'}
    emission={'payload_base64':base64.b64encode(raw).decode(),'bytes_written':len(raw),'fd1_before':child['fd0'],'fd1_after':child['fd0'],'isatty':True,'output_flags':0,'start':'2','end':'3'}
    ack={'at':'4','emission':emission};check_emission(ack,request,child);checks.append('positive_emission')
    for key,value in [('bytes_written',0),('payload_base64',base64.b64encode(b'wrong').decode()),('fd1_after','pipe:1'),('isatty',False),('output_flags',termios.OPOST),('start','0')]:
        rejected('bad_emission_'+key,lambda key=key,value=value:check_emission({**ack,'emission':{**emission,key:value}},request,child))
    names={'alpha':'R2','beta':'beta'};compare(names,dict(names));checks.append('names_exact')
    rejected('cross_window_rename',lambda:compare({'alpha':'alpha','beta':'R2'},names))
    rejected('rename_missing',lambda:compare({'alpha':'alpha','beta':'beta'},names))
    equal(normalize_names({'alpha':'FRESH_A','beta':'beta'},{'R2':'FRESH_A'}),names,'name normalization');checks.append('name_normalization')
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
    def __init__(self,config,phase,start,expected):
        self.config,self.phase,self.start,self.oracle=config,phase,start,expected
        self.root=Path(config['root'])/phase;self.binary,self.socket=config[phase],self.root/'s';self.env=environment(config,phase)
        self.case_end=start+150;self.cleaning=False;self.daemon=self.socket_identity=None
        self.owned,self.children,self.handles={},{},{};self.spawn_count=0
        self.accounting=LogAccounting(Path(config['root']).parent);self.sealed_command_count=None;self.sealed_profile_count=0;self.cleanup_command_count=0
        self.record={'phase':phase,'start':now(),'environment':self.env,'commands':[],'stages':[],'observations':[],'public_samples':[],'profiles':[],'marker_ledgers':[],'exits':[],'held_pidfds':{},'captures':[],'options':[],'mutations':[],'inventories':[]}
    def pending_closure(self):
        keys=('phase','start','status','spawn_commands','not_run','proof_sealed_at','cleanup','cleanup_seconds','cleanup_error','cleanup_traceback','exits','held_pidfds','recovery_owners','endpoint_after_cleanup_command','frozen_sha256')
        value={k:self.record.get(k) for k in keys}
        value.update(cleanup_commands=self.record['commands'][self.sealed_command_count:],cleanup_profiles=self.record['profiles'][self.sealed_profile_count:])
        return value

    def check_logging(self,point,admit):
        pending=self.record if self.sealed_command_count is None else self.pending_closure()
        reserve=PUBLICATION_RESERVE if self.cleaning else PRODUCTION_RESERVE
        return self.accounting.check(point,[pending],reserve=reserve,admit=admit)

    def seal_observation(self):
        self.record['proof_sealed_at']=now();self.record['spawn_commands']=self.spawn_count
        observed=self.root/'OBSERVED.json';value=copy.deepcopy(self.record)
        # A genuine oversize proof is not silently admitted. Keep a bounded failure
        # receipt and leave the already reserved closure mechanism usable.
        if not self.accounting.check('seal_observation',[value],reserve=CLOSURE_RESERVE+PUBLICATION_RESERVE,admit=False):
            if self.accounting.last is None or self.accounting.last['total_with_reserve']>self.accounting.cap:
                value={'phase':self.phase,'status':'LOG_BUDGET_FAIL','primary_status':self.record.get('status'),'primary_error':self.record.get('error'),'proof_sealed_at':self.record['proof_sealed_at'],'unwritten_full_record_sha256':hashlib.sha256(encoded_document(value)).hexdigest(),'omitted_full_record':True,'logging':self.accounting.summary()}
        write_once(observed,value);self.sealed_command_count=len(self.record['commands']);self.sealed_profile_count=len(self.record['profiles'])
        return observed

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
        if self.cleaning:
            require(args==['kill-server'] and self.cleanup_command_count<2,'bounded closure commands only')
            self.cleanup_command_count+=1
        self.check_logging('before_command',admit=not self.cleaning)
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup:
            argv += ['-f', str(self.root / 'startup.conf')]
        argv += args
        begin = now()
        p = subprocess.Popen(argv, cwd=self.root, env=self.env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        born, timed_out = identity(p.pid), False
        client_fd=os.pidfd_open(p.pid,0);client_after_open=identity(p.pid)
        require(same(born,client_after_open),'own client birth changed')
        try:
            out, err = p.communicate(timeout=self.remaining())
        except subprocess.TimeoutExpired:
            timed_out = True
            if same(born, identity(p.pid)):
                signal.pidfd_send_signal(client_fd,signal.SIGKILL,None,0)
            out, err = p.communicate(timeout=2)
        finally:os.close(client_fd)
        self.record['commands'].append({'argv': argv, 'start': begin, 'end': now(), 'identity': born,
                                        'exit_code': p.returncode, 'stdout': out, 'stderr': err, 'timeout': timed_out,'retained_client_pidfd':{'fd':client_fd,'after_open':client_after_open,'closed':True}})
        require(not timed_out, 'command timeout')
        if len(out)+len(err)>=65536:
            self.accounting.fault('command_output','postcommunicate output cap exceeded')
            c=self.record['commands'][-1]
            c.update(uncropped_output_sha256=hashlib.sha256((out+err).encode()).hexdigest(),uncropped_output_chars=len(out)+len(err),output_truncated=True)
            c['stdout'],c['stderr']=out[:4096],err[:4096]
        self.check_logging('after_command',admit=not self.cleaning)
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

    def hold(self,label,snapshot):
        require(complete(snapshot),'qualified live snapshot');before=snapshot['before'];fd=os.pidfd_open(before['pid'],0)
        poller=select.poll();poller.register(fd,select.POLLIN)
        meta={'fd':fd,'opened_at':now(),'original_identity':copy.deepcopy(before),'identity_after_open':identity(before['pid']),'events_before_request':poller.poll(0)}
        self.handles[label]=(fd,poller,meta);self.record['held_pidfds'][label]=copy.deepcopy(meta)
        require(same(before,meta['identity_after_open']) and not meta['events_before_request'],'held birth changed')

    def signal_retained(self,old,signum):
        matches=[(label,v) for label,v in self.handles.items() if same(v[2]['original_identity'],old)]
        require(len(matches)==1,'recovery owner needed; no held pidfd for discovered child')
        label,(fd,poller,meta)=matches[0];current=identity(old['pid'])
        require(same(old,current) and current['state'] not in ('X','Z') and not poller.poll(0),'retained signal stale')
        signal.pidfd_send_signal(fd,signum,None,0)
        return {'label':label,'signal':int(signum),'fd':fd,'original_identity':meta['original_identity'],'current':current,'sent_at':now()}

    def observe(self,label,emission_stage=None):
        target=self.children[label]['target'] if label in self.children else self.targets[label]
        public,daemon=self.public(target);own=identity(public['pane_pid']);require(own,'child absent');self.owned[own['pid']]=own
        stem=f"{own['pid']}-{own['start_ticks']}";markers=self.root/'markers';born_path=markers/('born-'+stem+'.json')
        born=self.wait_json(born_path);first=proc(own['pid']);expected=self.expected(label)
        item={'label':label,'target':target,'at':now(),'emission_stage':emission_stage,'public':public,'first':first,'expected':expected,'born':born,'born_path':str(born_path),'reobserve':label in self.children,'status':'UNPROVEN'}
        self.record['observations'].append(item);require(len(self.record['observations'])<=12,'challenge cap')
        check_marker(born,first,expected,None)
        equal([public[k] for k in ('pane_dead','pane_dead_status','pane_dead_signal','session_attached','pane_width','pane_height')],['0','','','0','80','24'],'live detached80x24')
        nonce=secrets.token_hex(24);request={'identity':own,'challenge':nonce,'issued_at':now(),'name':None if emission_stage is None else self.config['names'][self.phase][emission_stage]}
        path=markers/f'request-{stem}-{len(self.record["observations"])-1}.json';item.update(request=request,request_path=str(path));atomic_once(path,request)
        ack_path=markers/path.name.replace('request-','ack-',1);ack=self.wait_json(ack_path);item.update(ack=ack,ack_path=str(ack_path))
        if emission_stage:
            check_emission(ack,request,first)
            capture={'stage':emission_stage,'target':target,'nonce':nonce,'start':now(),'samples':[]};self.record['captures'].append(capture)
            for _ in range(40):
                index=len(self.record['commands']);raw=self.command(['capture-pane','-p','-t',target]);capture['samples'].append({'at':now(),'command_index':index,'raw':raw})
                if consumed(raw,nonce):break
                time.sleep(min(.05,self.remaining()))
            require(consumed(raw,nonce),'fresh terminal consumption marker missing')
            capture.update(end=now(),status='PASS');item['capture_index']=len(self.record['captures'])-1
        else:equal(ack['emission'],None,'inert challenge emitted output')
        second=proc(own['pid']);item['child']=second;after,daemon_after=self.public(target);item['public_after']=after
        require(stable_public(public,after),'public identities changed during challenge')
        ancestry=[];parent=second['before']['ppid'];item['ancestry']=ancestry
        for _ in range(8):
            a=proc(parent);ancestry.append(a);require(complete(a),'own ancestry missing')
            if parent==daemon['before']['pid']:break
            parent=a['before']['ppid']
        check_child(first,second,public,daemon_after,expected,ancestry);check_marker(ack,second,expected,nonce)
        if label in self.children:require(snapshot_same(self.children[label]['child'],second) and stable_public(self.children[label]['public'],after),'original identity changed')
        item['status']='PASS'
        if label not in self.children:self.children[label]=copy.deepcopy(item)
        return item

    def mutation(self,args,purpose):
        index=len(self.record['commands']);self.command(args,required=False);c=self.record['commands'][index]
        m={'purpose':purpose,'command_index':index,**{k:c[k] for k in ('argv','start','end','exit_code','stdout','stderr')}};self.record['mutations'].append(m)
        equal(c['exit_code'],0,'mutation command failed');return m

    def options(self,point):
        item={'point':point,'at':now(),'windows':{}};self.record['options'].append(item)
        for label,child in self.children.items():
            wid=child['public']['window_id'];pid=child['public']['pane_id'];values={'commands':[]}
            for key,args in [('window_allow',['show-options','-Awv','-t',wid,'allow-rename']),('pane_allow',['show-options','-Apv','-t',pid,'allow-rename']),('pane_local',['show-options','-pqv','-t',pid,'allow-rename']),('automatic',['show-options','-Awv','-t',wid,'automatic-rename'])]:
                values['commands'].append(len(self.record['commands']));values[key]=self.command(args)
            item['windows'][label]=values
            equal(values['pane_local'],'','no pane override');equal(values['automatic'],'off','automatic rename isolated')
            equal(values['window_allow'],values['pane_allow'],'pane inherits window flag')
        return copy.deepcopy(item)

    def inventory(self,point):
        item={'point':point,'at':now(),'command_indices':[]};self.record['inventories'].append(item)
        for field,args in [('panes',['list-panes','-a','-F',FORMAT]),('windows',['list-windows','-a','-F',WINDOW_FORMAT]),('sessions',['list-sessions','-F','#{session_id}|#{session_name}|#{session_attached}|#{session_windows}'])]:
            item['command_indices'].append(len(self.record['commands']));item[field+'_raw']=self.command(args)
        panes=[parse_public(line) for line in item['panes_raw'].splitlines()];windows=[parse_window(line) for line in item['windows_raw'].splitlines()]
        equal(len(panes),2,'two panes');equal(len(windows),2,'two windows');equal(item['sessions_raw'],self.children['alpha']['public']['session_id']+'|session|0|2','one detached session')
        names={}
        for label,child in self.children.items():
            expected=child['public'];p=[p for p in panes if p['pane_id']==expected['pane_id']];w=[w for w in windows if w['window_id']==expected['window_id']]
            equal(len(p),1,'unique pane');equal(len(w),1,'unique window');require(stable_public(expected,p[0]),'stable pane IDs/geometry required')
            for key in ('session_id','session_name','window_id','window_index','window_name'):equal(w[0][key],p[0][key],'pane/window attribution')
            equal(w[0]['window_panes'],1,'one pane per window');names[label]=w[0]['window_name']
        item.update(names=names,pidfd_events={k:v[1].poll(0) for k,v in self.handles.items()},daemon=proc(self.daemon['before']['pid']),endpoint=self.socket_info())
        require(snapshot_same(self.daemon,item['daemon']) and item['endpoint']==self.socket_identity and all(not x for x in item['pidfd_events'].values()),'owned liveness or endpoint changed')
        return copy.deepcopy(item)

    def setup(self):
        self.case_end=min(time.monotonic()+20,self.start+150);require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh fixture');self.profiles();self.targets={}
        for label in KINDS:
            args=['new-session','-d','-P','-F','#{pane_id}','-s','session','-n',label,'-x','80','-y','24'] if label=='alpha' else ['new-window','-d','-P','-F','#{pane_id}','-t',self.children['alpha']['public']['session_id'],'-n',label]
            self.spawn_count+=1;target=self.command([*args,'-c',str(self.root/'cwd'),*witness_argv(self.config,self.phase,label)],startup=label=='alpha')
            require(target.startswith('%') and target[1:].isdigit(),'qualified pane ID');self.targets[label]=target;obs=self.observe(label)
            equal(obs['public_after']['window_name'],label,'fixed initial name')
            if 'daemon' not in self.handles:self.hold('daemon',self.daemon)
            self.hold(label,obs['child'])
        for label,child in self.children.items():
            self.mutation(['set-option','-w','-t',child['public']['window_id'],'allow-rename','off'],'initial_'+label+'_off')
        values=self.options('setup');equal({k:v['window_allow'] for k,v in values['windows'].items()},{'alpha':'off','beta':'off'},'initial flags')
        self.inventory('setup')
        sources=[]
        for filename,label,value in [('beta-on.conf','beta','on'),('alpha-off.conf','alpha','off')]:
            wid=self.children[label]['public']['window_id'];require(wid.startswith('@') and wid[1:].isdigit(),'qualified exact window ID')
            template=(self.root/(filename+'.in')).read_text();text=template.replace('OWN_WINDOW_ID',wid)
            require(text==f'set-option -w -t {wid} allow-rename {value}\n','source template changed')
            path=self.root/filename
            with path.open('x') as f:f.write(text)
            sources.append({'path':str(path),'sha256':digest(path),'text':text,'owner_label':label,'window_id':wid})
        write_once(self.root/'SOURCE-INPUTS-FROZEN.json',{'at':now(),'sources':sources,'basis':'Qualified held identities; templates frozen before native; exact inputs frozen before R1.'})
        self.record['source_inputs']=sources
        self.profiles();self.remaining()

    def stage(self,name):
        self.case_end=min(time.monotonic()+20,self.start+150);begin=time.monotonic();entry={'id':name,'start':now(),'mutation_start':len(self.record['mutations']),'status':'UNPROVEN'};self.record['stages'].append(entry)
        try:
            before=self.options(name+'_before');pre=self.inventory(name+'_before')
            if name=='R2':self.mutation(['set-option','-w','-t',self.children['alpha']['public']['window_id'],'allow-rename','on'],'direct_alpha_on')
            elif name in ('R3_ON','R4'):
                filename='beta-on.conf' if name=='R3_ON' else 'alpha-off.conf';path=self.root/filename
                pin=next(x for x in self.record['source_inputs'] if x['path']==str(path));equal(digest(path),pin['sha256'],'frozen source altered')
                self.mutation(['source-file',str(path)],filename)
            options=self.options(name+'_effective')
            label='beta' if name.startswith('R3') else 'alpha';peer='alpha' if label=='beta' else 'beta'
            self.observe(label,name);first=self.inventory(name+'_after_consumption');self.observe(peer);time.sleep(min(.05,self.remaining()));second=self.inventory(name+'_after_peer')
            equal(first['names'],second['names'],'two stable post-consumption name observations')
            final_options=self.options(name+'_after')
            equal({k:{a:v[a] for a in ('window_allow','pane_allow','pane_local','automatic')} for k,v in final_options['windows'].items()},{k:{a:v[a] for a in ('window_allow','pane_allow','pane_local','automatic')} for k,v in options['windows'].items()},'effective options stable across emission')
            expected_names={'R1':{'alpha':'alpha','beta':'beta'},'R2':{'alpha':'R2','beta':'beta'},'R3_OFF':{'alpha':'R2','beta':'beta'},'R3_ON':{'alpha':'R2','beta':'R3_ON'},'R4':{'alpha':'R2','beta':'R3_ON'}}
            effect={'names':normalize_names(second['names'],self.config['names'][self.phase]),'options':{k:{a:v[a] for a in ('window_allow','pane_allow','pane_local','automatic')} for k,v in final_options['windows'].items()},'consumed':True,'stable_ids':True}
            entry['effect']=effect
            want_flags={'alpha':'off' if name in ('R1','R4') else 'on','beta':'on' if name in ('R3_ON','R4') else 'off'}
            entry['mutation_results']=[{k:m[k] for k in ('purpose','exit_code','stdout','stderr')} for m in self.record['mutations'][entry['mutation_start']:]]
            comparison={'effect':effect,'mutation_results':entry['mutation_results']};entry['comparison']=comparison
            if self.phase=='oracle':
                equal(effect['names'],expected_names[name],'oracle contradicts required rename precondition');equal({k:v['window_allow'] for k,v in effect['options'].items()},want_flags,'oracle flags contradict precondition')
            else:compare(comparison,self.oracle[name])
            entry['status']='PASS';self.remaining()
        except Exception as error:
            entry.update(status='RUNTIME_MISMATCH' if self.phase=='candidate' and isinstance(error,Failed) else 'ORACLE_CONTRADICTION' if isinstance(error,Failed) else 'UNPROVEN',error=str(error));raise
        finally:entry.update(end=now(),elapsed_seconds=time.monotonic()-begin)

    def execute(self):
        self.setup()
        for name in STAGES:self.stage(name)
        self.profiles()

    def record_exit(self,label):
        fd,poller,meta=self.handles[label];old=meta['original_identity'];e={'label':label,'cause':'final_cleanup','original_identity':old,'pidfd':copy.deepcopy(meta),'at':now(),'status':'UNPROVEN'};self.record['exits'].append(e)
        e['pidfd'].update(events_after_request=poller.poll(max(1,int(self.remaining()*1000))),observed_at=now());e['identity_after_exit_event']=identity(old['pid'])
        check_kernel_exit(old,e['identity_after_exit_event'],e['pidfd']);e['status']='PASS'
        # Handles remain held through owned cleanup; no reopen for fallback.

    def cleanup(self):
        begin=time.monotonic();self.cleaning=True;self.cleanup_end=min(begin+30,self.start+180);self.case_end=self.cleanup_end
        try:
            if self.daemon and snapshot_same(self.daemon,proc(self.daemon['before']['pid'])) and self.socket_info()==self.socket_identity:
                try:self.command(['kill-server'])
                except Exception as error:self.record.setdefault('closure_errors',[]).append({'step':'kill-server','error':repr(error)})
                for label in ('alpha','beta','daemon'):
                    if label in self.handles:
                        try:self.record_exit(label)
                        except Exception as error:self.record.setdefault('closure_errors',[]).append({'step':'record_exit_'+label,'error':repr(error)})
                require(any(e['label']=='daemon' and e['status']=='PASS' for e in self.record['exits']),'daemon exit must be observed before endpoint probe')
                probe={'at':now(),'pathname':self.socket_info()}
                with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as s:
                    s.settimeout(min(1,self.remaining()))
                    try:s.connect(str(self.socket));probe.update(accepting=True,errno=None)
                    except OSError as error:probe.update(accepting=False,errno=error.errno,error=repr(error))
                self.record['endpoint_after_cleanup_command']=probe
                require(not probe['accepting'] and probe['errno'] in (errno.ENOENT,errno.ECONNREFUSED),'listener absence unproven')
        finally:
            try:self.cleanup_owned()
            finally:
                self.record['recovery_owners']=[meta['original_identity'] for _,(_,poller,meta) in self.handles.items() if not poller.poll(0)]
                for label,(fd,poller,meta) in list(self.handles.items()):
                    os.close(fd);closed=now();self.record['held_pidfds'][label]['closed_at']=closed
                    for e in self.record['exits']:
                        if e['label']==label:e['pidfd']['closed_at']=closed
                    del self.handles[label]
                self.record['cleanup_seconds']=time.monotonic()-begin
        require(self.record['cleanup_seconds']<=30,'cleanup budget')
        require(not self.record.get('closure_errors'),'closure operation errors preserved')
    def cleanup_owned(self):
        start = time.monotonic()
        self.cleaning = True
        self.case_end = self.cleanup_end
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
                    result['actions'].append(self.signal_retained(old, sig))
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
def accounting_checks(config):
    """Pure regressions: real file accounting and real Run.cleanup orchestration.
    OS effects of the closure test are replaced at their I/O boundary; no product,
    process creation, pidfd operation, terminal or socket is executed by the fake.
    """
    checks=[]
    with tempfile.TemporaryDirectory(prefix='accounting-pure-',dir=config['root']) as temp:
        root=Path(temp);files=root/'files';files.mkdir();a=files/'one';a.write_bytes(b'1234567890')
        equal(physical_log_bytes(files),(10,1),'single physical file');checks.append('account_single_physical_file')
        (files/'two').write_bytes(a.read_bytes());equal(physical_log_bytes(files),(20,2),'identical independent copy counts twice');checks.append('account_equal_bytes_two_files')
        os.link(a,files/'same_inode');equal(physical_log_bytes(files),(20,2),'one physical inode once');checks.append('account_hardlink_once')
        counter=LogAccounting(files,cap=100);doc={'new':'data'};pending=len(encoded_document(doc))
        value=counter.measure([doc],future_bytes=7,reserve=11);equal(value['total_with_reserve'],20+pending+18,'new future reserve all counted');checks.append('account_pending_future_and_reserve')
        boundary=LogAccounting(files,cap=20+pending+18);require(boundary.check('boundary',[doc],future_bytes=7,reserve=11),'inclusive boundary');checks.append('account_exact_threshold_pass')
        over=LogAccounting(files,cap=20+pending+17);require(not over.check('one_over',[doc],future_bytes=7,reserve=11,admit=False),'one byte over must fail')
        equal(combined_verdict('PASS',over),'LOG_BUDGET_FAIL','real excess cannot PASS');checks.append('account_one_byte_over_fail')
        try:over.check('new_admission')
        except LogBudgetError:checks.append('account_excess_blocks_new_admission')
        else:raise AssertionError('admitted after log failure')
        future=LogAccounting(files,cap=20+pending+10);require(not future.check('future',[doc],future_bytes=11,admit=False),'future publication must count');checks.append('account_future_publication_blocks')
        equal(combined_verdict('RUNTIME_MISMATCH',over),'RUNTIME_MISMATCH','primary mismatch retained');equal(combined_verdict('UNPROVEN',over),'UNPROVEN','primary technical error retained');checks.append('account_primary_verdict_preserved')
        for mode in ('normal','real_excess','accounting_error'):
            phase_root=root/mode;phase_root.mkdir();(phase_root/'attempt1/oracle').mkdir(parents=True)
            c={'root':str(phase_root/'attempt1'),'oracle':'/pure-not-executed','tokens':{'oracle':'pure'}}
            run=Run(c,'oracle',time.monotonic(),{});run.record.update(status='PASS',not_run=[],pure_archived_payload='x'*4096)
            observed=run.seal_observation();sealed=json.loads(observed.read_text());require('pure_archived_payload' in sealed,'complete seal expected')
            pending_doc=run.pending_closure();require('pure_archived_payload' not in pending_doc,'persisted content must not be pending')
            measured=run.accounting.measure([pending_doc]);equal(measured['physical_bytes'],observed.stat().st_size,'sealed file counted once');equal(measured['unpersisted_bytes'],len(encoded_document(pending_doc)),'only new closure publication pending')
            if mode=='normal':checks.append('account_real_seal_excludes_persisted_record')
            states={'killed':False,'commands':0,'probes':0,'closed':[]}
            own={label:{'pid':pid,'start_ticks':str(pid*10),'state':'S','ppid':1} for label,pid in [('daemon',10),('alpha',20),('beta',21)]}
            def ident(pid):
                for x in own.values():
                    if x['pid']==pid:return {**x,'state':'Z' if states['killed'] else 'S'}
                return {'pid':pid,'start_ticks':str(pid*10),'state':'S','ppid':1}
            def fakeproc(pid):
                x=ident(pid);return {'before':x,'after':dict(x),'coherent':True,'exe':'/pure','exe_sha256':'pure','cmdline_base64':'AA==','fd0':'/pure','fd1':'/pure','cwd':'/pure'}
            run.daemon=fakeproc(10);run.socket_identity={'path':str(run.socket),'socket':True,'device':1,'inode':1};run.socket_info=lambda:dict(run.socket_identity)
            class Poll:
                def __init__(self,fd):self.fd=fd
                def poll(self,*args):return [[self.fd,1]] if states['killed'] else []
            for i,(label,x) in enumerate(own.items(),101):
                meta={'fd':i,'opened_at':now(),'original_identity':x,'identity_after_open':dict(x),'events_before_request':[]}
                run.handles[label]=(i,Poll(i),meta);run.record['held_pidfds'][label]=copy.deepcopy(meta)
            class Child:
                def __init__(self,*args,**kwargs):self.pid=99;self.returncode=0;states['commands']+=1
                def communicate(self,**kwargs):states['killed']=True;return ('','')
            class Endpoint:
                def __init__(self,*args):pass
                def __enter__(self):return self
                def __exit__(self,*args):pass
                def settimeout(self,value):pass
                def connect(self,path):states['probes']+=1;raise OSError(errno.ECONNREFUSED,'pure probe')
            def finish_owned():
                require(len(run.record['exits'])==3 and states['probes']==1,'mandatory closure observations were skipped')
                run.record['cleanup']={'status':'PASS','all_tracked_exited':True,'identities_after':[{'before':x,'after':None} for x in own.values()],'actions':[],'reaped':[],'socket_after':None,'owned_descendants_remaining':[]}
            run.cleanup_owned=finish_owned
            old=(globals()['identity'],globals()['proc'],globals()['physical_log_bytes'],subprocess.Popen,os.pidfd_open,os.close,socket.socket)
            try:
                globals()['identity']=ident;globals()['proc']=fakeproc;subprocess.Popen=Child;os.pidfd_open=lambda *args:199;os.close=lambda fd:states['closed'].append(fd);socket.socket=Endpoint
                if mode=='real_excess':run.accounting.cap=1
                if mode=='accounting_error':
                    def denied(*args):raise PermissionError('pure accounting error')
                    globals()['physical_log_bytes']=denied;run.record['error']='retained primary error'
                run.cleanup()
                require(states['commands']==1 and states['probes']==1 and len(run.record['exits'])==3 and all(e['status']=='PASS' and e['pidfd']['events_after_request'] and 'closed_at' in e['pidfd'] for e in run.record['exits']),'real cleanup path incomplete')
                equal(set(states['closed']),{101,102,103,199},'all virtual retained handles closed');require(not run.handles and not run.record['recovery_owners'],'virtual owned cleanup complete')
                if mode=='normal':require(not run.accounting.errors,'normal sealed cleanup double counted')
                else:
                    require(run.accounting.errors and combined_verdict('PASS',run.accounting)=='LOG_BUDGET_FAIL','failure lost')
                    run.cleaning=False
                    try:run.command(['new-window'])
                    except LogBudgetError:pass
                    else:raise AssertionError('new command admitted after accounting failure')
                    equal(states['commands'],1,'no new simulated product command')
                if mode=='accounting_error':equal(run.record['error'],'retained primary error','accounting masked primary')
            finally:
                globals()['identity'],globals()['proc'],globals()['physical_log_bytes'],subprocess.Popen,os.pidfd_open,os.close,socket.socket=old
            checks.append('real_cleanup_'+mode+'_retains_pidfd_and_endpoint')
        emergency=root/'emergency';emergency.mkdir();(emergency/'attempt1/oracle').mkdir(parents=True)
        run=Run({'root':str(emergency/'attempt1'),'oracle':'/pure','tokens':{'oracle':'pure'}},'oracle',time.monotonic(),{});run.record.update(status='UNPROVEN',error='first_error',not_run=[],oversized='x'*4096);run.accounting.cap=1
        sealed=run.seal_observation();value=json.loads(sealed.read_text());require(value['omitted_full_record'] and value['primary_error']=='first_error' and value['status']=='LOG_BUDGET_FAIL' and len(sealed.read_bytes())<4096,'oversize receipt must be bounded and honest');checks.append('account_oversize_bounded_failure_receipt')
    return checks

def oracle_gate(record,result_sha):
    equal(record['status'],'PASS','complete oracle required');equal([x['id'] for x in record['stages']],list(STAGES),'complete oracle stages')
    require(all(x['status']=='PASS' for x in record['stages']),'oracle stage failed');equal(record['spawn_commands'],2,'two helpers');equal(len(record['observations']),12,'complete challenges')
    equal([x['stage'] for x in record['captures']],list(STAGES),'all five emissions consumed')
    equal(record['cleanup']['status'],'PASS','cleanup required');equal({x['label'] for x in record['exits']},{'alpha','beta','daemon'},'three exits')
    for e in record['exits']:check_kernel_exit(e['original_identity'],e['identity_after_exit_event'],e['pidfd']);require('closed_at' in e['pidfd'],'closed handle required')
    return {'at':now(),'status':'PASS','driver_sha256':digest(__file__),'result_sha256':result_sha,'stages':{x['id']:x['comparison'] for x in record['stages']},'scope':'set-option -w target qualified window ID; query -Awv window and -Apv pane, pane-local -pqv absent; automatic-rename -Awv off'}

def prepare(config):
    root=Path(config['root']);require(root.is_dir(),'fixture root absent')
    for phase in ('oracle','candidate'):
        p=root/phase;p.mkdir()
        for name in ('home-empty','xdg-empty','markers','cwd'):(p/name).mkdir()
        (p/'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-option -g base-index 0\nset-option -g renumber-windows off\nset-option -gw automatic-rename off\nset-option -gw allow-rename off\n")
        (p/'beta-on.conf.in').write_text('set-option -w -t OWN_WINDOW_ID allow-rename on\n')
        (p/'alpha-off.conf.in').write_text('set-option -w -t OWN_WINDOW_ID allow-rename off\n')
    write_once(root/'PLAN.json',{'stages':STAGES,'cases':{'R1':['R1'],'R2':['R2'],'R3':['R3_OFF','R3_ON'],'R4':['R4']},'geometry':[80,24],'helpers':2,'daemon_per_product':1,'challenges_max':12,'child_snapshots_max':24,'scope':'window flag; pane inheritance queried; no pane override','consumption':'Same own PTY write: ESC k ASCII name ESC backslash CRLF SEEN_nonce CRLF; capture-pane must contain full fresh marker as line. Max40 polls50ms. Two final public inventories around peer fresh challenge; stableIDs and names after marker.','source_inputs':'Fixed templates before native; render only qualified own window ID and freeze exact files before R1.','stop':'First mismatch seals OBSERVED, then cleanup only, all subsequent stages NOT_RUN. No native retry.','budgets':{'lot_seconds':180,'transition_seconds':20,'cleanup_seconds':30,'logs_total':4194304,'production_reserve':PRODUCTION_RESERVE,'closure_reserve':CLOSURE_RESERVE,'publication_reserve':PUBLICATION_RESERVE,'accounting_root':str(root.parent)}})
    controls=pure_checks(config);controls['original_controls']=len(controls['checks']);controls['checks'].extend(accounting_checks(config));write_once(root/'PURE-CHECKS.json',controls)

def execute_phase(config,config_path,phase):
    root=Path(config['root']);frozen=json.loads((root/'FROZEN-NATIVE.json').read_text());artifacts={x['path']:x['sha256'] for x in frozen['artifacts']}
    mandatory=[str(Path(__file__).resolve()),str(Path(config_path).resolve()),str(root/'PLAN.json'),str(root/'PURE-CHECKS.json')]+[config[k] for k in ('oracle','candidate','daemon','python')]+[str(root/p/f) for p in ('oracle','candidate') for f in ('startup.conf','beta-on.conf.in','alpha-off.conf.in')]
    require(all(p in artifacts for p in mandatory),'freeze incomplete')
    for p,h in artifacts.items():equal(digest(p),h,'frozen input changed '+p)
    pure=json.loads((root/'PURE-CHECKS.json').read_text());require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure controls stale')
    expected={}
    if phase=='candidate':
        gate=json.loads((root/'ORACLE-FROZEN.json').read_text());require(gate['status']=='PASS' and gate['driver_sha256']==digest(__file__),'oracle gate invalid')
        equal(digest(root/'oracle/RESULT.json'),gate['result_sha256'],'oracle proof changed');expected=gate['stages']
    # Fail closed before opening any new product if prior physical logs use the reserve.
    admission=LogAccounting(root.parent);admission.check('phase_admission',future_bytes=8192,reserve=PRODUCTION_RESERVE)
    require(not (root/'RECOVERY-OWNER.json').exists(),'mechanical recovery ownership blocks new admission')
    require(ctypes.CDLL(None,use_errno=True).prctl(36,1,0,0,0)==0,'subreaper unavailable')
    write_once(root/phase/'STARTED.json',{'at':now(),'identity':proc(os.getpid())})
    start=time.monotonic();before=fd_snapshot();run=Run(config,phase,start,expected);run.record['frozen_sha256']=digest(root/'FROZEN-NATIVE.json')
    try:run.execute();run.record['status']='PASS'
    except Exception as error:
        run.record.update(status='RUNTIME_MISMATCH' if phase=='candidate' and isinstance(error,Failed) else 'ORACLE_CONTRADICTION' if isinstance(error,Failed) else 'UNPROVEN',error=str(error),traceback=traceback.format_exc())
    finally:
        primary=run.record['status'];completed={x['id'] for x in run.record['stages']};run.record['not_run']=[x for x in STAGES if x not in completed]
        observed=None
        try:observed=run.seal_observation()
        except Exception as error:run.accounting.fault('seal_write',repr(error))
        # Logging errors never bypass cleanup of already held identities.
        try:run.cleanup()
        except Exception as error:run.record.update(cleanup_error=repr(error),cleanup_traceback=traceback.format_exc())
        if run.record.get('recovery_owners'):
            write_once(root/'RECOVERY-OWNER.json',{'at':now(),'phase':phase,'identities':run.record['recovery_owners'],'new_admission':False,'reason':'Owned cleanup incomplete; no blind PID reopening authorized.'})
        after=fd_snapshot();elapsed=time.monotonic()-start
        closure_ok=not run.record.get('cleanup_error') and not run.record.get('recovery_owners') and before==after and elapsed<=180
        base_status=primary if closure_ok or primary!='PASS' else 'UNPROVEN'
        result=run.pending_closure() if run.sealed_command_count is not None else {'phase':phase,'cleanup':run.record.get('cleanup'),'exits':run.record['exits'],'held_pidfds':run.record['held_pidfds'],'recovery_owners':run.record.get('recovery_owners')}
        result.update(status=combined_verdict(base_status,run.accounting),behavior_status=primary,closure_status='PASS' if closure_ok else 'UNPROVEN',primary_error=run.record.get('error'),primary_traceback=run.record.get('traceback'),closure_errors=run.record.get('closure_errors',[]),end=now(),elapsed_seconds=elapsed,fd_before=before,fd_after=after,observed=None if observed is None else {'path':str(observed),'bytes':observed.stat().st_size,'sha256':digest(observed)},stage_statuses=[{'id':x['id'],'status':x['status']} for x in run.record['stages']])
        # Include future publication in accounting before either physical file is written.
        #64KiB bounds the oracle gate;256KiB remains for the wrapper/postcheck publication.
        run.accounting.check('final_publication',[result],future_bytes=65536 if phase=='oracle' else 0,reserve=PUBLICATION_RESERVE,admit=False)
        result.update(status=combined_verdict(base_status,run.accounting),logging=run.accounting.summary())
        run.accounting.check('final_publication_with_logging',[result],future_bytes=65536 if phase=='oracle' else 0,reserve=PUBLICATION_RESERVE,admit=False)
        result.update(status=combined_verdict(base_status,run.accounting),logging=run.accounting.summary())
        run.record['status']=result['status'];write_once(run.root/'RESULT.json',result)
    if phase=='oracle' and result['status']=='PASS':
        gate=oracle_gate(run.record,digest(run.root/'RESULT.json'));require(len(encoded_document(gate))<=65536,'reserved gate publication cap');write_once(root/'ORACLE-FROZEN.json',gate)
    physical,count=physical_log_bytes(root.parent)
    print(json.dumps({'phase':phase,'status':result['status'],'behavior_status':primary,'logging_status':result['logging']['status'],'closure_status':result['closure_status'],'result_sha256':digest(run.root/'RESULT.json'),'elapsed_seconds':elapsed,'log_bytes':physical,'physical_files':count,'stages':result['stage_statuses'],'not_run':run.record['not_run']}),flush=True)
    return 0 if result['status']=='PASS' and physical<=LOG_CAP else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness':witness(*sys.argv[2:6])
    elif len(sys.argv)==3 and sys.argv[1]=='prepare':prepare(json.loads(Path(sys.argv[2]).read_text()))
    elif len(sys.argv)==4 and sys.argv[1]=='run' and sys.argv[3] in ('oracle','candidate'):sys.exit(execute_phase(json.loads(Path(sys.argv[2]).read_text()),sys.argv[2],sys.argv[3]))
    else:raise SystemExit(__doc__)
