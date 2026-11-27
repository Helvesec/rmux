#!/usr/bin/env python3
"""Windows popup adapter using an ordinary owned OpenSSH terminal.

The Mac controller reads its own ssh PTY; Windows runs the public RMUX CLI.
There is no standalone ConPTY capture, injected hook, or product instrumentation.
Producer and timeout recognition are compiled unchanged from the pinned canonical
fixture AST because that fixture's top-level Unix imports cannot load on Windows.
"""
import ast
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import traceback
from types import SimpleNamespace

sys.dont_write_bytecode = True
CANONICAL_SHA = '139a95efff395a1755879f9a6876b35da4db03b457c754a122f5fc19c21c46af'
COLS, ROWS = 225, 54

def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def require(ok, message):
    if not ok:
        raise AssertionError(message)

def equal(actual, expected, message):
    require(actual == expected, f'{message}: {actual!r} != {expected!r}')

def write(path, value):
    path = Path(path)
    temp = path.with_suffix(path.suffix + '.tmp')
    temp.write_text(json.dumps(value, indent=2), encoding='utf-8')
    temp.replace(path)

def write_once(path, value):
    with Path(path).open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2)

def pin(path):
    path = Path(path)
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    return dict(path=str(path), bytes=path.stat().st_size, sha256=digest)

def canonical():
    path = Path(__file__).with_name('popup_reference.py')
    equal(pin(path)['sha256'], CANONICAL_SHA, 'canonical fixture changed')
    names = {'final_rows', 'final_bytes', 'burst_bytes', 'replacement_surface', 'producer', 'transport_timeout_events'}
    nodes = [n for n in ast.parse(path.read_text()).body if isinstance(n, ast.FunctionDef) and n.name in names]
    equal({n.name for n in nodes}, names, 'canonical exports')
    import re
    support = SimpleNamespace(require=require, equal=equal, now=now, write_once=write_once,
                              proc=producer_process, identity=lambda pid: identity(pid))
    space = dict(Path=Path, os=os, time=time, re=re, B=support, COLS=COLS, ROWS=ROWS)
    exec(compile(ast.Module(body=nodes, type_ignores=[]), str(path), 'exec'), space)
    return SimpleNamespace(**{name: space[name] for name in names})

def producer_process(pid):
    equal(pid, os.getpid(), 'producer may observe only itself')
    before = identity(pid)
    image = pin(sys.executable)
    argv = list(sys.orig_argv)
    after = identity(pid)
    equal(after, before, 'producer identity changed')
    return dict(before=before, after=after, executable=image, actual_argv=argv,
                argv_source='own interpreter sys.orig_argv; not inferred launch intent')

