"""Private ownership regressions: all process effects are simulated."""

import importlib.util
import copy
import base64
from contextlib import ExitStack
import json
from pathlib import Path
import signal
import subprocess
import unittest
from unittest.mock import Mock, patch


SPEC = importlib.util.spec_from_file_location(
    'renumber_reference', Path(__file__).with_name('renumber_windows_reference.py'))
REF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REF)


class Simulation:
    """No subprocess, /proc access, real clock wait, pidfd, signal or reap."""

    DAEMON, CHILD, HELPER, FOREIGN = 410010, 410011, 410012, 410099

    def __init__(self):
        self.config = {'root': '/SIMULATED/renumber', 'oracle': '/tmux',
                       'python': '/python', 'tokens': {'oracle': 'token'},
                       'hashes': {'python': 'python-pin'}}
        self.run = REF.Run(self.config, 'oracle', 0)
        self.expected = self.run.expected('a4')
        self.run.expected = Mock(return_value=copy.deepcopy(self.expected))
        self.live = {}
        self.snapshots = {}
        for pid, parent in ((self.DAEMON, 1), (self.CHILD, self.DAEMON),
                            (self.HELPER, self.DAEMON), (self.FOREIGN, 999)):
            own = {'pid': pid, 'ppid': parent, 'start_ticks': str(pid * 10), 'state': 'S'}
            self.live[pid] = own
            self.snapshots[pid] = {**copy.deepcopy(self.expected), 'before': own,
                                  'after': dict(own), 'coherent': True, 'fd0': '/dev/pts/9',
                                  'cmdline_base64': base64.b64encode(
                                      b'\0'.join(v.encode() for v in self.expected['argv']) + b'\0').decode()}
        self.public = {'pane_pid': self.CHILD, 'daemon_pid': self.DAEMON,
                       'pane_dead': '0', 'pane_dead_status': '', 'pane_dead_signal': ''}
        self.born = {**copy.deepcopy(self.expected), 'argv': self.expected['argv'][2:],
                     'identity': dict(self.live[self.CHILD]), 'challenge': None}
        self.ack = {**copy.deepcopy(self.born), 'challenge': 'nonce',
                    'tty': {'icanon': True, 'icrnl': True}, 'pending_bytes_base64': ''}
        self.tree, self.signals, self.reaps, self.marker_reads = {}, [], [], []
        self.ticks = 0.0
        self.failure = None
        self.run.owned[self.HELPER] = dict(self.live[self.HELPER])
        self.run.public = self.read_public
        self.run.wait_json = self.read_marker
        self.run.fresh_nonce = Mock(return_value='nonce')
        self.run.command = Mock(return_value='')
        self.run.profiles = Mock()
        self.run.socket_identity = {'private_socket': 'qualified'}
        self.run.socket_info = Mock(return_value=None)

    def read_public(self, target):
        daemon = copy.deepcopy(self.snapshots[self.DAEMON])
        self.run.daemon = daemon
        self.run.owned[self.DAEMON] = dict(daemon['before'])
        return dict(self.public), daemon

    def read_marker(self, path):
        self.marker_reads.append(str(path))
        # Initial attribution has no cleanup authority while waiting.
        if not self.run.children:
            assert self.public['pane_pid'] not in self.run.owned
        if self.failure:
            error, read_number = self.failure
            if len(self.marker_reads) == read_number:
                raise error
        return copy.deepcopy(self.born if len(self.marker_reads) % 2 else self.ack)

    def proc_path(self, *parts):
        assert parts[0] == '/proc', 'unexpected filesystem access'
        pid = int(parts[1])
        from unittest.mock import MagicMock
        task = MagicMock()
        task.__truediv__.return_value.read_text.return_value = ' '.join(
            str(child) for child in self.tree.get(pid, []) if child in self.live)
        result = Mock()
        result.iterdir.return_value = [task]
        return result

    def sleep(self, seconds):
        self.ticks += seconds

    def send(self, old, signum):
        self.signals.append((old['pid'], int(signum)))
        self.live.pop(old['pid'], None)
        return {'simulated_pid': old['pid'], 'signal': int(signum)}

    def reap(self, pid, options):
        self.reaps.append(pid)
        if pid > 0 and self.live.get(pid, {}).get('state') == 'Z':
            self.live.pop(pid)
            return pid, 0
        return 0, 0

    def __enter__(self):
        self.stack = ExitStack()
        patches = [patch.multiple(REF, identity=lambda pid: copy.deepcopy(self.live.get(pid)),
                                  proc=lambda pid: copy.deepcopy(self.snapshots.get(pid))
                                  if pid in self.live else None,
                                  Path=self.proc_path, signal_owned=self.send, atomic_once=Mock()),
                   patch.object(REF.time, 'monotonic', side_effect=lambda: self.ticks),
                   patch.object(REF.time, 'sleep', side_effect=self.sleep),
                   patch.object(REF.os, 'waitpid', side_effect=self.reap),
                   patch.object(REF.os, 'kill', side_effect=AssertionError('real signal')),
                   patch.object(REF.os, 'pidfd_open', create=True,
                                side_effect=AssertionError('real pidfd')),
                   patch.object(REF.subprocess, 'Popen', side_effect=AssertionError('real process'))]
        for context in patches:
            self.stack.enter_context(context)
        return self

    def __exit__(self, *error):
        return self.stack.__exit__(*error)

    def assert_unowned(self, test, pid):
        test.assertNotIn(pid, self.run.owned)
        test.assertNotIn(pid, [target for target, _ in self.signals])
        test.assertNotIn(pid, self.reaps)
        test.assertNotIn(-1, self.reaps)
        test.assertIn(pid, self.live)
        test.assertIn((self.HELPER, int(signal.SIGTERM)), self.signals)


