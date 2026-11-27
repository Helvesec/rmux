import sys,os,json,time,shlex,uuid,traceback,signal
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'tests/refresh_colour_reference'))
from support import Case,Client,identity,save,sha
from file_exchange import publish_json,read_json
import argparse
parser=argparse.ArgumentParser()
parser.add_argument('--binary',required=True)
parser.add_argument('--output',required=True)
args=parser.parse_args()
OUT=Path(args.output).resolve()
c=Case('candidate',args.binary,'theme',OUT,180)
runtime=Path.home()/'Projects'/('th-'+uuid.uuid4().hex[:6]);runtime.mkdir()
c.prefix=[c.binary,'-S',str(runtime/c.label),'-f','/dev/null']
serial=0; rows=[]; workers={}; live=[]
Q=b'\x1b[?996n'; ON=b'\x1b[?2031h'; OFF=b'\x1b[?2031l'; MODE=b'\x1b[?2031$p'
def worker(name,respawn=None):
 d=c.out/name;d.mkdir();workers[name]=d
 cmd=shlex.join([sys.executable,'-B',str(ROOT/'tests/theme_reference/worker.py'),str(d)])
 if respawn:c.q('respawn-pane','-k','-t',respawn,cmd)
 else:c.q('new-session','-d','-s',name,cmd,timeout=10)
 if not c.server:c.handshake()
 c.until(lambda:(d/'ready').exists() and (d/'ready').read_text().isdigit(),'worker ready')
 c.producers.append(identity(int((d/'ready').read_text())))
 return c.q('display-message','-p','-t',respawn or name,'#{pane_id}')
def emit(name,payload=b''):
 global serial
 serial+=1;d=workers[name]
 publish_json(d/'request.json',{'id':serial,'bytes':payload.hex()})
 def got():
  try:
   r=read_json(d/'reply.json');return r if r['id']==serial else None
  except (ValueError,FileNotFoundError):return None
 return c.until(got,'child reply')['bytes']
def obs(label,action=None,payload=None,names=None):
 before=c.q('show-buffer','-b','theme-hooks',check=False)
 if action:action()
 time.sleep(.10)
 names=names or list(workers)
 unsolicited={n:emit(n) for n in names}
 replies={n:emit(n,Q if payload is None else payload) for n in names}
 after=c.q('show-buffer','-b','theme-hooks',check=False)
 row={'label':label,'unsolicited':unsolicited,'request':(Q if payload is None else payload).hex(),'replies':replies,'hooks_delta':after[len(before):] if after.startswith(before) else after,'clients':c.q('list-clients','-F','#{client_pid}|#{client_name}|#{client_session}|#{client_theme}')}
 rows.append(row);save(c.out/'observations.json',rows);print(label,flush=True)
def attach(session):
 a=Client(c,session);live.append(a)
 c.until(lambda:any(r[0]==str(a.p.pid) for r in c.client_rows()),'attach ready')
 return a
def report(a,n):os.write(a.master,f'\x1b[?997;{n}n'.encode())
def detach(a):
 c.q('detach-client','-t',a.tty);a.close();c.clients.remove(a);live.remove(a)
checks=[]
D='1b5b3f3939373b316e';L='1b5b3f3939373b326e'
def expect(label,actual,wanted):
 checks.append({'label':label,'actual':actual,'expected':wanted,'passed':actual==wanted})
