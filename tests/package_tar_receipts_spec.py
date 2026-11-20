#!/usr/bin/env python3
"""Exercise the standalone tar verifier with inert Python-created archives.

These fixtures prove checker behavior only; they are not native packaging
receipts. Every positive invocation retains all five acquired checks.
"""
import copy
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
VERIFIER = ROOT / 'tests/package_tar_repro_spec.py'


class AssemblyReceipts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='tar-receipts-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        files = [('LICENSE', b'license\n'), ('LICENSE-APACHE', b'apache\n'),
                 ('LICENSE-MIT', b'mit\n')]
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            directory = tarfile.TarInfo('fixture')
            directory.type = tarfile.DIRTYPE
            directory.mode = 0o755
            directory.mtime = 1234567890
            archive.addfile(directory)
            for name, data in files:
                member = tarfile.TarInfo('fixture/' + name)
                member.mode = 0o644
                member.mtime = 1234567890
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        compressed = gzip.compress(raw.getvalue(), mtime=0)
        self.left, self.right = self.root / 'left.tar.gz', self.root / 'right.tar.gz'
        for path in (self.left, self.right):
            path.write_bytes(compressed)
        tools = [dict(name='tar', path='/inert/tar', bytes=1, sha256='a' * 64,
                      version='inert fixture; never executed')]
        contract = dict(root_name='fixture', source_date_epoch=1234567890, tools=tools,
                        files=[dict(path=n, bytes=len(d), mode=0o644,
                                    sha256=hashlib.sha256(d).hexdigest()) for n, d in files])
        self.contract = self.root / 'contract.json'
        self.contract.write_text(json.dumps(contract))
        self.receipt = dict(tools=tools, assemblies=[
            dict(rc=0, archive=dict(path=str(p), bytes=len(compressed),
                                   sha256=hashlib.sha256(compressed).hexdigest()))
            for p in (self.left, self.right)])

    def verify(self, receipt, accepted):
        path = self.root / 'receipt.json'
        path.write_text(json.dumps(receipt))
        with tempfile.TemporaryDirectory(dir=self.root) as proofs:
            result = subprocess.run([sys.executable, '-B', str(VERIFIER),
                '--contract', str(self.contract), '--left', str(self.left),
                '--right', str(self.right), '--receipt', str(path), '--proofs', proofs],
                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
        self.assertIn('Ran 5 tests', result.stderr)

    def test_two_successful_receipts_pass_in_either_order(self):
        self.verify(self.receipt, True)
        reversed_receipt = copy.deepcopy(self.receipt)
        reversed_receipt['assemblies'].reverse()
        self.verify(reversed_receipt, True)

    def test_zero_one_and_three_receipts_fail(self):
        for count in (0, 1, 3):
            with self.subTest(count=count):
                receipt = copy.deepcopy(self.receipt)
                receipt['assemblies'] = (receipt['assemblies'] * 2)[:count]
                self.verify(receipt, False)

    def test_duplicate_and_foreign_output_fail(self):
        for kind in ('duplicate', 'foreign', 'alias'):
            with self.subTest(kind=kind):
                receipt = copy.deepcopy(self.receipt)
                if kind == 'duplicate':
                    receipt['assemblies'][1] = copy.deepcopy(receipt['assemblies'][0])
                else:
                    path = self.root / (kind + '.tar.gz')
                    if kind == 'alias':
                        path.symlink_to(self.left)
                    else:
                        path.write_bytes(self.left.read_bytes())
                    receipt['assemblies'][1]['archive']['path'] = str(path)
                self.verify(receipt, False)

    def test_failed_or_malformed_receipt_fails(self):
        for value in (1, -1, '0', False, None):
            with self.subTest(rc=value):
                receipt = copy.deepcopy(self.receipt)
                receipt['assemblies'][0]['rc'] = value
                self.verify(receipt, False)
        for rows in (None, {}, 'two'):
            with self.subTest(rows=rows):
                receipt = copy.deepcopy(self.receipt)
                receipt['assemblies'] = rows
                self.verify(receipt, False)

    def test_wrong_digest_or_size_binding_fails(self):
        for side in (0, 1):
            for field, value in (('sha256', '0' * 64), ('bytes', 1)):
                with self.subTest(side=side, field=field):
                    receipt = copy.deepcopy(self.receipt)
                    receipt['assemblies'][side]['archive'][field] = value
                    self.verify(receipt, False)


if __name__ == '__main__':
    unittest.main(verbosity=2)
