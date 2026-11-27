#!/usr/bin/env python3
"""Bounded native control-window-size matrix. Freeze tmux before candidate replay.

Uses the established macOS executable-identity and owned-process harness. Literal
expectations are independent of RMUX output. Candidate requires a successful
oracle freeze with the same harness digest. Output directories must be fresh.
"""
import argparse
import json
import re
import sys
import time
import traceback
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests/client_loop_reference'))
from support import Case, Client, save, sha


class NativeCase(Case):
    def q(self, *args, timeout=5, **kwargs):
        return super().q(*args, timeout=timeout, **kwargs)

ORACLE_SHA = '9cc80ae8fda9aa0843e64294645ded292dbe3ef318aa8cea4ed2738e760f8326'


def control(c, client, command, error=False):
    cursor = len(client.drains[0].raw)
    token = 'CBOUNDARY' + str(time.monotonic_ns())
    payload = command + ('\n' if command == 'detach-client' else '\ndisplay-message -p ' + token + '\n')
    client.p.stdin.write(payload.encode())
    client.p.stdin.flush()
    boundary = b'%exit' if command == 'detach-client' else token.encode()
    c.until(lambda: boundary in client.drains[0].raw[cursor:], 'control boundary')
    # A command acknowledgement plus a separately acknowledged marker bounds
    # command execution; leave the reader a short opportunity to collect echoes.
    time.sleep(.04)
    lines = bytes(client.drains[0].raw[cursor:]).decode(errors='replace').splitlines()
    c.r.setdefault('control_commands', []).append(dict(pid=client.p.pid, command=command, lines=lines))
    c.require(any(line.startswith('%error ') for line in lines) == error,
              'control command result', command=command, lines=lines)
    return lines


def geometries(c, session):
    return c.q('list-windows', '-t', session, '-F', '#{window_width}x#{window_height}').splitlines()


def expect(c, session, wanted):
    actual = geometries(c, session)
    c.require(actual == wanted, 'canonical window geometries', session=session, expected=wanted, actual=actual)
    # Pane geometry must be observable in the same owner, including inactive windows.
    for index, dimensions in enumerate(wanted):
        panes = c.q('list-panes', '-t', f'{session}:{index}', '-F', '#{pane_width}x#{pane_height}')
        c.require(panes == dimensions, 'single pane follows window', target=f'{session}:{index}', expected=dimensions, actual=panes)


def setup(c, policy):
    c.new('s', rows=35, cols=110)
    c.q('set', '-g', 'status', 'off')
    c.q('new-window', '-d', '-t', 's:1', '/bin/sleep 60')
    for window in ['s:0', 's:1']:
        c.q('set', '-w', '-t', window, 'window-size', policy)
    return c.q('list-windows', '-t', 's', '-F', '#{window_id}').splitlines()


def policies(c, policy):
    ids = setup(c, policy)
    a = Client(c, 's', True)
    base = ['110x35'] * 2 if policy == 'manual' else ['80x24'] * 2

    def refresh(client, value, wanted, echoes=ids):
        lines = control(c, client, 'refresh-client -C ' + value)
        layouts = [line.split()[1] for line in lines if line.startswith('%layout-change ')]
        c.require(layouts == echoes, 'exact ordered layout echoes', expected=echoes, actual=layouts)
        expect(c, 's', wanted)

    refresh(a, ids[1] + ':', ['110x35'] * 2, [])
    refresh(a, ids[1] + ':65x25', [base[0], '65x25'])
    refresh(a, ids[1] + ':65x25', [base[0], '65x25'])
    refresh(a, ids[1] + ':', base)
    refresh(a, ids[1] + ':', base)
    both = ['110x35'] * 2 if policy == 'manual' else ['100x40'] * 2
    refresh(a, '100,40', both)
    second = '110x35' if policy == 'manual' else '120x45'
    refresh(a, ids[1] + ':120x45', [both[0], second])
    b = Client(c, 's', True)
    # Establish an actual latest owner for both windows before comparing policy.
    control(c, b, 'switch-client -t s:1')
    control(c, b, 'switch-client -t s:0')
    combined = {'latest': ['90x50', '90x45'], 'largest': ['100x50', '120x45'],
                'smallest': ['90x40', '90x45'], 'manual': ['110x35', '110x35']}[policy]
    refresh(b, '90x50', combined)
    first_cap = {'latest': '70x50', 'largest': '70x55', 'smallest': '70x50', 'manual': '70x35'}[policy]
    refresh(a, ids[0] + ':70x55', [first_cap, combined[1]])
    refresh(b, ids[0] + ':85x30', ['70x30', combined[1]])
    refresh(a, ids[0] + ':', ['85x30', combined[1]])
    refresh(b, ids[0] + ':', combined)
    refresh(a, ids[1] + ':', [combined[0]] * 2)
    # Window declarations must not overwrite either client's global geometry.
    control(c, b, 'detach-client')
    c.until(lambda: b.p.poll() is not None, 'second client detached')
    # tmux schedules inactive resizes on detach but leaves their stored geometry
    # pending until an immediate refresh. RMUX applies them eagerly. Record that
    # measured divergence, then use the same explicit flush in both engines.
    after_detach = both if c.engine != 'oracle' else [both[0], combined[0]]
    c.until(lambda: geometries(c, 's') == after_detach, 'post-detach geometry')
    expect(c, 's', after_detach)
    refresh(a, '100x40', both)
    c.handshake()


