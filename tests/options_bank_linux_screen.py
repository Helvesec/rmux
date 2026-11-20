"""Observe the attached client's real terminal stream for option recipes."""
import time
import re


def requires_client(plan):
    return any(row.get('attach_client') or any(key in row for key in ('client_screen', 'client_control', 'client_input', 'client_event', 'add_client')) for row in plan)


def spans(text, item):
    if 'text_pattern' in item:
        return [(match.start(), match.end()) for match in re.finditer(item['text_pattern'], text)]
    needle = item['text']
    return [(start, start + len(needle)) for start in range(len(text)) if text.startswith(needle, start)]


def observe(raw, expected):
    import pyte
    class Screen(pyte.Screen):
        def report_device_status(self, mode, **kwargs):
            # This offline observer never replies to the product's DSR query.
            return None

    screen = Screen(80, 24)
    stream = pyte.ByteStream(screen)
    stream.feed(raw)
    result = []
    for item in expected:
        rows = [item['row']] if 'row' in item else range(24)
        found = []
        for y in rows:
            text = screen.display[y]
            for start, end in spans(text, item):
                chars = [screen.buffer[y][x] for x in range(start, end)]
                if ('column' not in item or start == item['column']) and all(all(getattr(char, key) == value for key, value in item.items()
                           if key not in ('row', 'text', 'text_pattern', 'column', 'absent', 'colon_count')) for char in chars):
                    if 'colon_count' not in item or text[start:end].count(':') == item['colon_count']:
                        found.append(y)
        if bool(found) == bool(item.get('absent', False)):
            raise RuntimeError('attached screen consumer: ' + repr(item))
        result.append(dict(item))
    return result


def capture(run, row, command):
    from options_bank_linux import pin
    end = min(time.monotonic() + 5, run.deadline)
    path = run.root / ('screen-' + str(len(run.record['commands'])) + '.pty')
    while True:
        run.drain()
        try:
            if 'client_control' in row:
                matches = re.findall(row['client_control'].encode(), bytes(run.pty_bytes[run.client_mark:]))
                if not matches:
                    raise RuntimeError('terminal control consumer: ' + row['client_control'])
                observed = matches[-1].decode()
            else:
                observed = observe(bytes(run.pty_bytes), row['client_screen'])
            break
        except RuntimeError:
            if time.monotonic() >= end:
                path.write_bytes(run.pty_bytes)
                raise
            time.sleep(.025)
    path.write_bytes(run.pty_bytes)
    command.update(screen_stream=pin(path))
    if 'client_control' in row:
        command.update(client_control=observed, client_mark=run.client_mark)
    else:
        command['screen_observation'] = observed


def prompt_ready(run, argv):
    from options_bank_linux import require
    require(argv[0] == 'command-prompt' and '-p' in argv, 'prompt readiness target')
    prefix = argv[argv.index('-p') + 1]

    def ready():
        try:
            observe(bytes(run.pty_bytes), [dict(text=prefix, row=23)])
            return True
        except RuntimeError:
            return False

    run.wait(ready)


def verify(command, row, root):
    from pathlib import Path
    from options_bank_linux import authenticate, require
    artifact = command['screen_stream']
    require(Path(artifact['path']).parent == Path(root), 'screen stream target')
    authenticate(artifact)
    if 'client_control' in row:
        raw = Path(artifact['path']).read_bytes()[command['client_mark']:]
        matches = re.findall(row['client_control'].encode(), raw)
        require(bool(matches) and matches[-1].decode() == command['client_control'], 'terminal control replay')
    else:
        expected = observe(Path(artifact['path']).read_bytes(), row['client_screen'])
        require(command['screen_observation'] == expected, 'screen replay mismatch')


def retain_consumer(run, row):
    from options_bank_linux import children, describe, sha
    parent = run.children['daemon'].pid

    def consumer():
        known = {handle[2]['pid'] for handle in run.handles.values()}
        for pid in children(parent):
            if pid in known:
                continue
            if describe(pid)['argv'] == row.get('consumer_argv', ['/bin/sleep', '240']):
                label = row.get('consumer_label', row['role'].split(':')[0])
                run.hold('consumer-' + label, pid, sha(row.get('consumer_exe', '/bin/sleep')))
                return True
        return False

    run.wait(consumer)


def inject(run, frames):
    import os
    from options_bank_linux import require
    for frame in frames:
        payload = bytes.fromhex(frame['hex'])
        require(len(payload) <= 4096, 'bounded client input')
        require(os.write(run.master, payload) == len(payload), 'client input short write')
        time.sleep(frame.get('after_ms', 0) / 1000)
        run.drain()


def event(run, row, command):
    from options_bank_linux import pin, require
    expected = row['client_event']
    time.sleep(.5)
    run.drain()
    raw = bytes(run.pty_bytes[run.client_mark:])
    present = bytes.fromhex(expected['hex']) in raw
    path = run.root / ('event-' + str(len(run.record['commands'])) + '.pty')
    path.write_bytes(raw)
    command.update(event_stream=pin(path), client_event=dict(hex=expected['hex'], present=present))
    require(command['client_event'] == expected, 'attached terminal event')


def verify_event(command, row, root):
    from pathlib import Path
    from options_bank_linux import authenticate, require
    artifact = command['event_stream']
    require(Path(artifact['path']).parent == Path(root), 'terminal event target')
    authenticate(artifact)
    expected = row['client_event']
    actual = dict(hex=expected['hex'], present=bytes.fromhex(expected['hex']) in Path(artifact['path']).read_bytes())
    require(actual == command['client_event'] == expected, 'terminal event replay')


def add_client(run, specification):
    import os
    import fcntl
    import termios
    import struct
    import subprocess
    from options_bank_linux import require
    require(specification == dict(label='sized', session='other', cols=120, rows=40), 'bounded auxiliary client')
    master, slave = os.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', specification['rows'], specification['cols'], 0, 0))

    def tty_session():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    label = 'attach-' + specification['label']
    argv = [run.binary, '-N', '-S', str(run.socket), 'attach-session', '-t', specification['session']]
    try:
        child = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave, cwd=run.root, env=run.env, preexec_fn=tty_session)
    finally:
        os.close(slave)
    if not hasattr(run, 'auxiliary_clients'):
        run.auxiliary_clients = {}
    run.auxiliary_clients[label] = (master, bytearray())
    run.children[label] = child
    run.hold(label, child.pid, run.record['binary']['sha256'])
    run.wait(lambda: bool(run.auxiliary_clients[label][1]))
