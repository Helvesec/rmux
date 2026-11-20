#!/usr/bin/env python3
"""Bounded Darwin renumber-windows proof; acquired011/012 APIs only.

Linux022 supplies a scenario, never Darwin mappings. The fresh full oracle
freezes every intermediate mapping before the candidate. No import of015.
Real stdin receipts and filesystem liveness use disjoint nonce channels.
"""
import base64
import copy
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import secrets
import select
import sys
import termios
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode = True
C = D = None
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{session_name}|#{window_index}|#{window_name}|#{window_width}|#{window_height}'
FIELDS = ('daemon_pid','socket','session_id','window_id','pane_id','pane_pid','pane_tty','pane_dead','pane_dead_status','pane_dead_signal','session_name','window_index','window_name','window_width','window_height')
WINDOW_FORMAT = '#{session_id}|#{session_name}|#{window_id}|#{window_index}|#{window_name}|#{window_panes}'
WINDOW_FIELDS = ('session_id','session_name','window_id','window_index','window_name','window_panes')
KINDS = ('a4','a6','a8','b4','a10','a12','a14')
STAGES = {'N1-created': ('a4','a6','a8','b4'), 'N1-closed6': ('a4','a8','b4'),
          'N2-set-on': ('a4','a8','b4'), 'N2-created10': ('a4','a8','b4','a10'),
          'N2-closed4': ('a8','b4','a10'), 'N3-input': ('a8','b4','a10'),
          'N4-unset': ('a8','b4','a10'), 'N4-created12-14': ('a8','b4','a10','a12','a14'),
          'N4-closed12': ('a8','b4','a10','a14')}
ENV_KEYS = ('HOME','XDG_CONFIG_HOME','PATH','L6_RENUMBER_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM','L5_RENUMBER_API')
FILE_CAP = 4096
RECORD_CAP = 1536 * 1024
CHECKPOINT_CAP = 65536
LOT_CAP = 2 * 1024**2
TEXT_CAP = 256 * 1024

class Unproven(RuntimeError): pass
class Failed(RuntimeError): pass
def require(ok, message):
    if not ok: raise Unproven(message)
def equal(actual, expected, message):
    if actual != expected: raise Failed(f'{message}: {actual!r} != {expected!r}')
def now(): return datetime.datetime.now(datetime.timezone.utc).isoformat()
def digest(path):
    with Path(path).open('rb') as f: return hashlib.file_digest(f,'sha256').hexdigest()
def pin(path):
    path = Path(path)
    return {'path':str(path),'bytes':path.stat().st_size,'sha256':digest(path)}
def saved(value): return copy.deepcopy(value)
def encoded(value): return (json.dumps(value,separators=(',',':'))+'\n').encode()
def write_once(path, value, cap=RECORD_CAP):
    raw = encoded(value); require(len(raw) <= cap,'pre-write file cap')
    with Path(path).open('xb') as f: f.write(raw); f.flush(); os.fsync(f.fileno())
def atomic_once(path, value):
    path = Path(path); require(not path.exists(),'immutable marker exists')
    tmp = path.with_suffix(path.suffix+'.tmp'); write_once(tmp,value,FILE_CAP); os.rename(tmp,path)
def stem(own): return str(own['pid'])+'-'+'-'.join(map(str,own['birth']))

def bind_api(config):
    global C,D,Unproven,Failed
    ap = config['adapter_pin']; require(pin(ap['path']) == ap,'adapter provenance')
    equal(ap['sha256'],'3bb2a8a9faba1b860f00d7641696af364fddbaa48e28b79d846fa0249b14a7bb','acquired012 adapter')
    spec = importlib.util.spec_from_file_location('renumber_acquired012',ap['path'])
    C = importlib.util.module_from_spec(spec); spec.loader.exec_module(C); C.bind_api(config['api_pins'])
    D = C.D; Unproven,Failed = D.Unproven,D.Failed
    C.ENV_KEYS = ENV_KEYS  # Scope the acquired parser's allowlist in memory only.

def environment(config, phase):
    root = Path(config['roots'][phase])
    return {'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'xdg'),'PATH':'/usr/bin:/bin',
            'SHELL':'/bin/bash','TERM':'vt100','LANG':'C','RMUX_DISABLE_TMUX_FALLBACK':'1',
            'RMUX_DISABLE_TMUX_SHIM':'1','L6_RENUMBER_MARKER':config['tokens'][phase],
            'L5_RENUMBER_API':config['config_path']}
