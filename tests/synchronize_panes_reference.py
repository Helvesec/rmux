#!/usr/bin/env python3
"""Narrow Linux synchronize-panes/send-keys observer. Accepted020 API readonly.
No pending028 code imported. Mac must not import this driver. Single-use phases.
"""
import base64,copy,ctypes,hashlib,importlib.util,json,os,re,secrets,select,signal,socket,stat,subprocess,sys,tempfile,termios,time,traceback
from pathlib import Path
sys.dont_write_bytecode=True  # Keep the accepted020 API directory read-only in pane helpers.
API=Path(__file__).resolve().with_name('base_index_reference.py')
assert hashlib.sha256(API.read_bytes()).hexdigest()=='1e90f85bfae33b7a9645623ed80475cd1a484880dd341c92afe73af7f5f8c5f6'
spec=importlib.util.spec_from_file_location('accepted020',API);api=importlib.util.module_from_spec(spec);spec.loader.exec_module(api)
now,require,equal,identity,proc,same,digest=api.now,api.require,api.equal,api.identity,api.proc,api.same,api.digest
write_once,atomic_once=api.write_once,api.atomic_once
CAP=8*1024**2;PRODUCTION_RESERVE=1048576;CLOSURE_RESERVE=262144;PUBLICATION_RESERVE=262144
STAGES=('P1','P2','P3');ENV_KEYS=api.ENV_KEYS
class ScopeInoperative(Exception):pass
class BudgetFailure(api.Unproven):pass
def encoded(v):return (json.dumps(v,indent=2)+'\n').encode()
def physical(root):
 seen=set();size=0
 for p in Path(root).rglob('*'):
  st=p.lstat()
  if stat.S_ISREG(st.st_mode) and (st.st_dev,st.st_ino) not in seen:seen.add((st.st_dev,st.st_ino));size+=st.st_size
 return size,len(seen)
def settled_publication(record,size,count,future,reserve):
 # Freeze metadata and its own serialized size before admitting any write.
 require(all(type(x) is int and x>=0 for x in (size,count,future,reserve)),'nonnegative byte accounting')
 result=copy.deepcopy(record)
 last={'point':'final_publication_with_summary','physical_bytes':size,'physical_files':count,
       'pending_bytes':0,'future_bytes':future,'reserve_bytes':reserve,'total':size+future+reserve,'cap':CAP}
 result['logging']={'last':last,'errors':[],'status':'PASS'}
 for _ in range(16):
  payload=encoded(result);pending=len(payload);total=size+pending+future+reserve
  if last['pending_bytes']==pending and last['total']==total:return result,payload
  last.update(pending_bytes=pending,total=total)
 raise BudgetFailure('final metadata size did not settle')

def check_deadline(end):require(time.monotonic()<end,'bounded wait timeout')
def parse_token(raw):
 require(isinstance(raw,bytes) and len(raw)<=128,'token byte cap')
 m=re.fullmatch(rb'S029_(P1|P2|P3|END_A|END_B)_([0-9a-f]{48})\n',raw);require(m is not None,'token grammar')
 return {'stage':m[1].decode(),'nonce':m[2].decode(),'token':raw[:-1].decode()}
def validate_ledger(rows,own,label,phase,run_token,allowed):
 require(len(rows)<=6,'receipt cap');seen=set()
 for i,r in enumerate(rows,1):
  equal(r['counter'],i,'monotonic receipt sequence');require(same(r['identity'],own),'receipt birth')
  equal(r['label'],label,'receipt helper');equal(r['phase'],phase,'receipt phase');equal(r['run_token'],run_token,'receipt instance')
  parsed=parse_token(base64.b64decode(r['line_base64']));equal(parsed,{k:r[k] for k in ('stage','nonce','token')},'receipt bytes')
  equal(allowed[r['stage']],r['token'],'planned exact token');require(r['nonce'] not in seen,'duplicate token');seen.add(r['nonce'])
 return [r['stage'] for r in rows]
def witness_argv(config,phase,label):return [config['python'],'-I',str(Path(__file__).resolve()),'witness',label,str(Path(config['root'])/phase/'markers'),config['tokens'][phase],phase]
def witness(label,root,run_token,phase):
 root=Path(root);own=identity(os.getpid());stem=f"{own['pid']}-{own['start_ticks']}";rows=[];buffer=b'';loops=0
 flags=termios.tcgetattr(0);require(flags[3]&termios.ICANON and flags[0]&termios.ICRNL,'canonical line input required');flags[3]&=~termios.ECHO;termios.tcsetattr(0,termios.TCSANOW,flags);os.set_blocking(0,False)
 def report(challenge):return {'at':now(),'kind':label,'token':run_token,'challenge':challenge,'exit_code':0,'identity':identity(os.getpid()),'cwd':os.getcwd(),'argv':sys.argv,'environment':{k:os.environ.get(k) for k in ENV_KEYS},'phase':phase,'loops':loops,'received':copy.deepcopy(rows),'input_empty':not buffer,'termios_iflag':flags[0],'termios_lflag':flags[3]}
 def drain():
  nonlocal buffer
  while True:
   try:chunk=os.read(0,1024)
   except BlockingIOError:break
   if not chunk:raise RuntimeError('stdin closed')
   buffer+=chunk;require(len(buffer)<=1024,'stdin cap')
   while b'\n' in buffer:
    line,buffer=buffer.split(b'\n',1);parsed=parse_token(line+b'\n');require(len(rows)<6,'six receipt maximum')
    r={**parsed,'at':now(),'counter':len(rows)+1,'label':label,'phase':phase,'run_token':run_token,'identity':identity(os.getpid()),'line_base64':base64.b64encode(line+b'\n').decode()}
    require(all(x['nonce']!=r['nonce'] for x in rows),'duplicate received nonce');rows.append(r);atomic_once(root/f'receipt-{stem}-{len(rows)}.json',r)
 atomic_once(root/('born-'+stem+'.json'),report(None));end=time.monotonic()+180
 while time.monotonic()<end:
  loops+=1;drain()
  for path in sorted(root.glob('request-'+stem+'-*.json')):
   ack=root/path.name.replace('request-','ack-',1)
   if not ack.exists():
    request=json.loads(path.read_text());require(same(request['identity'],own),'request birth');drain();atomic_once(ack,report(request['challenge']))
  time.sleep(.01)
 os._exit(124)

