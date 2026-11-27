#!/usr/bin/env python3
"""Bounded Darwin destroy-unattached D1-D4, acquired011/012 APIs read-only.

Nine live challenges and eighteen raw child snapshots per product. JSON value
sharing is lossless and decoded before every audit; identities and nonces are
never omitted. No017 import, new FFI, attach or shared daemon operation.
"""
import copy
import datetime
import errno
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import socket
import sys
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode = True
C = D = None
KINDS = ('alpha','beta','gamma')
SURVIVORS = {'D1':list(KINDS),'D2':['beta','gamma'],'D3':['beta'],'D4':[]}
ENV_KEYS = ('HOME','XDG_CONFIG_HOME','PATH','L6_DESTROY_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM','L5_DESTROY_API')
FIELDS = ('daemon_pid','socket','session_id','window_id','pane_id','pane_pid','pane_tty','pane_dead','pane_dead_status','session_attached','session_name','window_name','window_width','window_height')
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_dead}|#{pane_dead_status}|#{session_attached}|#{session_name}|#{window_name}|#{window_width}|#{window_height}'
SESSION_KEYS = ('session_id','session_name','session_attached','session_windows')
SESSION_FORMAT = '#{session_id}|#{session_name}|#{session_attached}|#{session_windows}'
FILE_CAP = 4096
RECORD_CAP = 192*1024
LOT_CAP = 256*1024

class Unproven(RuntimeError): pass
class Failed(RuntimeError): pass
def require(ok,message):
    if not ok: raise Unproven(message)
def equal(actual,expected,message):
    if actual != expected: raise Failed(f'{message}: {actual!r} != {expected!r}')
def now(): return datetime.datetime.now(datetime.timezone.utc).isoformat()
def digest(path):
    with Path(path).open('rb') as f: return hashlib.file_digest(f,'sha256').hexdigest()
def pin(path):
    path=Path(path); return {'path':str(path),'bytes':path.stat().st_size,'sha256':digest(path)}
def saved(value): return copy.deepcopy(value)
def encoded(value): return (json.dumps(value,separators=(',',':'))+'\n').encode()
def write_once(path,value,cap=RECORD_CAP):
    raw=encoded(value); require(len(raw)<=cap,'file cap before open')
    with Path(path).open('xb') as f: f.write(raw); f.flush(); os.fsync(f.fileno())
def atomic_once(path,value):
    path=Path(path); require(not path.exists(),'immutable marker exists'); temp=path.with_suffix(path.suffix+'.tmp')
    write_once(temp,value,FILE_CAP); os.rename(temp,path)

def pack(value):
    """Content-equal containers >=256 bytes share one value, with backward refs."""
    values=[]; seen={}
    def visit(obj):
        if not isinstance(obj,(dict,list)): return obj
        require(not isinstance(obj,dict) or set(obj)!={'$ref'},'reserved ref key')
        key=json.dumps(obj,sort_keys=True,separators=(',',':')); shared=len(key.encode())>=256
        if shared and key in seen: return {'$ref':seen[key]}
        out={k:visit(v) for k,v in obj.items()} if isinstance(obj,dict) else [visit(v) for v in obj]
        if not shared: return out
        index=len(values); values.append(out); seen[key]=index; return {'$ref':index}
    root=visit(value)
    return {'encoding':'shared-json-v1','root':root,'values':values}

def unpack(document):
    require(isinstance(document,dict),'shared JSON envelope must be object')
    equal(set(document),{'encoding','root','values'},'shared JSON schema'); equal(document['encoding'],'shared-json-v1','encoding')
    require(isinstance(document['values'],list),'shared JSON values must be list')
    values=[]
    def visit(obj):
        if isinstance(obj,dict):
            if set(obj)=={'$ref'}:
                index=obj['$ref']; require(type(index) is int and 0<=index<len(values),'backward reference required')
                return saved(values[index])
            return {k:visit(v) for k,v in obj.items()}
        if isinstance(obj,list): return [visit(v) for v in obj]
        return obj
    for value in document['values']:
        require(isinstance(value,(dict,list)),'shared JSON entry must be container')
        values.append(visit(value))
    return visit(document['root'])