def linked(c):
    ids = setup(c, 'largest')
    for session in ['t', 'u']:
        c.new(session, rows=30, cols=90)
        c.q('set', '-w', '-t', session, 'window-size', 'largest')
    c.q('link-window', '-s', 's:1', '-t', 't:5')
    a, b = Client(c, 's', True), Client(c, 't', True)
    control(c, a, 'refresh-client -C 100x40')
    control(c, b, 'refresh-client -C 90x50')
    control(c, a, f'refresh-client -C {ids[1]}:60x15')
    expect(c, 's', ['100x40', '60x15'])
    c.require(c.q('display-message', '-p', '-t', 't:5', '#{window_id}|#{window_width}x#{window_height}') == ids[1]+'|60x15', 'linked alias has canonical identity and dimensions')
    cursor = len(b.drains[0].raw)
    control(c, a, 'switch-client -t u')
    c.until(lambda: c.q('display-message', '-p', '-t', 't:5', '#{window_width}x#{window_height}') == '90x50', 'departure releases inactive linked constraint')
    lines = bytes(b.drains[0].raw[cursor:]).decode(errors='replace').splitlines()
    changed = next(i for i, line in enumerate(lines) if line.startswith('%client-session-changed '))
    layout = next(i for i, line in enumerate(lines) if line.startswith('%layout-change '+ids[1]+' '))
    c.require(changed < layout, 'switch notification precedes geometry')
    control(c, a, 'switch-client -t s')
    expect(c, 's', ['100x40', '60x15'])
    # Canonical ID survives rename/reindex, unlike a display coordinate.
    c.q('move-window', '-s', 's:1', '-t', 's:3')
    c.q('rename-window', '-t', 's:3', 'renamed')
    control(c, a, f'refresh-client -C {ids[1]}:65x20')
    c.require(c.q('display-message', '-p', '-t', 't:5', '#{window_width}x#{window_height}') == '65x20', 'renumbered window still receives declaration')
    c.q('unlink-window', '-k', '-t', 's:3')
    c.require(c.q('display-message', '-p', '-t', 't:5', '#{window_width}x#{window_height}') == '90x50', 'unlink changes eligibility')
    control(c, a, 'switch-client -t t')
    c.require(c.q('display-message', '-p', '-t', 't:5', '#{window_width}x#{window_height}') == '65x20', 'declaration survives until canonical destruction')
    c.q('kill-window', '-t', 't:5')
    c.q('new-window', '-d', '-t', 't:5', '/bin/sleep 60')
    c.q('set', '-w', '-t', 't:5', 'window-size', 'largest')
    new_id = c.q('display-message', '-p', '-t', 't:5', '#{window_id}')
    c.require(new_id != ids[1], 'replacement owns a fresh identity')
    control(c, a, 'refresh-client -C 100x40')
    c.require(c.q('display-message', '-p', '-t', 't:5', '#{window_width}x#{window_height}') == '100x50', 'replacement does not inherit old constraint')


def mixed(c):
    ids = setup(c, 'largest')
    c.q('set', '-t', 's', 'status', '3')
    a = Client(c, 's', True)
    pty_client = Client(c, 's', False, rows=43, cols=120)
    c.wait_clients([a, pty_client])
    control(c, a, 'refresh-client -C 100x50')
    control(c, a, f'refresh-client -C {ids[1]}:65x25')
    expect(c, 's', ['120x50', '65x25'])
    c.q('set', '-w', '-t', 's:1', 'aggressive-resize', 'on')
    control(c, a, f'refresh-client -C {ids[1]}:60x20')
    c.require(geometries(c, 's')[1] == '65x25', 'inactive aggressive window retains size without eligible clients')
    control(c, a, 'switch-client -t s:1')
    c.until(lambda: geometries(c, 's')[1] == '60x20', 'selected aggressive window applies cap')
    c.q('resize-window', '-A', '-t', 's:1')
    c.require(geometries(c, 's')[1] == '60x20', 'resize-window -A includes control cap')
    # Native process sees the exact pane dimensions, not just formatter state.
    c.q('respawn-pane', '-k', '-t', 's:1.0', 'stty size; /bin/sleep 60')
    c.until(lambda: '20 60' in c.q('capture-pane', '-p', '-t', 's:1.0'), 'native PTY window dimensions')


