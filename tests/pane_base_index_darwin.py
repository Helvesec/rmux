#!/usr/bin/env python3
"""Darwin P1--P4: pane indices and actual canonical PTY input routing.

Uses only pinned acquired011/012 Darwin APIs. Linux021 supplies the scenario,
not native evidence. No import or source dependency on pending014. Each product
lot and both absolute argv plans are frozen before a fresh Mac oracle runs.
"""
import base64
import copy
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import secrets
import select
import sys
import termios
import time
import traceback
from contextlib import chdir
sys.dont_write_bytecode = True
C = D = None
FORMAT = '#{pid}|#{socket_path}|#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{pane_index}|#{window_index}|#{window_width}|#{window_height}'
FIELDS = ('daemon_pid', 'socket', 'session_id', 'window_id', 'pane_id', 'pane_pid', 'pane_tty', 'pane_dead', 'pane_dead_status', 'pane_dead_signal', 'pane_index', 'window_index', 'window_width', 'window_height')
KINDS = ('alpha1', 'alpha2', 'beta1', 'beta2', 'alpha3')
EXIT_ORDER = ('alpha3', 'alpha2', 'alpha1', 'beta2', 'beta1')
EXPECTED = {'P1': {'alpha1': 4, 'alpha2': 5},
            'P2': {'alpha1': 7, 'alpha2': 8, 'beta1': 4, 'beta2': 5},
            'P3': {'alpha1': 7, 'alpha2': 8, 'beta1': 4, 'beta2': 5},
            'P4': {'alpha1': 4, 'alpha2': 5, 'alpha3': 6, 'beta1': 4, 'beta2': 5}}
ENV_KEYS = ('HOME', 'XDG_CONFIG_HOME', 'PATH', 'L6_PANEINDEX_MARKER', 'BASH_ENV', 'ENV', 'RMUX_DISABLE_TMUX_SHIM', 'L5_PANEINDEX_API')
FILE_CAP = 4096
RECORD_CAP = 800 * 1024
LOT_CAP = 2 * 1024**2
STDOUT_CAP = 256 * 1024

class Unproven(RuntimeError): pass
class Failed(RuntimeError): pass
def require(ok, message):
    if not ok: raise Unproven(message)
def equal(actual, expected, message):
    if actual != expected: raise Failed(f'{message}: {actual!r} != {expected!r}')
def now(): return datetime.datetime.now(datetime.timezone.utc).isoformat()
def digest(path):
    with Path(path).open('rb') as f: return hashlib.file_digest(f, 'sha256').hexdigest()
def pin(path):
    path = Path(path)
    return {'path': str(path), 'bytes': path.stat().st_size, 'sha256': digest(path)}
def encoded(value): return (json.dumps(value, separators=(',', ':')) + '\n').encode()
def write_once(path, value, cap=RECORD_CAP):
    raw = encoded(value); require(len(raw) <= cap, 'output cap before write')
    with Path(path).open('xb') as f: f.write(raw); f.flush(); os.fsync(f.fileno())
def atomic_once(path, value):
    path = Path(path); require(not path.exists(), 'immutable marker exists')
    temp = path.with_suffix(path.suffix + '.tmp')
    write_once(temp, value, FILE_CAP); os.rename(temp, path)
def stem(own): return str(own['pid']) + '-' + '-'.join(map(str, own['birth']))

def bind_api(config):
    global C, D, Unproven, Failed
    ap = config['adapter_pin']
    require(pin(ap['path']) == ap, 'acquired012 adapter changed')
    require(ap['sha256'] == '3bb2a8a9faba1b860f00d7641696af364fddbaa48e28b79d846fa0249b14a7bb', 'unqualified adapter')
    spec = importlib.util.spec_from_file_location('paneindex_acquired012', ap['path'])
    C = importlib.util.module_from_spec(spec); spec.loader.exec_module(C)
    C.bind_api(config['api_pins']); D = C.D
    Unproven, Failed = D.Unproven, D.Failed
    # The acquired parser is unchanged; only its explicit allowlist is scoped
    # to this new observer. No on-disk API edits and no environment tail saved.
    C.ENV_KEYS = ENV_KEYS

