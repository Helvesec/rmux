#!/usr/bin/env python3
"""Pure sensitivity and early-failure retention checks; no product process."""
import base64
import copy
import json
from pathlib import Path
import stat
import struct
import sys
import tempfile
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import default_command_darwin as A


def checks(config):
    A.bind_api(config['api_pins'])
    D = A.D
    expected = {'exe': '/python', 'exe_sha256': 'pin', 'argv': ['/python', '-I', '/driver', 'witness', 'C'],
                'cwd': '/cwd', 'kind': 'C', 'token': 'fresh', 'environment': {'HOME': '/empty'}}
    def fake(pid, parent):
        own = {'pid': pid, 'ppid': parent, 'birth': [10, pid], 'status': 2}
        raw = struct.pack('=i', len(expected['argv'])) + b'/python\0\0' + b'\0'.join(v.encode() for v in expected['argv']) + b'\0'
        return {'before': own, 'after': dict(own), 'image': {'executable': '/python', 'sha256': 'pin'},
                'args': D.parse_procargs(raw), 'cwd': {'path': '/cwd'},
                'environment': expected['environment'], 'fd0': {'path': '/dev/ttys099', 'mode': stat.S_IFCHR}}
    child, daemon = fake(41, 40), fake(40, 1)
    public = {'pane_pid': 41, 'daemon_pid': 40, 'pane_tty': '/dev/ttys099'}
    marker = {**expected, 'argv': expected['argv'][2:], 'identity': child['before'], 'challenge': 'nonce'}
    A.check_child(child, copy.deepcopy(child), public, daemon, expected)
    A.check_marker(marker, child, expected, 'nonce')
    passed = ['positive_exact_child_and_challenge']
    def rejected(name, fn):
        try: fn()
        except (A.Unproven, A.Failed): passed.append(name)
        else: raise AssertionError('insensitive observer: ' + name)
    for section, field, value in [('image', 'executable', '/wrong'), ('image', 'sha256', 'wrong'),
                                  ('args', 'argv', ['interpreted']), ('args', 'argv_base64', 'AA=='),
                                  ('cwd', 'path', '/wrong'), ('fd0', 'path', '/pipe'),
                                  ('before', 'ppid', 99), ('before', 'birth', [0, 1])]:
        bad = copy.deepcopy(child); bad[section][field] = value
        rejected('wrong_child_' + section + '_' + field,
                 lambda bad=bad: A.check_child(bad, bad, public, daemon, expected))
    bad = copy.deepcopy(child); bad['environment'] = {'PATH': '/contaminated'}
    rejected('kernel_environment', lambda: A.check_child(bad, bad, public, daemon, expected))
    rejected('public_pane', lambda: A.check_child(child, child, {**public, 'pane_pid': 42}, daemon, expected))
    rejected('public_daemon', lambda: A.check_child(child, child, {**public, 'daemon_pid': 42}, daemon, expected))
    for field, value in [('kind', 'A'), ('token', 'old'), ('challenge', 'stale'), ('cwd', '/bad'),
                         ('argv', ['bad']), ('environment', {}), ('identity', {'pid': 41, 'birth': [0, 1]})]:
        rejected('wrong_marker_' + field, lambda field=field, value=value:
                 A.check_marker({**marker, field: value}, child, expected, 'nonce'))
    rejected('command_count_before_exec', lambda: A.command_budget({'commands': [{}] * 64}))
    rejected('command_record_reserve', lambda: A.command_budget({'commands': [], 'oversize': 'x' * (400 * 1024)}))
    clean = {'pass': True, 'all_absent': True, 'socket_absent': True, 'fds_equal': True,
             'errors': [], 'rescues': [], 'processes': [{'before': child['before'], 'after': None, 'exit_event': True}]}
    A.check_cleanup(clean); passed.append('positive_cleanup')
    for key, value in [('pass', False), ('all_absent', False), ('socket_absent', False),
                       ('fds_equal', False), ('errors', ['error']), ('rescues', ['kill'])]:
        rejected('cleanup_' + key, lambda key=key, value=value: A.check_cleanup({**clean, key: value}))
    rejected('cleanup_missing_exit', lambda: A.check_cleanup({**clean, 'processes': [{**clean['processes'][0], 'exit_event': False}]}))
    rejected('cleanup_live_birth', lambda: A.check_cleanup({**clean, 'processes': [{**clean['processes'][0], 'after': child['before']}]}))
    pins = copy.deepcopy(config['api_pins']); pins[0]['sha256'] = 'wrong'
    rejected('API_provenance', lambda: A.bind_api(pins))
    with tempfile.TemporaryDirectory(prefix='L5-012-pure-') as tmp:
        path = Path(tmp) / 'oversize.json'
        rejected('file_cap_before_write', lambda: A.write_once(path, 'x' * A.FILE_CAP, A.FILE_CAP))
        assert not path.exists()
        for field, value in [('environment', {}), ('identity', {'pid': 41, 'birth': [0, 1]})]:
            run = object.__new__(A.Run); run.root = Path(tmp); run.children = {}
            run.record = {'observations': []}
            run.public = lambda target: {'public': public, 'daemon': daemon}
            run.track = lambda own, role: own
            run.expected_for = lambda label: expected
            bad = {**marker, 'challenge': None, field: value}
            run.wait_json = lambda path, bad=bad: bad
            run.snapshot = lambda pid: child
            saved = []; run.checkpoint = lambda: saved.append(copy.deepcopy(run.record))
            original = D.birth; D.birth = lambda pid: child['before']
            try: rejected('early_failure_retention_' + field, lambda: run.observe('d1', 'alpha:0.0'))
            finally: D.birth = original
            item = saved[-1]['observations'][0]
            assert item['first'] == child and item['born'] == bad
            assert item['status'] in ('FAIL', 'UNPROVEN') and 'request' not in item
    for phase in ('oracle', 'candidate'):
        plan = A.plan(config, phase)
        assert len(plan) == 7 and len({p[1] for p in plan}) == 7
        assert plan[-1][2] == ['respawn-pane', '-k', '-t', 'alpha:local.0']
        assert A.environment(config, phase)['PATH'] == '/usr/bin:/bin'
        assert A.environment(config, phase)['RMUX_DISABLE_TMUX_SHIM'] == '1'
    return {'at': A.now(), 'status': 'PASS', 'checks': passed,
            'driver_sha256': A.digest(A.__file__), 'product_runs': 0}


if __name__ == '__main__':
    config = json.loads(Path(sys.argv[1]).read_text())
    result = checks(config)
    A.write_once(Path(sys.argv[2]), result)
    print(json.dumps(result))
