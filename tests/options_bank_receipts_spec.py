"""Common-bank receipt regressions; all records are synthetic, never native proof."""
import copy
import datetime
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import options_bank as bank
from options_bank_linux_spec import fixture


def shift(receipt, delta):
    for item in [receipt, *receipt['commands']]:
        for key in ('started', 'ended'):
            item[key] = (datetime.datetime.fromisoformat(item[key]) + delta).isoformat()


class CommonReceiptAcceptance(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        cls.root = Path(cls.directory.name)
        registry, cls.contract = fixture()
        next(row for row in registry['options'] if row['name'] == 'message-limit')['scopes'] = ['server']
        path = cls.root / 'registry.json'
        bank.save(path, registry)
        cls.contract.update(registry=bank.pin(path), native_source='synthetic-head',
                            native_binaries={phase: bank.pin(__file__) for phase in ('oracle', 'candidate')},
                            recipe_sha256=hashlib.sha256(json.dumps(cls.contract['recipe'], sort_keys=True).encode()).hexdigest())
        cls.controls = bank.canaries(path, cls.write_contract(), cls.root)
        cls.cell = cls.contract['native_cell_ids'][0]
        cls.baseline = {}
        for phase in ('oracle', 'candidate'):
            receipt = bank.load(cls.root / 'binding-canaries' / 'valid-synthetic' / (phase + '.json'))
            receipt.update(execution_status='COMPLETE',
                           closure={'status': 'PASS', 'errors': [], 'endpoint_connect_errno': 2})
            cls.baseline[phase] = receipt
        # Establish a valid pair even against the pre-fix, simultaneous canary seed.
        delta = (bank.command_time(cls.baseline['oracle']['ended'])
                 - bank.command_time(cls.baseline['candidate']['started']))
        shift(cls.baseline['candidate'], delta)

    @classmethod
    def write_contract(cls):
        path = cls.root / 'contract.json'
        bank.save(path, cls.contract)
        return path

    def check_pair(self, records):
        pair = {}
        for phase, receipt in records.items():
            path = self.root / (phase + '.json')
            path.write_text(json.dumps(receipt))
            pair[phase] = bank.pin(path)
        return bank.actual_pair(self.cell, pair, self.contract, allow_synthetic=True)

    def test_existing_canaries(self):
        self.assertEqual(self.controls['status'], 'PASS', self.controls)

    def default_fixture(self):
        """Build pinned synthetic defaults for acceptance and mutation regressions."""
        contract = copy.deepcopy(self.contract)
        registry = bank.load(contract['registry']['path'])
        next(row for row in registry['options'] if row['name'] == 'pane-border-lines')['scopes'] = ['window']
        path = self.root / 'default-registry.json'
        bank.save(path, registry)
        contract['registry'] = bank.pin(path)
        contract['default_recipe']['option_ids'] = ['wire:PaneBorderLines']
        contract.update(default_recipe_sha256=hashlib.sha256(json.dumps(
            contract['default_recipe'], sort_keys=True).encode()).hexdigest(),
            default_driver=bank.pin(__file__), python=bank.pin(__file__))
        plan, cells = bank.default_plan(contract)
        cell = next(iter(cells))
        inventory = 'pane-border-lines single\n'
        archived = self.root / 'archived-default.json'
        bank.save(archived, {'binary': contract['native_binaries']['oracle'],
                            'commands': [{'role': 'inventory--gw', 'stdout': inventory}]})
        baseline = self.root / 'default-baseline.json'
        bank.save(baseline, {'receipt': bank.pin(archived), 'options': {
            'pane-border-lines': {'named': inventory, 'value': 'single\n'}}})
        contract['default_oracle_baseline'] = bank.pin(baseline)
        pair = {}
        for phase in ('oracle', 'candidate'):
            receipt = copy.deepcopy(self.baseline[phase])
            root = str(self.root)
            socket = str(self.root / (phase + '.socket'))
            public = ['10', socket, 'bank', '@0', '%0', '11']
            operations = [
                {'role': 'new-session', 'argv': ['new-session', '-d', '-s', 'bank', '-n', 'bank',
                 '-x', '80', '-y', '24', '-c', root, contract['python']['path'], '-I',
                 contract['default_driver']['path'], 'helper', root, contract['default_recipe']['token']]},
                {'role': 'public-inventory', 'argv': ['list-panes', '-a', '-F',
                 '#{pid}|#{socket_path}|#{session_name}|#{window_id}|#{pane_id}|#{pane_pid}']},
                *plan, {'role': 'closure-kill-server', 'argv': ['kill-server']}]
            outputs = ['', '|'.join(public) + '\n', inventory, inventory, 'single\n', '']
            for command, operation, stdout in zip(receipt['commands'], operations, outputs):
                command.update(role=operation['role'], stdout=stdout,
                               argv=[receipt['binary']['path'], '-N', '-S', socket, *operation['argv']],
                               birth={'pid': command['pid'], 'ppid': 99, 'start_ticks': '1', 'state': 'R'},
                               wait_method='Popen.communicate', after=None)
                command['stdout_sha256'] = hashlib.sha256(stdout.encode()).hexdigest()
            receipt.update(recipe_sha256=contract['default_recipe_sha256'], driver=contract['default_driver'],
                           fd_balance={'before': 3, 'after': 3}, attached_client=None, public=public,
                           startup={'sha256': hashlib.sha256(contract['default_recipe']['startup'].encode()).hexdigest()},
                           endpoint={'path': socket, 'device': 1, 'inode': 1},
                           processes={}, cells={cell: [2, 3, 4]})
            receipt['closure'].update(exits={}, started=receipt['commands'][-1]['started'],
                                      ended=receipt['ended'], elapsed_seconds=2)
            for label, pid, ppid in [('daemon', 10, 99), ('helper', 11, 10)]:
                birth = {'pid': pid, 'ppid': ppid, 'start_ticks': '1', 'state': 'R'}
                receipt['processes'][label] = {'identity': birth, 'exe_sha256': bank.pin(__file__)['sha256']}
                receipt['closure']['exits'][label] = {
                    'birth': birth, 'pidfd_exit': True, 'after': None, 'wait_status': 0}
            path = self.root / ('default-' + phase + '.json')
            bank.save(path, receipt)
            pair[phase] = bank.pin(path)
        return contract, pair, cell, inventory

    def test_default_recipe_neighbor_canaries(self):
        contract, pair, cell, inventory = self.default_fixture()
        self.assertEqual(bank.actual_pair(cell, pair, contract, allow_synthetic=True),
                         {'name': 'pane-border-lines', 'named': inventory, 'value': 'single\n'})
        controls = bank.default_canaries(contract, pair, self.root)
        self.assertEqual(controls['status'], 'PASS', [row for row in controls['controls'] if row['status'] != 'PASS'])

    def test_closed_endpoints_and_exact_time_boundary(self):
        for phase in ('oracle', 'candidate'):
            for code in (2, 111):
                records = copy.deepcopy(self.baseline)
                records[phase]['closure']['endpoint_connect_errno'] = code
                with self.subTest(phase=phase, errno=code):
                    self.check_pair(records)

    def test_each_phase_rejects_contradictory_or_incomplete_closure(self):
        mutations = [
            ('failed-execution', lambda r: r.update(execution_status='FAIL')),
            ('missing-execution', lambda r: r.pop('execution_status')),
            ('failed-closure', lambda r: r['closure'].update(status='FAIL')),
            ('missing-status', lambda r: r['closure'].pop('status')),
            ('missing-closure', lambda r: r.pop('closure')),
            ('missing-errors', lambda r: r['closure'].pop('errors')),
            ('missing-errno', lambda r: r['closure'].pop('endpoint_connect_errno')),
            ('review-reproduction', lambda r: r.update(execution_status='FAIL', closure={
                'status': 'PASS', 'errors': ['owned child remains'], 'endpoint_connect_errno': 0})),
        ]
        for value in (None, [], 'PASS'):
            mutations.append(('closure-' + repr(value), lambda r, v=value: r.update(closure=v)))
        for value in (['owned child remains'], None, {}, '', False):
            mutations.append(('errors-' + repr(value), lambda r, v=value: r['closure'].update(errors=v)))
        for value in (0, 13, 61, None, '2', True, False, 2.0, 111.0):
            mutations.append(('errno-' + repr(value), lambda r, v=value: r['closure'].update(endpoint_connect_errno=v)))
        for phase in ('oracle', 'candidate'):
            for name, mutate in mutations:
                records = copy.deepcopy(self.baseline)
                mutate(records[phase])
                with self.subTest(phase=phase, mutation=name), self.assertRaises(bank.GateError):
                    self.check_pair(records)

    def test_candidate_must_start_after_oracle_finishes(self):
        for delta in (datetime.timedelta(days=-1), datetime.timedelta(microseconds=-1)):
            records = copy.deepcopy(self.baseline)
            shift(records['candidate'], delta)
            with self.subTest(delta=delta), self.assertRaisesRegex(bank.GateError, 'products not sequential'):
                self.check_pair(records)

    def test_oracle_end_includes_time_after_last_observation(self):
        records = copy.deepcopy(self.baseline)
        records['oracle']['ended'] = (bank.command_time(records['oracle']['ended'])
                                      + datetime.timedelta(microseconds=1)).isoformat()
        with self.assertRaisesRegex(bank.GateError, 'products not sequential'):
            self.check_pair(records)

    def test_later_candidate_and_timezone_equivalent_boundary(self):
        for delta in (datetime.timedelta(), datetime.timedelta(seconds=1)):
            records = copy.deepcopy(self.baseline)
            shift(records['candidate'], delta)
            zone = datetime.timezone(datetime.timedelta(hours=-5))
            for item in [records['candidate'], *records['candidate']['commands']]:
                for key in ('started', 'ended'):
                    item[key] = bank.command_time(item[key]).astimezone(zone).isoformat()
            with self.subTest(delta=delta):
                self.check_pair(records)


if __name__ == '__main__':
    unittest.main()
