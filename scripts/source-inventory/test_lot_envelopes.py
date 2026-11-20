"""F1: compensating lots cannot hide growth behind invented zero settlements."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import production_gate as gate
from model import canonical


class LotEnvelopeTests(unittest.TestCase):
    def test_compensating_lots_require_real_reviewed_bounds(self):
        audit = Path(os.environ.get('SOURCE_AUDIT', '.rmux-audit/f1-fixtures')).resolve()
        audit.mkdir(parents=True, exist_ok=True)
        repo = Path(tempfile.mkdtemp(dir=audit))

        def git(*args):
            return subprocess.check_output(['git', '-C', str(repo), *args],
                                           stderr=subprocess.PIPE, text=True).strip()

        def write(path, value):
            target = repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(value)

        def commit():
            git('add', '.')
            git('commit', '--allow-empty', '-qm', 'synthetic lot')
            return git('rev-parse', 'HEAD')

        git('init', '-q')
        git('config', 'user.name', 'F1 fixture')
        git('config', 'user.email', 'fixture@example.invalid')
        write('Cargo.toml', '[package]\nname="fixture"\nversion="0.1.0"\n')
        write('src/lib.rs', 'pub fn base() {}\n')
        parent = commit()
        reference = commit()
        declarations = dict(version=1, files={'tooling': ['Cargo.toml', *[gate.PREFIX + name for name in (
                                'declarations.json', 'production-reference.json',
                                'blacklist-policy.json', 'production-ledger.json')]]},
                            documents={}, provenance={}, includes={}, rust_roots={}, crate_roles={})
        write(gate.PREFIX + 'declarations.json', canonical(declarations))
        write(gate.PREFIX + 'production-reference.json', '{}')
        write(gate.PREFIX + 'blacklist-policy.json', canonical(dict(
            adopted=True, token_sha256=[list(token) for token in gate.TOKENS],
            generic_pattern=gate.GENERIC)))
        initial = dict(lot='INT145', before=parent, after=reference,
                       before_production=1, after_production=1, delta=0,
                       mandate={'production_growth_envelope': 0})
        write(gate.PREFIX + 'production-ledger.json', canonical(dict(schema=1, entries=[initial])))
        before = commit()
        write('src/lib.rs', 'pub fn base() {}\npub fn added() {}\n')
        a = commit()
        write('src/lib.rs', 'pub fn base() {}\n')
        b = commit()
        mandates = [dict(id=lot, production_growth_envelope=0,
                        reviewed_before=left, reviewed_after=right)
                    for lot, left, right in [('A', before, a), ('B', a, b)]]
        rows = [dict(initial, lot=m['id'], before=before, after=before) for m in mandates]
        write(gate.PREFIX + 'production-ledger.json', canonical(dict(schema=1, entries=[initial, *rows])))
        after = commit()
        with patch.object(gate, 'REFERENCE_REVISION', reference), \
                patch.object(gate, 'REFERENCE_LINES', 1), \
                patch.object(gate, 'validate_reference'):
            parser = Path(os.environ['SOURCE_SYNTAX'])

            def evaluate(end, supplied, **kwargs):
                return gate.evaluate(repo, before, end, parser, supplied, **kwargs)

            with self.assertRaisesRegex(ValueError, 'lot production RED|reviewed bounds'):
                evaluate(after, mandates)
            # Honest settlements must expose A's own excess despite the net zero.
            rows = [dict(rows[0], before=before, after=a, after_production=2, delta=1),
                    dict(rows[1], before=a, after=b, before_production=2, delta=-1)]

            def settled():
                write(gate.PREFIX + 'production-ledger.json', canonical(
                    dict(schema=1, entries=[initial, *rows])))
                return commit()

            after = settled()
            with self.assertRaisesRegex(ValueError, 'lot production RED'):
                evaluate(after, mandates)
            allowed = copy.deepcopy(mandates)
            allowed[0].update(production_growth_envelope=1,
                              production_growth_justification='synthetic addition')
            rows[0]['mandate'] = gate.envelope_fields(allowed[0])
            after = settled()
            result = evaluate(after, allowed)[0]
            self.assertEqual((result['delta'], result['balance']), (0, 0))
            for label, field, value in (
                    ('overlap', 'reviewed_before', before),
                    ('gap', 'reviewed_before', reference),
                    ('false endpoint', 'reviewed_after', reference),
                    ('short SHA', 'reviewed_after', b[:12]),
                    ('mutable ref', 'reviewed_after', 'HEAD'),
                    ('missing SHA', 'reviewed_after', None)):
                bad = copy.deepcopy(allowed)
                bad[1][field] = value
                with self.subTest(label=label), self.assertRaisesRegex(ValueError, 'reviewed bounds'):
                    evaluate(after, bad)
            # An uncovered +1/-1 pair cannot be hidden in the ledger-write tail.
            write('src/lib.rs', 'pub fn base() {}\npub fn hidden() {}\n')
            commit()
            write('src/lib.rs', 'pub fn base() {}\n')
            uncovered = commit()
            with self.assertRaisesRegex(ValueError, 'uncovered composition changes'):
                evaluate(uncovered, allowed)
            # Recording a single lot still uses the exact measured argv bounds.
            result, ledger, _, _ = evaluate(b, (), record=dict(id='recorded'))
            self.assertEqual(result['delta'], 0)
            self.assertEqual((ledger['entries'][-1]['before'], ledger['entries'][-1]['after']),
                             (before, b))


class ClosedLedgerRedactionTests(unittest.TestCase):
    """D458: only the authorized home-prefix rewrite of closed lot 108 is tolerated."""
    HOME = '/Users/example-user/'

    def ledgers(self):
        redacted = json.loads((gate.TOOL / 'production-ledger.json').read_text())['entries']
        index = next(i for i, row in enumerate(redacted) if row['lot'] == gate.REDACTED_LOT)
        original = copy.deepcopy(redacted)
        mandate = original[index]['mandate']
        self.assertEqual(mandate[gate.REDACTED_FIELD].count(gate.NEUTRAL_HOME), 1)
        mandate[gate.REDACTED_FIELD] = mandate[gate.REDACTED_FIELD].replace(
            gate.NEUTRAL_HOME, self.HOME)
        return original, redacted, index

    def test_authorized_home_redaction_of_closed_entry_is_accepted(self):
        original, redacted, _ = self.ledgers()
        gate.check_closed_entries(original, redacted)
        gate.check_closed_entries(original, [*redacted, dict(lot='next')])
        gate.check_closed_entries(redacted, redacted)

    def test_other_closed_entry_changes_stay_refused(self):
        original, redacted, index = self.ledgers()
        field = gate.REDACTED_FIELD

        def other_prefix(rows):
            rows[index]['mandate'][field] = original[index]['mandate'][field].replace(
                self.HOME, '/opt/')

        def extra_edit(rows):
            rows[index]['mandate'][field] += ' '

        def envelope(rows):
            rows[index]['mandate']['production_growth_envelope'] += 1

        def other_lot(rows):
            rows[index - 1]['mandate'][field] = 'changed'

        def reverted(rows):
            rows[:] = copy.deepcopy(original)

        def dropped(rows):
            del rows[index]

        two_homes = copy.deepcopy(original)
        two_homes[index]['mandate'][field] += ' ' + self.HOME

        def both_redacted(rows):
            rows[index]['mandate'][field] = two_homes[index]['mandate'][field].replace(
                self.HOME, gate.NEUTRAL_HOME)

        cases = (('other prefix', original, other_prefix), ('extra edit', original, extra_edit),
                 ('two homes', two_homes, both_redacted),
                 ('envelope', original, envelope), ('other lot', original, other_lot),
                 ('unredaction', redacted, reverted), ('dropped entry', original, dropped))
        for label, old, mutate in cases:
            rows = copy.deepcopy(redacted)
            mutate(rows)
            with self.subTest(label=label), self.assertRaisesRegex(
                    ValueError, 'closed production ledger entries changed'):
                gate.check_closed_entries(old, rows)
