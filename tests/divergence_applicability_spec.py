"""Exact integrated ledger delta; historical population and strict reader retained."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/validation'))
import contracts

PATH = 'tests/reference/tmux_compat/divergences.toml'
BASE = '80575f422b07e12f1399e54ebee5049dc65503e6'


class DivergenceApplicability(unittest.TestCase):
    def test_exact_integrated_delta_preserves_the_historical_population(self):
        import reference_applicability as applicability
        contract = json.loads((ROOT / 'docs/validation/reference-contracts.json').read_text())
        historical = subprocess.check_output(['git', 'show', BASE + ':' + PATH]).decode()
        raw = (ROOT / PATH).read_bytes()
        reference = {'path': PATH, 'sha256': hashlib.sha256(historical.encode()).hexdigest()}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / PATH
            target.parent.mkdir(parents=True)
            target.write_bytes(raw)
            read = lambda ref=reference, data=contract, old=False: applicability.read_divergence_reference(
                root, ref, data, contracts.read_reference, historical=old)
            self.assertEqual(read(), raw.decode())
            self.assertEqual(read(old=True), historical)
            self.assertEqual(len(contracts.source_ids('divergences', read(old=True))), 59)
            self.assertEqual(len(contracts.source_ids('divergences', read())), 64)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                contracts.read_reference(root, reference)
            for altered in (raw + b' ', raw.replace(b'C-D93', b'C-D94'),
                            raw.replace(b'C-D32', b'C-D33'),
                            raw[:raw.rfind(b'\n[[entry]]')],
                            raw.replace(b'inventory = "bounded"', b'inventory = "complete"', 1)):
                target.write_bytes(altered)
                with self.assertRaises(ValueError):
                    read()
            target.write_bytes(raw)
            for declaration in (None, {}, {'current_sha256': hashlib.sha256(raw).hexdigest()}):
                altered = copy.deepcopy(contract)
                altered['divergence_reference_applicability'] = declaration
                with self.assertRaises(ValueError):
                    read(data=altered)
            altered = copy.deepcopy(contract)
            altered['divergence_reference_applicability']['integrated_changes'][0]['commit'] = '0' * 40
            with self.assertRaises(ValueError):
                read(data=altered)
            altered = copy.deepcopy(contract)
            altered['source_base'] = '0' * 40
            with self.assertRaises(ValueError):
                read(data=altered)
            with self.assertRaises(ValueError):
                read(ref={'path': PATH, 'sha256': hashlib.sha256(raw).hexdigest()})
            other = root / 'other.toml'
            other.write_bytes(raw)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                read(ref={'path': 'other.toml', 'sha256': reference['sha256']})
            target.write_text(historical)
            self.assertEqual(read(), historical)
            self.assertEqual(read(old=True), historical)
            target.unlink()
            target.symlink_to(ROOT / PATH)
            with self.assertRaisesRegex(ValueError, 'escapes root'):
                read()

    def test_inventory_requires_every_historical_identity_and_original_locator(self):
        contract = json.loads((ROOT / 'docs/validation/reference-contracts.json').read_text())
        manifest = json.loads((ROOT / 'docs/validation/legacy-scenarios.json').read_text())
        row = next(r for r in manifest['collections'] if r['source']['path'] == PATH)
        manifest['collections'] = [row]
        manifest['cardinality'] = 59
        expected = [r for r in contracts.reference_population()['collections']
                    if r['source']['path'] == PATH]
        mapped = contracts.inventory(ROOT, manifest, expected, contract=contract)
        self.assertEqual([r['legacy_id'] for r in mapped.values()], row['legacy_ids'])
        self.assertTrue(all(r['status'] == 'NOT_REQUALIFIED' and r['source'] == row['source']
                            for r in mapped.values()))
        # Without the declaration the evolved ledger is never read: the exact
        # historical object is, and a copy without that history stays stale.
        self.assertEqual(contracts.inventory(ROOT, manifest, expected), mapped)
        with tempfile.TemporaryDirectory() as directory:
            copied = Path(directory) / PATH
            copied.parent.mkdir(parents=True)
            copied.write_bytes((ROOT / PATH).read_bytes())
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                contracts.inventory(Path(directory), manifest, expected)
        for ids in (row['legacy_ids'][:-1], row['legacy_ids'] + ['C-D93'],
                    list(reversed(row['legacy_ids']))):
            altered = copy.deepcopy(manifest)
            altered['collections'][0]['legacy_ids'] = ids
            with self.assertRaisesRegex(ValueError, 'legacy omission/reorder/rename'):
                contracts.inventory(ROOT, altered, expected, contract=contract)
        altered = copy.deepcopy(manifest)
        altered['collections'][0]['source']['sha256'] = hashlib.sha256((ROOT / PATH).read_bytes()).hexdigest()
        with self.assertRaisesRegex(ValueError, 'historical divergence locator altered'):
            contracts.inventory(ROOT, altered, expected, contract=contract)


class IntegratedLedgerApplicability(unittest.TestCase):
    def setUp(self):
        import reference_applicability as applicability
        self.applicability = applicability
        self.contract = json.loads((ROOT / 'docs/validation/reference-contracts.json').read_text())
        self.historical = subprocess.check_output(['git', 'show', BASE + ':' + PATH]).decode()
        self.previous = subprocess.check_output([
            'git', 'show', 'f02f5d698f83a338c74c2f20c430c708ea340e36:' + PATH]).decode()
        self.current = (ROOT / PATH).read_text()
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.target = self.root / PATH
        self.target.parent.mkdir(parents=True)
        self.reference = {'path': PATH, 'sha256': hashlib.sha256(self.historical.encode()).hexdigest()}

    def read(self, contract, *, historical=False):
        return self.applicability.read_divergence_reference(
            self.root, self.reference, contract, contracts.read_reference, historical=historical)

    def test_previous_integrated_declaration_and_bytes_remain_applicable(self):
        previous_contract = copy.deepcopy(self.contract)
        previous_contract['divergence_reference_applicability'] = copy.deepcopy(
            self.applicability.DIVERGENCE_BINDING)
        self.target.write_text(self.previous)
        self.assertEqual(self.read(previous_contract), self.previous)
        self.assertEqual(self.read(previous_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(self.contract)
        self.target.write_text(self.current)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(previous_contract)

    def test_integrated_ledger_requires_all_edits_and_exact_provenance(self):
        self.target.write_text(self.current)
        self.assertEqual(self.read(self.contract), self.current)
        self.assertEqual(self.read(self.contract, historical=True), self.historical)
        for identity in ('C-D36', 'C-D70', 'C-D85'):
            def entry(text):
                return '[[entry]]\n' + next(part for part in text.split('[[entry]]\n')
                                            if part.startswith('id = "' + identity + '"\n'))
            self.target.write_text(self.current.replace(entry(self.current), entry(self.previous), 1))
            with self.subTest(identity=identity), self.assertRaisesRegex(ValueError, 'stale reference'):
                self.read(self.contract)
        self.target.write_text(self.current)
        for index in range(3, 7):
            for field, width in (('commit', 40), ('sha256', 64)):
                altered = copy.deepcopy(self.contract)
                altered['divergence_reference_applicability']['integrated_changes'][index][field] = '0' * width
                with self.subTest(index=index, field=field), self.assertRaisesRegex(ValueError, 'unbound'):
                    self.read(altered)
        altered = copy.deepcopy(self.contract)
        altered['divergence_reference_applicability']['changed_ids'].remove('C-D36')
        with self.assertRaisesRegex(ValueError, 'unbound'):
            self.read(altered)

    def test_evidence_relocation_preserves_previous_binding_and_historical_projection(self):
        previous = subprocess.check_output([
            'git', 'show', '3d6d9408d9c1147d75be85325861bad88b5b387f:' + PATH]).decode()
        previous_contract = copy.deepcopy(self.contract)
        previous_contract['divergence_reference_applicability'] = copy.deepcopy(
            self.applicability.LEDGER_BINDING)
        self.target.write_text(previous)
        self.assertEqual(self.read(previous_contract), previous)
        self.assertEqual(self.read(previous_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(self.contract)
        self.target.write_text(self.current)
        self.assertEqual(self.read(self.contract), self.current)
        self.assertEqual(self.read(self.contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(previous_contract)

    def test_evidence_relocation_rejects_partial_edits_and_forged_provenance(self):
        relocations = (
            ('crates/rmux-client/src/attach_windows/input.rs',
             'crates/rmux-client/src/attach/windows/input.rs'),
            ('crates/rmux-server/src/handler_client_runtime.rs',
             'crates/rmux-server/src/handler/client_runtime_support.rs'),
            ('crates/rmux-server/src/handler_pane/attached_input/bracketed_paste.rs',
             'crates/rmux-server/src/handler/pane_support/pane_attached_input/bracketed_paste.rs'),
            ('crates/rmux-server/src/handler_pane/attached_input/terminal_response.rs',
             'crates/rmux-server/src/handler/pane_support/pane_attached_input/terminal_response.rs'),
        )
        self.target.write_text(self.current)
        self.assertEqual(self.read(self.contract), self.current)
        for old, path in relocations:
            self.assertEqual(self.current.count(path), 1)
            # Neither a partial revert nor an unrelated tracked file is valid.
            for invalid in (old, 'tests/cli_surface.rs'):
                self.target.write_text(self.current.replace(path, invalid, 1))
                with self.subTest(path=path, invalid=invalid), self.assertRaisesRegex(
                        ValueError, 'stale reference'):
                    self.read(self.contract)
        self.target.write_text(self.current)
        for field, width in (('commit', 40), ('sha256', 64)):
            altered = copy.deepcopy(self.contract)
            altered['divergence_reference_applicability']['integrated_changes'][-1][field] = '0' * width
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)

    def added_entry_ledger(self):
        """The v4 ledger at its trunk commit, with its exact v4 declaration."""
        text = subprocess.check_output([
            'git', 'show', '4ec27dc18fe3fdf75419cdc065f806dcb946c49b:' + PATH]).decode()
        contract = copy.deepcopy(self.contract)
        contract['divergence_reference_applicability'] = copy.deepcopy(
            self.applicability.ADDED_ENTRY_LEDGER_BINDING)
        return text, contract

    def test_added_entry_preserves_relocated_binding_and_historical_projection(self):
        added = self.applicability.ADDED_C_D94
        current, contract = self.added_entry_ledger()
        self.assertTrue(current.endswith(added))
        relocated = current[:-len(added)]
        relocated_contract = copy.deepcopy(self.contract)
        relocated_contract['divergence_reference_applicability'] = copy.deepcopy(
            self.applicability.RELOCATED_LEDGER_BINDING)
        self.target.write_text(relocated)
        self.assertEqual(self.read(relocated_contract), relocated)
        self.assertEqual(self.read(relocated_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(contract)
        self.target.write_text(current)
        self.assertEqual(self.read(contract), current)
        self.assertEqual(self.read(contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(relocated_contract)

    def test_added_entry_rejects_altered_entry_and_forged_provenance(self):
        added = self.applicability.ADDED_C_D94
        current, contract = self.added_entry_ledger()
        # Neither an edited appended entry nor an edit above it is accepted.
        for altered in (current.replace('id = "C-D94"', 'id = "C-D95"', 1),
                        current.replace('_product_divergence"]\nlimitations', '"]\nlimitations', 1),
                        current[:-len(added)] + added.replace('B|%1', 'A|%0', 1),
                        current.replace('id = "C-D93"', 'id = "C-D96"', 1)):
            self.assertNotEqual(altered, current)
            self.target.write_text(altered)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                self.read(contract)
        self.target.write_text(current)
        for field, width in (('commit', 40), ('sha256', 64)):
            altered = copy.deepcopy(contract)
            altered['divergence_reference_applicability']['integrated_changes'][-1][field] = '0' * width
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)
        for ids in (['C-D93'], ['C-D94', 'C-D93'], ['C-D93', 'C-D94', 'C-D95']):
            altered = copy.deepcopy(contract)
            altered['divergence_reference_applicability']['added_ids'] = ids
            with self.subTest(ids=ids), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)

    def edited_entry_ledger(self):
        """The v5 ledger at its trunk commit, with its exact v5 declaration."""
        text = subprocess.check_output([
            'git', 'show', 'e5b9a6f90912cea605c253514d7887aa7ee05205:' + PATH]).decode()
        contract = copy.deepcopy(self.contract)
        contract['divergence_reference_applicability'] = copy.deepcopy(
            self.applicability.EDITED_ENTRY_LEDGER_BINDING)
        return text, contract

    def test_edited_entry_preserves_added_binding_and_historical_projection(self):
        edited, edited_contract = self.edited_entry_ledger()
        current, prior = self.applicability.EDITED_C_D44
        self.assertEqual(edited.count(current), 1)
        added, added_contract = self.added_entry_ledger()
        self.assertEqual(edited.replace(current, prior, 1), added)
        self.target.write_text(added)
        self.assertEqual(self.read(added_contract), added)
        self.assertEqual(self.read(added_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(edited_contract)
        self.target.write_text(edited)
        self.assertEqual(self.read(edited_contract), edited)
        self.assertEqual(self.read(edited_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(added_contract)

    def test_edited_entry_rejects_partial_edits_and_forged_provenance(self):
        edited, edited_contract = self.edited_entry_ledger()
        current, prior = self.applicability.EDITED_C_D44
        added = self.applicability.ADDED_C_D94
        test = '_two_hooked_sessions_runs_both_hooks_product_divergence'
        limitation = 'runs both hooks",\n'
        race = 'races its control client exit against the first hook and usually runs only that hook'
        # Neither half of the C-D44 edit, its unmeasured systematic wording,
        # an edit of another entry nor of the appended C-D94 entry is accepted.
        for altered in (edited.replace(current, current.replace(test, '_two_hooked', 1), 1),
                        edited.replace(current, current.replace(limitation, '', 1), 1),
                        edited.replace(race, 'runs the first hook while its control client exits', 1),
                        edited.replace(current, current.replace('C-D44', 'C-D95', 1), 1),
                        edited[:-len(added)] + added.replace('B|%1', 'A|%0', 1),
                        edited.replace('id = "C-D93"', 'id = "C-D96"', 1)):
            self.assertNotEqual(altered, edited)
            self.target.write_text(altered)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                self.read(edited_contract)
        self.target.write_text(edited)
        for field, width in (('commit', 40), ('sha256', 64)):
            altered = copy.deepcopy(edited_contract)
            altered['divergence_reference_applicability']['integrated_changes'][-1][field] = '0' * width
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)
        for key, value in (('changed_ids', [i for i in edited_contract['divergence_reference_applicability']
                                            ['changed_ids'] if i != 'C-D44']),
                           ('added_ids', ['C-D93', 'C-D94', 'C-D44'])):
            altered = copy.deepcopy(edited_contract)
            altered['divergence_reference_applicability'][key] = value
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)

    def appended_entry_ledger(self):
        """The v6 ledger at its trunk commit, with its exact v6 declaration."""
        text = subprocess.check_output([
            'git', 'show', '7f6febd388e9e6b399d4ceccb9330ac207d799ea:' + PATH]).decode()
        contract = copy.deepcopy(self.contract)
        contract['divergence_reference_applicability'] = copy.deepcopy(
            self.applicability.APPENDED_ENTRY_LEDGER_BINDING)
        return text, contract

    def test_appended_entry_preserves_edited_binding_and_historical_projection(self):
        current, contract = self.appended_entry_ledger()
        appended = self.applicability.ADDED_C_D95
        edited, edited_contract = self.edited_entry_ledger()
        self.assertEqual(current, edited + appended)
        self.target.write_text(edited)
        self.assertEqual(self.read(edited_contract), edited)
        self.assertEqual(self.read(edited_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(contract)
        self.target.write_text(current)
        self.assertEqual(self.read(contract), current)
        self.assertEqual(self.read(contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(edited_contract)

    def test_appended_entry_rejects_altered_entry_and_forged_provenance(self):
        current, contract = self.appended_entry_ledger()
        appended = self.applicability.ADDED_C_D95
        edited, _ = self.edited_entry_ledger()
        test = 'insert_line_outside_region_clears_every_inserted_row_product_divergence'
        # Neither an edited C-D95 entry, a C-D95 without its test, nor an
        # edit of the C-D44 or C-D94 entries above it is accepted.
        for altered in (edited + appended.replace('id = "C-D95"', 'id = "C-D96"', 1),
                        edited + appended.replace(test, 'insert_line_outside_region', 1),
                        edited + appended.replace('L1 L2 L3 <blank> L5 L4', 'L1 L2 L3 <blank> <blank> L4', 1),
                        edited.replace('(12 of 16 measured runs)', '(16 of 16 measured runs)', 1) + appended,
                        edited.replace('B|%1', 'A|%0', 1) + appended,
                        edited + appended + '\n'):
            self.assertNotEqual(altered, current)
            self.target.write_text(altered)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                self.read(contract)
        self.target.write_text(current)
        for field, width in (('commit', 40), ('sha256', 64)):
            altered = copy.deepcopy(contract)
            altered['divergence_reference_applicability']['integrated_changes'][-1][field] = '0' * width
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)
        for ids in (['C-D93', 'C-D94'], ['C-D93', 'C-D95', 'C-D94'], ['C-D93', 'C-D94', 'C-D95', 'C-D96']):
            altered = copy.deepcopy(contract)
            altered['divergence_reference_applicability']['added_ids'] = ids
            with self.subTest(ids=ids), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)

    def macos_entry_ledger(self):
        """The v7 ledger at its trunk commit, with its exact v7 declaration."""
        text = subprocess.check_output([
            'git', 'show', 'ff064c20ef654e85cbbb93d299ceed891f4a811c:' + PATH]).decode()
        contract = copy.deepcopy(self.contract)
        contract['divergence_reference_applicability'] = copy.deepcopy(
            self.applicability.ADDED_MACOS_ENTRY_LEDGER_BINDING)
        return text, contract

    def test_macos_entry_preserves_appended_binding_and_historical_projection(self):
        current, contract = self.macos_entry_ledger()
        added = self.applicability.ADDED_C_D96
        self.assertTrue(current.endswith(added))
        appended, appended_contract = self.appended_entry_ledger()
        self.assertEqual(current[:-len(added)], appended)
        self.target.write_text(appended)
        self.assertEqual(self.read(appended_contract), appended)
        self.assertEqual(self.read(appended_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(contract)
        self.target.write_text(current)
        self.assertEqual(self.read(contract), current)
        self.assertEqual(self.read(contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(appended_contract)

    def test_macos_entry_rejects_altered_entry_and_forged_provenance(self):
        current, contract = self.macos_entry_ledger()
        added = self.applicability.ADDED_C_D96
        edited_c_d44 = self.applicability.EDITED_C_D44[0]
        # Neither an edited C-D96 (its id, one of its four lists, its host or
        # its pinning test) nor an edit of the C-D44, C-D95 or C-D94 entries
        # above it is accepted.
        for altered in (current.replace('id = "C-D96"', 'id = "C-D97"', 1),
                        current[:-len(added)] + added.replace('[a-c\\\\] against b gives 1',
                                                                   '[a-c\\\\] against b gives 0', 1),
                        current[:-len(added)] + added.replace('On macOS, ', 'On Linux, ', 1),
                        current[:-len(added)] + added.replace('_on_every_host"', '"', 1),
                        current.replace(edited_c_d44, edited_c_d44.replace('12 of 16', '16 of 16', 1), 1),
                        current.replace('L1 L2 L3 <blank> L5 L4', 'L1 L2 L3 <blank> <blank> L4', 1),
                        current.replace('id = "C-D94"', 'id = "C-D97"', 1)):
            self.assertNotEqual(altered, current)
            self.target.write_text(altered)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                self.read(contract)
        self.target.write_text(current)
        for field, width in (('commit', 40), ('sha256', 64)):
            altered = copy.deepcopy(contract)
            altered['divergence_reference_applicability']['integrated_changes'][-1][field] = '0' * width
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)
        for ids in (['C-D93', 'C-D94', 'C-D95'], ['C-D93', 'C-D94', 'C-D96', 'C-D95'],
                    ['C-D93', 'C-D94', 'C-D95', 'C-D96', 'C-D97']):
            altered = copy.deepcopy(contract)
            altered['divergence_reference_applicability']['added_ids'] = ids
            with self.subTest(ids=ids), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)

    def test_octal_entry_preserves_macos_binding_and_historical_projection(self):
        appended = self.applicability.ADDED_C_D97
        added, added_contract = self.macos_entry_ledger()
        self.assertEqual(self.current, added + appended)
        self.target.write_text(added)
        self.assertEqual(self.read(added_contract), added)
        self.assertEqual(self.read(added_contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(self.contract)
        self.target.write_text(self.current)
        self.assertEqual(self.read(self.contract), self.current)
        self.assertEqual(self.read(self.contract, historical=True), self.historical)
        with self.assertRaisesRegex(ValueError, 'stale reference'):
            self.read(added_contract)

    def test_octal_entry_rejects_altered_entry_and_forged_provenance(self):
        appended = self.applicability.ADDED_C_D97
        added, _ = self.macos_entry_ledger()
        test = 'octal_escapes_that_cannot_form_utf8_become_replacement_characters_product_divergence'
        # Neither an edited C-D97 entry, a C-D97 without its test, nor an
        # edit of the C-D44, C-D94 or C-D96 entries above it is accepted.
        for altered in (added + appended.replace('id = "C-D97"', 'id = "C-D98"', 1),
                        added + appended.replace(test, 'octal_escapes_that_cannot_form_utf8', 1),
                        added + appended.replace('types ff 61', 'types ef bf bd 61', 1),
                        added.replace('(12 of 16 measured runs)', '(16 of 16 measured runs)', 1) + appended,
                        added.replace('B|%1', 'A|%0', 1) + appended,
                        added.replace('On macOS, ', 'On Linux, ', 1) + appended,
                        added + appended + '\n'):
            self.assertNotEqual(altered, self.current)
            self.target.write_text(altered)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                self.read(self.contract)
        self.target.write_text(self.current)
        for field, width in (('commit', 40), ('sha256', 64)):
            altered = copy.deepcopy(self.contract)
            altered['divergence_reference_applicability']['integrated_changes'][-1][field] = '0' * width
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)
        for ids in (['C-D93', 'C-D94', 'C-D95', 'C-D96'], ['C-D93', 'C-D94', 'C-D95', 'C-D97', 'C-D96'],
                    ['C-D93', 'C-D94', 'C-D95', 'C-D96', 'C-D97', 'C-D98']):
            altered = copy.deepcopy(self.contract)
            altered['divergence_reference_applicability']['added_ids'] = ids
            with self.subTest(ids=ids), self.assertRaisesRegex(ValueError, 'unbound'):
                self.read(altered)


if __name__ == '__main__':
    unittest.main()