class Run(api.Run):
 def __init__(self,config,phase,start,expected):
  self.config,self.phase,self.start=config,phase,start;self.root=Path(config['root'])/phase;self.binary,self.socket=config[phase],self.root/'s';self.env=api.environment(config,phase);self.case_end=start+150;self.cleaning=False
  self.daemon=self.socket_identity=None;self.owned={};self.children={};self.handles={};self.spawn_count=0;self.expected_effects=expected;self.log_errors=[];self.log_last=None;self.sealed=False;self.received={};self.injected=[]
  self.record={'phase':phase,'start':now(),'environment':self.env,'commands':[],'profiles':[],'public_samples':[],'observations':[],'cases':[],'options':[],'inventories':[],'injections':[],'source_inputs':[]}
 def account(self,point,admit=True,reserve=None,future=0):
  try:
   size,count=physical(Path(self.config['root']).parent);pending=len(encoded(self.record));res=reserve if reserve is not None else (PUBLICATION_RESERVE if self.cleaning else PRODUCTION_RESERVE)
   self.log_last={'point':point,'physical_bytes':size,'physical_files':count,'pending_bytes':pending,'future_bytes':future,'reserve_bytes':res,'total':size+pending+future+res,'cap':CAP}
   if self.log_last['total']>CAP:raise BudgetFailure('physical plus pending/reserve cap')
  except Exception as e:
   if len(self.log_errors)<8:self.log_errors.append({'at':now(),'point':point,'error':repr(e)})
  if admit and self.log_errors:raise BudgetFailure('logging failed: '+repr(self.log_errors))
  return not self.log_errors
 def publish_result(self,path,future=0):
  # A previous failure cannot be erased by a later smaller document.
  if self.log_errors:raise BudgetFailure('sticky accounting failure blocks publication')
  try:
   size,count=physical(Path(self.config['root']).parent)
   result,payload=settled_publication(self.record,size,count,future,PUBLICATION_RESERVE)
   self.log_last=result['logging']['last']
   if self.log_last['total']>CAP:raise BudgetFailure('final physical plus serialized pending/reserve cap')
   # Write the exact bytes admitted above; no status/label mutation follows.
   with Path(path).open('xb') as stream:
    stream.write(payload);stream.flush();os.fsync(stream.fileno())
  except Exception as error:
   if len(self.log_errors)<8:self.log_errors.append({'at':now(),'point':'final_publication_with_summary','error':repr(error)})
   raise BudgetFailure('final publication rejected: '+repr(error)) from error
  self.record=result
  return result

 def hold(self,label,old):
  if label in self.handles:require(same(old,self.handles[label][2]['original_identity']),'retained identity changed');return
  fd=os.pidfd_open(old['pid']);poll=select.poll();poll.register(fd,select.POLLIN);meta={'fd':fd,'opened_at':now(),'original_identity':copy.deepcopy(old),'identity_after_open':identity(old['pid']),'events_before_request':poll.poll(0)}
  self.handles[label]=(fd,poll,meta);require(same(old,meta['identity_after_open']) and not meta['events_before_request'],'pidfd qualification');self.owned[old['pid']]=old
 def command(self,args,startup=False,required=True):
  self.remaining();self.account('before_command',admit=not self.cleaning)
  if self.cleaning:require(args==['kill-server'],'closure command scope')
  argv=[self.binary,*([] if startup else ['-N']),'-S',str(self.socket),*(['-f',str(self.root/'startup.conf')] if startup else []),*args];begin=now()
  p=subprocess.Popen(argv,cwd=self.root,env=self.env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True);old=identity(p.pid);fd=os.pidfd_open(p.pid);after=identity(p.pid);require(same(old,after),'client pidfd birth');timeout=False
  try:
   try:out,err=p.communicate(timeout=self.remaining())
   except subprocess.TimeoutExpired:
    timeout=True;signal.pidfd_send_signal(fd,signal.SIGKILL,None,0);out,err=p.communicate(timeout=2)
  finally:os.close(fd)
  item={'argv':argv,'start':begin,'end':now(),'identity':old,'retained_client_pidfd':{'after_open':after,'closed':True},'exit_code':p.returncode,'stdout':out,'stderr':err,'timeout':timeout};self.record['commands'].append(item)
  if len(out.encode())+len(err.encode())>65536:
   item.update(stdout=out[:4096],stderr=err[:4096],output_bytes=len(out.encode())+len(err.encode()),output_sha256=hashlib.sha256((out+err).encode()).hexdigest());self.log_errors.append({'point':'command_output','error':'postcommunicate cap'})
  self.account('after_command',admit=not self.cleaning)
  require(not timeout,'command timeout')
  if required:require(p.returncode==0,'command failed '+repr(args)+': '+err)
  return out.strip()
 def public(self,target):
  pub,daemon=super().public(target);self.hold('daemon',daemon['before']);return pub,daemon
 def expected(self,label):return {'kind':label,'exit_code':0,'token':self.config['tokens'][self.phase],'exe':str(Path(self.config['python']).resolve()),'exe_sha256':self.config['hashes']['python'],'argv':witness_argv(self.config,self.phase,label),'cwd':str(self.root/'cwd'),'environment':{k:self.env.get(k) for k in ENV_KEYS}}
 def observe(self,label,target,reobserve=False):
  require(len(self.record['observations'])<12,'challenge cap');item=super().observe(label,target,reobserve);self.hold(label,item['child']['before']);ack=item['ack']
  equal(ack['phase'],self.phase,'helper phase');require(ack['loops']>item['born']['loops'] and ack['input_empty'],'helper progression and drained complete lines')
  allowed=self.config['lines'][self.phase];rows=ack['received'];validate_ledger(rows,item['child']['before'],label,self.phase,self.config['tokens'][self.phase],allowed)
  old=self.received.get(label,[]);equal(rows[:len(old)],old,'immutable receipt prefix')
  stem=f"{item['child']['before']['pid']}-{item['child']['before']['start_ticks']}"
  for receipt in rows:equal(json.loads((self.root/'markers'/f"receipt-{stem}-{receipt['counter']}.json").read_text()),receipt,'receipt file equals snapshot')
  self.received[label]=copy.deepcopy(rows);return item
 def barrier(self):
  for label in ('A','B'):self.observe(label,self.children[label]['public']['pane_id'],True)
  self.inventory()
 def inventory(self):
  raw=self.command(['list-panes','-a','-F',api.FORMAT]);rows=[api.parse_public(x) for x in raw.splitlines()];equal(len(rows),2,'exact two panes');equal(len({x['pane_id'] for x in rows}),2,'distinct panes');equal(len({x['window_id'] for x in rows}),1,'one window');equal(len({x['session_id'] for x in rows}),1,'one session')
  for label,c in self.children.items():equal(next(x for x in rows if x['pane_id']==c['public']['pane_id']),c['public'],'stable pane identity')
  geometry=self.command(['display-message','-p','-t',self.children['A']['public']['window_id'],'#{window_width}|#{window_height}|#{window_panes}|#{session_attached}']);equal(geometry,'80|24|2|0','window geometry and no attach')
  item={'at':now(),'raw':raw,'rows':rows,'geometry':geometry,'held_events':{k:p.poll(0) for k,(_,p,_) in self.handles.items()}};require(not any(item['held_events'].values()),'owned identity exited during cases');self.record['inventories'].append(item)
 def options(self,stage,value):
  w=self.children['A']['public']['window_id'];record={'at':now(),'stage':stage,'windows':{},'command_indices':[]}
  for label in ('A','B'):
   target=self.children[label]['public']['pane_id'];vals={}
   for key,flags,t in [('window','-Awv',w),('pane','-Apv',target),('pane_local','-pqv',target)]:
    record['command_indices'].append(len(self.record['commands']));vals[key]=self.command(['show-options',flags,'-t',t,'synchronize-panes'])
   equal(vals,{'window':value,'pane':value,'pane_local':''},'window scope and pane inheritance');record['windows'][label]=vals
  self.record['options'].append(record);return record
 def inject(self,stage,label):
  require(len(self.injected)<6 and stage not in self.injected,'injection cap/unique');target=self.children[label]['public']['pane_id'];token=self.config['lines'][self.phase][stage];entry={'stage':stage,'label':label,'target':target,'token':token,'at':now(),'command_index':len(self.record['commands'])};self.record['injections'].append(entry);self.injected.append(stage)
  self.command(['send-keys','-t',target,token,'Enter']);entry['end']=now()
  old=self.children[label]['child']['before'];stem=f"{old['pid']}-{old['start_ticks']}"
  while True:
   check_deadline(self.case_end)
   found=[]
   for p in sorted((self.root/'markers').glob('receipt-'+stem+'-*.json')):
    require(p.stat().st_size<8192,'receipt size');r=json.loads(p.read_text())
    if r['token']==token:found.append(r)
   if found:
    equal(len(found),1,'one target receipt');require(same(found[0]['identity'],old),'injected token ownership');break
   time.sleep(.01)
  time.sleep(.15);self.remaining();return entry
 def effect(self,stage):return {label:sum(r['stage']==stage for r in self.received[label]) for label in ('A','B')}
 def execute(self):
  require(not self.socket.exists() and not list((self.root/'markers').iterdir()),'fresh private fixture');self.profiles();self.case_end=min(self.start+150,time.monotonic()+20)
  cwd=str(self.root/'cwd');self.command(['new-session','-d','-s','sync','-n','sync','-x','80','-y','24','-c',cwd,*witness_argv(self.config,self.phase,'A')],startup=True);self.spawn_count+=1
  self.command(['split-window','-d','-h','-t','sync:0.0','-c',cwd,*witness_argv(self.config,self.phase,'B')]);self.spawn_count+=1
  self.observe('A','sync:0.0');self.observe('B','sync:0.1');self.inventory();wid=self.children['A']['public']['window_id']
  source=self.root/'sync-off.conf';text='set-option -w -t '+wid+' synchronize-panes off\n';equal((self.root/'sync-off.conf.in').read_text().replace('OWN_WINDOW_ID',wid),text,'frozen source template');source.write_text(text);pin={'path':str(source),'bytes':source.stat().st_size,'sha256':digest(source),'text':text,'window_id':wid};self.record['source_inputs']=[pin];write_once(self.root/'SOURCE-INPUTS-FROZEN.json',{'at':now(),'sources':[pin]})
  scope_pending=False
  for stage in STAGES:
   self.case_end=min(self.start+150,time.monotonic()+20);entry={'id':stage,'start':now(),'monotonic_start':time.monotonic()};self.record['cases'].append(entry)
   try:
    value='on' if stage=='P2' else 'off'
    if stage=='P3':equal(digest(source),pin['sha256'],'frozen source-file');self.command(['source-file',str(source)])
    else:self.command(['set-option','-w','-t',wid,'synchronize-panes',value])
    entry['options']=self.options(stage,value);self.inject(stage,'B' if stage=='P3' else 'A');self.barrier();entry['received_counts']=self.effect(stage)
    if self.phase=='candidate':equal(entry['received_counts'],self.expected_effects[stage],'oracle reception counts')
    elif stage=='P1':equal(entry['received_counts'],{'A':1,'B':0},'OFF isolates paneA')
    elif stage=='P2':
     require(entry['received_counts'] in ({'A':1,'B':0},{'A':1,'B':1}),'unexpected ON counts');scope_pending=entry['received_counts']=={'A':1,'B':0}
    elif stage=='P3':equal(entry['received_counts'],{'A':0,'B':1},'OFF source isolates paneB')
    entry['status']='OBSERVED'
   except Exception as e:entry.update(status='FAIL',error=repr(e));raise
   finally:entry.update(end=now(),elapsed_seconds=time.monotonic()-entry['monotonic_start'])
   if scope_pending:break
  # Two ordered end sentinels, one to each owned pane, then fresh FS barriers.
  # These are the final input mutations and attest processing beyond case tokens.
  self.case_end=min(self.start+150,time.monotonic()+20)
  self.inject('END_A','A');self.inject('END_B','B');self.barrier()
  for label,stage in [('A','END_A'),('B','END_B')]:
   require(any(r['stage']==stage for r in self.received[label]),'end sentinel missing')
  for case in self.record['cases']:
   equal(self.effect(case['id']),case['received_counts'],'receipt counts stable past both sentinels');case['status']='OBSERVED_SCOPE_INOPERATIVE' if scope_pending and case['id']=='P2' else 'PASS'
  self.record.update(final_received=copy.deepcopy(self.received),sentinel_barrier_complete=True,spawn_commands=self.spawn_count)
  self.profiles()
  if scope_pending:raise ScopeInoperative('synchronize-panes ON did not replicate targeted send-keys to paneB, certified past both end sentinels')
 def seal(self,status,error):
  self.record.update(status=status,error=error,proof_sealed_at=now(),not_run=[s for s in STAGES if s not in [x['id'] for x in self.record['cases']]],held_pidfds={k:copy.deepcopy(v[2]) for k,v in self.handles.items()});self.account('seal',False,reserve=CLOSURE_RESERVE+PUBLICATION_RESERVE)
  observed=self.root/'OBSERVED.json';document=self.record
  if self.log_errors:document={'status':'LOG_BUDGET_FAIL','primary_status':status,'error':error,'omitted_full_record':True,'unwritten_sha256':hashlib.sha256(encoded(self.record)).hexdigest(),'log_errors':self.log_errors}
  write_once(observed,document);self.sealed=True;self.record={'phase':self.phase,'commands':[],'profiles':[],'exits':[],'cleanup':{},'log_errors':self.log_errors};return {'path':str(observed),'bytes':observed.stat().st_size,'sha256':digest(observed)}
 def cleanup(self):
  begin=time.monotonic();self.cleaning=True;self.case_end=min(self.start+180,begin+30);result={'start':now(),'actions':[],'reaped':[],'errors':[]};self.record['cleanup']=result
  self.account('closure_start',False)
  try:
   if self.daemon and api.snapshot_same(self.daemon,proc(self.daemon['before']['pid'])) and self.socket_info()==self.socket_identity:self.command(['kill-server'])
  except Exception as e:result['errors'].append({'operation':'private kill-server','error':repr(e)})
  for sig in (None,signal.SIGTERM,signal.SIGKILL):
   limit=min(self.case_end,time.monotonic()+(2 if sig is None else 1))
   for label,(fd,poll,meta) in self.handles.items():
    if sig and not poll.poll(0):
     old=meta['original_identity'];current=identity(old['pid'])
     if same(old,current) and current['state']!='Z':signal.pidfd_send_signal(fd,sig,None,0);result['actions'].append({'label':label,'fd':fd,'signal':int(sig),'at':now()})
   while time.monotonic()<limit and any(not poll.poll(0) for _,poll,_ in self.handles.values()):time.sleep(.01)
  for label,(fd,poll,meta) in self.handles.items():
   e={'label':label,'cause':'cleanup','pidfd':copy.deepcopy(meta),'at':now()};self.record['exits'].append(e)
   try:
    e['pidfd'].update(events_after_request=poll.poll(0),observed_at=now());e['after']=identity(meta['original_identity']['pid']);api.check_kernel_exit(meta['original_identity'],e['after'],e['pidfd']);e['status']='PASS'
   except Exception as error:e.update(status='UNPROVEN',error=repr(error));result['errors'].append({'operation':'retained exit '+label,'error':repr(error)})
  if any(e['label']=='daemon' and e['status']=='PASS' for e in self.record['exits']):
   ep={'at':now(),'pathname':self.socket_info()}
   with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as probe:
    probe.settimeout(.2)
    try:probe.connect(str(self.socket));ep.update(accepting=True,errno=None)
    except OSError as e:ep.update(accepting=False,errno=e.errno,error=repr(e))
   result['endpoint_probe']=ep
  for _ in range(50):
   try:
    while True:
     pid,status=os.waitpid(-1,os.WNOHANG)
     if not pid:break
     result['reaped'].append({'pid':pid,'status':status})
   except ChildProcessError:pass
   if all(not same(old,identity(pid)) for pid,old in self.owned.items()):break
   time.sleep(.02)
  result['identities_after']=[{'before':old,'after':identity(pid)} for pid,old in self.owned.items()];result['all_tracked_exited']=all(not same(x['before'],x['after']) for x in result['identities_after'])
  children=[]
  for task in Path('/proc/self/task').iterdir():
   try:children.extend(int(x) for x in (task/'children').read_text().split())
   except FileNotFoundError:pass
  result['owned_descendants_remaining']=sorted(set(children));endpoint=self.socket_info()
  if endpoint==self.socket_identity and endpoint and result['all_tracked_exited'] and not children:self.socket.unlink();result['stale_owned_socket_removed']=endpoint
  result['socket_after']=self.socket_info();result['recovery_owners']=[x['before'] for x in result['identities_after'] if same(x['before'],x['after'])]+[identity(pid) for pid in children if pid not in self.owned]
  if result['recovery_owners']:write_once(Path(self.config['root'])/'RECOVERY-OWNER.json',{'at':now(),'owners':result['recovery_owners'],'new_admission':False})
  for label,(fd,_,meta) in self.handles.items():
   os.close(fd);meta['closed_at']=now()
   for e in self.record['exits']:
    if e['label']==label:e['pidfd']['closed_at']=meta['closed_at']
  self.handles.clear();result.update(end=now(),elapsed_seconds=time.monotonic()-begin);self.profiles();self.account('closure_end',False)
  api.check_cleanup(result,self.owned);require(len(self.record['exits'])==3 and all(e['status']=='PASS' for e in self.record['exits']),'all three retained exits');require(result['endpoint_probe']['errno'] in (2,111) and not result['endpoint_probe']['accepting'],'shutdown endpoint');require(not result['errors'],'closure error');require(result['elapsed_seconds']<30,'closure budget');result['status']='PASS'