def environment(config, phase):
    root = Path(config['roots'][phase])
    return {'HOME': str(root/'home'), 'XDG_CONFIG_HOME': str(root/'xdg'),
            'PATH': '/usr/bin:/bin', 'SHELL': '/bin/bash', 'TERM': 'vt100', 'LANG': 'C',
            'RMUX_DISABLE_TMUX_FALLBACK': '1', 'RMUX_DISABLE_TMUX_SHIM': '1',
            'L6_PANEINDEX_MARKER': config['tokens'][phase], 'L5_PANEINDEX_API': config['config_path']}
def witness_argv(config, phase, label):
    return [config['pins']['python']['path'], '-I', config['driver'], 'witness', label,
            str(Path(config['roots'][phase])/'markers'), config['tokens'][phase], '0']
def plan(config, phase):
    cwd = str(Path(config['roots'][phase])/'cwd')
    def session(name, label):
        return ['new-session', '-d', '-s', name, '-x', '80', '-y', '24', '-c', cwd, *witness_argv(config, phase, label)]
    def split(target, label):
        return ['split-window', '-d', '-P', '-F', '#{pane_id}', '-t', target, '-c', cwd, *witness_argv(config, phase, label)]
    return [('P1', 'alpha1', session('alpha', 'alpha1'), 'alpha:0.4'),
            ('P1', 'alpha2', split('alpha:0.4', 'alpha2'), 'PRINTED_PANE_ID'),
            ('P2', 'beta1', session('beta', 'beta1'), 'beta:0.4'),
            ('P2', 'beta2', split('beta:0.4', 'beta2'), 'PRINTED_PANE_ID'),
            ('P4', 'alpha3', split('alpha:0.5', 'alpha3'), 'PRINTED_PANE_ID')]

def witness(kind, root, token, exit_code):
    bind_api(json.loads(Path(os.environ['L5_PANEINDEX_API']).read_text()))
    require(kind in KINDS and exit_code == '0', 'unplanned helper')
    root = Path(root); own = D.birth(os.getpid()); key = stem(own)
    tty = termios.tcgetattr(0)
    tty_state = {'icanon': bool(tty[3] & termios.ICANON), 'icrnl': bool(tty[0] & termios.ICRNL)}
    require(all(tty_state.values()), 'canonical PTY and CR-to-NL required')
    received, buffer = [], b''
    def report(challenge):
        return {'at': now(), 'kind': kind, 'token': token, 'challenge': challenge,
                'exit_code': 0, 'identity': D.birth(os.getpid()), 'cwd': os.getcwd(),
                'argv': sys.argv, 'tty': tty_state, 'received': list(received),
                'pending_bytes_base64': base64.b64encode(buffer).decode(),
                'environment': {k: os.environ.get(k) for k in ENV_KEYS}}
    atomic_once(root/('born-'+key+'.json'), report(None))
    end = time.monotonic() + 180
    while time.monotonic() < end:
        while select.select([0], [], [], 0)[0]:
            data = os.read(0, FILE_CAP); require(data, 'unexpected stdin EOF')
            buffer += data; require(len(buffer) <= FILE_CAP and len(received) < 8, 'stdin cap')
            while b'\n' in buffer:
                line, buffer = buffer.split(b'\n', 1)
                require(len(line) == 48 and all(c in b'0123456789abcdef' for c in line), 'unplanned PTY input')
                received.append(line.decode('ascii'))
                receipt = report(None); receipt['stdin_bytes_base64'] = base64.b64encode(line+b'\n').decode()
                atomic_once(root/(f'stdin-{key}-{len(received)}.json'), receipt)
        requests = sorted(root.glob('request-'+key+'-*.json'))
        require(len(requests) <= 16, 'liveness challenge cap')
        for path in requests:
            ack = root/path.name.replace('request-', 'ack-', 1)
            if not ack.exists():
                require(path.stat().st_size <= FILE_CAP, 'request cap')
                request = json.loads(path.read_text())
                require(set(request) == {'identity', 'challenge', 'issued_at'}, 'liveness request schema')
                require(D.same(own, request['identity']), 'liveness PID/birth mismatch')
                atomic_once(ack, report(request['challenge']))
        release = root/('exit-'+key+'.json')
        if release.exists():
            require(release.stat().st_size <= FILE_CAP, 'exit request cap')
            request = json.loads(release.read_text())
            require(D.same(own, request['identity']), 'exit PID/birth mismatch')
            equal(request['exit_code'], 0, 'fixed exit intent')
            atomic_once(root/('exit-intent-'+key+'.json'), report(request['challenge']))
            os._exit(0)
        time.sleep(.02)
    os._exit(124)

