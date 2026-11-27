"""Owned short-lived PTY peer: sizes and bytes are observed in the actual child."""
import os,sys,json,time,select,tty,pathlib
root=pathlib.Path(sys.argv[1]); label=sys.argv[2]; pid=os.getpid()
tty.setraw(0)
log=root/f'child-{pid}.jsonl'
def emit(token,raw):
 size=os.get_terminal_size(0)
 row={'pid':pid,'label':label,'token':token,'stdin_hex':raw.hex(),'cols':size.columns,'rows':size.lines,'isatty':os.isatty(0),'mono_ns':time.monotonic_ns()}
 data=(json.dumps(row,sort_keys=True)+'\n').encode()
 with log.open('ab',buffering=0) as f:f.write(data)
 os.write(1,b'NATIVE-CHILD '+data)
 return row
emit('ready',b''); end=time.monotonic()+60; pending=b''
while time.monotonic()<end:
 ready,_,_=select.select([0],[],[],min(1,max(0,end-time.monotonic())))
 if not ready:continue
 data=os.read(0,4096)
 if not data:break
 pending+=data
 while b'\n' in pending:
  raw,pending=pending.split(b'\n',1); token=raw.decode('ascii')
  emit(token,raw+b'\n')
  if token=='QUIT':sys.exit(0)
sys.exit(3)
