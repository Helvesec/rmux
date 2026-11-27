#!/usr/bin/env python3
"""Pure discriminating checks for the Darwin default-shell observer."""
import base64
from copy import deepcopy
import json
import hashlib
from pathlib import Path
import stat
import struct
import sys
import unittest
from unittest.mock import patch

import default_shell_darwin as d

ROOT = Path('/private/synthetic-default-shell')
PINS = {k: {'path': '/bin/' + k, 'bytes': 100, 'sha256': str(i) * 64}
        for i, k in enumerate(('bash', 'zsh', 'sleep', 'tmux', 'rmux', 'rmux_daemon'), 1)}


def fake(pid=41, parent=40, kind='bash', argv=None, pane=True):
    argv = argv or ['bash', '-c', d.TEXT]
    raw = struct.pack('=i', len(argv)) + ('/bin/' + kind).encode() + b'\0\0\0'
    raw += b'\0'.join(x.encode() for x in argv) + b'\0'
    i = {'pid': pid, 'birth': [pid * 10, 123], 'ppid': parent, 'pgid': pid, 'status': 2}
    return {'before': i, 'after': dict(i), 'image': {'executable': '/bin/' + kind,
            'sha256': PINS[kind]['sha256']}, 'args': d.parse_procargs(raw),
            'cwd': {'path': str(ROOT), 'dev': 1, 'ino': 2, 'returned_size': 2352},
            'fd0': {'path': '/dev/ttys999', 'mode': stat.S_IFCHR | 0o620} if pane else None}