def stable_public(a, b):
    return {k: v for k, v in a.items() if k != 'pane_index'} == {k: v for k, v in b.items() if k != 'pane_index'}
def check_indices(rows, children, expected):
    equal(len(rows), len(expected), 'exact pane inventory')
    require(len({r['pane_id'] for r in rows}) == len(rows), 'duplicate pane ID')
    require(len({(r['window_id'], r['pane_index']) for r in rows}) == len(rows), 'duplicate pane index')
    sessions = {r['session_id'] for r in rows}; windows = {r['window_id'] for r in rows}
    equal(len(sessions), 1 if len(expected) == 2 else 2, 'exact session count')
    equal(len(windows), len(sessions), 'exact window count')
    for label, index in expected.items():
        old = children[label]['public']; found = [r for r in rows if r['pane_id'] == old['pane_id']]
        require(len(found) == 1 and stable_public(old, found[0]), 'stable pane attribution')
        row = found[0]; equal(row['pane_index'], str(index), 'pane index '+label)
        equal((row['window_index'], row['window_width'], row['window_height']), ('0', '80', '24'), 'window index/geometry')
        equal(row['pane_dead'], '0', 'pane alive')
def delivery_snapshot(value): return copy.deepcopy(value)
def check_delivery(received, before, target, nonce):
    equal(set(received), set(before), 'delivery population')
    require(target in before and all(nonce not in v for v in before.values()), 'unknown target/stale nonce')
    for label in before:
        equal(received[label], before[label]+([nonce] if label == target else []), 'exclusive stdin '+label)
def check_marker(marker, child, expected, challenge):
    C.check_marker(marker, child, expected, challenge)
    equal(marker['exit_code'], 0, 'exit intent only')
    equal(marker['tty'], {'icanon': True, 'icrnl': True}, 'canonical PTY observed without mutation')
def check_stdin(receipt, nonce, expected_received):
    equal(base64.b64decode(receipt['stdin_bytes_base64'], validate=True), (nonce+'\n').encode(), 'actual stdin bytes')
    equal(receipt['received'], expected_received, 'cumulative stdin receipt')
    equal(receipt['pending_bytes_base64'], '', 'no partial stdin')
def command_budget(record, cleaning=False):
    require(len(record['commands']) < (160 if cleaning else 158), 'CLI count before exec')
    used = sum(len(c.get('stdout', '').encode()) for c in record['commands'])
    require(used+FILE_CAP <= STDOUT_CAP, 'stdout reserve before exec')
    require(len(encoded(record)) < 720*1024, 'record reserve before exec')
def check_exit(row):
    require(row['exit_event'] and not D.same(row['before'], row['after']), 'NOTE_EXIT and absent birth required')
    equal(row['numeric_status_claimed'], False, 'no nonparent status credit')
def check_cleanup(row, owned):
    C.check_cleanup(row)
    expected = {(v['pid'], tuple(v['birth'])) for v in owned.values()}
    actual = {(p['before']['pid'], tuple(p['before']['birth'])) for p in row['processes']}
    require(expected and actual == expected and len(row['processes']) == len(expected), 'complete cleanup population')

