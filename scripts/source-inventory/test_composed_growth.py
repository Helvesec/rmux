"""D124 acceptance: account for exactly 913 without absorbing unreviewed code."""
import copy
import json
from pathlib import Path
import unittest

import ledger_evidence


class ComposedGrowth(unittest.TestCase):
    def test_d124_exact_reviewed_attachments_and_explicit_open_remainder(self):
        repo = Path(__file__).resolve().parents[2]
        ledger = json.loads(Path(__file__).with_name('production-ledger.json').read_text())
        growth = ledger.get('composed_growth', {})
        self.assertEqual(growth.get('total'), 913)
        self.assertEqual(growth['before'], 'f54ae83954c368c5c4e3706806690e0388c5414e')
        self.assertEqual(growth['after'], '7a63d529d7e6213f2e1410266734531e0b7555b8')
        self.assertEqual(913, sum(row['delta'] for row in growth['attachments'])
                         + growth['open_remainder'])
        self.assertEqual(growth['status'], 'OUVERT')
        self.assertEqual(len(growth['historical_outside_ancestry']), 8)
        root = ledger_evidence.audit_root(repo, ledger)
        if root is None:
            self.skipTest(ledger_evidence.AUDIT_SKIP)
        ledger_evidence.validate(repo, ledger, root)
        register = 'invalid bounded D124 register'
        for field, value, reason in [
                ('total', 914, register),
                ('open_remainder', growth['open_remainder'] + 1,
                 'D124 913 differs from attachments plus open remainder'),
                ('status', 'CLOSED', register), ('before', growth['before'][:12], register)]:
            bad = copy.deepcopy(ledger)
            bad['composed_growth'][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, reason):
                ledger_evidence.validate(repo, bad, root)
        bad = copy.deepcopy(ledger)
        bad['composed_growth']['historical_outside_ancestry'][0]['status'] = 'CREDITED'
        with self.assertRaisesRegex(ValueError, 'historical branch head cannot credit'):
            ledger_evidence.validate(repo, bad, root)
        for index, row in enumerate(growth['attachments']):
            self.assertTrue(row['owner'])
            self.assertTrue(row['lane'])
            self.assertEqual(row['status'], 'PASS')
            owner = 'D124 attachment lacks its lane owner or PASS'
            for field, value, reason in [
                    ('review', 'sha256:' + '0' * 64, 'unresolvable ledger evidence locator'),
                    ('owner', '', owner), ('status', 'OUVERT', owner),
                    ('after', growth['before'], 'attachment leaves its reviewed composition')]:
                bad = copy.deepcopy(ledger)
                bad['composed_growth']['attachments'][index][field] = value
                with self.subTest(index=index, field=field):
                    with self.assertRaisesRegex(ValueError, reason):
                        ledger_evidence.validate(repo, bad, root)
        if growth['attachments']:
            bad = copy.deepcopy(ledger)
            bad['composed_growth']['attachments'].append(growth['attachments'][0])
            bad['composed_growth']['open_remainder'] -= growth['attachments'][0]['delta']
            with self.assertRaisesRegex(ValueError, 'duplicate D124 attachment'):
                ledger_evidence.validate(repo, bad, root)
