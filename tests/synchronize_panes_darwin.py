#!/usr/bin/env python3
"""Finite Darwin synchronize-panes send-keys experiment, private line receipts.

Only acquired011/012 APIs and accepted020 strict codec; no022 code dependency.
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
import select
import socket
import sys
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode=True
C=D=None
KINDS=('alpha','beta')
CASES=('P1','P2','P3')
ENV_KEYS=('HOME','XDG_CONFIG_HOME','PATH','L5_SYNCHRONIZE_MARKER','BASH_ENV','ENV','RMUX_DISABLE_TMUX_SHIM','L5_SYNCHRONIZE_API')
FIELDS=('daemon_pid','socket','session_id','window_id','pane_id','pane_pid','pane_tty','pane_dead','pane_dead_status','session_attached','session_name','window_name','window_width','window_height')
FORMAT='#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_dead}|#{pane_dead_status}|#{session_attached}|#{session_name}|#{window_name}|#{window_width}|#{window_height}'
FILE_CAP=8192
RECORD_CAP=512*1024
LOT_CAP=1024*1024
class RuntimeMismatch(RuntimeError): pass
class OracleUnexercised(RuntimeError): pass


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
    spec=importlib.util.spec_from_file_location('synchronize_acquired012',ap['path']); C=importlib.util.module_from_spec(spec); spec.loader.exec_module(C)
    C.bind_api(config['api_pins']); D=C.D; Unproven,Failed=D.Unproven,D.Failed; C.ENV_KEYS=ENV_KEYS

def environment(config,phase):
    root=Path(config['roots'][phase])
    return {'HOME':str(root/'home'),'XDG_CONFIG_HOME':str(root/'xdg'),'PATH':'/usr/bin:/bin','SHELL':'/bin/bash','TERM':'vt100','LANG':'C',
            'RMUX_DISABLE_TMUX_FALLBACK':'1','RMUX_DISABLE_TMUX_SHIM':'1','L5_SYNCHRONIZE_MARKER':config['tokens'][phase],'L5_SYNCHRONIZE_API':config['config_path']}

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

def hex_nonce(value):
    return type(value) is str and len(value)==48 and all(c in '0123456789abcdef' for c in value)
def wire_line(token,phase,nonce):
    require(hex_nonce(token) and hex_nonce(nonce) and phase in (*CASES,'SA','SB'),'frozen line fields')
    return ('L5|'+token+'|'+phase+'|'+nonce+'\n').encode('ascii')
def parse_line(raw,token,nonces):
    require(type(raw) is bytes and len(raw)<=128 and raw.endswith(b'\n') and raw.count(b'\n')==1,'one complete bounded line')
    try: tag,actual,phase,nonce=raw[:-1].decode('ascii').split('|')
    except (ValueError,UnicodeDecodeError): raise Unproven('line syntax')
    require(tag=='L5' and actual==token and phase in nonces and nonce==nonces[phase],'phase/token/nonce binding')
    equal(raw,wire_line(token,phase,nonce),'canonical line bytes'); return phase,nonce

def receipt_sha(row): return hashlib.sha256(encoded(row)).hexdigest()
def validate_prefix(rows,previous,own,label,token,nonces):
    require(type(rows) is list and len(rows)<=6,'six receipt cap'); equal(rows[:len(previous)],previous,'immutable previous prefix'); require(len(rows)>=len(previous),'prefix shortened')
    seen=set(); prev=None
    for index,row in enumerate(rows,1):
        equal(set(row),{'at','identity','label','token','phase','nonce','counter','line_hex','previous_sha256'},'receipt schema')
        equal(row['identity'],own,'exact receipt instance'); equal(row['label'],label,'helper label'); equal(row['token'],token,'receipt fixture token')
        require(type(row['counter']) is int and row['counter']==index,'counter contiguous nonbool')
        equal(row['previous_sha256'],prev,'ordered receipt hash chain')
        try: raw=bytes.fromhex(row['line_hex'])
        except ValueError: raise Unproven('line hex')
        phase,nonce=parse_line(raw,token,nonces); equal((row['phase'],row['nonce']),(phase,nonce),'receipt content')
        require(nonce not in seen,'duplicate receipt nonce'); seen.add(nonce); prev=receipt_sha(row)
    return [row['phase'] for row in rows]

def check_sealed(prefixes,sent_before,sent_after,identities,token,nonces,injections):
    allowed={x['phase']:x for x in injections}; require(set(identities)==set(KINDS),'two owned helpers')
    for label in KINDS:
        phases=validate_prefix(prefixes[label],sent_before[label],identities[label],label,token,nonces)
        equal(prefixes[label],sent_after[label],'barrier prefix matches sealed snapshot')
        own_sentinel='SA' if label=='alpha' else 'SB'; other='SB' if label=='alpha' else 'SA'
        require(phases and phases[-1]==own_sentinel and phases.count(own_sentinel)==1 and other not in phases,'ordered own final sentinel')
        require(all(phase in allowed for phase in phases),'no uninjected phase')
        order=[allowed[phase]['index'] for phase in phases]; equal(order,sorted(order),'input order matches injection order')
    return {phase:[label for label in KINDS if phase in [r['phase'] for r in prefixes[label]]] for phase in CASES if phase in allowed}

def witness(label,root,token,code):
    config=json.loads(Path(os.environ['L5_SYNCHRONIZE_API']).read_text()); bind_api(config); require(label in KINDS and code=='0','planned helper')
    phase=next(k for k,value in config['tokens'].items() if value==token); nonces=config['line_nonces'][phase]
    root=Path(root); own=D.birth(os.getpid()); key=C.stem(own); rows=[]; buffer=b''; os.set_blocking(0,False)
    def report(challenge):
        return {'at':now(),'kind':label,'token':token,'challenge':challenge,'identity':D.birth(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,
                'environment':{k:os.environ.get(k) for k in ENV_KEYS},'receipts':saved(rows),'partial_bytes':len(buffer),'pty':os.ttyname(0),'barrier':'stdin drained to EAGAIN before prefix snapshot'}
    def drain():
        nonlocal buffer
        while True:
            try: chunk=os.read(0,1024)
            except BlockingIOError: break
            require(chunk,'own PTY input closed'); buffer+=chunk; require(len(buffer)<=4096,'input buffer cap')
            while b'\n' in buffer:
                raw,buffer=buffer.split(b'\n',1); raw+=b'\n'; p,n=parse_line(raw,token,nonces)
                row={'at':now(),'identity':saved(own),'label':label,'token':token,'phase':p,'nonce':n,'counter':len(rows)+1,'line_hex':raw.hex(),'previous_sha256':receipt_sha(rows[-1]) if rows else None}
                validate_prefix([*rows,row],rows,own,label,token,nonces)
                atomic_once(root/f'line-{key}-{row["counter"]}.json',row); rows.append(saved(row))
    atomic_once(root/('born-'+key+'.json'),report(None)); end=time.monotonic()+180
    while time.monotonic()<end:
        if select.select([0],[],[],.02)[0]: drain()
        requests=sorted(root.glob('request-'+key+'-*.json')); require(len(requests)<=12,'barrier cap')
        for path in requests:
            ack=root/path.name.replace('request-','ack-',1)
            if ack.exists(): continue
            require(path.stat().st_size<=FILE_CAP,'request file cap'); request=json.loads(path.read_text())
            equal(set(request),{'identity','challenge','issued_at'},'barrier request schema'); require(D.same(own,request['identity']) and hex_nonce(request['challenge']),'own fresh barrier')
            drain(); atomic_once(ack,report(request['challenge']))
    os._exit(124)

def spawn_plan(config,phase,label):
    args=['new-session','-d','-P','-F','#{pane_id}','-s','proof','-n','pair','-x','80','-y','24'] if label=='alpha' else ['split-window','-d','-h','-P','-F','#{pane_id}','-t','proof:0.0']
    return [*args,'-c',str(Path(config['roots'][phase])/'cwd'),*witness_argv(config,phase,label)]

def check_budget(record):
    require(len(record['commands'])<128,'CLI cap')
    require(sum(len(e.get(k,'').encode()) for e in record['commands'] for k in ('stdout','stderr'))+2*FILE_CAP<128*1024,'CLI text reserve')
    require(len(encoded(record))<768*1024,'logical record memory cap')
    require(len(encoded(pack(record)))<RECORD_CAP-64*1024,'packed record reserve')

def projection(record):
    return {'recipients':record['sealed']['recipients'],'scopes':[{k:row[k] for k in ('case','point','window_local','effective')} for row in record['scopes']],
            'case_statuses':[{k:row[k] for k in ('id','status')} for row in record['cases']]}

class Run:
    def __init__(self,frozen):
        D.Run.__init__(self,frozen,None); self.config=frozen['config']; self.env=environment(self.config,self.phase); self.children=self.observed
        self.nonces=set(); self.prefixes={k:[] for k in KINDS}; self.identities={}; self.targets={}; self.oracle=frozen.get('oracle_projection')
        self.record.update(environment=self.env,marker_ledgers=[],held_watches=[],scopes=[],mutations=[],injections=[],barriers=[],endpoint_probes=[])
    def remaining(self):
        end=min(self.cleanup_end,self.start+180) if self.cleaning else min(self.case_end,self.start+150)
        left=end-time.monotonic(); require(left>0,'lot/transition bound'); return min(left,5)
    def checkpoint(self):
        row={'at':now(),'phase':self.phase,'cases':self.record['cases'],'observations':len(self.record['observations']),'injections':len(self.record['injections']),'commands':len(self.record['commands'])}
        raw=encoded(row); require(len(raw)<=FILE_CAP,'checkpoint cap'); tmp=self.root/'progress.tmp'; tmp.write_bytes(raw); os.replace(tmp,self.root/'progress.json')
    def observe(self,label):
        require(len(self.record['observations'])<12,'12barriers24snapshots max'); item=C.Run.observe(self,label,self.targets[label],label in self.children)
        nonce=item['request']['challenge']; require(hex_nonce(nonce) and nonce not in self.nonces,'fresh barrier nonce'); self.nonces.add(nonce)
        own=item['born']['identity']; self.identities.setdefault(label,saved(own)); equal(own,self.identities[label],'same born identity')
        ack=item['ack']; equal(ack['partial_bytes'],0,'complete lines at barrier'); equal(ack['pty'],item['public']['pane_tty'],'receipt PTY')
        phases=validate_prefix(ack['receipts'],self.prefixes[label],own,label,self.config['tokens'][self.phase],self.config['line_nonces'][self.phase])
        for row in ack['receipts']:
            path=self.root/'markers'/f'line-{C.stem(own)}-{row["counter"]}.json'; equal(json.loads(path.read_text()),row,'immutable receipt disk bytes')
        self.prefixes[label]=saved(ack['receipts']); self.record['barriers'].append({'at':now(),'label':label,'challenge':nonce,'observation_index':len(self.record['observations'])-1,'prefix':saved(ack['receipts']),'phases':phases})
        if not item['reobserve']: self.children[label]=saved(item)
        return item
    def pair(self):
        for label in KINDS: self.observe(label)
        return saved(self.prefixes)
    def spawn(self,label):
        require(label not in self.targets and self.spawn_count<2,'two helpers only'); self.spawn_count+=1
        target=self.command(spawn_plan(self.config,self.phase,label),startup=label=='alpha'); require(target.startswith('%') and target[1:].isdigit(),'pane ID')
        self.targets[label]=target; self.observe(label)
        if label=='beta':
            a,b=[self.children[k]['public'] for k in KINDS]; require(a['session_id']==b['session_id'] and a['window_id']==b['window_id'] and a['pane_id']!=b['pane_id'],'one window two panes')
    def mutate(self,case,args):
        live=[]
        for old in self.owned.values():
            current=D.birth(old['pid']); event=self.watches[old['pid']].poll_exit(); require(D.same(old,current) and not event,'retained owned live birth'); live.append({'before':saved(old),'after':current,'exit_event':event})
        index=len(self.record['commands']); self.command(args,required=False); cmd=self.record['commands'][index]
        row={'case':case,'command_index':index,'live':live,**{k:cmd[k] for k in ('argv','returncode','stdout','stderr','start','end')}}; self.record['mutations'].append(row)
        if cmd['returncode']!=0 and self.phase=='candidate': raise RuntimeMismatch('option command differs from oracle')
        require(cmd['returncode']==0,'oracle option command failed')
    def scopes(self,case,point,value):
        window=self.children['alpha']['public']['window_id']; indices=[]
        def query(args): indices.append(len(self.record['commands'])); return self.command(args)
        row={'at':now(),'case':case,'point':point,'window_local':query(['show-options','-wqv','-t',window,'synchronize-panes']),'effective':query(['show-options','-wAv','-t',window,'synchronize-panes']),'command_indices':indices}
        self.record['scopes'].append(saved(row)); equal((row['window_local'],row['effective']),(value,value),'actual scoped synchronization option'); return row
    def inject(self,phase,label):
        require(len(self.record['injections'])<6,'six injections maximum'); nonce=self.config['line_nonces'][self.phase][phase]
        require(all(x['nonce']!=nonce for x in self.record['injections']),'fresh line nonce')
        line=wire_line(self.config['tokens'][self.phase],phase,nonce); index=len(self.record['injections']); commands=[]
        for args in (['send-keys','-l','-t',self.targets[label],line[:-1].decode('ascii')],['send-keys','-t',self.targets[label],'Enter']):
            commands.append(len(self.record['commands'])); self.command(args)
        row={'index':index,'at':now(),'phase':phase,'target_label':label,'target_pane':self.targets[label],'nonce':nonce,'line_hex':line.hex(),'command_indices':commands}; self.record['injections'].append(row)
        # A real receiver must produce a complete-line receipt; echo is irrelevant.
        end=time.monotonic()+min(2,self.remaining())
        while True:
            own=self.identities[label]; paths=sorted((self.root/'markers').glob('line-'+C.stem(own)+'-*.json'))
            if any(json.loads(p.read_text()).get('nonce')==nonce for p in paths): break
            require(time.monotonic()<end,'target receipt deadline'); time.sleep(.02)
    def seal(self):
        self.case_end=min(time.monotonic()+20,self.start+150); before=saved(self.prefixes)
        self.mutate('FINAL',['set-option','-w','-t',self.children['alpha']['public']['window_id'],'synchronize-panes','off']); self.scopes('FINAL','before_sentinels','off')
        self.inject('SA','alpha'); self.inject('SB','beta'); after=self.pair()
        recipients=check_sealed(after,before,self.prefixes,self.identities,self.config['tokens'][self.phase],self.config['line_nonces'][self.phase],self.record['injections'])
        self.record['sealed']={'at':now(),'before_sentinels':before,'after_sentinels':after,'recipients':recipients,'identity_snapshots':saved(self.identities),'closed_by':'ordered own final sentinel plus fresh stdin-drained barrier on each helper'}
        return recipients
    def execute(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh fixture'); self.profiles(); self.case_end=min(time.monotonic()+20,self.start+150)
        for label in KINDS: self.spawn(label)
        previous=self.pair(); stopped=False
        for case in CASES:
            if stopped: self.record['cases'].append({'id':case,'status':'NOT_RUN','reason':'first nonreplication/mismatch'}); continue
            self.case_end=min(time.monotonic()+20,self.start+150); value='on' if case=='P2' else 'off'; label='beta' if case=='P3' else 'alpha'
            row={'id':case,'start':now(),'before':saved(previous),'status':'RUNNING'}; self.record['cases'].append(row)
            args=['source-file',str(self.root/'sync-off.conf')] if case=='P3' else ['set-option','-w','-t',self.children['alpha']['public']['window_id'],'synchronize-panes',value]
            self.mutate(case,args); self.scopes(case,'before',value); self.inject(case,label)
            # Finite settle before barriers; only final ordered sentinels establish absence.
            time.sleep(min(.2,self.remaining())); after=self.pair(); self.scopes(case,'after',value)
            got=[k for k in KINDS if case in [r['phase'] for r in after[k]]]; wanted=['alpha','beta'] if case=='P2' else [label]
            row.update(after=saved(after),tentative_recipients=got,end=now(),status='OBSERVED'); previous=saved(after)
            if got!=wanted: stopped=True; row['provisional_scope_difference']=True
        recipients=self.seal()
        mismatch=None
        for row in self.record['cases']:
            if row['status']=='NOT_RUN': continue
            case=row['id']; got=recipients[case]; wanted=['alpha','beta'] if case=='P2' else ['beta'] if case=='P3' else ['alpha']
            if self.phase=='candidate': wanted=self.oracle['recipients'][case]
            row.update(sealed_recipients=got,status='PASS' if got==wanted else 'ORACLE_PATH_UNEXERCISED' if self.phase=='oracle' and case=='P2' and got==['alpha'] else 'RUNTIME_MISMATCH' if self.phase=='candidate' else 'ORACLE_PRECONDITION_FAILED')
            if got!=wanted and mismatch is None: mismatch={'at':now(),'case':case,'expected_recipients':wanted,'actual_recipients':got,'closed_by_sentinels':True}
        if mismatch:
            self.record['mismatch']=mismatch
            if self.phase=='oracle' and mismatch['case']=='P2' and mismatch['actual_recipients']==['alpha']: raise OracleUnexercised('tmux send-keys did not replicate; no candidate admitted')
            if self.phase=='candidate': raise RuntimeMismatch('recipient set differs from closed Darwin oracle')
            raise Unproven('oracle finite precondition differs')
        require(all(row['status']=='PASS' for row in self.record['cases']),'all three cases exercised before oracle gate')
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
        require(present+len(encoded(pack(self.record)))+128*1024<LOT_CAP,'whole lot reserve before CLI')
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

def physical_files(root):
    seen=set(); total=0
    for path in Path(root).rglob('*'):
        if not path.is_file(): continue
        st=path.stat(); key=(st.st_dev,st.st_ino)
        if key not in seen: seen.add(key); total+=st.st_size
    return total

def verify_frozen(frozen):
    config=frozen['config']; require(frozen['phase'] in ('oracle','candidate'),'phase')
    for p in frozen['artifacts']: require(pin(p['path'])==p,'frozen pin '+p['path'])
    required={str(Path(__file__).resolve()),config['config_path'],config['adapter_pin']['path'],frozen['pure']['path'],*[p['path'] for p in config['pins'].values()],*[p['path'] for p in config['api_pins']]}
    required.update(str(Path(root)/name) for root in config['roots'].values() for name in ('startup.conf','sync-off.conf'))
    require(required<={p['path'] for p in frozen['artifacts']},'mandatory provenance'); equal(config,json.loads(Path(config['config_path']).read_text()),'config'); equal(frozen['pins'],config['pins'],'pins')
    equal(frozen['root'],config['roots'][frozen['phase']],'root'); equal(frozen['plans'],{k:spawn_plan(config,frozen['phase'],k) for k in KINDS},'plans'); equal(frozen['environment'],environment(config,frozen['phase']),'environment')
    pure=json.loads(Path(frozen['pure']['path']).read_text()); require(pure['status']=='PASS' and pure['driver_sha256']==digest(__file__),'pure gate')
    require(not (Path(frozen['root'])/'STARTED.json').exists(),'one native lot only')
    if frozen['phase']=='candidate':
        p=frozen['oracle_gate']; require(pin(p['path'])==p,'oracle gate pin'); gate=json.loads(Path(p['path']).read_text())
        require(gate['status']=='PASS' and pin(gate['result']['path'])==gate['result'] and gate['cleanup_pass'],'closed complete oracle')
        equal(gate['driver_sha256'],digest(__file__),'same native source'); equal(frozen['oracle_projection'],gate['projection'],'exact measured Darwin expectations')
        equal((gate['challenges'],gate['helpers'],gate['daemon_generations']),(12,2,1),'complete native oracle')
        equal(gate['projection']['recipients'],{'P1':['alpha'],'P2':['alpha','beta'],'P3':['beta']},'send-keys path actually exercised')

def execute(frozen_path):
    frozen=json.loads(frozen_path.read_text()); verify_frozen(frozen); bind_api(frozen['config']); run=Run(frozen)
    write_once(run.root/'STARTED.json',{'at':now(),'frozen':pin(frozen_path),'observer':D.birth(os.getpid())},FILE_CAP)
    try: run.execute(); run.record['status']='PASS'
    except OracleUnexercised as error: run.record.update(status='ORACLE_PATH_UNEXERCISED',error=str(error),candidate_admitted=False)
    except RuntimeMismatch as error: run.record.update(status='RUNTIME_MISMATCH',error=str(error),owner='L2 runtime')
    except BaseException as error: run.record.update(status='FAIL' if isinstance(error,Failed) else 'UNPROVEN',error=repr(error),traceback=traceback.format_exc())
    finally:
        present={r['id'] for r in run.record['cases']}
        for case in CASES:
            if case not in present: run.record['cases'].append({'id':case,'status':'NOT_RUN','reason':'first failure'})
        run.record['proof_frozen_at']=now()
        try: run.cleanup()
        except BaseException as error: run.record.update(cleanup_exception=repr(error),status='UNPROVEN')
        run.record.update(end=now(),seconds=time.monotonic()-run.start,frozen=pin(frozen_path),distinct_barrier_nonces=len(run.nonces),
          limits=['Private complete-line receipts, not PTY echo.','Absence only through ordered sentinel and fresh barrier cut.','NOTE_EXIT/absence/ECHILD not numeric indirect exit or author reap.','Disk SHA not mapped image attestation.','Finite known-parent observations not exhaustive continuity.'])
        document=pack(run.record); equal(unpack(document),run.record,'lossless strict codec'); raw=encoded(document); logical=encoded(run.record)
        # Charge the prospective physical RESULT, packed buffer, logical buffer,
        # and all existing distinct inodes before writing any final result.
        charge={'at':now(),'physical_existing_bytes':physical_files(run.root),'serialized_result_bytes':len(raw),'unpersisted_packed_buffer_bytes':len(raw),
                'unpersisted_logical_buffer_bytes':len(logical),'closing_metadata_reserve_bytes':32*1024,'cap_bytes':LOT_CAP}
        charge['charged_bytes']=sum(charge[k] for k in ('physical_existing_bytes','serialized_result_bytes','unpersisted_packed_buffer_bytes','unpersisted_logical_buffer_bytes','closing_metadata_reserve_bytes'))
        require(charge['charged_bytes']<=LOT_CAP,'physical plus buffered lot budget before RESULT publication')
        write_once(run.root/'BUDGET-BEFORE-RESULT.json',charge); write_once(run.root/'RESULT.json',document)
    if run.record['status']=='PASS' and run.phase=='oracle':
        write_once(run.root/'ORACLE-FROZEN.json',{'at':now(),'status':'PASS','cleanup_pass':run.record['cleanup']['pass'],'result':pin(run.root/'RESULT.json'),'driver_sha256':digest(__file__),
                  'projection':projection(run.record),'challenges':len(run.record['observations']),'helpers':run.spawn_count,'daemon_generations':1})
    print(json.dumps({'status':run.record['status'],'result':pin(run.root/'RESULT.json'),'seconds':run.record['seconds']}),flush=True)
    return 0 if run.record['status']=='PASS' else 3 if run.record['status']=='ORACLE_PATH_UNEXERCISED' else 2 if run.record['status']=='RUNTIME_MISMATCH' else 1

if __name__=='__main__':
    if len(sys.argv)==6 and sys.argv[1]=='witness': witness(*sys.argv[2:])
    elif len(sys.argv)==3 and sys.argv[1]=='--frozen': raise SystemExit(execute(Path(sys.argv[2])))
    elif len(sys.argv)==3 and sys.argv[1]=='--decode': print(json.dumps(unpack(json.loads(Path(sys.argv[2]).read_text())),indent=2))
    else: raise SystemExit('Use --frozen FILE.json or --decode RESULT.json')
