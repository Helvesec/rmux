#!/usr/bin/env python3
"""Bounded Darwin allow-rename ESC-k proof; acquired011/012, strict020 codec.

One private session, two windows/helpers. Four finite cases, no attach/GUI.
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
import secrets
import socket
import sys
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode=True
C=D=None
KINDS=('alpha','beta')
CASES=('R1','R2','R3','R4')
ENV_KEYS=('HOME','XDG_CONFIG_HOME','PATH','L6_ALLOWRENAME_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM','L5_ALLOWRENAME_API')
FIELDS=('daemon_pid','socket','session_id','window_id','pane_id','pane_pid','pane_tty','pane_dead','pane_dead_status','session_attached','session_name','window_name','window_width','window_height')
FORMAT='#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_dead}|#{pane_dead_status}|#{session_attached}|#{session_name}|#{window_name}|#{window_width}|#{window_height}'
FILE_CAP=4096
RECORD_CAP=768*1024
LOT_CAP=1024*1024
class RuntimeMismatch(RuntimeError): pass


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

def bind_api(config):
    global C,D,Unproven,Failed
    ap=config['adapter_pin']; require(pin(ap['path'])==ap,'adapter provenance')
    equal(ap['sha256'],'3bb2a8a9faba1b860f00d7641696af364fddbaa48e28b79d846fa0249b14a7bb','acquired012 adapter')
    spec=importlib.util.spec_from_file_location('allowrename_acquired012',ap['path']); C=importlib.util.module_from_spec(spec); spec.loader.exec_module(C)
    C.bind_api(config['api_pins']); D=C.D; Unproven,Failed=D.Unproven,D.Failed; C.ENV_KEYS=ENV_KEYS

def environment(config,phase):
    root=Path(config['roots'][phase])
    return {'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'xdg'),'PATH':'/usr/bin:/bin','SHELL':'/bin/bash','TERM':'vt100','LANG':'C',
            'RMUX_DISABLE_TMUX_FALLBACK':'1','RMUX_DISABLE_TMUX_SHIM':'1','L6_ALLOWRENAME_MARKER':config['tokens'][phase],'L5_ALLOWRENAME_API':config['config_path']}

def witness_argv(config,phase,label):
    return [config['pins']['python']['path'],'-I',config['driver'],'witness',label,str(Path(config['roots'][phase])/'markers'),config['tokens'][phase],'0']

def check_probe(row):
    require(row['accepting'] is False and row['errno'] in (errno.ENOENT,errno.ECONNREFUSED),'absent listener needs non-timeout refusal/absence')

def check_unlink(endpoint,current,identities,probe):
    require(identities and all(p['exit_event'] and not D.same(p['before'],p['after']) for p in identities),'all attributed births absent before unlink')
    require(endpoint is not None and endpoint==current and current['uid']==os.getuid(),'exact owned endpoint')
    check_probe(probe); equal(probe['errno'],errno.ECONNREFUSED,'stale pathname must refuse connection'); equal(probe['path_state'],current,'fresh probe inode')

def check_cleanup(row,owned):
    C.check_cleanup(row); expected={(p['pid'],tuple(p['birth'])) for p in owned}; actual={(p['before']['pid'],tuple(p['before']['birth'])) for p in row['processes']}
    require(expected and expected==actual and len(row['processes'])==len(expected),'complete cleanup population')

def emission_bytes(name,nonce):
    require(type(name) is str and 1<=len(name)<=48 and all(c in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_' for c in name),'safe unique ASCII name')
    require(type(nonce) is str and len(nonce)==48 and all(c in '0123456789abcdef' for c in nonce),'nonce48hex')
    return ('\x1bk'+name+'\x1b\\\r\nL5-'+nonce+'\r\n').encode('ascii')

def witness(label,root,token,code):
    bind_api(json.loads(Path(os.environ['L5_ALLOWRENAME_API']).read_text())); require(label in KINDS and code=='0','planned helper')
    root=Path(root); own=D.birth(os.getpid()); key=C.stem(own)
    def report(nonce,emission=None):
        return {'at':now(),'kind':label,'token':token,'challenge':nonce,'identity':D.birth(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,
                'environment':{k:os.environ.get(k) for k in ENV_KEYS},'emission':emission,'pty':os.ttyname(1)}
    atomic_once(root/('born-'+key+'.json'),report(None)); end=time.monotonic()+180
    while time.monotonic()<end:
        requests=sorted(root.glob('request-'+key+'-*.json')); require(len(requests)<=12,'helper request cap')
        for path in requests:
            ack=root/path.name.replace('request-','ack-',1)
            if ack.exists(): continue
            require(path.stat().st_size<=FILE_CAP,'request cap'); request=json.loads(path.read_text())
            equal(set(request),{'identity','challenge','issued_at','name'},'request schema'); require(D.same(own,request['identity']),'held helper birth')
            emission=None
            if request['name'] is not None:
                payload=emission_bytes(request['name'],request['challenge']); offset=0; emitted_at=now()
                while offset<len(payload):
                    written=os.write(1,payload[offset:]); require(written>0,'PTY write progress'); offset+=written
                emission={'name':request['name'],'bytes_hex':payload.hex(),'bytes_written':offset,'at':emitted_at,'marker':'L5-'+request['challenge']}
            atomic_once(ack,report(request['challenge'],emission))
        time.sleep(.02)
    os._exit(124)

def spawn_plan(config,phase,label):
    common=['-d','-P','-F','#{pane_id}']
    args=['new-session',*common,'-s','proof','-n','alpha','-x','80','-y','24'] if label=='alpha' else ['new-window',*common,'-t','proof:1','-n','beta']
    return [*args,'-c',str(Path(config['roots'][phase])/'cwd'),*witness_argv(config,phase,label)]

def check_budget(record):
    require(len(record['commands'])<192,'192 CLI cap')
    require(sum(len(e.get(k,'').encode()) for e in record['commands'] for k in ('stdout','stderr'))+2*FILE_CAP<192*1024,'CLI text reserve')
    require(len(encoded(record))<2*1024*1024,'logical memory bound')
    require(len(encoded(pack(record)))<RECORD_CAP-48*1024,'packed reserve')

def stable_public(before,after):
    equal({k:v for k,v in before.items() if k!='window_name'},{k:v for k,v in after.items() if k!='window_name'},'stable IDs/public geometry')

def check_emission(ack,request,public,captured):
    equal(ack['pty'],public['pane_tty'],'own output PTY')
    if request['name'] is None: equal(ack['emission'],None,'no emission for ordinary challenge'); return
    payload=emission_bytes(request['name'],request['challenge']); emit=ack['emission']
    equal((emit['name'],emit['bytes_hex'],emit['bytes_written']),(request['name'],payload.hex(),len(payload)),'exact ESC-k bytes')
    equal(emit['marker'],'L5-'+request['challenge'],'marker nonce')
    require(emit['marker'] in captured.splitlines(),'terminal consumed bytes after ESC-k')

def normalized_names(names,config,phase):
    mapping={value:key for key,value in config['names'][phase].items()}; mapping.update(alpha='initial_alpha',beta='initial_beta')
    return {key:mapping.get(value,'UNEXPECTED:'+value) for key,value in names.items()}

def expected_names(case,point='after'):
    if case=='R1': return {'alpha':'initial_alpha','beta':'initial_beta'}
    if case=='R2' or (case=='R3' and point=='beta_off'): return {'alpha':'R2','beta':'initial_beta'}
    return {'alpha':'R2','beta':'R3on'}

def projection(record):
    return [{'case':e['case'],'point':e['point'],'names':e['normalized_names'],'options':e['options']} for e in record['states']]

class Run:
    def __init__(self,frozen):
        D.Run.__init__(self,frozen,None); self.config=frozen['config']; self.env=environment(self.config,self.phase); self.children=self.observed
        self.nonces=set(); self.oracle=frozen.get('oracle_projection'); self.alive=[]
        self.record.update(environment=self.env,marker_ledgers=[],held_watches=[],states=[],mutations=[],endpoint_probes=[],emissions=[])
    def remaining(self):
        end=min(self.cleanup_end,self.start+180) if self.cleaning else min(self.case_end,self.start+150)
        left=end-time.monotonic(); require(left>0,'lot/transition deadline'); return min(left,5)
    def checkpoint(self):
        row={'at':now(),'phase':self.phase,'cases':self.record['cases'],'observations':len(self.record['observations']),'commands':len(self.record['commands'])}
        raw=encoded(row); require(len(raw)<FILE_CAP,'progress cap'); tmp=self.root/'progress.tmp'; tmp.write_bytes(raw); os.replace(tmp,self.root/'progress.json')
    def observe(self,label,name=None):
        require(len(self.record['observations'])<12,'twelve challenges/twenty-four snapshots cap')
        target=self.targets[label]; sample=self.public(target); pub=sample['public']; own=D.birth(pub['pane_pid']); require(own is not None,'own helper present'); self.track(own,'pane')
        expected=self.expected_for(label); key=C.stem(own); markers=self.root/'markers'; index=len(self.record['observations'])
        item={'at':now(),'label':label,'target':target,'public':saved(pub),'expected':expected,'status':'UNPROVEN'}; self.record['observations'].append(item)
        try:
            born_path=markers/('born-'+key+'.json'); born=self.wait_json(born_path); first=self.snapshot(own['pid'])
            item.update(born=born,born_path=str(born_path),first=saved(first)); C.check_marker(born,first,expected,None)
            if name is not None: item['options_before_emission']=self.options()
            nonce=secrets.token_hex(24); require(nonce not in self.nonces,'fresh nonce'); self.nonces.add(nonce)
            request={'identity':own,'challenge':nonce,'issued_at':now(),'name':name}; path=markers/f'request-{key}-{index}.json'; atomic_once(path,request)
            ackpath=markers/path.name.replace('request-','ack-',1); ack=self.wait_json(ackpath); captured=''; captures=[]
            if name is not None:
                while True:
                    cmd_index=len(self.record['commands']); captured=self.command(['capture-pane','-p','-t',target]); captures.append(cmd_index)
                    if 'L5-'+nonce in captured.splitlines(): break
                    require(len(captures)<20,'bounded terminal marker polls'); time.sleep(min(.025,self.remaining()))
            second=self.snapshot(own['pid']); after=self.public(target)
            item.update(request=request,request_path=str(path),ack=ack,ack_path=str(ackpath),child=saved(second),public_after=saved(after['public']),capture_command_indices=captures)
            stable_public(pub,after['public']); C.check_child(first,second,pub,after['daemon'],expected); C.check_marker(ack,second,expected,nonce); check_emission(ack,request,pub,captured)
            if label in self.children:
                old=self.children[label]; require(D.stable(old['child'],second),'same child birth/image'); stable_public(old['public'],after['public'])
            else: self.children[label]=saved(item)
            if name is not None: self.record['emissions'].append({'label':label,'request':saved(request),'ack':saved(ack),'observation_index':index})
            self.helpers(); item['status']='PASS'; return item
        except Exception as error: item.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error)); raise
        finally: self.checkpoint()
    def spawn(self,label):
        require(label not in self.targets and self.spawn_count<2,'two helpers only'); self.spawn_count+=1
        target=self.command(spawn_plan(self.config,self.phase,label),startup=label=='alpha'); require(target.startswith('%') and target[1:].isdigit(),'pane ID')
        self.targets[label]=target; self.observe(label); self.alive.append(label)
        if label=='beta':
            a,b=[self.children[k]['public'] for k in KINDS]
            require(a['session_id']==b['session_id'] and a['window_id']!=b['window_id'] and a['pane_id']!=b['pane_id'],'one session two distinct windows/panes')
    def mutate(self,case,args):
        live=[]
        for old in self.owned.values():
            current=D.birth(old['pid']); event=self.watches[old['pid']].poll_exit(); require(D.same(old,current) and not event,'held birth before mutation'); live.append({'before':saved(old),'after':current,'exit_event':event})
        index=len(self.record['commands']); self.command(args,required=False); entry=self.record['commands'][index]
        self.record['mutations'].append({'case':case,'command_index':index,'live':live,**{k:entry[k] for k in ('argv','returncode','stdout','stderr','start','end')}})
        if entry['returncode']!=0 and self.phase=='candidate': raise RuntimeMismatch('option mutation differs from successful Darwin oracle')
        require(entry['returncode']==0,'oracle option mutation failed')
    def options(self):
        rows={}
        for label in KINDS:
            pub=self.children[label]['public']; window=pub['window_id']; pane=pub['pane_id']; indices=[]
            def query(args): indices.append(len(self.record['commands'])); return self.command(args)
            rows[label]={'window_local':query(['show-options','-wqv','-t',window,'allow-rename']),
                         'pane_local':query(['show-options','-pqv','-t',pane,'allow-rename']),
                         'effective':query(['show-options','-pAv','-t',pane,'allow-rename']),
                         'automatic_rename':query(['show-options','-wAv','-t',window,'automatic-rename'])}
            self.record.setdefault('option_queries',[]).append({'at':now(),'label':label,'command_indices':indices,'values':saved(rows[label])})
        return rows
    def state(self,case,point,values):
        options=self.options(); samples=[]
        for _ in range(2):
            names={}
            for label in KINDS:
                pub=self.public(self.targets[label])['public']; stable_public(self.children[label]['public'],pub); names[label]=pub['window_name']
            samples.append({'at':now(),'names':names}); time.sleep(min(.05,self.remaining()))
        row={'at':now(),'case':case,'point':point,'samples':samples,'options':options,'normalized_names':normalized_names(samples[-1]['names'],self.config,self.phase)}; self.record['states'].append(saved(row))
        wanted={label:{'window_local':value,'pane_local':'','effective':value,'automatic_rename':'off'} for label,value in values.items()}
        expected={'case':case,'point':point,'names':expected_names(case,point),'options':wanted}
        actual={k:row[k] for k in ('case','point','options')}; actual['names']=row['normalized_names']
        if self.phase=='candidate': expected=next(x for x in self.oracle if x['case']==case and x['point']==point)
        if actual!=expected or samples[0]['names']!=samples[1]['names']:
            if self.phase=='candidate':
                self.record['mismatch']={'at':now(),'case':case,'expected':expected,'actual':actual,'samples':saved(samples),'owner':'L2 runtime'}
                raise RuntimeMismatch('measured Darwin rename/options mismatch')
            equal(actual,expected,'oracle finite contract'); equal(samples[0]['names'],samples[1]['names'],'oracle names stable')
    def execute(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh fixture'); self.profiles(); self.targets={}
        for case in CASES:
            self.case_end=min(time.monotonic()+20,self.start+150); entry={'id':case,'start':now(),'status':'RUNNING'}; self.record['cases'].append(entry)
            try:
                names=self.config['names'][self.phase]
                if case=='R1':
                    for label in KINDS: self.spawn(label)
                    for label in KINDS: self.mutate(case,['set-option','-w','-t',self.children[label]['public']['window_id'],'allow-rename','off'])
                    self.state(case,'before',{'alpha':'off','beta':'off'}); self.observe('alpha',names['R1']); self.observe('beta'); self.state(case,'after',{'alpha':'off','beta':'off'})
                elif case=='R2':
                    self.mutate(case,['set-option','-w','-t',self.children['alpha']['public']['window_id'],'allow-rename','on'])
                    self.observe('alpha',names['R2']); self.observe('beta'); self.state(case,'after',{'alpha':'on','beta':'off'})
                elif case=='R3':
                    self.observe('beta',names['R3off']); self.state(case,'beta_off',{'alpha':'on','beta':'off'})
                    self.mutate(case,['source-file',str(self.root/'beta-on.conf')]); self.observe('beta',names['R3on']); self.observe('alpha'); self.state(case,'after',{'alpha':'on','beta':'on'})
                else:
                    self.mutate(case,['source-file',str(self.root/'alpha-off.conf')]); self.observe('alpha',names['R4']); self.observe('beta'); self.state(case,'after',{'alpha':'off','beta':'on'})
                entry['status']='PASS'
            except BaseException as error:
                entry.update(status='RUNTIME_MISMATCH' if isinstance(error,RuntimeMismatch) else 'FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error))
                for later in CASES[CASES.index(case)+1:]: self.record['cases'].append({'id':later,'status':'NOT_RUN','reason':'stop at first failure'})
                raise
            finally: entry['end']=now(); self.checkpoint()
    def socket_info(self): return D.Run.socket_info(self)

    def profiles(self,post_shutdown=False): return C.Run.profiles(self,post_shutdown)

    def wait_json(self,path): return C.Run.wait_json(self,path)

    def snapshot(self,pid): return C.Run.snapshot(self,pid)

    def ledger(self): return C.Run.ledger(self)

    def track(self,row,role):
        new=row['pid'] not in self.owned; value=D.Run.track(self,row,role); require(len(self.owned)<=3,'one daemon two helpers')
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

    def endpoint_probe(self):
        row={'at':now(),'path_state':self.socket_info(),'probe':'AF_UNIX SOCK_STREAM connect; no CLI/autostart'}; self.record['endpoint_probes'].append(row)
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
            stream.settimeout(min(.5,self.remaining()))
            try: stream.connect(str(self.socket)); row.update(accepting=True,errno=None)
            except OSError as error: row.update(accepting=False,errno=error.errno,error=repr(error))
        check_probe(row); row.update(listener_absent=True,pathname_absent=row['path_state'] is None); return saved(row)

    def cleanup(self):
        self.cleaning=True; self.cleanup_end=min(time.monotonic()+30,self.start+180)
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
    config=frozen['config']; require(frozen['phase'] in ('oracle','candidate'),'phase')
    for p in frozen['artifacts']: require(pin(p['path'])==p,'frozen artifact '+p['path'])
    required={str(Path(__file__).resolve()),config['config_path'],config['adapter_pin']['path'],frozen['pure']['path'],*[p['path'] for p in config['pins'].values()],*[p['path'] for p in config['api_pins']]}
    required.update(str(Path(root)/name) for root in config['roots'].values() for name in ('startup.conf','beta-on.conf','alpha-off.conf'))
    require(required<={p['path'] for p in frozen['artifacts']},'complete frozen provenance')
    equal(config,json.loads(Path(config['config_path']).read_text()),'immutable config'); equal(frozen['pins'],config['pins'],'tool pins'); equal(frozen['root'],config['roots'][frozen['phase']],'root')
    equal(frozen['plans'],{label:spawn_plan(config,frozen['phase'],label) for label in KINDS},'plans'); equal(frozen['environment'],environment(config,frozen['phase']),'environment')
    pure=json.loads(Path(frozen['pure']['path']).read_text()); require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure gate')
    require(not (Path(frozen['root'])/'STARTED.json').exists(),'one lot only')
    if frozen['phase']=='candidate':
        p=frozen['oracle_gate']; require(pin(p['path'])==p,'oracle gate pin'); gate=json.loads(Path(p['path']).read_text())
        require(gate['status']=='PASS' and pin(gate['result']['path'])==gate['result'],'oracle complete and unchanged')
        equal(gate['driver_sha256'],digest(__file__),'same source as native oracle'); equal(frozen['oracle_projection'],gate['projection'],'observed Darwin expectations')
        equal((gate['live_observations'],gate['helpers'],gate['daemon_generations']),(11,2,1),'complete oracle population')

def execute(frozen_path):
    frozen=json.loads(frozen_path.read_text()); verify_frozen(frozen); bind_api(frozen['config']); run=Run(frozen)
    write_once(run.root/'STARTED.json',{'at':now(),'frozen':pin(frozen_path),'observer':D.birth(os.getpid())},FILE_CAP)
    try: run.execute(); run.record['status']='PASS'
    except RuntimeMismatch as error: run.record.update(status='RUNTIME_MISMATCH',error=str(error),stopped_before_next_case=True)
    except BaseException as error: run.record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=repr(error),traceback=traceback.format_exc())
    finally:
        try: run.cleanup()
        except BaseException as error: run.record.update(cleanup_exception=repr(error),status='UNPROVEN')
        run.record.update(end=now(),seconds=time.monotonic()-run.start,frozen=pin(frozen_path),distinct_nonces=len(run.nonces),
            limits=['Disk hash not mapped-image attestation.','Finite own-parent samples, not exhaustive continuity.','NOTE_EXIT and absence not numeric helper exit/reap.','No Linux proof transfer.'])
        document=pack(run.record); equal(unpack(document),run.record,'lossless strict codec')
        require(len(encoded(document))+sum(p.stat().st_size for p in run.root.rglob('*') if p.is_file())+16*1024<LOT_CAP,'whole lot log reserve')
        write_once(run.root/'RESULT.json',document)
    if run.record['status']=='PASS' and run.phase=='oracle':
        write_once(run.root/'ORACLE-FROZEN.json',{'at':now(),'status':'PASS','result':pin(run.root/'RESULT.json'),'driver_sha256':digest(__file__),
            'projection':projection(run.record),'live_observations':len(run.record['observations']),'helpers':run.spawn_count,'daemon_generations':1})
    print(json.dumps({'status':run.record['status'],'result':pin(run.root/'RESULT.json'),'seconds':run.record['seconds']}),flush=True)
    return 0 if run.record['status']=='PASS' else 2 if run.record['status']=='RUNTIME_MISMATCH' else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness': witness(*sys.argv[2:])
    elif len(sys.argv)==3 and sys.argv[1]=='--frozen': raise SystemExit(execute(Path(sys.argv[2])))
    elif len(sys.argv)==3 and sys.argv[1]=='--decode': print(json.dumps(unpack(json.loads(Path(sys.argv[2]).read_text())),indent=2))
    else: raise SystemExit('Use --frozen FILE.json or --decode RESULT.json')
