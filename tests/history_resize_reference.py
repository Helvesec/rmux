#!/usr/bin/env python3
"""Bounded Linux vertical history-resize oracle; no runtime changes or builds.

Usage: python3 -I tests/history_resize_reference.py prepare CONFIG.json
       python3 -I tests/history_resize_reference.py run CONFIG.json

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
LABEL = 'a20'
KEYS = ('S1:initial', 'S2:shrink11', 'S2:grow12', 'S3:shrink8', 'S3:grow12', 'S4:plus6')
HEIGHTS = dict(zip(KEYS, (12, 11, 12, 8, 12, 12)))
METRICS = '#{pane_width}|#{pane_height}|#{history_size}|#{history_limit}|#{cursor_x}|#{cursor_y}'


def payload(first, stop):
    return b''.join(B.line(LABEL, i) for i in range(first, stop))


def witness_argv(config, phase):
    return [config['python'], '-I', str(Path(__file__).resolve()), 'witness', LABEL,
            str(Path(config['root']) / phase / 'markers'), config['tokens'][phase], '0']


def witness(label, root, token, code):
    B.equal((label, code), (LABEL, '0'), 'fixed witness arguments')
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
        data = payload(first, stop)
        sent = 0
        while sent < len(data):
            written = os.write(1, data[sent:])
            B.require(written > 0, 'short helper emission')
            sent += written
        return {'completed_at': B.now(), 'bytes': len(payload(0, stop)), 'lines': stop,
                'sha256': hashlib.sha256(payload(0, stop)).hexdigest(),
                'last_batch': {'first': first, 'stop': stop, 'bytes': sent,
                               'sha256': hashlib.sha256(data).hexdigest()}}
    B.atomic_once(root / ('born-' + stem + '.json'), report(None))
    emission = emit(0, 81)
    B.atomic_once(root / ('emitted-' + stem + '.json'), report(None))
    advanced = False
    end = time.monotonic() + 180
    while time.monotonic() < end:
        advance = root / ('advance-' + stem + '.json')
        if advance.exists() and not advanced:
            request = json.loads(advance.read_text())
            B.require(B.same(request['identity'], own), 'advance birth mismatch')
            B.equal((request['first'], request['stop']), (81, 87), 'six fixed new lines')
            emission = emit(81, 87)
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


def inventory(raw, metrics, stop, height):
    B.equal((metrics['width'], metrics['height'], metrics['history_limit']), (80, height, 20), 'geometry/limit')
    B.require(0 <= metrics['cursor_x'] < 80 and 0 <= metrics['cursor_y'] < height, 'cursor bounds')
    B.require(raw.endswith(b'\n'), 'capture framing newline absent')
    rows = raw.split(b'\n')[:-1]
    nonblank = [row for row in rows if row]
    B.require(nonblank, 'empty capture')
    indices = []
    for row in nonblank:
        prefix = b'HIST|a20|'
        B.require(row.startswith(prefix), 'foreign line')
        suffix = row[len(prefix):]
        B.require(len(suffix) == 4 and suffix.isdigit(), 'invalid index')
        index = int(suffix)
        B.equal(row + b'\n', B.line(LABEL, index), 'exact line')
        indices.append(index)
    B.require(0 <= indices[0] < stop, 'index bounds')
    B.equal(indices, list(range(indices[0], stop)), 'contiguous suffix to last emission')
    B.equal(rows, nonblank + [b''] * (len(rows) - len(nonblank)), 'interior blank')
    B.equal(len(rows), metrics['history_size'] + height, 'history plus viewport rows')
    # Height transfers may exceed hlimit: observe that value, never clamp it.
    B.require(0 <= metrics['history_size'] <= 128, 'bounded inventory')
    return {'indices': indices, 'first': indices[0], 'last': indices[-1],
            'nonblank_rows': len(nonblank), 'blank_rows': len(rows) - len(nonblank),
            'rows': len(rows), 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}


def exact(actual, golden):
    B.equal(actual, golden, 'exact oracle bytes, indices, history, geometry and cursor')


def pure_checks(config):
    inherited = B.pure_checks(config)
    metrics = {'width': 80, 'height': 11, 'history_size': 21, 'history_limit': 20, 'cursor_x': 0, 'cursor_y': 10}
    def capture(first, m):
        raw = payload(first, 81) + b'\n'
        return {'raw_base64': base64.b64encode(raw).decode(), 'metrics': m,
                'inventory': inventory(raw, m, 81, m['height'])}
    # Synthetic controls of observer sensitivity, never a native golden.
    good = capture(50, metrics)
    bad_batch = capture(52, {**metrics, 'history_size': 19})
    stale = capture(50, {**metrics, 'height': 12, 'history_size': 20, 'cursor_y': 11})
    lost = capture(51, {**metrics, 'history_size': 20})
    checks = []
    for name, bad in [('batch2_wrongly_applied_to_resize', bad_batch),
                      ('stale_capture_before_resize', stale), ('lost_oldest_line', lost)]:
        try:
            exact(bad, good)
        except B.Failed:
            checks.append(name)
        else:
            raise AssertionError('insensitive observer: ' + name)
    return {'at': B.now(), 'status': 'PASS', 'driver_sha256': B.digest(__file__),
            'base_sha256': B.digest(BASE), 'inherited': inherited, 'rejected': checks,
            'synthetic_only': True, 'product_runs': 0}


class Run(B.Run):
    def expected(self, label):
        B.equal(label, LABEL, 'one helper')
        return {'kind': LABEL, 'exit_code': 0, 'token': self.config['tokens'][self.phase],
                'exe': str(Path(self.config['python']).resolve()), 'exe_sha256': self.config['hashes']['python'],
                'argv': witness_argv(self.config, self.phase), 'cwd': str(self.root / 'cwd'),
                'environment': {k: self.env.get(k) for k in B.ENV_KEYS}}

    def advance(self):
        child = self.children[LABEL]
        own = child['child']['before']
        B.require(B.snapshot_same(child['child'], B.proc(own['pid'])), 'advance child changed')
        stem = f"{own['pid']}-{own['start_ticks']}"
        request = {'identity': own, 'challenge': secrets.token_hex(24), 'issued_at': B.now(), 'first': 81, 'stop': 87}
        path = self.root / 'markers' / ('advance-' + stem + '.json')
        B.atomic_once(path, request)
        ack_path = path.with_name('advanced-' + stem + '.json')
        ack = self.wait_json(ack_path)
        B.check_marker(ack, child['child'], child['expected'], request['challenge'])
        B.require(request['issued_at'] <= ack['emission']['completed_at'] <= ack['at'], 'advance ordering')
        self.record['advance'] = {'request': request, 'request_path': str(path), 'ack': ack, 'ack_path': str(ack_path)}

    def capture_stage(self, key):
        target = 'alpha:a20.0'
        child = self.children[LABEL]
        latest = self.record['observations'][-1]
        stop = 87 if key == KEYS[-1] else 81
        emission = latest['ack']['emission']
        B.equal(emission['bytes'], len(payload(0, stop)), 'complete emission bytes')
        B.equal(emission['lines'], stop, 'complete emission lines')
        B.equal(emission['sha256'], hashlib.sha256(payload(0, stop)).hexdigest(), 'complete emission hash')
        B.require(emission['completed_at'] <= latest['request']['issued_at'] <= latest['ack']['at'], 'emission then fresh challenge')
        item = {'key': key, 'label': LABEL, 'target': target, 'at': B.now(), 'public': latest['public'],
                'identity': child['child']['before'], 'challenge': latest['request']['challenge'],
                'emission': emission, 'samples': [], 'status': 'UNPROVEN'}
        self.record['captures'].append(item)
        previous = None
        try:
            while True:
                self.remaining()
                raw = self.command(['capture-pane', '-p', '-t', target, '-S', '-', '-E', '-'], raw=True)
                self.capture_bytes += len(raw)
                B.require(self.capture_bytes + len(payload(0, 87)) <= 65536, 'product text cap')
                values = self.command(['display-message', '-p', '-t', target, METRICS]).split('|')
                B.require(len(values) == 6 and all(v.isdigit() for v in values), 'geometry/history/cursor API unavailable')
                metrics = dict(zip(('width', 'height', 'history_size', 'history_limit', 'cursor_x', 'cursor_y'), map(int, values)))
                sample = {'at': B.now(), 'raw_base64': base64.b64encode(raw).decode(), 'metrics': metrics}
                item['samples'].append(sample)
                if B.line(LABEL, stop - 1)[:-1] not in raw.split(b'\n'):
                    time.sleep(min(.04, self.remaining()))
                    continue
                stable = {'raw_base64': sample['raw_base64'], 'metrics': metrics,
                          'inventory': inventory(raw, metrics, stop, HEIGHTS[key])}
                if previous == stable:
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

    def execute(self):
        B.require(not self.socket.exists() and not list((self.root / 'markers').iterdir()), 'fixture not fresh')
        self.profiles()
        try:
            for case in ('S1', 'S2', 'S3', 'S4'):
                self.case_end = min(time.monotonic() + 20, self.start + 150)
                entry = {'id': case, 'start': B.now(), 'monotonic_start': time.monotonic()}
                self.record['cases'].append(entry)
                try:
                    if case == 'S1':
                        self.spawn_count += 1
                        self.command(['new-session', '-d', '-s', 'alpha', '-n', LABEL, '-x', '80', '-y', '12',
                                      '-c', str(self.root / 'cwd'), *witness_argv(self.config, self.phase)], startup=True)
                        self.observe(LABEL, 'alpha:a20.0')
                        self.capture_stage(KEYS[0])
                    elif case in ('S2', 'S3'):
                        keys = KEYS[1:3] if case == 'S2' else KEYS[3:5]
                        for key in keys:
                            self.command(['resize-window', '-t', 'alpha:a20', '-x', '80', '-y', str(HEIGHTS[key])])
                            self.observe(LABEL, 'alpha:a20.0', True)
                            self.capture_stage(key)
                    else:
                        self.advance()
                        self.observe(LABEL, 'alpha:a20.0', True)
                        self.capture_stage(KEYS[-1])
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
            # A product mismatch still receives a commanded, observed normal exit.
            if LABEL in self.children:
                self.case_end = min(time.monotonic() + 20, self.start + 150)
                self.exit_witness(LABEL, True)


def prepare(config):
    root = Path(config['root'])
    for phase in ('oracle', 'candidate'):
        p = root / phase
        p.mkdir()
        for name in ('home-empty', 'xdg-empty', 'markers', 'cwd'):
            (p / name).mkdir()
        (p / 'startup.conf').write_text("set-option -g default-shell /bin/bash\nset-option -g default-command ''\nset-window-option -g remain-on-exit on\nset-option -g history-limit 20\n")
    B.write_once(root / 'PLAN.json', {'keys': KEYS, 'heights': HEIGHTS, 'width': 80, 'limit': 20,
        'initial_lines': [0, 80], 'additional_lines': [81, 86], 'children_per_product': 1,
        'metrics_format': METRICS, 'capture_argv': ['capture-pane', '-p', '-t', 'alpha:a20.0', '-S', '-', '-E', '-'],
        'resize_argv_template': ['resize-window', '-t', 'alpha:a20', '-x', '80', '-y', 'HEIGHT'],
        'phases': {phase: {'witness_argv': witness_argv(config, phase), 'environment': B.environment(config, phase)} for phase in ('oracle', 'candidate')},
        'golden_policy': 'Complete native oracle only; no preselected content or size after resize.'})
    B.write_once(root / 'PURE-CHECKS.json', pure_checks(config))


def oracle_gate(record, result_sha):
    B.equal(record['status'], 'PASS', 'complete oracle required')
    B.equal([x['id'] for x in record['cases']], ['S1', 'S2', 'S3', 'S4'], 'all oracle cases')
    B.require(all(x['status'] == 'PASS' for x in record['cases']), 'oracle case failed')
    B.equal([c['key'] for c in record['captures']], list(KEYS), 'all six oracle captures')
    B.require(all(c['status'] == 'PASS' for c in record['captures']), 'oracle capture failed')
    B.equal([e['status'] for e in record['exits']], ['PASS'], 'normal oracle exit required')
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
    mandatory += [str(root / p / 'startup.conf') for p in ('oracle', 'candidate')]
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
