"""Acceptance: publish the implemented Web combinations and explicit refusals."""
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


class WebCompatibilityAcceptance(unittest.TestCase):
    def test_real_daemon_provider_combination_is_published(self):
        path = ROOT / '.github/release/web-compatibility-matrix.json'
        self.assertTrue(path.is_file(),
                        'missing published full × localhost-run Web compatibility cell')
        matrix = json.loads(path.read_text())
        cells = {(cell['artifact'], cell['network']): cell
                 for cell in matrix['artifact_network']}
        supported = cells['full', 'localhost-run']
        self.assertEqual(supported['status'], 'supported')
        self.assertIn('crates/rmux-server/tunnels/localhost-run.toml', supported['support'])
        refused = cells['no-web', 'localhost-run']
        self.assertEqual(refused['status'], 'unsupported')
        self.assertTrue(refused['reason'])
        self.assertEqual({pair['status'] for pair in matrix['frontend_daemon']}, {'supported'})
        self.assertNotIn('cloudflare', {item['id'] for item in matrix['providers']})


if __name__ == '__main__':
    unittest.main()