def identity(pid):
    # Same ordinary owned-process birth/image observation as the existing OS support.
    import importlib.util
    path = Path(__file__).parent / 'refresh_colour_reference/windows_support.py'
    spec = importlib.util.spec_from_file_location('existing_windows_identity', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    value = module.windows_identity(pid)
    require(value is not None, f'owned process absent: {pid}')
    value['birth_filetime'] = int(value['ps_lstart_argv'])
    return value

def windows_token():
    import re
    whoami = r'C:\Windows\System32\whoami.exe'
    user = subprocess.check_output([whoami, '/user', '/fo', 'csv', '/nh'])
    groups = subprocess.check_output([whoami, '/groups', '/fo', 'csv', '/nh'])
    # SIDs are ASCII identifiers independent of localized display-name encoding.
    sid = re.search(rb'S-1-5-21-[0-9-]+', user)[0].decode('ascii')
    integrity = int(re.search(rb'S-1-16-(\d+)', groups)[1])
    return sid, integrity


def total(root):
    return sum(p.stat().st_size for p in Path(root).rglob('*') if p.is_file())

def resources(config):
    root, source = Path(config['windows_root']), Path(config['windows_source'])
    projects = root.parent.parent
    owned = [p for p in projects.iterdir() if p.is_dir() and p.name.startswith('rmux-example-L4')]
    owned += [p for p in root.parent.iterdir() if p.is_dir() and (p.name == 'L4' or p.name.startswith('L4-'))]
    new = total(root) + total(source)
    old = 0  # The producing target is already included in this job's source tree.
    result = dict(at=now(), new_pathsum=new, reused_plus_new_pathsum=old + new,
                  owner_pathsum=sum(total(p) for p in owned), free=shutil.disk_usage(root).free,
                  logs=total(root / config['lot']) if (root / config['lot']).exists() else 0)
    require(new < 6442450944, '037 new pathsum bound')
    require(result['owner_pathsum'] < 32000000000 and result['free'] > 50000000000, 'owner/free bound')
    require(result['logs'] < 100663296, 'log bound')
    return result


def stalled_context(ready, request_id):
    """Bind a stall to the independently retained READY and request receipt."""
    return dict(request_id=request_id, endpoint=ready['endpoint'],
                runner_identity=ready['runner_identity'],
                daemon_identity=ready['roots'][0]['identity'],
                attach_identity=ready['roots'][1]['identity'],
                cli_prefix=ready['cli_prefix'])

def stalled_status(observed, begin, expected, cleanup_started=None):
    """Fail closed on facts from one original Windows monotonic interval.

    The separately written begin receipt and READY identities are the anchors;
    neither a returned status nor a caller-supplied elapsed duration is proof.
    Wall timestamps remain raw diagnostics and are never compared across hosts.
    """
    import math
    def number(value):
        require(type(value) in (int, float) and math.isfinite(value) and value >= 0, 'invalid monotonic time')
        return value
    def owned(value):
        require(type(value['pid']) is int and value['pid'] > 0, 'missing PID')
        require(type(value['birth_filetime']) is int and value['birth_filetime'] > 0, 'missing birth')
    def query_facts(query):
        equal(query['clock_identity'], expected['runner_identity'], 'query clock owner')
        owned(query['identity'])
        equal(query['argv'], expected['cli_prefix'] + ['list-clients', '-F', '#{client_pid}'], 'public owned PID query')
        require(type(query['exit']) is int and query['exit'] == 0 and query['timeout'] is False, 'query failed')
        require(not query.get('error') and query['stderr'] == '', 'query error')
        require(isinstance(query['start'], str) and bool(query['start']) and isinstance(query['end'], str) and bool(query['end']), 'raw wall interval missing')
        start, end = number(query['monotonic_start']), number(query['monotonic_end'])
        require(start <= end, 'reversed query interval')
        return start, end
    try:
        require(not observed.get('error'), 'observation error')
        require(type(expected['request_id']) is int and expected['request_id'] > 0, 'request identity')
        for key in ('runner_identity', 'daemon_identity', 'attach_identity'):
            owned(expected[key])
        equal(begin['context'], expected, 'begin ownership/request mismatch')
        equal(observed['begin'], begin, 'original begin receipt mismatch')
        equal(begin['clock'], 'Windows time.monotonic', 'native clock required')
        start = number(begin['monotonic_begin'])
        deadline = number(begin['monotonic_deadline'])
        equal(deadline, start + 7, 'original seven-second deadline')
        equal(begin['initial_exit_log'], '', 'contaminated initial timeout log')
        _, healthy_end = query_facts(begin['healthy_query'])
        require(healthy_end <= start, 'healthy query after begin')
        equal(begin['healthy_query']['stdout'].strip(), str(expected['attach_identity']['pid']), 'owned healthy client')
        proof = observed['timeout_observation']
        previous = number(proof['previous_monotonic'])
        seen = number(proof['observed_monotonic'])
        require(start <= previous < seen <= deadline and seen >= start + 5, 'timeout observation outside policy')
        events = canonical().transport_timeout_events(proof['raw_log'], expected['daemon_identity']['pid'], expected['attach_identity']['pid'], seen - start)
        require(len(events) == 1 and events == proof['events'], 'owned timeout antecedent absent')
        query_start, query_end = query_facts(observed['query'])
        require(seen <= query_start <= query_end <= deadline, 'absence query completed outside original deadline')
        equal(observed['query']['stdout'], '', 'client still listed')
        finished = number(observed['completed_monotonic'])
        require(finished >= deadline, 'no-read observation ended early')
        equal(observed['pause_seconds'], finished - start, 'incorrect measured no-read interval')
        require(observed['before_cleanup'] is True, 'cleanup-contaminated evidence')
        for cleanup in (observed['cleanup_started_monotonic'], cleanup_started):
            if cleanup is not None:
                require(number(cleanup) >= finished, 'cleanup preceded complete observation')
        return 'PASS_BOUNDED_STALLED'
    except (AssertionError, KeyError, TypeError, ValueError, OverflowError):
        return 'FAIL_UNPROVEN_STALLED_EVIDENCE'

def popup_lot_status(cases, ready, cleanup_started=None, final=False):
    """Recheck stalled evidence at aggregation, including actual final cleanup."""
    try:
        require(bool(cases), 'no observed cases')
        for case in cases:
            require(not case.get('error'), 'case error')
            if case['id'] == 'stalled500':
                expected = stalled_context(ready, case['stall_request_id'])
                require(case['read_bytes_during_pause'] == 0, 'reader consumed stalled output')
                require(not final or cleanup_started is not None, 'final cleanup receipt missing')
                equal(stalled_status(case, case['stall_begin'], expected, cleanup_started), 'PASS_BOUNDED_STALLED', 'stalled evidence not qualified')
            else:
                require(case['status'].startswith('PASS_'), 'failed independent case')
        return 'PASS_BOUNDED_CELLS'
    except (AssertionError, KeyError, TypeError, ValueError, OverflowError):
        return 'FAIL_WITH_BOUNDED_OBSERVATIONS'

def windows_run(config_path):
    import ctypes as C
    from ctypes import wintypes as W
    config = json.loads(Path(config_path).read_text())
    root = Path(config['windows_root']) / config['lot']
    root.mkdir()
    (root / 'requests').mkdir()
    (root / 'responses').mkdir()
    record = dict(start=now(), commands=[], roots=[], producer_handles=[], resources=[], cases={}, failure=None)
    start = time.monotonic()
    deadline = start + 540
    children, logs, held = [], [], {}
    daemon = attach = None
    k = C.WinDLL('kernel32', use_last_error=True)
    k.OpenProcess.argtypes = [W.DWORD, W.BOOL, W.DWORD]
    k.OpenProcess.restype = W.HANDLE
    k.CloseHandle.argtypes = [W.HANDLE]
    k.GetProcessTimes.argtypes = [W.HANDLE] + [C.POINTER(W.FILETIME)] * 4
    k.GetExitCodeProcess.argtypes = [W.HANDLE, C.POINTER(W.DWORD)]
    k.WaitForSingleObject.argtypes = [W.HANDLE, W.DWORD]
    def popen_identity(p, argv):
        times = [W.FILETIME() for _ in range(4)]
        require(k.GetProcessTimes(int(p._handle), *[C.byref(t) for t in times]), 'owned Popen birth')
        birth = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        return dict(pid=p.pid, birth_filetime=birth, ps_lstart_argv=str(birth), path=argv[0], image_source='absolute Popen argv; binary hash qualified')
    def hold(expected):
        pid = expected['pid']
        if pid in held:
            return
        h = k.OpenProcess(0x1000 | 0x100000, False, pid)
        require(bool(h), 'owned child handle unavailable')
        times = [W.FILETIME() for _ in range(4)]
        require(k.GetProcessTimes(h, *[C.byref(t) for t in times]), 'owned child birth query')
        birth = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        equal(birth, expected['birth_filetime'], 'owned child PID reused')
        held[pid] = (h, expected)
    def observations():
        result = []
        for h, expected in held.values():
            code = W.DWORD()
            require(k.GetExitCodeProcess(h, C.byref(code)), 'owned exit status')
            result.append(dict(identity=expected, wait=k.WaitForSingleObject(h, 0), exit_code=code.value))
        return result
    def spawn(argv, terminal=False):
        if terminal:
            p = subprocess.Popen(argv, cwd=config['windows_source'], env=env)
        else:
            out = (root / f'process-{len(children)}.log').open('wb')
            logs.append(out)
            p = subprocess.Popen(argv, cwd=config['windows_source'], env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=out)
        own = popen_identity(p, argv)
        record['roots'].append(dict(argv=argv, identity=own))
        children.append(p)
        return p
    def cli(args, check=True, seconds=15):
        require(len(record['commands']) + len(record.get('roots', [])) < 512, 'public CLI count bound')
        argv = prefix + args
        began = now()
        monotonic_start = time.monotonic()
        p = subprocess.Popen(argv, cwd=config['windows_source'], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        own = popen_identity(p, argv)
        timed_out = False
        try:
            out, err = p.communicate(timeout=seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            p.kill()
            out, err = p.communicate(timeout=5)
        monotonic_end = time.monotonic()
        row = dict(start=began, end=now(), monotonic_start=monotonic_start, monotonic_end=monotonic_end,
                   clock_identity=record['runner_identity'], argv=argv, identity=own, exit=p.returncode, timeout=timed_out,
                   stdout=out.decode('utf-8', errors='replace'), stderr=err.decode('utf-8', errors='replace'))
        record['commands'].append(row)
        write(root / 'COMMANDS.json', record['commands'])
        p._handle.Close()
        require(not timed_out and (not check or row['exit'] == 0), f'public command failed: {row}')
        return row
    def helper_sample(label):
        # Existing ordinary own-process metadata, limited to qualified live roots
        # and their descendants. The transient collector is itself counted.
        roots = []
        for child in children:
            if child.poll() is None:
                roots.append(identity(child.pid))
        for item in observations():
            if item['wait'] == 258:
                current = identity(item['identity']['pid'])
                equal(current['birth_filetime'], item['identity']['birth_filetime'], 'live helper birth')
                roots.append(current)
        expected = {x['pid']: x for x in roots}
        argv = [config['powershell'], '-NoProfile', '-NonInteractive', '-File', config['helper_collector'],
                ','.join(str(p) for p in sorted(expected))]
        p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        own = popen_identity(p, argv)
        try:
            out, err = p.communicate(timeout=8)
        except subprocess.TimeoutExpired:
            p.kill(); out, err = p.communicate(timeout=3)
            raise AssertionError('owned metadata deadline')
        finally:
            p._handle.Close()
        equal(p.returncode, 0, 'owned metadata exit')
        rows = json.loads(out.decode('utf-8-sig'))
        require(isinstance(rows, list), 'owned metadata array')
        daemon_pid = daemon.pid if daemon else None
        helpers = [x for x in rows if x['pid'] != daemon_pid]
        sample = dict(at=now(), label=label, roots=roots, rows=rows, collector=own,
                      collector_exit=p.returncode, stderr=err.decode(errors='replace'),
                      simultaneous_observed=len(helpers)+1,
                      continuous_peak='UNKNOWN; finite scoped metadata samples miss transient overlap')
        record.setdefault('helper_samples', []).append(sample)
        write(root / 'HELPERS.json', record['helper_samples'])
        require(sample['simultaneous_observed'] <= 8, 'eight helpers including collector exceeded')
        return sample
    def start_popup(request):
        label = request['label']
        require(label in config['case_labels'], 'unfrozen case label')
        equal(request['bytes'], config['producer_bytes'][label], 'unfrozen producer bytes')
        require(request['bytes'] <= 524288, 'producer cap')
        folder = root / label
        folder.mkdir()
        argv = [config['python'], str(Path(__file__)), 'producer', str(folder), label, str(request['bytes'])]
        command = subprocess.list2cmdline(argv)
        spawn(prefix + ['display-popup', '-c', request['tty'], '-B', '-w', '100%', '-h', '100%', command])
        return dict(at=now(), producer_argv=argv, popup_cli=record['roots'][-1])
    def state():
        cases = {}
        for label in config['case_labels']:
            folder = root / label
            data = {}
            for name in ['born', 'stage0', 'stage1', 'stage2', 'stage3', 'emitted', 'exit-intent']:
                path = folder / (name + '.json')
                if path.exists():
                    try:
                        data[name] = json.loads(path.read_text())
                    except json.JSONDecodeError:
                        continue  # A producer is still finishing this own write-once receipt.
            if 'born' in data:
                hold(data['born']['process']['before'])
            cases[label] = data
        return dict(cases=cases, processes=observations(), attach_exit=attach.poll() if attach else None,
                    exit_log=exit_log.read_text() if exit_log.exists() else '',
                    popup_exits=[p.poll() for p in children[2:]])
    try:
        record['resources'].append(resources(config))
        record['runner_identity'] = identity(os.getpid())
        for binary in config['binaries']:
            equal(pin(binary['path']), binary, 'qualified producing binary changed')
        equal(pin(Path(__file__).with_name('popup_reference.py'))['sha256'], CANONICAL_SHA, 'fixture')
        sid, integrity = windows_token()
        equal(sid, config['sid'], 'current SID')
        equal(integrity, 12288, 'current high integrity')
        record['token'] = dict(sid=sid, integrity_rid=integrity)
        record['terminal_geometry'] = list(os.get_terminal_size(1))
        equal(record['terminal_geometry'], [225, 54], 'ordinary SSH Windows terminal geometry')
        env = dict(os.environ, PYTHONDONTWRITEBYTECODE='1', TERM='xterm-256color',
                   RMUX_CLIENT_SHELL=r'C:\Windows\System32\cmd.exe', COMSPEC=r'C:\Windows\System32\cmd.exe')
        for key in ['RMUX', 'TMUX', 'RMUX_PERF_TRACE', 'RMUX_EXAMPLE_INITIAL_TRACE_CONFIG']:
            env.pop(key, None)
        exit_log = root / 'attach-exit.log'
        env['RMUX_ATTACH_EXIT_LOG'] = str(exit_log)
        endpoint = config['endpoint']
        conf = root / 'rmux.conf'
        conf.write_text('set-option -s exit-empty off\nset-option -g default-shell C:/Windows/System32/cmd.exe\nset-option -g status off\n')
        prefix = [config['binary'], '-N', '-S', endpoint, '-f', str(conf)]
        daemon = spawn([config['daemon'], '--__internal-daemon', endpoint, '--config-file', str(conf)])
        ready = time.monotonic() + 15
        while True:
            row = cli(['list-sessions'], check=False)
            if row['exit'] == 0 or 'no sessions' in row['stderr'].lower():
                break
            require(time.monotonic() < ready and daemon.poll() is None, 'private daemon readiness')
            time.sleep(.1)
        cli(['new-session', '-d', '-s', 'alpha', '-x', '225', '-y', '54', config['python'], str(Path(__file__)), 'idle'])
        pane = cli(['display-message', '-p', '-t', 'alpha:0.0', '#{pane_pid}'])
        hold(identity(int(pane['stdout'].strip())))
        attach = spawn(prefix + ['attach-session', '-t', 'alpha'], terminal=True)
        ready = dict(at=now(), roots=record['roots'][:], runner_identity=record['runner_identity'],
                     terminal_geometry=record['terminal_geometry'], endpoint=endpoint, cli_prefix=prefix)
        write(root / 'READY.json', ready)
        processed = set()
        sampled = time.monotonic()
        while True:
            require(time.monotonic() < deadline, '540s native observation deadline')
            if time.monotonic() - sampled >= 5:
                record['resources'].append(resources(config))
                write(root / 'RESOURCE-SAMPLES.json', record['resources'])
                sampled = time.monotonic()
            requests = sorted((root / 'requests').glob('*.json'))
            stop = False
            for request_path in requests:
                if request_path.name in processed:
                    continue
                request = json.loads(request_path.read_text())
                response = dict(id=request['id'], at=now())
                try:
                    op = request['op']
                    if op == 'cli':
                        response['command'] = cli(request['args'])
                    elif op == 'start':
                        response['launch'] = start_popup(request)
                    elif op == 'replace':
                        # Adjacent public operations in one native request, without transport delay.
                        response['close'] = cli(['display-popup', '-C', '-c', request['tty']])
                        response['launch'] = start_popup(request)
                    elif op == 'helpers':
                        response['helpers'] = helper_sample(request['label'])
                    elif op == 'mark':
                        require(request['label'] in config['case_labels'], 'unfrozen marker case')
                        require(request['name'] in ['go', 'exit', 'continue0', 'continue1', 'continue2', 'continue3'], 'marker')
                        (root / request['label'] / request['name']).write_text(now())
                    elif op == 'state':
                        response['state'] = state()
                    elif op == 'stall':
                        label = request['label']
                        require(label == 'stalled500' and label in config['case_labels'], 'stalled label')
                        folder = root / label
                        initial_log = exit_log.read_text() if exit_log.exists() else ''
                        equal(initial_log, '', 'prior healthy exit event')
                        healthy = cli(['list-clients', '-F', '#{client_pid}'])
                        equal(healthy['stdout'].strip(), str(attach.pid), 'same owned healthy attach')
                        begin = time.monotonic()
                        deadline = begin + 7
                        expected = stalled_context(ready, request['id'])
                        anchor = dict(context=expected, clock='Windows time.monotonic',
                                      monotonic_begin=begin, monotonic_deadline=deadline,
                                      initial_exit_log=initial_log, healthy_query=healthy)
                        write_once(root / 'STALL-BEGIN.json', anchor)
                        for stage in range(4):
                            (folder / f'continue{stage}').write_text(now())
                        (folder / 'go').write_text(now())
                        previous = begin
                        observation = query = None
                        while time.monotonic() < deadline:
                            time.sleep(min(.1, max(0, deadline - time.monotonic())))
                            seen = time.monotonic()
                            raw_log = exit_log.read_text() if exit_log.exists() else ''
                            require(len(raw_log.encode()) < 1048576, 'owned timeout log cap')
                            events = canonical().transport_timeout_events(raw_log, daemon.pid, attach.pid, seen - begin)
                            if events:
                                equal(len(events), 1, 'unique owned timeout antecedent')
                                observation = dict(previous_monotonic=previous, observed_monotonic=seen,
                                                   events=events, raw_log=raw_log)
                                break
                            previous = seen
                        # Query promptly after the antecedent, using only the original remaining window.
                        remaining = deadline - time.monotonic()
                        if observation is not None and remaining > 0:
                            query = cli(['list-clients', '-F', '#{client_pid}'], seconds=remaining)
                        while time.monotonic() < deadline:
                            time.sleep(min(.05, max(0, deadline - time.monotonic())))
                        completed = time.monotonic()
                        observed = dict(begin=anchor, pause_seconds=completed - begin,
                            completed_monotonic=completed, timeout_observation=observation, query=query,
                            before_cleanup=record.get('cleanup_started_monotonic') is None,
                            cleanup_started_monotonic=record.get('cleanup_started_monotonic'))
                        observed['status'] = stalled_status(observed, anchor, expected)
                        response['stalled'] = observed
                    elif op == 'stop':
                        record['controller_verdict'] = request['verdict']
                        stop = True
                    else:
                        raise AssertionError('unknown adapter operation')
                    response['ok'] = True
                except Exception:
                    response.update(ok=False, error=traceback.format_exc())
                write(root / 'responses' / request_path.name, response)
                processed.add(request_path.name)
            if stop:
                break
            time.sleep(.02)
    except Exception:
        record['failure'] = traceback.format_exc()
        write(root / 'FAILURE-BEFORE-CLEANUP.json', dict(at=now(), error=record['failure']))
    finally:
        record['cleanup_started_monotonic'] = time.monotonic()
        record['cleanup_started'] = now()
        if daemon is not None and daemon.poll() is None:
            try:
                cli(['kill-server'], check=False, seconds=10)
            except Exception:
                record['cleanup_command_error'] = traceback.format_exc()
        for p in reversed(children):
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                record.setdefault('forced_owned_cleanup', []).append(p.pid)
                p.kill()
                p.wait(timeout=5)
            record.setdefault('wait_statuses', []).append(dict(pid=p.pid, exit=p.returncode))
            p._handle.Close()
        for stream in logs:
            stream.close()
        record['producer_handles'] = observations()
        for h, _ in held.values():
            k.CloseHandle(h)
        record['end'] = now()
        record['seconds'] = time.monotonic() - start
        record['cleanup_pass'] = all(x['wait'] == 0 for x in record['producer_handles'])
        record['resources'].append(resources(config))
        record['binary_after'] = [pin(x['path']) for x in config['binaries']]
        write(root / 'WINDOWS-RESULT.json', record)
    return 0 if not record['failure'] and record['cleanup_pass'] else 1

def controller(config_path):
    import fcntl
    import pty
    import select
    import signal
    import struct
    import termios
    config = json.loads(Path(config_path).read_text())
    sys.path.insert(0, config['parser_path'])
    import pyte
    common = canonical()
    root = Path(config['mac_root']) / config['lot']
    root.mkdir()
    remote = '/' + config['windows_root'].replace('\\', '/') + '/' + config['lot']
    record = dict(start=now(), cases=[], surfaces={}, transfers=[], failure=None)
    raw = bytearray()
    seq = 0
    class Screen(pyte.Screen):
        def report_device_status(self, mode, **kwargs):
            if not kwargs.get('private'):
                super().report_device_status(mode)
    screen = Screen(225, 54)
    stream = pyte.ByteStream(screen)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 54, 225, 0, 0))
    ssh = subprocess.Popen(config['ssh_argv'], stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    record['terminal_transport'] = dict(argv=config['ssh_argv'], pid=ssh.pid, local_geometry=[225, 54])
    began = time.monotonic()
    def pump(seconds=.02, read=True):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            require(time.monotonic() - began < 540, 'controller540s deadline')
            if read and select.select([master], [], [], .01)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                raw.extend(data)
                require(len(raw) <= 6 * 1024 * 1024, 'raw capture cap')
                stream.feed(data)
            else:
                time.sleep(.01)
    def transfer(lines, required=True):
        p = subprocess.run(['/usr/bin/sftp', '-q', '-b', '-', 'windows-reference-host'], input='\n'.join(lines) + '\n',
                           capture_output=True, text=True, timeout=10)
        record['transfers'].append(dict(at=now(), commands=lines, exit=p.returncode, stderr=p.stderr))
        require(not required or p.returncode == 0, 'own SFTP transport failed: ' + p.stderr)
        return p.returncode == 0
    def get(name, required=True):
        path = root / name.replace('/', '-')
        ok = transfer(['get ' + remote + '/' + name + ' ' + str(path)], required)
        return json.loads(path.read_text()) if ok else None
    def rpc(op, read=True, **kwargs):
        nonlocal seq
        seq += 1
        name = f'{seq:04d}.json'
        path = root / ('request-' + name)
        write_once(path, dict(id=seq, op=op, **kwargs))
        transfer(['put ' + str(path) + ' ' + remote + '/requests/' + name + '.pending',
                  'rename ' + remote + '/requests/' + name + '.pending ' + remote + '/requests/' + name])
        timeout = time.monotonic() + 20
        while True:
            response = get('responses/' + name, required=False)
            if response is not None:
                require(response['ok'], response.get('error', 'RPC failed'))
                return response
            require(time.monotonic() < timeout, 'adapter response deadline')
            pump(.05, read=read)
    def command(args, read=True):
        row = rpc('cli', read=read, args=args)['command']
        require(row['exit'] == 0 and not row['timeout'], 'public query failed')
        return row['stdout'].strip()
    def state(read=True):
        return rpc('state', read=read)['state']
    def until(predicate, message, seconds=12):
        end = time.monotonic() + seconds
        while not predicate():
            require(time.monotonic() < end, message)
            pump(.05)
    def mark(label, name, read=True):
        rpc('mark', read=read, label=label, name=name)
    def snapshot(key, expected=None):
        value = dict(rows=screen.display, cursor=[screen.cursor.x, screen.cursor.y], hidden=screen.cursor.hidden,
                     geometry=[screen.columns, screen.lines])
        record['surfaces'][key] = value
        (root / (key + '.pty.bin')).write_bytes(raw)
        write(root / (key + '.surface.json'), value)
        if expected is not None:
            equal(value, expected, 'exact independent tmux full surface/cursor ' + key)
        return value
    def replacement(oracle):
        case = dict(id='replacement', start=now(), status='RUNNING')
        record['cases'].append(case)
        def observed(label):
            return state()['cases'][label]
        def birth(label, launch):
            until(lambda: 'born' in observed(label), 'replacement producer birth absent')
            proc = observed(label)['born']['process']
            equal(proc['actual_argv'], launch['producer_argv'], 'actual producer argv')
            equal(proc['before'], proc['after'], 'producer birth stable')
            equal(proc['executable'], config['python_pin'], 'actual producer image')
            return proc
        def process(st, proc):
            return next(x for x in st['processes'] if x['identity']['pid'] == proc['before']['pid'])
        def helpers(label):
            case.setdefault('helpers', []).append(rpc('helpers', label=label)['helpers'])
        old = 'replace-old'
        launch = rpc('start', label=old, bytes=512000, tty=tty)['launch']
        case['old'] = birth(old, launch)
        helpers('old-born')
        mark(old, 'go')
        for stage in range(3):
            until(lambda: f'stage{stage}' in observed(old), 'old producer stage absent')
            if stage < 2:
                mark(old, f'continue{stage}')
        boundary = state()
        case['old_boundary'] = boundary
        sent = boundary['cases'][old]['stage2']['bytes']
        require(0 < sent < 512000 - len(common.final_bytes(old)), 'old burst must remain incomplete')
        require('emitted' not in boundary['cases'][old], 'old final already emitted')
        equal(process(boundary, case['old'])['wait'], 258, 'old producer must be live')
        until(lambda: any('BURST039|replace-old|' in row for row in screen.display), 'old labelled surface absent')
        pump(.3)
        old_surface = snapshot('replace-old-boundary')
        case['noop_launch'] = rpc('start', label='replace-noop', bytes=512000, tty=tty)['launch']
        until(lambda: len(state()['popup_exits']) >= 2 and state()['popup_exits'][1] is not None, 'NOOP CLI wait absent')
        noop = state()
        equal(noop['popup_exits'][1], 0, 'existing-popup NOOP CLI status')
        equal(noop['cases']['replace-noop'], {}, 'NOOP created a producer')
        equal(process(noop, case['old'])['wait'], 258, 'NOOP replaced old producer')
        pump(.3)
        snapshot('replace-old-noop', old_surface)
        case['noop'] = noop
        helpers('before-close')
        case['switch'] = rpc('replace', label='replace-new', bytes=512000, tty=tty)
        case['new'] = birth('replace-new', case['switch']['launch'])
        require((case['old']['before']['pid'], case['old']['before']['birth_filetime']) !=
                (case['new']['before']['pid'], case['new']['before']['birth_filetime']), 'new producer identity reused')
        until(lambda: process(state(), case['old'])['wait'] == 0 and state()['popup_exits'][0] is not None,
              'old producer/CLI termination before new GO')
        case['after_switch_before_new_go'] = state()
        require('emitted' not in case['after_switch_before_new_go']['cases'][old], 'old final emitted after close')
        helpers('new-born-old-ended')
        mark('replace-new', 'go')
        for stage in range(4):
            until(lambda: f'stage{stage}' in observed('replace-new'), 'new producer stage absent')
            if stage in (1, 2):
                cols, rows = (200, 50) if stage == 1 else (225, 54)
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
                os.kill(ssh.pid, signal.SIGWINCH)
                screen.resize(lines=rows, columns=cols)
                until(lambda: command(['list-clients', '-F', '#{client_width}|#{client_height}']) == f'{cols}|{rows}', 'replacement terminal resize absent')
                pump(.2)
                snapshot(f'replace-new-resize{stage}')
                case.setdefault('native_resizes', []).append([cols, rows])
            mark('replace-new', f'continue{stage}')
        until(lambda: 'emitted' in observed('replace-new'), 'new emission absent')
        emission = observed('replace-new')['emitted']
        equal(emission['bytes'], 512000, 'new producer byte bound')
        equal(emission['final_size'], [225, 54], 'new native geometry')
        equal(emission['final_hex'], common.final_bytes('replace-new').hex(), 'canonical new output')
        until(lambda: common.replacement_surface(screen), 'new exact full surface absent')
        snapshot('replace-new-live', oracle['replace-new-live'])
        case['emission'] = emission
        mark('replace-new', 'exit')
        until(lambda: process(state(), case['new'])['wait'] == 0, 'new actual held-handle wait absent')
        case['after_eof'] = state()
        equal(process(case['after_eof'], case['new'])['exit_code'], 0, 'new actual exit status')
        require('exit-intent' in case['after_eof']['cases']['replace-new'], 'new exit intent absent')
        pump(.3)
        snapshot('replace-new-eof', oracle['replace-new-eof'])
        equal(command(['list-clients', '-F', '#{client_pid}']), tty, 'EOF client lost')
        pump(.2)
        baseline = len(raw)
        quiescence = []
        for _ in range(5):
            begin = time.monotonic()
            pump(.1)
            equal(len(raw), baseline, 'delayed post-EOF terminal bytes')
            require(common.replacement_surface(screen), 'delayed full surface contamination')
            quiescence.append(dict(seconds=time.monotonic()-begin, raw_bytes=len(raw)))
        snapshot('replace-new-quiescent', oracle['replace-new-quiescent'])
        case['quiescence'] = quiescence
        final = state()
        equal(process(final, case['old'])['wait'], 0, 'old held handle no longer signaled')
        equal(final['cases']['replace-noop'], {}, 'delayed NOOP producer')
        require('emitted' not in final['cases'][old], 'delayed old final')
        helpers('quiescent')
        case.update(status='PASS_BOUNDED_REPLACEMENT', before_cleanup=True, end=now(), final=final,
                    internal_generation='UNKNOWN', continuous_helper_peak='UNKNOWN')
        write_once(root / 'REPLACEMENT-BEFORE-CLEANUP.json', case)
        command(['display-popup', '-C', '-c', tty])
    tty = None
    try:
        timeout = time.monotonic() + 30
        while True:
            ready = get('READY.json', required=False)
            if ready:
                record['native_ready'] = ready
                break
            require(ssh.poll() is None and time.monotonic() < timeout, 'native terminal startup failed')
            pump(.1)
        timeout = time.monotonic() + 15
        while not tty:
            line = command(['list-clients', '-F', '#{client_pid}|#{client_tty}|#{client_width}|#{client_height}'])
            if line:
                fields = line.split('|')
                equal(len(fields), 4, 'one owned attached client')
                equal(fields[2:], ['225', '54'], 'native client geometry')
                equal(int(fields[0]), ready['roots'][1]['identity']['pid'], 'actual owned attach PID')
                record['native_client_tty'] = fields[1]
                tty = fields[0]  # Windows public PID target; client_tty may be empty.
            require(time.monotonic() < timeout, 'client not registered')
            pump(.1)
        record['tty'] = tty
        oracle = json.loads(Path(config['oracle']).read_text())['surfaces']
        if config.get('focus_replacement'):
            replacement(oracle)
        for spec in config['cases']:
            label, count = spec['label'], spec['bytes']
            case = dict(id=label, bytes=count, start=now(), status='RUNNING')
            record['cases'].append(case)
            try:
                rpc('start', label=label, bytes=count, tty=tty)
                until(lambda: 'born' in state()['cases'][label], 'producer birth absent')
                case['producer'] = state()['cases'][label]['born']['process']['before']
                if spec.get('stalled'):
                    pump(.3)
                    before = len(raw)
                    case['stall_request_id'] = seq + 1
                    expected = stalled_context(ready, case['stall_request_id'])
                    no_read_start = time.monotonic()
                    observed = rpc('stall', read=False, label=label)['stalled']
                    anchor = get('STALL-BEGIN.json')
                    equal(len(raw), before, 'non-reader consumed terminal bytes')
                    no_read_end = time.monotonic()
                    case.update(observed, id=label, stall_request_id=expected['request_id'],
                                stall_begin=anchor, read_bytes_during_pause=0, end=now(),
                                controller_no_read_interval=dict(clock='controller time.monotonic',
                                    start=no_read_start, end=no_read_end, seconds=no_read_end-no_read_start))
                    case['status'] = stalled_status(observed, anchor, expected)
                    continue
                mark(label, 'go')
                if spec.get('slow'):
                    pause_begin = time.monotonic()
                    pump(1, read=False)
                    case['healthy_no_read_seconds'] = time.monotonic() - pause_begin
                for stage in range(4):
                    until(lambda: f'stage{stage}' in state()['cases'][label], 'producer stage absent')
                    if spec.get('close') and stage == 1:
                        command(['display-popup', '-C', '-c', tty])
                        until(lambda: any(x['identity']['pid'] == case['producer']['pid'] and x['wait'] == 0 for x in state()['processes']), 'closed producer remains alive')
                        case.update(status='PASS_BOUNDED_CLOSE', end=now())
                        break
                    if spec.get('resize') and stage in (1, 2):
                        cols, rows = (200, 50) if stage == 1 else (225, 54)
                        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
                        os.kill(ssh.pid, signal.SIGWINCH)
                        screen.resize(lines=rows, columns=cols)
                        until(lambda: command(['list-clients', '-F', '#{client_width}|#{client_height}']) == f'{cols}|{rows}', 'Windows terminal resize unobserved')
                        case.setdefault('native_resizes', []).append([cols, rows])
                    mark(label, f'continue{stage}')
                if spec.get('close'):
                    continue
                until(lambda: 'emitted' in state()['cases'][label], 'producer emission absent')
                emission = state()['cases'][label]['emitted']
                equal(emission['bytes'], count, 'bounded producer bytes')
                equal(emission['final_size'], [225, 54], 'native producer console geometry')
                equal(emission['final_hex'], common.final_bytes(label).hex(), 'shared canonical producer bytes')
                expected = oracle[label + '-live']
                until(lambda: screen.display == expected['rows'] and [screen.cursor.x, screen.cursor.y] == expected['cursor'], 'full terminal surface/cursor absent')
                equal(command(['list-clients', '-F', '#{client_pid}']), tty, 'healthy client retained')
                snapshot(label + '-live', expected)
                mark(label, 'exit')
                pid = state()['cases'][label]['born']['process']['before']['pid']
                until(lambda: any(x['identity']['pid'] == pid and x['wait'] == 0 for x in state()['processes']), 'EOF producer not exited')
                pump(.3)
                snapshot(label + '-eof', oracle[label + '-eof'])
                equal(command(['list-clients', '-F', '#{client_pid}']), tty, 'healthy EOF client retained')
                case.update(status='PASS_BOUNDED_SURFACE', end=now(), emission=emission, process_state=state())
                command(['display-popup', '-C', '-c', tty])
                pump(.2)
            except Exception:
                case.update(status='FAIL', error=traceback.format_exc(), end=now())
                write(root / (label + '-FAILURE-BEFORE-CLEANUP.json'), case)
                (root / (label + '-failure.pty.bin')).write_bytes(raw)
                write(root / (label + '-failure.surface.json'), dict(rows=screen.display,cursor=[screen.cursor.x,screen.cursor.y],hidden=screen.cursor.hidden,geometry=[screen.columns,screen.lines]))
                # A failed expectation is retained; only a still-owned healthy client admits an independent next case.
                equal(command(['list-clients', '-F', '#{client_pid}']), tty, 'client lost after failed case')
                command(['display-popup', '-C', '-c', tty])
                pump(.2)
        record['status'] = popup_lot_status(record['cases'], ready)
    except Exception:
        record['failure'] = traceback.format_exc()
        record['status'] = 'FAIL'
        if record['cases'] and record['cases'][-1]['status'] == 'RUNNING':
            record['cases'][-1].update(status='FAIL', error=record['failure'])
        write(root / 'FAILURE-BEFORE-CLEANUP.json', dict(at=now(), failure=record['failure']))
    finally:
        (root / 'terminal.pty.bin').write_bytes(raw)
        try:
            if ssh.poll() is None and tty:
                rpc('stop', verdict=record['status'])
            elif ssh.poll() is None:
                # Startup failure still belongs to the Windows runner's bounded cleanup.
                rpc('stop', verdict='FAIL')
        except Exception:
            record['stop_error'] = traceback.format_exc()
        end = time.monotonic() + 65
        while ssh.poll() is None and time.monotonic() < end:
            pump(.1)
        require(ssh.poll() is not None, 'Windows cleanup still pending; preserve exact owned route')
        record['ssh_exit'] = ssh.wait()
        os.close(master)
        record['windows_result'] = get('WINDOWS-RESULT.json', required=False)
        record['pool_exit'] = get('POOL-EXIT.json', required=False)
        if any(c['id'] == 'stalled500' for c in record['cases']):
            cleanup = (record['windows_result'] or {}).get('cleanup_started_monotonic')
            record['status'] = popup_lot_status(record['cases'], record.get('native_ready'), cleanup, final=True)
        if not record['pool_exit'] or record['pool_exit']['actual'] != 0:
            record['status'] = 'FAIL'
        if not record['windows_result'] or not record['windows_result']['cleanup_pass']:
            record['status'] = 'FAIL'
        if record.get('failure') or record.get('stop_error'):
            record['status'] = 'FAIL'
        record.update(end=now(), seconds=time.monotonic()-began, raw_bytes=len(raw))
        write(root / 'RESULT.json', record)
    return 0 if record['status'] == 'PASS_BOUNDED_CELLS' else 1

if __name__ == '__main__':
    mode = sys.argv[1]
    if mode == 'producer':
        canonical().producer(*sys.argv[2:])
    elif mode == 'idle':
        time.sleep(540)
    elif mode == 'token-check':
        sid, level = windows_token()
        equal(sid, 'S-1-5-21-3649436459-2219758892-2353706094-1001', 'own SID')
        equal(level, 12288, 'high integrity')
        print(json.dumps(dict(status='PASS', sid=sid, integrity_rid=level, product_runs=0, PTY_runs=0)))
    elif mode == 'windows':
        sys.exit(windows_run(sys.argv[2]))
    elif mode == 'controller':
        sys.exit(controller(sys.argv[2]))
    else:
        raise SystemExit('unknown mode')
