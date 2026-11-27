#!/usr/bin/env python3
"""Bounded Linux history-limit 0/1/2 oracle; no runtime changes or builds.

Usage: python3 -I tests/history_limit_small_reference.py prepare CONFIG.json
       python3 -I tests/history_limit_small_reference.py run CONFIG.json

The sibling history_limit_reference.py is imported unchanged at its pinned hash.
Only the complete, cleaned-up native oracle supplies candidate golden values.
"""
import base64
import copy
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import secrets
import sys
import time
import traceback

sys.dont_write_bytecode = True
BASE = Path(__file__).with_name('history_limit_reference.py').resolve()
BASE_SHA = 'e3597042cf4e0c839995e8d68b0247cd291d71e900b07931f72d0bc6087bc9bc'
assert hashlib.sha256(BASE.read_bytes()).hexdigest() == BASE_SHA
spec = importlib.util.spec_from_file_location('history015', BASE)
B = importlib.util.module_from_spec(spec)
spec.loader.exec_module(B)
LABELS = ('a20', 'a40', 'b20', 'a40b', 'a30', 'b30')
TARGETS = {label: ('beta' if label.startswith('b') else 'alpha') + ':' + label + '.0' for label in LABELS}
KEYS = ('Z1:a20', 'Z2:old-a20', 'Z2:a40', 'Z2:b20',
        *('Z3:global-' + label for label in LABELS[:3]), 'Z3:a40b',
        *('Z3:unset-' + label for label in LABELS[:4]), 'Z3:a30', 'Z3:b30',
        *('Z3:plus6-' + label for label in LABELS),
        *('Z3:clear-' + label for label in LABELS))
METRICS = '#{pane_width}|#{pane_height}|#{history_size}|#{history_limit}|#{cursor_x}|#{cursor_y}'


def payload(label, first, stop):
    return b''.join(B.line(label, i) for i in range(first, stop))


def witness_argv(config, phase, label):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', label,
            str(Path(config['root']) / phase / 'markers'), config['tokens'][phase], '0']


def witness(label, root, token, code):
    B.require(label in LABELS and code == '0', 'fixed witness arguments')
    root = Path(root)
    own = B.identity(os.getpid())
    stem = f"{own['pid']}-{own['start_ticks']}"
    emission = None
    def report(challenge):
        return {'at': B.now(), 'kind': label, 'token': token, 'challenge': challenge,
                'exit_code': 0, 'identity': B.identity(os.getpid()), 'cwd': os.getcwd(),
                'argv': sys.argv, 'emission': emission,
                'environment': {k: os.environ.get(k) for k in B.ENV_KEYS}}
    def emit(first, stop):
        data = payload(label, first, stop)
        sent = 0
        while sent < len(data):
            written = os.write(1, data[sent:])
            B.require(written > 0, 'short helper emission')
            sent += written
        return {'completed_at': B.now(), 'bytes': len(payload(label, 0, stop)), 'lines': stop,
                'sha256': hashlib.sha256(payload(label, 0, stop)).hexdigest(),
                'last_batch': {'first': first, 'stop': stop, 'bytes': sent,
                               'sha256': hashlib.sha256(data).hexdigest()}}
    B.atomic_once(root / ('born-' + stem + '.json'), report(None))
    emission = emit(0, 60)
    B.atomic_once(root / ('emitted-' + stem + '.json'), report(None))
    advanced = False
    end = time.monotonic() + 180
    while time.monotonic() < end:
        advance = root / ('advance-' + stem + '.json')
        if advance.exists() and not advanced:
            request = json.loads(advance.read_text())
            B.require(B.same(request['identity'], own), 'advance birth mismatch')
            B.equal((request['first'], request['stop']), (60, 66), 'six fixed new lines')
            emission = emit(60, 66)
            B.atomic_once(root / ('advanced-' + stem + '.json'), report(request['challenge']))
            advanced = True
        for path in sorted(root.glob('request-' + stem + '-*.json')):
            ack = root / path.name.replace('request-', 'ack-', 1)
            if not ack.exists():
                request = json.loads(path.read_text())
                B.require(B.same(request['identity'], own), 'challenge birth mismatch')
                B.atomic_once(ack, report(request['challenge']))
        release = root / ('exit-' + stem + '.json')
        if release.exists():
            request = json.loads(release.read_text())
            B.require(B.same(request['identity'], own), 'exit birth mismatch')
            B.equal(request['exit_code'], 0, 'normal exit code')
            B.atomic_once(root / ('exit-intent-' + stem + '.json'), report(request['challenge']))
            os._exit(0)
        time.sleep(.02)
    os._exit(124)