def invalid(c):
    ids = setup(c, 'largest')
    a = Client(c, 's', True)
    control(c, a, f'refresh-client -C {ids[1]}:65x25')
    for suffix in ['0x20', '10001x20', '20x10001', '65536x20', '20x65536']:
        control(c, a, f'refresh-client -C {ids[1]}:{suffix}', error=True)
        expect(c, 's', ['80x24', '65x25'])
    if c.engine != 'oracle':
        for text in [ids[1]+':bad', ids[1]+':75,28', ids[1]+':75x28tail', '@4294967296:', '@999999:80x24']:
            control(c, a, 'refresh-client -C '+text, error=True)
            expect(c, 's', ['80x24', '65x25'])
    control(c, a, f'refresh-client -C {ids[1]}:')
    expect(c, 's', ['80x24'] * 2)



def aliases(c):
    ids = setup(c, 'largest')
    c.q('link-window', '-d', '-s', 's:1', '-t', 's:3')
    a = Client(c, 's', True)
    lines = control(c, a, f'refresh-client -C {ids[1]}:65x25')
    layouts = [line.split()[1] for line in lines if line.startswith('%layout-change ')]
    c.require(layouts == ids, 'one echo per canonical window despite duplicate aliases', actual=layouts, expected=ids)
    control(c, a, 'refresh-client -B "dims:@*:#{window_id}|#{window_width}x#{window_height}"')
    c.until(lambda: (ids[1]+'|65x25').encode() in a.drains[0].raw, 'B subscription observes committed geometry', 4)
    rows = c.q('list-clients', '-F', '#{client_pid}|#{client_name}').splitlines()
    name = next(row.split('|')[1] for row in rows if row.startswith(str(a.p.pid)+'|'))
    source = c.out / 'refresh.conf'
    source.write_text(f'refresh-client -t {name} -C {ids[1]}:60x20\n')
    c.q('source-file', str(source.resolve()))
    c.require(geometries(c, 's') == ['80x24', '60x20', '60x20'], 'source-file updates both aliases through shared parser')
    c.until(lambda: (ids[1]+'|60x20').encode() in a.drains[0].raw, 'B observes source-file geometry', 4)
    c.q('refresh-client', '-t', name, '-C', ids[1]+':')
    c.require(geometries(c, 's') == ['80x24'] * 3, 'public CLI removal restores both aliases')


def manual_explicit(c):
    ids = setup(c, 'manual')
    a = Client(c, 's', True)
    control(c, a, f'refresh-client -C {ids[1]}:60x15')
    control(c, a, 'resize-window -t s:1 -x 95 -y 32')
    expect(c, 's', ['110x35', '60x15'])
    control(c, a, f'refresh-client -C {ids[1]}:60x15')
    expect(c, 's', ['110x35', '60x15'])
    control(c, a, f'refresh-client -C {ids[1]}:')
    expect(c, 's', ['110x35', '95x32'])


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--engine', choices=['oracle', 'candidate'], required=True)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--freeze', type=Path)
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    digest = sha(__file__)
    if args.engine == 'oracle':
        assert sha(args.binary) == ORACLE_SHA
    else:
        assert args.freeze
        freeze = json.loads(args.freeze.read_text())
        assert freeze['qualified'] and freeze['harness_sha256'] == digest
    cases = {policy: lambda c, policy=policy: policies(c, policy)
             for policy in ['latest', 'largest', 'smallest', 'manual']}
    cases.update(linked=linked, mixed=mixed, invalid=invalid, aliases=aliases, manual_explicit=manual_explicit)
    receipts = {}
    for name, run in cases.items():
        c = NativeCase(args.engine, str(args.binary.resolve()), name, args.output, 55)
        try:
            run(c)
            c.r.update(status='passed')
        except Exception as exc:
            c.r.update(status='failed', reason=str(exc))
            (c.out / 'exception.txt').write_text(traceback.format_exc())
        finally:
            c.close()
        receipts[name] = c.r['status']
        print(name, c.r['status'], c.r.get('reason', ''), flush=True)
    result = dict(qualified=all(s == 'passed' for s in receipts.values()),
                  harness_sha256=digest, oracle_sha256=ORACLE_SHA, cases=receipts)
    save(args.output / 'result.json', result)
    return int(not result['qualified'])


if __name__ == '__main__':
    raise SystemExit(main())
