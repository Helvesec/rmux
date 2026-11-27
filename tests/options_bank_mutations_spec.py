"""In-memory counterproofs of common-bank canaries; never native evidence."""
import ast
import datetime
from pathlib import Path
import unittest

import options_bank as bank
import options_bank_receipts_spec as receipts


PHASES = ('oracle', 'candidate')
ZONES = (-12, 0, 14)
OFFSETS = (-86400000000, -1, 0, 1, 1000000)


def binding_groups():
    groups = {}
    for phase in PHASES:
        groups[phase + '-execution'] = {
            phase + '-failed-execution-with-PASS-closure', phase + '-missing-execution_status'}
        groups[phase + '-errors'] = {phase + '-missing-errors', *(
            phase + '-PASS-closure-errors-' + str(i) for i in range(5))}
        groups[phase + '-endpoint'] = {phase + '-missing-endpoint_connect_errno', *(
            phase + '-PASS-closure-errno-' + str(i) for i in range(8))}
    groups['order'] = {'candidate-before-oracle', 'candidate-overlaps-oracle-by-one-microsecond',
                       'oracle-closes-after-candidate-starts'}
    return groups


def boundary_ids(offsets):
    return {f'zone-{oz}-{cz}-delta-{micros}' for oz in ZONES for cz in ZONES for micros in offsets}


def differences(a, b, path=''):
    if type(a) is not type(b):
        return {path}
    if isinstance(a, dict):
        return set().union(*(differences(a[key], b[key], path + '/' + key)
                             if key in a and key in b else {path + '/' + key}
                             for key in a.keys() | b.keys()))
    if isinstance(a, list):
        if len(a) != len(b):
            return {path}
        return set().union(*(differences(x, y, path + '/' + str(i))
                             for i, (x, y) in enumerate(zip(a, b))))
    return set() if a == b else {path}