def witness_argv(config, phase, label):
    return [config['pins']['python']['path'],'-I',config['driver'],'witness',label,
            str(Path(config['roots'][phase])/'markers'),config['tokens'][phase],'0']
def plan(config, phase):
    cwd = str(Path(config['roots'][phase])/'cwd')
    def session(name,label):
        return ['new-session','-d','-s',name,'-n',label,'-x','80','-y','24','-c',cwd,*witness_argv(config,phase,label)]
    def window(label,index):
        return ['new-window','-d','-t','alpha:'+str(index),'-n',label,'-c',cwd,*witness_argv(config,phase,label)]
    return [('N1','a4',session('alpha','a4'),'alpha:a4.0'),
            ('N1','a6',window('a6',6),'alpha:a6.0'),('N1','a8',window('a8',8),'alpha:a8.0'),
            ('N1','b4',session('beta','b4'),'beta:b4.0'),('N2','a10',window('a10',10),'alpha:a10.0'),
            ('N4','a12',window('a12',12),'alpha:a12.0'),('N4','a14',window('a14',14),'alpha:a14.0')]

def witness(kind, root, token, code):
    bind_api(json.loads(Path(os.environ['L5_RENUMBER_API']).read_text()))
    require(kind in KINDS and code == '0','unplanned witness')
    root = Path(root); own = D.birth(os.getpid()); key = stem(own)
    attr = termios.tcgetattr(0); tty = {'icanon':bool(attr[3]&termios.ICANON),'icrnl':bool(attr[0]&termios.ICRNL)}
    require(all(tty.values()),'canonical slave PTY required; no termios mutation')
    received,buffer = [],b''
    def report(challenge):
        return {'at':now(),'kind':kind,'token':token,'challenge':challenge,'exit_code':0,
                'exit_code_scope':'helper policy declaration only; product closure may signal it',
                'identity':D.birth(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,'tty':tty,
                'received':list(received),'pending_bytes_base64':base64.b64encode(buffer).decode(),
                'environment':{k:os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root/('born-'+key+'.json'),report(None)); end = time.monotonic()+180
    while time.monotonic() < end:
        while select.select([0],[],[],0)[0]:
            data = os.read(0,FILE_CAP); require(data,'unexpected stdin EOF')
            buffer += data; require(len(buffer) <= FILE_CAP and len(received) < 4,'stdin cap')
            while b'\n' in buffer:
                line,buffer = buffer.split(b'\n',1)
                require(len(line) == 48 and all(c in b'0123456789abcdef' for c in line),'unplanned stdin bytes')
                received.append(line.decode('ascii')); receipt = report(None)
                receipt['stdin_bytes_base64'] = base64.b64encode(line+b'\n').decode()
                atomic_once(root/(f'stdin-{key}-{len(received)}.json'),receipt)
        requests = sorted(root.glob('request-'+key+'-*.json')); require(len(requests) <= 20,'challenge count')
        for path in requests:
            ack = root/path.name.replace('request-','ack-',1)
            if not ack.exists():
                require(path.stat().st_size <= FILE_CAP,'request file cap'); request = json.loads(path.read_text())
                equal(set(request),{'identity','challenge','issued_at'},'liveness request schema')
                require(D.same(own,request['identity']),'challenge birth')
                atomic_once(ack,report(request['challenge']))
        time.sleep(.02)
    os._exit(124)

def parse_public(text):
    parts = text.split('|'); equal(len(parts),len(FIELDS),'public field count')
    row = dict(zip(FIELDS,parts))
    for k in ('daemon_pid','pane_pid','window_index'): require(row[k].isdigit(),'public number'); row[k] = int(row[k])
    return row
def parse_window(text):
    parts = text.split('|'); equal(len(parts),len(WINDOW_FIELDS),'window field count')
    row = dict(zip(WINDOW_FIELDS,parts))
    for k in ('window_index','window_panes'): require(row[k].isdigit(),'window number'); row[k] = int(row[k])
    return row
def stable_public(a,b):
    return {k:v for k,v in a.items() if k != 'window_index'} == {k:v for k,v in b.items() if k != 'window_index'}
def check_layout(rows,windows,children):
    equal(len(rows),len(children),'exact pane count'); equal(len(windows),len(children),'exact window count')
    for k in ('window_id','pane_id'): equal(len({r[k] for r in rows}),len(rows),'unique '+k)
    equal(len({r['window_id'] for r in windows}),len(windows),'unique listed window')
    equal(len({(r['session_id'],r['window_index']) for r in windows}),len(windows),'unique session/index')
    logical,sessions = [],{}
    for label,child in children.items():
        old = child['public']; found = [r for r in rows if r['pane_id'] == old['pane_id']]
        require(len(found) == 1 and stable_public(old,found[0]),'stable pane/window attribution')
        row = found[0]; name = 'beta' if label == 'b4' else 'alpha'
        equal(row['session_name'],name,'session name'); equal(row['window_name'],label,'stable window name')
        equal((row['window_width'],row['window_height']),('80','24'),'fixed geometry')
        equal((row['pane_dead'],row['pane_dead_status'],row['pane_dead_signal']),('0','',''),'live status')
        require(row['window_index'] >= 4,'below fixed base4')
        if label == 'b4': equal(row['window_index'],4,'independent beta')
        matches = [w for w in windows if w['window_id'] == row['window_id']]
        equal(matches,[{**{k:row[k] for k in WINDOW_FIELDS if k != 'window_panes'},'window_panes':1}],'pane/window consistency')
        if name in sessions: equal(sessions[name],row['session_id'],'stable session ID')
        sessions[name] = row['session_id']; logical.append({'label':label,'session_name':name,'window_index':row['window_index']})
    require(len(sessions) <= 2 and len(set(sessions.values())) == len(sessions),'distinct bounded sessions')
    return sorted(logical,key=lambda r:(r['session_name'],r['window_index']))
def check_mapping(actual,expected): equal(actual,expected,'exact Mac oracle mapping and order')
def check_delivery(before,after,label,nonce):
    equal(set(before),set(after),'input population'); require(label in before and all(nonce not in v for v in before.values()),'stale input/absent target')
    for name,values in before.items(): equal(after[name],values+([nonce] if name == label else []),'exclusive stdin '+name)
def check_marker(marker,child,expected,challenge):
    C.check_marker(marker,child,expected,challenge); equal(marker['exit_code'],0,'helper declaration only')
    equal(marker['tty'],{'icanon':True,'icrnl':True},'observed canonical PTY')
def check_stdin(receipt,nonce,expected):
    equal(base64.b64decode(receipt['stdin_bytes_base64'],validate=True),(nonce+'\n').encode(),'actual stdin bytes')
    equal(receipt['received'],expected,'input history'); equal(receipt['pending_bytes_base64'],'','partial input')
def check_exit(row):
    require(row['exit_event'] and not D.same(row['before'],row['after']),'NOTE_EXIT and absent birth')
    equal(row['numeric_status_claimed'],False,'no indirect numeric status credit')
def check_cleanup(row,owned):
    C.check_cleanup(row)
    expected = {(p['pid'],tuple(p['birth'])) for p in owned.values()}
    actual = {(p['before']['pid'],tuple(p['before']['birth'])) for p in row['processes']}
    require(expected and expected == actual and len(row['processes']) == len(expected),'cleanup population complete')
def command_budget(record,text_budget,cleaning=False):
    require(len(record['commands']) < (220 if cleaning else 218),'command cap before exec')
    used = sum(len(e.get(k,'').encode()) for e in record['commands'] for k in ('stdout','stderr'))
    require(used+2*FILE_CAP <= text_budget,'remaining combined text reserve before exec')
    require(len(encoded(record)) < RECORD_CAP-96*1024,'record reserve before exec')

class Run:
    def __init__(self,frozen):
        D.Run.__init__(self,frozen,None); self.config = frozen['config']; self.env = environment(self.config,self.phase)
        self.children = self.observed; self.closed = set(); self.received = {}; self.nonces = set(); self.mappings = {}
        self.oracle = frozen.get('oracle_mappings'); self.text_budget = frozen['text_budget']
        self.record.update(environment=self.env,layouts=[],deliveries=[],closures=[],marker_ledgers=[])
    def remaining(self):
        end = min(self.cleanup_end,self.start+180) if self.cleaning else min(self.case_end,self.start+150)
        left = end-time.monotonic(); require(left > 0,'native deadline'); return min(left,5)
    def socket_info(self): return D.Run.socket_info(self)
    def profiles(self,post_shutdown=False): return C.Run.profiles(self,post_shutdown)
    def wait_json(self,path): return C.Run.wait_json(self,path)
    def snapshot(self,pid): return C.Run.snapshot(self,pid)
    def active(self): return {k:v for k,v in self.children.items() if k not in self.closed}
    def track(self,row,role):
        result = D.Run.track(self,row,role); require(len(self.owned) <= 9,'daemon+eight births cap'); return result
    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent,D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own = D.birth(pid)
                if own is None or own['ppid'] != parent['pid'] or pid in self.owned: continue
                self.track(own,'unexpected-helper'); self.record['helpers'].append({'at':now(),'parent':parent,'identity':own})
                require(self.cleaning,'unexpected helper')
    def checkpoint(self):
        # Complete raw data is retained once in RESULT. Checkpoints keep the
        # latest evidence and counts, avoiding a second full result in 2MiB.
        row = {'at':now(),'phase':self.phase,'cases':saved(self.record['cases']),
               'counts':{k:len(self.record[k]) for k in ('commands','observations','layouts','closures','deliveries')},
               'last_observation':saved(self.record['observations'][-1:]),'cleanup':self.record.get('cleanup')}
        raw = encoded(row); require(len(raw) <= CHECKPOINT_CAP,'checkpoint cap')
        temp = self.root/'progress.tmp'; temp.write_bytes(raw); os.replace(temp,self.root/'progress.json')
    def fresh_nonce(self):
        nonce = secrets.token_hex(24); require(nonce not in self.nonces,'duplicate nonce'); self.nonces.add(nonce); return nonce
    def command(self,args,startup=False,required=True):
        command_budget(self.record,self.text_budget,self.cleaning)
        argv = [self.binary,*([] if startup else ['-N']),'-S',str(self.socket)]
        if startup: argv += ['-f',str(self.root/'startup.conf')]
        argv += args; entry = {'start':now(),'argv':argv,'pre_exec_file_cap':FILE_CAP}; self.record['commands'].append(entry)
        old = resource.getrlimit(resource.RLIMIT_FSIZE)
        require(old[0] == resource.RLIM_INFINITY or old[0] >= FILE_CAP,'file limit too small')
        try:
            resource.setrlimit(resource.RLIMIT_FSIZE,(FILE_CAP,old[1]))
            with chdir(self.root): result = D.native.run(argv,env=self.env,timeout=self.remaining(),check=False)
            entry.update(pid=result.pid,returncode=result.returncode,stdout=result.stdout.decode(),stderr=result.stderr.decode())
            require(len(result.stdout) < FILE_CAP and len(result.stderr) < FILE_CAP,'CLI file cap reached')
        except BaseException as error: entry['error'] = repr(error); raise
        finally: resource.setrlimit(resource.RLIMIT_FSIZE,old); entry['end'] = now()
        if required: require(result.returncode == 0,'CLI failed '+repr(args))
        return result.stdout.decode().strip()
    def public(self,target):
        public = parse_public(self.command(['display-message','-p','-t',target,FORMAT]))
        require(public['socket'] == str(self.socket),'endpoint escaped'); endpoint = self.socket_info(); require(endpoint is not None,'socket missing')
        daemon = D.process(public['daemon_pid']); self.track(daemon['before'],'daemon')
        key = 'tmux' if self.phase == 'oracle' else 'rmux_daemon'
        equal(daemon['image']['executable'],self.config['pins'][key]['path'],'daemon path'); equal(daemon['image']['sha256'],self.config['pins'][key]['sha256'],'daemon disk SHA')
        if self.daemon is None: self.daemon,self.endpoint = daemon,endpoint
        else: require(D.stable(self.daemon,daemon) and self.endpoint == endpoint,'daemon/socket changed')
        item = {'at':now(),'target':target,'public':public,'daemon':daemon,'endpoint':endpoint}
        self.record['public_samples'].append(item); return item
    def expected_for(self,label):
        p = self.config['pins']['python']
        return {'kind':label,'token':self.config['tokens'][self.phase],'exe':p['path'],'exe_sha256':p['sha256'],
                'argv':witness_argv(self.config,self.phase,label),'cwd':str(self.root/'cwd'),
                'environment':{k:self.env.get(k) for k in ENV_KEYS}}
    def observe(self,label,target,reobserve=False):
        pub = self.public(target); public = pub['public']; own = D.birth(public['pane_pid']); require(own is not None,'child missing'); self.track(own,'pane')
        key = stem(own); markers = self.root/'markers'; index = len(self.record['observations'])
        item = {'at':now(),'label':label,'target':target,'reobserve':reobserve,'public':public,'expected':self.expected_for(label),'status':'UNPROVEN'}
        self.record['observations'].append(item)
        try:
            path = markers/('born-'+key+'.json'); born = self.wait_json(path); first = self.snapshot(own['pid'])
            item.update(first=first,born=born,born_path=str(path)); self.checkpoint()
            check_marker(born,first,item['expected'],None); equal(born['received'],[],'new witness starts empty')
            nonce = self.fresh_nonce(); request = {'identity':own,'challenge':nonce,'issued_at':now()}
            path = markers/(f'request-{key}-{index}.json'); item.update(request=request,request_path=str(path)); atomic_once(path,request)
            ack_path = markers/path.name.replace('request-','ack-',1); ack = self.wait_json(ack_path); second = self.snapshot(own['pid'])
            item.update(ack=ack,ack_path=str(ack_path),child=second); after = self.public(target); item['public_after'] = after['public']
            equal(after['public'],public,'public stable across challenge'); C.check_child(first,second,public,after['daemon'],item['expected'])
            check_marker(ack,second,item['expected'],nonce); equal(ack['pending_bytes_base64'],'','no partial stdin')
            if reobserve:
                old = self.children[label]; require(D.stable(old['child'],second) and stable_public(old['public'],public),'old birth/ID changed')
            else: self.children[label] = item
            self.helpers(); item['status'] = 'PASS'; return item
        except Exception as error: item.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error)); raise
        finally: self.checkpoint()
    def options(self,target,effective,local):
        equal(self.command(['show-options','-gv','base-index']),'4','global base4')
        equal(self.command(['show-options','-Av','-t',target,'base-index']),'4','effective base4')
        equal(self.command(['show-options','-gv','renumber-windows']),'off','global renumberoff')
        equal(self.command(['show-options','-Av','-t',target,'renumber-windows']),effective,'effective renumber')
        equal(self.command(['show-options','-qv','-t',target,'renumber-windows']),local or '','local renumber')
    def layout(self,stage):
        equal(tuple(self.active()),STAGES[stage],'stage population')
        observations = []
        for label,old in self.active().items():
            observations.append(len(self.record['observations'])); obs = self.observe(label,old['public']['pane_id'],True)
            equal(obs['ack']['received'],self.received[label],'stdin ledger at layout')
        item = {'stage':stage,'at':now(),'observations':observations,'samples':[],'status':'UNPROVEN'}; self.record['layouts'].append(item)
        try:
            for _ in range(2):
                rows = [parse_public(t) for t in self.command(['list-panes','-a','-F',FORMAT]).splitlines()]
                windows = [parse_window(t) for t in self.command(['list-windows','-a','-F',WINDOW_FORMAT]).splitlines()]
                logical = check_layout(rows,windows,self.active())
                item['samples'].append({'at':now(),'panes':rows,'windows':windows,'logical':saved(logical)})
                time.sleep(min(.03,self.remaining()))
            first,second = item['samples']; equal(first['panes'],second['panes'],'double pane inventory'); equal(first['windows'],second['windows'],'double window inventory')
            if stage == 'N1-created': equal({r['label']:r['window_index'] for r in logical},{'a4':4,'a6':6,'a8':8,'b4':4},'initial explicit indices')
            if stage == 'N1-closed6': equal({r['label']:r['window_index'] for r in logical},{'a4':4,'a8':8,'b4':4},'off keeps gap')
            if stage == 'N2-closed4': equal([r['label'] for r in logical if r['session_name']=='alpha'],['a8','a10'],'relative identity order')
            if self.phase == 'candidate': check_mapping(logical,self.oracle[stage])
            self.mappings[stage] = saved(logical); item.update(logical=saved(logical),status='PASS'); return item
        except Exception as error: item.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error)); raise
        finally: self.checkpoint()
    def delivery(self,target,address_kind):
        label = 'a10'; child = self.children[label]; nonce = self.fresh_nonce(); before = saved({k:self.received[k] for k in self.active()})
        public = self.public(target)['public']; require(stable_public(child['public'],public),'wrong input target')
        e = {'at':now(),'target':target,'address_kind':address_kind,'target_label':label,'nonce':nonce,'public_before':saved(public),'received_before':saved(before),'status':'UNPROVEN'}
        self.record['deliveries'].append(e)
        # This input nonce is never placed in a filesystem request to a helper.
        self.command(['send-keys','-t',target,'-l',nonce]); self.command(['send-keys','-t',target,'Enter'])
        path = self.root/'markers'/(f"stdin-{stem(child['child']['before'])}-{len(before[label])+1}.json")
        receipt = self.wait_json(path); e.update(receipt=saved(receipt),receipt_path=str(path)); check_marker(receipt,child['child'],child['expected'],None)
        check_stdin(receipt,nonce,before[label]+[nonce]); after = {}; e['observations'] = []
        for name,original in self.active().items():
            e['observations'].append(len(self.record['observations'])); obs = self.observe(name,original['public']['pane_id'],True); after[name] = saved(obs['ack']['received'])
        check_delivery(before,after,label,nonce); e['received_after'] = saved(after)
        for k,v in after.items(): self.received[k] = saved(v)
        public_after = self.public(target)['public']; equal(public_after,public,'input target after delivery')
        e.update(public_after=saved(public_after),status='PASS',end=now()); self.checkpoint()
    def close_owned(self,labels,final=False):
        require(labels and all(k in self.active() for k in labels),'only live owned windows closed')
        require(final or len(labels) == 1,'one window closure')
        group = {'at':now(),'labels':list(labels),'final':final,'processes':[],'status':'UNPROVEN'}; self.record['closures'].append(group)
        for label in labels:
            child = self.children[label]; own = child['child']['before']; snap = self.snapshot(own['pid']); watch = self.watches[own['pid']]
            require(D.stable(child['child'],snap) and not watch.poll_exit() and D.same(own,D.birth(own['pid'])),'live closure identity')
            group['processes'].append({'label':label,'before':saved(own),'snapshot_before':snap,'numeric_status_claimed':False})
        if final:
            own = self.daemon['before']; daemon = D.process(own['pid'])
            require(D.stable(self.daemon,daemon) and self.socket_info() == self.endpoint and not self.watches[own['pid']].poll_exit(),'private daemon before close')
            group['processes'].append({'label':'daemon','before':saved(own),'snapshot_before':daemon,'numeric_status_claimed':False})
            args = ['kill-server']
        else:
            child = self.children[labels[0]]; pub = self.public(child['public']['pane_id'])['public']
            require(stable_public(child['public'],pub),'window closure target changed')
            args = ['kill-window','-t',child['public']['window_id']]
        group.update(command=args,command_index=len(self.record['commands']),issued_at=now()); self.command(args)
        for row in group['processes']:
            own = row['before']; watch = self.watches[own['pid']]; watch.wait(self.remaining()); after = D.birth(own['pid'])
            while D.same(own,after): time.sleep(min(.02,self.remaining())); after = D.birth(own['pid'])
            row.update(exit_event=watch.exited,after=after); check_exit(row)
        self.closed.update(labels); group.update(status='PASS',end=now()); self.checkpoint()
    def spawn(self,label):
        rows = [r for r in plan(self.config,self.phase) if r[1]==label]; require(len(rows)==1 and self.spawn_count<7,'planned birth cap')
        _,_,args,target = rows[0]; self.spawn_count += 1; self.command(args,startup=label=='a4'); item = self.observe(label,target)
        equal(item['public']['window_name'],label,'new window label'); equal(item['ack']['received'],[],'new stdin empty'); self.received[label] = []
    def ledger(self): return C.Run.ledger(self)
    def execute(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh fixture'); self.profiles()
        for case in ('N1','N2','N3','N4'):
            self.case_end = min(time.monotonic()+20,self.start+150); entry = {'id':case,'start':now(),'monotonic_start':time.monotonic()}; self.record['cases'].append(entry)
            try:
                if case == 'N1':
                    for label in ('a4','a6','a8','b4'): self.spawn(label)
                    self.options('alpha','off',None); self.options('beta','off',None)
                    self.layout('N1-created'); self.close_owned(['a6']); self.layout('N1-closed6')
                elif case == 'N2':
                    self.command(['set-option','-t','alpha','renumber-windows','on']); self.options('alpha','on','on'); self.options('beta','off',None)
                    self.layout('N2-set-on'); self.spawn('a10'); self.layout('N2-created10'); self.close_owned(['a4']); self.layout('N2-closed4')
                elif case == 'N3':
                    mapping = self.mappings['N2-closed4'] if self.phase=='oracle' else self.oracle['N2-closed4']
                    index = next(r['window_index'] for r in mapping if r['label']=='a10')
                    self.delivery(f'alpha:{index}.0','numeric'); self.delivery(self.children['a10']['public']['pane_id'],'stable-pane-ID'); self.layout('N3-input')
                else:
                    self.command(['set-option','-g','renumber-windows','off']); self.command(['source-file',str(self.root/'live.conf')])
                    self.options('alpha','off',None); self.options('beta','off',None); self.layout('N4-unset')
                    self.spawn('a12'); self.spawn('a14'); self.layout('N4-created12-14'); before = saved(self.mappings['N4-created12-14'])
                    self.close_owned(['a12']); after = self.layout('N4-closed12')['logical']; equal(after,[r for r in before if r['label']!='a12'],'unsetoff keeps all surviving gaps')
                self.ledger(); self.profiles(); self.remaining(); entry['status'] = 'PASS'
            except Exception as error: entry.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error)); raise
            finally: entry.update(end=now(),seconds=time.monotonic()-entry['monotonic_start']); self.checkpoint()
        self.case_end = min(time.monotonic()+20,self.start+150); self.close_owned(list(self.active()),final=True); self.ledger()
    def cleanup(self):
        self.cleaning = True; self.cleanup_end = min(time.monotonic()+30,self.start+180)
        row = {'start':now(),'errors':[],'rescues':[],'processes':[]}; self.record['cleanup'] = row
        def attempt(name,fn):
            try: return fn()
            except Exception as error: row['errors'].append({'step':name,'error':repr(error)})
        if self.daemon is None and self.socket.exists(): attempt('recover-endpoint',lambda:self.public('alpha'))
        if self.daemon:
            own = self.daemon['before']; watch = self.watches[own['pid']]
            def shutdown():
                # After retained NOTE_EXIT, only birth absence is required: do
                # not request argv/cwd of a process already closed by product.
                if watch.poll_exit(): row['daemon_already_exited'] = True; return
                if not D.same(own,D.birth(own['pid'])): watch.wait(self.remaining()); row['daemon_already_exited'] = True; return
                require(D.stable(self.daemon,D.process(own['pid'])) and self.socket_info()==self.endpoint,'cleanup ownership')
                self.helpers(); self.command(['kill-server'],required=False); watch.wait(self.remaining())
            attempt('qualified-shutdown',shutdown)
        for pid,old in list(self.owned.items()):
            watch = self.watches[pid]; attempt('wait-'+str(pid),lambda:watch.wait(self.remaining()))
            if not watch.poll_exit() and D.same(old,D.birth(pid)):
                row['rescues'].append({'identity':old,'signal':9}); attempt('rescue-'+str(pid),lambda:watch.retire_after_failure(self.remaining()))
            try:
                collected,status = os.waitpid(pid,os.WNOHANG); reaped = {'pid':collected,'raw_status':status}
            except ChildProcessError: reaped = {'ECHILD':True,'note':'indirect; NOTE_EXIT and birth absence checked separately'}
            after = D.birth(pid)
            while D.same(old,after) and time.monotonic()<self.cleanup_end: time.sleep(.02); after = D.birth(pid)
            row['processes'].append({'before':old,'after':after,'exit_event':watch.exited,'waitpid':reaped})
        row['all_absent'] = all(p['exit_event'] and not D.same(p['before'],p['after']) for p in row['processes'])
        if row['all_absent'] and self.socket.exists() and self.socket_info()==self.endpoint: self.socket.unlink(); row['owned_socket_residue_removed'] = True
        row['socket_absent'] = not self.socket.exists(); attempt('profiles',lambda:self.profiles(True))
        for watch in self.watches.values(): watch.close()
        row['fd_after'] = D.fd_snapshot(); row['fds_equal'] = row['fd_after']==self.record['fd_before']; row['end'] = now()
        row['pass'] = bool(self.daemon and row['all_absent'] and row['socket_absent'] and row['fds_equal'] and not row['errors'] and not row['rescues'] and time.monotonic()<=self.cleanup_end)
        self.checkpoint(); check_cleanup(row,self.owned)