class Run:
    def __init__(self, frozen):
        D.Run.__init__(self, frozen, None)
        self.config = frozen['config']; self.env = environment(self.config, self.phase)
        self.children = self.observed; self.received = {}; self.all_nonces = set()
        self.record.update(environment=self.env, inventories=[], deliveries=[], exits=[], marker_ledgers=[])
    def remaining(self):
        end = min(self.start+180, self.cleanup_end) if self.cleaning else min(self.case_end, self.start+150)
        left = end-time.monotonic(); require(left > 0, 'native deadline exceeded'); return min(5, left)
    def socket_info(self): return D.Run.socket_info(self)
    def profiles(self, post_shutdown=False): return C.Run.profiles(self, post_shutdown)
    def wait_json(self, path): return C.Run.wait_json(self, path)
    def snapshot(self, pid): return C.Run.snapshot(self, pid)
    def track(self, row, role):
        result = D.Run.track(self, row, role)
        require(len(self.owned) <= 7, 'one daemon/six child budget')
        return result
    def helpers(self):
        for parent in list(self.owned.values()):
            if not D.same(parent, D.birth(parent['pid'])): continue
            for pid in D.children(parent['pid']):
                own = D.birth(pid)
                if own is None or own['ppid'] != parent['pid'] or pid in self.owned: continue
                self.track(own, 'unexpected-helper')
                self.record['helpers'].append({'at': now(), 'parent': parent, 'identity': own})
                require(self.cleaning, 'unexpected private helper')
    def checkpoint(self):
        raw = encoded(self.record); require(len(raw) <= RECORD_CAP, 'record pre-write cap')
        tmp = self.root/'progress.tmp'; tmp.write_bytes(raw); os.replace(tmp, self.root/'progress.json')
    def fresh_nonce(self):
        nonce = secrets.token_hex(24); require(nonce not in self.all_nonces, 'duplicate nonce')
        self.all_nonces.add(nonce); return nonce
    def command(self, args, startup=False, required=True):
        command_budget(self.record, self.cleaning)
        argv = [self.binary, *([] if startup else ['-N']), '-S', str(self.socket)]
        if startup: argv += ['-f', str(self.root/'startup.conf')]
        argv += args
        entry = {'start': now(), 'argv': argv, 'pre_exec_file_cap': FILE_CAP}; self.record['commands'].append(entry)
        old = resource.getrlimit(resource.RLIMIT_FSIZE)
        require(old[0] == resource.RLIM_INFINITY or old[0] >= FILE_CAP, 'file limit too small')
        try:
            resource.setrlimit(resource.RLIMIT_FSIZE, (FILE_CAP, old[1]))
            with chdir(self.root): result = D.native.run(argv, env=self.env, timeout=self.remaining(), check=False)
            entry.update(pid=result.pid, returncode=result.returncode, stdout=result.stdout.decode(), stderr=result.stderr.decode())
            require(len(result.stdout) < FILE_CAP and len(result.stderr) < FILE_CAP, 'CLI output reached cap')
        except BaseException as error:
            entry['error'] = repr(error); raise
        finally:
            resource.setrlimit(resource.RLIMIT_FSIZE, old); entry['end'] = now()
        if required: require(result.returncode == 0, 'CLI failed: '+repr(args))
        return result.stdout.decode().strip()
    def parse_public(self, text):
        fields = text.split('|'); require(len(fields) == len(FIELDS), 'public format incomplete')
        row = dict(zip(FIELDS, fields)); row['daemon_pid'] = int(row['daemon_pid']); row['pane_pid'] = int(row['pane_pid'])
        require(row['socket'] == str(self.socket), 'endpoint escaped')
        return row
    def public(self, target):
        public = self.parse_public(self.command(['display-message', '-p', '-t', target, FORMAT]))
        endpoint = self.socket_info(); require(endpoint is not None, 'endpoint missing')
        daemon = D.process(public['daemon_pid']); self.track(daemon['before'], 'daemon')
        key = 'tmux' if self.phase == 'oracle' else 'rmux_daemon'
        equal(daemon['image']['executable'], self.config['pins'][key]['path'], 'daemon executable')
        equal(daemon['image']['sha256'], self.config['pins'][key]['sha256'], 'daemon disk hash')
        if self.daemon is None: self.daemon, self.endpoint = daemon, endpoint
        else: require(D.stable(self.daemon, daemon) and self.endpoint == endpoint, 'daemon/socket changed')
        row = {'at': now(), 'target': target, 'public': public, 'daemon': daemon, 'endpoint': endpoint}
        self.record['public_samples'].append(row); return row
    def expected_for(self, label):
        python = self.config['pins']['python']
        return {'kind': label, 'token': self.config['tokens'][self.phase], 'exe': python['path'],
                'exe_sha256': python['sha256'], 'argv': witness_argv(self.config, self.phase, label),
                'cwd': str(self.root/'cwd'), 'environment': {k: self.env.get(k) for k in ENV_KEYS}}
    def observe(self, label, target, reobserve=False):
        pub = self.public(target); public = pub['public']; own = D.birth(public['pane_pid'])
        require(own is not None, 'child absent'); self.track(own, 'pane')
        key = stem(own); markers = self.root/'markers'
        item = {'label': label, 'target': target, 'reobserve': reobserve, 'at': now(),
                'expected': self.expected_for(label), 'public': public, 'status': 'UNPROVEN'}
        index = len(self.record['observations']); self.record['observations'].append(item)
        try:
            path = markers/('born-'+key+'.json'); born = self.wait_json(path); first = self.snapshot(own['pid'])
            item.update(born=born, born_path=str(path), first=first); self.checkpoint()
            check_marker(born, first, item['expected'], None); equal(born['received'], [], 'fresh helper stdin')
            nonce = self.fresh_nonce(); request = {'identity': own, 'challenge': nonce, 'issued_at': now()}
            path = markers/(f'request-{key}-{index}.json'); item.update(request=request, request_path=str(path))
            atomic_once(path, request); ack_path = markers/path.name.replace('request-', 'ack-', 1)
            ack = self.wait_json(ack_path); second = self.snapshot(own['pid'])
            item.update(ack=ack, ack_path=str(ack_path), child=second)
            after = self.public(target); item['public_after'] = after['public']
            equal(after['public'], public, 'public stability across liveness challenge')
            C.check_child(first, second, public, after['daemon'], item['expected'])
            check_marker(ack, second, item['expected'], nonce)
            if reobserve:
                old = self.children[label]
                require(D.stable(old['child'], second) and stable_public(old['public'], public), 'old child/ID changed')
            else: self.children[label] = item
            self.helpers(); item['status'] = 'PASS'; return item
        except Exception as error:
            item.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error)); raise
        finally: self.checkpoint()
    def options(self, target, effective, local):
        equal(self.command(['show-options', '-wgv', 'pane-base-index']), '4', 'global pane index')
        equal(self.command(['show-options', '-wAv', '-t', target, 'pane-base-index']), str(effective), 'effective pane index')
        equal(self.command(['show-options', '-wqv', '-t', target, 'pane-base-index']), '' if local is None else str(local), 'local pane index')
        equal(self.command(['show-options', '-gv', 'base-index']), '0', 'window base fixed')
        equal(self.command(['show-options', '-gv', 'renumber-windows']), 'off', 'renumber fixed')
    def inventory(self, case):
        entry = {'case': case, 'expected': EXPECTED[case], 'samples': []}; self.record['inventories'].append(entry)
        for _ in range(2):
            rows = [self.parse_public(s) for s in self.command(['list-panes', '-a', '-F', FORMAT]).splitlines()]
            entry['samples'].append({'at': now(), 'rows': rows}); check_indices(rows, self.children, EXPECTED[case])
            time.sleep(min(.03, self.remaining()))
        equal(entry['samples'][0]['rows'], entry['samples'][1]['rows'], 'double public inventory')
    def delivery(self, case, target, address_kind):
        nonce = self.fresh_nonce(); label = 'alpha2'; child = self.children[label]; old = delivery_snapshot(self.received)
        public = self.public(target)['public']; require(stable_public(child['public'], public), 'wrong input target')
        equal(public['pane_index'], str(EXPECTED[case][label]), 'current numeric target')
        entry = {'case': case, 'at': now(), 'target': target, 'address_kind': address_kind, 'target_label': label,
                 'nonce': nonce, 'public_before': public, 'received_before': old, 'status': 'UNPROVEN'}
        self.record['deliveries'].append(entry)
        # Nonce enters the helper ONLY through PTY input. No request file contains
        # this value; filesystem challenges use separate fresh nonces.
        self.command(['send-keys', '-t', target, '-l', nonce]); self.command(['send-keys', '-t', target, 'Enter'])
        path = self.root/'markers'/(f"stdin-{stem(child['child']['before'])}-{len(old[label])+1}.json")
        receipt = self.wait_json(path); entry.update(receipt=receipt, receipt_path=str(path))
        check_marker(receipt, child['child'], child['expected'], None)
        check_stdin(receipt, nonce, old[label]+[nonce])
        observed = {}; entry['observation_indices'] = []
        for name, original in self.children.items():
            entry['observation_indices'].append(len(self.record['observations']))
            item = self.observe(name, original['public']['pane_id'], True)
            equal(item['ack']['pending_bytes_base64'], '', 'no partial unexpected input')
            observed[name] = item['ack']['received']
        check_delivery(observed, old, label, nonce)
        entry['received_after'] = delivery_snapshot(observed); self.received = delivery_snapshot(observed)
        after = self.public(target)['public']; equal(after, public, 'target after actual input')
        entry.update(public_after=after, status='PASS', end=now()); self.checkpoint()
    def spawn(self, label):
        rows = [r for r in plan(self.config, self.phase) if r[1] == label]
        require(len(rows) == 1 and self.spawn_count < 5, 'planned birth count')
        case, _, args, target = rows[0]; self.spawn_count += 1
        output = self.command(args, startup=label == 'alpha1')
        if target == 'PRINTED_PANE_ID':
            require(output.startswith('%') and output[1:].isdigit(), 'printed split ID missing'); target = output
        item = self.observe(label, target)
        equal(item['public']['pane_index'], str(EXPECTED[case][label]), 'new pane index')
        equal(item['ack']['received'], [], 'new pane stdin empty'); self.received[label] = []
    def ledger(self): return C.Run.ledger(self)
    def exit_witness(self, label):
        child = self.children[label]; own = child['child']['before']; watch = self.watches[own['pid']]
        current = self.snapshot(own['pid']); require(D.stable(child['child'], current), 'old child before exit')
        require(not watch.poll_exit() and D.same(own, D.birth(own['pid'])), 'child exited before release')
        row = {'label': label, 'at': now(), 'before': own, 'pre_release': current, 'numeric_status_claimed': False}
        self.record['exits'].append(row)
        nonce = self.fresh_nonce(); request = {'identity': own, 'challenge': nonce, 'exit_code': 0, 'issued_at': now()}
        path = self.root/'markers'/('exit-'+stem(own)+'.json'); row.update(request=request, request_path=str(path))
        atomic_once(path, request); intent_path = self.root/'markers'/('exit-intent-'+stem(own)+'.json')
        intent = self.wait_json(intent_path); row.update(intent=intent, intent_path=str(intent_path))
        check_marker(intent, child['child'], child['expected'], nonce)
        equal(intent['received'], self.received[label], 'final exclusive stdin'); equal(intent['pending_bytes_base64'], '', 'final partial input')
        watch.wait(self.remaining()); after = D.birth(own['pid'])
        while D.same(own, after): time.sleep(min(.02, self.remaining())); after = D.birth(own['pid'])
        row.update(exit_event=watch.exited, after=after); check_exit(row); row['status'] = 'PASS'
    def execute(self):
        require(not self.socket.exists() and not list((self.root/'markers').iterdir()), 'fixture not fresh'); self.profiles()
        for case in EXPECTED:
            self.case_end = min(time.monotonic()+20, self.start+150)
            entry = {'id': case, 'start': now(), 'monotonic_start': time.monotonic()}; self.record['cases'].append(entry)
            try:
                if case == 'P1':
                    self.spawn('alpha1'); self.options('alpha:0', 4, None); self.spawn('alpha2')
                elif case == 'P2':
                    self.spawn('beta1'); self.spawn('beta2')
                    self.command(['set-window-option', '-t', 'alpha:0', 'pane-base-index', '7'])
                    self.options('alpha:0', 7, 7); self.options('beta:0', 4, None)
                    for label, child in self.children.items(): self.observe(label, child['public']['pane_id'], True)
                elif case == 'P3': self.delivery(case, 'alpha:0.8', 'numeric')
                else:
                    self.command(['source-file', str(self.root/'live.conf')])
                    self.options('alpha:0', 4, None); self.options('beta:0', 4, None)
                    for label, child in self.children.items(): self.observe(label, child['public']['pane_id'], True)
                    self.delivery(case, 'alpha:0.5', 'numeric')
                    self.delivery(case, self.children['alpha2']['public']['pane_id'], 'stable-pane-ID')
                    self.spawn('alpha3')
                    self.observe('alpha2', self.children['alpha2']['public']['pane_id'], True)
                self.inventory(case); self.ledger(); self.profiles(); self.remaining(); entry['status'] = 'PASS'
            except Exception as error:
                entry.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=str(error)); raise
            finally:
                entry.update(end=now(), seconds=time.monotonic()-entry['monotonic_start']); self.checkpoint()
        self.case_end = min(time.monotonic()+20, self.start+150)
        for label in EXIT_ORDER: self.exit_witness(label)
        self.ledger()
    def cleanup(self):
        self.cleaning = True; self.cleanup_end = min(time.monotonic()+30, self.start+180)
        row = {'start': now(), 'errors': [], 'rescues': [], 'processes': []}; self.record['cleanup'] = row
        def attempt(name, fn):
            try: return fn()
            except Exception as error: row['errors'].append({'step': name, 'error': repr(error)})
        if self.daemon is None and self.socket.exists(): attempt('recover-endpoint', lambda: self.public('alpha'))
        if self.daemon:
            own = self.daemon['before']; watch = self.watches[own['pid']]
            def shutdown():
                if watch.poll_exit(): row['daemon_already_exited'] = True; return
                if not D.same(own, D.birth(own['pid'])):
                    watch.wait(self.remaining()); row['daemon_already_exited'] = True; return
                require(D.stable(self.daemon, D.process(own['pid'])) and self.socket_info() == self.endpoint, 'cleanup ownership changed')
                self.helpers()
                output = self.command(['kill-server'], required=False)
                row['shutdown_stdout'] = output
                # Last pane can stop tmux between identity check and kill-server.
                # Retained NOTE_EXIT and disappearance decide this race.
                watch.wait(self.remaining())
            attempt('qualified-shutdown', shutdown)
        for pid, old in list(self.owned.items()):
            watch = self.watches[pid]
            attempt('wait-'+str(pid), lambda: watch.wait(self.remaining()))
            if not watch.poll_exit() and D.same(old, D.birth(pid)):
                row['rescues'].append({'identity': old, 'signal': 9})
                attempt('owned-rescue-'+str(pid), lambda: watch.retire_after_failure(self.remaining()))
            try:
                collected, status = os.waitpid(pid, os.WNOHANG); reaped = {'pid': collected, 'raw_status': status}
            except ChildProcessError: reaped = {'ECHILD': True, 'note': 'indirect; NOTE_EXIT and birth absence separately'}
            after = D.birth(pid)
            while D.same(old, after) and time.monotonic() < self.cleanup_end:
                time.sleep(.02); after = D.birth(pid)
            row['processes'].append({'before': old, 'after': after, 'exit_event': watch.exited, 'waitpid': reaped})
        row['all_absent'] = all(p['exit_event'] and not D.same(p['before'], p['after']) for p in row['processes'])
        if row['all_absent'] and self.socket.exists() and self.socket_info() == self.endpoint:
            self.socket.unlink(); row['owned_socket_residue_removed'] = True
        row['socket_absent'] = not self.socket.exists()
        attempt('private-profiles', lambda: self.profiles(True))
        for watch in self.watches.values(): watch.close()
        row['fd_after'] = D.fd_snapshot(); row['fds_equal'] = row['fd_after'] == self.record['fd_before']; row['end'] = now()
        row['pass'] = bool(self.daemon and row['all_absent'] and row['socket_absent'] and row['fds_equal'] and not row['errors'] and not row['rescues'] and time.monotonic() <= self.cleanup_end)
        self.checkpoint(); check_cleanup(row, self.owned)