class CanaryCounterproofs(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = Path(bank.__file__)
        cls.source_bytes = cls.source.read_bytes()
        cls.addClassCleanup(cls.check_source_unchanged)
        # Reuse the acceptance fixtures, not archived paths or native binaries.
        receipts.CommonReceiptAcceptance.setUpClass()
        cls.addClassCleanup(receipts.CommonReceiptAcceptance.doClassCleanups)
        fixture = receipts.CommonReceiptAcceptance()
        cls.root = fixture.root
        cls.contract = fixture.contract
        cls.default_contract, cls.default_pair, _, _ = fixture.default_fixture()
        cls.binding_result = fixture.controls
        cls.binding_rows = cls.rows(cls.binding_result, cls.root)
        cls.default_result = bank.default_canaries(cls.default_contract, cls.default_pair, cls.root)
        cls.default_rows = cls.rows(cls.default_result, cls.root)

    @classmethod
    def check_source_unchanged(cls):
        if cls.source.read_bytes() != cls.source_bytes:
            raise AssertionError('mutation tests changed options_bank.py on disk')

    @classmethod
    def rows(cls, result, output):
        rows = result['controls'] + result.get('focused', {}).get('controls', [])
        # Runs own their receipts but share pinned registry/contract artifacts.
        indexed = {row['id']: {**row, 'reason': row['reason'].replace(str(output), '$RUN')
                              .replace(str(cls.root), '$RUN')}
                   for row in rows}
        if len(indexed) != len(rows):
            raise AssertionError('duplicate canary IDs')
        return indexed

    def variant(self, function, marker, expression):
        """Replace exactly one guard in an isolated module namespace."""
        tree = ast.parse(self.source_bytes, filename=str(self.source))
        count = 0
        for definition in tree.body:
            if isinstance(definition, ast.FunctionDef) and definition.name == function:
                for node in ast.walk(definition):
                    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                            and node.func.id == 'require' and len(node.args) > 1
                            and marker in ast.unparse(node.args[1])):
                        original = ast.unparse(node.args[0])
                        node.args[0] = ast.parse(expression.replace('ORIGINAL', '(' + original + ')'),
                                                mode='eval').body
                        count += 1
        self.assertEqual(count, 1, (function, marker))
        namespace = {'__name__': 'memory_canary_mutant', '__file__': str(self.source)}
        exec(compile(ast.fix_missing_locations(tree), str(self.source) + ':memory-mutant', 'exec'), namespace)
        return namespace

    def run_variant(self, name, namespace, *, default=False):
        output = self.root / name
        output.mkdir()
        if default:
            result = namespace['default_canaries'](self.default_contract, self.default_pair, output)
        else:
            result = namespace['canaries'](self.root / 'registry.json', self.root / 'contract.json', output)
        observed = self.rows(result, output)
        baseline = self.default_rows if default else self.binding_rows
        self.assertEqual(observed.keys(), baseline.keys(), name)
        self.assertEqual(result['status'], 'FAIL', name)
        return observed

    def assert_exact_escape(self, name, observed, expected, *, default=False):
        baseline = self.default_rows if default else self.binding_rows
        escaped = {key for key, row in observed.items()
                   if row.get('expected_gate') == 'FAIL' and row['observed_gate'] == 'PASS'}
        changed = {key for key in observed if observed[key] != baseline[key]}
        self.assertEqual(escaped, expected, name)
        self.assertEqual(changed, expected, name)
        self.assertTrue(all(observed[key]['status'] == 'FAIL' for key in expected))

    def test_baselines_and_binding_fixture_isolation(self):
        self.assertEqual(self.binding_result['status'], 'PASS', self.binding_result)
        self.assertEqual(self.default_result['status'], 'PASS', self.default_result)
        negative = set().union(*binding_groups().values())
        positive = {f'{p}-known-closed-endpoint-{code}' for p in PHASES for code in (2, 111)}
        positive |= {f'candidate-at-oracle-end-zone-{zone}' for zone in ZONES}
        positive.add('candidate-one-microsecond-after-oracle')
        self.assertEqual((len(negative), len(positive)), (37, 8))
        seeds = {p: bank.load(self.root / 'binding-canaries' / 'valid-synthetic' / (p + '.json'))
                 for p in PHASES}
        cell = self.contract['native_cell_ids'][0]
        option, operations = bank.bound_recipe(cell, self.contract)
        for name in sorted(negative | positive):
            with self.subTest(canary=name):
                row = self.binding_rows[name]
                self.assertEqual(row['expected_gate'], 'FAIL' if name in negative else 'PASS')
                records = {p: bank.load(self.root / 'binding-canaries' / name / (p + '.json')) for p in PHASES}
                delta = {p: differences(seeds[p], records[p]) for p in PHASES}
                phase = 'oracle' if name.startswith('oracle-') else 'candidate'
                self.assertFalse(delta['candidate' if phase == 'oracle' else 'oracle'])
                self.assertEqual(sum(bool(paths) for paths in delta.values()),
                                 0 if name.endswith('-known-closed-endpoint-2') else 1)
                for p in PHASES:
                    bank.bound_commands(records[p], records[p]['cells'][cell], option, operations, self.contract)
                gap = bank.command_time(records['candidate']['started']) - bank.command_time(records['oracle']['ended'])
                if name in binding_groups()['order']:
                    self.assertLess(gap, datetime.timedelta())
                elif 'candidate-at-oracle-end-zone-' in name:
                    self.assertEqual(gap, datetime.timedelta())
                elif name == 'candidate-one-microsecond-after-oracle':
                    self.assertEqual(gap, datetime.timedelta(microseconds=1))
                else:
                    self.assertLessEqual(delta[phase], {'/execution_status', '/closure/errors',
                                                       '/closure/endpoint_connect_errno'})

    def test_binding_weakened_guards_escape_exact_groups(self):
        for name, expected in binding_groups().items():
            with self.subTest(mutant=name):
                if name == 'order':
                    ns = self.variant('actual_pair', 'products not sequential', 'True')
                else:
                    phase, key = name.split('-')
                    marker = {'execution': 'unclosed/incomplete', 'errors': 'closure errors',
                              'endpoint': 'endpoint not closed'}[key]
                    weakened = "isinstance(closure, dict) and closure.get('status') == 'PASS'" if key == 'execution' else 'True'
                    ns = self.variant('linux_closure', marker,
                                      f"({weakened}) if receipt['phase'] == {phase!r} else ORIGINAL")
                rows = self.run_variant('binding-remove-' + name, ns)
                self.assert_exact_escape(name, rows, expected)

    def test_binding_valid_endpoint_witnesses_reject_stricter_mutants(self):
        for phase in PHASES:
            for forbidden in (2, 111):
                with self.subTest(phase=phase, forbidden=forbidden):
                    allowed = 111 if forbidden == 2 else 2
                    ns = self.variant('linux_closure', 'endpoint not closed',
                                      f"(type(closure.get('endpoint_connect_errno')) is int and "
                                      f"closure['endpoint_connect_errno'] == {allowed}) "
                                      f"if receipt['phase'] == {phase!r} else ORIGINAL")
                    rows = self.run_variant(f'binding-reject-{phase}-{forbidden}', ns)
                    row = rows[f'{phase}-known-closed-endpoint-{forbidden}']
                    self.assertEqual((row['status'], row['observed_gate'], row['reason']),
                                     ('FAIL', 'FAIL', 'endpoint not closed'))

    def test_binding_valid_boundaries_reject_stricter_mutants(self):
        equal = {f'candidate-at-oracle-end-zone-{zone}' for zone in ZONES}
        for name, expression, expected in [
            ('equal', "command_time(receipts['oracle']['ended']) < command_time(receipts['candidate']['started'])", equal),
            ('microsecond', "command_time(receipts['oracle']['ended']) + datetime.timedelta(microseconds=2) <= command_time(receipts['candidate']['started'])",
             equal | {'candidate-one-microsecond-after-oracle'}),
        ]:
            with self.subTest(mutant=name):
                rows = self.run_variant('binding-reject-' + name,
                                        self.variant('actual_pair', 'products not sequential', expression))
                self.assertEqual({key for key in rows if rows[key] != self.binding_rows[key]}, expected)
                self.assertTrue(all(rows[key]['status'] == 'FAIL'
                                    and rows[key]['reason'] == 'products not sequential' for key in expected))

    def test_default_boundaries_preserve_local_chronology(self):
        self.assertEqual(len(boundary_ids(OFFSETS)), 45)
        for oz in ZONES:
            for cz in ZONES:
                for micros in OFFSETS:
                    name = f'zone-{oz}-{cz}-delta-{micros}'
                    with self.subTest(canary=name):
                        row = self.default_rows[name]
                        self.assertEqual(row['expected_gate'], 'FAIL' if micros < 0 else 'PASS')
                        records = {p: bank.load(self.root / 'default-canaries' / name / (p + '.json')) for p in PHASES}
                        for phase, hours in [('oracle', oz), ('candidate', cz)]:
                            record = records[phase]
                            bank.default_phase(record, self.default_contract, phase, allow_synthetic=True)
                            for item in [record, *record['commands'], record['closure']]:
                                for key in ('started', 'ended'):
                                    self.assertEqual(bank.command_time(item[key]).utcoffset(), datetime.timedelta(hours=hours))
                        gap = bank.command_time(records['candidate']['started']) - bank.command_time(records['oracle']['ended'])
                        self.assertEqual(gap, datetime.timedelta(microseconds=micros))
                        pair = {p: bank.pin(self.root / 'default-canaries' / name / (p + '.json')) for p in PHASES}
                        cell = next(iter(records['oracle']['cells']))
                        if micros < 0:
                            with self.assertRaisesRegex(bank.GateError, '^default products not sequential$'):
                                bank.actual_pair(cell, pair, self.default_contract, allow_synthetic=True)
                        else:
                            bank.actual_pair(cell, pair, self.default_contract, allow_synthetic=True)

    def test_default_completion_canaries_change_only_the_selected_phase(self):
        seeds = {p: bank.load(self.default_pair[p]['path']) for p in PHASES}
        for phase in PHASES:
            for kind in ('failed', 'missing'):
                name = f'{phase}-{kind}-execution'
                with self.subTest(canary=name):
                    row = self.default_rows[name]
                    self.assertEqual((row['expected_gate'], row['observed_gate'], row['reason']),
                                     ('FAIL', 'FAIL', 'unclosed/incomplete default product'))
                    for p in PHASES:
                        record = bank.load(self.root / 'default-canaries' / name / (p + '.json'))
                        self.assertEqual(differences(seeds[p], record), {'/execution_status'} if p == phase else set())

    def test_default_weakened_guards_escape_exact_groups(self):
        groups = {
            'execution': ('unclosed/incomplete', {'incomplete-closure', *(
                f'{phase}-{kind}-execution' for phase in PHASES for kind in ('failed', 'missing'))}),
            'errors': ('closure errors', {'closure-errors-with-PASS-label', 'missing-closure-errors', *(
                f'invalid-closure-errors-{i}' for i in range(3))}),
            'endpoint': ('endpoint not closed', {'live-endpoint-contradicts-closure', 'missing-closure-errno', *(
                f'invalid-closure-errno-{i}' for i in range(5))}),
        }
        for name, (marker, expected) in groups.items():
            with self.subTest(mutant=name):
                rows = self.run_variant('default-remove-' + name,
                                        self.variant('linux_closure', marker, 'True'), default=True)
                self.assert_exact_escape(name, rows, expected, default=True)

    def test_default_order_mutants_have_exact_effects(self):
        ns = self.variant('actual_default_pair', 'default products not sequential', 'True')
        rows = self.run_variant('default-remove-order', ns, default=True)
        self.assert_exact_escape('default-order', rows, boundary_ids((-86400000000, -1)), default=True)
        # The base synthetic pair is also exactly adjacent, including these
        # positive neighbors whose mutations leave timestamps untouched.
        neighbors = {'exact-observed-oracle-default', 'known-closed-endpoint-2', 'known-closed-endpoint-111',
                     'helper-wait-unknown', 'legal-zero-and-nonzero-public-IDs'}
        for name, expression, offsets in [
            ('equal', "command_time(receipts['oracle']['ended']) < command_time(receipts['candidate']['started'])", (0,)),
            ('microsecond', "command_time(receipts['oracle']['ended']) + datetime.timedelta(microseconds=2) <= command_time(receipts['candidate']['started'])", (0, 1)),
        ]:
            with self.subTest(mutant=name):
                ns = self.variant('actual_default_pair', 'default products not sequential', expression)
                rows = self.run_variant('default-reject-' + name, ns, default=True)
                # Ordering precedes cross-product value comparison, so this
                # negative neighbor changes rejection cause as well.
                expected = neighbors | boundary_ids(offsets) | {'printed-value-without-correct-default'}
                self.assertEqual({key for key in rows if rows[key] != self.default_rows[key]}, expected)
                self.assertTrue(all(rows[key]['status'] == 'FAIL'
                                    and rows[key]['reason'] == 'default products not sequential' for key in expected))


if __name__ == '__main__':
    unittest.main()
