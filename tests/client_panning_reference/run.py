#!/usr/bin/env python3
"""Freeze tmux 3.7b first, then replay the same native PTY/view assertions."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time
import traceback

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT/'tests/client_loop_reference'))
from support import Case, Client, save, sha
sys.path.insert(0, str(ROOT/'tests'))
from control_window_sizes_reference import control
class NativeCase(Case):
    def q(self, *args, timeout=5, **kwargs):
        if args and args[0]=='new-session' and self.server is None:
            timeout=10
        return super().q(*args, timeout=timeout, **kwargs)

ORACLE_SHA = '9cc80ae8fda9aa0843e64294645ded292dbe3ef318aa8cea4ed2738e760f8326'


def setup(c, status='off', position='bottom'):
    c.new('s', rows=50, cols=140)
    c.q('set', '-g', 'status', status)
    c.q('set', '-g', 'status-position', position)
    c.q('set', '-g', 'status-left', '')
    c.q('set', '-g', 'status-right', 'PAN=#{window_offset_x},#{window_offset_y}')
    c.q('set', '-w', 'window-size', 'manual')
    c.q('resize-window', '-x', '140', '-y', '50')
    p = Client(c, 's', False)
    c.wait_clients([p])
    receipt = c.out/'input.txt'
    command = shlex.join([sys.executable, str(ROOT/'tests/client_panning_reference/producer.py'), str(receipt.resolve())])
    c.q('respawn-pane', '-k', command)
    c.until(lambda: receipt.exists(), 'native fixture ready')
    c.receipt, c.p = receipt, p
    view(c, p, 0, 0, status=status, position=position)
    return p


def offsets(c, p):
    # Client-loop binding selects a generation by observed PID. Keep the base's
    # command-client versus display-message delivery-client distinction intact.
    lines = c.q('list-clients', '-F', '#{client_pid}|#{window_offset_x},#{window_offset_y}|#{window_bigger}').splitlines()
    return next(line.split('|',1)[1] for line in lines if line.startswith(str(p.p.pid)+'|'))


def screen(c, p, label, cols=80, rows=24):
    raw = c.out/(label+'.bin')
    raw.write_bytes(bytes(p.drains[0].raw))
    result = subprocess.run(['/opt/homebrew/bin/node', str(ROOT/'tests/client_panning_reference/screen.mjs'), str(raw), str(cols), str(rows)], capture_output=True, text=True, timeout=3)
    if result.returncode: raise RuntimeError(result.stderr)
    parsed = json.loads(result.stdout)
    save(c.out/(label+'.json'), parsed)
    c.r.setdefault('screens', []).append(dict(label=label, raw_sha256=sha(raw), **parsed))
    return parsed


def view(c, p, x, y, status='off', position='bottom', cells=True, cols=80, rows=24, bigger=True):
    wanted = f'{x},{y}|1' if bigger else ',|0'
    c.until(lambda: offsets(c,p) == wanted, 'viewport offsets '+wanted)
    time.sleep(.08)
    label = 'view-'+str(len(c.r.get('screens', [])))
    parsed = screen(c,p,label,cols,rows)
    if cells:
        n = 0 if status=='off' else int(status) if status.isdigit() else 1
        first = n if position=='top' else 0
        expected = [''.join(f'{r:02d}:{col:03d}|' for col in range(20))[x:x+cols] for r in range(y,y+rows-n)]
        actual = parsed['rows'][first:first+rows-n]
        c.require(actual == expected, 'actual terminal cells follow viewport', expected=expected, actual=actual, offset=[x,y])
    if status != 'off':
        n = int(status) if status.isdigit() else 1
        lines = parsed['rows'][:n] if position=='top' else parsed['rows'][-n:]
        c.require(any(f'PAN={x},{y}' in line for line in lines), 'status uses the same client viewport', lines=lines)
    c.require(offsets(c,p) == wanted, 'observed offsets stable through screen read')
    return parsed


def pan(c, p, *args):
    c.q('refresh-client', '-t', p.tty, *args)


def key(c, text):
    before = c.receipt.read_text()
    c.q('send-keys', '-l', text)
    c.until(lambda: len(c.receipt.read_text()) > len(before), 'fixture input acknowledged')
    time.sleep(.08)


def transitions(c):
    p = setup(c)
    for args,x,y in [(('-R',),1,0),(('-D',),1,1),(('-LU','3'),0,1),(('-cR','7'),0,0),
                     (('-RD','7'),7,0),(('-UD','3'),7,0),(('-D','2147483647'),7,26),
                     (('-R','2147483647'),60,26),(('-R','2147483647'),60,26),
                     (('-L','4'),56,26),(('-U','6'),56,20)]:
        pan(c,p,*args);view(c,p,x,y)
    key(c,'f');view(c,p,56,20)
    pan(c,p,'-c');view(c,p,60,26)
    key(c,'o');view(c,p,0,0)
    key(c,'m');view(c,p,60,2)
    key(c,'h');view(c,p,0,0)
    key(c,'v');view(c,p,60,2)
    pan(c,p,'-L','9');view(c,p,51,2)
    key(c,'o');view(c,p,51,2)
    for args in [('-D','0'),('-c','0'),('-R','2147483648')]:
        c.q('refresh-client','-t',p.tty,*args,check=False)
        c.require(c.r['commands'][-1]['returncode'] != 0, 'invalid adjustment fails', args=args)
        view(c,p,51,2)
    pan(c,p,'0');view(c,p,51,2)
    pan(c,p,'-c');view(c,p,0,0)
    c.require(c.q('display-message','-p','#{window_width}x#{window_height}|#{pane_width}x#{pane_height}') == '140x50|140x50', 'panning preserves canonical/native size')


def routes(c):
    p=setup(c)
    ctrl=Client(c,'s',True)
    control(c,ctrl,'refresh-client -t '+p.tty+' -R 8 ; refresh-client -t '+p.tty+' -D 3')
    view(c,p,8,3)
    source=c.out/'pan.conf';source.write_text('refresh-client -t '+p.tty+' -LU 2\n')
    c.q('source-file',str(source.resolve()));view(c,p,6,3)
    c.q('refresh-client','-t',p.tty,'-U','2',';','refresh-client','-t',p.tty,'-R','1');view(c,p,7,1)
    c.q('bind-key','-n','F6','refresh-client','-R','3')
    os.write(p.master,b'\x1b[17~');view(c,p,10,1)
    # Pan takes precedence over otherwise supported effects.
    pan(c,p,'-R','-S','-l','-f','ignore-size','-C','@0:20x10','2');view(c,p,12,1)
    flags=c.q('list-clients','-F','#{client_pid}|#{client_flags}')
    c.require(not any('ignore-size' in r for r in flags.splitlines() if r.startswith(str(p.p.pid)+'|')), 'pan does not apply subordinate flags')
    control(c,ctrl,'refresh-client -R -C 20x10')
    c.require(c.q('display-message','-p','#{window_width}x#{window_height}')=='140x50','control pan does not execute C')
    control(c,ctrl,'refresh-client -B "off::#{window_offset_x},#{window_offset_y}|#{window_bigger}"')
    c.until(lambda: b',|0' in ctrl.drains[0].raw, 'B retains control viewport context',4)
    control(c,ctrl,'refresh-client -c')
    view(c,p,12,1)
    pan(c,p,'-c');view(c,p,0,0)


def geometry(c):
    p=setup(c,'2','top')
    pan(c,p,'-R','20');pan(c,p,'-D','7');view(c,p,20,7,status='2',position='top')
    p2=Client(c,'s',False,rows=30,cols=100)
    view(c,p2,0,0,status='2',position='top',cols=100,rows=30)
    view(c,p,20,7,status='2',position='top')
    c.q('set','status-position','bottom');view(c,p,20,7,status='2')
    pan(c,p,'-D','2147483647');view(c,p,20,28,status='2')
    c.q('set','status','off');view(c,p,20,26)
    fcntl.ioctl(p.master,termios.TIOCSWINSZ,struct.pack('HHHH',35,120,0,0))
    os.kill(p.p.pid,signal.SIGWINCH)
    view(c,p,20,15,cols=120,rows=35)
    fcntl.ioctl(p.master,termios.TIOCSWINSZ,struct.pack('HHHH',24,80,0,0))
    os.kill(p.p.pid,signal.SIGWINCH)
    view(c,p,20,15)
    # A canonical C constraint clamps offsets and preserves the manual reference.
    ctrl=Client(c,'s',True)
    wid=c.q('display-message','-p','#{window_id}')
    control(c,ctrl,'refresh-client -C '+wid+':90x30')
    view(c,p,10,6,cells=False)
    control(c,ctrl,'refresh-client -C '+wid+':')
    view(c,p,10,6,cells=False)
    c.q('resize-window','-x','70','-y','20');view(c,p,0,0,cells=False,bigger=False)
    c.q('resize-window','-x','140','-y','50')
    # Reflow moves the canonical cursor differently in the inherited engines
    # (separate baseline probe). Set the same cursor before testing auto reset.
    c.r['cursor_after_reflow']=c.q('display-message','-p','#{cursor_x},#{cursor_y}')
    key(c,'o');view(c,p,0,0,cells=False)
    key(c,'f');view(c,p,60,26,cells=False)


def precedence(c):
    p = setup(c)
    ctrl = Client(c, 's', True)
    for x, flag in enumerate(['-C', '-A', '-B', '-f'], 1):
        control(c, ctrl, 'refresh-client -t '+p.tty+' -R '+flag+' bad')
        view(c, p, x, 0)
    pan(c, p, '-R', '-C', 'bad', '2'); view(c, p, 6, 0)
    source = c.out/'precedence.conf'
    source.write_text('refresh-client -t '+p.tty+' -RCbad 2\n')
    c.q('source-file', str(source.resolve())); view(c, p, 8, 0)
    control(c, ctrl, 'refresh-client -R -C bad')
    view(c, p, 8, 0)
    control(c, ctrl, 'refresh-client -C bad', error=True)
    view(c, p, 8, 0)


def identities(c):
    p=setup(c)
    pan(c,p,'-R','11');pan(c,p,'-D','4');view(c,p,11,4)
    wid=c.q('display-message','-p','#{window_id}')
    c.q('move-window','-s','s:0','-t','s:3');view(c,p,11,4)
    c.q('rename-window','renamed');view(c,p,11,4)
    c.q('new-window','-d','-t','s:1','/bin/sleep 60')
    c.q('select-window','-t','s:1');view(c,p,0,0,cells=False,bigger=False)
    c.q('select-window','-t','s:3');view(c,p,0,0)
    pan(c,p,'-D','3');view(c,p,0,3)
    c.new('t',rows=24,cols=80)
    c.q('link-window','-s','s:3','-t','t:5')
    c.q('switch-client','-c',p.tty,'-t','t:5')
    view(c,p,0,3)
    c.require(c.q('display-message','-p','-t','t:5','#{window_id}')==wid,'pan alias retains canonical identity')
    c.q('switch-client','-c',p.tty,'-t','s:1')
    c.q('switch-client','-c',p.tty,'-t','s:3');view(c,p,0,0)


def mouse_zoom_selection(c):
    p=setup(c)
    pan(c,p,'-R','10');pan(c,p,'-D','3');view(c,p,10,3)
    key(c,'e')
    before=c.receipt.read_text();os.write(p.master,b'\x1b[<0;5;6M')
    c.until(lambda: c.receipt.read_text()!=before,'application mouse input')
    c.require('1b5b3c303b31353b394d' in c.receipt.read_text(), 'mouse translated to canonical pane 15,9', receipt=c.receipt.read_text())
    c.q('set','mouse','on')
    c.q('split-window','-h','-d','-l','50','/bin/sleep 60')
    pan(c,p,'-R','40');view(c,p,50,3,cells=False)
    os.write(p.master,b'\x1b[<0;60;5M\x1b[<0;60;5m')
    c.until(lambda:c.q('display-message','-p','#{pane_index}')=='1','panned mouse focuses right pane')
    c.q('resize-pane','-Z');view(c,p,50,3,cells=False)
    c.q('resize-pane','-Z');view(c,p,50,3,cells=False)
    c.q('select-pane','-t','s:0.0')
    c.q('copy-mode');pan(c,p,'-c');view(c,p,0,0,cells=False)
    c.q('send-keys','-X','begin-selection');c.q('send-keys','-X','cursor-right');pan(c,p,'-R','5');view(c,p,5,0,cells=False)
    c.require(c.q('display-message','-p','#{pane_in_mode}|#{selection_present}')=='1|1','panning preserves copy-mode selection')
    c.q('send-keys','-X','cancel');view(c,p,5,0,cells=False)

def render_neighbors(c):
    p=setup(c)
    pan(c,p,'-R','10');pan(c,p,'-D','3');view(c,p,10,3)
    key(c,'w')
    rendered=view(c,p,10,3,cells=False)
    c.require(rendered['rows'][6][10:17]=='UPDATED', 'live output reaches the panned cells')
    # Default reset binding traverses actual prefix dispatch.
    os.write(p.master,b'\x02\x1b[3~');view(c,p,0,0,cells=False)
    pan(c,p,'ignored-position');view(c,p,0,0,cells=False)
    # A zoomed pane still has canonical content larger than the terminal.
    c.q('split-window','-h','-d','/bin/sleep 60')
    c.q('resize-pane','-Z')
    command=shlex.join([sys.executable,str(ROOT/'tests/client_panning_reference/producer.py'),str(c.receipt.resolve())])
    c.q('respawn-pane','-k',command)
    pan(c,p,'-R','20');pan(c,p,'-D','5');view(c,p,20,5)
    c.require(c.q('display-message','-p','#{window_zoomed_flag}|#{pane_index}')=='1|0','pan preserves zoom and selection')
    c.q('resize-pane','-Z');view(c,p,20,5,cells=False)
    c.q('kill-pane','-t','s:0.1')
    c.q('resize-window','-x','200','-y','50')
    key(c,'m');pan(c,p,'-c');view(c,p,40,2,cells=False)


def delayed_mouse_selection(c):
    p = setup(c)
    c.q('set', 'mouse', 'on')
    c.q('copy-mode')
    c.q('bind', '-T', 'copy-mode', 'DoubleClick1Pane', 'send-keys', '-X', 'select-word')
    pan(c, p, '-R', '10'); pan(c, p, '-D', '3')
    view(c, p, 10, 3, cells=False)
    os.write(p.master, b'\x1b[<0;5;6M\x1b[<0;5;6m')
    time.sleep(.05)
    os.write(p.master, b'\x1b[<0;5;6M\x1b[<0;5;6m')
    fmt = '#{copy_cursor_x},#{copy_cursor_y}|#{selection_start_x},#{selection_start_y}|#{selection_end_x},#{selection_end_y}'
    # The unpanned C baseline uses an inclusive end/cursor; tmux uses the next
    # column. Preserve that existing convention while translating the click.
    end = 16 if c.engine == 'oracle' else 15
    expected = f'{end},8|14,8|{end},8'
    c.until(lambda: c.q('display-message', '-p', fmt) == expected, 'delayed click selects the panned word')
    c.r['selection_observation'] = c.q('display-message', '-p', fmt)
    c.q('send-keys', '-X', 'copy-selection-no-clear')
    c.require(c.q('show-buffer') == '08', 'copied text is from the actually clicked panned row')
    view(c, p, 10, 3, cells=False)


def cursor_visibility(c):
    p=setup(c)
    def check(visible, position=None):
        time.sleep(.08)
        value=screen(c,p,'cursor-'+str(len(c.r.get('screens',[]))))
        c.require(value['cursor_visible']==visible,'terminal cursor visibility follows viewport',actual=value,expected=visible)
        if position is not None:c.require(value['cursor']==position,'terminal cursor position follows viewport',actual=value['cursor'],expected=position)
    check(True,[0,0])
    pan(c,p,'-R','10');check(False)
    pan(c,p,'-L','10');check(True,[0,0])
    c.q('split-window','-h','-d','-l','99','/bin/sleep 60')
    key(c,'o')
    pan(c,p,'-R','60');check(False)
    pan(c,p,'-c');check(True,[0,0])
    pan(c,p,'-R','60');check(False)
    c.q('select-pane','-t','s:0.1');check(False)
    pan(c,p,'-c');check(True,[41,0])
    c.q('resize-pane','-Z');check(True,[0,0])
    pan(c,p,'-R','60');check(False)


def client_loops(c):
    p=setup(c,'on')
    pan(c,p,'-R','14');pan(c,p,'-D','5');view(c,p,14,5,status='on')
    p2=Client(c,'s',False)
    ctrl=Client(c,'s',True)
    fmt='#{L:#{client_width}:#{window_offset_x},#{window_offset_y};}'
    expected='80:14,5;80:0,0;80:,;'
    c.q('set','status-right-length','60')
    c.q('set','status-right',fmt)
    time.sleep(.15)
    for client in [p,p2]:
        value=screen(c,client,'loop-'+str(client.p.pid))
        c.require(expected in value['rows'][-1],'L uses each client viewport inside a rendered status',expected=expected,actual=value['rows'][-1])
    control(c,ctrl,'refresh-client -B "pan::'+fmt+'"')
    def subscription(expected, cursor):
        def changed():
            lines = bytes(ctrl.drains[0].raw[cursor:]).decode(errors='replace').splitlines()
            return [line for line in lines if line.startswith('%subscription-changed pan ') and line.endswith(expected)]
        lines = c.until(changed, 'B and rendered L agree', 4)
        c.require(bool(lines), 'B reports the client loop offsets', expected=expected, lines=lines)
    subscription(expected, 0)
    for client, args, expected in [
        (p2, ('-R', '3'), '80:14,5;80:3,0;80:,;'),
        (p, ('-D', '2'), '80:14,7;80:3,0;80:,;'),
        (p, ('-c',), '80:0,0;80:3,0;80:,;'),
    ]:
        cursor = len(ctrl.drains[0].raw)
        pan(c, client, *args)
        subscription(expected, cursor)
        for rendered in [p, p2]:
            c.q('refresh-client', '-S', '-t', rendered.tty)
            time.sleep(.08)
            value = screen(c, rendered, 'loop-update-'+str(len(c.r['screens'])))
            c.require(expected in value['rows'][-1], 'status L follows independent pan/reset', expected=expected, actual=value['rows'][-1])


CASES={f.__name__:f for f in [transitions,routes,geometry,identities,mouse_zoom_selection,render_neighbors,cursor_visibility,client_loops,delayed_mouse_selection,precedence]}

def main():
    a=argparse.ArgumentParser();a.add_argument('--engine',choices=['oracle','candidate'],required=True);a.add_argument('--binary',required=True);a.add_argument('--output',type=Path,required=True);a.add_argument('--freeze',type=Path);a.add_argument('--cases',nargs='+',choices=CASES,default=list(CASES));args=a.parse_args()
    if args.output.exists():raise SystemExit('fresh output required')
    args.output.mkdir(parents=True)
    files=[Path(__file__),Path(__file__).with_name('producer.py'),Path(__file__).with_name('screen.mjs'),ROOT/'tests/client_loop_reference/support.py',ROOT/'tests/control_window_sizes_reference.py',ROOT/'tests/xterm-oracle/package-lock.json']
    hashes={str(p.relative_to(ROOT)):sha(p) for p in files}
    if args.engine=='oracle':assert sha(args.binary)==ORACLE_SHA
    if args.freeze:
        freeze=json.loads(args.freeze.read_text());assert freeze['hashes']==hashes and freeze['passed']
    results=[]
    for name in args.cases:
        c=NativeCase(args.engine,args.binary,name,args.output,120)
        try:
            CASES[name](c);c.handshake();c.r['passed']=True
        except Exception:
            c.r['passed']=False;c.r['error']=traceback.format_exc();print(c.r['error'],flush=True)
        finally:c.close()
        results.append(dict(case=name,passed=c.r['passed'],assertions=len(c.r['assertions'])))
        print(name,c.r['passed'],flush=True)
    result=dict(engine=args.engine,binary=str(Path(args.binary).resolve()),binary_sha256=sha(args.binary),hashes=hashes,cases=results,passed=all(r['passed'] for r in results))
    save(args.output/'result.json',result)
    return 0 if result['passed'] else 1
if __name__=='__main__':sys.exit(main())
