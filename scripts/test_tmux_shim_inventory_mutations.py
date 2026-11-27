"""Source drift, broken joins and false qualification must fail closed."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import tmux_shim_extract as extract
import tmux_shim_inventory as inventory

ROOT = Path(__file__).resolve().parents[1]


class SourceMutations(unittest.TestCase):
    def setUp(self):
        audit = ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix='shim-fixture-', dir=audit)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        self.put('src/tmux_shim.rs', 'pub fn apply_tmux_shim() {}\n')
        self.put('src/unmarked.rs', 'pub fn ordinary() {}\n')
        self.put('tests/local.rs', '#[test]\nfn keeps_product_divergence() {}\n')
        self.put(extract.LEDGER, '''schema = 1
[policy]
oracle = "tmux 3.7b"
unlisted_divergences_are_bugs = true
[[entry]]
id = "C-D1"
summary = "Synthetic fixture only"
inventory = "bounded"
tests = ["tests/local.rs::keeps_product_divergence"]
''')
        self.put(extract.CONTRACT, json.dumps({'families': [
            {'id': 'conformance', 'routes': ['CLI', 'control-mode']}]}))
        self.put(extract.LEGACY, json.dumps({'collections': [{
            'id': 'old-ledger', 'source': {'path': extract.LEDGER},
            'legacy_ids': ['C-D1'], 'retained_status': 'HISTORICAL_SOURCE_ONLY_NO_NEW_VERDICT'}]}))
        self.put(extract.PIN, 'synthetic fixture, not an oracle\n')
        subprocess.run(['git', '-C', str(self.root), 'add', '.'], check=True)
        self.refresh()

    def put(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def refresh(self):
        self.catalogue = inventory.generate(self.root)
        self.put(inventory.CATALOGUE, inventory.encoded(self.catalogue).decode())
        self.put(inventory.MANIFEST, inventory.encoded(
            inventory.manifest(self.root, self.catalogue)).decode())

    def test_valid_source_ledger_manifest_join(self):
        self.assertEqual(inventory.check(self.root)['divergences'], 1)
        row = self.catalogue['divergences'][0]
        self.assertEqual(row['historical_manifest_id'], 'old-ledger/C-D1')
        self.assertEqual(row['canary']['candidate_tests'],
                         ['tests/local.rs::keeps_product_divergence'])
        self.assertEqual([r['route'] for r in row['route_matrix']], ['CLI', 'control-mode'])

    def test_new_real_shim_is_detected_from_the_git_index(self):
        paths = ('src/new.rs',
                 'crates/rmux-server/src/handler_overlay_tests/future_popup.rs',
                 'crates/rmux-server/src/pane_io/tests/future_graphics.rs')
        for path in paths:
            with self.subTest(path=path):
                self.put(path, 'pub fn new_tmux_shim() {}\n')
                subprocess.run(['git', '-C', str(self.root), 'add', path], check=True)
                with self.assertRaisesRegex(ValueError, 'omitted source: ' + path):
                    inventory.validate(self.root, self.catalogue)
                self.refresh()
                inventory.check(self.root)
                (self.root / path).unlink()
                subprocess.run(['git', '-C', str(self.root), 'rm', '--cached', '-q',
                                path], check=True)
                with self.assertRaisesRegex(ValueError, 'orphan source: ' + path):
                    inventory.validate(self.root, self.catalogue)
                self.refresh()
                inventory.check(self.root)

    def run_cli(self, command):
        return subprocess.run(
            [sys.executable, str(ROOT / 'scripts/tmux_shim_inventory.py'), command,
             '--root', str(self.root)], capture_output=True, text=True)

    def extract_with_sentinels(self):
        for path in (inventory.CATALOGUE, inventory.MANIFEST):
            self.put(path, 'sentinel ' + path + '\n')
        return self.run_cli('extract')

    def test_check_and_extract_name_every_untracked_source(self):
        paths = ('tests/untracked b.rs', 'crates/a/src/untracked.rs')
        for path in paths:
            self.put(path, 'pub fn untracked_tmux_shim() {}\n')
        self.put('src/untracked.RS', 'not in the discover filter\n')
        expected = 'tmux shim inventory: untracked source: ' + ', '.join(sorted(paths)) + '\n'
        for command in ('check', 'extract'):
            with self.subTest(command=command):
                result = self.run_cli(command)
                self.assertEqual((result.returncode, result.stderr), (1, expected),
                                 result.stdout)

    def test_extract_refuses_any_untracked_repository_source(self):
        paths = ('crates/rmux-server/src/untracked_tmux_shim.rs', 'src/untracked.rs',
                 'tests/untracked.rs', 'xtask/src/untracked.rs',
                 'scripts/fuzz/untracked.rs', 'build.rs', 'new dir/with space.rs')
        for path in paths:
            with self.subTest(path=path):
                self.put(path, 'pub fn untracked_tmux_shim() {}\n')
                self.put(path[:-3] + '.txt', 'not rust\n')
                result = self.extract_with_sentinels()
                (self.root / path).unlink()
                (self.root / (path[:-3] + '.txt')).unlink()
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertEqual(result.stderr,
                                 'tmux shim inventory: untracked source: ' + path + '\n')
                for written in (inventory.CATALOGUE, inventory.MANIFEST):
                    self.assertEqual((self.root / written).read_text(),
                                     'sentinel ' + written + '\n')

    def test_extract_ignores_an_ignored_rust_source(self):
        self.put('.git/info/exclude', 'target/\n')
        self.put('target/debug/build/out.rs', 'pub fn generated_tmux_shim() {}\n')
        result = self.extract_with_sentinels()
        self.assertEqual(result.returncode, 0, result.stderr)
        inventory.check(self.root)

    def test_even_unmarked_source_changes_require_catalogue_review(self):
        self.put('src/unmarked.rs', 'pub fn ordinary() { /* changed */ }\n')
        with self.assertRaisesRegex(ValueError, 'inventory drift'):
            inventory.validate(self.root, self.catalogue)

    def test_commented_or_literal_function_cannot_satisfy_a_ledger_reference(self):
        for text in ['// fn keeps_product_divergence() {}\n',
                     '/* outer /* inner */ fn keeps_product_divergence() {} */\n',
                     'const S: &str = r##"fn keeps_product_divergence() {}"##;\n',
                     'const S: &str = "fn keeps_product_divergence() {}";\n']:
            with self.subTest(text=text):
                self.put('tests/local.rs', text)
                self.refresh()
                with self.assertRaisesRegex(ValueError, 'orphan ledger reference'):
                    inventory.check(self.root)

    def test_missing_local_ledger_path_is_not_external_evidence(self):
        (self.root / 'tests/local.rs').unlink()
        subprocess.run(['git', '-C', str(self.root), 'rm', '--cached', '-q',
                        'tests/local.rs'], check=True)
        self.refresh()
        with self.assertRaisesRegex(ValueError, 'orphan ledger reference'):
            inventory.check(self.root)

    def test_divergence_omission_duplicate_and_false_bug_claim_are_rejected(self):
        mutations = [lambda d: d['divergences'].clear(),
                     lambda d: d['divergences'].append(copy.deepcopy(d['divergences'][0])),
                     lambda d: d['divergences'][0].update(classification='bugtmux-confirme')]
        for mutation in mutations:
            altered = copy.deepcopy(self.catalogue)
            mutation(altered)
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                inventory.validate(self.root, altered)

    def test_removed_route_or_canary_and_forged_manifest_are_rejected(self):
        for field in ['route_matrix', 'canary']:
            altered = copy.deepcopy(self.catalogue)
            altered['divergences'][0][field] = []
            with self.subTest(field=field), self.assertRaises(ValueError):
                inventory.validate(self.root, altered)
        data = json.loads((self.root / inventory.MANIFEST).read_text())
        data['runtime_credit'] = True
        self.put(inventory.MANIFEST, json.dumps(data))
        with self.assertRaisesRegex(ValueError, 'stale shim manifest'):
            inventory.check(self.root)

    def test_escaping_source_symlink_is_rejected(self):
        target = self.root / 'src/tmux_shim.rs'
        target.unlink()
        target.symlink_to(ROOT / 'scripts/tmux_shim_inventory.py')
        with self.assertRaisesRegex(ValueError, 'source symlink'):
            inventory.validate(self.root, self.catalogue)


if __name__ == '__main__':
    unittest.main()
