#!/usr/bin/env python3
"""Bounded native Darwin supplement to the accepted Linux input reference.

Only P1/P2/P3/E1, with the oracle frozen before RMUX. No performance claim.
The reference and helper are supplied as byte-pinned private copies.
"""
import argparse
import ctypes as C
from datetime import datetime, timezone
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import select
import secrets
import shlex
import signal
import stat
import struct
import subprocess
import sys
import termios
import time
import tty

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
sys.path.insert(0, str(ROOT / 'scripts/bench'))
import darwin_driver as native
import darwin_terminal_restore as restoration

REFERENCE_SHA = 'bbd5fdfe91ac615becb27b23652af05c4ad603b5c0006cd8ae28b52476ab48bd'
HELPER_SHA = '389da8879380ee4d2f91afa241aa058b4d06b438e018f5351e93062e9a40e2dd'
CONTRACT_SHA = '47d8e3e8b7ec78c2422e6da9bbcf408c10506e386fa11555028cbe5724b643d7'
RUNTIME_SHA = '8ea6218f35e348a7c77794003527e6d11072f29e'


def now():
    return datetime.now(timezone.utc).isoformat()


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def save(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(json.dumps(value, indent=2, default=lambda item: item.hex()
                                    if isinstance(item, bytes) else str(item)) + '\n')
    temporary.replace(path)


def attributes(value):
    return [*value[:6], [x.hex() if isinstance(x, bytes) else x for x in value[6]]]


def slave_identity(fd):
    info = os.fstat(fd)
    return {'dev': info.st_dev, 'ino': info.st_ino, 'rdev': info.st_rdev,
            'mode': info.st_mode, 'fd': fd, 'path': os.ttyname(fd)}


# Darwin SDK sys/proc_info.h; exact return sizes are mandatory.
class BSDInfo(C.Structure):
    _fields_ = [(n, C.c_uint32) for n in (
        'flags', 'status', 'xstatus', 'pid', 'ppid', 'uid', 'gid', 'ruid',
        'rgid', 'svuid', 'svgid', 'reserved')]
    _fields_ += [('comm', C.c_char * 16), ('name', C.c_char * 32)]
    _fields_ += [(n, C.c_uint32) for n in ('nfiles', 'pgid', 'jobc', 'tdev', 'tpgid')]
    _fields_ += [('nice', C.c_int32), ('start_sec', C.c_uint64), ('start_usec', C.c_uint64)]


class FileInfo(C.Structure):
    _fields_ = [('openflags', C.c_uint32), ('status', C.c_uint32),
                ('offset', C.c_int64), ('type', C.c_int32), ('guardflags', C.c_uint32)]


class VStat(C.Structure):
    _fields_ = [('dev', C.c_uint32), ('mode', C.c_uint16), ('nlink', C.c_uint16),
                ('ino', C.c_uint64), ('uid', C.c_uint32), ('gid', C.c_uint32)]
    _fields_ += [(n, C.c_int64) for n in (
        'atime', 'atimensec', 'mtime', 'mtimensec', 'ctime', 'ctimensec',
        'birthtime', 'birthtimensec', 'size', 'blocks')]
    _fields_ += [('blksize', C.c_int32), ('flags', C.c_uint32),
                ('gen', C.c_uint32), ('rdev', C.c_uint32), ('spare', C.c_int64 * 2)]


class VNode(C.Structure):
    _fields_ = [('info', VStat), ('type', C.c_int32), ('pad', C.c_int32),
                ('fsid', C.c_int32 * 2)]


class FDPath(C.Structure):
    _fields_ = [('file', FileInfo), ('vnode', VNode), ('path', C.c_char * 1024)]


LIB = C.CDLL('/usr/lib/libproc.dylib', use_errno=True)
LIB.proc_pidinfo.argtypes = [C.c_int, C.c_int, C.c_uint64, C.c_void_p, C.c_int]
LIB.proc_pidinfo.restype = C.c_int
LIB.proc_pidpath.argtypes = [C.c_int, C.c_void_p, C.c_uint32]
LIB.proc_pidpath.restype = C.c_int
LIB.proc_pidfdinfo.argtypes = [C.c_int, C.c_int, C.c_int, C.c_void_p, C.c_int]
LIB.proc_pidfdinfo.restype = C.c_int


def bsd(pid):
    value = BSDInfo()
    C.set_errno(0)
    size = LIB.proc_pidinfo(pid, 3, 0, C.byref(value), C.sizeof(value))
    if size == 0 and C.get_errno() == errno.ESRCH:
        return None
    if size != C.sizeof(value):
        raise OSError(C.get_errno(), f'proc_pidinfo size {size}, expected {C.sizeof(value)}')
    if value.pid != pid:
        raise RuntimeError('native PID identity mismatch')
    return value


def identity(pid):
    first = bsd(pid)
    if first is None:
        return None
    buffer = C.create_string_buffer(4096)
    C.set_errno(0)
    count = LIB.proc_pidpath(pid, buffer, len(buffer))
    if count <= 0:
        raise OSError(C.get_errno(), 'proc_pidpath failed for known process')
    second = bsd(pid)
    if second is None or (first.start_sec, first.start_usec) != (second.start_sec, second.start_usec):
        raise RuntimeError('process changed during identity observation')
    executable = os.fsdecode(buffer.value)
    return {'pid': pid, 'ppid': first.ppid, 'birth': [first.start_sec, first.start_usec],
            'pgid': first.pgid, 'status': first.status, 'executable': executable,
            'sha256': sha(executable)}


def fd_identity(pid, fd):
    value = FDPath()
    C.set_errno(0)
    size = LIB.proc_pidfdinfo(pid, fd, 2, C.byref(value), C.sizeof(value))
    if size != C.sizeof(value):
        raise OSError(C.get_errno(), f'proc_pidfdinfo size {size}, expected {C.sizeof(value)}')
    info = value.vnode.info
    return {'path': os.fsdecode(value.path), 'dev': info.dev, 'ino': info.ino,
            'rdev': info.rdev, 'mode': info.mode, 'returned_size': size}


def raw_tty(pid, path):
    descriptor = fd_identity(pid, 0)
    fd = os.open(path, os.O_RDONLY | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        actual = termios.tcgetattr(fd)
        if (descriptor['dev'], descriptor['ino'], descriptor['rdev']) != (info.st_dev, info.st_ino, info.st_rdev):
            raise RuntimeError('application fd0 does not match the owned tty')
        if not stat.S_ISCHR(info.st_mode):
            raise RuntimeError('application fd0 is not a character device')
        expected = list(actual)
        expected[6] = list(actual[6])
        # Use the local Python/Darwin raw transformation; no Linux numeric flags.
        tty.cfmakeraw(expected)
        if actual != expected:
            raise RuntimeError('application tty is not raw')
        return {'fd0': descriptor, 'tty': path, 'attributes': attributes(actual), 'raw': True}
    finally:
        os.close(fd)


def event(queue, timeout):
    if timeout <= 0:
        raise TimeoutError('native observation deadline')
    events = queue.control(None, 8, timeout)
    for item in events:
        if item.flags & select.KQ_EV_ERROR:
            raise OSError(item.data, os.strerror(item.data))
    return events


def notify(root, value):
    write_fifo(Path(root) / 'events', value)


def write_fifo(path, value):
    fd = os.open(path, os.O_WRONLY | os.O_NONBLOCK)
    try:
        if os.write(fd, value) != len(value):
            raise RuntimeError('incomplete private FIFO frame')
    finally:
        os.close(fd)


def application(root, aware, ready_path, challenge):
    root = Path(root)
    before = termios.tcgetattr(0)
    tty.setraw(0)
    signal.signal(signal.SIGHUP, lambda *_: sys.exit(0))
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    queue = select.kqueue()
    gate = os.open(root / 'emit', os.O_RDWR | os.O_NONBLOCK)
    queue.control([select.kevent(gate, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_ADD)], 0, 0)
    try:
        save(root / 'application-start.json', {
            'at': now(), 'identity': identity(os.getpid()), 'tty': os.ttyname(0),
            'raw': raw_tty(os.getpid(), os.ttyname(0)), 'bracketed_aware': aware,
            'challenge': challenge})
        notify(root, b's')
        if not event(queue, 20) or os.read(gate, 64) != b'emit\n':
            raise RuntimeError('invalid application emission gate')
        (root / 'application-input.bin').write_bytes(b'')
        (root / 'application-reads.jsonl').write_text('')
        output = (b'\x1b[?2004h' if aware else b'\x1b[?2004l')
        output += b'INPUT READY\r\n\x1b]2;INPUT-READY\x07'
        os.write(1, output)
        save(root / 'application-ready.json', {'at': now(), 'output_hex': output.hex()})
        write_fifo(ready_path, (challenge + '\n').encode())
        queue.control([select.kevent(gate, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_DELETE),
                       select.kevent(0, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_ADD)], 0, 0)
        with (root / 'application-input.bin').open('ab', buffering=0) as sink:
            while event(queue, 20):
                data = os.read(0, 65536)
                if not data:
                    break
                read_ns = time.monotonic_ns()
                with (root / 'application-reads.jsonl').open('a') as log:
                    log.write(json.dumps({'read_ns': read_ns, 'hex': data.hex()}) + '\n')
                sink.write(data)
                notify(root, b'd')
    finally:
        termios.tcsetattr(0, termios.TCSANOW, before)
        os.close(gate)
        queue.close()
        save(root / 'application-end.json', {'at': now()})


def child_terminal():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def drain(fd, output):
    while True:
        try:
            data = os.read(fd, 65536)
        except BlockingIOError:
            return
        except OSError as error:
            if error.errno == errno.EIO:
                return
            raise
        if not data:
            return
        output.extend(data)
        if len(output) > 2 * 1024 * 1024:
            raise RuntimeError('client output exceeds 2 MiB')


def drained_exit(watcher, master, output, timeout):
    """Darwin terminal exit must keep draining the master, including detach output."""
    queue = select.kqueue()
    deadline = time.monotonic() + timeout
    try:
        drain(master, output)
        if watcher.poll_exit():
            return
        try:
            queue.control([select.kevent(master, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_ADD),
                           select.kevent(watcher.pid, filter=select.KQ_FILTER_PROC,
                                         flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                         fflags=select.KQ_NOTE_EXIT)], 0, 0)
        except ProcessLookupError:
            watcher.wait(max(.001, deadline - time.monotonic()))
            return
        while not watcher.poll_exit():
            drain(master, output)
            event(queue, deadline - time.monotonic())
        drain(master, output)
    finally:
        queue.close()


def terminal_host(root, specification):
    """Own the controlling terminal until restoration has been inspected.

    Darwin invalidates tty ioctls when its session leader exits. This private
    host keeps the session alive while the actual client exits normally. It is
    the sole reaper of that client; the outer driver reaps only this host.
    """
    spec = json.loads(Path(specification).read_text())
    child = subprocess.Popen(spec['argv'], env=spec.get('env'))
    watcher = native.FixtureProcess(child.pid)
    queue = select.kqueue()
    release = os.open(root / 'host-release', os.O_RDWR | os.O_NONBLOCK)
    queue.control([select.kevent(release, filter=select.KQ_FILTER_READ,
                                 flags=select.KQ_EV_ADD)], 0, 0)
    try:
        save(root / 'client-start.json', {'identity': identity(child.pid),
                                         'host': identity(os.getpid()), 'argv': spec['argv']})
        notify(root, b'c')
        watcher.wait(32)
        code = child.wait()
        save(root / 'client-end.json', {'at': now(), 'code': code,
                                       'exit_event': watcher.exited,
                                       'reaped': child.returncode is not None,
                                       'termios': attributes(termios.tcgetattr(0))})
        notify(root, b'e')
        if not event(queue, 12):
            raise RuntimeError('terminal host release deadline')
        command = os.read(release, 64)
        if command == b'service\n':
            host_service(root)
            if not event(queue, 12):
                raise RuntimeError('terminal host release after service deadline')
            command = os.read(release, 64)
        if command != b'close\n':
            raise RuntimeError('invalid terminal host command')
    finally:
        try:
            if not watcher.poll_exit():
                watcher.retire_after_failure(3)
            child.wait()
        finally:
            watcher.close()
            queue.close()
            os.close(release)


def host_service(root):
    """The first slave availability observer after the client has been reaped.

    Only a FIFO command reaches this function. No terminal configuration write,
    flush, reopen, FIONREAD, or retry occurs here or in restore_terminal.
    """
    request = json.loads((root / 'service-request.json').read_text())
    deadline_ns = request['deadline_ns']
    queue = select.kqueue()
    ack = {'read_calls': 0, 'read_limit': 64, 'error': None}
    try:
        first = {'at': now(), 'operation': 'kqueue EVFILT_READ add on inherited fd0',
                 'slave': slave_identity(0), 'host': identity(os.getpid())}
        first['first_observer_ns'] = time.monotonic_ns()
        queue.control([select.kevent(0, filter=select.KQ_FILTER_READ,
                                     flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT)], 0, 0)
        first['observer_registered_ns'] = time.monotonic_ns()
        save(root / 'service-first-observer.json', first)
        save(root / 'service-ready.json', {'ready_ns': time.monotonic_ns()})
        notify(root, b'r')
        if not event(queue, (deadline_ns - time.monotonic_ns()) / 1e9):
            raise TimeoutError('terminal service read readiness')
        ack['read_calls'] = 1
        ack['read_hex'] = os.read(0, 64).hex()
        ack['read_ns'] = time.monotonic_ns()
    except Exception as error:
        ack['error'] = failure(error)
    finally:
        queue.close()
        ack['ack_ns'] = time.monotonic_ns()
        save(root / 'service-ack.json', ack)
        notify(root, b'a')


def restoration_client(root):
    """Calibration only: raw-to-canonical transition, with no slave read observer."""
    before = termios.tcgetattr(0)
    raw = list(before)
    raw[6] = list(before[6])
    tty.cfmakeraw(raw)
    gate = os.open(root / 'fixture-restore', os.O_RDWR | os.O_NONBLOCK)
    queue = select.kqueue()
    try:
        queue.control([select.kevent(gate, filter=select.KQ_FILTER_READ,
                                     flags=select.KQ_EV_ADD)], 0, 0)
        termios.tcsetattr(0, termios.TCSANOW, raw)
        os.write(1, b'\x1b[?2004h')
        save(root / 'fixture-raw.json', {'identity': identity(os.getpid()),
                                       'attached': attributes(termios.tcgetattr(0)),
                                       'slave_observers': []})
        notify(root, b'f')
        if not event(queue, 10) or os.read(gate, 64) != b'restore\n':
            raise RuntimeError('calibration restore gate failed')
        # This is the simulated client's sole normal restore, not a repair by
        # the observer; it happens before exit and before the raw-after snapshot.
        termios.tcsetattr(0, termios.TCSANOW, before)
        os.write(1, b'\x1b[?2004l')
    finally:
        queue.close()
        os.close(gate)


def restore_terminal(root, master, slave, before, attached, expected_slave,
                     expected_host, product, client_end, cleanup_stop, marker, behavior_complete=True):
    """A–F on the existing owned terminal, preserving every observation on failure."""
    record = {'contract_sha256': restoration.CONTRACT_SHA, 'pass': False,
              'before': before, 'attached': attached, 'slave_before': expected_slave,
              'slave_observers_before_snapshot': [], 'client_exit_event': client_end['exit_event'],
              'client_reaped': client_end['reaped'], 'client_code': client_end['code']}
    echo = bytearray()
    queue = None
    events_fd = None
    try:
        drain(master, product)
        (root / 'product-output.bin').write_bytes(product)
        record['product_output'] = {'sha256': sha(root / 'product-output.bin'), 'bytes': len(product)}
        record['product_saved_ns'] = time.monotonic_ns()
        # Snapshot precedes *all* new input servicing, even when another check fails.
        record['raw_after'] = attributes(termios.tcgetattr(slave))
        record['raw_snapshot_ns'] = time.monotonic_ns()
        record['historical_strict_equal'] = record['raw_after'] == before
        record['flag_xor'] = [a ^ b for a, b in zip(before[:4], record['raw_after'][:4])]
        save(root / 'restoration-raw.json', record)
        restoration.admit(before, attached)
        restoration.require(behavior_complete, 'product bytes/silence incomplete; service forbidden')
        record['outer_mode_2004'] = restoration.outer_mode(product)
        record['host_at_raw'] = identity(expected_host['pid'])
        record['slave_at_raw'] = slave_identity(slave)
        restoration.same_identity(record['host_at_raw'], expected_host)
        restoration.same_slave(record['slave_at_raw'], expected_slave)
        restoration.same_slave(fd_identity(expected_host['pid'], 0), expected_slave)
        record['classification'] = restoration.classify(before, attached, record['raw_after'])
        record['classification_ns'] = time.monotonic_ns()
        restoration.require(client_end['exit_event'] and client_end['reaped'] and client_end['code'] == 0,
                            'client exit/reap invalid before service')
        marker = restoration.marker_bytes(marker)
        # Only the events FIFO is observed while awaiting the host's out-of-band ack.
        # Echo stays on the master until the single drain following that ack.
        queue = select.kqueue()
        events_fd = os.open(root / 'events', os.O_RDWR | os.O_NONBLOCK)
        queue.control([select.kevent(events_fd, filter=select.KQ_FILTER_READ,
                                     flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR)], 0, 0)
        record['command_ns'] = time.monotonic_ns()
        record['cleanup_deadline_ns'] = int(cleanup_stop * 1e9)
        deadline_ns = min(record['command_ns'] + 5_000_000_000, record['cleanup_deadline_ns'])
        record['service_deadline_ns'] = deadline_ns
        save(root / 'service-request.json', {'deadline_ns': deadline_ns})
        write_fifo(root / 'host-release', b'service\n')

        def until_receipt(name):
            while not (root / name).exists():
                event(queue, (deadline_ns - time.monotonic_ns()) / 1e9)
                try:
                    os.read(events_fd, 4096)
                except BlockingIOError:
                    pass
            restoration.require(time.monotonic_ns() <= deadline_ns, 'service receipt too late')
            return json.loads((root / name).read_text())

        ready = until_receipt('service-ready.json')
        first = json.loads((root / 'service-first-observer.json').read_text())
        record.update(first_observer_ns=first['first_observer_ns'], ready_ns=ready['ready_ns'])
        record['first_observer'] = first
        restoration.same_identity(first['host'], expected_host)
        restoration.same_slave(first['slave'], expected_slave)
        record['marker_hex'] = marker.hex()
        record['write_before_ns'] = time.monotonic_ns()
        record['write_calls'] = 1
        record['write_bytes'] = os.write(master, marker)
        record['write_after_ns'] = time.monotonic_ns()
        ack = until_receipt('service-ack.json')
        record['ack'] = ack
        record['ack_ns'] = ack['ack_ns']
        record['read_ns'] = ack.get('read_ns')
        drain(master, echo)
        record['after_service'] = attributes(termios.tcgetattr(slave))
        record['final_snapshot_ns'] = time.monotonic_ns()
        record['host_at_final'] = identity(expected_host['pid'])
        record['slave_at_final'] = slave_identity(slave)
        restoration.same_identity(record['host_at_final'], expected_host)
        restoration.same_slave(record['slave_at_final'], expected_slave)
        restoration.same_slave(fd_identity(expected_host['pid'], 0), expected_slave)
        restoration.effects(before, marker, ack, bytes(echo), record['after_service'])
        restoration.boundary(record)
        record['pass'] = True
    except Exception as error:
        record['error'] = failure(error)
        raise
    finally:
        if queue is not None:
            queue.close()
        if events_fd is not None:
            os.close(events_fd)
        (root / 'restoration-echo.bin').write_bytes(echo)
        record['echo_hex'] = echo.hex()
        record['echo_sha256'] = sha(root / 'restoration-echo.bin')
        save(root / 'restoration.json', record)
    return record


def calibration(root):
    """Own Python child/PTY only. This never starts tmux or RMUX."""
    root.mkdir(parents=True, mode=0o700, exist_ok=False)
    report = {'start': now(), 'product_execution': False, 'self': identity(os.getpid()),
              'abi_sizes': {'bsd': C.sizeof(BSDInfo), 'fdpath': C.sizeof(FDPath)}}
    for name in ('events', 'emit', 'host-release'):
        os.mkfifo(root / name, 0o600)
    events = os.open(root / 'events', os.O_RDWR | os.O_NONBLOCK)
    queue = select.kqueue()
    queue.control([select.kevent(events, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_ADD)], 0, 0)
    master, slave = os.openpty()
    os.set_blocking(master, False)
    output = bytearray()
    original = termios.tcgetattr(slave)
    process = watcher = child_watch = None

    def until_file(name):
        deadline = time.monotonic() + 5
        while not (root / name).exists():
            drain(master, output)
            event(queue, deadline - time.monotonic())
            try:
                os.read(events, 4096)
            except BlockingIOError:
                pass
        return json.loads((root / name).read_text())

    try:
        with native.Readiness() as ready:
            command = [sys.executable, '-B', str(Path(__file__).resolve()), '--application', str(root),
                       '--ready-path', str(ready.path), '--challenge', ready.challenge, '--aware']
            save(root / 'launch.json', {'argv': command})
            process = subprocess.Popen([sys.executable, '-B', str(Path(__file__).resolve()),
                                        '--terminal-host', str(root), '--specification', str(root / 'launch.json')],
                                       stdin=slave, stdout=slave, stderr=slave, preexec_fn=child_terminal)
            watcher = native.FixtureProcess(process.pid)
            report['terminal_host'] = until_file('client-start.json')
            report['application'] = until_file('application-start.json')
            pid = report['application']['identity']['pid']
            assert pid == report['terminal_host']['identity']['pid']
            child_watch = native.FixtureProcess(pid)
            report['raw'] = raw_tty(pid, os.ttyname(slave))
            write_fifo(root / 'emit', b'emit\n')
            ready.wait(5)
            payload = b'raw\n\xc3\xa9\t\x1b'
            os.write(master, payload)
            deadline = time.monotonic() + 5
            while len((root / 'application-input.bin').read_bytes()) < len(payload):
                event(queue, deadline - time.monotonic())
                try:
                    os.read(events, 4096)
                except BlockingIOError:
                    pass
            report['received_hex'] = (root / 'application-input.bin').read_bytes().hex()
            assert report['received_hex'] == payload.hex()
            os.kill(pid, signal.SIGHUP)
            drained_exit(child_watch, master, output, 5)
            report['child_end'] = until_file('client-end.json')
            report['termios_restored'] = termios.tcgetattr(slave) == original
            assert report['child_end']['code'] == 0 and report['termios_restored']
            write_fifo(root / 'host-release', b'close\n')
            drained_exit(watcher, master, output, 5)
            report['host_exit'] = process.wait()
            assert report['host_exit'] == 0
            report['pass'] = True
    finally:
        # Closing our PTY is an emergency release only, never a PASS.
        for fd in (master, slave):
            os.close(fd)
        for watched in (child_watch, watcher):
            if watched is not None:
                try:
                    if not watched.poll_exit():
                        watched.retire_after_failure(3)
                finally:
                    watched.close()
        if process is not None:
            process.wait()
        queue.close()
        os.close(events)
        report['end'] = now()
        report['client_output_hex'] = output.hex()
        save(root / 'calibration.json', report)
    return report


def load_reference(directory):
    path = directory / 'input_paste_escape_reference.py'
    helper = directory / 'g6_selection_copy_reference.py'
    if sha(path) != REFERENCE_SHA or sha(helper) != HELPER_SHA:
        raise RuntimeError('accepted reference bytes differ')
    spec = importlib.util.spec_from_file_location('accepted_input', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def failure(error):
    result = {'type': type(error).__name__, 'message': str(error)}
    for name in ('returncode', 'timeout', 'stdout', 'stderr'):
        value = getattr(error, name, None)
        if value is not None:
            result[name] = value.hex() if isinstance(value, bytes) else value
    return result


def fd_set():
    result = set()
    for name in os.listdir('/dev/fd'):
        try:
            os.fstat(int(name))
        except OSError:
            continue
        result.add(int(name))
    return result


def make_case(reference, contract, reference_dir, markers=None):
    class DarwinCase(reference.InputCase):
        def __init__(self, binary, kind, root, name, timeout, run_stop):
            super().__init__(binary, kind, root, (name, [], reference.EXPECTED[name]), timeout)
            self.root.chmod(0o700)
            self.run_stop = run_stop
            self.watches = {}
            self.owned = {}
            self.host = None
            self.client_pid = None
            self.socket_identity = None
            self.ready = native.Readiness()
            self.queue = select.kqueue()
            for name in ('events', 'emit', 'host-release'):
                os.mkfifo(self.root / name, 0o600)
            self.events_fd = os.open(self.root / 'events', os.O_RDWR | os.O_NONBLOCK)
            self.queue.control([select.kevent(self.events_fd, filter=select.KQ_FILTER_READ,
                                              flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR)], 0, 0)
            for key in contract['native_fixture']['remove_inherited']:
                self.env.pop(key, None)
            for name in ('home', 'xdg'):
                (self.root / name).mkdir(mode=0o700)
            self.env = dict(PATH='/usr/bin:/bin', HOME=str(self.root / 'home'),
                            XDG_CONFIG_HOME=str(self.root / 'xdg'), SHELL='/bin/sh',
                            LC_ALL='C', LANG='C', TERM='xterm-256color',
                            RMUX_DISABLE_TMUX_FALLBACK='1', PYTHONDONTWRITEBYTECODE='1')
            self.record['runtime_source_sha'] = RUNTIME_SHA
            self.record['reference_sha256'] = REFERENCE_SHA
            self.record['readiness_challenge'] = self.ready.challenge
            self.marker = (bytes.fromhex(markers[f'{kind}/{self.name}']) if markers else
                           b'restore:' + secrets.token_hex(16).encode() + b'\n')

        def remaining(self):
            stop = self.cleanup_stop if self.cleaning else min(self.deadline, self.run_stop - 12)
            remaining = stop - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('bounded native case deadline')
            return min(4.0, remaining)

        def pump(self):
            if self.master is not None:
                drain(self.master, self.raw)

        def wait_event(self, timeout):
            self.pump()
            event(self.queue, timeout)
            self.pump()
            try:
                os.read(self.events_fd, 4096)
            except BlockingIOError:
                pass

        def until(self, predicate, label):
            end = time.monotonic() + self.remaining()
            while True:
                self.pump()
                if predicate():
                    return
                timeout = min(end - time.monotonic(), self.remaining())
                if timeout <= 0:
                    raise TimeoutError(label)
                self.wait_event(timeout)

        def q(self, *args, check=True):
            command = [self.binary, '-S', str(self.socket), '-f', '/dev/null', *args]
            entry = {'at': now(), 'argv': command}
            self.record['commands'].append(entry)
            self.pump()
            try:
                result = native.run(command, env=self.env, timeout=self.remaining(), check=False)
            except Exception as error:
                entry['error'] = failure(error)
                raise
            entry.update(code=result.returncode, pid=result.pid,
                         stdout_hex=result.stdout.hex(), stderr_hex=result.stderr.hex())
            self.pump()
            if check and result.returncode:
                raise subprocess.CalledProcessError(result.returncode, command, result.stdout, result.stderr)
            return result

        def track(self, pid, role, expected_sha=None):
            observed = identity(pid)
            if observed is None:
                raise RuntimeError(f'{role} disappeared before registration')
            if expected_sha is not None and observed['sha256'] != expected_sha:
                raise RuntimeError(f'{role} executable pin differs: {observed}')
            if pid not in self.watches:
                self.watches[pid] = native.FixtureProcess(pid)
                self.owned[pid] = {**observed, 'roles': [role]}
            else:
                self.equal(observed['birth'], self.owned[pid]['birth'], 'same owned process birth')
                self.owned[pid] = {**observed, 'roles': list(set(self.owned[pid]['roles'] + [role]))}
            return observed

        def start(self):
            self.equal(self.socket.exists(), False, 'fresh socket initially absent')
            command = shlex.join([sys.executable, '-B', str(Path(__file__).resolve()),
                                 '--application', str(self.root), '--ready-path', str(self.ready.path),
                                 '--challenge', self.ready.challenge,
                                 *(['--aware'] if self.name in ('P1', 'P3') else [])])
            self.started = True
            self.q('new-session', '-d', '-s', 'g6', '-x', '20', '-y', '5', command)
            info = self.socket.lstat()
            self.equal(stat.S_ISSOCK(info.st_mode), True, 'private Unix socket')
            self.socket_identity = [info.st_dev, info.st_ino, info.st_uid]
            pid, socket, version = self.fmt('#{pid}|#{socket_path}|#{version}').split('|')
            self.equal(str(Path(socket).resolve()), str(self.socket), 'private socket endpoint')
            server_pin = contract['pins']['rmux_daemon' if self.kind == 'rmux' else 'tmux']['sha256']
            self.record['server'] = {**self.track(int(pid), 'server', server_pin), 'version': version}
            self.q('set-option', '-g', 'status', 'off')
            self.q('set-option', '-s', 'escape-time', '50')
            self.equal(self.text('show-options', '-s', '-v', 'escape-time'), '50', 'escape-time setup')
            self.until(lambda: (self.root / 'application-start.json').exists(), 'application raw start')
            app = json.loads((self.root / 'application-start.json').read_text())
            self.equal(app['challenge'], self.ready.challenge, 'fresh application record')
            app_identity = self.track(app['identity']['pid'], 'application', identity(os.getpid())['sha256'])
            pane_pid, pane_tty = self.fmt('#{pane_pid}|#{pane_tty}').split('|')
            pane = self.track(int(pane_pid), 'pane')
            self.equal(app_identity['pid'] == pane['pid'] or app_identity['ppid'] == pane['pid'],
                       True, 'application belongs to pane process')
            self.equal(app['tty'], pane_tty, 'application and pane tty')
            self.record['application'] = app
            self.record['application_native_observation'] = raw_tty(app_identity['pid'], pane_tty)
            self.record['pane_process'] = pane

            self.master, self.slave = os.openpty()
            os.set_blocking(self.master, False)
            fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 5, 20, 0, 0))
            self.original_termios = termios.tcgetattr(self.slave)
            self.record['termios_before'] = attributes(self.original_termios)
            restoration.require(self.record['termios_before'] == restoration.PROFILE,
                                'initial profile outside frozen admission')
            self.record['slave_before'] = slave_identity(self.slave)
            self.record['terminal_observers'] = {
                'recorded_ns': time.monotonic_ns(),
                'outer_master': 'parent kqueue EVFILT_READ and nonblocking drain',
                'outer_slave_before_attach': [],
                'outer_slave_while_attached': 'client input route; host has no input observer',
                'host_while_client_exits': 'NOTE_EXIT on child and EVFILT_READ on host-release FIFO',
                'outer_slave_after_exit_until_raw_snapshot': [],
                'application_tty': 'distinct owned application PTY; raw_tty observation recorded separately'}
            self.client_tty = os.ttyname(self.slave)
            self.queue.control([select.kevent(self.master, filter=select.KQ_FILTER_READ,
                                              flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR)], 0, 0)
            argv = [self.binary, '-S', str(self.socket), '-f', '/dev/null', 'attach-session', '-t', 'g6']
            save(self.root / 'launch.json', {'argv': argv, 'env': self.env})
            host_argv = [sys.executable, '-B', str(Path(__file__).resolve()),
                         '--terminal-host', str(self.root), '--specification', str(self.root / 'launch.json')]
            self.host = subprocess.Popen(host_argv, stdin=self.slave, stdout=self.slave,
                                         stderr=self.slave, env=self.env, preexec_fn=child_terminal)
            self.track(self.host.pid, 'terminal_host')
            self.until(lambda: (self.root / 'client-start.json').exists(), 'native terminal host')
            host_start = json.loads((self.root / 'client-start.json').read_text())
            self.record['terminal_host'] = self.track(self.host.pid, 'terminal_host', identity(os.getpid())['sha256'])
            self.client_pid = host_start['identity']['pid']
            self.record['client'] = self.track(self.client_pid, 'client', contract['pins'][self.kind]['sha256'])
            self.until(lambda: self.client_tty in self.text('list-clients', '-F', '#{client_tty}'), 'client attached')
            clients = self.text('list-clients', '-F', '#{client_pid}|#{client_tty}|#{client_width}|#{client_height}')
            self.record['list_clients'] = clients
            self.equal(clients.splitlines(), [f'{self.client_pid}|{self.client_tty}|20|5'], 'one client identity and geometry')
            self.equal(self.fmt('#{pane_width}|#{pane_height}'), '20|5', 'pane geometry')
            self.record['termios_attached'] = attributes(termios.tcgetattr(self.slave))
            restoration.admit(self.record['termios_before'], self.record['termios_attached'])
            self.equal(termios.tcgetattr(self.slave) != self.original_termios, True, 'client terminal changed')
            self.record['client_fd0'] = fd_identity(self.client_pid, 0)
            self.equal(self.record['client_fd0']['rdev'], os.fstat(self.slave).st_rdev, 'client fd0 owned tty')
            write_fifo(self.root / 'emit', b'emit\n')
            self.ready.wait(self.remaining())
            self.record['producer_acknowledged_ns'] = time.monotonic_ns()
            self.until(lambda: self.fmt('#{pane_title}') == 'INPUT-READY', 'server processed application output')
            self.equal(self.text('capture-pane', '-p', '-t', 'g6:0.0').splitlines()[0], 'INPUT READY', 'native content')
            self.equal(self.mode(), '|0', 'ordinary input route')
            if self.name == 'E1':
                self.record['settle_start_ns'] = time.monotonic_ns()
                time.sleep(.7)
                self.pump()
                self.record['settle_end_ns'] = time.monotonic_ns()
            self.record['escape_time_at_input'] = self.text('show-options', '-s', '-v', 'escape-time')
            self.equal(self.record['escape_time_at_input'], '50', 'escape-time readback')
            self.record['readiness_ns'] = time.monotonic_ns()

        def expect_by(self, expected, sent_ns, label):
            end_ns = sent_ns + reference.BOUND_NS
            while True:
                self.remaining()
                self.pump()
                if len(self.received()) >= len(expected):
                    self.equal(self.received(), expected, label + ' exact bytes')
                    read_ns = self.reads()[-1]['read_ns']
                    self.equal(read_ns <= end_ns, True, label + ' within 1500ms')
                    return read_ns
                left = (end_ns - time.monotonic_ns()) / 1e9
                if left <= 0:
                    raise TimeoutError(label + ' did not deliver exact bytes within1500ms')
                self.wait_event(min(left, self.remaining()))

        def equal(self, actual, expected, label):
            if isinstance(actual, bytes):
                actual = actual.hex()
            if isinstance(expected, bytes):
                expected = expected.hex()
            return super().equal(actual, expected, label)

        def finish(self):
            self.cleaning = True
            self.cleanup_stop = min(self.run_stop, time.monotonic() + 12)
            cleanup = self.record['cleanup']
            cleanup['started_ns'] = time.monotonic_ns()
            cleanup['deadline_ns'] = int(self.cleanup_stop * 1e9)
            cleanup['errors'] = []
            cleanup['rescues'] = []

            def attempt(label, action):
                try:
                    return action()
                except Exception as error:
                    cleanup['errors'].append({'step': label, **failure(error)})
                    return None

            if self.client_pid is not None:
                attempt('detach-client', lambda: self.q('detach-client', '-s', 'g6'))
                attempt('client-exit', lambda: drained_exit(self.watches[self.client_pid], self.master, self.raw, self.remaining()))
                attempt('client-receipt', lambda: self.until(lambda: (self.root / 'client-end.json').exists(), 'client exit receipt'))
                if (self.root / 'client-end.json').exists():
                    cleanup['client_end'] = json.loads((self.root / 'client-end.json').read_text())
                    attempt('client-status', lambda: self.equal(cleanup['client_end']['code'], 0, 'normal client exit'))
                def restored():
                    result = restore_terminal(
                        self.root, self.master, self.slave, self.record['termios_before'],
                        self.record['termios_attached'], self.record['slave_before'],
                        self.record['terminal_host'], self.raw, cleanup['client_end'],
                        self.cleanup_stop, self.marker, self.record.get('behavior_pass', False))
                    cleanup['restoration'] = result
                    cleanup['termios_after'] = result['after_service']
                    cleanup['termios_restored'] = result['pass']
                    cleanup['outer_mode_2004'] = result['outer_mode_2004']
                attempt('termios', restored)
                if (self.root / 'restoration.json').exists():
                    cleanup['restoration'] = json.loads((self.root / 'restoration.json').read_text())
                attempt('client-drain', self.pump)
            if self.started:
                attempt('kill-server', lambda: self.q('kill-server'))
            for pid, watched in self.watches.items():
                if self.host is not None and pid == self.host.pid:
                    continue
                attempt(f'exit-{pid}', lambda watched=watched: watched.wait(self.remaining()))
            # A failed cleanup stays failed even if emergency disposal succeeds.
            for pid, watched in self.watches.items():
                if self.host is not None and pid == self.host.pid:
                    continue
                if not watched.poll_exit():
                    def rescue(pid=pid, watched=watched):
                        current = identity(pid)
                        if current is None or current['birth'] != self.owned[pid]['birth']:
                            raise RuntimeError('known identity changed before rescue')
                        cleanup['rescues'].append({'pid': pid, 'birth': current['birth'], 'signal': 'SIGKILL'})
                        watched.retire_after_failure(max(.1, self.remaining()))
                    attempt(f'rescue-{pid}', rescue)
            if self.host is not None:
                def release_host():
                    host_watch = self.watches[self.host.pid]
                    if not host_watch.poll_exit():
                        write_fifo(self.root / 'host-release', b'close\n')
                        drained_exit(host_watch, self.master, self.raw, self.remaining())
                    cleanup['host_exit_code'] = self.host.wait()
                    self.equal(cleanup['host_exit_code'], 0, 'terminal host collected')
                attempt('terminal-host', release_host)
                if not self.watches[self.host.pid].poll_exit():
                    def rescue_host():
                        current = identity(self.host.pid)
                        if current is None or current['birth'] != self.owned[self.host.pid]['birth']:
                            raise RuntimeError('terminal host identity changed')
                        cleanup['rescues'].append({'pid': self.host.pid, 'signal': 'SIGKILL'})
                        os.kill(self.host.pid, signal.SIGKILL)
                        # Release this fixture's terminal even on an abnormal exit.
                        for field in ('master', 'slave'):
                            fd = getattr(self, field)
                            if fd is not None:
                                os.close(fd)
                                setattr(self, field, None)
                        self.watches[self.host.pid].wait(max(.05, self.cleanup_stop - time.monotonic()))
                        cleanup['host_exit_code'] = self.host.wait()
                    attempt('terminal-host-rescue', rescue_host)
            cleanup['processes'] = [{**self.owned[pid], 'exit_observed': watched.exited}
                                    for pid, watched in self.watches.items()]
            cleanup['all_known_exits_observed'] = all(watched.exited for watched in self.watches.values())
            if cleanup['all_known_exits_observed'] and self.socket.exists():
                def remove_socket():
                    info = self.socket.lstat()
                    self.equal([info.st_dev, info.st_ino, info.st_uid], self.socket_identity, 'same owned socket residue')
                    self.socket.unlink()
                    cleanup['owned_socket_residue_removed'] = True
                attempt('socket-residue', remove_socket)
            cleanup['socket_absent'] = not self.socket.exists()
            attempt('binary-pin', lambda: self.equal(sha(self.binary), contract['pins'][self.kind]['sha256'], 'binary unchanged'))
            (self.root / 'client-output.bin').write_bytes(self.raw)
            self.record['client_output'] = {'bytes': len(self.raw), 'sha256': sha(self.root / 'client-output.bin')}
            cleanup['pass'] = (not cleanup['errors'] and not cleanup['rescues']
                               and cleanup['all_known_exits_observed'] and cleanup['socket_absent']
                               and cleanup.get('termios_restored', False)
                               and cleanup.get('outer_mode_2004', {}).get('restored', False))
            self.record['end'] = now()
            cleanup['ended_ns'] = time.monotonic_ns()
            if cleanup['ended_ns'] > cleanup['deadline_ns']:
                cleanup['errors'].append({'step': 'cleanup-deadline', 'message': '12-second budget exceeded'})
                cleanup['pass'] = False

        def close_resources(self):
            for watched in self.watches.values():
                watched.close()
            self.queue.close()
            self.ready.close()
            for fd in (self.master, self.slave, self.events_fd):
                if fd is not None:
                    os.close(fd)

        def execute(self):
            try:
                self.start()
                self.run_inputs()  # The accepted Linux payload/order/quiet-tail implementation.
                if self.name == 'P3':
                    writes = self.record['writes']
                    gaps = [b['before_ns'] - a['after_ns'] for a, b in zip(writes, writes[1:])]
                    self.record['fragment_gaps_ns'] = gaps
                    self.equal(all(gap >= 5_000_000 for gap in gaps), True, 'fragment gaps >=5ms')
                self.record['behavior_pass'] = True
            except Exception as error:
                self.record['behavior_pass'] = False
                self.record['primary_error'] = failure(error)
            finally:
                try:
                    self.finish()
                except Exception as error:
                    self.record['cleanup']['pass'] = False
                    self.record['cleanup']['unhandled_error'] = failure(error)
                finally:
                    self.close_resources()
                    closed_ns = time.monotonic_ns()
                    self.record['cleanup']['fds_closed_ns'] = closed_ns
                    if closed_ns > self.record['cleanup'].get('deadline_ns', closed_ns):
                        self.record['cleanup']['errors'].append({
                            'step': 'FD-close-deadline', 'message': '12-second cleanup budget exceeded'})
                        self.record['cleanup']['pass'] = False
            self.record['pass'] = self.record['behavior_pass'] and self.record['cleanup']['pass']
            save(self.root / 'receipt.json', self.record)
            return self.record
    return DarwinCase


def run_suite(args):
    if sha(args.contract) != CONTRACT_SHA:
        raise RuntimeError('frozen contract bytes differ')
    contract = json.loads(args.contract.read_text())
    reference = load_reference(args.reference_dir)
    frozen = json.loads(args.restoration_freeze.read_text())
    if frozen['restoration_contract_sha256'] != restoration.CONTRACT_SHA or frozen['profile'] != restoration.PROFILE:
        raise RuntimeError('restoration freeze differs')
    for pin in frozen['scripts']:
        if sha(pin['path']) != pin['sha256']:
            raise RuntimeError('frozen script differs')
    if not frozen['calibration_valid']:
        raise RuntimeError('restoration calibration not valid')
    for case in contract['cases']:
        if reference.EXPECTED[case['id']].hex() != case['expected_hex']:
            raise RuntimeError('reference expectation differs from frozen contract')
    for key in ('tmux', 'rmux', 'rmux_daemon'):
        pin = contract['pins'][key]
        if sha(pin['path']) != pin['sha256']:
            raise RuntimeError(f'{key} pin mismatch')
    args.root.mkdir(parents=True, mode=0o700, exist_ok=False)
    summary = {'start': now(), 'complete': False, 'pass': False, 'oracle': [], 'rmux': [],
               'runtime_source_sha': RUNTIME_SHA, 'driver_sha256': sha(__file__),
               'driver_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
               'reference_sha256': REFERENCE_SHA, 'helper_sha256': HELPER_SHA,
               'contract_sha256': CONTRACT_SHA, 'performance_evidence': False,
               'restoration_freeze_sha256': sha(args.restoration_freeze),
               'pins': {k: contract['pins'][k] for k in ('tmux', 'rmux', 'rmux_daemon')}}
    save(args.root / 'summary.json', summary)
    stop = time.monotonic() + 180
    baseline = fd_set()
    case_type = make_case(reference, contract, args.reference_dir, frozen['markers'])
    try:
        version = native.run([contract['pins']['tmux']['path'], '-V'], env=dict(os.environ), timeout=4)
        summary['oracle_version'] = version.stdout.decode().strip()
        if summary['oracle_version'] != 'tmux 3.7b':
            raise RuntimeError('oracle version differs')
        for kind in ('tmux', 'rmux'):
            for name in reference.EXPECTED:
                if kind == 'rmux' and sha(args.root / 'ORACLE-FROZEN.json') != summary['oracle_sha256']:
                    raise RuntimeError('oracle changed after freeze')
                remaining = min(20, stop - time.monotonic() - 12)
                if remaining <= 0:
                    raise TimeoutError('suite reserve reached; no new case')
                case = case_type(contract['pins'][kind]['path'], kind,
                                 args.root / ('t' if kind == 'tmux' else 'r') / name, name, remaining, stop)
                result = case.execute()
                summary['oracle' if kind == 'tmux' else 'rmux'].append(result)
                summary['parent_fds_stable'] = fd_set() == baseline
                if result['pass']:
                    try:
                        restoration.cleanup_ok(result['cleanup'], summary['parent_fds_stable'])
                    except Exception as error:
                        result['pass'] = False
                        result['cleanup']['pass'] = False
                        result['cleanup']['final_guard_error'] = failure(error)
                        save(case.root / 'receipt.json', result)
                save(args.root / 'summary.json', summary)
                print(json.dumps({'kind': kind, 'case': name, 'pass': result['pass'],
                                  'primary_error': result.get('primary_error'), 'cleanup': result['cleanup']}), flush=True)
                if not result['pass'] or not summary['parent_fds_stable']:
                    raise RuntimeError(f'{kind}/{name} failed; stop without replay')
            if kind == 'tmux':
                save(args.root / 'ORACLE-FROZEN.json', {'at': now(), 'cases': summary['oracle'],
                                                       'tmux_sha256': contract['pins']['tmux']['sha256']})
                summary['oracle_sha256'] = sha(args.root / 'ORACLE-FROZEN.json')
                save(args.root / 'summary.json', summary)
        summary['complete'] = summary['pass'] = True
    except Exception as error:
        summary['error'] = failure(error)
    finally:
        summary['end'] = now()
        summary['binary_hashes_after'] = {k: sha(contract['pins'][k]['path']) for k in ('tmux', 'rmux', 'rmux_daemon')}
        if any(summary['binary_hashes_after'][k] != contract['pins'][k]['sha256'] for k in summary['binary_hashes_after']):
            summary['complete'] = summary['pass'] = False
            summary['binary_pin_failure'] = True
        save(args.root / 'summary.json', summary)
    return 0 if summary['pass'] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--calibrate', type=Path)
    parser.add_argument('--contract', type=Path)
    parser.add_argument('--restoration-freeze', type=Path)
    parser.add_argument('--reference-dir', type=Path)
    parser.add_argument('--root', type=Path)
    parser.add_argument('--terminal-host', type=Path)
    parser.add_argument('--restoration-client', type=Path)
    parser.add_argument('--specification', type=Path)
    parser.add_argument('--application', type=Path)
    parser.add_argument('--ready-path')
    parser.add_argument('--challenge')
    parser.add_argument('--aware', action='store_true')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('native Darwin is required')
    if args.terminal_host:
        terminal_host(args.terminal_host, args.specification)
        return 0
    if args.restoration_client:
        restoration_client(args.restoration_client)
        return 0
    if args.application:
        application(args.application, args.aware, args.ready_path, args.challenge)
        return 0
    if args.calibrate:
        print(json.dumps(calibration(args.calibrate)))
        return 0
    if args.contract and args.reference_dir and args.root and args.restoration_freeze:
        return run_suite(args)
    parser.error('select an operation')


if __name__ == '__main__':
    raise SystemExit(main())
