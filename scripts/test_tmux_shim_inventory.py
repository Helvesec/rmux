"""Bidirectional completeness regressions against actual repository sources."""
import copy
import hashlib
from pathlib import Path
import subprocess
import unittest

import tmux_shim_extract as extract
import tmux_shim_inventory as inventory

ROOT = Path(__file__).resolve().parents[1]


class ShimInventory(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.catalogue = inventory.load_catalogue(ROOT)

    def test_repository_is_complete(self):
        inventory.validate(ROOT, self.catalogue)

    def test_current_rust_population_and_source_links_match_disk(self):
        tracked = subprocess.check_output(
            ['git', '-C', str(ROOT), 'ls-files', '-z']).decode().split('\0')
        population = {row['path']: row['sha256'] for row in
                      self.catalogue['rust_and_ledger_population']}
        for path in sorted(p for p in tracked if p.endswith('.rs')):
            with self.subTest(population=path):
                self.assertEqual(population.get(path), hashlib.sha256(
                    (ROOT / path).read_bytes()).hexdigest())
        links = self.catalogue['sources'] + [
            row['source'] for row in self.catalogue['ledger_references']
            if 'source' in row]
        for row in links:
            with self.subTest(source=row['path']):
                self.assertEqual(row['sha256'], hashlib.sha256(
                    (ROOT / row['path']).read_bytes()).hexdigest())

    def test_composed_overlay_and_graphics_sources_are_inventoried(self):
        population = {row['path']: row for row in
                      self.catalogue['rust_and_ledger_population']}
        sources = {row['path']: row for row in self.catalogue['sources']}
        for path in ('crates/rmux-server/src/handler_overlay_tests/popup_status.rs',
                     'crates/rmux-server/src/pane_io/tests/graphics_restore.rs'):
            with self.subTest(path=path):
                self.assertEqual(population.get(path), extract.reference(ROOT, path))
                self.assertIn(path, sources)

    def test_c_d70_normal_output_canary_resolves_exactly(self):
        locator = ('crates/rmux-core/src/screen/tests.rs::'
                   'reflow_overflow_capacity_freezes_on_normal_output_after_growth'
                   '_product_divergence')
        entry = next(row for row in extract.ledger(ROOT) if row['id'] == 'C-D70')
        self.assertIn(locator, entry['tests'])
        resolved = next(row for row in extract.local_references(ROOT, [entry])
                        if row['locator'] == locator)
        self.assertEqual(resolved['status'], 'source-only-not-executed')

    def test_real_shim_cannot_be_omitted(self):
        altered = copy.deepcopy(self.catalogue)
        real = next(row for row in altered['sources']
                    if row['path'] == 'crates/rmux-server/src/tmux_shim.rs')
        altered['sources'].remove(real)
        with self.assertRaisesRegex(ValueError, 'omitted source'):
            inventory.validate(ROOT, altered)

    def test_orphan_source_is_rejected(self):
        altered = copy.deepcopy(self.catalogue)
        altered['sources'].append(dict(altered['sources'][0], path='src/absent_tmux_shim.rs'))
        with self.assertRaisesRegex(ValueError, 'orphan source'):
            inventory.validate(ROOT, altered)

    def test_duplicate_source_is_rejected(self):
        altered = copy.deepcopy(self.catalogue)
        altered['sources'].append(copy.deepcopy(altered['sources'][0]))
        with self.assertRaisesRegex(ValueError, 'duplicate source'):
            inventory.validate(ROOT, altered)


if __name__ == '__main__':
    unittest.main()
