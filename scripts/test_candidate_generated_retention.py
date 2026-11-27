"""Frozen regression: retain each generated version before its consumer starts."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class GeneratedRetention(unittest.TestCase):
    def test_versions_errors_and_consumer_boundary(self):
        for outcome in (0, 7, 'exception'):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                out = root / 'out'
                out.mkdir()
                (out / 'nested').mkdir()
                manifest = root / 'Cargo.toml'
                manifest.write_bytes(b'[package]')
                builds = module('candidate_identity_buildscripts')
                units = module('candidate_identity_units')
                guard = SimpleNamespace(
                    __file__=str(root / 'identity.py'), IdentityError=RuntimeError,
                    context=lambda: (root, 'head', root), source_identity=lambda *a: None,
                    support=lambda name: units if name == 'candidate_identity_units' else module(name),
                    file_hash=lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest(),
                    hashlib=hashlib, stamp=lambda st: (st.st_size, st.st_mtime_ns),
                    verify=lambda *a: {}, record_launch=lambda *a: None,
                    verify_built=lambda *a: None, jobserver=lambda: {})
                wrapper = root / 'build-script'
                wrapper.write_text(builds.shell(guard))
                producers = {}
                for version in ('A', 'B'):
                    payload = root / ('producer-' + version)
                    payload.write_bytes(version.encode())
                    receipt = root / ('producer-' + version + '.json')
                    receipt.write_text(json.dumps({'graph': {'input': {'manifest': str(manifest)}}}))
                    producers[version] = dict(path=str(payload), sha256=guard.file_hash(payload),
                                               receipt=str(receipt), kind='build-script')
                env = dict(OUT_DIR=str(out), CARGO_MANIFEST_DIR=str(root))
                def observed(*args, **kw):
                    return next(p for p in producers.values()
                                if kw.get('expected') == p['sha256'] or str(args[1]) == p['path'])
                def produce(version, status):
                    producer = producers[version]
                    binding = dict(head='head', source=str(root), payload=producer['path'],
                                   sha256=producer['sha256'], wrapper_sha256=guard.file_hash(wrapper))
                    (root / '.identity-build-script.json').write_text(json.dumps(binding))
                    def child(*args, **kw):
                        (out / 'nested' / 'generated.rs').write_bytes(version.encode())
                        (out / 'empty').write_bytes(b'')
                        if status == 'exception':
                            raise OSError('producer interrupted after output')
                        return status
                    with patch.dict(os.environ, env), patch.object(units, 'verify', side_effect=observed), \
                            patch.object(builds.subprocess, 'call', side_effect=child):
                        if status == 'exception':
                            with self.assertRaisesRegex(OSError, 'producer interrupted'):
                                builds.run(guard, [str(wrapper)])
                        else:
                            self.assertEqual(builds.run(guard, [str(wrapper)]), status)
                produce('A', 0)
                with patch.object(units, 'verify', side_effect=observed):
                    first = builds.verify_output(guard, env, root, 'head', root)
                first_receipt = Path(first['receipt'])
                frozen = first_receipt.read_bytes()
                record = json.loads(frozen)
                self.assertIn('retained_files', record, 'hash-only generated outputs lose version A')
                self.assertEqual(set(record['retained_files']), {'nested/generated.rs', 'empty'})
                for name, retained in record['retained_files'].items():
                    self.assertEqual(Path(retained['path']).read_bytes(), (out / name).read_bytes())
                    self.assertEqual(guard.file_hash(retained['path']), record['files'][name])
                    self.assertFalse(Path(retained['path']).is_relative_to(out))
                # A modified live alias must be rejected before any next consumer.
                generated = out / 'nested' / 'generated.rs'
                generated.write_bytes(b'foreign')
                with patch.object(units, 'verify', side_effect=observed):
                    with self.assertRaises(RuntimeError):
                        builds.verify_output(guard, env, root, 'head', root)
                generated.write_bytes(b'A')
                produce('B', outcome)
                with patch.object(units, 'verify', side_effect=observed):
                    builds.verify_receipt(guard, first_receipt, root, 'head', root)
                    if outcome == 0:
                        second = builds.verify_output(guard, env, root, 'head', root)
                        self.assertNotEqual(second['receipt'], first['receipt'])
                        self.assertEqual(second['producer'], producers['B'])
                    else:
                        with self.assertRaises(RuntimeError):
                            builds.verify_output(guard, env, root, 'head', root)
                        errors = list((root / 'identity-generator-errors').glob('*.json'))
                        self.assertEqual(len(errors), 1, 'failed output must also survive alias reuse')
                        failed = json.loads(errors[0].read_text())
                        self.assertEqual(failed['producer'], producers['B'])
                        saved = failed['retained_files']['nested/generated.rs']
                        self.assertEqual(Path(saved['path']).read_bytes(), b'B')
                self.assertEqual(first_receipt.read_bytes(), frozen)
                self.assertEqual(Path(record['retained_files']['nested/generated.rs']['path']).read_bytes(), b'A')
                retained = Path(record['retained_files']['nested/generated.rs']['path'])
                retained.write_bytes(b'changed retained bytes')
                with patch.object(units, 'verify', side_effect=observed):
                    with self.assertRaises(RuntimeError):
                        builds.verify_receipt(guard, first_receipt, root, 'head', root)


if __name__ == '__main__':
    unittest.main()
