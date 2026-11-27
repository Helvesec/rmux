"""Equal empty compiler artifacts must retain each producer's provenance."""
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class EmptyMetadataReceipts(unittest.TestCase):
    def test_two_empty_outputs_keep_independent_producer_graphs(self):
        spec = importlib.util.spec_from_file_location(
            'units', Path(__file__).with_name('candidate_identity_units.py'))
        units = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(units)
        retention_spec = importlib.util.spec_from_file_location(
            'retention', Path(__file__).with_name('candidate_identity_retention.py'))
        retention = importlib.util.module_from_spec(retention_spec)
        retention_spec.loader.exec_module(retention)
        manifest_spec = importlib.util.spec_from_file_location(
            'manifests', Path(__file__).with_name('candidate_identity_manifest.py'))
        manifests = importlib.util.module_from_spec(manifest_spec)
        manifest_spec.loader.exec_module(manifests)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'Cargo.lock').write_bytes(b'locked fixture')
            compiler = Path(sys.executable).resolve()
            guard = SimpleNamespace(
                file_hash=lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest(),
                stamp=lambda st: (st.st_size, st.st_mtime_ns), IdentityError=RuntimeError,
                support=lambda name: {'candidate_identity_retention': retention,
                                      'candidate_identity_manifest': manifests}[name])
            qualified = []
            for name in ('render_pane', 'budget'):
                path = root / ('lib' + name + '.rmeta')
                path.write_bytes(b'')
                graph = dict(head='head', source=str(root), dependencies=[],
                             input={'input': name}, lock_sha256=guard.file_hash(root / 'Cargo.lock'),
                             compiler=str(compiler), compiler_sha256=guard.file_hash(compiler))
                unit = dict(outputs=[path], stamps={}, kind='library',
                            graph=graph, graph_sha256=units.digest(graph))
                with patch.object(units, 'check_depinfo'):
                    units.after(guard, unit, root, 'head', root)
                qualified.append((path, unit))
            for path, unit in qualified:
                actual = units.verify(guard, path, root, 'head', root)
                self.assertEqual(actual['graph_sha256'], unit['graph_sha256'])
                record = json.loads(Path(actual['receipt']).read_text())
                self.assertEqual(record['compiler_output'], str(path))
                self.assertEqual(record['path'], actual['path'])
                self.assertEqual(Path(actual['path']).read_bytes(), b'')
                self.assertEqual(record['graph'], unit['graph'])
            self.assertNotEqual(*(units.verify(guard, path, root, 'head', root)['receipt']
                                  for path, unit in qualified))


if __name__ == '__main__':
    unittest.main()