def inventory(raw, metrics, label, stop):
    B.equal((metrics['width'], metrics['height']), (80, 12), 'fixed geometry')
    B.require(metrics['history_limit'] in (0, 1, 2), 'small history limit')
    B.require(0 <= metrics['history_size'] <= 2, 'bounded history inventory')
    B.equal((metrics['cursor_x'], metrics['cursor_y']), (0, 11), 'cursor after LF')
    B.require(raw.endswith(b'\n'), 'capture framing newline absent')
    rows = raw.split(b'\n')[:-1]
    nonblank = [row for row in rows if row]
    B.require(nonblank, 'empty capture')
    indices = []
    for row in nonblank:
        prefix = ('HIST|' + label + '|').encode()
        B.require(row.startswith(prefix), 'foreign line')
        suffix = row[len(prefix):]
        B.require(len(suffix) == 4 and suffix.isdigit(), 'invalid index')
        index = int(suffix)
        B.equal(row + b'\n', B.line(label, index), 'exact line')
        indices.append(index)
    B.require(0 <= indices[0] < stop, 'index bounds')
    B.equal(indices, list(range(indices[0], stop)), 'contiguous suffix to last emission')
    B.equal(rows, nonblank + [b''], 'exact trailing blank row')
    B.equal(len(rows), metrics['history_size'] + 12, 'history plus viewport rows')
    return {'indices': indices, 'first': indices[0], 'last': indices[-1],
            'nonblank_rows': len(nonblank), 'blank_rows': 1,
            'rows': len(rows), 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}


def exact(actual, golden):
    B.equal(actual, golden, 'exact oracle bytes, indices, history, geometry and cursor')


def pure_checks(config):
    inherited = B.pure_checks(config)
    def capture(size, limit, first=None):
        metrics = dict(width=80, height=12, history_size=size, history_limit=limit, cursor_x=0, cursor_y=11)
        raw = payload('a20', 49-size if first is None else first, 60) + b'\n'
        return {'raw_base64': base64.b64encode(raw).decode(), 'metrics': metrics,
                'inventory': inventory(raw, metrics, 'a20', 60)}
    checks = []
    def rejected(name, fn):
        try:
            fn()
        except (B.Failed, B.Unproven):
            checks.append(name)
        else:
            raise AssertionError('insensitive observer: ' + name)
    cold = capture(0, 0)
    one, two = capture(1, 1), capture(2, 2)
    cleared = capture(0, 2)
    checks.append('positive_zero_one_two_clear_inventory')
    rejected('cold_zero_wrongly_keeps_one', lambda: exact(capture(1, 0), cold))
    rejected('capacity_one_loses_oldest', lambda: exact(capture(0, 1), one))
    rejected('capacity_two_loses_oldest', lambda: exact(capture(1, 2), two))
    rejected('clear_history_does_not_clear', lambda: exact(two, cleared))
    rejected('raised_cold_zero_wrongly_enables_collection', lambda: exact(two, cleared))
    rejected('wrong_inheritance_limit', lambda: exact(capture(1, 2), one))
    rejected('wrong_row_count', lambda: capture(2, 2, 48))
    bad = copy.deepcopy(two); bad['raw_base64'] = one['raw_base64']
    rejected('clamped_capture_loses_line', lambda: exact(bad, two))
    fake = {'status': 'PASS', 'cases': [{'id': c, 'status': 'PASS'} for c in ('Z1', 'Z2', 'Z3')],
            'captures': [{'key': key, 'status': 'PASS', 'final': two} for key in KEYS],
            'exits': [{'label': label, 'status': 'PASS'} for label in LABELS], 'cleanup': {'status': 'PASS'}}
    B.equal(oracle_gate(fake, 'synthetic')['captures'], {key: two for key in KEYS}, 'gate serialization')
    fake['captures'].pop()
    rejected('gate_incomplete_clear_isolation', lambda: oracle_gate(fake, 'synthetic'))
    return {'at': B.now(), 'status': 'PASS', 'driver_sha256': B.digest(__file__),
            'base_sha256': B.digest(BASE), 'inherited': inherited, 'checks': checks,
            'synthetic_only': True, 'product_runs': 0}


