"""One bounded acceptance for the SBOM and reference-contract TOML consumers."""
from pathlib import Path
import copy
import hashlib
import json
import tempfile
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
PROBE = r'''
import hashlib, json, sys
from pathlib import Path
import copy
import hashlib
import json
import tempfile
if sys.argv[2] == 'hidden':
    sys.modules['tomllib'] = None
root = Path(sys.argv[1])
if sys.argv[2] == 'reference':
    import tomllib
sys.path.insert(0, str(root / 'scripts/packaging'))
sys.path.insert(0, str(root / 'scripts/validation'))
import sbom
import contracts

parsed = []
def record(frame, event, value):
    if event == 'return' and frame.f_code.co_name == 'loads' and isinstance(value, dict):
        parsed.append(value)

def graph(raw):
    if sys.argv[2] == 'reference':
        return tomllib.loads(raw.decode())
    source = {'commit': 'a' * 40, 'tree': 'b' * 40}
    return sbom.build_graph({'payload_source': source}, {
        'metadata': {'packages': []}, 'auxiliary': {'observations': []},
        'units': {'fingerprint_records': []}, 'lock': raw,
        'root_manifest': {**source, 'entries': [
            {'path': 'Cargo.lock', 'sha256': hashlib.sha256(raw).hexdigest()}]}})

def identities(text):
    if sys.argv[2] == 'reference':
        return [row['id'] for row in tomllib.loads(text)['entry']]
    return contracts.source_ids('divergences', text)

sys.setprofile(record)
graph((root / 'Cargo.lock').read_bytes())
ids = identities((root / 'tests/reference/tmux_compat/divergences.toml').read_text())
sys.setprofile(None)
assert len(parsed) == 2, parsed
rejections = []
# A single trailing inline-table comma is valid in TOML 1.1 (Python 3.15).
# A doubled comma stays invalid for both the reference and the subset reader.
for text in ('a=1\na=2\n', '[a]\nx=1\n[a]\n', 'a=["unfinished]\n',
             'a={b=1,,}\n'):
    for consume in (lambda: graph(text.encode()),
                    lambda: identities(text)):
        try:
            consume()
        except ValueError:
            rejections.append(True)
        else:
            rejections.append(False)
assert all(rejections), rejections
print(json.dumps({'parsed': parsed, 'ids': ids, 'rejections': rejections}, sort_keys=True))
'''


class SbomContractsPythonFloor(unittest.TestCase):
    def probe(self, interpreter, mode):
        result = subprocess.run([str(interpreter), '-I', '-B', '-c', PROBE,
                                 str(ROOT), mode], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_real_inputs_and_rejections_match_without_tomllib(self):
        reference_python = sys.executable
        if sys.version_info < (3, 11) and sys.platform == 'darwin':
            reference_python = '/opt/homebrew/bin/python3'
        baseline = self.probe(reference_python, 'reference')
        self.assertEqual(self.probe(sys.executable, 'normal'), baseline)
        self.assertEqual(self.probe(sys.executable, 'hidden'), baseline)
        if sys.platform == 'darwin':
            self.assertEqual(self.probe('/usr/bin/python3', 'hidden'), baseline)


    def test_only_exact_packaging_pin_delta_is_applicable(self):
        sys.path.insert(0, str(ROOT / 'scripts/validation'))
        import contracts
        import reference_applicability

        # The real consumer reads packaging and every other reference.
        self.assertEqual(contracts.validate()['status'], 'PASS_REFERENCE_CONTRACT_ONLY')
        path = 'scripts/packaging/rmux-package-layout.json'
        historical = '7853088e4e54068a137f64c26605f87411397985b6a15b58545765b6685ee569'
        current = '102c8ba0867791364613b8c05a10b8937451342da83edc2e67394cdfa2c0013d'
        old_pin = b'9d065b2f82a341121461c9a918f3c9cffe5e793fcdaeac2ae44857d8250ae2ce'
        new_pin = b'0b6decf7e4b0461ee2af0e552112333114236ead0a497c3b5713c979338d7f40'
        contract = json.loads((ROOT / 'docs/validation/reference-contracts.json').read_text())
        reference = {'path': path, 'sha256': historical}
        # The descriptor kept evolving after this delta; derive the accepted
        # bytes from the frozen historical object and the 18 declared pins.
        previous = contracts.frozen_source_bytes(ROOT, reference)
        self.assertEqual(previous.count(old_pin), 18)
        raw = previous.replace(old_pin, new_pin)
        self.assertEqual(hashlib.sha256(raw).hexdigest(), current)
        self.assertEqual(raw.count(new_pin), 18)
        read = reference_applicability.read_packaging_reference
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / path
            target.parent.mkdir(parents=True)
            target.write_bytes(raw)
            self.assertEqual(read(root, reference, contract, contracts.read_reference), raw.decode())
            # Existing callers retain strict SHA validation, even for this path.
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                contracts.read_reference(root, reference)
            for altered in (raw + b' ', raw.replace(b'"version": 1', b'"version": 2', 1),
                            json.dumps(json.loads(raw)).encode()):
                target.write_bytes(altered)
                with self.assertRaises(ValueError):
                    read(root, reference, contract, contracts.read_reference)
            # Each of the 18 approved leaves must be present; no partial delta.
            parts = raw.split(new_pin)
            for index in range(18):
                altered = new_pin.join(parts[:index + 1]) + old_pin + new_pin.join(parts[index + 1:])
                target.write_bytes(altered)
                with self.assertRaises(ValueError):
                    read(root, reference, contract, contracts.read_reference)
            target.write_bytes(raw)
            for declaration in (None, {}, {'current_sha256': current}):
                altered = copy.deepcopy(contract)
                altered['packaging_reference_applicability'] = declaration
                with self.assertRaises(ValueError):
                    read(root, reference, altered, contracts.read_reference)
            altered = copy.deepcopy(contract)
            altered['packaging_reference_applicability']['current_sha256'] = '0' * 64
            with self.assertRaises(ValueError):
                read(root, reference, altered, contracts.read_reference)
            with self.assertRaises(ValueError):
                read(root, {'path': path, 'sha256': current}, contract, contracts.read_reference)
            target.write_bytes(previous)
            self.assertEqual(read(root, reference, contract, contracts.read_reference), previous.decode())
            # Applicability never extends to unrelated references.
            other = root / 'other.json'
            other.write_bytes(raw)
            with self.assertRaisesRegex(ValueError, 'stale reference'):
                read(root, {'path': 'other.json', 'sha256': historical}, contract, contracts.read_reference)
            target.unlink()
            target.symlink_to(ROOT / path)
            with self.assertRaisesRegex(ValueError, 'escapes root'):
                read(root, reference, contract, contracts.read_reference)


if __name__ == '__main__':
    unittest.main()
