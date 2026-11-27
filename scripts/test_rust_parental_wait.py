"""Frozen F7 acceptance: the real Rust helper must own native child waits.

First action (Windows host, after minimal-fixture preflight):
PythonABS -I -S -B scripts/test_rust_parental_wait.py
RMUX_F7_FIXTURE, RMUX_F7_CHILD and RMUX_F7_AUDIT are private native targets.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import unittest


class RustParentalWait(unittest.TestCase):
    def test_three_rust_entries_preserve_output_and_observe_original_wait(self):
        root = Path(os.environ['RMUX_F7_AUDIT'])
        parent = Path(os.environ['RMUX_F7_FIXTURE'])
        child = Path(os.environ['RMUX_F7_CHILD'])
        image_hash = hashlib.sha256(child.read_bytes()).hexdigest()
        head = os.environ['RMUX_IDENTITY_HEAD']
        for entry in ('consumer-prepare', 'consumer-check', 'cargo-consumer'):
            for code in (0, 7):
                with self.subTest(entry=entry, exit=code):
                    before = set(root.rglob('wait.json'))
                    result = subprocess.run([str(parent), str(child), str(code), entry],
                                            capture_output=True, check=False)
                    self.assertEqual(result.returncode, code)
                    self.assertEqual(result.stdout, f'stdout:{code}'.encode())
                    self.assertEqual(result.stderr, f'stderr:{code}'.encode())
                    receipts = [json.loads(p.read_text()) for p in set(root.rglob('wait.json')) - before]
                    receipts = [r for r in receipts if r.get('owner') == 'rust-consumer']
                    self.assertEqual(len(receipts), 1, 'Rust child lacks an original-handle wait receipt')
                    receipt = receipts[0]
                    self.assertEqual(receipt['entry'], entry)
                    self.assertEqual(receipt['head'], head)
                    self.assertEqual(receipt['case'], os.environ['RMUX_F7_CASE'])
                    self.assertEqual(receipt['executable_sha256'], image_hash)
                    self.assertEqual(Path(receipt['child']['image']), child)
                    self.assertGreater(receipt['handle'], 0)
                    self.assertGreater(receipt['child']['pid'], 0)
                    self.assertGreater(receipt['child']['birth_100ns'], 0)
                    self.assertGreater(receipt['parent']['birth_100ns'], 0)
                    self.assertEqual(Path(receipt['parent']['image']), parent)
                    self.assertLessEqual(receipt['before_ns'], receipt['child']['birth_ns'])
                    self.assertLessEqual(receipt['child']['birth_ns'], receipt['wait_started_ns'])
                    self.assertLessEqual(receipt['child']['end_ns'], receipt['wait_finished_ns'])
                    self.assertLessEqual(receipt['wait_started_ns'], receipt['wait_finished_ns'])
                    self.assertEqual(receipt['wait_api'], 'WaitForSingleObject')
                    self.assertEqual(receipt['wait_return'], 0)
                    self.assertEqual(receipt['exit'], code)
                    self.assertEqual(receipt['rust_wait_exit'], code)


if __name__ == '__main__':
    unittest.main(verbosity=2)
