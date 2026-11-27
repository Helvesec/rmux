#!/usr/bin/env python3
"""Sensitivity checks for Darwin live/exit/respawn observations; no product run."""
import copy
import json
from pathlib import Path
import stat
import struct
import sys
import tempfile
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import remain_on_exit_darwin as A


def checks(config):
    A.bind_api(config); D = A.D
    expected = {'exe': '/python', 'exe_sha256': 'pin', 'argv': ['/python', '-I', '/driver', 'witness', 'seven', '7'],
                'cwd': '/cwd', 'kind': 'seven', 'token': 'fresh', 'exit_code': 7, 'environment': {'HOME': '/empty'}}
    def fake(pid, parent):
        own = {'pid': pid, 'ppid': parent, 'birth': [10, pid], 'status': 2}
        raw = struct.pack('=i', len(expected['argv'])) + b'/python\0\0' + b'\0'.join(v.encode() for v in expected['argv']) + b'\0'
        return {'before': own, 'after': dict(own), 'image': {'executable': '/python', 'sha256': 'pin'},
                'args': D.parse_procargs(raw), 'cwd': {'path': '/cwd'}, 'environment': expected['environment'],
                'fd0': {'path': '/dev/ttys099', 'mode': stat.S_IFCHR}}
    child, daemon = fake(41, 40), fake(40, 1)
    public = {'pane_pid': 41, 'daemon_pid': 40, 'pane_tty': '/dev/ttys099', 'socket': '/s',
              'session_id': '$0', 'window_id': '@1', 'pane_id': '%1',
              'pane_dead': '0', 'pane_dead_status': '', 'pane_dead_signal': ''}
    marker = {**expected, 'argv': expected['argv'][2:], 'identity': child['before'], 'challenge': 'nonce'}
    A.check_child(child, copy.deepcopy(child), public, daemon, expected)
    A.check_marker(marker, child, expected, 'nonce')
    passed = ['positive_live_exact_argv_nonce']
    def rejected(name, fn):
        try: fn()
        except (A.Unproven, A.Failed): passed.append(name)
        else: raise AssertionError('insensitive observer: ' + name)
    for field, value in [('challenge', 'stale'), ('identity', {'pid': 41, 'birth': [0, 1]}),
                         ('environment', {}), ('argv', ['wrong']), ('exit_code', 0)]:
        rejected('marker_' + field, lambda field=field, value=value:
                 A.check_marker({**marker, field: value}, child, expected, 'nonce'))
    rejected('dead_public_as_live', lambda: A.check_child(child, child, {**public, 'pane_dead': '1'}, daemon, expected))
    dead = copy.deepcopy(child); dead['before']['status'] = dead['after']['status'] = 5
    rejected('dead_birth_as_live', lambda: A.check_child(dead, dead, public, daemon, expected))
    for section, field, value in [('image', 'executable', '/wrong'), ('image', 'sha256', 'wrong'),
                                  ('args', 'argv', ['interpreted']), ('args', 'argv_base64', 'AA=='),
                                  ('cwd', 'path', '/bad'), ('fd0', 'path', '/pipe'), ('before', 'birth', [0, 0])]:
        bad = copy.deepcopy(child); bad[section][field] = value
        rejected('child_' + section + '_' + field,
                 lambda bad=bad: A.check_child(bad, bad, public, daemon, expected))
    dead_public = {**public, 'pane_dead': '1', 'pane_dead_status': '7'}
    exit_record = {'original_child': child, 'original_public': public,
                   'request': {'identity': child['before']},
                   'watch': {'event_before_request': False, 'event_after_request': True},
                   'identity_after_exit_event': None, 'expected_kept': True,
                   'expected_exit_code': 7, 'actual_public_exit_status': '7',
                   'pane_samples': [{'child_identity': None, 'rows': [dead_public]} for _ in range(2)]}
    A.check_exit(exit_record); passed.append('positive_retained_public_exit')
    removed = {**exit_record, 'expected_kept': False, 'actual_public_exit_status': None,
               'pane_samples': [{'child_identity': None, 'rows': []} for _ in range(2)]}
    A.check_exit(removed); passed.append('positive_removed_no_status_claim')
    rejected('intent_as_removed_exit_status', lambda: A.check_exit({**removed, 'actual_public_exit_status': '7'}))
    rejected('intent_without_kernel_exit', lambda: A.check_exit({**exit_record, 'watch': {'event_before_request': False, 'event_after_request': False}}))
    rejected('stale_exit_event', lambda: A.check_exit({**exit_record, 'watch': {'event_before_request': True, 'event_after_request': True}}))
    rejected('exited_birth_still_live', lambda: A.check_exit({**exit_record, 'identity_after_exit_event': child['before']}))
    rejected('missing_second_exit_sample', lambda: A.check_exit({**exit_record, 'pane_samples': exit_record['pane_samples'][:1]}))
    bad = copy.deepcopy(exit_record); bad['pane_samples'][-1]['rows'][0]['pane_dead_status'] = '0'
    rejected('false_public_exit_status', lambda: A.check_exit(bad))
    old = {'public': public, 'child': child, 'request': {'challenge': 'old'}}
    new = {'public': {**public, 'pane_pid': 42}, 'child': fake(42, 40), 'request': {'challenge': 'new'}}
    A.check_respawn(old, new); passed.append('positive_respawn_same_pane_fresh_child')
    for field in ('session_id', 'window_id', 'pane_id'):
        bad = copy.deepcopy(new); bad['public'][field] = 'wrong'
        rejected('respawn_' + field, lambda bad=bad: A.check_respawn(old, bad))
    rejected('respawn_same_birth', lambda: A.check_respawn(old, {**new, 'child': child}))
    rejected('respawn_stale_nonce', lambda: A.check_respawn(old, {**new, 'request': {'challenge': 'old'}}))
    bad = copy.deepcopy(new); bad['child']['args']['argv_base64'] = 'AA=='
    rejected('respawn_changed_argv', lambda: A.check_respawn(old, bad))
    clean = {'pass': True, 'all_absent': True, 'socket_absent': True, 'fds_equal': True,
             'errors': [], 'rescues': [], 'processes': [{'before': child['before'], 'after': None, 'exit_event': True}]}
    A.A.check_cleanup(clean); passed.append('positive_cleanup')
    for field, value in [('pass', False), ('all_absent', False), ('socket_absent', False), ('fds_equal', False), ('rescues', ['kill'])]:
        rejected('cleanup_' + field, lambda field=field, value=value: A.A.check_cleanup({**clean, field: value}))
    bad = copy.deepcopy(config); bad['adapter_pin']['sha256'] = 'wrong'
    rejected('dependency_provenance', lambda: A.bind_api(bad))
    with tempfile.TemporaryDirectory(prefix='L5-013-pure-') as tmp:
        path = Path(tmp) / 'oversize.json'
        rejected('file_cap_before_open', lambda: A.write_once(path, 'x' * A.FILE_CAP, A.FILE_CAP))
        assert not path.exists()
    for phase in ('oracle', 'candidate'):
        plan = A.plan(config, phase)
        assert len(plan) == 8 and plan[4][2] == ['respawn-pane', '-t', 'alpha:seven.0']
        assert A.environment(config, phase)['RMUX_DISABLE_TMUX_SHIM'] == '1'
    return {'at': A.now(), 'status': 'PASS', 'checks': passed, 'driver_sha256': A.digest(A.__file__), 'product_runs': 0}


if __name__ == '__main__':
    result = checks(json.loads(Path(sys.argv[1]).read_text()))
    A.write_once(Path(sys.argv[2]), result)
    print(json.dumps(result))
