#!/usr/bin/env python3
"""Darwin exit-unattached with exit-empty ON; fresh oracle, finite mismatch.

Only acquired011/012 modules are imported. All option-induced events are frozen
before cleanup. Shared JSON is lossless, including raw snapshots and every poll.
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
sys.dont_write_bytecode=True
C=D=None
GENERATIONS=('A','B')
LABELS={'A':'alpha','B':'beta'}
ENV_KEYS=('HOME','XDG_CONFIG_HOME','PATH','L6_EXITUNATTACHED_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM','L5_EXITUNATTACHED_API')
FIELDS=('daemon_pid','socket','session_id','window_id','pane_id','pane_pid','pane_tty','pane_dead','pane_dead_status','session_attached','session_name','window_name','window_width','window_height')
FORMAT='#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_dead}|#{pane_dead_status}|#{session_attached}|#{session_name}|#{window_name}|#{window_width}|#{window_height}'
FILE_CAP=4096
RECORD_CAP=208*1024
LOT_CAP=256*1024
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
def read_record(path): return unpack(json.loads(Path(path).read_text()))

def bind_api(config):
    global C,D,Unproven,Failed
    ap=config['adapter_pin']; require(pin(ap['path'])==ap,'adapter provenance')
    equal(ap['sha256'],'3bb2a8a9faba1b860f00d7641696af364fddbaa48e28b79d846fa0249b14a7bb','acquired012 adapter')
    spec=importlib.util.spec_from_file_location('exitunattached_acquired012',ap['path']); C=importlib.util.module_from_spec(spec); spec.loader.exec_module(C)
    C.bind_api(config['api_pins']); D=C.D; Unproven,Failed=D.Unproven,D.Failed; C.ENV_KEYS=ENV_KEYS

def root_for(config,phase,gen): return Path(config['roots'][phase])/gen
def environment(config,phase,gen):
    root=root_for(config,phase,gen)
    return {'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'xdg'),'PATH':'/usr/bin:/bin','SHELL':'/bin/bash','TERM':'vt100','LANG':'C',
            'RMUX_DISABLE_TMUX_FALLBACK':'1','RMUX_DISABLE_TMUX_SHIM':'1','L6_EXITUNATTACHED_MARKER':config['tokens'][phase][gen],'L5_EXITUNATTACHED_API':config['config_path']}
def witness_argv(config,phase,gen):
    return [config['pins']['python']['path'],'-I',config['driver'],'witness',LABELS[gen],str(root_for(config,phase,gen)/'markers'),config['tokens'][phase][gen],'0']
def spawn_plan(config,phase,gen):
    label=LABELS[gen]
    return ['new-session','-d','-P','-F','#{pane_id}','-s',label,'-n',label,'-x','80','-y','24','-c',str(root_for(config,phase,gen)/'cwd'),*witness_argv(config,phase,gen)]
def witness(label,root,token,code):
    bind_api(json.loads(Path(os.environ['L5_EXITUNATTACHED_API']).read_text())); require(label in LABELS.values() and code=='0','planned helper')
    root=Path(root); own=D.birth(os.getpid()); key=C.stem(own)
    def report(challenge):
        return {'at':now(),'kind':label,'token':token,'challenge':challenge,'exit_code':0,'exit_code_scope':'intention only; no numeric status inferred from option/cleanup',
                'identity':D.birth(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,'environment':{k:os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root/('born-'+key+'.json'),report(None)); end=time.monotonic()+180
    while time.monotonic()<end:
        requests=sorted(root.glob('request-'+key+'-*.json')); require(len(requests)<=4,'four challenges per generation')
        for path in requests:
            ack=root/path.name.replace('request-','ack-',1)
            if not ack.exists():
                require(path.stat().st_size<=FILE_CAP,'request cap'); request=json.loads(path.read_text())
                equal(set(request),{'identity','challenge','issued_at'},'challenge schema'); require(D.same(own,request['identity']),'fresh challenge birth')
                atomic_once(ack,report(request['challenge']))
        time.sleep(.02)
    os._exit(124)

def check_exit(row):
    require(row['exit_event'] and not D.same(row['before'],row['after']),'NOTE_EXIT plus absent birth'); equal(row['numeric_status_claimed'],False,'no numeric indirect status')
def classify(rows,elapsed):
    require(set(rows)=={'daemon','helper'},'complete effect population')
    if all(r['exit_event'] and not D.same(r['before'],r['after']) for r in rows.values()): return 'exited'
    if all(not r['exit_event'] and D.same(r['before'],r['after']) for r in rows.values()):
        require(elapsed>=20,'survival requires full20s'); return 'survived_20s'
    require(elapsed>=20,'mixed state requires completed bounded window'); return 'mixed_at_20s'
def check_probe(row):
    require(row['accepting'] is False and row['errno'] in (errno.ENOENT,errno.ECONNREFUSED),'absent listener requires non-timeout refusal/absence')
def check_unlink(endpoint,current,identities,probe):
    require(identities and all(p['exit_event'] and not D.same(p['before'],p['after']) for p in identities),'all attributed births absent before unlink')
    require(endpoint is not None and endpoint==current and current['uid']==os.getuid(),'exact owned endpoint'); check_probe(probe)
    equal(probe['errno'],errno.ECONNREFUSED,'stale pathname refuses connection'); equal(probe['path_state'],current,'fresh probe inode')
def check_cleanup(row,owned):
    C.check_cleanup(row); expected={(p['pid'],tuple(p['birth'])) for p in owned}; actual={(p['before']['pid'],tuple(p['before']['birth'])) for p in row['processes']}
    require(expected and expected==actual and len(row['processes'])==len(expected),'complete cleanup population')
def check_budget(record):
    commands=[e for g in record['generations'] for e in g['commands']]
    require(len(commands)<64,'CLI count before exec'); require(sum(len(e.get(k,'').encode()) for e in commands for k in ('stdout','stderr'))+2*FILE_CAP<=64*1024,'text reserve before exec')
    require(len(encoded(record))<1024*1024,'logical record memory bound'); require(len(encoded(pack(record)))<RECORD_CAP-24*1024,'shared record reserve before exec')
def gen_projection(record):
    e=record['effect']; mutation=record['mutation']
    return {'generation':record['gen'],'before_options':record['before_options'],
            'mutation':{k:mutation[k] for k in ('args','returncode','stdout','stderr')},
            'lifecycle':e['classification'],'daemon_exit_event':e['processes']['daemon']['exit_event'],
            'helper_exit_event':e['processes']['helper']['exit_event'],'service':record['post_observation']['service'],
            'after_options':record['post_observation'].get('options')}
def compare(actual,expected):
    if actual!=expected: raise RuntimeMismatch('measured Darwin oracle mismatch')

class Gen:
    def __init__(self,lot,name):
        self.lot,self.gen=lot,name; self.config=lot.config
        D.Run.__init__(self,{**lot.frozen,'root':str(root_for(self.config,lot.phase,name))},None)
        self.env=environment(self.config,self.phase,name); self.children=self.observed; self.label=LABELS[name]
        self.record.update(gen=name,environment=self.env,marker_ledgers=[],held_watches=[],endpoint_probes=[],transitions=[])
        lot.record['generations'].append(self.record)
    def remaining(self): return self.lot.remaining()
    def checkpoint(self): return self.lot.checkpoint()
    def socket_info(self): return D.Run.socket_info(self)
    def profiles(self,post_shutdown=False): return C.Run.profiles(self,post_shutdown)
    def wait_json(self,path): return C.Run.wait_json(self,path)
    def snapshot(self,pid): return C.Run.snapshot(self,pid)
    def ledger(self): return C.Run.ledger(self)
    def track(self,row,role):
        new=row['pid'] not in self.owned; value=D.Run.track(self,row,role)
        require(sum(len(g.owned) for g in self.lot.gens.values())<=4 and len(self.owned)<=2,'two daemons/two helpers total')
        if new:
            watch=self.watches[row['pid']]; current=D.birth(row['pid']); event=watch.poll_exit()
            self.record['held_watches'].append({'at':now(),'identity':saved(row),'role':role,'after_register':current,'exit_event':event,'kqueue_fd':watch._queue.fileno()})
            require(D.same(row,current) and not event,'live registered birth')
        return value
    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent,D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own=D.birth(pid)
                if own is None or own['ppid']!=parent['pid'] or pid in self.owned: continue
                self.track(own,'unexpected-helper'); self.record['helpers'].append({'at':now(),'parent':parent,'identity':own}); require(self.lot.cleaning,'unexpected own helper')
    def command(self,args,startup=False,required=True):
        check_budget(self.lot.record)
        present=sum(p.stat().st_size for p in self.lot.root.rglob('*') if p.is_file())
        require(present+len(encoded(pack(self.lot.record)))+24*1024<LOT_CAP,'whole lot reserve before CLI')
        argv=[self.binary,*([] if startup else ['-N']),'-S',str(self.socket)]
        if startup: argv+=['-f',str(self.root/'startup.conf')]
        argv+=args; entry={'start':now(),'argv':argv,'pre_exec_file_cap':FILE_CAP}; self.record['commands'].append(entry)
        old=resource.getrlimit(resource.RLIMIT_FSIZE); require(old[0]==resource.RLIM_INFINITY or old[0]>=FILE_CAP,'file limit too small')
        try:
            resource.setrlimit(resource.RLIMIT_FSIZE,(FILE_CAP,old[1]))
            with chdir(self.root): result=D.native.run(argv,env=self.env,timeout=self.remaining(),check=False)
            entry.update(pid=result.pid,returncode=result.returncode,stdout=result.stdout.decode(),stderr=result.stderr.decode())
            require(len(result.stdout)<FILE_CAP and len(result.stderr)<FILE_CAP,'CLI cap reached')
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
        else: require(D.stable(self.daemon,daemon) and endpoint==self.endpoint,'same daemon/endpoint')
        equal((pub['pane_dead'],pub['pane_dead_status'],pub['session_attached'],pub['window_width'],pub['window_height']),('0','','0','80','24'),'detached live pane geometry')
        item={'at':now(),'target':target,'public':pub,'daemon':daemon,'endpoint':endpoint}; self.record['public_samples'].append(item); return item
    def expected_for(self,label):
        p=self.config['pins']['python']
        return {'kind':label,'token':self.config['tokens'][self.phase][self.gen],'exe':p['path'],'exe_sha256':p['sha256'],'argv':witness_argv(self.config,self.phase,self.gen),
                'cwd':str(self.root/'cwd'),'environment':{k:self.env.get(k) for k in ENV_KEYS}}
    def observe(self,label,target,reobserve=False):
        require(sum(len(g.record['observations']) for g in self.lot.gens.values())<8 and len(self.record['observations'])<4,'eight challenges/sixteen child snapshots total')
        item=C.Run.observe(self,label,target,reobserve); nonce=item['request']['challenge']
        require(len(nonce)==48 and nonce not in self.lot.nonces and all(c in '0123456789abcdef' for c in nonce),'fresh nonce48hex'); self.lot.nonces.add(nonce)
        equal(item['ack']['exit_code'],0,'intention only'); equal((item['public']['session_name'],item['public']['window_name']),(label,label),'public label')
        if not reobserve: self.children[label]=saved(item)
        return item
    def options(self):
        out={name:self.command(['show-options','-sv',name]) for name in ('exit-empty','exit-unattached')}
        equal(out['exit-empty'],'on','exit-empty ON throughout'); require(out['exit-unattached'] in ('on','off'),'measured exit-unattached boolean'); return out
    def prepare(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh generation'); self.profiles()
        require(self.spawn_count==0,'one helper per generation'); self.spawn_count=1; target=self.command(spawn_plan(self.config,self.phase,self.gen),startup=True)
        require(target.startswith('%') and target[1:].isdigit(),'pane ID'); self.observe(self.label,target); self.observe(self.label,target,True)
        self.record['before_options']=self.options()
        if self.gen=='A': equal(self.record['before_options']['exit-unattached'],'off','explicit initial off')
        else: self.record['default_exit_unattached']=self.record['before_options']['exit-unattached']
        self.ledger(); self.profiles()
    def mutate(self):
        checks=[]
        for role,old in [('daemon',self.daemon['before']),('helper',self.children[self.label]['child']['before'])]:
            current=D.birth(old['pid']); event=self.watches[old['pid']].poll_exit(); checks.append({'role':role,'before':saved(old),'current':current,'exit_event':event})
            require(D.same(old,current) and not event,'live retained birth before mutation')
        args=['set-option','-s','exit-unattached','on'] if self.gen=='A' else ['source-file',str(self.root/'live.conf')]
        index=len(self.record['commands']); self.command(args,required=False); command=self.record['commands'][index]
        self.record['mutation']={'at':now(),'args':args if self.gen=='A' else ['source-file','PRIVATE/live.conf'],'command_index':index,'pre_mutation':checks,
                                 **{k:command[k] for k in ('start','end','returncode','stdout','stderr')}}
    def effect(self):
        start=time.monotonic(); end=start+20; rows={}
        for role,old in [('daemon',self.daemon['before']),('helper',self.children[self.label]['child']['before'])]:
            rows[role]={'before':saved(old),'exit_event':False,'numeric_status_claimed':False}
        out={'start':now(),'window_seconds':20,'poll_columns':['elapsed_seconds','daemon_NOTE_EXIT','helper_NOTE_EXIT'],'polls':[],'processes':rows,'cause':'option_activation'}
        self.record['effect']=out
        while True:
            events={role:self.watches[row['before']['pid']].poll_exit() for role,row in rows.items()}
            elapsed=time.monotonic()-start; out['polls'].append([elapsed,events['daemon'],events['helper']])
            for role,row in rows.items():
                if events[role]:
                    if not row['exit_event']: row['event_observed_at']=now()
                    row.update(exit_event=True,after=D.birth(row['before']['pid']),identity_checked_at=now())
            if all(row['exit_event'] and not D.same(row['before'],row.get('after')) for row in rows.values()): break
            left=end-time.monotonic()
            if left<=0: break
            require(time.monotonic()<self.lot.start+150,'work budget'); time.sleep(min(.025,left))
        for row in rows.values(): row.update(after=D.birth(row['before']['pid']),identity_checked_at=now())
        out.update(end=now(),seconds=time.monotonic()-start)
        out['classification']=classify(rows,out['seconds'])
        for row in rows.values():
            if row['exit_event'] and not D.same(row['before'],row['after']): check_exit(row)
        self.record['effect']=saved(out)  # Never alias later cleanup event updates.
    def endpoint_probe(self,cause):
        row={'at':now(),'cause':cause,'path_state':self.socket_info(),'probe':'AF_UNIX SOCK_STREAM connect; no CLI/autostart'}; self.record['endpoint_probes'].append(row)
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
            stream.settimeout(min(.5,self.remaining()))
            try: stream.connect(str(self.socket)); row.update(accepting=True,errno=None)
            except OSError as error: row.update(accepting=False,errno=error.errno,error=repr(error))
        return saved(row)
    def post_observe(self):
        classification=self.record['effect']['classification']
        if classification=='survived_20s':
            item=self.children[self.label]
            self.observe(self.label,item['target'],True); first=self.options(); inventory1=self.command(['list-sessions','-F','#{session_id}|#{session_name}|#{session_attached}'])
            self.observe(self.label,item['target'],True); second=self.options(); inventory2=self.command(['list-sessions','-F','#{session_id}|#{session_name}|#{session_attached}'])
            equal(first,second,'stable post-window options')
            if self.phase=='oracle': equal(first['exit-unattached'],'on','oracle activation effective')
            expected=item['public']['session_id']+'|'+self.label+'|0'; equal(inventory1,expected,'same private detached session'); equal(inventory2,expected,'second detached inventory')
            self.record['post_observation']={'at':now(),'service':'responding','options':first,'inventories':[inventory1,inventory2],'endpoint':self.socket_info(),'cause':'after_option_window_before_cleanup'}
        else:
            probe=self.endpoint_probe('option_activation')
            if classification=='exited': check_probe(probe)
            self.record['post_observation']={'at':now(),'service':'absent' if not probe['accepting'] and probe['errno'] in (errno.ENOENT,errno.ECONNREFUSED) else 'responding' if probe['accepting'] else 'unproven','endpoint_probe':probe}
        self.record['proof_frozen_at']=now(); self.record['projection']=gen_projection(self.record)

class Lot:
    def __init__(self,frozen):
        self.frozen,self.config,self.phase=frozen,frozen['config'],frozen['phase']; self.root=Path(frozen['root'])
        self.start=time.monotonic(); self.transition_end=self.start+20; self.cleaning=False; self.nonces=set(); self.gens={}
        self.record={'phase':self.phase,'start':now(),'generations':[],'fd_before':D.fd_snapshot(),'planned_generations':list(GENERATIONS)}
    def remaining(self):
        end=min(self.cleanup_end,self.start+180) if self.cleaning else min(self.transition_end,self.start+150)
        left=end-time.monotonic(); require(left>0,'lot/transition deadline'); return min(left,5)
    def checkpoint(self):
        out={'at':now(),'phase':self.phase,'generations':[{ 'gen':g.gen,'observations':len(g.record['observations']),'commands':len(g.record['commands']),
              'effect':g.record.get('effect',{}).get('classification'),'projection':g.record.get('projection')} for g in self.gens.values()]}
        raw=encoded(out); require(len(raw)<=FILE_CAP,'progress cap'); tmp=self.root/'progress.tmp'; tmp.write_bytes(raw); os.replace(tmp,self.root/'progress.json')
    def transition(self,g,name,fn):
        self.transition_end=min(time.monotonic()+20,self.start+150); row={'name':name,'start':now(),'monotonic_start':time.monotonic()}; g.record['transitions'].append(row)
        try: fn(); row['status']='COMPLETE'
        except BaseException as error: row.update(status='UNPROVEN',error=repr(error)); raise
        finally: row.update(end=now(),seconds=time.monotonic()-row['monotonic_start']); self.checkpoint()
    def execute(self):
        for name in GENERATIONS:
            self.gens[name]=Gen(self,name); g=self.gens[name]
            self.transition(g,'prepare',g.prepare)
            if name=='B': require(not D.same(g.daemon['before'],self.gens['A'].daemon['before']),'new generationB')
            self.transition(g,'mutation',g.mutate)
            # No product CLI runs during this complete20s observation window.
            self.transition(g,'option-window',g.effect)
            self.transition(g,'post-observation',g.post_observe)
            if self.phase=='candidate':
                expected=self.frozen['oracle_projection'][name]; actual=g.record['projection']
                if actual!=expected:
                    self.record['mismatch']={'at':now(),'generation':name,'expected':saved(expected),'actual':saved(actual),'owner':'L2 common runtime',
                                            'rule':'Stop at first runtime mismatch, before another generation; cleanup has not started.'}
                    compare(actual,expected)
            self.checkpoint()
    def cleanup(self):
        self.cleaning=True; self.cleanup_end=min(time.monotonic()+30,self.start+180)
        row={'start':now(),'errors':[],'rescues':[],'processes':[],'endpoints':[],'shutdowns':[]}; self.record['cleanup']=row
        def attempt(name,fn):
            try: return fn()
            except Exception as error: row['errors'].append({'step':name,'error':repr(error)})
        for g in self.gens.values():
            if g.daemon:
                old=g.daemon['before']; watch=g.watches[old['pid']]
                if not watch.poll_exit() and D.same(old,D.birth(old['pid'])):
                    def close(g=g,old=old):
                        current=D.process(old['pid']); require(D.stable(g.daemon,current) and g.socket_info()==g.endpoint,'private cleanup ownership')
                        entry={'at':now(),'gen':g.gen,'daemon':current,'endpoint':g.socket_info(),'command_index':len(g.record['commands']),'cause':'final_cleanup_not_option_effect'}; row['shutdowns'].append(entry)
                        g.command(['kill-server']); g.watches[old['pid']].wait(self.remaining())
                    attempt('kill-server-'+g.gen,close)
            for pid,old in g.owned.items():
                watch=g.watches[pid]; attempt('wait-'+str(pid),lambda:watch.wait(self.remaining()))
                if not watch.poll_exit() and D.same(old,D.birth(pid)):
                    row['rescues'].append({'identity':old,'signal':9}); attempt('owned-rescue-'+str(pid),lambda:watch.retire_after_failure(self.remaining()))
                try: collected,status=os.waitpid(pid,os.WNOHANG); wait={'pid':collected,'raw_status':status}
                except ChildProcessError: wait={'ECHILD':True,'note':'indirect; no helper numeric exit/reap inferred'}
                after=D.birth(pid)
                while D.same(old,after) and time.monotonic()<self.cleanup_end: time.sleep(.02); after=D.birth(pid)
                row['processes'].append({'gen':g.gen,'before':saved(old),'after':after,'exit_event':watch.exited,'waitpid':wait,'cause':'final_cleanup_observation'})
        row['all_absent']=bool(row['processes']) and all(p['exit_event'] and not D.same(p['before'],p['after']) for p in row['processes'])
        for g in self.gens.values():
            def endpoint_cleanup(g=g):
                require(row['all_absent'],'all births absent before unlink'); probe=g.endpoint_probe('final_cleanup'); check_probe(probe); current=g.socket_info()
                receipt={'gen':g.gen,'probe':probe,'before':current,'removed':False}; row['endpoints'].append(receipt)
                if current is not None:
                    fresh=[{**p,'after':D.birth(p['before']['pid'])} for p in row['processes']]; check_unlink(g.endpoint,current,fresh,probe)
                    receipt['identity_recheck']=fresh; g.socket.unlink(); receipt.update(removed=True,removed_at=now())
                receipt['after']=g.socket_info(); require(receipt['after'] is None,'endpoint residue')
            attempt('endpoint-'+g.gen,endpoint_cleanup); attempt('profiles-'+g.gen,lambda g=g:g.profiles(True))
            for watch in g.watches.values(): watch.close()
        row['socket_absent']=all(not g.socket.exists() for g in self.gens.values()); row['fd_after']=D.fd_snapshot(); row['fds_equal']=row['fd_after']==self.record['fd_before']; row['end']=now()
        row['pass']=bool(self.gens and row['all_absent'] and row['socket_absent'] and row['fds_equal'] and not row['errors'] and not row['rescues'] and time.monotonic()<=self.cleanup_end)
        check_cleanup(row,[p for g in self.gens.values() for p in g.owned.values()])

def verify_frozen(frozen):
    c=frozen['config']; require(frozen['phase'] in ('oracle','candidate'),'phase')
    for p in frozen['artifacts']: require(pin(p['path'])==p,'frozen pin changed '+p['path'])
    mandatory=[str(Path(__file__).resolve()),c['config_path'],c['adapter_pin']['path'],frozen['pure']['path'],*[p['path'] for p in c['pins'].values()],*[p['path'] for p in c['api_pins']]]
    mandatory += [str(root_for(c,phase,gen)/name) for phase in c['roots'] for gen in GENERATIONS for name in ('startup.conf','live.conf')]
    require(set(mandatory)<={p['path'] for p in frozen['artifacts']},'mandatory provenance')
    equal(c,json.loads(Path(c['config_path']).read_text()),'config'); equal(frozen['pins'],c['pins'],'runtime pins'); equal(frozen['root'],c['roots'][frozen['phase']],'root')
    equal(frozen['plans'],{g:spawn_plan(c,frozen['phase'],g) for g in GENERATIONS},'plans'); equal(frozen['environments'],{g:environment(c,frozen['phase'],g) for g in GENERATIONS},'environments')
    pure=json.loads(Path(frozen['pure']['path']).read_text()); require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure controls stale')
    require(not (Path(frozen['root'])/'STARTED.json').exists(),'single allowed lot already attempted')
    if frozen['phase']=='candidate':
        p=frozen['oracle_gate']; require(pin(p['path'])==p,'oracle gate pin'); gate=json.loads(Path(p['path']).read_text())
        require(gate['status']=='PASS' and pin(gate['result']['path'])==gate['result'],'complete oracle result'); equal(gate['driver_sha256'],digest(__file__),'same oracle driver')
        equal(frozen['oracle_projection'],gate['projection'],'measured oracle projection'); equal(set(gate['projection']),set(GENERATIONS),'complete two generation oracle')
        require(gate['live_observations']<=8 and gate['helpers']==2 and gate['daemon_generations']==2,'bounded complete oracle')

def execute(frozen_path):
    frozen=json.loads(frozen_path.read_text()); verify_frozen(frozen); bind_api(frozen['config']); lot=Lot(frozen)
    write_once(lot.root/'STARTED.json',{'at':now(),'frozen':pin(frozen_path),'observer':D.birth(os.getpid())},FILE_CAP)
    try: lot.execute(); lot.record['status']='PASS'
    except RuntimeMismatch as error: lot.record.update(status='RUNTIME_MISMATCH',error=str(error),stopped_before_next_generation=True)
    except BaseException as error: lot.record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=repr(error),traceback=traceback.format_exc())
    finally:
        try: lot.cleanup()
        except BaseException as error: lot.record.update(cleanup_exception=repr(error),status='UNPROVEN')
        lot.record.update(end=now(),seconds=time.monotonic()-lot.start,frozen=pin(frozen_path),distinct_nonces=len(lot.nonces),
                          limits=['Disk hashes not mapped-image attestation.','Finite known-parent sampling, not exhaustive transient coverage.',
                                  'NOTE_EXIT and ECHILD do not prove numeric helper status or author reap.','Cleanup exits are separate from option effects.'])
        document=pack(lot.record); equal(unpack(document),lot.record,'lossless evidence encoding')
        require(len(encoded(document))+sum(p.stat().st_size for p in lot.root.rglob('*') if p.is_file())+12*1024<LOT_CAP,'whole lot plus outer/gate reserve')
        write_once(lot.root/'RESULT.json',document)
    if lot.record['status']=='PASS' and lot.phase=='oracle':
        write_once(lot.root/'ORACLE-FROZEN.json',{'at':now(),'status':'PASS','result':pin(lot.root/'RESULT.json'),'driver_sha256':digest(__file__),
                   'projection':{g.gen:g.record['projection'] for g in lot.gens.values()},'live_observations':sum(len(g.record['observations']) for g in lot.gens.values()),
                   'helpers':sum(g.spawn_count for g in lot.gens.values()),'daemon_generations':len(lot.gens)})
    print(json.dumps({'status':lot.record['status'],'result':pin(lot.root/'RESULT.json'),'seconds':lot.record['seconds']}),flush=True)
    return 0 if lot.record['status']=='PASS' else 2 if lot.record['status']=='RUNTIME_MISMATCH' else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness': witness(*sys.argv[2:])
    elif len(sys.argv)==3 and sys.argv[1]=='--frozen': raise SystemExit(execute(Path(sys.argv[2])))
    elif len(sys.argv)==3 and sys.argv[1]=='--decode': print(json.dumps(read_record(sys.argv[2]),indent=2))
    else: raise SystemExit('Use --frozen FILE.json or --decode RESULT.json; witness is private.')
