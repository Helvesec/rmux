"""Production budget: G stays a recorded target, the ratchet blocks growth."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from compare import release_baseline
from inventory import scan
from model import canonical, read_tree
from production_budget import BUDGET, budget_summary, enforce, load_budget, lowered_budget

REVISION = 'e504ff12b21fe6784c44470482f6feafdad774e0'


def budget(lines=3, **ratchet):
    return {'schema': 'rmux-source-budget/1', 'unit': 'production lines',
            'G': {'production_lines': 2, 'status': 'not met', 'deferred_to': '0.12',
                  'basis': 'fixture', 'decision': 'fixture decision'},
            'production_ratchet': dict({'production_lines': lines, 'measured_revision': REVISION,
                                        'production_growth_envelope': 0}, **ratchet)}


def encoded(value):
    return canonical(value).encode()


class BudgetSchemaTests(unittest.TestCase):
    def test_published_budget_records_unmet_g_and_measured_ratchet(self):
        published = load_budget(BUDGET.read_bytes())
        self.assertEqual(published['G']['production_lines'], 301947)
        self.assertEqual(published['G']['status'], 'not met')
        self.assertEqual(published['G']['deferred_to'], '0.12')
        self.assertEqual(published['production_ratchet']['production_growth_envelope'], 1925)
        self.assertGreater(published['production_ratchet']['production_lines'],
                           published['G']['production_lines'])

    def test_malformed_budgets_fail_closed(self):
        cases = []
        for mutate in (
                lambda b: b.update(extra=1),
                lambda b: b.update(schema='rmux-source-budget/2'),
                lambda b: b['G'].update(status='deferred'),
                lambda b: b['G'].update(decision=''),
                lambda b: b['G'].update(production_lines=-1),
                lambda b: b['production_ratchet'].update(production_lines=True),
                lambda b: b['production_ratchet'].update(measured_revision='e504ff12'),
                lambda b: b['production_ratchet'].update(unknown='x'),
                lambda b: b['production_ratchet'].pop('production_growth_envelope'),
                lambda b: b['production_ratchet'].update(production_growth_envelope=5),
                lambda b: b['production_ratchet'].update(
                    production_growth_envelope=1001, production_growth_justification='why')):
            value = budget()
            mutate(value)
            cases.append(value)
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ValueError):
                load_budget(encoded(value))

    def test_declared_envelope_requires_its_justification_and_decision(self):
        accepted = budget(production_growth_envelope=1001, production_growth_justification='why',
                          production_growth_decision='D1')
        self.assertEqual(load_budget(encoded(accepted)), accepted)


class BudgetCheckTests(unittest.TestCase):
    def report(self, production, errors=()):
        return {'revision': REVISION, 'totals': {'production': production, 'tests': 9},
                'errors': list(errors)}

    def test_equal_or_lower_production_is_green_and_g_gap_is_reported(self):
        for production in (3, 1):
            with self.subTest(production=production):
                summary = enforce(budget_summary(self.report(production), budget()))
                self.assertIsNone(summary['ratchet_violation'])
                self.assertEqual(summary['delta_to_G'], production - 2)
                self.assertEqual(summary['G_status'], 'not met')

    def test_one_line_above_the_ratchet_is_red(self):
        summary = budget_summary(self.report(4), budget())
        self.assertIn('production ratchet RED: 4 lines', summary['ratchet_violation'])
        with self.assertRaises(ValueError):
            enforce(summary)

    def test_growth_within_a_justified_envelope_is_green(self):
        value = budget(production_growth_envelope=2, production_growth_justification='why')
        self.assertIsNone(budget_summary(self.report(5), value)['ratchet_violation'])
        self.assertIsNotNone(budget_summary(self.report(6), value)['ratchet_violation'])

    def test_unclassified_inventory_cannot_be_budgeted(self):
        with self.assertRaises(ValueError):
            budget_summary(self.report(1, ['unclassified file: x']), budget())

    def test_recomputed_ratchet_only_moves_down(self):
        original = budget()
        lowered = lowered_budget(dict(self.report(2), revision='a' * 40), copy.deepcopy(original))
        self.assertEqual(lowered['production_ratchet'],
                         {'production_lines': 2, 'measured_revision': 'a' * 40,
                          'production_growth_envelope': 0})
        self.assertEqual(lowered['G'], original['G'])
        with self.assertRaises(ValueError):
            lowered_budget(self.report(4), original)


class CompareBudgetCliTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.repo = Path(directory.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Budget fixture')
        self.git('config', 'user.email', 'fixture')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args],
                                       stderr=subprocess.PIPE, text=True).strip()

    def report(self, lines):
        (self.repo / 'src.txt').write_text('x\n' * lines)
        self.git('add', '.')
        self.git('commit', '-qm', f'{lines} production lines')
        revision = self.git('rev-parse', 'HEAD')
        tree = read_tree(self.repo, revision)
        policy = dict(version=1, files={'production': list(tree)}, documents={},
                      provenance={}, includes={}, rust_roots={}, crate_roles={})
        return scan(self.repo, revision, Path('/unused'), policy, encoded(policy), parsed={})

    def compare(self, before, after, ratchet, *flags, shallow=None):
        directory = Path(tempfile.mkdtemp(dir=self.repo / '.git'))
        source = '--before' if shallow is None else '--release-baseline'
        reference = before if shallow is None else release_baseline(before)
        for name, value in (('before.json', reference), ('after.json', after),
                            ('budget.json', budget(ratchet, measured_revision=before['revision']))):
            (directory / name).write_text(canonical(value))
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('compare.py')),
            '--repo', str(shallow or self.repo), source, str(directory / 'before.json'),
            '--after', str(directory / 'after.json'), '--output', str(directory / 'delta.json'),
            '--budget', str(directory / 'budget.json'), *flags], capture_output=True, text=True)
        return result, json.loads((directory / 'delta.json').read_text())

    def test_check_ratchets_enforces_the_budget_ratchet(self):
        before = self.report(3)
        after = self.report(4)
        result, delta = self.compare(before, after, 3, '--check-ratchets')
        self.assertEqual(result.returncode, 1)
        self.assertIn('production ratchet RED: 4 lines', result.stderr)
        self.assertEqual(delta['production_budget']['delta_to_ratchet'], 1)
        result, _ = self.compare(before, after, 4, '--check-ratchets')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_release_ratchets_enforce_exact_cap_without_measurement_commit(self):
        before = self.report(3)
        after = self.report(4)
        with tempfile.TemporaryDirectory() as directory:
            clone = Path(directory) / 'shallow'
            self.git('clone', '-q', '--no-tags', '--depth=1', self.repo.as_uri(), str(clone))
            self.assertEqual(self.git('-C', str(clone), 'rev-list', '--all'), after['revision'])
            missing = subprocess.run(['git', '-C', str(clone), 'cat-file', '-e', before['revision']],
                                     capture_output=True)
            self.assertNotEqual(missing.returncode, 0)
            result, _ = self.compare(before, after, 4, '--check-ratchets', shallow=clone)
            self.assertEqual(result.returncode, 0, result.stderr)
            result, _ = self.compare(before, after, 3, '--check-ratchets', shallow=clone)
            self.assertEqual(result.returncode, 1)
            self.assertIn('production ratchet RED: 4 lines', result.stderr)

    def test_budget_is_reported_without_enforcement_outside_check_ratchets(self):
        before = self.report(3)
        after = self.report(4)
        result, delta = self.compare(before, after, 3)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNotNone(delta['production_budget']['ratchet_violation'])


if __name__ == '__main__':
    unittest.main()