def verify_frozen(frozen):
    require(frozen['phase'] in ('oracle', 'candidate'), 'unknown phase')
    config = frozen['config']
    for item in frozen['artifacts']: require(pin(item['path']) == item, 'frozen artifact changed: '+item['path'])
    required = [str(Path(__file__).resolve()), config['config_path'], config['adapter_pin']['path'], frozen['pure']['path'],
                *[p['path'] for p in config['pins'].values()], *[p['path'] for p in config['api_pins']]]
    required += [str(Path(root)/name) for root in config['roots'].values() for name in ('startup.conf', 'live.conf')]
    require(set(required) <= {p['path'] for p in frozen['artifacts']}, 'missing provenance')
    equal(json.loads(Path(config['config_path']).read_text()), config, 'config changed')
    equal(frozen['plan'], json.loads(json.dumps(plan(config, frozen['phase']))), 'plan changed')
    equal(frozen['environment'], environment(config, frozen['phase']), 'environment changed')
    equal(frozen['pins'], config['pins'], 'binary pins changed'); equal(frozen['root'], config['roots'][frozen['phase']], 'root changed')
    pure = json.loads(Path(frozen['pure']['path']).read_text())
    require(pure['status'] == 'PASS' and pure['driver_sha256'] == digest(__file__), 'pure controls stale')
    require(not (Path(frozen['root'])/'STARTED.json').exists(), 'lot already attempted')
    if frozen['phase'] == 'candidate':
        gate_pin = frozen['oracle_gate']; require(pin(gate_pin['path']) == gate_pin, 'oracle gate changed')
        gate = json.loads(Path(gate_pin['path']).read_text())
        require(gate['status'] == 'PASS' and pin(gate['result']['path']) == gate['result'], 'oracle result changed')
        equal(gate['driver_sha256'], digest(__file__), 'oracle driver'); equal(gate['indices'], EXPECTED, 'oracle indices')
        equal(set(gate['argv']), set(KINDS), 'oracle births')
        for label in KINDS: equal(gate['argv'][label], witness_argv(config, 'oracle', label), 'raw oracle argv')
        equal(gate['deliveries'], [['P3', 'numeric'], ['P4', 'numeric'], ['P4', 'stable-pane-ID']], 'oracle delivery matrix')