def read_record(path): return unpack(json.loads(Path(path).read_text()))

def bind_api(config):
    global C,D,Unproven,Failed
    ap=config['adapter_pin']; require(pin(ap['path'])==ap,'adapter provenance')
    equal(ap['sha256'],'3bb2a8a9faba1b860f00d7641696af364fddbaa48e28b79d846fa0249b14a7bb','acquired012 adapter')
    spec=importlib.util.spec_from_file_location('destroy_acquired012',ap['path']); C=importlib.util.module_from_spec(spec); spec.loader.exec_module(C)
    C.bind_api(config['api_pins']); D=C.D; Unproven,Failed=D.Unproven,D.Failed; C.ENV_KEYS=ENV_KEYS

def environment(config,phase):
    root=Path(config['roots'][phase])
    return {'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'xdg'),'PATH':'/usr/bin:/bin','SHELL':'/bin/bash','TERM':'vt100','LANG':'C',
            'RMUX_DISABLE_TMUX_FALLBACK':'1','RMUX_DISABLE_TMUX_SHIM':'1','L6_DESTROY_MARKER':config['tokens'][phase],'L5_DESTROY_API':config['config_path']}
def witness_argv(config,phase,label):
    return [config['pins']['python']['path'],'-I',config['driver'],'witness',label,str(Path(config['roots'][phase])/'markers'),config['tokens'][phase],'0']
def spawn_plan(config,phase,label):
    return ['new-session','-d','-P','-F','#{pane_id}','-s',label,'-n',label,'-x','80','-y','24','-c',str(Path(config['roots'][phase])/'cwd'),*witness_argv(config,phase,label)]