class Controls(unittest.TestCase):
    def setUp(self):
        self.child = fake()
        self.daemon = fake(40, 1, 'tmux', ['tmux'], False)
        self.public = {'pane_pid': 41, 'daemon_pid': 40, 'pane_tty': '/dev/ttys999'}
        self.expected = {k: self.child['args'][k] for k in ('argv', 'argv_base64')}

    def check(self, a=None, b=None, pub=None):
        a = a or self.child
        d.check_child(a, b or deepcopy(a), pub or self.public, self.daemon,
                      's5text', PINS, ROOT, self.expected)

    def test_exact_positive(self):
        self.check()

    def test_wrong_executable_path_hash_cwd_and_pty(self):
        for section, field, value in [('image', 'executable', '/bin/zsh'),
                ('image', 'sha256', PINS['zsh']['sha256']), ('cwd', 'path', '/elsewhere'),
                ('fd0', 'mode', stat.S_IFIFO), ('fd0', 'path', '/dev/ttys000')]:
            with self.subTest(field=field):
                a = deepcopy(self.child); a[section][field] = value
                with self.assertRaises((d.Unproven, d.Failed)): self.check(a)

    def test_exact_argv0_and_c_and_extra_arg_rejected(self):
        for argv in [['/bin/bash', '-c', d.TEXT], ['bash', d.TEXT], ['bash', '-c', d.TEXT, 'extra']]:
            with self.subTest(argv=argv), self.assertRaises(d.Failed): self.check(fake(argv=argv))

    def test_pid_birth_parent_and_pane_mismatch(self):
        for key, value in [('pid', 99), ('birth', [1, 2]), ('ppid', 77)]:
            a = deepcopy(self.child); a['before'][key] = value
            with self.subTest(key=key), self.assertRaises(d.Unproven): self.check(a, self.child)
        for key in ('pane_pid', 'daemon_pid', 'pane_tty'):
            pub = {**self.public, key: 'wrong'}
            with self.subTest(key=key), self.assertRaises(d.Unproven): self.check(pub=pub)

    def test_raw_prefix_and_argv_contradiction(self):
        a = deepcopy(self.child); a['args']['argv_base64'] = base64.b64encode(b'wrong\0').decode()
        with self.assertRaises(d.Failed): self.check(a)
        a = deepcopy(self.child); a['args']['procargs_prefix_base64'] = fake(argv=['bash'])['args']['procargs_prefix_base64']
        with self.assertRaises(d.Failed): self.check(a)

    def test_parser_discards_environment_and_preserves_empty_arg(self):
        raw = struct.pack('=i', 3) + b'/bin/example\0\0example\0\0third\0SECRET=excluded\0'
        result = d.parse_procargs(raw)
        self.assertEqual(result['argv'], ['example', '', 'third'])
        self.assertNotIn(b'SECRET', base64.b64decode(result['procargs_prefix_base64']))
        self.assertEqual(d.parse_procargs(base64.b64decode(result['procargs_prefix_base64'])), result)

    def test_parser_malformed_inputs_rejected(self):
        for raw in [b'', struct.pack('=i', 0) + b'abc\0x\0', struct.pack('=i', 129) + b'abc\0x\0',
                    struct.pack('=i', 2) + b'abc\0x\0', struct.pack('=i', 1) + b'abc']:
            with self.subTest(raw=raw), self.assertRaises(d.Unproven): d.parse_procargs(raw)

    def test_provenance_environment_plan_guards(self):
        frozen = {'phase': 'oracle', 'root': str(ROOT), 'pins': PINS,
                  'artifacts': list(PINS.values()), 'plan': json.loads(json.dumps(d.plan())),
                  'environment': d.environment(ROOT)}
        with patch.object(d, 'pin', side_effect=lambda path: next(v for v in PINS.values() if v['path'] == path)):
            d.verify_frozen(frozen)
            bad = deepcopy(frozen); bad['artifacts'].pop()
            with self.assertRaises(d.Unproven): d.verify_frozen(bad)
            bad = deepcopy(frozen); bad['environment']['BASH_ENV'] = '/contaminated'
            with self.assertRaises(d.Unproven): d.verify_frozen(bad)
            bad = deepcopy(frozen); bad['plan'][0][2].append('/bin/bash')
            with self.assertRaises(d.Unproven): d.verify_frozen(bad)
        with patch.object(d, 'pin', return_value={'wrong': True}), self.assertRaises(d.Unproven):
            d.verify_frozen(frozen)

    def test_profile_contamination_guard(self):
        fake_run = object.__new__(d.Run)
        fake_run.root = ROOT; fake_run.record = {'profiles': []}
        with patch.object(Path, 'iterdir', return_value=[Path('.bash_profile')]), self.assertRaises(d.Unproven):
            fake_run.profiles()

    def test_history_only_after_shutdown_empty_owned_regular(self):
        row = {'post_shutdown': True, 'entries': {'home': [], 'zdot': ['.zsh_history'], 'xdg': []},
               'history': {'uid': 501, 'mode': stat.S_IFREG | 0o600, 'bytes': 0,
                           'sha256': hashlib.sha256(b'').hexdigest()}}
        d.check_profiles(row, 501)
        bad = deepcopy(row); bad['post_shutdown'] = False
        with self.assertRaises(d.Unproven): d.check_profiles(bad, 501)
        for key, value in [('bytes', 1), ('sha256', '0' * 64), ('uid', 0), ('mode', stat.S_IFLNK | 0o700)]:
            bad = deepcopy(row); bad['history'][key] = value
            with self.subTest(key=key), self.assertRaises(d.Unproven): d.check_profiles(bad, 501)
        for directory in ('home', 'zdot', 'xdg'):
            bad = deepcopy(row); bad['entries'][directory].append('.profile')
            with self.subTest(directory=directory), self.assertRaises(d.Unproven): d.check_profiles(bad, 501)


if __name__ == '__main__':
    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Controls))
    if len(sys.argv) == 2:
        d.write_once(Path(sys.argv[1]), {'status': 'PASS' if result.wasSuccessful() else 'FAIL',
                     'tests': result.testsRun, 'failures': len(result.failures), 'errors': len(result.errors),
                     'native_processes': 0, 'fixtures_synthetic': True, 'source': d.pin(d.__file__)})
    raise SystemExit(not result.wasSuccessful())