try:
 p=worker('p');q=worker('q')
 obs('unknown');expect('unknown',rows[-1]['replies'],{'p':'','q':''})
 obs('ordered',payload=b'\x1b]11;#ffffff\x07'+Q+b'\x1b]11;#000000\x07'+Q)
 expect('ordered-two-colours',rows[-1]['replies'],{'p':L+D,'q':L+D})
 obs('reset',payload=b'\x1b]111;\x07'+Q)
 obs('subscribe',payload=ON+MODE)
 obs('white',payload=b'\x1b]11;#ffffff\x07')
 expect('notify-app',rows[-1]['replies'],{'p':L,'q':L})
 obs('white-duplicate',payload=b'\x1b]11;#ffffff\x07')
 expect('deduplicate-app',rows[-1]['replies'],{'p':'','q':''})
 obs('reset2',payload=b'\x1b]111;\x07')
 a=attach('p');obs('attach-unknown')
 obs('client-dark',lambda:report(a,1))
 expect('C-D90',rows[-1]['unsolicited'],{'p':D,'q':''})
 expect('client-query',rows[-1]['replies'],{'p':D,'q':''})
 obs('client-dark-duplicate',lambda:report(a,1))
 expect('duplicate-report-notify',rows[-1]['unsolicited'],{'p':'','q':''})
 b=attach('p');obs('second-unknown')
 expect('known-plus-unknown',rows[-1]['replies'],{'p':D,'q':''})
 obs('conflict',lambda:report(b,2))
 expect('conflict-unknown',rows[-1]['replies'],{'p':'','q':''})
 obs('conflict-resolves',lambda:detach(b))
 expect('detach-restores-known',rows[-1]['replies'],{'p':D,'q':''})
 obs('switch',lambda:c.q('switch-client','-c',a.tty,'-t','q'))
 expect('switch-source',rows[-1]['replies'],{'p':'','q':D})
 obs('detach',lambda:detach(a))
 expect('detach-unknown',rows[-1]['replies'],{'p':'','q':''})
 # The same bounded qualification matrix: colour priorities and owned lifetimes.
 a=attach('p');obs('replacement-unknown')
 expect('replacement-unknown',rows[-1]['replies'],{'p':'','q':''})
 c.q('set-buffer','-b','theme-hooks','')
 for name in ['dark','light']:
  c.q('set-hook','-g','client-'+name+'-theme','set-buffer -a -b theme-hooks '+name+',')
 obs('light-hook',lambda:report(a,2))
 expect('light-hook',rows[-1]['hooks_delta'],'light,')
 obs('duplicate-light-hook',lambda:report(a,2))
 expect('duplicate-light-hook',rows[-1]['hooks_delta'],'light,')
 expect('duplicate-light-silent',rows[-1]['unsolicited'],{'p':'','q':''})
 obs('outer-black',lambda:os.write(a.master,b'\x1b]11;rgb:0000/0000/0000\x07'))
 expect('outer-bg-priority',rows[-1]['replies'],{'p':D,'q':''})
 obs('app-white-priority',payload=b'\x1b]11;#ffffff\x07'+Q)
 expect('app-over-outer',rows[-1]['replies'],{'p':L,'q':L})
 for rgb,want in [('#7f7f80',D),('#7f8080',L)]:
  obs('threshold-'+rgb,payload=b'\x1b]11;'+rgb.encode()+b'\x07'+Q)
  expect('threshold-'+rgb,rows[-1]['replies'],{'p':want,'q':want})
 obs('app-reset-to-outer',payload=b'\x1b]111;\x07'+Q)
 expect('reset-priority',rows[-1]['replies'],{'p':D,'q':''})
 ctl=Client(c,'q',control=True)
 target=c.q('list-clients','-F','#{client_name}','-f','#{client_control_mode}')
 obs('unknown-control')
 expect('C-D91',rows[-1]['replies'],{'p':D,'q':''})
 obs('R-white',lambda:c.q('refresh-client','-t',target,'-r',p+':\x1b]11;#ffffff\x07'))
 expect('R-scope',rows[-1]['replies'],{'p':L,'q':''})
 obs('R-over-app',payload=b'\x1b]11;#000000\x07'+Q)
 expect('R-over-app',rows[-1]['replies'],{'p':L,'q':D})
 c.q('detach-client','-t',target);ctl.close();c.clients.remove(ctl)
 obs('last-control-exit')
 expect('last-control-exit',rows[-1]['replies'],{'p':D,'q':D})
 ctl=Client(c,'q',control=True)
 obs('control-replacement')
 expect('R-reactivates',rows[-1]['replies'],{'p':L,'q':D})
 worker('p2',respawn=p);del workers['p']
 obs('respawn-R-kept-subscription-reset',payload=MODE+Q)
 expect('respawn-mode',rows[-1]['replies'],{'q':'1b5b3f323033313b312479'+D,'p2':'1b5b3f323033313b322479'+L})
 obs('disable-all',payload=OFF+MODE)
 expect('disable-all',rows[-1]['replies'],{'q':'1b5b3f323033313b322479','p2':'1b5b3f323033313b322479'})
 # Invalid outer report is consumed, unlike tmux (C-D92).
 obs('invalid-report',lambda:report(a,9),payload=b'')
 expect('C-D92',rows[-1]['unsolicited'],{'q':'','p2':''})
 # Finish the control first; direct client still has its own black background.
 target=c.q('list-clients','-F','#{client_name}','-f','#{client_control_mode}')
 c.q('detach-client','-t',target);ctl.close();c.clients.remove(ctl)
 obs('fragmented-query-prefix',payload=b'\x1b[?99',names=['p2'])
 expect('fragment-prefix-silent',rows[-1]['replies'],{'p2':''})
 c.q('switch-client','-c',a.tty,'-t','q')
 obs('fragmented-query-suffix',payload=b'6n',names=['p2'])
 expect('fragment-query-not-retargeted',rows[-1]['replies'],{'p2':''})
 obs('current-other-pane',names=['q'])
 expect('current-other-pane',rows[-1]['replies'],{'q':D})
 obs('clear-app-before-hook',payload=b'\x1b]111;\x07')
 c.q('set-hook','-g','client-light-theme',shlex.join(['switch-client','-c',a.tty,'-t','p']))
 obs('hook-switch-with-report-tail',lambda:os.write(a.master,b'\x1b[?997;2n\x1b[?997;1n'))
 expect('hook-tail-current-source',rows[-1]['replies'],{'q':'','p2':D})
 # Replace the attach after a fragmented report; old bytes cannot establish the new client's state.
 obs('retired-partial-report',lambda:os.write(a.master,b'\x1b[?997;'),payload=b'')
 detach(a);a=attach('p')
 obs('replacement-after-partial')
 expect('new-registration-no-theme',rows[-1]['replies'],{'q':'','p2':''})
 obs('replacement-full-report',lambda:report(a,1))
 expect('new-registration-owned-report',rows[-1]['replies'],{'q':'','p2':D})
 # A session containing an inactive window remains eligible; duplicate aliases do not duplicate notifications.
 c.q('set-hook','-gu','client-light-theme')
 c.q('move-window','-s','q:0','-t','p:1')
 obs('inactive-window-eligible')
 expect('inactive-window-eligible',rows[-1]['replies'],{'q':D,'p2':D})
 c.q('link-window','-s','p:1','-t','p:2')
 obs('subscribe-linked',payload=ON)
 obs('linked-change',lambda:os.write(a.master,b'\x1b]11;rgb:ffff/ffff/ffff\x07'),payload=b'')
 expect('linked-once-per-PaneId',rows[-1]['unsolicited'],{'q':L,'p2':L})
 worker('p3',respawn=p);del workers['p2']
 expect('first-query-new-runtime-outer-source',read_json(workers['p3']/'initial.json')['reply'],L)
 obs('replies-under-2026',payload=b'\x1b[?2026h'+Q+MODE+b'\x1b[c')
 expect('replies-under-2026',rows[-1]['replies'],{'q':L+'1b5b3f323033313b3124791b5b3f313b3263','p3':L+'1b5b3f323033313b3224791b5b3f313b3263'})
 obs('2026-still-on',payload=b'\x1b[?2026$p')
 expect('2026-still-on',rows[-1]['replies'],{'q':'1b5b3f323032363b312479','p3':'1b5b3f323032363b312479'})
 obs('end-2026',payload=b'\x1b[?2026l')
 # Two backgrounds use creation order; a late reply remains a fact of the switched client.
 z=worker('z');b=attach('p')
 obs('second-background-black',lambda:os.write(b.master,b'\x1b]11;#000000\x07'))
 expect('first-background-wins',rows[-1]['replies'],{'q':L,'p3':L,'z':''})
 c.q('switch-client','-c',a.tty,'-t','z')
 obs('late-background-after-switch',lambda:os.write(a.master,b'\x1b]11;#000000\x1b\\'))
 expect('late-background-owner',rows[-1]['replies'],{'q':D,'p3':D,'z':D})
 detach(b);c.q('switch-client','-c',a.tty,'-t','p')
 c.q('kill-session','-t','z');del workers['z']
 # Keep the owned client stopped while the server publishes suspension, then let the protocol resume.
 os.kill(a.p.pid,signal.SIGSTOP)
 obs('suspended-source',lambda:c.q('suspend-client','-t',a.tty))
 expect('suspended-source-unknown',rows[-1]['replies'],{'q':'','p3':''})
 os.kill(a.p.pid,signal.SIGCONT)
 time.sleep(.1)
 os.kill(a.p.pid,signal.SIGCONT)
 c.until(lambda:'suspended' not in c.q('list-clients','-F','#{client_flags}'),'client resumed')
 obs('resumed-source')
 expect('resumed-source-known',rows[-1]['replies'],{'q':D,'p3':D})
 # Session groups and join/break keep stable pane identity and recompute eligibility.
 c.q('new-session','-d','-s','group','-t','p')
 c.q('switch-client','-c',a.tty,'-t','group')
 obs('grouped-session-source')
 expect('grouped-session-source',rows[-1]['replies'],{'q':D,'p3':D})
 c.q('switch-client','-c',a.tty,'-t','p');c.q('kill-session','-t','group')
 c.q('unlink-window','-t','p:2')
 c.q('join-pane','-d','-s',q,'-t',p)
 obs('joined-pane-source')
 expect('joined-pane-source',rows[-1]['replies'],{'q':D,'p3':D})
 c.q('break-pane','-d','-s',q)
 obs('broken-pane-source')
 expect('broken-pane-source',rows[-1]['replies'],{'q':D,'p3':D})
 # Window respawn uses a fresh parser, preserving the PaneId's source data only.
 d=c.out/'p4';d.mkdir();workers['p4']=d
 command=shlex.join([sys.executable,'-B',str(ROOT/'tests/theme_reference/worker.py'),str(d)])
 c.q('respawn-window','-k','-t',p,command)
 c.until(lambda:(d/'ready').exists() and (d/'ready').read_text().isdigit(),'window respawn worker ready')
 c.producers.append(identity(int((d/'ready').read_text())))
 del workers['p3']
 expect('window-respawn-first-query',read_json(d/'initial.json')['reply'],D)
 obs('window-respawn-subscription',payload=MODE)
 expect('window-respawn-subscription',rows[-1]['replies'],{'q':'1b5b3f323033313b312479','p4':'1b5b3f323033313b322479'})
 # A destroyed PaneId cannot lend its retained R report to a new pane.
 c.q('kill-pane','-t',p);del workers['p4']
 d=c.out/'fresh';d.mkdir();workers['fresh']=d
 command=shlex.join([sys.executable,'-B',str(ROOT/'tests/theme_reference/worker.py'),str(d)])
 fresh=c.q('split-window','-d','-P','-F','#{pane_id}','-t',q,command)
 c.until(lambda:(d/'ready').exists() and (d/'ready').read_text().isdigit(),'fresh pane worker ready')
 c.producers.append(identity(int((d/'ready').read_text())))
 expect('new-pane-id',fresh!=p,True)
 ctl=Client(c,'p',control=True)
 target=c.q('list-clients','-F','#{client_name}','-f','#{client_control_mode}')
 c.q('refresh-client','-t',target,'-r',p+':\x1b]11;#ffffff\x07',check=False)
 obs('retired-R-does-not-transfer')
 expect('retired-R-does-not-transfer',rows[-1]['replies'],{'q':D,'fresh':D})
 for name,d in workers.items():expect('tty-'+name,read_json(d/'initial.json')['tty'],[True,True,True])
 c.r['success']=all(r['passed'] for r in checks)
finally:
 c.r['checks']=checks;c.r['producers']=c.producers;c.close();save(c.out/'observations.json',rows)
 c.r['runtime_remaining']=list(map(str,runtime.iterdir()))
 if not list(runtime.iterdir()):runtime.rmdir()
 save(c.out/'receipt.json',c.r)
print(json.dumps(checks,indent=2))
sys.exit(0 if all(r['passed'] for r in checks) else 1)
