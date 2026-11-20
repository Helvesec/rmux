"""D89: FF measures absolute rules while settlement stays strict at closure."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import production_gate as gate
from model import canonical


class ProductionModeTests(unittest.TestCase):
    def setUp(self):
        audit = Path(os.environ.get('SOURCE_AUDIT', '.rmux-audit/d89-fixtures')).resolve()
        audit.mkdir(parents=True, exist_ok=True)
        self.repo = Path(tempfile.mkdtemp(dir=audit))
        self.git('init', '-q')
        self.git('config', 'user.name', 'D89 fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.write('Cargo.toml', '[package]\nname="fixture"\nversion="0.1.0"\n')
        self.write('src/lib.rs', 'pub fn base() {}\n')
        parent = self.commit()
        reference = self.commit()
        declarations = dict(version=1, files={'tooling': ['Cargo.toml', *[
            gate.PREFIX + name for name in ('declarations.json', 'production-reference.json',
                                           'blacklist-policy.json', 'production-ledger.json')]]},
            documents={}, provenance={}, includes={}, rust_roots={}, crate_roles={})
        self.write(gate.PREFIX + 'declarations.json', canonical(declarations))
        self.write(gate.PREFIX + 'production-reference.json', '{}')
        self.write(gate.PREFIX + 'blacklist-policy.json', canonical(dict(
            adopted=True, token_sha256=[list(token) for token in gate.TOKENS],
            generic_pattern=gate.GENERIC)))
        initial = dict(lot='INT145', before=parent, after=reference,
                       before_production=1, after_production=1, delta=0,
                       mandate={'production_growth_envelope': 0})
        self.ledger = dict(schema=1, entries=[initial])
        self.write(gate.PREFIX + 'production-ledger.json', canonical(self.ledger))
        self.before = self.commit()
        self.write('src/lib.rs', 'pub fn base() {}\npub fn addition() {}\n')
        self.after = self.commit()
        for name, value in [('REFERENCE_REVISION', reference), ('REFERENCE_LINES', 1)]:
            p = patch.object(gate, name, value)
            p.start()
            self.addCleanup(p.stop)
        p = patch.object(gate, 'validate_reference')
        p.start()
        self.addCleanup(p.stop)

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args],
                                       stderr=subprocess.PIPE, text=True).strip()

    def write(self, path, value):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(value)

    def commit(self):
        self.git('add', '.')
        self.git('commit', '--allow-empty', '-qm', 'D89 composition fixture')
        return self.git('rev-parse', 'HEAD')

    def evaluate(self, **kwargs):
        return gate.evaluate(self.repo, self.before, self.after,
                             Path(os.environ['SOURCE_SYNTAX']), **kwargs)

    def test_uncovered_composition_passes_only_explicit_ff(self):
        with self.assertRaisesRegex(ValueError, 'uncovered composition changes'):
            self.evaluate()
        for mode in ('campaign', 'closure'):
            with self.subTest(mode=mode), self.assertRaisesRegex(
                    ValueError, 'uncovered composition changes'):
                self.evaluate(mode=mode)
        result, ledger, reports, delta = self.evaluate(mode='ff')
        self.assertEqual(result['mode'], 'ff')
        self.assertEqual(result['schema'], 'rmux-production-gate/2')
        self.assertEqual(result['tree'], self.git('rev-parse', self.after + '^{tree}'))
        self.assertEqual((result['production_before'], result['production_after'], result['delta']),
                         (1, 2, 1))
        self.assertEqual(result['blacklist_matches'], [])
        self.assertIsNone(result['balance'])
        self.assertIsNone(result['envelope'])
        self.assertEqual(result['settlement_checks'], 'deferred')
        self.assertEqual(ledger, self.ledger)
        self.assertIn(self.after, reports)
        self.assertEqual(delta['literal_ratchet_increases'], [])

    def test_absolute_blacklist_remains_red_in_ff(self):
        self.write('src/lib.rs', 'pub fn base() {} // fleet\n')
        self.after = self.commit()
        with self.assertRaisesRegex(ValueError, 'absolute production blacklist RED'):
            self.evaluate(mode='ff')
        # Unchanged inherited occurrences are still absolute failures.
        self.before = self.after
        with self.assertRaisesRegex(ValueError, 'absolute production blacklist RED'):
            self.evaluate(mode='ff')

    def test_new_lot_cannot_settle_an_unrecorded_prior_balance(self):
        self.before = self.after  # The first +1 remains deliberately unrecorded.
        self.write('src/lib.rs', 'pub fn base() {}\npub fn addition() {}\npub fn next() {}\n')
        self.after = self.commit()
        record = dict(id='next', production_growth_envelope=1,
                      production_growth_justification='next measured addition only')
        with self.assertRaisesRegex(ValueError, 'balance does not reconcile'):
            self.evaluate(record=record)

    def test_ff_preserves_inventory_ratchets_and_adopted_policy(self):
        changes = (
            ('unknown-input', 'unknown.txt', 'unclassified\n', 'inventory RED'),
            ('attribute', 'src/lib.rs', '#[allow(dead_code)]\npub fn base() {}\n',
             'source ratchet RED'),
            ('policy', gate.PREFIX + 'blacklist-policy.json', canonical(dict(
                adopted=True, token_sha256=[['ab', 5, '0' * 64]], generic_pattern=gate.GENERIC)),
             'adopted blacklist policy changed'),
            ('reference', gate.PREFIX + 'production-reference.json', '{"changed":true}',
             'adopted reference/policy changed'))
        original = self.after
        for label, path, content, error in changes:
            self.git('checkout', '-q', original)
            self.write(path, content)
            self.after = self.commit()
            with self.subTest(label=label), self.assertRaisesRegex(ValueError, error):
                self.evaluate(mode='ff')

    def test_unknown_mode_and_ff_settlement_options_fail_closed(self):
        for mode in ('FF', '', 'unknown', None):
            with self.subTest(mode=mode), self.assertRaisesRegex(ValueError, 'mode'):
                self.evaluate(mode=mode)
        for options in (dict(record={'id': 'A'}), dict(mandates=[{'id': 'A'}]),
                        dict(freeze=True)):
            with self.subTest(options=options), self.assertRaisesRegex(ValueError, 'FF|ff'):
                self.evaluate(mode='ff', **options)

    def test_cli_forwards_explicit_modes_and_default_is_strict(self):
        parser = self.repo / 'parser'
        parser.write_text('fixture')
        argv = ['production_gate.py', '--repo', str(self.repo), '--before', self.before,
                '--after', self.after, '--parser', str(parser), '--output', str(self.repo / 'out')]
        for mode in (None, 'ff', 'campaign', 'closure'):
            with self.subTest(mode=mode), patch.object(gate, 'TOOL', self.repo / gate.PREFIX), \
                    patch.object(gate, 'git', side_effect=[self.after.encode(), b'']), \
                    patch.object(gate, 'qualified_parser', return_value={'qualified': True}) as qualified, \
                    patch.object(gate, 'evaluate', side_effect=ValueError('measured sentinel')) as evaluate, \
                    patch.object(sys, 'argv', argv + (['--mode', mode] if mode else [])), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(gate.main(), 1)
                self.assertEqual(evaluate.call_args.kwargs['mode'], mode or 'closure')
                qualified.assert_called_once()
        with patch.object(sys, 'argv', argv + ['--mode', 'unknown']), \
                patch.object(gate, 'evaluate') as evaluate, \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            gate.main()
        self.assertEqual(error.exception.code, 2)
        evaluate.assert_not_called()

    def test_cli_ff_checks_parser_twice_and_never_writes_ledger(self):
        parser = self.repo / 'parser'
        parser.write_text('fixture')
        out = self.repo / 'out'
        argv = ['production_gate.py', '--repo', str(self.repo), '--before', self.before,
                '--after', self.after, '--parser', str(parser), '--output', str(out),
                '--mode', 'ff']
        ledger_path = self.repo / gate.PREFIX / 'production-ledger.json'
        original = ledger_path.read_bytes()
        result = dict(mode='ff', exit=0, settlement_checks='deferred')
        for changed in (True, False):
            identities = [{'qualified': True}, {'qualified': not changed}]
            with self.subTest(changed=changed), \
                    patch.object(gate, 'TOOL', self.repo / gate.PREFIX), \
                    patch.object(gate, 'git', side_effect=[self.after.encode(), b'']), \
                    patch.object(gate, 'qualified_parser', side_effect=identities) as qualified, \
                    patch.object(gate, 'evaluate', return_value=(result, {}, {}, {})), \
                    patch.object(sys, 'argv', argv), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(gate.main(), 1 if changed else 0)
                self.assertEqual(qualified.call_count, 2)
                self.assertEqual(ledger_path.read_bytes(), original)
                self.assertEqual(out.exists(), not changed)
        self.assertEqual(json.loads((out / 'production.json').read_text())['parser_identity'],
                         {'qualified': True})
