"""Private harness ownership controls; no product executable is invoked."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

import product
import scenarios
import support


AUDIT = Path(__file__).resolve().parents[2] / 'target' / 'tmp' / 'fjobs'


@contextmanager
def fixture_case():
    AUDIT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='native fixture ', dir=AUDIT) as root:
        with patch.dict(os.environ, FORMAT_JOBS_SOCKET_ROOT=root):
            case = support.Case('oracle', '/bin/sleep', 'owned', root, 10)
        case.handshake = Mock()
        children = []
        foreign = support.identity(os.getpid())

        def creation(*args, **kwargs):
            if args[0] in ('new-session', 'new-window', 'split-window'):
                command = shlex.split(args[-1])
                assert command[0] == 'exec'
                child = subprocess.Popen(['/bin/sh', '-c', args[-1]], stdin=subprocess.DEVNULL,
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                         start_new_session=True)
                # Popen owns this unreaped child; native identity is recorded
                # before either Case.close or the supervisor can signal it.
                marker = shlex.split(args[-1])[-1]
                born = case.until(lambda: support.identity(child.pid)
                                  if support.pid_marker(marker) == child.pid and
                                  support.producer_argv_matches(child.pid, marker) else None,
                                  'fixture child birth and creation argument', 1)
                children.append((child, born))
            elif args[0] == 'display-message':
                return str(foreign['pid'])
            return ''

        case.q = Mock(side_effect=creation)
        try:
            yield case, children, foreign
        finally:
            case.close()
            for child, born in children:
                before = support.identity(child.pid)
                if child.poll() is None:
                    assert before == born, 'native child identity changed before signal'
                    child.terminate()
                rc = child.wait(timeout=2)
                with (AUDIT / 'native-waits.jsonl').open('a') as log:
                    log.write(json.dumps({'pid': child.pid, 'birth': born,
                                          'before_supervisor_cleanup': before,
                                          'original_wait_returncode': rc,
                                          'identity_after_wait': support.identity(child.pid)}) + '\n')


class ProducerPaths(unittest.TestCase):
    def test_all_creation_entries_ignore_foreign_pane_pid(self):
        entries = [('new', ('s',)), ('new_self_identifying', ('t',)),
                   ('new_window', ('s:1',)), ('split_window', ('s:0',))]
        for method, args in entries:
            with self.subTest(method=method), fixture_case() as (case, children, foreign):
                fixture = getattr(case, method)(*args)
                self.assertEqual(len(children), 1)
                self.assertNotEqual(fixture['identity']['pid'], foreign['pid'])
                self.assertEqual(case.producers, [fixture['identity']])
                self.assertTrue(support.marker_matches_identity(fixture['marker'], fixture['identity']))
                self.assertTrue(support.producer_argv_matches(fixture['identity']['pid'], fixture['marker']))
                self.assertIn(fixture['marker'], shlex.split(fixture['creation_argv'][-1]))
                self.assertFalse(any(call.args[0] == 'display-message'
                                     for call in case.q.call_args_list))
                real_kill = os.kill

                def owned_signal(pid, sig):
                    self.assertNotEqual(pid, foreign['pid'])
                    self.assertEqual(support.identity(pid), fixture['identity'])
                    self.assertTrue(support.marker_matches_identity(fixture['marker'], fixture['identity']))
                    real_kill(pid, sig)

                with patch.object(support.os, 'kill', side_effect=owned_signal) as spy:
                    case.close()
                self.assertTrue(spy.called)
                self.assertEqual(support.identity(os.getpid()), foreign)

    def test_foreign_marker_with_same_executable_cannot_register(self):
        for method, args in [('new', ('s',)), ('new_self_identifying', ('s',)),
                             ('new_window', ('s:1',)), ('split_window', ('s:0',))]:
            with self.subTest(method=method), fixture_case() as (case, children, foreign):
                def foreign_marker(*argv, **kwargs):
                    if argv[0] in ('new-session', 'new-window', 'split-window'):
                        Path(shlex.split(argv[-1])[-1]).write_text(str(foreign['pid']))
                    return str(foreign['pid'])
                case.q.side_effect = foreign_marker
                with patch.object(support.os, 'kill') as spy:
                    with self.assertRaises(support.Failure):
                        getattr(case, method)(*args)
                    case.close()
                self.assertEqual(children, [])
                self.assertEqual(case.producers, [])
                self.assertEqual(case._producer_fixtures, [])
                if method in ('new', 'new_self_identifying'):
                    case.handshake.assert_called_once()
                spy.assert_not_called()

    def test_absent_marker_is_required_before_dispatch(self):
        with fixture_case() as (case, children, foreign):
            marker = case.out / 'producer-fixed.pid'
            marker.write_text(str(foreign['pid']))
            with patch.object(support.uuid, 'uuid4', return_value=Mock(hex='fixed')):
                with self.assertRaises(support.Failure):
                    case.new('s')
            case.q.assert_not_called()
            self.assertEqual(case.producers, [])

    def test_cwd_and_dimensions_survive_creation_wrapper(self):
        with fixture_case() as (case, children, foreign):
            fixture = case.new('s', rows=17, cols=43, name='named')
            self.assertEqual(fixture['creation_argv'][:-1],
                             ['new-session', '-d', '-s', 's', '-x', '43', '-y', '17', '-n', 'named'])
            cwd = case.out / 'cwd with spaces'
            cwd.mkdir()
            fixture = case.new_window('s:2', cwd=cwd)
            self.assertEqual(fixture['creation_argv'][:-1],
                             ['new-window', '-d', '-t', 's:2', '-c', str(cwd)])

    def test_command_error_after_creation_keeps_only_proven_cleanup(self):
        for method, args, error in [('new', ('s',), support.Skip),
                                    ('new_window', ('s:1',), support.Failure),
                                    ('split_window', ('s:0',), support.Failure)]:
            with self.subTest(method=method), fixture_case() as (case, children, foreign):
                creation = case.q.side_effect
                def fail_after_creation(*argv, **kwargs):
                    creation(*argv, **kwargs)
                    raise support.Failure('injected creation failure')
                case.q.side_effect = fail_after_creation
                with self.assertRaises(error):
                    getattr(case, method)(*args)
                self.assertEqual(len(case.producers), 1)
                self.assertNotIn(foreign, case.producers)
                case.close()
                self.assertIsNone(support.identity(children[0][0].pid))

    def test_handshake_error_occurs_after_ownership_registration(self):
        with fixture_case() as (case, children, foreign):
            case.handshake.side_effect = support.Failure('injected handshake failure')
            with self.assertRaises(support.Skip):
                case.new('s')
            self.assertEqual(len(case.producers), 1)
            case.close()
            self.assertIsNone(support.identity(children[0][0].pid))

    def test_error_without_marker_never_acquires_cleanup_authority(self):
        with fixture_case() as (case, children, foreign):
            case.q.side_effect = support.Failure('creation failed before any child')
            with patch.object(support.os, 'kill') as spy:
                with self.assertRaises(support.Skip):
                    case.new('s')
                case.close()
            self.assertEqual(case.producers, [])
            case.handshake.assert_called_once()
            spy.assert_not_called()

    def test_registration_rejects_changed_birth_and_executable(self):
        for field in ('path', 'ps_lstart_argv' if support.sys.platform == 'darwin' else 'start_ticks'):
            with self.subTest(field=field), fixture_case() as (case, children, foreign):
                marker = case.out / 'test.pid'
                marker.write_text(str(foreign['pid']))
                changed = dict(foreign, **{field: 'changed'})
                fixture = {'marker': str(marker), 'target': 's', 'executable': foreign['path']}
                with patch.object(support, 'producer_argv_matches', return_value=True), \
                     patch.object(support, 'identity', side_effect=[foreign, changed]):
                    with self.assertRaises(support.Failure):
                        case._register_producer(fixture)
                self.assertEqual(case.producers, [])

    def test_registration_rejects_an_initial_wrong_executable(self):
        with fixture_case() as (case, children, foreign):
            marker = case.out / 'wrong-executable.pid'
            marker.write_text(str(foreign['pid']))
            fixture = {'marker': str(marker), 'target': 's', 'executable': '/different/image'}
            with patch.object(support, 'producer_argv_matches', return_value=True), \
                 patch.object(support.os, 'kill') as spy:
                with self.assertRaises(support.Failure):
                    case._register_producer(fixture)
                case.close()
            self.assertEqual(case.producers, [])
            spy.assert_not_called()

    def test_target_marker_cannot_claim_another_owned_live_producer(self):
        with fixture_case() as (case, children, foreign):
            first, second = case.new('s'), case.new('t')
            self.assertNotEqual(first['identity']['pid'], second['identity']['pid'])
            self.assertFalse(support.marker_matches_identity(first['marker'], second['identity']))
            self.assertFalse(support.producer_argv_matches(second['identity']['pid'], first['marker']))

    def test_close_does_not_trust_unregistered_producer_list_entry(self):
        with fixture_case() as (case, children, foreign):
            case.producers.append(foreign)
            with patch.object(support.os, 'kill') as spy:
                case.close()
            spy.assert_not_called()

    def test_close_escalation_is_limited_to_a_proven_live_producer(self):
        with fixture_case() as (case, children, foreign):
            fixture = case.new('s')
            pid = fixture['identity']['pid']
            with patch.object(support.os, 'kill') as spy:
                case.close()
            self.assertEqual(spy.call_args_list,
                             [unittest.mock.call(pid, signal.SIGTERM),
                              unittest.mock.call(pid, signal.SIGKILL)])
            self.assertNotEqual(pid, foreign['pid'])
            self.assertEqual(support.identity(pid), fixture['identity'])
            self.assertFalse(case.r['cleanup'][-1]['owned_process_gone'])

    def test_close_rechecks_birth_executable_marker_and_argv(self):
        for change in ('birth', 'executable', 'marker', 'argv'):
            with self.subTest(change=change), fixture_case() as (case, children, foreign):
                fixture = case.new('s')
                owned = fixture['identity']
                real_identity = support.identity
                if change in ('birth', 'executable'):
                    field = ('path' if change == 'executable' else
                             'ps_lstart_argv' if support.sys.platform == 'darwin' else 'start_ticks')
                    altered = dict(owned, **{field: 'changed'})
                    guard = patch.object(support, 'identity',
                                         side_effect=lambda pid: altered if pid == owned['pid'] else real_identity(pid))
                elif change == 'marker':
                    Path(fixture['marker']).write_text(str(foreign['pid']))
                    guard = patch.object(support, 'producer_argv_matches', wraps=support.producer_argv_matches)
                else:
                    guard = patch.object(support, 'producer_argv_matches', return_value=False)
                with guard, patch.object(support.os, 'kill') as spy:
                    case.close()
                spy.assert_not_called()
                self.assertEqual(real_identity(owned['pid']), owned)
                # Restore the fixture marker before the context's owned cleanup.
                Path(fixture['marker']).write_text(str(owned['pid']))


class ScenarioProducerRouting(unittest.TestCase):
    def test_setup_and_invalidation_keep_independent_fixture_identities(self):
        with fixture_case() as (case, children, foreign):
            case.wait_clients = Mock()
            with patch.object(scenarios, 'Client', return_value=Mock()):
                scenarios.setup(case)
            self.assertEqual(len(case.producers), 1)

        with fixture_case() as (case, children, foreign):
            fixture = case.new('s')
            client = Mock()
            assertions = case.require
            # Consumer results are stubbed, while the two independent producer
            # identity controls and their original assertions execute unchanged.
            def require(condition, description, **details):
                if description in ('wrong-PID control is a distinct live producer',
                                    'target fixture rejects a coherent wrong live PID attribution'):
                    assertions(condition, description, **details)
            case.require = require
            with patch.object(scenarios, 'setup_self_identifying', return_value=(client, fixture)), \
                 patch.object(scenarios, 'Client', return_value=Mock()), \
                 patch.object(scenarios, 'settle'), patch.object(scenarios, 'display'), \
                 patch.object(scenarios.time, 'sleep'):
                scenarios.invalidation(case)
            self.assertEqual(len(case.producers), 2)
            controls = [item for item in case.r['assertions'] if item['description'] in (
                'wrong-PID control is a distinct live producer',
                'target fixture rejects a coherent wrong live PID attribution')]
            self.assertEqual(len(controls), 2)
            self.assertTrue(all(item['passed'] for item in controls))

    def test_affected_scenarios_use_common_creation_entries(self):
        # Execute the existing scenario bodies with consumer observations stubbed;
        # assertions, formats, targets and populations remain in the source.
        class StopObservation(Exception):
            pass

        class ScenarioCase:
            def __init__(self):
                self.engine, self.r, self.server = 'candidate', {}, None
                self.calls = []
                self.producers = []
            def new(self, session, **kwargs):
                self.calls.append(('new', session))
            def new_window(self, target, cwd=None):
                self.calls.append(('new_window', target))
            def split_window(self, target):
                self.calls.append(('split_window', target))
            def q(self, *args, **kwargs):
                if args[0] in ('new-session', 'new-window', 'split-window'):
                    raise AssertionError('producer bypassed ownership helper')
                if args[0] == 'link-window':
                    raise StopObservation()
                return ''
            def require(self, *args, **kwargs):
                pass
            def until(self, *args, **kwargs):
                return True
            def wait_clients(self, *args):
                pass

        with tempfile.TemporaryDirectory(dir=AUDIT) as root:
            expected = [(product, 'profiles', [('new', 't'), ('new_window', 's:1'), ('new_window', 's:2')]),
                        (product, 'occurrence_stability', [('new_window', 's:1')]),
                        (scenarios, 'loops', [('new', 't'), ('new_window', 's:1'), ('split_window', 's:0')]),
                        (scenarios, 'linked_occurrences', [('new_window', 's:1')])]
            for module, name, wanted in expected:
                case = ScenarioCase()
                case.out = Path(root) / name
                case.out.mkdir()
                client = Mock()
                client.p.pid = 111
                with self.subTest(name=name), \
                     patch.object(module, 'setup', return_value=client), \
                     patch.object(module, 'Client', return_value=client), \
                     patch.object(module, 'display', return_value=['']), \
                     patch.object(module, 'settle'), patch.object(module, 'control'), \
                     patch.object(module.time, 'sleep'):
                    try:
                        getattr(module, name)(case)
                    except StopObservation:
                        pass
                self.assertEqual(case.calls, wanted)
                self.assertEqual(case.producers, [])


if __name__ == '__main__':
    unittest.main()