def publication_checks(root):
 # This function can be AST-selected with only the pure accounting methods.
 # Its oracle reads actual file bytes and uses JSONEncoder independently of
 # physical()/encoded(); no product, process inspection or native API is used.
 root=Path(root)/'accounting-pure';root.mkdir();checks=[];details={}
 def check(name,condition):
  assert condition,name
  checks.append(name)
 def census(folder):
  seen=set();total=0
  for directory,_,names in os.walk(folder,followlinks=False):
   for name in names:
    path=Path(directory)/name;info=path.lstat();key=(info.st_dev,info.st_ino)
    if stat.S_ISREG(info.st_mode) and key not in seen:
     seen.add(key);total+=len(path.read_bytes())
  return total,len(seen)
 def wire(document):
  return ''.join(json.JSONEncoder(indent=2).iterencode(document)).encode('utf-8')+b'\n'
 def total(folder,document):
  last=document['logging']['last']
  return census(folder)[0]+len(wire(document))+last['future_bytes']+last['reserve_bytes']
 def coherent(folder,document):
  last=document['logging']['last'];size,count=census(folder)
  return (last['physical_bytes'],last['physical_files'],last['pending_bytes'],last['total'])==(size,count,len(wire(document)),total(folder,document))
 seed={'status':'PASS','phase':'pure','pending_buffers':['p'*2048,{'unicode':'é\n😀','unpersisted':True}],'logging_errors':[]}
 def fixture(name):
  folder=root/name;folder.mkdir();(folder/'nested').mkdir()
  (folder/'a').write_bytes(b'abc');(folder/'b').write_bytes(b'abc');os.link(folder/'a',folder/'alias')
  (folder/'nested/.hidden').write_bytes(b'\x00z');(folder/'empty').write_bytes(b'')
  os.symlink('a',folder/'symlink')
  return folder
 def run_for(folder):
  run=Run.__new__(Run);run.config={'root':str(folder/'attempt')};run.record=copy.deepcopy(seed)
  run.log_errors=[];run.log_last=None;run.cleaning=True
  return run
 def reject(name,run,path,**kwargs):
  before=census(path.parent)
  try:run.publish_result(path,**kwargs)
  except BudgetFailure:pass
  else:raise AssertionError('publication accepted '+name)
  check(name,not path.exists() and census(path.parent)==before and bool(run.log_errors))
 base=fixture('physical')
 check('physical_copies_inodes_hidden_empty',census(base)==(8,4) and physical(base)==(8,4))
 check('copies_are_distinct_inodes',(base/'a').stat().st_ino!=(base/'b').stat().st_ino)
 check('hardlink_counted_once',(base/'a').stat().st_ino==(base/'alias').stat().st_ino)
 check('authorized_scan_trailing_slash',len(list(os.scandir(str(base)+os.sep)))==6)
 for status in ('PASS','ORACLE_SCOPE_INOPERATIVE','RUNTIME_MISMATCH','UNPROVEN'):
  record={**seed,'status':status};frozen,payload=settled_publication(record,*census(base),65536,PUBLICATION_RESERVE)
  check('settled_'+status,coherent(base,frozen) and payload==wire(frozen) and frozen['status']==status)
  record['pending_buffers'][1]['unpersisted']=False
  check('frozen_copy_'+status,frozen['pending_buffers'][1]['unpersisted'] is True)
  seed['pending_buffers'][1]['unpersisted']=True
 intermediate=run_for(base);intermediate.account('closure',reserve=CLOSURE_RESERVE+PUBLICATION_RESERVE,future=65536)
 check('pending_and_both_fixed_reserves',intermediate.log_last['total']==census(base)[0]+len(wire(intermediate.record))+65536+CLOSURE_RESERVE+PUBLICATION_RESERVE)
 exact=fixture('exact');future=CAP-PUBLICATION_RESERVE-4096
 # Locate the exact frontier from independently encoded bytes, not the
 # producer's claimed pending_bytes/total. The reservation has no disk cost.
 for _ in range(16):
  draft,_=settled_publication(seed,*census(exact),future,PUBLICATION_RESERVE)
  delta=CAP-total(exact,draft)
  if delta==0:break
  future+=delta
 else:raise AssertionError('independent frontier search')
 check('exact_frontier_includes_all_bytes',coherent(exact,draft) and total(exact,draft)==CAP)
 run=run_for(exact);published=run.publish_result(exact/'RESULT.json',future=future)
 raw=(exact/'RESULT.json').read_bytes()
 check('exact_cap_admitted_exact_serialization',raw==wire(published) and len(raw)==published['logging']['last']['pending_bytes'])
 check('persisted_bytes_replace_pending_once',census(exact)[0]+future+PUBLICATION_RESERVE==CAP)
 details['exact']={'cap':CAP,'physical_before':8,'physical_files_before':4,'pending_bytes':len(raw),'future_bytes':future,'reserve_bytes':PUBLICATION_RESERVE,'total':CAP,'published_sha256':hashlib.sha256(raw).hexdigest()}
 above=fixture('above');run=run_for(above)
 reject('one_byte_above_rejected_without_write',run,above/'RESULT.json',future=future+1)
 check('above_total_is_cap_plus_one',run.log_last['total']==CAP+1)
 reject('sticky_after_smaller_pending',run,above/'RESULT.json',future=0)
 try:run.account('later-small')
 except BudgetFailure:checks.append('sticky_blocks_later_account')
 else:raise AssertionError('sticky account admitted')
 # A post-admission label mutation reproduces the documented13-byte hazard.
 changed=copy.deepcopy(published);changed['logging']['last']['point']+='x'*13
 check('canary_label_growth_detected',len(wire(changed))==len(raw)+13 and not coherent(base,changed) and total(base,changed)==CAP+13)
 changed=copy.deepcopy(published);changed['logging']['last']['physical_bytes']-=3;changed['logging']['last']['total']-=3
 check('canary_missing_physical_copy_detected',not coherent(base,changed))
 changed=copy.deepcopy(published);changed['pending_buffers'].append('new buffer')
 check('canary_unmeasured_buffer_detected',not coherent(base,changed))
 legacy_root=fixture('legacy');legacy_future=CAP-PUBLICATION_RESERVE-4096
 for _ in range(16):
  legacy=run_for(legacy_root);document=legacy.record
  legacy.account('final_publication',False,reserve=PUBLICATION_RESERVE,future=legacy_future)
  document['logging']={'last':legacy.log_last,'errors':legacy.log_errors,'status':'PASS'}
  legacy.account('final_publication_with_summary',False,reserve=PUBLICATION_RESERVE,future=legacy_future)
  document['logging']['last']=legacy.log_last
  delta=CAP-legacy.log_last['total']
  if delta==0:break
  legacy_future+=delta
 else:raise AssertionError('legacy threshold search')
 check('legacy_admits_cap_but_actual_exceeds_13',not legacy.log_errors and legacy.log_last['total']==CAP and total(legacy_root,document)==CAP+13)
 check('legacy_counterexample_never_written',not (legacy_root/'RESULT.json').exists())
 details['legacy']={'measured_total':legacy.log_last['total'],'independent_total':total(legacy_root,document),'pending_claim':legacy.log_last['pending_bytes'],'actual_pending':len(wire(document)),'publication_attempted':False}
 invalid=fixture('invalid');reject('negative_future_rejected',run_for(invalid),invalid/'RESULT.json',future=-1)
 bad=run_for(invalid);bad.record['not_json']=object();reject('serialization_failure_rejected',bad,invalid/'RESULT.json')
 bad.record.pop('not_json');reject('serialization_failure_sticky',bad,invalid/'RESULT.json')
 original=globals()['physical']
 def unavailable(_):raise OSError('pure fixture census failure')
 bad=run_for(invalid)
 try:
  globals()['physical']=unavailable
  reject('census_failure_rejected',bad,invalid/'RESULT.json')
 finally:globals()['physical']=original
 reject('census_failure_sticky',bad,invalid/'RESULT.json')
 check('driver_cap_and_reserves_unchanged',CAP==8388608 and PUBLICATION_RESERVE==262144 and CLOSURE_RESERVE==262144)
 return {'status':'PASS','checks':checks,'count':len(checks),'details':details,'fixture':str(root),'oracle':'os.walk/lstat unique inodes and actual file reads; JSONEncoder.iterencode UTF8 bytes independent of production helpers','native_runs':0}