def verify_frozen(frozen):
    require(frozen['phase'] in ('oracle','candidate'),'phase'); config = frozen['config']
    for p in frozen['artifacts']: require(pin(p['path'])==p,'frozen pin changed: '+p['path'])
    paths = [str(Path(__file__).resolve()),config['config_path'],config['adapter_pin']['path'],frozen['pure']['path'],
             *[p['path'] for p in config['pins'].values()],*[p['path'] for p in config['api_pins']]]
    paths += [str(Path(r)/n) for r in config['roots'].values() for n in ('startup.conf','live.conf')]
    require(set(paths)<={p['path'] for p in frozen['artifacts']},'mandatory provenance')
    equal(config,json.loads(Path(config['config_path']).read_text()),'config'); equal(frozen['plan'],json.loads(json.dumps(plan(config,frozen['phase']))),'plan')
    equal(frozen['environment'],environment(config,frozen['phase']),'environment'); equal(frozen['pins'],config['pins'],'binaries'); equal(frozen['root'],config['roots'][frozen['phase']],'root')
    pure = json.loads(Path(frozen['pure']['path']).read_text()); require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure stale')
    require(not (Path(frozen['root'])/'STARTED.json').exists(),'lot already attempted')
    require(0<frozen['text_budget']<=TEXT_CAP and frozen['prior_text_bytes']+frozen['text_budget']<=TEXT_CAP,'cumulative text budget')
    if frozen['phase']=='candidate':
        p = frozen['oracle_gate']; require(pin(p['path'])==p,'oracle gate pin'); gate = json.loads(Path(p['path']).read_text())
        require(gate['status']=='PASS' and pin(gate['result']['path'])==gate['result'],'oracle complete result')
        equal(gate['driver_sha256'],digest(__file__),'oracle driver'); equal(list(gate['mappings']),list(STAGES),'complete oracle stages')
        equal(frozen['oracle_mappings'],gate['mappings'],'exact oracle mappings'); equal(set(gate['argv']),set(KINDS),'oracle population')
        for label in KINDS: equal(gate['argv'][label],witness_argv(config,'oracle',label),'raw oracle argv')
        equal(gate['deliveries'],['numeric','stable-pane-ID'],'oracle stdin matrix')