class Run(B.Run):
    def expected(self, label):
        B.require(label in LABELS, 'bounded helper label')
        return {'kind': label, 'exit_code': 0, 'token': self.config['tokens'][self.phase],
                'exe': str(Path(self.config['python']).resolve()), 'exe_sha256': self.config['hashes']['python'],
                'argv': witness_argv(self.config, self.phase, label), 'cwd': str(self.root / 'cwd'),
                'environment': {k: self.env.get(k) for k in B.ENV_KEYS}}

    def advance(self, label):
        child = self.children[label]
        own = child['child']['before']
        B.require(B.snapshot_same(child['child'], B.proc(own['pid'])), 'advance child changed')
        stem = f"{own['pid']}-{own['start_ticks']}"
        request = {'identity': own, 'challenge': secrets.token_hex(24), 'issued_at': B.now(), 'first': 60, 'stop': 66}
        path = self.root / 'markers' / ('advance-' + stem + '.json')
        B.atomic_once(path, request)
        ack_path = path.with_name('advanced-' + stem + '.json')
        ack = self.wait_json(ack_path)
        B.check_marker(ack, child['child'], child['expected'], request['challenge'])
        B.require(request['issued_at'] <= ack['emission']['completed_at'] <= ack['at'], 'advance ordering')
        self.record.setdefault('advances', []).append({'label': label, 'request': request, 'request_path': str(path), 'ack': ack, 'ack_path': str(ack_path)})

    def capture_stage(self, key, label):
        target = TARGETS[label]
        child = self.children[label]
        latest = self.record['observations'][-1]
        stop = 66 if key.startswith(('Z3:plus6-', 'Z3:clear-')) else 60
        emission = latest['ack']['emission']
        B.equal(emission['bytes'], len(payload(label, 0, stop)), 'complete emission bytes')
        B.equal(emission['lines'], stop, 'complete emission lines')
        B.equal(emission['sha256'], hashlib.sha256(payload(label, 0, stop)).hexdigest(), 'complete emission hash')
        B.require(emission['completed_at'] <= latest['request']['issued_at'] <= latest['ack']['at'], 'emission then fresh challenge')
        item = {'key': key, 'label': label, 'target': target, 'at': B.now(), 'public': latest['public'],
                'identity': child['child']['before'], 'challenge': latest['request']['challenge'],
                'emission': emission, 'samples': [], 'status': 'UNPROVEN'}
        self.record['captures'].append(item)
        previous = None
        try:
            while True:
                self.remaining()
                raw = self.command(['capture-pane', '-p', '-t', target, '-S', '-', '-E', '-'], raw=True)
                self.capture_bytes += len(raw)
                B.require(self.capture_bytes + sum(len(payload(k, 0, 66)) for k in LABELS) <= 65536, 'product text cap')
                values = self.command(['display-message', '-p', '-t', target, METRICS]).split('|')
                B.require(len(values) == 6 and all(v.isdigit() for v in values), 'geometry/history/cursor API unavailable')
                metrics = dict(zip(('width', 'height', 'history_size', 'history_limit', 'cursor_x', 'cursor_y'), map(int, values)))
                sample = {'at': B.now(), 'raw_base64': base64.b64encode(raw).decode(), 'metrics': metrics}
                item['samples'].append(sample)
                if B.line(label, stop - 1)[:-1] not in raw.split(b'\n'):
                    time.sleep(min(.04, self.remaining()))
                    continue
                stable = {'raw_base64': sample['raw_base64'], 'metrics': metrics,
                          'inventory': inventory(raw, metrics, label, stop)}
                if previous == stable:
                    clamped = self.command(['capture-pane', '-p', '-t', target, '-S', '-999', '-E', '999'], raw=True)
                    self.capture_bytes += len(clamped)
                    B.require(self.capture_bytes + sum(len(payload(k, 0, 66)) for k in LABELS) <= 65536, 'product text cap')
                    item['clamped_raw_base64'] = base64.b64encode(clamped).decode()
                    B.equal(clamped, raw, 'capture oversized range clamps to full retained grid')
                    item['final'] = stable
                    public_after, _ = self.public(target)
                    item['public_after'] = public_after
                    B.equal(public_after, latest['public'], 'public identity across capture')
                    B.require(B.snapshot_same(child['child'], B.proc(child['child']['before']['pid'])), 'child changed during capture')
                    if self.phase == 'candidate':
                        exact(stable, self.oracle_captures[key])
                    item['status'] = 'PASS'
                    return
                previous = stable
                time.sleep(min(.04, self.remaining()))
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, B.Failed) else 'UNPROVEN', error=str(error))
            raise

    def sample(self, label, key, new=False):
        self.observe(label, TARGETS[label], not new)
        self.capture_stage(key, label)

    def spawn(self, label, key):
        B.require(self.spawn_count < 6 and label not in self.children, 'bounded fresh child')
        session = TARGETS[label].split(':')[0]
        if label in ('a20', 'b20'):
            args = ['new-session', '-d', '-s', session, '-n', label, '-x', '80', '-y', '12']
        else:
            args = ['new-window', '-d', '-t', session + ':', '-n', label]
        self.spawn_count += 1
        self.command([*args, '-c', str(self.root / 'cwd'), *witness_argv(self.config, self.phase, label)], startup=label == 'a20')
        self.sample(label, key, True)

    def execute(self):
        B.require(not self.socket.exists() and not list((self.root / 'markers').iterdir()), 'fixture not fresh')
        self.profiles()
        try:
            for case in ('Z1', 'Z2', 'Z3'):
                self.case_end = min(time.monotonic() + 20, self.start + 150)
                entry = {'id': case, 'start': B.now(), 'monotonic_start': time.monotonic()}
                self.record['cases'].append(entry)
                try:
                    if case == 'Z1':
                        self.spawn('a20', 'Z1:a20')
                        self.options('alpha', 0, 0, None)
                    elif case == 'Z2':
                        self.command(['set-option', '-t', 'alpha', 'history-limit', '1'])
                        self.options('alpha', 0, 1, 1)
                        self.sample('a20', 'Z2:old-a20')
                        self.spawn('a40', 'Z2:a40')
                        self.spawn('b20', 'Z2:b20')
                        self.options('beta', 0, 0, None)
                    else:
                        self.command(['set-option', '-g', 'history-limit', '2'])
                        self.options('alpha', 2, 1, 1)
                        self.options('beta', 2, 2, None)
                        for label in LABELS[:3]:
                            self.sample(label, 'Z3:global-' + label)
                        self.spawn('a40b', 'Z3:a40b')
                        self.command(['source-file', str(self.root / 'live.conf')])
                        self.options('alpha', 2, 2, None)
                        self.options('beta', 2, 2, None)
                        for label in LABELS[:4]:
                            self.sample(label, 'Z3:unset-' + label)
                        self.spawn('a30', 'Z3:a30')
                        self.spawn('b30', 'Z3:b30')
                        for label in LABELS:
                            self.advance(label)
                            self.sample(label, 'Z3:plus6-' + label)
                        before_clear = {c['label']: c['final'] for c in self.record['captures'] if c['key'].startswith('Z3:plus6-')}
                        self.command(['clear-history', '-t', TARGETS['a40']])
                        for label in LABELS:
                            self.sample(label, 'Z3:clear-' + label)
                            after = self.record['captures'][-1]['final']
                            if label != 'a40':
                                exact(after, before_clear[label])
                            else:
                                B.equal(after['metrics']['history_size'], 0, 'selected pane history cleared')
                    self.ledger()
                    self.profiles()
                    self.remaining()
                    entry['status'] = 'PASS'
                except Exception as error:
                    entry.update(status='FAIL' if isinstance(error, B.Failed) else 'UNPROVEN', error=str(error))
                    raise
                finally:
                    entry.update(end=B.now(), elapsed_seconds=time.monotonic() - entry['monotonic_start'])
        finally:
            # Preserve the original product failure while attempting every known child's normal exit.
            failures = []
            for label in self.children:
                self.case_end = min(time.monotonic() + 20, self.start + 150)
                try:
                    self.exit_witness(label, True)
                except Exception as error:
                    failures.append(repr(error))
            self.record['normal_exit_errors'] = failures
            self.record['text_bytes_bound'] = self.capture_bytes + sum(len(payload(k, 0, 66)) for k in LABELS)
            if failures and sys.exc_info()[0] is None:
                raise B.Unproven('normal exit proof failed: ' + repr(failures))