def pure_checks(config):
 checks=[]
 for phase in ('oracle','candidate'):
  for label in ('A','B'):
   argv=witness_argv(config,phase,label)[2:];require(len(argv)==6 and argv[1]=='witness' and len(argv[2:])==4,'helper dispatch arity')
 checks.append('helper_actual_argv_dispatch_arity')
 def ok(name,fn):fn();checks.append(name)
 def no(name,fn):
  try:fn()
  except (api.Unproven,api.Failed,KeyError,ValueError):checks.append(name)
  else:raise AssertionError('observer accepted invalid '+name)
 token='S029_P1_'+'a'*48;raw=(token+'\n').encode();ok('parser_exact',lambda:equal(parse_token(raw),{'stage':'P1','nonce':'a'*48,'token':token},'parser'))
 for i,bad in enumerate([raw[:-1],raw+b'\n',raw.replace(b'P1',b'P9'),raw.replace(b'a',b'g'),b'x'*129,raw.replace(b'S029',b'S028')]):no('parser_reject_'+str(i),lambda bad=bad:parse_token(bad))
 own={'pid':11,'start_ticks':'22','state':'S','ppid':10};receipt={'counter':1,'identity':own,'label':'A','phase':'oracle','run_token':'run',**parse_token(raw),'line_base64':base64.b64encode(raw).decode()};allowed={'P1':token}
 def validate(rows):return validate_ledger(rows,own,'A','oracle','run',allowed)
 ok('receipt_identity_token_counter',lambda:validate([receipt]))
 for key,value in [('counter',2),('identity',{**own,'start_ticks':'old'}),('label','B'),('phase','candidate'),('run_token','old'),('nonce','b'*48),('token','wrong'),('line_base64',base64.b64encode(raw.replace(b'P1',b'P2')).decode())]:no('receipt_wrong_'+key,lambda key=key,value=value:validate([{**receipt,key:value}]))
 no('duplicate_nonce',lambda:validate([receipt,{**receipt,'counter':2}]))
 no('wrong_order',lambda:validate([{**receipt,'counter':2},receipt]))
 no('receipt_population_cap',lambda:validate([receipt]*7))
 snap=copy.deepcopy([receipt]);receipt['identity']['state']='Z';ok('snapshot_deepcopy',lambda:equal(snap[0]['identity']['state'],'S','deepcopy'));receipt['identity']['state']='S'
 no('snapshot_prefix_mutation',lambda:equal([{**receipt,'counter':2}][:1],[receipt],'prefix'))
 no('bounded_timeout',lambda:check_deadline(time.monotonic()-1));ok('future_deadline',lambda:check_deadline(time.monotonic()+10))
 expected={'exe':'/python','exe_sha256':'hash','argv':['/python','-I','/driver','witness','A'],'cwd':'/cwd','kind':'A','token':'run','exit_code':0,'environment':{}}
 def fake(pid,parent):
  ident={'pid':pid,'start_ticks':str(pid),'ppid':parent,'state':'S'}
  return {**expected,'before':ident,'after':dict(ident),'coherent':True,'fd0':'/dev/pts/1','cmdline_base64':base64.b64encode(b'\0'.join(x.encode() for x in expected['argv'])+b'\0').decode()}
 child,daemon=fake(11,10),fake(10,1);pub={'pane_pid':11,'daemon_pid':10};marker={**expected,'argv':expected['argv'][2:],'identity':child['before'],'challenge':'nonce'}
 ok('accepted020_child',lambda:api.check_child(child,copy.deepcopy(child),pub,daemon,expected,[daemon]))
 for key,value in [('exe','/bad'),('exe_sha256','bad'),('argv',[]),('cwd','/bad'),('fd0','pipe:0')]:no('child_'+key,lambda key=key,value=value:api.check_child({**child,key:value},{**child,key:value},pub,daemon,expected,[daemon]))
 no('stale_snapshot',lambda:api.check_child(child,{**child,'before':{**child['before'],'start_ticks':'old'}},pub,daemon,expected,[daemon]))
 no('wrong_public_pane',lambda:api.check_child(child,child,{**pub,'pane_pid':12},daemon,expected,[daemon]))
 no('wrong_private_ancestry',lambda:api.check_child(child,child,pub,daemon,expected,[fake(13,1)]))
 ok('fresh_marker',lambda:api.check_marker(marker,child,expected,'nonce'));no('stale_nonce',lambda:api.check_marker(marker,child,expected,'other'))
 clean={'identities_after':[{'before':own,'after':None}],'all_tracked_exited':True,'socket_after':None,'owned_descendants_remaining':[]};ok('cleanup_exact',lambda:api.check_cleanup(clean,{11:own}))
 for name,bad in [('missing',{'identities_after':[]}),('surviving',{'identities_after':[{'before':own,'after':own}]}),('socket',{'socket_after':{}})]:no('cleanup_'+name,lambda bad=bad:api.check_cleanup({**clean,**bad},{11:own}))
 fd={'fd':3,'identity_after_open':own,'events_before_request':[],'events_after_request':[[3,1]]};ok('held_exit_exact',lambda:api.check_kernel_exit(own,None,fd))
 no('exit_without_event',lambda:api.check_kernel_exit(own,None,{**fd,'events_after_request':[]}));no('exit_live_birth',lambda:api.check_kernel_exit(own,own,fd));no('exit_stale_handle',lambda:api.check_kernel_exit(own,None,{**fd,'identity_after_open':{**own,'start_ticks':'bad'}}))
 with tempfile.TemporaryDirectory(dir=config['root'],prefix='pure-') as temp:
  t=Path(temp);(t/'a').write_bytes(b'123');ok('physical_one',lambda:equal(physical(t),(3,1),'one'));(t/'b').write_bytes(b'123');ok('physical_equal_copies_both_count',lambda:equal(physical(t),(6,2),'copies'));os.link(t/'a',t/'c');ok('physical_hardlink_once',lambda:equal(physical(t),(6,2),'inode'))
  (t/'attempt/oracle').mkdir(parents=True);run=Run({'root':str(t/'attempt'),'oracle':'/unused','tokens':{'oracle':'pure'}},'oracle',time.monotonic(),{});run.record['large']='x'*10000;run.account('before');old=run.log_last['pending_bytes'];ref=run.seal('UNPROVEN','pure');run.account('after')
  ok('sealed_not_counted_as_pending',lambda:require(run.log_last['pending_bytes']<old and run.log_last['physical_bytes']==ref['bytes']+6,'sealed physical only'))
  no('future_reserve_overflow',lambda:run.account('future',future=CAP));no('sticky_blocks_admission',lambda:run.account('again'))
 checks.extend(publication_checks(config['root'])['checks'])
 return {'at':now(),'status':'PASS','checks':checks,'driver_sha256':digest(__file__),'api020_sha256':digest(API),'product_runs':0}

