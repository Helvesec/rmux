#!/usr/bin/env python3
"""Darwin exit-empty E1--E4 using pinned acquired011/012 APIs only.

Fresh complete Mac oracle precedes candidate. A residual endpoint pathname is
not a live listener. Private stale endpoints may be removed only after every
tracked generation is absent and a fresh non-timeout connect failure. No016
import, new FFI, attach, shared daemon or host termios change.
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
import stat
import sys
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode = True
C = D = None
KINDS = ('alpha','beta','gamma')
GENERATIONS = ('reuse','default')
ENV_KEYS = ('HOME','XDG_CONFIG_HOME','PATH','L6_EXITEMPTY_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM','L5_EXITEMPTY_API')
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_dead}|#{pane_dead_status}|#{session_name}|#{window_name}|#{window_width}|#{window_height}'
FIELDS = ('daemon_pid','socket','session_id','window_id','pane_id','pane_pid','pane_tty','pane_dead','pane_dead_status','session_name','window_name','window_width','window_height')
FILE_CAP = 4096
RECORD_CAP = 320*1024
CHECKPOINT_CAP = 32768
LOT_CAP = 512*1024

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
    path = Path(path)
    return {'path':str(path),'bytes':path.stat().st_size,'sha256':digest(path)}
def saved(value): return copy.deepcopy(value)
def encoded(value): return (json.dumps(value,separators=(',',':'))+'\n').encode()
def write_once(path,value,cap=RECORD_CAP):
    raw = encoded(value); require(len(raw)<=cap,'file cap before open')
    with Path(path).open('xb') as f: f.write(raw); f.flush(); os.fsync(f.fileno())
def atomic_once(path,value):
    path = Path(path); require(not path.exists(),'immutable marker exists')
    temp = path.with_suffix(path.suffix+'.tmp'); write_once(temp,value,FILE_CAP); os.rename(temp,path)
def bind_api(config):
    global C,D,Unproven,Failed
    ap = config['adapter_pin']; require(pin(ap['path'])==ap,'adapter provenance')
    equal(ap['sha256'],'3bb2a8a9faba1b860f00d7641696af364fddbaa48e28b79d846fa0249b14a7bb','acquired012 adapter')
    spec = importlib.util.spec_from_file_location('exitempty_acquired012',ap['path'])
    C = importlib.util.module_from_spec(spec); spec.loader.exec_module(C); C.bind_api(config['api_pins'])
    D = C.D; Unproven,Failed = D.Unproven,D.Failed
    C.ENV_KEYS = ENV_KEYS  # New allowlist in this observer only; disk API unchanged.
def generation(label): return 'default' if label=='gamma' else 'reuse'
def root_for(config,phase,gen): return Path(config['roots'][phase])/gen
def environment(config,phase,gen):
    root = root_for(config,phase,gen)
    return {'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'xdg'),'PATH':'/usr/bin:/bin','SHELL':'/bin/bash',
            'TERM':'vt100','LANG':'C','RMUX_DISABLE_TMUX_FALLBACK':'1','RMUX_DISABLE_TMUX_SHIM':'1',
            'L6_EXITEMPTY_MARKER':config['tokens'][phase][gen],'L5_EXITEMPTY_API':config['config_path']}
def witness_argv(config,phase,label):
    gen = generation(label)
    return [config['pins']['python']['path'],'-I',config['driver'],'witness',label,
            str(root_for(config,phase,gen)/'markers'),config['tokens'][phase][gen],'0']
def spawn_plan(config,phase,label):
    return ['new-session','-d','-P','-F','#{pane_id}','-s',label,'-n',label,'-x','80','-y','24',
            '-c',str(root_for(config,phase,generation(label))/'cwd'),*witness_argv(config,phase,label)]

def witness(label,root,token,code):
    bind_api(json.loads(Path(os.environ['L5_EXITEMPTY_API']).read_text())); require(label in KINDS and code=='0','planned helper')
    root = Path(root); own = D.birth(os.getpid()); key = C.stem(own)
    def report(challenge):
        return {'at':now(),'kind':label,'token':token,'challenge':challenge,'exit_code':0,
                'exit_code_scope':'intention/declaration only; product closure may signal helper',
                'identity':D.birth(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,
                'environment':{k:os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root/('born-'+key+'.json'),report(None)); end = time.monotonic()+120
    while time.monotonic()<end:
        requests = sorted(root.glob('request-'+key+'-*.json')); require(len(requests)<=7,'challenge cap')
        for path in requests:
            ack = root/path.name.replace('request-','ack-',1)
            if not ack.exists():
                require(path.stat().st_size<=FILE_CAP,'request cap'); request = json.loads(path.read_text())
                equal(set(request),{'identity','challenge','issued_at'},'challenge schema')
                require(D.same(own,request['identity']),'fresh challenge birth'); atomic_once(ack,report(request['challenge']))
        time.sleep(.02)
    os._exit(124)

def check_exit(row):
    require(row['exit_event'] and not D.same(row['before'],row['after']),'NOTE_EXIT and absent birth')
    equal(row['numeric_status_claimed'],False,'no fabricated nonparent status')
def check_empty(samples,daemon,endpoint):
    equal(len(samples),2,'two empty public samples')
    for s in samples:
        require(D.stable(daemon,s['daemon']) and D.stable(s['daemon'],s['daemon_after']),'same live empty generation')
        equal(s['endpoint'],endpoint,'same empty endpoint'); equal(s['sessions'],'','zero sessions')
        equal(s['option'],'off','empty option off'); require(not s['exit_event'],'empty daemon unexpectedly exited')
def check_probe(row):
    require(row['accepting'] is False and row['errno'] in (errno.ENOENT,errno.ECONNREFUSED),'no listener requires non-timeout refusal/absence')
def check_stale_removal(endpoint,current,identities,probe):
    require(identities and all(p['exit_event'] and not D.same(p['before'],p['after']) for p in identities),'all attributed generations/children absent before unlink')
    require(endpoint is not None and current==endpoint,'exact owned endpoint before unlink'); check_probe(probe)
    equal(probe['path_state'],current,'fresh probe inode'); require(current['uid']==os.getuid(),'owned endpoint uid')
def check_cleanup(row,owned):
    C.check_cleanup(row)
    expected = {(p['pid'],tuple(p['birth'])) for p in owned}
    actual = {(p['before']['pid'],tuple(p['before']['birth'])) for p in row['processes']}
    require(expected and expected==actual and len(row['processes'])==len(expected),'complete cleanup population')
def command_budget(record):
    commands = [e for g in record['generations'] for e in g['commands']]
    require(len(commands)<64,'CLI count before exec')
    require(sum(len(e.get(k,'').encode()) for e in commands for k in ('stdout','stderr'))+2*FILE_CAP<=128*1024,'text reserve before exec')
    require(len(encoded(record))<RECORD_CAP-48*1024,'raw record reserve before exec')

class Gen:
    def __init__(self,lot,name):
        self.lot,self.gen = lot,name
        D.Run.__init__(self,{**lot.frozen,'root':str(root_for(lot.config,lot.phase,name))},None)
        self.config = lot.config; self.env = environment(self.config,self.phase,name); self.children = self.observed
        self.record.update(gen=name,environment=self.env,empty_samples=[],closures=[],daemon_exits=[],endpoint_probes=[],marker_ledgers=[])
        lot.record['generations'].append(self.record)
    def remaining(self): return self.lot.remaining()
    def checkpoint(self): return self.lot.checkpoint(self)
    def socket_info(self): return D.Run.socket_info(self)
    def profiles(self,post_shutdown=False): return C.Run.profiles(self,post_shutdown)
    def wait_json(self,path): return C.Run.wait_json(self,path)
    def snapshot(self,pid): return C.Run.snapshot(self,pid)
    def track(self,row,role):
        value = D.Run.track(self,row,role)
        require(sum(len(g.owned) for g in self.lot.gens.values())<=5,'two daemons/three helpers cap')
        return value
    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent,D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own = D.birth(pid)
                if own is None or own['ppid']!=parent['pid'] or pid in self.owned: continue
                self.track(own,'unexpected-helper'); self.record['helpers'].append({'at':now(),'parent':parent,'identity':own})
                require(self.lot.cleaning,'unexpected own helper')
    def command(self,args,startup=False,required=True):
        command_budget(self.lot.record)
        argv = [self.binary,*([] if startup else ['-N']),'-S',str(self.socket)]
        if startup: argv += ['-f',str(self.root/'startup.conf')]
        argv += args; entry = {'start':now(),'argv':argv,'pre_exec_file_cap':FILE_CAP}; self.record['commands'].append(entry)
        old = resource.getrlimit(resource.RLIMIT_FSIZE); require(old[0]==resource.RLIM_INFINITY or old[0]>=FILE_CAP,'file limit too small')
        try:
            resource.setrlimit(resource.RLIMIT_FSIZE,(FILE_CAP,old[1]))
            with chdir(self.root): result = D.native.run(argv,env=self.env,timeout=self.remaining(),check=False)
            entry.update(pid=result.pid,returncode=result.returncode,stdout=result.stdout.decode(),stderr=result.stderr.decode())
            require(len(result.stdout)<FILE_CAP and len(result.stderr)<FILE_CAP,'CLI output cap reached')
        except BaseException as error: entry['error'] = repr(error); raise
        finally: resource.setrlimit(resource.RLIMIT_FSIZE,old); entry['end'] = now()
        if required: require(result.returncode==0,'CLI failed '+repr(args))
        return result.stdout.decode().strip()
    def public(self,target):
        fields = self.command(['display-message','-p','-t',target,FORMAT]).split('|'); equal(len(fields),len(FIELDS),'public fields')
        pub = dict(zip(FIELDS,fields)); pub['daemon_pid']=int(pub['daemon_pid']); pub['pane_pid']=int(pub['pane_pid'])
        equal(pub['socket'],str(self.socket),'private endpoint'); endpoint=self.socket_info(); require(endpoint is not None,'endpoint exists')
        daemon=D.process(pub['daemon_pid']); self.track(daemon['before'],'daemon'); key='tmux' if self.phase=='oracle' else 'rmux_daemon'
        equal(daemon['image']['executable'],self.config['pins'][key]['path'],'daemon executable'); equal(daemon['image']['sha256'],self.config['pins'][key]['sha256'],'daemon disk hash')
        if self.daemon is None: self.daemon,self.endpoint=saved(daemon),saved(endpoint)
        else: require(D.stable(self.daemon,daemon) and endpoint==self.endpoint,'daemon/endpoint identity changed')
        equal((pub['pane_dead'],pub['pane_dead_status'],pub['window_width'],pub['window_height']),('0','','80','24'),'live pane and geometry')
        item={'at':now(),'target':target,'public':pub,'daemon':daemon,'endpoint':endpoint}; self.record['public_samples'].append(item); return item
    def expected_for(self,label):
        p=self.config['pins']['python']
        return {'kind':label,'token':self.config['tokens'][self.phase][self.gen],'exe':p['path'],'exe_sha256':p['sha256'],
                'argv':witness_argv(self.config,self.phase,label),'cwd':str(self.root/'cwd'),'environment':{k:self.env.get(k) for k in ENV_KEYS}}
    def observe(self,label,target,reobserve=False):
        require(sum(len(g.record['observations']) for g in self.lot.gens.values())<7,'seven live observation cap')
        item=C.Run.observe(self,label,target,reobserve)
        nonce=item['request']['challenge']; require(len(nonce)==48 and nonce not in self.lot.nonces,'fresh unique48hex nonce')
        self.lot.nonces.add(nonce); equal(item['ack']['exit_code'],0,'helper intention only')
        equal((item['public']['session_name'],item['public']['window_name']),(label,label),'public label')
        if not reobserve: self.children[label]=saved(item)
        return item
    def spawn(self,label):
        require(generation(label)==self.gen and self.spawn_count<(1 if self.gen=='default' else 2),'planned helper count')
        self.spawn_count+=1; target=self.command(spawn_plan(self.config,self.phase,label),startup=label in ('alpha','gamma'))
        require(target.startswith('%') and target[1:].isdigit(),'new paneID'); item=self.observe(label,target); self.observe(label,target,True)
        return saved(item)
    def close_window(self,label):
        child=self.children[label]; own=child['child']['before']; watch=self.watches[own['pid']]
        pub=self.public(child['public']['pane_id']); equal(pub['public'],child['public'],'exact live private closure target')
        current=self.snapshot(own['pid']); require(D.stable(child['child'],current) and not watch.poll_exit() and D.same(own,D.birth(own['pid'])),'retained live closure identity')
        e={'label':label,'at':now(),'before':saved(own),'preclosure':current,'command':['kill-window','-t',child['public']['window_id']],
           'command_index':len(self.record['commands']),'numeric_status_claimed':False,'status':'UNPROVEN'}; self.record['closures'].append(e)
        self.command(e['command']); watch.wait(self.remaining()); after=D.birth(own['pid'])
        while D.same(own,after): time.sleep(min(.02,self.remaining())); after=D.birth(own['pid'])
        e.update(exit_event=watch.exited,after=after); check_exit(e); e['status']='PASS'; self.checkpoint()
    def empty_server(self):
        samples=[]; self.record['empty_samples'].append(samples); own=self.daemon['before']; watch=self.watches[own['pid']]
        for _ in range(2):
            row={'at':now(),'daemon':D.process(own['pid']),'endpoint':self.socket_info(),
                 'sessions':self.command(['list-sessions','-F','#{session_id}|#{session_name}']),
                 'option':self.command(['show-options','-sv','exit-empty']),
                 'daemon_after':D.process(own['pid']),'exit_event':watch.poll_exit()}
            samples.append(saved(row)); time.sleep(min(.05,self.remaining()))
        check_empty(samples,self.daemon,self.endpoint); return saved(samples)
    def endpoint_probe(self):
        row={'at':now(),'path_state':self.socket_info(),'probe':'AF_UNIX SOCK_STREAM connect; no CLI/autostart'}
        self.record['endpoint_probes'].append(row)
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
            stream.settimeout(min(.5,self.remaining()))
            try: stream.connect(str(self.socket)); row.update(accepting=True,errno=None)
            except OSError as error: row.update(accepting=False,errno=error.errno,error=repr(error))
        check_probe(row); row.update(listener_absent=True,pathname_absent=row['path_state'] is None); return saved(row)
    def daemon_exit(self,limit=3):
        own=self.daemon['before']; watch=self.watches[own['pid']]; end=time.monotonic()+min(limit,self.remaining())
        while not watch.poll_exit() and time.monotonic()<end: time.sleep(.02)
        row={'at':now(),'before':saved(own),'exit_event':watch.exited,'numeric_status_claimed':False,'after':D.birth(own['pid'])}
        self.record['daemon_exits'].append(row)
        if not row['exit_event']: return False
        while D.same(own,row['after']): time.sleep(min(.02,self.remaining())); row['after']=D.birth(own['pid'])
        check_exit(row); row['endpoint']=self.endpoint_probe(); return True
    def ledger(self): return C.Run.ledger(self)

class Lot:
    def __init__(self,frozen):
        self.frozen,self.config,self.phase=frozen,frozen['config'],frozen['phase']; self.root=Path(frozen['root'])
        self.start=time.monotonic(); self.case_end=self.start+20; self.cleaning=False; self.nonces=set(); self.gens={}
        self.record={'phase':self.phase,'start':now(),'generations':[],'cases':[],'fd_before':D.fd_snapshot()}
    def remaining(self):
        end=min(self.cleanup_end,self.start+120) if self.cleaning else min(self.case_end,self.start+90)
        left=end-time.monotonic(); require(left>0,'lot/transition deadline'); return min(left,5)
    def checkpoint(self,gen):
        row={'at':now(),'phase':self.phase,'cases':saved(self.record['cases']),
             'counts':{g.gen:{k:len(g.record[k]) for k in ('observations','commands','closures')} for g in self.gens.values()},
             'last_observation':saved(gen.record['observations'][-1:]),'cleanup':self.record.get('cleanup')}
        raw=encoded(row); require(len(raw)<=CHECKPOINT_CAP,'checkpoint cap'); tmp=self.root/'progress.tmp'; tmp.write_bytes(raw); os.replace(tmp,self.root/'progress.json')
    def execute(self):
        for name in GENERATIONS:
            root=root_for(self.config,self.phase,name); require(not (root/'s').exists() and not list((root/'markers').iterdir()),'fresh generation')
        self.gens['reuse']=Gen(self,'reuse'); reuse=self.gens['reuse']; reuse.profiles()
        for case in ('E1','E2','E3','E4'):
            self.case_end=min(time.monotonic()+20,self.start+90); e={'id':case,'start':now(),'monotonic_start':time.monotonic()}; self.record['cases'].append(e)
            try:
                g=reuse
                if case=='E1':
                    reuse.spawn('alpha'); reuse.command(['set-option','-s','exit-empty','off']); equal(reuse.command(['show-options','-sv','exit-empty']),'off','explicit off')
                    reuse.close_window('alpha'); e['empty']=reuse.empty_server()
                elif case=='E2':
                    item=reuse.spawn('beta'); require(not D.same(item['child']['before'],reuse.children['alpha']['child']['before']),'fresh beta birth')
                    e['same_daemon']=D.stable(reuse.daemon,reuse.public(item['public']['pane_id'])['daemon']); require(e['same_daemon'],'same daemon reuse')
                    equal(reuse.command(['show-options','-sv','exit-empty']),'off','reuse keeps off')
                    e.update(alpha_birth=saved(reuse.children['alpha']['child']['before']),beta_birth=saved(item['child']['before']))
                elif case=='E3':
                    reuse.command(['source-file',str(reuse.root/'live.conf')]); equal(reuse.command(['show-options','-sv','exit-empty']),'on','source-file on')
                    reuse.observe('beta',reuse.children['beta']['public']['pane_id'],True); reuse.close_window('beta')
                    require(reuse.daemon_exit(),'explicit on daemon did not exit within3s'); e['endpoint']=saved(reuse.record['endpoint_probes'][-1])
                else:
                    self.gens['default']=Gen(self,'default'); g=self.gens['default']; g.profiles(); g.spawn('gamma')
                    require(not D.same(g.daemon['before'],reuse.daemon['before']),'new daemon generation')
                    default=g.command(['show-options','-sv','exit-empty']); require(default in ('on','off'),'measured default value')
                    g.close_window('gamma'); exited=g.daemon_exit(); e['observed']={'default':default,'behavior':'exited' if exited else 'alive_after_3s'}
                    require(exited,'fresh default daemon did not terminate within3s')
                    e['endpoint']=saved(g.record['endpoint_probes'][-1])
                    if self.phase=='candidate': equal(e['observed'],self.frozen['oracle_default'],'fresh default vs Mac oracle')
                g.ledger(); g.profiles(); self.remaining(); e['status']='PASS'
            except Exception as error: e.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=str(error)); raise
            finally: e.update(end=now(),seconds=time.monotonic()-e['monotonic_start']); self.checkpoint(g)
    def cleanup(self):
        self.cleaning=True; self.cleanup_end=min(time.monotonic()+30,self.start+120)
        row={'start':now(),'errors':[],'rescues':[],'processes':[],'endpoints':[]}; self.record['cleanup']=row
        def attempt(name,fn):
            try: return fn()
            except Exception as error: row['errors'].append({'step':name,'error':repr(error)})
        for gen in self.gens.values():
            if gen.daemon:
                own=gen.daemon['before']; watch=gen.watches[own['pid']]
                if not watch.poll_exit() and D.same(own,D.birth(own['pid'])):
                    def close(g=gen,o=own):
                        require(D.stable(g.daemon,D.process(o['pid'])) and g.socket_info()==g.endpoint,'cleanup ownership')
                        g.command(['kill-server']); g.watches[o['pid']].wait(self.remaining())
                    attempt('qualified-shutdown-'+gen.gen,close)
            for pid,old in gen.owned.items():
                watch=gen.watches[pid]; attempt('wait-'+str(pid),lambda:watch.wait(self.remaining()))
                if not watch.poll_exit() and D.same(old,D.birth(pid)):
                    row['rescues'].append({'identity':old,'signal':9}); attempt('owned-rescue-'+str(pid),lambda:watch.retire_after_failure(self.remaining()))
                try:
                    collected,status=os.waitpid(pid,os.WNOHANG); reaped={'pid':collected,'raw_status':status}
                except ChildProcessError: reaped={'ECHILD':True,'note':'indirect; NOTE_EXIT and birth absence separately'}
                after=D.birth(pid)
                while D.same(old,after) and time.monotonic()<self.cleanup_end: time.sleep(.02); after=D.birth(pid)
                row['processes'].append({'generation':gen.gen,'before':saved(old),'after':after,'exit_event':watch.exited,'waitpid':reaped})
        row['all_absent']=bool(row['processes']) and all(p['exit_event'] and not D.same(p['before'],p['after']) for p in row['processes'])
        for gen in self.gens.values():
            def endpoint_cleanup(g=gen):
                require(row['all_absent'],'no endpoint removal while any attributed generation is live')
                probe=g.endpoint_probe(); current=g.socket_info(); e={'generation':g.gen,'probe':probe,'before':current,'removed':False}; row['endpoints'].append(e)
                if current is not None:
                    fresh=[{**p,'after':D.birth(p['before']['pid'])} for p in row['processes']]
                    check_stale_removal(g.endpoint,current,fresh,probe); e['identity_recheck']=fresh
                    g.socket.unlink(); e['removed']=True; e['removed_at']=now()
                e['after']=g.socket_info(); require(e['after'] is None,'endpoint residue')
            attempt('endpoint-'+gen.gen,endpoint_cleanup); attempt('profiles-'+gen.gen,lambda g=gen:g.profiles(True))
            for watch in gen.watches.values(): watch.close()
        row['socket_absent']=all(not g.socket.exists() for g in self.gens.values())
        row['fd_after']=D.fd_snapshot(); row['fds_equal']=row['fd_after']==self.record['fd_before']; row['end']=now()
        row['pass']=bool(self.gens and row['all_absent'] and row['socket_absent'] and row['fds_equal'] and not row['errors'] and not row['rescues'] and time.monotonic()<=self.cleanup_end)
        check_cleanup(row,[p for g in self.gens.values() for p in g.owned.values()])

def verify_frozen(frozen):
    c=frozen['config']; require(frozen['phase'] in ('oracle','candidate'),'phase')
    for p in frozen['artifacts']: require(pin(p['path'])==p,'frozen pin changed '+p['path'])
    mandatory=[str(Path(__file__).resolve()),c['config_path'],c['adapter_pin']['path'],frozen['pure']['path'],*[p['path'] for p in c['pins'].values()],*[p['path'] for p in c['api_pins']]]
    mandatory += [str(root_for(c,phase,gen)/n) for phase in ('oracle','candidate') for gen in GENERATIONS for n in ('startup.conf','live.conf')]
    require(set(mandatory)<={p['path'] for p in frozen['artifacts']},'mandatory provenance')
    equal(frozen['pins'],c['pins'],'runtime pins'); equal(c,json.loads(Path(c['config_path']).read_text()),'config'); equal(frozen['root'],c['roots'][frozen['phase']],'root')
    equal(frozen['plans'],{k:spawn_plan(c,frozen['phase'],k) for k in KINDS},'plans'); equal(frozen['environments'],{g:environment(c,frozen['phase'],g) for g in GENERATIONS},'environments')
    pure=json.loads(Path(frozen['pure']['path']).read_text()); require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure controls stale')
    require(not (Path(frozen['root'])/'STARTED.json').exists(),'lot already attempted')
    if frozen['phase']=='candidate':
        p=frozen['oracle_gate']; require(pin(p['path'])==p,'oracle gate pin'); gate=json.loads(Path(p['path']).read_text())
        require(gate['status']=='PASS' and pin(gate['result']['path'])==gate['result'],'complete oracle result')
        equal(gate['driver_sha256'],digest(__file__),'oracle driver'); equal(frozen['oracle_default'],gate['default'],'oracle default observation')
        equal(gate['live_observations'],7,'complete bounded oracle observations'); equal(gate['helpers'],3,'oracle helpers'); equal(gate['daemon_generations'],2,'oracle daemons')

def execute(frozen_path):
    frozen=json.loads(frozen_path.read_text()); verify_frozen(frozen); bind_api(frozen['config']); lot=Lot(frozen)
    write_once(lot.root/'STARTED.json',{'at':now(),'frozen':pin(frozen_path),'observer':D.birth(os.getpid())})
    try: lot.execute(); lot.record['status']='PASS'
    except BaseException as error: lot.record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=repr(error),traceback=traceback.format_exc())
    finally:
        try: lot.cleanup()
        except BaseException as error: lot.record.update(cleanup_exception=repr(error),status='UNPROVEN')
        lot.record.update(end=now(),seconds=time.monotonic()-lot.start,frozen=pin(frozen_path),distinct_nonces=len(lot.nonces),
                          limits=['Disk hashes not mapped-image attestation.','Scoped own-helper sampling not exhaustive transient coverage.',
                                  'Indirect NOTE_EXIT and ECHILD not author reap or numeric exit status.','Pathname and accepting listener are distinct observations.'])
        require(len(encoded(lot.record))+sum(p.stat().st_size for p in lot.root.rglob('*') if p.is_file())+8192<LOT_CAP,'lot logs plus outer/gate reserve')
        write_once(lot.root/'RESULT.json',lot.record)
    if lot.record['status']=='PASS' and lot.phase=='oracle':
        write_once(lot.root/'ORACLE-FROZEN.json',{'at':now(),'status':'PASS','result':pin(lot.root/'RESULT.json'),'driver_sha256':digest(__file__),
                   'default':lot.record['cases'][-1]['observed'],'live_observations':sum(len(g.record['observations']) for g in lot.gens.values()),
                   'helpers':sum(g.spawn_count for g in lot.gens.values()),'daemon_generations':len(lot.gens),
                   'endpoint_semantics':'NOTE_EXIT + birth absence + failed non-timeout connection; pathname state recorded separately before cleanup.'})
    print(json.dumps({'status':lot.record['status'],'result':pin(lot.root/'RESULT.json'),'seconds':lot.record['seconds']}),flush=True)
    return 0 if lot.record['status']=='PASS' else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness': witness(*sys.argv[2:])
    elif len(sys.argv)==3 and sys.argv[1]=='--frozen': raise SystemExit(execute(Path(sys.argv[2])))
    else: raise SystemExit('Use --frozen FILE.json, or private witness mode.')
