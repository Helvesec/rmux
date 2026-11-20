#!/usr/bin/env python3
"""Negative A–G witnesses and a native functional-restoration control; no product."""
import argparse
import copy
import ctypes
import errno
import json
import os
from pathlib import Path
import secrets
import select
import signal
import subprocess
import sys
import termios
import time

import input_paste_escape_darwin as m
import darwin_terminal_restore as r


def mock_controls(root):
    root.mkdir(parents=True, mode=0o700, exist_ok=False)
    results = []
    before = copy.deepcopy(r.PROFILE)
    attached = copy.deepcopy(before)
    attached[3] = 0
    pending = copy.deepcopy(before)
    pending[3] |= termios.PENDIN
    marker = b'restore:' + b'a' * 32 + b'\n'
    ack = {'read_hex': marker.hex(), 'read_calls': 1, 'read_limit': 64, 'error': None,
           'read_ns': 8, 'ack_ns': 9}
    echo = marker[:-1] + b'\r\n'

    def witness(label, call, inputs, reject=True):
        row = {'label': label, 'inputs': copy.deepcopy(inputs), 'expected_rejection': reject}
        try:
            row['return'] = call()
            row['rejected'] = False
        except (AssertionError, KeyError, TypeError, IndexError) as error:
            row.update(rejected=True, error=m.failure(error))
        row['pass'] = row['rejected'] == reject
        results.append(row)
        m.save(root / 'witnesses.json', results)
        if not row['pass']:
            raise AssertionError('semantic calibration invalidation: ' + label)

    def classify(value):
        return r.classify(before, attached, value)

    def final(value):
        return r.effects(before, marker, ack, echo, value)

    witness('exact raw accepted as classification only', lambda: classify(before), before, False)
    witness('PENDIN-only raw accepted as classification only', lambda: classify(pending), pending, False)
    for row in results:
        assert row['return']['restoration_pass'] is False
    # SDK tcflag_t is unsigned long: exercise every bit, including reserved bits.
    width = ctypes.sizeof(ctypes.c_ulong) * 8
    for field in range(4):
        for bit in range(width):
            mask = 1 << bit
            if field == 3 and mask == termios.PENDIN:
                continue
            for with_pending in (False, True):
                value = copy.deepcopy(pending if with_pending else before)
                value[field] ^= mask
                witness(f'D flag{field} bit{bit} pending{with_pending}',
                        lambda value=value: classify(value), value)
            value = copy.deepcopy(before)
            value[field] ^= mask
            witness(f'F flag{field} bit{bit}', lambda value=value: final(value), value)
    for index in range(20):
        value = copy.deepcopy(before)
        value[6][index] = f'{int(value[6][index], 16) ^ 1:02x}'
        witness(f'D cc{index}', lambda value=value: classify(value), value)
        witness(f'F cc{index}', lambda value=value: final(value), value)
    for index in (4, 5):
        value = copy.deepcopy(before)
        value[index] += 1
        witness(f'D speed{index}', lambda value=value: classify(value), value)
        witness(f'F speed{index}', lambda value=value: final(value), value)
    witness('PENDIN initially present', lambda: r.classify(pending, attached, pending), pending)
    witness('canonical transition absent', lambda: r.classify(before, before, pending), before)
    only_echo = copy.deepcopy(attached); only_echo[3] = termios.ECHO
    only_canon = copy.deepcopy(attached); only_canon[3] = termios.ICANON
    witness('ECHO transition absent', lambda: r.classify(before, only_echo, pending), only_echo)
    witness('ICANON transition absent', lambda: r.classify(before, only_canon, pending), only_canon)
    witness('PENDIN persists at final', lambda: final(pending), pending)
    for label, data in [('prefix', b'x' + marker), ('suffix', marker + b'x'),
                        ('truncated', marker[:-1]), ('EOF', b''),
                        ('incomplete canonical line prefix', b'leftover' + marker)]:
        bad = {**ack, 'read_hex': data.hex()}
        witness('read ' + label, lambda bad=bad: r.effects(before, marker, bad, echo, before), bad)
    for label, bad in [('error', {**ack, 'error': {'type': 'OSError'}}), ('no ack', None),
                       ('second read', {**ack, 'read_calls': 2}), ('read >64', {**ack, 'read_limit': 65}),
                       ('missing raw bytes', {'read_calls': 1, 'read_limit': 64})]:
        witness('read ' + label, lambda bad=bad: r.effects(before, marker, bad, echo, before), bad)
    for key in ('read_hex', 'read_calls', 'read_limit', 'read_ns', 'ack_ns', 'error'):
        bad = {k: v for k, v in ack.items() if k != key}
        witness('missing read field ' + key, lambda bad=bad: r.effects(before, marker, bad, echo, before), bad)
    for label, bad in [('absent', b''), ('extra', echo + b'x'), ('wrong newline', marker),
                       ('prefix', b'x' + echo), ('truncated', echo[:-1])]:
        witness('echo ' + label, lambda bad=bad: r.effects(before, marker, ack, bad, before), bad)
    for label, product in [('no disable', b'\x1b[?2004h'),
                           ('disable before enable', b'\x1b[?2004l\x1b[?2004h'),
                           ('no enable', b'\x1b[?2004l')]:
        witness('2004 ' + label, lambda product=product: r.outer_mode(product), product)
    host = {'pid': 1, 'birth': [2, 3], 'executable': '/synthetic', 'sha256': 'synthetic'}
    witness('host absent before snapshot', lambda: r.same_identity(None, host), None)
    for key in host:
        bad = {**host, key: None}
        witness('host identity ' + key, lambda bad=bad: r.same_identity(bad, host), bad)
    slave = {'dev': 1, 'ino': 2, 'rdev': 3, 'mode': 4}
    for key in slave:
        bad = {**slave, key: 0}
        witness('slave identity ' + key, lambda bad=bad: r.same_slave(bad, slave), bad)
    trace = dict(client_exit_event=True, client_reaped=True, client_code=0,
                 slave_observers_before_snapshot=[], product_saved_ns=1, raw_snapshot_ns=2,
                 classification_ns=3, command_ns=4, first_observer_ns=5, ready_ns=6,
                 write_before_ns=7, write_after_ns=8, read_ns=8, ack_ns=9, final_snapshot_ns=10,
                 service_deadline_ns=11, cleanup_deadline_ns=12, write_calls=1, write_bytes=41)
    witness('complete boundary', lambda: r.boundary(trace), trace, False)
    for key in trace:
        bad = {k: v for k, v in trace.items() if k != key}
        witness('missing trace ' + key, lambda bad=bad: r.boundary(bad), bad)
    for key, value in [('client_exit_event', False), ('client_reaped', False), ('client_code', 1),
                       ('slave_observers_before_snapshot', ['FIONREAD']), ('raw_snapshot_ns', 6),
                       ('first_observer_ns', 1), ('write_calls', 2), ('write_bytes', 40),
                       ('final_snapshot_ns', 100), ('cleanup_deadline_ns', 5),
                       ('service_deadline_ns', 5_000_000_005)]:
        bad = {**trace, key: value}
        witness('invalid trace ' + key, lambda bad=bad: r.boundary(bad), bad)
    clean = dict(errors=[], rescues=[], all_known_exits_observed=True, host_exit_code=0,
                 client_end={'reaped': True, 'code': 0}, socket_absent=True,
                 termios_restored=True, outer_mode_2004={'restored': True})
    witness('complete cleanup', lambda: r.cleanup_ok(clean, True), clean, False)
    for key, value in [('errors', ['error']), ('rescues', ['SIGKILL']),
                       ('all_known_exits_observed', False), ('host_exit_code', 1),
                       ('client_end', {'reaped': False, 'code': 0}),
                       ('client_end', {'reaped': True, 'code': 1}), ('socket_absent', False),
                       ('termios_restored', False), ('outer_mode_2004', {'restored': False})]:
        bad = {**clean, key: value}
        witness('cleanup ' + key, lambda bad=bad: r.cleanup_ok(bad, True), bad)
    witness('parent FD left open', lambda: r.cleanup_ok(clean, False), clean)
    witness('valid full final effects', lambda: final(before), before, False)
    report = {'pass': True, 'product_execution': False, 'witnesses': len(results),
              'rejections': sum(x['rejected'] for x in results), 'tcflag_bits': width,
              'driver_sha256': m.sha(m.__file__), 'checks_sha256': m.sha(r.__file__),
              'script_sha256': m.sha(__file__), 'witnesses_sha256': m.sha(root / 'witnesses.json')}
    m.save(root / 'summary.json', report)
    return report