def witness(label,root,token,code):
    bind_api(json.loads(Path(os.environ['L5_DESTROY_API']).read_text())); require(label in KINDS and code=='0','planned helper')
    root=Path(root); own=D.birth(os.getpid()); key=C.stem(own)
    def report(challenge):
        return {'at':now(),'kind':label,'token':token,'challenge':challenge,'exit_code':0,'exit_code_scope':'intention only; product destruction may signal helper',
                'identity':D.birth(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,'environment':{k:os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root/('born-'+key+'.json'),report(None)); end=time.monotonic()+120
    while time.monotonic()<end:
        requests=sorted(root.glob('request-'+key+'-*.json')); require(len(requests)<=9,'challenge cap')
        for path in requests:
            ack=root/path.name.replace('request-','ack-',1)
            if not ack.exists():
                require(path.stat().st_size<=FILE_CAP,'request cap'); request=json.loads(path.read_text())
                equal(set(request),{'identity','challenge','issued_at'},'challenge schema'); require(D.same(own,request['identity']),'fresh challenge birth')
                atomic_once(ack,report(request['challenge']))
        time.sleep(.02)
    os._exit(124)

def parse_sessions(raw):
    rows=[]
    for line in raw.splitlines():
        parts=line.split('|'); equal(len(parts),4,'session fields'); row=dict(zip(SESSION_KEYS,parts))
        require(row['session_attached'].isdigit() and row['session_windows'].isdigit(),'session integer fields')
        row['session_attached']=int(row['session_attached']); row['session_windows']=int(row['session_windows']); rows.append(row)
    return sorted(rows,key=lambda r:r['session_name'])
def check_inventory(rows,children):
    labels=[]; ids=[]
    for row in rows:
        name=row['session_name']; require(name in children,'unknown private session')
        equal(row['session_id'],children[name]['public']['session_id'],'original session ID')
        equal((row['session_attached'],row['session_windows']),(0,1),'one window detached session'); labels.append(name); ids.append(row['session_id'])
    require(len(labels)==len(set(labels)) and len(ids)==len(set(ids)),'duplicate session')
    return sorted(labels)
def check_scopes(row,global_value,local_values):
    equal(row['global'],global_value,'global destroy-unattached'); equal(row['exit_empty'],'off','daemon stays with no sessions')
    equal(set(row['sessions']),set(local_values),'live session scopes')
    for label,local in local_values.items():
        equal(row['sessions'][label],{'local':'' if local is None else local,'effective':global_value if local is None else local},'local/effective '+label)
def check_exit(row):
    require(row['exit_event'] and not D.same(row['before'],row['after']),'NOTE_EXIT plus birth absence')
    equal(row['numeric_status_claimed'],False,'no fabricated helper status')
def check_pair(pair,children,daemon,endpoint):
    equal(len(pair['samples']),2,'two inventories'); first,second=pair['samples']; equal(first['sessions'],second['sessions'],'stable inventories')
    for sample in pair['samples']:
        require(D.stable(daemon,sample['daemon']),'same live daemon'); equal(sample['endpoint'],endpoint,'same endpoint')
        require(not sample['daemon_exit_event'],'empty daemon exited'); check_inventory(sample['sessions'],children)
    equal(pair['survivors'],check_inventory(first['sessions'],children),'exact survivor list')
def check_probe(row):
    require(row['accepting'] is False and row['errno'] in (errno.ENOENT,errno.ECONNREFUSED),'absent listener needs non-timeout refusal/absence')
def check_unlink(endpoint,current,identities,probe):
    require(identities and all(p['exit_event'] and not D.same(p['before'],p['after']) for p in identities),'all attributed births absent before unlink')
    require(endpoint is not None and endpoint==current and current['uid']==os.getuid(),'exact owned endpoint')
    check_probe(probe); equal(probe['errno'],errno.ECONNREFUSED,'stale pathname must refuse connection'); equal(probe['path_state'],current,'fresh probe inode')
def check_cleanup(row,owned):
    C.check_cleanup(row); expected={(p['pid'],tuple(p['birth'])) for p in owned}; actual={(p['before']['pid'],tuple(p['before']['birth'])) for p in row['processes']}
    require(expected and expected==actual and len(row['processes'])==len(expected),'complete cleanup population')
def check_budget(record):
    require(len(record['commands'])<96,'CLI count before exec')
    require(sum(len(e.get(k,'').encode()) for e in record['commands'] for k in ('stdout','stderr'))+2*FILE_CAP<=64*1024,'text reserve before exec')
    require(len(encoded(record))<768*1024,'raw memory record bound'); require(len(encoded(pack(record)))<RECORD_CAP-24*1024,'shared record reserve before exec')

def projection(record):
    return {'survivors':{e['case']:e['survivors'] for e in record['inventories']},
            'mutations':[{k:e[k] for k in ('case','args','returncode','stdout','stderr')} for e in record['mutations']],
            'scopes':[{k:e[k] for k in ('case','point','global','exit_empty','sessions')} for e in record['scopes']]}

class Run:
    def __init__(self,frozen):
        D.Run.__init__(self,frozen,None); self.config=frozen['config']; self.env=environment(self.config,self.phase); self.children=self.observed
        self.alive=[]; self.nonces=set(); self.oracle=frozen.get('oracle_projection'); self.exited=set()
        self.record.update(environment=self.env,marker_ledgers=[],held_watches=[],scopes=[],mutations=[],inventories=[],exits=[],endpoint_probes=[])
    def remaining(self):
        end=min(self.cleanup_end,self.start+120) if self.cleaning else min(self.case_end,self.start+90)
        left=end-time.monotonic(); require(left>0,'lot/transition deadline'); return min(left,5)
    def socket_info(self): return D.Run.socket_info(self)
    def profiles(self,post_shutdown=False): return C.Run.profiles(self,post_shutdown)
    def wait_json(self,path): return C.Run.wait_json(self,path)
    def snapshot(self,pid): return C.Run.snapshot(self,pid)
    def ledger(self): return C.Run.ledger(self)
    def track(self,row,role):
        new=row['pid'] not in self.owned; value=D.Run.track(self,row,role); require(len(self.owned)<=4,'one daemon three helpers')
        if new:
            watch=self.watches[row['pid']]; current=D.birth(row['pid']); exited=watch.poll_exit()
            self.record['held_watches'].append({'at':now(),'identity':saved(row),'role':role,'after_register':current,'exit_event':exited,'kqueue_fd':watch._queue.fileno()})
            require(D.same(row,current) and not exited,'live identity at NOTE_EXIT registration')
        return value
    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent,D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own=D.birth(pid)
                if own is None or own['ppid']!=parent['pid'] or pid in self.owned: continue
                self.track(own,'unexpected-helper'); self.record['helpers'].append({'at':now(),'parent':parent,'identity':own}); require(self.cleaning,'unexpected own helper')
    def checkpoint(self):
        row={'at':now(),'phase':self.phase,'cases':self.record['cases'],'observations':len(self.record['observations']),
             'commands':len(self.record['commands']),'inventories':len(self.record['inventories']),'exits':len(self.record['exits']),
             'last_nonce':self.record['observations'][-1].get('request',{}).get('challenge') if self.record['observations'] else None}
        raw=encoded(row); require(len(raw)<=FILE_CAP,'progress cap'); tmp=self.root/'progress.tmp'; tmp.write_bytes(raw); os.replace(tmp,self.root/'progress.json')
    def command(self,args,startup=False,required=True):
        check_budget(self.record)
        present=sum(p.stat().st_size for p in self.root.rglob('*') if p.is_file())
        require(present+len(encoded(pack(self.record)))+24*1024<LOT_CAP,'whole lot reserve before CLI')
        argv=[self.binary,*([] if startup else ['-N']),'-S',str(self.socket)]
        if startup: argv+=['-f',str(self.root/'startup.conf')]
        argv+=args; entry={'start':now(),'argv':argv,'pre_exec_file_cap':FILE_CAP}; self.record['commands'].append(entry)
        old=resource.getrlimit(resource.RLIMIT_FSIZE); require(old[0]==resource.RLIM_INFINITY or old[0]>=FILE_CAP,'file limit too small')
        try:
            resource.setrlimit(resource.RLIMIT_FSIZE,(FILE_CAP,old[1]))
            with chdir(self.root): result=D.native.run(argv,env=self.env,timeout=self.remaining(),check=False)
            entry.update(pid=result.pid,returncode=result.returncode,stdout=result.stdout.decode(),stderr=result.stderr.decode())
            require(len(result.stdout)<FILE_CAP and len(result.stderr)<FILE_CAP,'CLI output cap reached')
        except BaseException as error: entry['error']=repr(error); raise
        finally: resource.setrlimit(resource.RLIMIT_FSIZE,old); entry['end']=now()
        if required: require(result.returncode==0,'CLI failed '+repr(args))
        return result.stdout.decode().strip()
    def public(self,target):
        fields=self.command(['display-message','-p','-t',target,FORMAT]).split('|'); equal(len(fields),len(FIELDS),'public fields'); pub=dict(zip(FIELDS,fields))
        pub['daemon_pid']=int(pub['daemon_pid']); pub['pane_pid']=int(pub['pane_pid']); equal(pub['socket'],str(self.socket),'private endpoint')
        endpoint=self.socket_info(); require(endpoint is not None,'endpoint exists'); daemon=D.process(pub['daemon_pid']); self.track(daemon['before'],'daemon')
        key='tmux' if self.phase=='oracle' else 'rmux_daemon'; equal(daemon['image']['executable'],self.config['pins'][key]['path'],'daemon executable'); equal(daemon['image']['sha256'],self.config['pins'][key]['sha256'],'daemon hash')
        if self.daemon is None: self.daemon,self.endpoint=saved(daemon),saved(endpoint)
        else: require(D.stable(self.daemon,daemon) and endpoint==self.endpoint,'daemon/endpoint changed')
        equal((pub['pane_dead'],pub['pane_dead_status'],pub['session_attached'],pub['window_width'],pub['window_height']),('0','','0','80','24'),'detached live pane and geometry')
        item={'at':now(),'target':target,'public':pub,'daemon':daemon,'endpoint':endpoint}; self.record['public_samples'].append(item); return item
    def expected_for(self,label):
        p=self.config['pins']['python']
        return {'kind':label,'token':self.config['tokens'][self.phase],'exe':p['path'],'exe_sha256':p['sha256'],'argv':witness_argv(self.config,self.phase,label),
                'cwd':str(self.root/'cwd'),'environment':{k:self.env.get(k) for k in ENV_KEYS}}
    def observe(self,label,target,reobserve=False):
        require(len(self.record['observations'])<9,'nine observations/eighteen child snapshots cap'); item=C.Run.observe(self,label,target,reobserve)
        nonce=item['request']['challenge']; require(len(nonce)==48 and nonce not in self.nonces and all(c in '0123456789abcdef' for c in nonce),'fresh nonce48hex')
        self.nonces.add(nonce); equal(item['ack']['exit_code'],0,'intention only'); equal((item['public']['session_name'],item['public']['window_name']),(label,label),'public label')
        if not reobserve: self.children[label]=saved(item)
        return item
    def spawn(self,label):
        require(label not in self.children and self.spawn_count<3,'three planned helpers'); self.spawn_count+=1
        target=self.command(spawn_plan(self.config,self.phase,label),startup=label=='alpha'); require(target.startswith('%') and target[1:].isdigit(),'pane ID')
        self.observe(label,target); self.alive.append(label)
    def mutate(self,case,args):
        checks=[]
        for label in ['daemon',*self.alive]:
            old=self.daemon['before'] if label=='daemon' else self.children[label]['child']['before']; watch=self.watches[old['pid']]; current=D.birth(old['pid']); event=watch.poll_exit()
            checks.append({'label':label,'before':saved(old),'current':current,'exit_event':event}); require(D.same(old,current) and not event,'retained live birth before mutation')
        index=len(self.record['commands']); self.command(args,required=False); command=self.record['commands'][index]
        item={'case':case,'args':args if args[0]!='source-file' else ['source-file','PRIVATE/live.conf'],'command_index':index,'pre_mutation':checks,
              **{k:command[k] for k in ('start','end','returncode','stdout','stderr')}}; self.record['mutations'].append(saved(item))
        if self.phase=='candidate': equal({k:item[k] for k in ('case','args','returncode','stdout','stderr')},next(m for m in self.oracle['mutations'] if m['case']==case),'exact oracle mutation outcome')
        require(command['returncode']==0,'product mutation failed'); return index
    def scopes(self,case,point,global_value,locals_by_label):
        item={'case':case,'point':point,'at':now(),'command_indices':[],'sessions':{}}; self.record['scopes'].append(item)
        def query(args): item['command_indices'].append(len(self.record['commands'])); return self.command(args)
        item['global']=query(['show-options','-gv','destroy-unattached']); item['exit_empty']=query(['show-options','-sv','exit-empty'])
        for label in locals_by_label:
            item['sessions'][label]={'local':query(['show-options','-qv','-t',label,'destroy-unattached']),'effective':query(['show-options','-Av','-t',label,'destroy-unattached'])}
        check_scopes(item,global_value,locals_by_label)
        if self.phase=='candidate': equal({k:item[k] for k in ('case','point','global','exit_empty','sessions')},next(s for s in self.oracle['scopes'] if s['case']==case and s['point']==point),'exact oracle scopes')
        return saved(item)
    def inventory_sample(self):
        index=len(self.record['commands']); raw=self.command(['list-sessions','-F',SESSION_FORMAT])
        return {'at':now(),'command_index':index,'raw':raw,'sessions':parse_sessions(raw),'daemon':D.process(self.daemon['before']['pid']),
                'endpoint':self.socket_info(),'daemon_exit_event':self.watches[self.daemon['before']['pid']].poll_exit()}
    def record_exit(self,label,case,index):
        old=self.children[label]['child']['before']; watch=self.watches[old['pid']]
        row={'at':now(),'label':label,'case':case,'trigger_command_index':index,'before':saved(old),'original_public':saved(self.children[label]['public']),
             'numeric_status_claimed':False,'status':'UNPROVEN'}; self.record['exits'].append(row)
        watch.wait(self.remaining()); after=D.birth(old['pid'])
        while D.same(old,after): time.sleep(min(.02,self.remaining())); after=D.birth(old['pid'])
        row.update(observed_at=now(),exit_event=watch.exited,after=after); check_exit(row); row['status']='PASS'; self.exited.add(label)
    def inventory_pair(self,case,target=None,index=None):
        item={'case':case,'at':now(),'samples':[],'status':'UNPROVEN'}; self.record['inventories'].append(item)
        if target is not None:
            watch=self.watches[self.children[target]['child']['before']['pid']]; end=time.monotonic()+min(3,self.remaining())
            while not watch.poll_exit() and time.monotonic()<end: time.sleep(.02)
            item['target_exit_event']=watch.exited
        first=self.inventory_sample(); item['samples'].append(saved(first)); survivors=check_inventory(first['sessions'],self.children)
        require(set(survivors)<=set(self.alive),'no resurrection'); removed=[s for s in self.alive if s not in survivors]
        for label in removed:
            require(index is not None,'spontaneous removal'); self.record_exit(label,case,index)
        for label in survivors:
            require(not self.watches[self.children[label]['child']['before']['pid']].poll_exit(),'live survivor exited'); self.observe(label,self.children[label]['target'],True)
        time.sleep(min(.05,self.remaining())); second=self.inventory_sample(); item['samples'].append(saved(second)); item.update(survivors=survivors,removed=removed)
        check_pair(item,self.children,self.daemon,self.endpoint)
        for label in survivors: require(not self.watches[self.children[label]['child']['before']['pid']].poll_exit(),'survivor exited during pair')
        # Projection is the finite pilot contract; the complete new Mac oracle
        # independently confirms it before any candidate may start.
        equal(survivors,SURVIVORS[case],'pilot survivor projection')
        if self.phase=='candidate': equal(survivors,self.oracle['survivors'][case],'exact measured Darwin oracle survivors')
        self.alive=saved(survivors); item['status']='PASS'
    def execute(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh fixture'); self.profiles()
        for case in SURVIVORS:
            self.case_end=min(time.monotonic()+20,self.start+90); e={'id':case,'start':now(),'monotonic_start':time.monotonic()}; self.record['cases'].append(e)
            try:
                if case=='D1':
                    for label in KINDS: self.spawn(label)
                    self.scopes(case,'before_beta_override','off',{label:None for label in KINDS})
                    self.mutate(case,['set-option','-t','beta','destroy-unattached','off']); self.inventory_pair(case)
                    self.scopes(case,'after','off',{'alpha':None,'beta':'off','gamma':None})
                elif case=='D2':
                    self.scopes(case,'before','off',{'alpha':None,'beta':'off','gamma':None})
                    index=self.mutate(case,['set-option','-t','alpha','destroy-unattached','on']); self.inventory_pair(case,'alpha',index)
                    self.scopes(case,'after','off',{'beta':'off','gamma':None})
                elif case=='D3':
                    self.scopes(case,'before','off',{'beta':'off','gamma':None})
                    index=self.mutate(case,['set-option','-g','destroy-unattached','on']); self.inventory_pair(case,'gamma',index)
                    self.scopes(case,'after','on',{'beta':'off'})
                else:
                    self.scopes(case,'before','on',{'beta':'off'}); index=self.mutate(case,['source-file',str(self.root/'live.conf')])
                    self.inventory_pair(case,'beta',index); self.scopes(case,'after','on',{})
                self.ledger(); self.profiles(); self.remaining(); e['status']='PASS'
            except BaseException as error: e.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error)); raise
            finally: e.update(end=now(),seconds=time.monotonic()-e['monotonic_start']); self.checkpoint()
    def endpoint_probe(self):
        row={'at':now(),'path_state':self.socket_info(),'probe':'AF_UNIX SOCK_STREAM connect; no CLI/autostart'}; self.record['endpoint_probes'].append(row)
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
            stream.settimeout(min(.5,self.remaining()))
            try: stream.connect(str(self.socket)); row.update(accepting=True,errno=None)
            except OSError as error: row.update(accepting=False,errno=error.errno,error=repr(error))
        check_probe(row); row.update(listener_absent=True,pathname_absent=row['path_state'] is None); return saved(row)
    def cleanup(self):
        self.cleaning=True; self.cleanup_end=min(time.monotonic()+30,self.start+120)
        row={'start':now(),'errors':[],'rescues':[],'processes':[]}; self.record['cleanup']=row
        def attempt(name,fn):
            try: return fn()
            except Exception as error: row['errors'].append({'step':name,'error':repr(error)})
        if self.daemon:
            old=self.daemon['before']; watch=self.watches[old['pid']]
            if not watch.poll_exit() and D.same(old,D.birth(old['pid'])):
                def close():
                    current=D.process(old['pid']); require(D.stable(self.daemon,current) and self.socket_info()==self.endpoint,'private daemon cleanup ownership')
                    row['qualified_shutdown']={'at':now(),'daemon':current,'endpoint':self.socket_info(),'command_index':len(self.record['commands'])}
                    self.command(['kill-server']); watch.wait(self.remaining())
                attempt('qualified-kill-server',close)
        for pid,old in self.owned.items():
            watch=self.watches[pid]; attempt('wait-'+str(pid),lambda:watch.wait(self.remaining()))
            if not watch.poll_exit() and D.same(old,D.birth(pid)):
                row['rescues'].append({'identity':old,'signal':9}); attempt('owned-rescue-'+str(pid),lambda:watch.retire_after_failure(self.remaining()))
            try: collected,status=os.waitpid(pid,os.WNOHANG); wait={'pid':collected,'raw_status':status}
            except ChildProcessError: wait={'ECHILD':True,'note':'indirect; no numeric exit/reap inferred'}
            after=D.birth(pid)
            while D.same(old,after) and time.monotonic()<self.cleanup_end: time.sleep(.02); after=D.birth(pid)
            row['processes'].append({'before':saved(old),'after':after,'exit_event':watch.exited,'waitpid':wait})
        row['all_absent']=bool(row['processes']) and all(p['exit_event'] and not D.same(p['before'],p['after']) for p in row['processes'])
        def endpoint_cleanup():
            require(row['all_absent'],'all births absent before endpoint cleanup'); probe=self.endpoint_probe(); current=self.socket_info()
            receipt={'probe':probe,'before':current,'removed':False}; row['endpoint']=receipt
            if current is not None:
                fresh=[{**p,'after':D.birth(p['before']['pid'])} for p in row['processes']]; check_unlink(self.endpoint,current,fresh,probe)
                receipt['identity_recheck']=fresh; self.socket.unlink(); receipt.update(removed=True,removed_at=now())
            receipt['after']=self.socket_info(); require(receipt['after'] is None,'endpoint residue')
        attempt('endpoint',endpoint_cleanup); attempt('profiles',lambda:self.profiles(True))
        for watch in self.watches.values(): watch.close()
        row['socket_absent']=not self.socket.exists(); row['fd_after']=D.fd_snapshot(); row['fds_equal']=row['fd_after']==self.record['fd_before']; row['end']=now()
        row['pass']=bool(row['all_absent'] and row['socket_absent'] and row['fds_equal'] and not row['errors'] and not row['rescues'] and time.monotonic()<=self.cleanup_end)
        check_cleanup(row,list(self.owned.values()))

def verify_frozen(frozen):
    c=frozen['config']; require(frozen['phase'] in ('oracle','candidate'),'phase')
    for p in frozen['artifacts']: require(pin(p['path'])==p,'frozen pin changed '+p['path'])
    mandatory=[str(Path(__file__).resolve()),c['config_path'],c['adapter_pin']['path'],frozen['pure']['path'],*[p['path'] for p in c['pins'].values()],*[p['path'] for p in c['api_pins']]]
    mandatory += [str(Path(root)/name) for root in c['roots'].values() for name in ('startup.conf','live.conf')]
    require(set(mandatory)<={p['path'] for p in frozen['artifacts']},'mandatory provenance')
    equal(c,json.loads(Path(c['config_path']).read_text()),'config'); equal(frozen['pins'],c['pins'],'runtime pins'); equal(frozen['root'],c['roots'][frozen['phase']],'root')
    equal(frozen['plans'],{label:spawn_plan(c,frozen['phase'],label) for label in KINDS},'plans'); equal(frozen['environment'],environment(c,frozen['phase']),'environment')
    pure=json.loads(Path(frozen['pure']['path']).read_text()); require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure controls stale')
    require(not (Path(frozen['root'])/'STARTED.json').exists(),'lot already attempted')
    if frozen['phase']=='candidate':
        p=frozen['oracle_gate']; require(pin(p['path'])==p,'oracle gate pin'); gate=json.loads(Path(p['path']).read_text())
        require(gate['status']=='PASS' and pin(gate['result']['path'])==gate['result'],'complete oracle result'); equal(gate['driver_sha256'],digest(__file__),'same oracle driver')
        equal(frozen['oracle_projection'],gate['projection'],'measured oracle projection'); equal(gate['live_observations'],9,'complete nine observation oracle')
        equal(gate['helpers'],3,'oracle helpers'); equal(gate['daemon_generations'],1,'oracle daemon')

def execute(frozen_path):
    frozen=json.loads(frozen_path.read_text()); verify_frozen(frozen); bind_api(frozen['config']); run=Run(frozen)
    write_once(run.root/'STARTED.json',{'at':now(),'frozen':pin(frozen_path),'observer':D.birth(os.getpid())},FILE_CAP)
    try: run.execute(); run.record['status']='PASS'
    except BaseException as error: run.record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=repr(error),traceback=traceback.format_exc())
    finally:
        try: run.cleanup()
        except BaseException as error: run.record.update(cleanup_exception=repr(error),status='UNPROVEN')
        run.record.update(end=now(),seconds=time.monotonic()-run.start,frozen=pin(frozen_path),distinct_nonces=len(run.nonces),
                          limits=['Disk hashes not mapped-image attestation.','Finite known-parent sampling not exhaustive transient coverage.',
                                  'NOTE_EXIT and ECHILD not author reap or numeric helper status.','Pathname and listener distinct.'])
        document=pack(run.record); equal(unpack(document),run.record,'lossless evidence encoding')
        require(len(encoded(document))+sum(p.stat().st_size for p in run.root.rglob('*') if p.is_file())+12*1024<LOT_CAP,'whole lot logs plus outer/gate reserve')
        write_once(run.root/'RESULT.json',document)
    if run.record['status']=='PASS' and run.phase=='oracle':
        write_once(run.root/'ORACLE-FROZEN.json',{'at':now(),'status':'PASS','result':pin(run.root/'RESULT.json'),'driver_sha256':digest(__file__),
                   'projection':projection(run.record),'live_observations':len(run.record['observations']),'helpers':run.spawn_count,'daemon_generations':1})
    print(json.dumps({'status':run.record['status'],'result':pin(run.root/'RESULT.json'),'seconds':run.record['seconds']}),flush=True)
    return 0 if run.record['status']=='PASS' else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness': witness(*sys.argv[2:])
    elif len(sys.argv)==3 and sys.argv[1]=='--frozen': raise SystemExit(execute(Path(sys.argv[2])))
    elif len(sys.argv)==3 and sys.argv[1]=='--decode': print(json.dumps(read_record(sys.argv[2]),indent=2))
    else: raise SystemExit('Use --frozen FILE.json or --decode RESULT.json; witness is private.')