class OwnershipTests(unittest.TestCase):
    def test_foreign_pane_marker_failure_then_cleanup(self):
        foreign = {'pid': 410041, 'start_ticks': 'foreign', 'state': 'S'}
        helper = {'pid': 410040, 'start_ticks': 'owned', 'state': 'S'}
        live = {v['pid']: dict(v) for v in (foreign, helper)}
        run = REF.Run.__new__(REF.Run)
        run.root = Path('/SIMULATED/renumber/oracle')
        run.start, run.case_end, run.cleaning = 0, 20, False
        run.owned, run.children = {helper['pid']: helper}, {}
        run.daemon, run.socket_identity = None, None
        run.record = {'observations': []}
        run.public = Mock(return_value=({'pane_pid': foreign['pid']}, None))
        run.fresh_nonce = Mock(return_value='fresh')
        run.wait_json = Mock(side_effect=REF.Unproven('marker timeout'))
        run.profiles, run.socket_info = Mock(), Mock(return_value=None)
        ticks, signals = [0.0], []

        def sleep(seconds):
            ticks[0] += seconds

        def send(old, signum):
            signals.append((old['pid'], signum))
            live.pop(old['pid'], None)
            return {'simulated_pid': old['pid'], 'signal': int(signum)}

        proc_path = Mock()
        proc_path.iterdir.return_value = []
        with patch.multiple(REF, identity=lambda pid: live.get(pid),
                            signal_owned=send, Path=Mock(return_value=proc_path)), \
                patch.object(REF.time, 'monotonic', side_effect=lambda: ticks[0]), \
                patch.object(REF.time, 'sleep', side_effect=sleep), \
                patch.object(REF.os, 'waitpid', return_value=(0, 0)), \
                patch.object(REF.os, 'kill', side_effect=AssertionError('real signal')), \
                patch.object(REF.os, 'pidfd_open', create=True,
                             side_effect=AssertionError('real pidfd')), \
                patch.object(REF.subprocess, 'Popen', side_effect=AssertionError('real process')):
            try:
                with self.assertRaisesRegex(REF.Unproven, 'marker timeout'):
                    run.observe('a4', 'alpha:a4.0')
            finally:
                run.cleanup()

        self.assertNotIn(foreign['pid'], run.owned,
                         'an observed PID must not acquire cleanup ownership')
        self.assertNotIn(foreign['pid'], [pid for pid, _ in signals])
        self.assertIn((helper['pid'], signal.SIGTERM), signals)
        self.assertEqual(live, {foreign['pid']: foreign})
        self.assertEqual(run.record['cleanup']['status'], 'PASS')

    def test_attribution_failure_matrix(self):
        cases = {
            'missing_marker': (lambda s: setattr(s, 'failure', (REF.Unproven('marker timeout'), 1)), 'marker timeout'),
            'marker_error': (lambda s: setattr(s, 'failure', (json.JSONDecodeError('invalid marker', '', 0), 1)), 'invalid marker'),
            'marker_eof': (lambda s: setattr(s, 'failure', (EOFError('marker EOF'), 1)), 'marker EOF'),
            'challenge_timeout': (lambda s: setattr(s, 'failure', (REF.Unproven('challenge timeout'), 2)), 'challenge timeout'),
            'challenge_eof': (lambda s: setattr(s, 'failure', (EOFError('challenge EOF'), 2)), 'challenge EOF'),
            'marker_birth': (lambda s: s.born['identity'].update(start_ticks='stale'), 'marker PID/birth'),
            'marker_token': (lambda s: s.born.update(token='wrong'), 'marker token'),
            'marker_environment': (lambda s: s.born.update(environment={}), 'marker environment'),
            'wrong_argv': (lambda s: s.snapshots[s.CHILD].update(argv=['wrong']), 'child argv'),
            'wrong_parent': (lambda s: s.snapshots[s.CHILD]['before'].update(ppid=999), 'missing own ancestor'),
            'wrong_challenge': (lambda s: s.ack.update(challenge='stale'), 'fresh challenge echo'),
            'ack_birth': (lambda s: s.ack['identity'].update(start_ticks='stale'), 'marker PID/birth'),
            'wrong_tty': (lambda s: s.ack.update(tty={}), 'canonical helper'),
            'pending_input': (lambda s: s.ack.update(pending_bytes_base64='WA=='), 'no partial unexpected input'),
        }
        for name, (mutate, message) in cases.items():
            with self.subTest(case=name):
                sim = Simulation()
                mutate(sim)
                with sim:
                    try:
                        with self.assertRaisesRegex(Exception, message):
                            sim.run.observe('a4', 'alpha:a4.0')
                    finally:
                        sim.run.cleanup()
                sim.assert_unowned(self, sim.CHILD)
                self.assertNotIn('a4', sim.run.children)

    def test_complete_attribution_authorizes_cleanup(self):
        sim = Simulation()
        with sim:
            observed = sim.run.observe('a4', 'alpha:a4.0')
            self.assertEqual(observed['status'], 'PASS')
            self.assertEqual(sim.run.owned[sim.CHILD], sim.live[sim.CHILD])
            sim.run.cleanup()
        self.assertIn((sim.CHILD, int(signal.SIGTERM)), sim.signals)
        self.assertNotIn(-1, sim.reaps)

    def test_observed_birth_must_match_first_snapshot(self):
        sim = Simulation()
        sim.live[sim.CHILD] = {**sim.live[sim.CHILD], 'start_ticks': 'earlier-birth'}
        with sim:
            try:
                with self.assertRaisesRegex(REF.Unproven, 'observed child birth changed'):
                    sim.run.observe('a4', 'alpha:a4.0')
            finally:
                sim.run.cleanup()
        sim.assert_unowned(self, sim.CHILD)

    def test_reobserve_failure_preserves_previous_owned_helper(self):
        sim = Simulation()
        with sim:
            original = sim.run.observe('a4', 'alpha:a4.0')
            sim.public['pane_pid'] = sim.FOREIGN
            sim.failure = (REF.Unproven('marker timeout'), 3)
            try:
                with self.assertRaisesRegex(REF.Unproven, 'marker timeout'):
                    sim.run.observe('a4', '%owned', True)
            finally:
                sim.run.cleanup()
        self.assertIs(sim.run.children['a4'], original)
        sim.assert_unowned(self, sim.FOREIGN)
        self.assertIn((sim.CHILD, int(signal.SIGTERM)), sim.signals)

    def test_unrelated_harness_child_is_never_discovered_or_reaped(self):
        sim = Simulation()
        sim.live[sim.FOREIGN]['ppid'] = REF.os.getpid()
        sim.live[sim.FOREIGN]['state'] = 'Z'
        sim.tree[REF.os.getpid()] = [sim.FOREIGN]
        with sim:
            sim.run.cleanup()
        sim.assert_unowned(self, sim.FOREIGN)

    def test_private_daemon_descendant_chain_is_owned(self):
        sim = Simulation()
        sim.live[sim.FOREIGN]['ppid'] = sim.CHILD
        sim.tree = {sim.DAEMON: [sim.CHILD], sim.CHILD: [sim.FOREIGN]}
        with sim:
            sim.read_public('target')
            sim.run.cleanup()
        self.assertEqual({v['pid'] for v in sim.run.record['cleanup']['ownership_discovery']},
                         {sim.CHILD, sim.FOREIGN})
        self.assertIn((sim.CHILD, int(signal.SIGTERM)), sim.signals)
        self.assertIn((sim.FOREIGN, int(signal.SIGTERM)), sim.signals)
        self.assertNotIn(-1, sim.reaps)

    def test_discovery_rejects_unlinked_or_reused_root(self):
        for mode in ('unlinked_child', 'reused_root'):
            with self.subTest(mode=mode):
                sim = Simulation()
                sim.tree[sim.HELPER] = [sim.FOREIGN]
                if mode == 'reused_root':
                    sim.live[sim.FOREIGN]['ppid'] = sim.HELPER
                    sim.live[sim.HELPER] = {**sim.live[sim.HELPER], 'start_ticks': 'reused'}
                with sim:
                    sim.run.cleanup()
                self.assertNotIn(sim.FOREIGN, sim.run.owned)
                self.assertNotIn(sim.FOREIGN, [pid for pid, _ in sim.signals])
                self.assertNotIn(sim.FOREIGN, sim.reaps)

    def test_owned_zombie_reaped_by_exact_pid(self):
        sim = Simulation()
        sim.live[sim.HELPER]['state'] = 'Z'
        with sim:
            sim.run.cleanup()
        self.assertIn(sim.HELPER, sim.reaps)
        self.assertNotIn(-1, sim.reaps)
        self.assertFalse(sim.signals)

    def test_reused_owned_pid_neither_signaled_nor_reaped(self):
        sim = Simulation()
        sim.live[sim.HELPER] = {**sim.live[sim.HELPER], 'start_ticks': 'foreign-birth'}
        with sim:
            sim.run.cleanup()
        self.assertFalse(sim.signals)
        self.assertNotIn(sim.HELPER, sim.reaps)

    def test_discovery_rejects_birth_changes_during_enumeration(self):
        for mode in ('parent', 'child'):
            with self.subTest(mode=mode):
                sim = Simulation()
                sim.tree[sim.HELPER] = [sim.FOREIGN]
                sim.live[sim.FOREIGN]['ppid'] = sim.HELPER
                calls = {}

                def read_identity(pid):
                    calls[pid] = calls.get(pid, 0) + 1
                    old = copy.deepcopy(sim.live.get(pid))
                    if (mode == 'parent' and pid == sim.HELPER and calls[pid] == 2):
                        sim.live[pid] = {**old, 'start_ticks': 'reused-parent'}
                        return dict(sim.live[pid])
                    if mode == 'child' and pid == sim.FOREIGN and calls[pid] == 1:
                        sim.live[pid] = {**old, 'start_ticks': 'reused-child'}
                    return old

                with sim, patch.object(REF, 'identity', side_effect=read_identity):
                    sim.run.cleanup()
                self.assertNotIn(sim.FOREIGN, sim.run.owned)
                self.assertNotIn(sim.FOREIGN, [pid for pid, _ in sim.signals])
                self.assertNotIn(sim.FOREIGN, sim.reaps)

    def test_second_snapshot_change_never_authorizes_cleanup(self):
        for mode in ('birth', 'incomplete'):
            with self.subTest(mode=mode):
                sim = Simulation()
                reads = []

                def snapshot(pid):
                    value = copy.deepcopy(sim.snapshots.get(pid)) if pid in sim.live else None
                    if pid == sim.CHILD:
                        reads.append(pid)
                        if len(reads) == 2:
                            if mode == 'birth':
                                value['before']['start_ticks'] = 'other-birth'
                            else:
                                value['coherent'] = False
                    return value

                with sim, patch.object(REF, 'proc', side_effect=snapshot):
                    try:
                        with self.assertRaisesRegex(REF.Unproven, 'two child snapshots incoherent'):
                            sim.run.observe('a4', 'alpha:a4.0')
                    finally:
                        sim.run.cleanup()
                sim.assert_unowned(self, sim.CHILD)

    def test_kill_server_error_keeps_owned_helper_fallback(self):
        sim = Simulation()
        sim.run.socket_info = lambda: sim.run.socket_identity if sim.DAEMON in sim.live else None
        sim.run.command = Mock(side_effect=OSError('simulated private server error'))
        with sim:
            sim.read_public('target')
            sim.run.cleanup()
        self.assertIn('simulated private server error', sim.run.record['cleanup']['kill_server_error'])
        sim.assert_unowned(self, sim.FOREIGN)

    def test_nonchild_wait_error_does_not_expand_reap_authority(self):
        sim = Simulation()
        with sim, patch.object(REF.os, 'waitpid', side_effect=ChildProcessError) as wait:
            sim.run.cleanup()
        self.assertTrue(wait.call_args_list)
        self.assertEqual({call.args[0] for call in wait.call_args_list}, {sim.HELPER})
        self.assertIn((sim.HELPER, int(signal.SIGTERM)), sim.signals)

    def test_owned_command_errors_and_timeouts(self):
        for mode in ('success', 'timeout', 'eof', 'error', 'budget'):
            with self.subTest(mode=mode):
                sim = Simulation()
                process = Mock(pid=sim.HELPER, returncode=0)
                process.communicate.return_value = ('', '')
                if mode == 'timeout':
                    process.communicate.side_effect = [subprocess.TimeoutExpired('own', 1), ('', '')]
                elif mode in ('eof', 'error'):
                    process.communicate.side_effect = EOFError('own EOF') if mode == 'eof' else OSError('own error')
                sim.run.owned = {}
                with sim, patch.object(REF.subprocess, 'Popen', return_value=process), \
                        patch.object(REF.os, 'getpid', return_value=sim.DAEMON):
                    if mode == 'budget':
                        sim.run.remaining = Mock(side_effect=[10, REF.Unproven('time budget exhausted')])
                    if mode == 'success':
                        REF.Run.command(sim.run, ['display-message'])
                    else:
                        with self.assertRaises((REF.Unproven, EOFError, OSError)):
                            REF.Run.command(sim.run, ['display-message'])
                    self.assertEqual(sim.run.owned[sim.HELPER]['start_ticks'], str(sim.HELPER * 10))
                    sim.run.cleanup()
                self.assertIn(sim.HELPER, [pid for pid, _ in sim.signals])
                self.assertNotIn(sim.FOREIGN, sim.reaps)

    def test_existing_pure_controls(self):
        config = {'root': '/SIMULATED/renumber', 'python': '/python',
                  'tokens': {'oracle': 'oracle-token', 'candidate': 'candidate-token'}}
        result = REF.pure_checks(config)
        self.assertEqual(result['status'], 'PASS')
        self.assertEqual(result['product_runs'], 0)


if __name__ == '__main__':
    unittest.main()