def prepare(config):
    root = Path(config['root'])
    for phase in ('oracle', 'candidate'):
        p = root / phase
        p.mkdir()
        for name in ('home-empty', 'xdg-empty', 'markers', 'cwd'):
            (p / name).mkdir()
        (p / 'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-window-option -g remain-on-exit on\nset-option -g history-limit 0\n")
        (p / 'live.conf').write_text('set-option -u -t alpha history-limit\n')
    B.write_once(root / 'PLAN.json', {'keys': KEYS, 'labels': LABELS, 'targets': TARGETS,
        'width': 80, 'height': 12, 'initial_lines': [0, 59], 'additional_lines': [60, 65],
        'children_per_product': 6, 'sessions_per_product': 2, 'metrics_format': METRICS,
        'capture_ranges': [['-S', '-', '-E', '-'], ['-S', '-999', '-E', '999']],
        'phases': {phase: {'witness_argv': {label: witness_argv(config, phase, label) for label in LABELS}, 'environment': B.environment(config, phase)} for phase in ('oracle', 'candidate')},
        'golden_policy': 'Complete native oracle only; no preselected small-history retention. Labels retained for pinned015 lifecycle compatibility; numbers in labels are not capacities.'})
    B.write_once(root / 'PURE-CHECKS.json', pure_checks(config))


def oracle_gate(record, result_sha):
    B.equal(record['status'], 'PASS', 'complete oracle required')
    B.equal([x['id'] for x in record['cases']], ['Z1', 'Z2', 'Z3'], 'all oracle cases')
    B.require(all(x['status'] == 'PASS' for x in record['cases']), 'oracle case failed')
    B.equal([c['key'] for c in record['captures']], list(KEYS), 'all 26 oracle captures')
    B.require(all(c['status'] == 'PASS' for c in record['captures']), 'oracle capture failed')
    B.equal([(e['label'], e['status']) for e in record['exits']], [(label, 'PASS') for label in LABELS], 'six normal oracle exits required')
    B.equal(record['cleanup']['status'], 'PASS', 'oracle cleanup required')
    return {'at': B.now(), 'status': 'PASS', 'driver_sha256': B.digest(__file__), 'base_sha256': B.digest(BASE),
            'result_sha256': result_sha, 'captures': {c['key']: c['final'] for c in record['captures']}}


def execute_lot(config, config_path):
    root = Path(config['root'])
    frozen = json.loads((root / 'FROZEN-NATIVE.json').read_text())
    pins = {x['path']: x['sha256'] for x in frozen['artifacts']}
    mandatory = [str(Path(__file__).resolve()), str(BASE), str(Path(config_path).resolve()),
                 str(root / 'PLAN.json'), str(root / 'PURE-CHECKS.json')]
    mandatory += [config[k] for k in ('oracle', 'candidate', 'daemon', 'python')]
    mandatory += [str(root / p / f) for p in ('oracle', 'candidate') for f in ('startup.conf', 'live.conf')]
    B.require(all(p in pins for p in mandatory), 'freeze incomplete')
    for p, sha in pins.items():
        B.equal(B.digest(p), sha, 'frozen file changed ' + p)
    for k, sha in config['hashes'].items():
        B.equal(B.digest(config[k]), sha, 'binary pin ' + k)
    pure = json.loads((root / 'PURE-CHECKS.json').read_text())
    B.require(pure['status'] == 'PASS' and pure['driver_sha256'] == B.digest(__file__), 'pure checks absent')
    B.require(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0, 'subreaper unavailable')
    B.write_once(root / 'STARTED.json', {'at': B.now(), 'identity': B.proc(os.getpid())})
    start = time.monotonic()
    lot = {'start': B.now(), 'phases': [], 'frozen_sha256': B.digest(root / 'FROZEN-NATIVE.json'), 'fd_before': B.fd_snapshot()}
    for phase in ('oracle', 'candidate'):
        if phase == 'candidate':
            gate = json.loads((root / 'ORACLE-FROZEN.json').read_text())
            B.equal(gate['driver_sha256'], B.digest(__file__), 'gate driver')
            B.equal(gate['result_sha256'], B.digest(root / 'oracle/RESULT.json'), 'gate oracle result')
            B.equal(gate['status'], 'PASS', 'oracle gate status')
        run = Run(config, phase, time.monotonic())
        try:
            run.execute()
            run.record['status'] = 'PASS'
        except Exception as error:
            run.record.update(status='FAIL' if isinstance(error, B.Failed) else 'UNPROVEN', error=str(error), traceback=traceback.format_exc())
        finally:
            try:
                run.cleanup()
            except Exception as error:
                run.record.update(status='UNPROVEN', cleanup_error=repr(error))
            run.record.update(end=B.now(), spawn_commands=run.spawn_count)
            B.write_once(run.root / 'RESULT.json', run.record)
        lot['phases'].append({'phase': phase, 'status': run.record['status'], 'result_sha256': B.digest(run.root / 'RESULT.json')})
        print(json.dumps(lot['phases'][-1]), flush=True)
        if run.record['status'] != 'PASS':
            break
        if phase == 'oracle':
            B.write_once(root / 'ORACLE-FROZEN.json', oracle_gate(run.record, B.digest(run.root / 'RESULT.json')))
    lot.update(end=B.now(), elapsed_seconds=time.monotonic() - start, fd_after=B.fd_snapshot())
    lot['status'] = 'PASS' if len(lot['phases']) == 2 and all(p['status'] == 'PASS' for p in lot['phases']) else 'STOPPED'
    if lot['fd_before'] != lot['fd_after'] or lot['elapsed_seconds'] > 360:
        lot['status'] = 'UNPROVEN'
    B.write_once(root / 'LOT.json', lot)
    print(json.dumps(lot), flush=True)
    return 0 if lot['status'] == 'PASS' else 1


if __name__ == '__main__':
    if len(sys.argv) == 6 and sys.argv[1] == 'witness':
        witness(*sys.argv[2:6])
    elif len(sys.argv) == 3 and sys.argv[1] in ('prepare', 'run'):
        config = json.loads(Path(sys.argv[2]).read_text())
        if sys.argv[1] == 'prepare':
            prepare(config)
        else:
            sys.exit(execute_lot(config, sys.argv[2]))
    else:
        raise SystemExit(__doc__)