def native_control(root):
    root.mkdir(parents=True, mode=0o700, exist_ok=False)
    baseline = m.fd_set()
    report = {'start': m.now(), 'product_execution': False, 'pass': False, 'errors': [], 'rescues': [],
              'driver_sha256': m.sha(m.__file__), 'checks_sha256': m.sha(r.__file__),
              'script_sha256': m.sha(__file__), 'self': m.identity(os.getpid())}
    for name in ('events', 'host-release', 'fixture-restore'):
        os.mkfifo(root / name, 0o600)
    events_fd = os.open(root / 'events', os.O_RDWR | os.O_NONBLOCK)
    queue = select.kqueue()
    queue.control([select.kevent(events_fd, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_ADD)], 0, 0)
    master, slave = os.openpty()
    os.set_blocking(master, False)
    before = m.attributes(termios.tcgetattr(slave))
    slave_before = m.slave_identity(slave)
    product = bytearray()
    host = host_watch = client_watch = None
    deadline = time.monotonic() + 20

    def remaining():
        left = deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError('native control deadline')
        return min(5, left)

    def until(name):
        while not (root / name).exists():
            m.drain(master, product)
            m.event(queue, remaining())
            try:
                os.read(events_fd, 4096)
            except BlockingIOError:
                pass
        return json.loads((root / name).read_text())

    try:
        r.require(before == r.PROFILE, 'native control initial profile outside admission')
        command = [sys.executable, '-B', m.__file__, '--restoration-client', str(root)]
        m.save(root / 'launch.json', {'argv': command})
        host_command = [sys.executable, '-B', m.__file__, '--terminal-host', str(root),
                        '--specification', str(root / 'launch.json')]
        report['host_argv'] = host_command
        host = subprocess.Popen(host_command, stdin=slave, stdout=slave, stderr=slave,
                                preexec_fn=m.child_terminal)
        host_watch = m.native.FixtureProcess(host.pid)
        start = until('client-start.json')
        report['terminal_host'] = start
        client_watch = m.native.FixtureProcess(start['identity']['pid'])
        raw = until('fixture-raw.json')
        r.require(raw['identity']['pid'] == start['identity']['pid'] and
                  raw['identity']['birth'] == start['identity']['birth'], 'calibration child changed')
        r.require(raw['identity']['sha256'] == report['self']['sha256'], 'calibration interpreter differs')
        report['client_after_exec'] = raw['identity']
        attached = m.attributes(termios.tcgetattr(slave))
        report['attached'] = attached
        r.admit(before, attached)
        report['cleanup_start_ns'] = time.monotonic_ns()
        cleanup_stop = min(deadline, time.monotonic() + 12)
        m.write_fifo(root / 'fixture-restore', b'restore\n')
        m.drained_exit(client_watch, master, product, remaining())
        end = until('client-end.json')
        report['restoration'] = m.restore_terminal(
            root, master, slave, before, attached, slave_before, start['host'], product,
            end, cleanup_stop, b'restore:' + secrets.token_hex(16).encode() + b'\n')
        r.require(report['restoration']['classification']['pending_reprocess'],
                  'native positive control did not exercise PENDIN')
        m.write_fifo(root / 'host-release', b'close\n')
        m.drained_exit(host_watch, master, product, min(remaining(), cleanup_stop - time.monotonic()))
        report['host_exit_code'] = host.wait()
        report['client_end'] = end
        r.require(report['host_exit_code'] == 0, 'native host did not exit normally')
        r.require(time.monotonic() <= cleanup_stop, 'native cleanup exceeds 12 seconds')
        report['pass'] = True
    except Exception as error:
        report['errors'].append(m.failure(error))
    finally:
        # On failure only, retire known child identities. Rescue always invalidates.
        if host is not None and host_watch is not None and not host_watch.poll_exit():
            try:
                m.write_fifo(root / 'host-release', b'close\n')
                m.drained_exit(host_watch, master, product, 2)
            except Exception as error:
                report['errors'].append(m.failure(error))
        for watched in (client_watch, host_watch):
            if watched is not None and not watched.poll_exit():
                report['rescues'].append(watched.pid)
                watched.retire_after_failure(2)
        if host is not None:
            report['host_collected'] = host.wait()
        report['known_exits'] = [w.exited for w in (client_watch, host_watch) if w is not None]
        for watched in (client_watch, host_watch):
            if watched is not None:
                watched.close()
        for fd in (master, slave, events_fd):
            os.close(fd)
        queue.close()
        report['fds_stable'] = m.fd_set() == baseline
        report['pass'] &= (not report['errors'] and not report['rescues'] and report['fds_stable']
                           and report['known_exits'] == [True, True])
        report['end'] = m.now()
        (root / 'collected-output.bin').write_bytes(product)
        m.save(root / 'summary.json', report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('mock', 'native'))
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    result = (mock_controls if args.mode == 'mock' else native_control)(args.root)
    print(json.dumps(result))
    return 0 if result['pass'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
