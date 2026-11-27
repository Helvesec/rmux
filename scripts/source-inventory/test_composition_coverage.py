"""Keep the measured INT148 witness separate from cumulative campaign debt."""
import json
import copy
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import unittest.mock

import production_gate as gate
from model import git
import ledger_evidence


class RealCompositionCoverage(unittest.TestCase):
    BASE = 'ea435dddd3fba0e2c9e8e7c8d97150b0712ed21b'
    CANDIDATE = '05f405c869ea66271d3eba6d717a9eb62ebf3558'

    def test_int148_real_composition_and_settlements(self):
        repo = Path(__file__).resolve().parents[2]
        ledger = json.loads((repo / gate.PREFIX / 'production-ledger.json').read_text())
        root = ledger_evidence.audit_root(repo, ledger)
        if root is None:
            self.skipTest(ledger_evidence.AUDIT_SKIP)
        before, after, rows, mandates = gate.coverage_witness(repo, ledger, 'INT148', root)
        self.assertEqual(before, self.BASE)
        self.assertEqual(after, self.CANDIDATE)
        self.assertEqual([row['lot'] for row in rows], [
            'S3-L014-D65-BLACKLIST-PRODUCTION-RATCHET-090',
            'S3-L014-R1283-F1-LOT-ENVELOPE-CORRECTION-091',
            'S3-L014-INT148-COVERAGE-GAP-CORRECTION-092'])
        self.assertEqual([(row['before_production'], row['after_production'], row['delta'])
                          for row in rows], [(343521, 343521, 0)] * 3)
        gate.check_lot_bounds(repo, self.BASE, after, rows, mandates)
        # A historical green is never campaign credit on another composition.
        with self.assertRaisesRegex(ValueError, 'uncovered|cover composition'):
            gate.check_lot_bounds(repo, self.BASE,
                git(repo, 'rev-parse', 'HEAD').decode().strip(), rows, mandates)

    def test_false_witness_and_unresolvable_proof_are_rejected(self):
        repo = Path(__file__).resolve().parents[2]
        ledger = json.loads((repo / gate.PREFIX / 'production-ledger.json').read_text())
        root = ledger_evidence.audit_root(repo, ledger)
        if root is None:
            self.skipTest(ledger_evidence.AUDIT_SKIP)
        for field, value, reason in [
                ('after', self.BASE, 'witness differs from measured reviewed bounds'),
                ('lots', ['INT145', 'INT145'], 'duplicate or missing witness lot'),
                ('bounds', 'sha256:' + '0' * 64, 'unresolvable ledger evidence locator')]:
            bad = copy.deepcopy(ledger)
            bad['coverage_witnesses']['INT148'][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, reason):
                gate.coverage_witness(repo, bad, 'INT148', root)
        ledger_evidence.validate(repo, ledger, root)
        for value, reason in [('ledger-origin:/absent', 'origin cannot replace proof bytes'),
                              ('sha256:' + '0' * 64, 'unresolvable ledger evidence locator')]:
            with self.subTest(locator=value), self.assertRaisesRegex(ValueError, reason):
                ledger_evidence.resolve(repo, ledger, value, root)
        bad = copy.deepcopy(ledger)
        bad['historical_settlements'][0]['delta'] = 0
        with self.assertRaisesRegex(ValueError, 'historical ledger facts changed'):
            ledger_evidence.validate(repo, bad, root)

    def test_parser_receipt_and_build_are_consumed_as_archived_bytes(self):
        repo = Path(__file__).resolve().parents[2]
        ledger = json.loads((repo / gate.PREFIX / 'production-ledger.json').read_text())
        parser = ledger['historical_settlements'][0]['measurement']['parser_origin']
        for label, locator in [('receipt', parser['unit']['receipt']),
                               ('build', parser['build'])]:
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with self.subTest(link=label, state='absent'), self.assertRaises(ValueError):
                    ledger_evidence.resolve(repo, ledger, locator, root)
                digest = parser['unit']['receipt_sha256'] if label == 'receipt' else (locator.removeprefix('sha256:') if locator.startswith('sha256:') else '0' * 64)
                (root / digest).write_bytes(b'corrupt archived proof')
                with self.subTest(link=label, state='corrupt'), self.assertRaises(ValueError):
                    ledger_evidence.resolve(repo, ledger, locator, root)
        self.assertEqual(parser['unit']['receipt'], 'sha256:' + parser['unit']['receipt_sha256'])

    def test_all_portable_proofs_and_inherited_envelope_are_verified(self):
        repo = Path(__file__).resolve().parents[2]
        ledger = json.loads((repo / gate.PREFIX / 'production-ledger.json').read_text())
        root = ledger_evidence.audit_root(repo, ledger)
        if root is None:
            self.skipTest(ledger_evidence.AUDIT_SKIP)
        reasons = {'witness': 'unresolvable ledger evidence locator',
                   'inherited': 'unresolvable ledger evidence locator',
                   'envelope': 'settlement envelope differs from archived mandate'}
        for field in ('witness', 'inherited', 'envelope'):
            bad = copy.deepcopy(ledger)
            if field == 'witness':
                bad['coverage_witnesses']['INT148']['bounds'] = 'sha256:' + '0' * 64
            elif field == 'inherited':
                bad['reviewed_ranges'][-1]['inherited_mandate'] = 'sha256:' + '0' * 64
            else:
                bad['entries'][-1]['mandate']['production_growth_envelope'] = 601
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, reasons[field]):
                ledger_evidence.validate(repo, bad, root)


