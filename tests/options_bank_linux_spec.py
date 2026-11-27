"""Acceptance of per-option planning; fixtures are not native evidence."""
import copy
import hashlib
import json
import os
from pathlib import Path
import unittest
import tempfile
from unittest import mock

import options_bank
import options_bank_linux as adapter


def fixture():
    contract = json.loads(Path(__file__).with_name('options_bank_contract.json').read_text())
    identities = options_bank.canonical_registry_identities()
    registry = {'schema': 1, 'registry_count': 153,
                'registry_symbol': 'registry::table::OPTIONS', 'target_os': 'linux',
                'oracle': {**contract['oracle_identity'], 'ordinary_names': list(identities)},
                'options': []}
    for name, identity in identities.items():
        registry['options'].append({'name': name, **identity, 'scopes': ['session'],
                                   'default': 'fixture', 'value_type': 'Number',
                                   'consumer': {'status': 'FAIL_MISSING'}})
    return registry, contract


class PerOptionAcceptance(unittest.TestCase):
    def test_default_shell_append_keeps_a_portable_executable(self):
        registry, contract = fixture()
        plan, _ = adapter.per_option_plan(registry, contract, ['default-shell'])
        value = next(row['expected_stdout'].strip() for row in plan
                     if row['role'] == 'default-shell:appended-value')
        self.assertTrue(os.access(value, os.X_OK), value)
        base = next(row['argv'][-1] for row in plan
                    if row['role'] == 'default-shell:append-base')
        self.assertTrue(os.access(base, os.X_OK), base)

    def test_status_interval_restoration_has_a_fresh_first_job(self):
        registry, contract = fixture()
        plan, _ = adapter.per_option_plan(registry, contract, ['status-interval'])
        templates = [row['argv'][-1] for row in plan
                     if row['role'].endswith('-template')]
        # tmux retains each #() job's last output by its command key.
        # Reusing SECOND's key cannot establish a fresh FIRST restoration.
        self.assertEqual(len(templates), 3)
        self.assertEqual(len(set(templates)), 3, templates)
        primes = [row['client_screen'] for row in plan
                  if row['role'].endswith('-prime')]
        self.assertEqual(primes, [[dict(text='FIRST', row=23)]] * 3)
        observers = [row['argv'] for row in plan
                     if row['role'] in ('status-interval:effect',
                                        'status-interval:second-effect',
                                        'status-interval:restored-effect')]
        self.assertEqual(observers, [['display-message', '-p', '']] * 3)
        waits = [row['client_input'] for row in plan
                 if row['role'].endswith('-wait')]
        self.assertEqual(waits, [[dict(hex='', after_ms=1500)]] * 3)

    def test_cleanup_receipt_records_the_final_observed_consumer_exit(self):
        """A helper can be reaped between consecutive /proc observations."""
        for name, state in [('get-clipboard', 'Z'), ('set-titles-string', 'X')]:
            with self.subTest(option=name), tempfile.TemporaryDirectory() as directory:
                run = adapter.Run.__new__(adapter.Run)
                run.root = Path(directory)
                run.socket = run.root / 'socket'
                run.start = adapter.time.monotonic()
                run.contract = {'default_recipe': {'token': 'owned-helper'}}
                run.master = None
                run.record = {}
                birth = dict(pid=101, start_ticks='10', ppid=100, state='S')
                daemon = dict(pid=100, start_ticks='9', ppid=99, state='R')
                run.handles = {'helper': (11, mock.Mock(poll=mock.Mock(return_value=[(11, 1)])), birth),
                               'daemon': (12, mock.Mock(poll=mock.Mock(return_value=[(12, 1)])), daemon)}
                run.children = {'daemon': mock.Mock(pid=100, wait=mock.Mock(return_value=0))}
                run.drain = mock.Mock()
                snapshots = iter([{**birth, 'state': state}, None, None, None])
                def observe(pid):
                    return next(snapshots, None) if pid == birth['pid'] else None
                with mock.patch.object(adapter, 'identity', side_effect=observe), \
                     mock.patch.object(adapter.os, 'waitpid', side_effect=ChildProcessError), \
                     mock.patch.object(adapter.os, 'close'):
                    run.cleanup()
                self.assertEqual(run.record['closure']['status'], 'PASS')
                ended = run.record['closure']['exits']['helper']
                self.assertEqual(ended['birth'], birth)
                self.assertIs(ended['pidfd_exit'], True)
                self.assertIsNone(ended['after'])

    def test_xterm_keys_accepts_window_mutations_without_changing_terminal_input(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, ['xterm-keys'])
        self.assertEqual(cells['xterm-keys']['status'], 'REQUIRED')
        effects = [plan[i] for i in cells['xterm-keys']['obligations']['effect']]
        self.assertEqual([row['argv'][-1] for row in effects
                          if row['argv'][0] == 'set-option'], ['off', 'on', 'off'])
        observers = [row for row in effects if row.get('raw_observation')]
        self.assertEqual(len(observers), 3)
        self.assertTrue(all(row.get('raw_equal_to') == 'xterm-keys:effect'
                            for row in observers[1:]))
        self.assertTrue(all('raw_differ_from' not in row for row in observers))
        self.assertTrue(cells['xterm-keys']['obligations']['sibling'])
        self.assertTrue(cells['xterm-keys']['obligations']['configuration'])
        self.assertIn(['show-options', '-gwv', 'xterm-keys'], [row['argv'] for row in plan])
        commands = []
        for row in plan:
            command = dict(role=row['role'], argv=['/fixture', '-N', '-S', '/private', *row['argv']],
                           exit_code=1 if row['error'] else 0,
                           stdout=row.get('expected_stdout', ''), stderr='')
            if 'source_text' in row:
                command['source_text'] = row['source_text']
            if row.get('raw_observation'):
                command['raw_input'] = '1b5b313b3544'
            for channel in ('stdout', 'stderr'):
                command[channel + '_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
            commands.append(command)
        adapter.per_option_observations(commands, plan, cells)
        for role, field, value in [('second-effect', 'raw_input', '01'),
                                    ('value', 'stdout', 'on\n')]:
            broken = copy.deepcopy(commands)
            command = next(row for row in broken if row['role'] == 'xterm-keys:' + role)
            command[field] = value
            if field == 'stdout':
                command['stdout_sha256'] = hashlib.sha256(value.encode()).hexdigest()
            with self.subTest(role=role), self.assertRaises(RuntimeError):
                adapter.per_option_observations(broken, plan, cells)

    def test_argumentless_client_consumers_use_explicit_end_of_options(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, ['default-client-command'])
        effects = [plan[index]['argv'] for index in cells['default-client-command']['obligations']['effect']
                   if plan[index]['role'] in {'default-client-command:effect',
                                             'default-client-command:second-effect',
                                             'default-client-command:restored-effect'}]
        self.assertEqual(effects, [['--'], ['--'], ['--']])
        self.assertTrue(all(row['argv'] for row in plan))

    def test_exit_wait_probes_keep_exact_planned_commands_and_indices(self):
        from options_bank_linux_windows import planned_commands
        prefix = ['/fixture', '-N', '-S', '/private/socket']
        plan = [dict(role='remain:effect', argv=['list-windows'], wait_pane_exit='bank:1', wait_pane_kept=True)]
        def probe(value, code=0):
            row = dict(role='remain:effect-exit-probe',
                       argv=prefix + ['list-panes', '-t', 'bank:1', '-F', '#{pane_dead}'],
                       stdout=value, stderr='', exit_code=code)
            for channel in ('stdout', 'stderr'):
                row[channel + '_sha256'] = hashlib.sha256(row[channel].encode()).hexdigest()
            return row
        expected = dict(role='remain:effect', argv=prefix + ['list-windows'])
        commands = [{}, {}, probe('0\n'), probe('1\n'), expected, {}]
        observed, indices = planned_commands(commands, plan, prefix)
        self.assertEqual(observed, [expected])
        self.assertEqual(indices, [4])
        for mutation in ('target', 'digest', 'missing', 'unfinished', 'extra'):
            changed = copy.deepcopy(commands)
            if mutation == 'target': changed[2]['argv'][6] = 'bank:9'
            elif mutation == 'digest': changed[2]['stdout_sha256'] = 'unbound'
            elif mutation == 'missing': del changed[2:4]
            elif mutation == 'unfinished': changed[3] = probe('0\n')
            else: changed.insert(-1, probe('1\n'))
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError):
                planned_commands(changed, plan, prefix)

    def test_successful_exit_waits_for_window_removal_under_failed_retention(self):
        from options_bank_linux_windows import pane_exit_observed
        dead = dict(exit_code=0, stdout='1\n')
        removed = dict(exit_code=1, stdout='')
        self.assertTrue(pane_exit_observed(dict(wait_pane_kept=True), dead))
        self.assertFalse(pane_exit_observed(dict(wait_pane_kept=True), removed))
        self.assertFalse(pane_exit_observed(dict(wait_pane_kept=False), dead))
        self.assertTrue(pane_exit_observed(dict(wait_pane_kept=False), removed))

    def test_styled_literal_can_begin_inside_an_overlapping_match(self):
        from options_bank_linux_screen import spans
        self.assertEqual(spans('AAAA', {'text': 'AAA'}), [(0, 3), (1, 4)])

    def test_allow_set_title_controls_osc_title_consumer(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, ['allow-set-title'])
        self.assertEqual(cells['allow-set-title']['status'], 'REQUIRED')
        effects = [plan[index] for index in cells['allow-set-title']['obligations']['effect']]
        titles = [row['expected_stdout'] for row in effects
                  if row['argv'][0] == 'display-message'
                  and row['argv'][-1] == '#{pane_title}']
        self.assertEqual(titles, ['FIRST\n', 'FIRST\n', 'THIRD\n'])
        self.assertTrue(any(row['argv'][0] == 'respawn-pane' for row in effects))

    def test_first_remaining_codepoint_widths_changes_terminal_cursor(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, ['codepoint-widths'])
        self.assertEqual(cells['codepoint-widths']['status'], 'REQUIRED')
        effects = [plan[index] for index in cells['codepoint-widths']['obligations']['effect']]
        cursors = [row['expected_stdout'] for row in effects
                   if row['argv'][0] == 'display-message'
                   and row['argv'][-1] == '#{cursor_x}']
        self.assertEqual(cursors, ['1\n', '2\n', '1\n'])
        self.assertTrue(any(row['argv'][0] == 'new-window' for row in effects))
        commands = []
        for row in plan:
            command = dict(role=row['role'], argv=['/fixture', '-N', '-S', '/private', *row['argv']],
                           exit_code=1 if row['error'] else 0,
                           stdout=row.get('expected_stdout', ''), stderr='')
            if 'source_text' in row:
                command['source_text'] = row['source_text']
            for channel in ('stdout', 'stderr'):
                command[channel + '_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
            commands.append(command)
        adapter.per_option_observations(commands, plan, cells)
        changed = next(row for row in commands if row['role'] == 'codepoint-widths:effect')
        changed['stdout'] = '1\n'
        changed['stdout_sha256'] = hashlib.sha256(changed['stdout'].encode()).hexdigest()
        with self.assertRaisesRegex(RuntimeError, 'consumer'):
            adapter.per_option_observations(commands, plan, cells)

    def test_frozen_full_153_option_acceptance(self):
        """Every ordinary option needs mutations and a downstream consumer."""
        registry, contract = fixture()
        names = list(options_bank.canonical_registry_identities())
        self.assertEqual(len(names), 153)
        plan, cells = adapter.per_option_plan(registry, contract, names)
        self.assertEqual(set(cells), set(names))
        missing = [name for name in names if cells[name]['status'] != 'REQUIRED']
        self.assertEqual(missing, [], 'Missing ordinary option consumers')
        required = {'default', 'replace', 'effect', 'scope', 'unset',
                    'only-if-unset', 'quiet', 'format'}
        for name in names:
            with self.subTest(option=name):
                self.assertTrue(required <= set(cells[name]['obligations']))
                for obligation in required:
                    self.assertTrue(cells[name]['obligations'][obligation])
                effects = [plan[i] for i in cells[name]['obligations']['effect']]
                self.assertTrue(any(row['argv'][0] not in
                                    ('set-option', 'show-options', 'set-window-option',
                                     'show-window-options', 'source-file') for row in effects),
                                'A stored value is not a downstream effect')
        for command, flags in [('set-option', 'aFgopqsuUw'),
                               ('show-options', 'AgHpqsvw')]:
            observed = {flag for row in plan if row['argv'][0] == command
                        for arg in row['argv'][1:] if arg.startswith('-')
                        for flag in arg[1:]}
            self.assertTrue(set(flags) <= observed,
                            f'{command}: missing manual flags {set(flags) - observed}')

    def test_neighbor_server_options_require_buffer_eviction_and_alias_execution(self):
        registry, contract = fixture()
        names = ['buffer-limit', 'command-alias']
        plan, cells = adapter.per_option_plan(registry, contract, names)
        for name in names:
            self.assertEqual(cells[name]['status'], 'REQUIRED')
            self.assertTrue(cells[name]['obligations']['effect'])
            self.assertTrue(cells[name]['obligations']['sibling'])
            self.assertTrue(cells[name]['obligations']['configuration'])
        commands = [row['argv'] for row in plan]
        self.assertIn(['set-buffer', 'third'], commands)
        self.assertIn(['list-buffers', '-F', '#{buffer_name}|#{buffer_sample}'], commands)
        self.assertIn(['bank-alias'], commands)
        observed = []
        for row in plan:
            command = {'role': row['role'], 'argv': ['/fixture', '-N', '-S', '/private', *row['argv']],
                       'exit_code': 1 if row['error'] else 0,
                       'stdout': row.get('expected_stdout', ''), 'stderr': ''}
            for channel in ('stdout', 'stderr'):
                command[channel + '_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
            observed.append(command)
        adapter.per_option_observations(observed, plan, cells)
        for name in names:
            broken = copy.deepcopy(observed)
            effect = next(row for row in broken if row['role'] == name + ':effect')
            effect['stdout'] = 'option stored, consumer unchanged\n'
            effect['stdout_sha256'] = hashlib.sha256(effect['stdout'].encode()).hexdigest()
            with self.assertRaisesRegex(RuntimeError, 'consumer'):
                adapter.per_option_observations(broken, plan, cells)

    def test_message_limit_accepts_bound_observer_and_rejects_unrelated_echo(self):
        registry, contract = fixture()
        next(entry for entry in registry['options'] if entry['name'] == 'message-limit')['scopes'] = ['server']
        audit = Path(__file__).resolve().parent.parent / 'target' / 'tmp'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as folder:
            root = Path(folder)
            registry_path = root / 'registry.json'
            options_bank.save(registry_path, registry)
            contract.update(registry=options_bank.pin(registry_path), native_source='fixture-head',
                            native_binaries={phase: options_bank.pin(__file__)
                                             for phase in ('oracle', 'candidate')},
                            recipe_sha256=hashlib.sha256(json.dumps(contract['recipe'], sort_keys=True).encode()).hexdigest())
            controls = options_bank.binding_canaries(registry, contract, root)
            pair = {}
            cell = contract['native_cell_ids'][0]
            for phase in ('oracle', 'candidate'):
                receipt = options_bank.load(root / 'binding-canaries' / 'valid-synthetic' / (phase + '.json'))
                observer = receipt['commands'][-1]
                observer['stdout'] = f"12:34: client-{observer['pid']} command: show-messages\n"
                observer['stdout_sha256'] = hashlib.sha256(observer['stdout'].encode()).hexdigest()
                path = root / (phase + '.json')
                options_bank.save(path, receipt)
                pair[phase] = options_bank.pin(path)
            options_bank.actual_pair(cell, pair, contract, allow_synthetic=True)
            self.assertEqual(controls['status'], 'PASS')
            for text in ('12:34: client-999 command: show-messages\n',
                         '12:34: client command: show-messages\n'):
                receipt['commands'][-1]['stdout'] = text
                receipt['commands'][-1]['stdout_sha256'] = hashlib.sha256(text.encode()).hexdigest()
                path.write_text(json.dumps(receipt))
                pair['candidate'] = options_bank.pin(path)
                with self.subTest(text=text), self.assertRaises(options_bank.GateError):
                    options_bank.actual_pair(cell, pair, contract, allow_synthetic=True)
        plan, cells = adapter.per_option_plan(registry, contract, ['message-limit'])
        self.assertEqual(cells['message-limit']['status'], 'REQUIRED')
        self.assertIn(['set-option', '-s', 'message-limit', '1'], [row['argv'] for row in plan])
        self.assertIn(['show-options', '-sv', 'message-limit'], [row['argv'] for row in plan])
        commands = []
        for index, row in enumerate(plan):
            text = row.get('expected_stdout', '')
            if row['role'] == 'inventory-client':
                text = '42|/dev/pts/999\n'
            if row['role'] == 'message-limit:before':
                text = '12:34: ' + contract['recipe']['markers']['first'] + '\n12:34: ' + contract['recipe']['markers']['last'] + '\n'
            if row['role'] in ('message-limit:after', 'message-limit:append-after'):
                text = f'23:59: client-{200+index} command: show-messages\n'
            argv = ['/fixture', '-N', '-S', '/private/socket',
                    *(arg if arg != '$client' else '/dev/pts/999' for arg in row['argv'])]
            command = {'role': row['role'], 'argv': argv, 'pid': 200+index,
                       'exit_code': 0, 'stdout': text, 'stderr': ''}
            for channel in ('stdout', 'stderr'):
                command[channel+'_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
            commands.append(command)
        adapter.per_option_observations(commands, plan, cells)
        observer = next(command for command in commands if command['role'] == 'message-limit:after')
        observer['stdout'] = '23:59: client-999 command: show-messages\n'
        observer['stdout_sha256'] = hashlib.sha256(observer['stdout'].encode()).hexdigest()
        with self.assertRaisesRegex(options_bank.GateError, 'bound observer'):
            adapter.per_option_observations(commands, plan, cells)


    def test_pane_border_status_requires_a_native_geometry_consumer(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, ['pane-border-status'])
        self.assertEqual(cells['pane-border-status']['status'], 'REQUIRED')
        effects = [plan[index] for index in cells['pane-border-status']['obligations']['effect']]
        sizes = [row['expected_pty_size'] for row in effects if 'expected_pty_size' in row]
        self.assertIn([79, 23], sizes)
        self.assertTrue(any(row['expected_stdout'].startswith('0|1|79|23|') for row in effects))
        self.assertTrue(any(row['expected_stdout'].startswith('0|0|79|23|') for row in effects))
        self.assertEqual(set(cells['pane-border-status']['obligations']),
                         {'default', 'replace', 'effect', 'scope', 'only-if-unset', 'quiet', 'unset', 'format', 'configuration'})

    def test_first_missing_backspace_option_requires_real_input_consumer(self):
        registry, contract = fixture()
        first = next(entry['name'] for entry in registry['options']
                     if entry['name'] not in adapter.PER_OPTION_EFFECTS)
        self.assertEqual(first, 'backspace')
        plan, cells = adapter.per_option_plan(registry, contract, ['backspace'])
        self.assertEqual(cells['backspace']['status'], 'REQUIRED')
        commands = [row['argv'] for row in plan]
        self.assertIn(['send-keys', '-t', 'bank:bank', 'BSpace'], commands)
        self.assertIn(['capture-pane', '-p', '-t', 'bank:bank'], commands)
        self.assertTrue(cells['backspace']['obligations']['effect'])
        observed = []
        for row in plan:
            command = {'role': row['role'], 'argv': ['/fixture', '-N', '-S', '/private', *row['argv']],
                       'exit_code': 1 if row['error'] else 0,
                       'stdout': row.get('expected_stdout', ''), 'stderr': ''}
            for channel in ('stdout', 'stderr'):
                command[channel + '_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
            observed.append(command)
        adapter.per_option_observations(observed, plan, cells)
        effect = next(command for command in observed if command['role'] == 'backspace:effect')
        self.assertEqual(effect['stdout'], '7f\n08\n' + '\n' * 22)
        effect['stdout'] = '7f\n7f\n' + '\n' * 22
        effect['stdout_sha256'] = hashlib.sha256(effect['stdout'].encode()).hexdigest()
        with self.assertRaisesRegex(RuntimeError, 'consumer'):
            adapter.per_option_observations(observed, plan, cells)

    def test_default_command_and_shell_have_real_launch_consumers(self):
        registry, contract = fixture()
        names = ['default-command', 'default-shell']
        plan, cells = adapter.per_option_plan(registry, contract, names)
        for name in names:
            with self.subTest(option=name):
                self.assertEqual(cells[name]['status'], 'REQUIRED')
                roles = {row['role']: row for row in plan}
                self.assertIn(name + ':effect', roles)
                self.assertIn(name + ':reset-effect', roles)
                self.assertIn(name + ':source-effect', roles)
                self.assertIn('append', cells[name]['obligations'])
                self.assertIn('source', cells[name]['obligations'])
                self.assertEqual(roles[name + ':effect']['argv'][-1], '#{pane_pid}')
                self.assertIn('consumer_argv', roles[name + ':create-consumer'])

    def test_owned_children_include_worker_threads(self):
        audit = Path(__file__).resolve().parent.parent / 'target' / 'tmp'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as folder:
            proc = Path(folder)
            for tid, children in [('100', '101'), ('103', '104 105')]:
                task = proc / '100' / 'task' / tid
                task.mkdir(parents=True)
                (task / 'children').write_text(children)
            self.assertEqual(adapter.children(100, proc), [101, 104, 105])

    def test_mode_plans_mutation_and_real_consumer_separately_from_defaults(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, ['base-index'])
        self.assertEqual(set(cells), {'base-index'})
        commands = [row['argv'] for row in plan]
        self.assertIn(['set-option', '-g', 'base-index', '7'], commands)
        self.assertIn(['new-window', '-d', '-t', 'bank:', '-n', 'effect', '/bin/sleep', '240'], commands)
        self.assertIn(['list-windows', '-t', 'bank', '-F', '#{window_name}|#{window_index}'], commands)
        self.assertEqual(set(cells['base-index']['obligations']),
                         {'default', 'replace', 'effect', 'unset', 'scope', 'only-if-unset', 'quiet', 'format'})
        self.assertTrue(all('indices' in cells[name] for name in cells))

    def test_omitted_and_renamed_options_cannot_enter_the_mode(self):
        for mutation in ('omitted', 'renamed'):
            registry, contract = fixture()
            if mutation == 'omitted':
                registry['options'].pop()
                registry['registry_count'] -= 1
                registry['oracle']['ordinary_names'].pop()
            else:
                entry = registry['options'][0]
                registry['oracle']['ordinary_names'].remove(entry['name'])
                entry['name'] = 'renamed-option'
                registry['oracle']['ordinary_names'].append(entry['name'])
            with self.subTest(mutation=mutation), self.assertRaises(options_bank.GateError):
                adapter.per_option_plan(registry, contract, ['base-index'])

    def test_value_seven_without_real_window_index_change_is_rejected(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, ['base-index'])
        commands = []
        for row in plan:
            stdout = row.get('expected_stdout', '')
            if 'expected_lines' in row:
                stdout = ''.join(line + '\n' for line in row['expected_lines'])
            commands.append({'role': row['role'], 'argv': ['/fixture', '-N', '-S', '/private/socket', *row['argv']],
                             'exit_code': 1 if row['error'] else 0, 'stdout': stdout, 'stderr': ''})
        def hash_outputs():
            for command in commands:
                for channel in ('stdout', 'stderr'):
                    command[channel + '_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
        hash_outputs()
        adapter.per_option_observations(commands, plan, cells)
        effect = next(command for command in commands if command['role'] == 'base-index:effect')
        effect['stdout'] = 'bank|0\neffect|1\n'
        hash_outputs()
        with self.assertRaisesRegex(RuntimeError, 'real consumer'):
            adapter.per_option_observations(commands, plan, cells)

    def test_unsupported_consumers_remain_open_and_message_limit_is_required(self):
        registry, contract = fixture()
        import options_bank_linux_keys as keys
        # Absence is an explicit fixture property now that mouse has a recipe.
        with mock.patch.object(keys, 'OPTIONS', keys.OPTIONS - {'mouse'}):
            _, cells = adapter.per_option_plan(registry, contract, ['mouse', 'message-limit'])
        self.assertEqual(cells['mouse']['status'], 'FAIL_MISSING')
        self.assertEqual(cells['message-limit']['status'], 'REQUIRED')
        self.assertFalse(cells['mouse']['indices'])
        self.assertTrue(cells['message-limit']['indices'])

    def test_independent_152_consumer_remainder(self):
        registry, contract = fixture()
        names = [entry['name'] for entry in registry['options'] if entry['name'] != 'xterm-keys']
        self.assertEqual(len(names), 152)
        plan, cells = adapter.per_option_plan(registry, contract, names)
        required = {'default', 'replace', 'effect', 'scope', 'unset', 'only-if-unset', 'quiet', 'format'}
        for name in names:
            with self.subTest(option=name):
                self.assertEqual(cells[name]['status'], 'REQUIRED')
                self.assertTrue(required <= set(cells[name]['obligations']))
                self.assertTrue(all(cells[name]['obligations'][key] for key in required))
                effects = [plan[index] for index in cells[name]['obligations']['effect']]
                if name == 'default-client-command':
                    self.assertEqual([row['expected_stdout'] for row in effects if row['argv'] == ['--']],
                                     ['FIRST\n', 'SECOND\n', 'FIRST\n'])
                else:
                    self.assertTrue(any(row['argv'] and row['argv'][0] not in
                                        ('set-option', 'show-options', 'set-window-option',
                                         'show-window-options', 'source-file') for row in effects))
        for command, flags in [('set-option', 'aFgopqsuUw'), ('show-options', 'AgHpqsvw')]:
            observed = {flag for row in plan if row['argv'] and row['argv'][0] == command
                        for arg in row['argv'][1:] if arg.startswith('-') for flag in arg[1:]}
            self.assertTrue(set(flags) <= observed)

    def test_only_if_unset_accepts_measured_oracle_rejection_and_keeps_value(self):
        registry, contract = fixture()
        plan, _ = adapter.per_option_plan(registry, contract, ['base-index'])
        selected = [row for row in plan if row['role'] in ('base-index:only-existing', 'base-index:only-existing-value')]
        commands = [
            {'role': 'base-index:only-existing', 'argv': ['/fixture', '-N', '-S', '/private', 'set-option', '-o', '-t', 'bank', 'base-index', '9'],
             'exit_code': 1, 'stdout': '', 'stderr': 'already set: base-index\n'},
            {'role': 'base-index:only-existing-value', 'argv': ['/fixture', '-N', '-S', '/private', 'show-options', '-v', '-t', 'bank', 'base-index'],
             'exit_code': 0, 'stdout': '3\n', 'stderr': ''}]
        for command in commands:
            for channel in ('stdout', 'stderr'):
                command[channel+'_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
        adapter.per_option_observations(commands, selected, {'base-index': {'indices': [0, 1], 'status': 'REQUIRED'}})

    def test_three_recipes_and_duplicate_or_unknown_selection(self):
        registry, contract = fixture()
        plan, cells = adapter.per_option_plan(registry, contract, list(adapter.PER_OPTION_EFFECTS))
        self.assertEqual(len(cells), 3)
        self.assertEqual(sum(row['role'].endswith(':create-consumer') for row in plan), 3)
        for names in ([], ['base-index', 'base-index'], ['pane-base-index-renamed']):
            with self.subTest(names=names), self.assertRaises(RuntimeError):
                adapter.per_option_plan(registry, contract, names)

    def test_process_state_change_does_not_change_retained_birth(self):
        registry, contract = fixture()
        next(entry for entry in registry['options'] if entry['name'] == 'base-index')['default'] = {'value': '0'}
        contract.update(registry={'path': '/fixture/registry'}, native_source='fixture-head',
                        default_driver={'path': '/fixture/driver'}, python={'path': '/fixture/python', 'sha256': 'python'},
                        native_binaries={'oracle': {'path': '/fixture/tmux', 'sha256': 'oracle'}}, default_recipe_sha256='recipe',
                        default_recipe={'kind': 'per-option-v1', 'startup': options_bank.REGISTRY_STARTUP_V1,
                                        'environment': {'SHELL': '/bin/bash'}, 'names': ['base-index'], 'token': 'token'})
        plan, cells = adapter.per_option_plan(registry, contract, ['base-index'])
        root, socket = '/fixture/private', '/fixture/private/socket'
        prefix = ['/fixture/tmux', '-N', '-S', socket]
        public = ['100', socket, 'bank', '@0', '%0', '101']
        setup = [
            {'role': 'new-session', 'argv': ['new-session', '-d', '-s', 'bank', '-n', 'bank', '-x', '80', '-y', '24',
                                           '-c', root, '/fixture/python', '-I', '/fixture/driver', 'helper', root, 'token']},
            {'role': 'public-inventory', 'argv': ['list-panes', '-a', '-F', '#{pid}|#{socket_path}|#{session_name}|#{window_id}|#{pane_id}|#{pane_pid}']}]
        commands = []
        for row in [*setup, *plan, {'role': 'closure-kill-server', 'argv': ['kill-server']}]:
            stdout = row.get('expected_stdout', '')
            if 'expected_lines' in row:
                stdout = ''.join(line+'\n' for line in row['expected_lines'])
            if row['role'] == 'public-inventory': stdout = '|'.join(public)+'\n'
            if row['role'] == 'inventory--g': stdout = ''.join(entry['name']+'\n' for entry in registry['options'])
            if row['role'] == 'base-index:default': stdout = '0\n'
            command = {'role': row['role'], 'argv': prefix+row['argv'], 'stdout': stdout, 'stderr': '',
                       'pid': 200+len(commands), 'exit_code': 1 if row.get('error') else 0,
                       'started': '2000-01-01T00:00:00+00:00', 'ended': '2000-01-01T00:00:00+00:00',
                       'birth': {'pid': 200+len(commands), 'start_ticks': '10', 'ppid': 99, 'state': 'R'},
                       'after': None, 'wait_method': 'Popen.communicate'}
            for channel in ('stdout', 'stderr'):
                command[channel+'_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
            commands.append(command)
        processes, exits = {}, {}
        for label, pid, image in [('daemon', 100, 'oracle'), ('helper', 101, 'python'), ('consumer-base-index', 102, 'sleep')]:
            birth = {'pid': pid, 'start_ticks': '10', 'ppid': 99 if label == 'daemon' else 100, 'state': 'S'}
            processes[label] = {'identity': birth, 'exe_sha256': image}
            exits[label] = {'birth': {**birth, 'state': 'R'}, 'pidfd_exit': True, 'after': None, 'wait_status': 0}
        receipt = {'phase': 'oracle', 'native_os': 'linux', 'source': 'fixture-head', 'binary': contract['native_binaries']['oracle'],
                   'driver': contract['default_driver'], 'recipe_sha256': 'recipe', 'execution_status': 'COMPLETE',
                   'closure': {'status': 'PASS', 'errors': [], 'endpoint_connect_errno': 2, 'exits': exits},
                   'fd_balance': {'before': {}, 'after': {}}, 'processes': processes, 'public': public,
                   'endpoint': {'path': socket}, 'startup': {'sha256': hashlib.sha256(options_bank.REGISTRY_STARTUP_V1.encode()).hexdigest()},
                   'environment': {'HOME': root+'/home', 'XDG_CONFIG_HOME': root+'/xdg', 'PATH': '/usr/bin:/bin', 'SHELL': '/bin/bash',
                                   'TERM': 'xterm', 'LANG': 'C.UTF-8', 'RMUX_DISABLE_TMUX_SHIM': '1', 'RMUX_DISABLE_TMUX_FALLBACK': '1'},
                   'commands': commands, 'started': commands[0]['started'], 'ended': commands[-1]['ended'],
                   'cells': {name: [index+2 for index in cell['indices']] for name, cell in cells.items()}}
        with mock.patch.object(adapter, 'load', return_value=registry), mock.patch.object(adapter, 'pin', return_value={'sha256': 'sleep'}):
            adapter.per_option_phase(receipt, contract, 'oracle')
            receipt['closure']['exits']['helper']['birth']['start_ticks'] = '11'
            with self.assertRaisesRegex(RuntimeError, 'birth/exit'):
                adapter.per_option_phase(receipt, contract, 'oracle')


if __name__ == '__main__':
    unittest.main()