def execute(frozen_path):
    frozen = json.loads(frozen_path.read_text()); verify_frozen(frozen); bind_api(frozen['config']); run = Run(frozen)
    write_once(run.root/'STARTED.json', {'at': now(), 'frozen': pin(frozen_path), 'observer': D.birth(os.getpid())})
    try: run.execute(); run.record['status'] = 'PASS'
    except BaseException as error:
        run.record.update(status='FAIL' if isinstance(error, Failed) else 'UNPROVEN', error=repr(error), traceback=traceback.format_exc())
    finally:
        try: run.cleanup()
        except BaseException as error: run.record.update(cleanup_exception=repr(error), status='UNPROVEN')
        run.record.update(end=now(), seconds=time.monotonic()-run.start, pane_spawn_commands=run.spawn_count,
                          frozen=pin(frozen_path), distinct_nonces=len(run.all_nonces),
                          limits=['Disk SHA is not mapped-image attestation.', 'Own-helper sampling is not exhaustive transient coverage.',
                                  'Indirect NOTE_EXIT/ECHILD is not author reap or numeric exit status.',
                                  'Canonical slave mode observed; no host termios mutation/restoration credit.'])
        require(len(encoded(run.record))+sum(p.stat().st_size for p in run.root.rglob('*') if p.is_file()) < LOT_CAP, 'lot log cap')
        write_once(run.root/'RESULT.json', run.record)
    if run.record['status'] == 'PASS' and run.phase == 'oracle':
        measured = {e['case']: {label: int(next(r['pane_index'] for r in e['samples'][0]['rows'] if r['pane_id'] == run.children[label]['public']['pane_id'])) for label in e['expected']} for e in run.record['inventories']}
        write_once(run.root/'ORACLE-FROZEN.json', {'at': now(), 'status': 'PASS', 'result': pin(run.root/'RESULT.json'),
                   'driver_sha256': digest(__file__), 'indices': measured,
                   'argv': {label: child['child']['args']['argv'] for label, child in run.children.items()},
                   'deliveries': [[e['case'], e['address_kind']] for e in run.record['deliveries']]})
    print(json.dumps({'status': run.record['status'], 'result': pin(run.root/'RESULT.json'), 'seconds': run.record['seconds']}), flush=True)
    return 0 if run.record['status'] == 'PASS' else 1

if __name__ == '__main__':
    if len(sys.argv) == 6 and sys.argv[1] == 'witness': witness(*sys.argv[2:])
    elif len(sys.argv) == 3 and sys.argv[1] == '--frozen': raise SystemExit(execute(Path(sys.argv[2])))
    else: raise SystemExit('Use --frozen FILE.json, or private witness mode.')