class DeclaredProofRootWiring(unittest.TestCase):
    """R2221: the D65/D124 canaries read the declared root, never the checkout default."""
    CANARIES = ('test_composed_growth.ComposedGrowth.'
                'test_d124_exact_reviewed_attachments_and_explicit_open_remainder',
                'test_composition_coverage.RealCompositionCoverage.'
                'test_int148_real_composition_and_settlements',
                'test_composition_coverage.RealCompositionCoverage.'
                'test_false_witness_and_unresolvable_proof_are_rejected')

    def test_declared_absent_proof_root_keeps_every_canary_red(self):
        repo = Path(__file__).resolve().parents[2]
        default = str(repo / '.rmux-audit/ledger-evidence/objects')

        def named(path, text):
            # FileNotFoundError quotes its path with repr(): Windows doubles '\\'.
            return path in text or repr(path)[1:-1] in text

        with tempfile.TemporaryDirectory() as directory:
            declared = str(Path(directory).resolve())
            for name in self.CANARIES:
                result = unittest.TestResult()
                with self.subTest(canary=name), unittest.mock.patch.dict(
                        os.environ, {ledger_evidence.AUDIT_VARIABLE: declared}):
                    unittest.defaultTestLoader.loadTestsFromName(name).run(result)
                    failures = '\n'.join(text for _, text in result.errors + result.failures)
                    self.assertEqual((result.testsRun, result.skipped), (1, []))
                    self.assertFalse(result.wasSuccessful())
                    self.assertFalse(named(default, failures), failures)
                    self.assertTrue(named(declared, failures)
                                    or 'ledger history unavailable' in failures, failures)


class BranchCompositionCoverage(unittest.TestCase):
    def test_exact_branch_composition_and_negative_ranges(self):
        audit = Path(os.environ.get('SOURCE_AUDIT', '.rmux-audit/branch-fixtures')).resolve()
        audit.mkdir(parents=True, exist_ok=True)
        repo = Path(tempfile.mkdtemp(dir=audit))

        def run(*args):
            return git(repo, *args).decode().strip()

        def commit(path, content):
            (repo / path).write_text(content)
            run('add', path)
            run('commit', '-qm', 'branch coverage fixture')
            return run('rev-parse', 'HEAD')

        run('init', '-q')
        run('config', 'user.name', 'Coverage fixture')
        run('config', 'user.email', 'fixture@example.invalid')
        base = commit('base', 'base\n')
        run('checkout', '-qb', 'left')
        left = commit('left', 'left\n')
        run('checkout', '-qb', 'right', base)
        right = commit('right', 'right\n')
        run('checkout', '-q', 'left')
        run('merge', '--no-ff', '-qm', 'reviewed composition', 'right')
        merged = run('rev-parse', 'HEAD')
        # The composed range explicitly owns both the right branch and merge.
        rows = [dict(lot='A', before=base, after=left),
                dict(lot='B', before=left, after=merged)]
        mandates = {row['lot']: dict(reviewed_before=row['before'], reviewed_after=row['after'])
                    for row in rows}
        gate.check_lot_bounds(repo, base, merged, rows, mandates)
        for label, altered in [('duplicate', [*rows, rows[0]]),
                               ('gap', rows[:1]),
                               ('overlap', [rows[0], dict(lot='B', before=base, after=merged)]),
                               ('uncovered merge', [rows[0], dict(lot='B', before=base, after=right)]),
                               ('false range', [rows[0], dict(lot='B', before=merged, after=left)])]:
            supplied = {row['lot']: dict(reviewed_before=row['before'], reviewed_after=row['after'])
                        for row in altered}
            with self.subTest(label=label), self.assertRaises(ValueError):
                gate.check_lot_bounds(repo, base, merged, altered, supplied)