def execute(frozen_path):
    frozen = json.loads(frozen_path.read_text()); verify_frozen(frozen); bind_api(frozen['config']); run = Run(frozen)
    write_once(run.root/'STARTED.json',{'at':now(),'frozen':pin(frozen_path),'observer':D.birth(os.getpid())})
    try: run.execute(); run.record['status'] = 'PASS'
    except BaseException as error: run.record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=repr(error),traceback=traceback.format_exc())
    finally:
        try: run.cleanup()
        except BaseException as error: run.record.update(cleanup_exception=repr(error),status='UNPROVEN')
        run.record.update(end=now(),seconds=time.monotonic()-run.start,pane_spawn_commands=run.spawn_count,frozen=pin(frozen_path),distinct_nonces=len(run.nonces),
                          limits=['Disk SHA is not mapped-image attestation.','Own-child sampling is not exhaustive transient-helper coverage.',
                                  'Indirect NOTE_EXIT/ECHILD is not author reap or numeric status.','No host termios mutation or restoration credit.'])
        other = sum(p.stat().st_size for p in run.root.rglob('*') if p.is_file())
        require(len(encoded(run.record))+other+16384 < LOT_CAP,'lot logs plus outer/gate reserve')
        write_once(run.root/'RESULT.json',run.record)
    if run.record['status']=='PASS' and run.phase=='oracle':
        write_once(run.root/'ORACLE-FROZEN.json',{'at':now(),'status':'PASS','result':pin(run.root/'RESULT.json'),'driver_sha256':digest(__file__),
                   'mappings':saved(run.mappings),'argv':{k:v['child']['args']['argv'] for k,v in run.children.items()},
                   'deliveries':[e['address_kind'] for e in run.record['deliveries']]})
    print(json.dumps({'status':run.record['status'],'result':pin(run.root/'RESULT.json'),'seconds':run.record['seconds']}),flush=True)
    return 0 if run.record['status']=='PASS' else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness': witness(*sys.argv[2:])
    elif len(sys.argv)==3 and sys.argv[1]=='--frozen': raise SystemExit(execute(Path(sys.argv[2])))
    else: raise SystemExit('Use --frozen FILE.json, or private witness mode.')
