#!/usr/bin/env python3
"""Pure B1--B4 sensitivity controls; no product, process or FFI observation."""
import copy
import json
from pathlib import Path
import stat
import struct
import sys
import tempfile
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import base_index_darwin as A


def checks(config):
    A.bind_api(config); D = A.D
    def public(label, number):
        return {'daemon_pid': 40, 'socket': '/s', 'session_id': '$1' if label == 'b4' else '$0',
                'window_id': '@' + str(number), 'pane_id': '%' + str(number), 'pane_pid': 41 + number,
                'pane_dead': '0', 'pane_dead_status': '', 'pane_dead_signal': '',
                'session_name': 'beta' if label == 'b4' else 'alpha', 'window_index': A.INDICES[label],
                'window_name': label, 'pane_tty': '/dev/ttys099'}
    children = {label: {'public': public(label, n)} for n, label in enumerate(A.KINDS)}
    rows = [c['public'] for c in children.values()]
    windows = [{**{k: r[k] for k in A.WINDOW_KEYS if k != 'window_panes'}, 'window_panes': 1} for r in rows]
    A.check_layout(rows, windows, children)
    passed = ['positive_B4_exact_inventory']
    def rejected(name, fn):
        try: fn()
        except (A.Unproven, A.Failed): passed.append(name)
        else: raise AssertionError('insensitive observer: ' + name)
    rejected('explicit12_ignored_for7', lambda: A.check_index({**children['a12']['public'], 'window_index': 7}, 'a12'))
    rejected('unset_chooses_occupied4', lambda: A.check_index({**children['a5']['public'], 'window_index': 4}, 'a5'))
    bad = copy.deepcopy(rows); bad[0]['window_index'] = 5
    rejected('unset_renumbers_old4', lambda: A.check_layout(bad, windows, children))
    bad = copy.deepcopy(rows); bad[-1]['pane_id'] = bad[0]['pane_id']
    rejected('duplicate_pane_ID', lambda: A.check_layout(bad, windows, children))
    rejected('extra_window', lambda: A.check_layout(rows, windows + [windows[0]], children))
    rejected('extra_pane', lambda: A.check_layout(rows + [rows[0]], windows, children))
    rejected('extra_helper', lambda: A.check_no_extra([{'pid': 999}]))
    expected = {'label': 'a4', 'kind': 'a4', 'exe': '/python', 'exe_sha256': 'pin',
                'argv': ['/python', '-I', '/driver', 'witness', 'a4', '0'],
                'cwd': '/cwd', 'token': 'fresh', 'exit_code': 0, 'environment': {'HOME': '/empty'}}
    def fake(pid, parent):
        own = {'pid': pid, 'ppid': parent, 'birth': [10, pid], 'status': 2}
        raw = struct.pack('=i', len(expected['argv'])) + b'/python\0\0' + b'\0'.join(v.encode() for v in expected['argv']) + b'\0'
        return {'before': own, 'after': dict(own), 'image': {'executable': '/python', 'sha256': 'pin'},
                'args': D.parse_procargs(raw), 'cwd': {'path': '/cwd'}, 'environment': expected['environment'],
                'fd0': {'path': '/dev/ttys099', 'mode': stat.S_IFCHR}}
    child, daemon = fake(41, 40), fake(40, 1); row = children['a4']['public']
    marker = {**expected, 'argv': expected['argv'][2:], 'identity': child['before'], 'challenge': 'nonce'}
    A.check_child(child, copy.deepcopy(child), row, daemon, expected)
    A.check_marker(marker, child, expected, 'nonce'); passed.append('positive_live_argv_birth_nonce')
    for field, value in [('challenge', 'stale'), ('identity', {'pid': 41, 'birth': [0, 1]}),
                         ('argv', ['wrong']), ('environment', {}), ('exit_code', 7)]:
        rejected('marker_' + field, lambda field=field, value=value:
                 A.check_marker({**marker, field: value}, child, expected, 'nonce'))
    for section, field, value in [('image', 'executable', '/wrong'), ('image', 'sha256', 'wrong'),
                                  ('args', 'argv_base64', 'AA=='), ('cwd', 'path', '/bad'),
                                  ('fd0', 'path', '/pipe'), ('before', 'birth', [0, 0])]:
        bad = copy.deepcopy(child); bad[section][field] = value
        rejected('child_' + section + '_' + field,
                 lambda bad=bad: A.check_child(bad, bad, row, daemon, expected))
    watch = {'identity': child['before'], 'event_before_request': False, 'event_after_request': True}
    A.check_kernel_exit(child['before'], None, watch); passed.append('positive_NOTE_EXIT')
    rejected('intent_without_NOTE_EXIT', lambda: A.check_kernel_exit(child['before'], None, {**watch, 'event_after_request': False}))
    rejected('old_exit_birth', lambda: A.check_kernel_exit(child['before'], None, {**watch, 'identity': {'pid': 41, 'birth': [0, 0]}}))
    rejected('exited_birth_live', lambda: A.check_kernel_exit(child['before'], child['before'], watch))
    clean = {'pass': True, 'all_absent': True, 'socket_absent': True, 'fds_equal': True,
             'errors': [], 'rescues': [], 'processes': [{'before': child['before'], 'after': None, 'exit_event': True}]}
    owned = {41: child['before']}; A.check_cleanup(clean, owned); passed.append('positive_cleanup')
    for field, value in [('pass', False), ('processes', []), ('socket_absent', False), ('fds_equal', False), ('rescues', ['kill'])]:
        rejected('cleanup_' + field, lambda field=field, value=value: A.check_cleanup({**clean, field: value}, owned))
    rejected('stdout_reserve_before_exec', lambda: A.command_budget({'commands': [], 'stdout_bytes': A.STDOUT_CAP - A.FILE_CAP + 1}))
    rejected('command_count_before_exec', lambda: A.command_budget({'commands': [{}] * 128, 'stdout_bytes': 0}))
    bad = copy.deepcopy(config); bad['adapter_pin']['sha256'] = 'wrong'
    rejected('dependency_provenance', lambda: A.bind_api(bad))
    with tempfile.TemporaryDirectory(prefix='L5-014-pure-') as tmp:
        path = Path(tmp) / 'oversize.json'
        rejected('marker_cap_before_open', lambda: A.write_once(path, 'x' * A.FILE_CAP, A.FILE_CAP))
        assert not path.exists()
    for phase in ('oracle', 'candidate'):
        plan = A.plan(config, phase)
        assert len(plan) == 5 and plan[3][2][1:4] == ['-d', '-t', 'alpha:12']
        for _, _, argv, _ in plan:
            if argv[0] == 'new-session':
                assert argv[argv.index('-x') + 1] == '80' and argv[argv.index('-y') + 1] == '24'
        assert A.environment(config, phase)['RMUX_DISABLE_TMUX_SHIM'] == '1'
    return {'at': A.now(), 'status': 'PASS', 'checks': passed, 'driver_sha256': A.digest(A.__file__), 'product_runs': 0}


if __name__ == '__main__':
    result = checks(json.loads(Path(sys.argv[1]).read_text()))
    A.write_once(Path(sys.argv[2]), result)
    print(json.dumps(result))
