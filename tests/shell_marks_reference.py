#!/usr/bin/env python3
"""Native OSC 133 PTY contract, with dedicated sockets and bounded commands.

Run with --binary /absolute/tmux-or-rmux --out fresh-directory. The same
literal cases run against the pinned Mac tmux 3.7b and the Linux candidate.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import traceback


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def executable(pid):
    if sys.platform == 'darwin':
        lib = ctypes.CDLL('/usr/lib/libproc.dylib')
        buf = ctypes.create_string_buffer(4096)
        if lib.proc_pidpath(int(pid), buf, len(buf)) <= 0:
            return None
        return os.path.realpath(os.fsdecode(buf.value))
    try:
        return os.readlink(f'/proc/{pid}/exe')
    except FileNotFoundError:
        return None


def emitter(root):
    import tty
    tty.setraw(0)
    root = Path(root)
    sequence = 1
    os.write(1, b'\x1b]2;READY\x07')
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        path = root / f'{sequence}.json'
        if not path.exists():
            time.sleep(.005)
            continue
        request = json.loads(path.read_text())
        for chunk in request['chunks']:
            os.write(1, bytes.fromhex(chunk))
            if len(request['chunks']) > 1:
                time.sleep(.01)
        if 'reply' in request:
            import select
            wanted = bytes.fromhex(request['reply'])
            received = bytearray()
            stop = time.monotonic() + 4
            while len(received) < len(wanted) and time.monotonic() < stop:
                if select.select([0], [], [], .05)[0]:
                    received.extend(os.read(0, 65536))
            (root / f'{sequence}.reply').write_text(received.hex())
        os.write(1, f'\x1b]2;ACK{sequence}\x07'.encode())
        sequence += 1
    raise SystemExit('emitter deadline')


class Case:
    def __init__(self, binary, root):
        self.binary = str(Path(binary).resolve())
        self.root = root.resolve()
        self.root.mkdir(parents=True, exist_ok=False)
        self.inbox = self.root
        # Unix sockets must remain below sockaddr_un's native pathname bound.
        self.socket = self.root / 's'
        self.env = dict(os.environ, TERM='xterm-256color', LC_ALL='C',
                        RMUX_DISABLE_TMUX_FALLBACK='1')
        for key in ['TMUX', 'RMUX', 'RMUX_SOCKET', 'RMUX_SOCKET_PATH']:
            self.env.pop(key, None)
        self.seq = 0
        self.pids = []
        self.receipt = dict(binary=self.binary, sha256=sha(binary), commands=[], checks=[])

    def q(self, *args, check=True):
        argv = [self.binary, '-S', str(self.socket), '-f', '/dev/null', *args]
        p = subprocess.run(argv, env=self.env, capture_output=True, timeout=8)
        self.receipt['commands'].append(dict(argv=argv, code=p.returncode,
                                            out=p.stdout.decode(errors='replace'),
                                            err=p.stderr.decode(errors='replace')))
        if check and p.returncode:
            raise AssertionError(self.receipt['commands'][-1])
        return p.stdout.decode().rstrip('\n')

    def until(self, predicate):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.01)
        raise AssertionError('observation deadline')

    def equal(self, actual, expected, label):
        self.receipt['checks'].append(dict(label=label, actual=actual, expected=expected))
        assert actual == expected, self.receipt['checks'][-1]

    def start(self, cols=20, rows=8):
        self.command = shlex.join([sys.executable, str(Path(__file__).resolve()),
                                  '--emit', str(self.root)])
        self.q('new-session', '-d', '-s', 's', '-x', str(cols), '-y', str(rows), self.command)
        self.q('set', '-g', 'status', 'off')
        self.q('set', '-g', 'allow-rename', 'off')
        pid, version, socket = self.q('display-message', '-p', '#{pid}|#{version}|#{socket_path}').split('|')
        self.server = int(pid)
        expected = self.binary if Path(self.binary).name == 'tmux' else str(Path(self.binary).with_name('rmux-daemon'))
        self.equal(executable(pid), expected, 'live server executable')
        self.equal(os.path.realpath(socket), str(self.socket), 'owned socket')
        self.receipt.update(server_pid=pid, server_binary=expected,
                            server_sha256=sha(expected), version=version)
        self.pids.append(int(self.q('display-message', '-p', '#{pane_pid}')))
        self.until(lambda: self.q('display-message', '-p', '#{pane_title}') == 'READY')

    def feed(self, data, fragmented=False, reply=None):
        self.seq += 1
        chunks = [bytes([b]).hex() for b in data] if fragmented else [data.hex()]
        temp = self.inbox / 'pending'
        request = dict(chunks=chunks)
        if reply is not None:
            request['reply'] = reply.hex()
        temp.write_text(json.dumps(request))
        temp.rename(self.inbox / f'{self.seq}.json')
        self.until(lambda: self.q('display-message', '-p', '#{pane_title}') == f'ACK{self.seq}')
        if reply is not None:
            self.equal((self.inbox / f'{self.seq}.reply').read_text(), reply.hex(), 'native PTY replies')

    def capture(self, *args):
        return self.q('capture-pane', '-p', '-F', *args)

    def position(self):
        return self.q('display-message', '-p', '#{copy_cursor_x},#{copy_cursor_y},#{scroll_position}')

    def motion(self, command, expected, *args):
        self.q('send-keys', '-X', command, *args)
        self.equal(self.position(), expected, command + ' ' + ' '.join(args))

    def finish(self):
        self.q('kill-server', check=False)
        if hasattr(self, 'server'):
            self.until(lambda: executable(self.server) is None)
        self.until(lambda: all(executable(pid) is None for pid in self.pids))
        # Only this freshly created case's dead socket may be removed.
        if self.socket.exists():
            self.socket.unlink()
        self.receipt.update(cleanup=True, sha256_after=sha(self.binary))
        self.equal(self.receipt['sha256_after'], self.receipt['sha256'], 'client binary unchanged')
        if 'server_binary' in self.receipt:
            self.equal(sha(self.receipt['server_binary']), self.receipt['server_sha256'],
                       'server binary unchanged')


def basic(c):
    c.start()
    c.feed(b'\x1b]133;A\x07prompt\x1b]133;B\x1b\\ cmd\r\n'
           b'\x1b]133;C\x1b\\out\r\n\x1b]133;D;7\x07done\r\n'
           b'\x1b]133;A;aid=x\x07second\r\n\x1b]133;C\x07result\r\nend', True)
    c.equal(c.capture('-E', '5'), 'P prompt cmd\nO out\n- done\nP second\nO result\n- end', 'A/B/C/D and parameters')
    c.q('copy-mode')
    for command, expected, args in [
        ('previous-prompt', '0,3,0', []), ('previous-prompt', '0,0,0', []),
        ('previous-prompt', '0,0,0', []), ('next-prompt', '0,3,0', []),
        ('next-prompt', '0,4,0', ['-o']), ('previous-prompt', '0,1,0', ['-o']),
        ('previous-prompt', '0,1,0', ['-o']), ('next-prompt', '0,4,0', ['-o']),
        ('next-prompt', '0,4,0', ['-o']),
    ]:
        c.motion(command, expected, *args)
    source = c.root / 'commands.conf'
    source.write_text('send-keys -X previous-prompt\n')
    c.q('source-file', str(source))
    c.equal(c.position(), '0,3,0', 'source-file reaches same navigation consumer')


def lifecycle(c):
    c.start()
    c.feed(b'\x1b]133;A\x07main\r\n\x1b]133;C\x07output')
    c.feed(b'\x1b[?1049h\x1b[H\x1b]133;C\x07alternate')
    c.equal(c.capture('-E', '0'), 'O alternate', 'alternate has own marks')
    c.equal(c.capture('-a', '-E', '1'), 'P main\nO output', 'saved main marks')
    c.feed(b'\x1b[?1049l')
    c.equal(c.capture('-E', '1'), 'P main\nO output', 'main restored')
    c.feed(b'\x1b[!p')
    c.equal(c.capture('-E', '1'), 'P main\nO output', 'DECSTR preserves marks')
    c.feed(b'\x1bcreset')
    c.equal(c.capture('-E', '0'), '- reset', 'RIS clears visible marks')
    c.feed(b'\r\n\x1b]133;A\x07old')
    # Fresh inbox for replacement; old files are retained for evidence.
    child = c.root / 'respawn'
    child.mkdir()
    cmd = shlex.join([sys.executable, str(Path(__file__).resolve()), '--emit', str(child)])
    c.q('respawn-pane', '-k', cmd)
    c.pids.append(int(c.q('display-message', '-p', '#{pane_pid}')))
    c.until(lambda: c.q('display-message', '-p', '#{pane_title}') == 'READY')
    c.equal(c.capture('-E', '7'), '\n'.join(['- '] * 8), 'respawn clears visible marks and content')
    c.inbox, c.seq = child, 0
    c.feed(b'\x1b]133;A\x07replacement')
    c.equal(c.capture('-E', '0'), 'P replacement', 'new runtime accepts its own marks')
    child = c.root / 'window'
    child.mkdir()
    cmd = shlex.join([sys.executable, str(Path(__file__).resolve()), '--emit', str(child)])
    c.q('respawn-window', '-k', cmd)
    c.pids.append(int(c.q('display-message', '-p', '#{pane_pid}')))
    c.until(lambda: c.q('display-message', '-p', '#{pane_title}') == 'READY')
    c.equal(c.capture('-E', '7'), '\n'.join(['- '] * 8), 'respawn-window clears marks')


def reflow(c):
    c.start(cols=10)
    c.feed(b'\x1b]133;A\x07abcdefghijklmno\r\n\x1b]133;C\x07output\r\nend')
    c.equal(c.capture('-E', '3'), 'PW abcdefghij\n- klmno\nO output\n- end', 'wrapped prompt')
    c.equal(c.capture('-J', '-E', '3'), 'PW abcdefghij- klmno\nO output\n- end', 'flags prefix physical rows before join')
    c.q('resize-window', '-x', '20')
    c.equal(c.capture('-E', '2'), 'P abcdefghijklmno\nO output\n- end', 'grow joins marked prompt')
    c.q('resize-window', '-x', '5')
    c.equal('\n'.join(c.capture('-S', '-', '-E', '-').splitlines()[:6]), 'PW abcde\nW fghij\n- klmno\nOW outpu\n- t\n- end', 'shrink retains starts once')


def history(c):
    c.start(rows=4)
    c.feed(b'\x1b]133;A\x07p0\r\n\x1b]133;C\x07o0\r\nx\r\n'
           b'\x1b]133;A\x07p1\r\n\x1b]133;C\x07o1\r\nx\r\n'
           b'\x1b]133;A\x07p2\r\nend')
    c.equal(c.capture('-S', '-', '-E', '-'), 'P p0\nO o0\n- x\nP p1\nO o1\n- x\nP p2\n- end', 'scroll retains marks')
    c.q('copy-mode')
    c.motion('previous-prompt', '0,2,0')
    c.motion('previous-prompt', '0,0,1')
    c.motion('previous-prompt', '0,0,4')
    c.motion('next-prompt', '0,0,3', '-o')
    c.motion('next-prompt', '0,0,0', '-o')
    c.q('send-keys', '-X', 'cancel')
    c.q('clear-history')
    c.equal(c.capture('-S', '-', '-E', '-'), 'O o1\n- x\nP p2\n- end', 'clear-history removes only history marks')
    c.q('copy-mode')
    c.motion('previous-prompt', '0,2,0')
    c.motion('previous-prompt', '0,2,0')


def narrow(c):
    c.start(cols=4, rows=12)
    c.feed('\x1b]133;A\x07界abc\x1b]133;C\x07defgh'.encode())
    c.q('resize-window', '-x', '1')
    c.q('copy-mode')
    c.q('send-keys', '-X', 'previous-prompt', '-o')
    c.equal(c.q('display-message', '-p', '#{copy_cursor_line}'), 'c',
            'narrow wide glyph keeps output anchor with its content')
    c.q('send-keys', '-X', 'previous-prompt')
    glyph = '__' if Path(c.binary).name == 'tmux' else '界'
    c.equal(c.q('display-message', '-p', '#{copy_cursor_line}'), glyph,
            'narrow wide glyph keeps prompt anchor')


def blank_reflow(c):
    c.start(cols=4, rows=8)
    c.feed(b'abcdefgh\x1b[8;1H\x1b]133;C\x07\x1b[H')
    c.equal(c.capture('-S', '-', '-E', '-'),
            'W abcd\n- efgh\n- \n- \n- \n- \n- \nO ',
            'blank output mark before reflow')
    c.q('resize-window', '-x', '2')
    c.equal(c.capture('-S', '-', '-E', '-'),
            'W ab\nW cd\nW ef\n- gh\n- \n- \n- \n- \n- \nO ',
            'blank output mark survives shrink')
    c.q('resize-window', '-x', '4')
    c.equal(c.capture('-S', '-', '-E', '-'),
            'W abcd\n- efgh\n- \n- \n- \n- \n- \nO ',
            'blank output mark survives grow')


def capture_boundary(c):
    c.start(cols=8, rows=2)
    c.feed(b'\x1b]133;A\x07\x1b[31mabcdefghijkl\x1b[0m\r\n'
           b'\x1b[?1049h\x1b[H\x1b]133;C\x07\x1b[32mVIM\x1b[0m')
    c.equal(c.q('capture-pane', '-p', '-S', '-', '-E', '-', '-J'),
            'abcdefgh\nVIM', 'plain joined capture keeps alternate boundary')
    prompt = 'P' if Path(c.binary).name == 'tmux' else 'PW'
    expected = f'{prompt} abcdefgh\nO VIM\n- '
    c.equal(c.q('capture-pane', '-p', '-S', '-', '-E', '-', '-F'),
            expected, 'formatted physical capture')
    c.equal(c.q('capture-pane', '-p', '-S', '-', '-E', '-', '-F', '-J'),
            expected, 'formatted join keeps alternate boundary')
    ansi = (f'{prompt} \x1b[31mabcdefgh\nO \x1b[32mVIM\n- '
            if Path(c.binary).name == 'tmux'
            else f'{prompt} \x1b[31mabcdefgh\x1b[39m\n'
                 'O \x1b[32mVIM\x1b[39m\n- ')
    c.equal(c.q('capture-pane', '-p', '-S', '-', '-E', '-', '-e', '-F', '-J'),
            ansi, 'ANSI formatted join keeps alternate boundary')
    c.equal(c.q('capture-pane', '-p', '-S', '-1', '-E', '-1', '-F', '-J'),
            f'{prompt} abcdefgh', 'history-only range does not cross boundary')
    c.equal(c.q('capture-pane', '-p', '-S', '0', '-E', '0', '-F', '-J'),
            'O VIM', 'alternate-only range does not cross boundary')
    c.equal(c.q('capture-pane', '-p', '-S', '-1', '-E', '0', '-F', '-J'),
            f'{prompt} abcdefgh\nO VIM', 'bounded range keeps alternate boundary')
    c.equal(c.q('capture-pane', '-p', '-a', '-S', '0', '-E', '1', '-F', '-J'),
            '- ijkl\n- ', 'saved capture remains continuous')
    c.q('copy-mode')
    c.q('send-keys', '-X', 'history-top')
    mode_expected = ('O VIM\n- ' if Path(c.binary).name == 'tmux'
                     else 'PW abcdefgh\nO VIM')
    c.equal(c.q('capture-pane', '-p', '-M', '-S', '0', '-E', '1', '-F', '-J'),
            mode_expected, 'copy-mode projection keeps its measured boundary')


def mutations(c):
    c.start(rows=4)
    c.feed(b'\x1b]133;A\x07p\r\n\x1b]133;C\x07o\r\nend')
    c.feed(b'\x1b[H\x1b[L')
    c.equal(c.capture('-E', '3'), '- \nP p\nO o\n- end', 'insert-line moves metadata')
    c.feed(b'\x1b[M')
    c.equal(c.capture('-E', '2'), 'P p\nO o\n- end', 'delete-line moves metadata')
    c.feed(b'\x1b[H\x1b[2K')
    c.equal(c.capture('-E', '0'), '- ', 'whole line erase clears metadata')
    c.feed(b'\x1b[2;1H\x1b[X')
    c.equal(c.capture('-S', '1', '-E', '1'), 'O ', 'partial erase retains line mark')
    c.feed(b'\x1b[2;3r\x1b[3;1H\n')
    c.equal(c.capture('-E', '2'), '- \n- end\n- ', 'partial scroll region drops removed mark')
    for label, seq in [('DCH', b'\x1b[99P'), ('ECH', b'\x1b[99X'), ('EL0', b'\x1b[K')]:
        c.feed(b'\x1bc\x1b]133;A\x07hello\r' + seq)
        c.equal(c.capture('-E', '0'), '- ', label + ' whole row clears mark')


def divergence(c):
    c.start(cols=5)
    c.feed(b'abcdef\x1b]133;C\x07ghijZ')
    c.equal(c.capture('-E', '2'), 'W abcde\nOW fghij\n- Z', 'continuation mark before grow')
    c.q('resize-window', '-x', '10')
    expected = 'W abcdefghij\n- Z' if Path(c.binary).name == 'tmux' else 'OW abcdefghij\n- Z'
    c.equal(c.capture('-E', '1'), expected, 'measured tmux mark loss; RMUX preserves at containing row')
    c.feed(b'\x1bc\x1b]133;Apple\x07x\r\n\x1b]133;Cgarbage\x07y')
    expected = 'P x\nO y' if Path(c.binary).name == 'tmux' else '- x\n- y'
    c.equal(c.capture('-E', '1'), expected, 'RMUX requires complete command token')
    c.feed(b'\x1bc\x1b]133;A\x18\x1b]133;C\x1a')
    expected = 'OP ' if Path(c.binary).name == 'tmux' else '- '
    c.equal(c.capture('-E', '0'), expected, 'CAN/SUB cancel OSC133 in RMUX')


def neighbours(c):
    c.start()
    c.q('set', '-g', 'set-clipboard', 'on')
    # tmux quantizes colours to 8-bit channels and this oracle build does not
    # implement Kitty negotiation. Preserve the measured RMUX base contract.
    reply = (b'' if Path(c.binary).name == 'tmux' else b'\x1b[?8u')
    reply += (b'\x1b]10;rgb:1212/5656/9a9a\x07' if Path(c.binary).name == 'tmux'
              else b'\x1b]10;rgb:1234/5678/9abc\x07')
    reply += b'\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]12;rgb:aaaa/bbbb/cccc\x07'
    c.feed(b'\x1b]133;A\x07p\x1b]10;rgb:1234/5678/9abc\x07'
           b'\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]12;rgb:aaaa/bbbb/cccc\x07'
           b'\x1b]52;c;T0sxMzM=\x07\x1b]133;C\x07\x1b[=8u\x1b[?u'
           b'\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07',
           reply=reply)
    c.equal(c.capture('-E', '0'), 'OP p', 'OSC133 coexists with neighbours')
    # Clipboard alerts drain asynchronously after the parser's title/replies,
    # on both the RMUX base and candidate. Observe the actual buffer consumer.
    c.until(lambda: c.q('show-buffer', check=False) == 'OK133')
    c.equal(c.q('show-buffer'), 'OK133', 'OSC52 daemon buffer consumer')


CASES = dict(basic=basic, lifecycle=lifecycle, reflow=reflow, history=history,
             mutations=mutations, divergence=divergence, neighbours=neighbours, narrow=narrow,
             blank_reflow=blank_reflow, capture_boundary=capture_boundary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary')
    parser.add_argument('--out', type=Path)
    parser.add_argument('--case', choices=CASES)
    parser.add_argument('--emit')
    args = parser.parse_args()
    if args.emit:
        return emitter(args.emit)
    args.out.mkdir(parents=True, exist_ok=False)
    results = {}
    for name, fn in CASES.items():
        if args.case and name != args.case:
            continue
        short_root = dict(blank_reflow='br', capture_boundary='cb').get(name, name)
        c = Case(args.binary, args.out / short_root)
        try:
            fn(c)
            results[name] = 'passed'
        except Exception:
            results[name] = traceback.format_exc()
        finally:
            try:
                c.finish()
            except Exception:
                results[name] = results.get(name, '') + '\ncleanup: ' + traceback.format_exc()
            (c.root / 'receipt.json').write_text(json.dumps(c.receipt, indent=2) + '\n')
        print(name, results[name], flush=True)
    (args.out / 'result.json').write_text(json.dumps(dict(cases=results, harness_sha256=sha(__file__)), indent=2) + '\n')
    return int(any(value != 'passed' for value in results.values()))


if __name__ == '__main__':
    sys.exit(main())