def prepare(config):
 root=Path(config['root']);require(root.is_dir(),'root missing')
 for phase in ('oracle','candidate'):
  p=root/phase;p.mkdir()
  for name in ('home-empty','xdg-empty','cwd','markers'):(p/name).mkdir()
  (p/'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-option -g base-index 0\nset-option -gw pane-base-index 0\nset-option -gw automatic-rename off\nset-option -gw synchronize-panes off\n")
  (p/'sync-off.conf.in').write_text('set-option -w -t OWN_WINDOW_ID synchronize-panes off\n')
 write_once(root/'PLAN.json',{'stages':list(STAGES),'injection':['send-keys','-t','QUALIFIED_OWN_PANE_ID','FRESH_ASCII_TOKEN','Enter'],'literal_flag':False,'cases':{'P1':'OFF direct, paneA','P2':'ON direct, paneA','P3':'OFF source-file, paneB'},'end_sentinels':['END_A to paneA','END_B to paneB'],'absence':'Receipts are helper consumption, not PTY echo. Input drained before each filesystem acknowledgment. Final ordered sentinel per pane and fresh paired acknowledgments certify a bounded prefix past prior injections. No unlimited absence claim.','scope_stop':'P2 no peer receipt => skipP3; two end sentinels then paired barriers must confirm same reception prefix. Complete cleanup then ORACLE_SCOPE_INOPERATIVE; never candidate.','budgets':{'seconds':180,'case':20,'cleanup':30,'helpers':2,'daemon':1,'injections':6,'challenges':12,'snapshots':24,'log_cap':CAP,'production_reserve':PRODUCTION_RESERVE,'closure_reserve':CLOSURE_RESERVE,'publication_reserve':PUBLICATION_RESERVE}})
 write_once(root/'PURE-CHECKS.json',pure_checks(config))

def execute(config,path,phase):
 root=Path(config['root']);frozen=json.loads((root/'FROZEN-NATIVE.json').read_text());refs={a['path']:a['sha256'] for a in frozen['artifacts']}
 required=[str(Path(__file__).resolve()),str(API),str(Path(path).resolve()),str(root/'PLAN.json'),str(root/'PURE-CHECKS.json')]+[config[k] for k in ('oracle','candidate','daemon','python')]+[str(root/p/f) for p in ('oracle','candidate') for f in ('startup.conf','sync-off.conf.in')]
 require(all(p in refs for p in required),'freeze incomplete')
 for p,h in refs.items():equal(digest(p),h,'frozen input '+p)
 require(not (root/'RECOVERY-OWNER.json').exists(),'recovery blocks admission');require(not (root/phase/'STARTED.json').exists(),'single-use native lot');expected={}
 if phase=='candidate':
  gate=json.loads((root/'ORACLE-FROZEN.json').read_text());equal(gate['status'],'PASS','complete oracle');equal(gate['result_sha256'],digest(root/'oracle/RESULT.json'),'oracle result');equal(gate['driver_sha256'],digest(__file__),'oracle source');expected=gate['effects']
 require(physical(root.parent)[0]+PRODUCTION_RESERVE<CAP,'phase physical budget');require(ctypes.CDLL(None,use_errno=True).prctl(36,1,0,0,0)==0,'subreaper')
 write_once(root/phase/'STARTED.json',{'at':now(),'identity':proc(os.getpid())});before=api.fd_snapshot();start=time.monotonic();run=Run(config,phase,start,expected);status='PASS';error=None;trace=None
 try:run.execute()
 except ScopeInoperative as e:status='ORACLE_SCOPE_INOPERATIVE';error=str(e)
 except Exception as e:status='RUNTIME_MISMATCH' if phase=='candidate' and isinstance(e,api.Failed) else 'UNPROVEN';error=repr(e);trace=traceback.format_exc()
 observations=copy.deepcopy(run.record);observed=None
 try:observed=run.seal(status,error)
 except Exception as e:run.log_errors.append({'point':'seal','error':repr(e)});run.record={'phase':phase,'commands':[],'profiles':[],'exits':[]}
 closure_error=None
 try:run.cleanup()
 except Exception as e:closure_error=repr(e);run.record['cleanup_traceback']=traceback.format_exc()
 after=api.fd_snapshot();elapsed=time.monotonic()-start;closure_ok=closure_error is None and before==after and elapsed<180
 result={**run.record,'start':observations['start'],'end':now(),'behavior_status':status,'status':status if closure_ok and not run.log_errors else ('UNPROVEN' if status in ('PASS','ORACLE_SCOPE_INOPERATIVE') else status),'error':error,'traceback':trace,'closure_error':closure_error,'closure_status':'PASS' if closure_ok else 'UNPROVEN','observed':observed,'frozen_sha256':digest(root/'FROZEN-NATIVE.json'),'elapsed_seconds':elapsed,'fd_before':before,'fd_after':after,'case_statuses':[{'id':c['id'],'status':c['status']} for c in observations['cases']],'not_run':[s for s in STAGES if s not in [c['id'] for c in observations['cases']]],'logging_errors':run.log_errors}
 run.record=result
 try:result=run.publish_result(root/phase/'RESULT.json',future=65536 if phase=='oracle' and result['status']=='PASS' else 0)
 except BudgetFailure as error:
  print(json.dumps({'phase':phase,'status':'LOG_BUDGET_FAIL','behavior_status':status,'closure_status':result['closure_status'],'error':str(error),'result_publication_complete':False}),flush=True)
  return 1
 if phase=='oracle' and result['status']=='PASS':
  equal([c['id'] for c in observations['cases']],list(STAGES),'complete oracle cases');require(observations['sentinel_barrier_complete'],'sentinel gate');equal(observations['cases'][1]['received_counts'],{'A':1,'B':1},'scope exercised')
  gate={'at':now(),'status':'PASS','driver_sha256':digest(__file__),'result_sha256':digest(root/phase/'RESULT.json'),'effects':{c['id']:c['received_counts'] for c in observations['cases']},'options':{c['id']:c['options']['windows'] for c in observations['cases']}};require(len(encoded(gate))<65536,'gate reserve');write_once(root/'ORACLE-FROZEN.json',gate)
 print(json.dumps({'phase':phase,'status':result['status'],'closure_status':result['closure_status'],'logging_status':result['logging']['status'],'elapsed_seconds':elapsed,'result_sha256':digest(root/phase/'RESULT.json'),'physical_log_bytes':physical(root.parent)[0],'not_run':result['not_run']}),flush=True)
 return 0 if result['status'] in ('PASS','ORACLE_SCOPE_INOPERATIVE') else 1

if __name__=='__main__':
 if len(sys.argv)==6 and sys.argv[1]=='witness':witness(*sys.argv[2:])
 else:
  mode,path=sys.argv[1:3];config=json.loads(Path(path).read_text())
  if mode=='prepare':prepare(config)
  elif mode=='run':raise SystemExit(execute(config,path,sys.argv[3]))
  else:raise ValueError('mode')
