"""A build script's first compiler probe must not need its future output seal."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class BuildProbe(unittest.TestCase):
    def probe_during_first_producer(self, mutate=None):
        units = module('candidate_identity_units')
        builds = module('candidate_identity_buildscripts')
        retention = module('candidate_identity_retention')
        manifests = module('candidate_identity_manifest')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            manifest = root / 'Cargo.toml'
            manifest.write_text('[package]\nname="probe"\nversion="0.0.0"\n')
            (root / 'Cargo.lock').write_bytes(b'locked')
            code = root / 'probe.rs'
            code.write_text('pub fn probe() {}\n')
            output = root / 'out'
            output.mkdir()
            payload = root / 'producer.payload'
            payload.write_bytes(b'observed producer')
            wrapper = root / 'build-script'
            wrapper.write_text(builds.shell(SimpleNamespace(__file__=str(root / 'identity.py'))))
            file_hash = lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest()
            producer = dict(path=str(payload), sha256=file_hash(payload), kind='build-script')
            receipt = root / 'producer.json'
            receipt.write_text(json.dumps({'graph': {'input': {'manifest': str(manifest)}}}))
            producer['receipt'] = str(receipt)
            binding = dict(head='head', source=str(root), payload=str(payload),
                           sha256=file_hash(payload), wrapper_sha256=file_hash(wrapper))
            (root / '.identity-build-script.json').write_text(json.dumps(binding))
            launches = []
            guard = SimpleNamespace(
                __file__=str(root / 'identity.py'), IdentityError=RuntimeError,
                context=lambda: (root, 'head', root), source_identity=lambda *a: None,
                support=lambda name: {'candidate_identity_units': units,
                                      'candidate_identity_buildscripts': builds,
                                      'candidate_identity_manifest': manifests,
                                      'candidate_identity_retention': retention}[name],
                file_hash=file_hash, hashlib=hashlib, tracked_input=lambda *a: None,
                verify=lambda *a: dict(path=str(payload), sha256=file_hash(payload)),
                verify_built=lambda *a: None,
                record_launch=lambda records: launches.extend(records), jobserver=lambda: {},
                stamp=lambda st: (st.st_size, st.st_mtime_ns))
            env = dict(OUT_DIR=str(output), CARGO_MANIFEST_DIR=str(root),
                       RMUX_IDENTITY_RUSTC=str(Path(sys.executable).resolve()))
            args = [env['RMUX_IDENTITY_RUSTC'], '--crate-name=probe', '--crate-type=lib',
                    '--emit=dep-info,metadata', '--out-dir', str(output / 'probe'), str(code)]

            def child(*a, **kw):
                self.assertEqual(len(launches), 1, 'producer must be observed before its child')
                active = dict(env, **kw.get('env', {}))
                actual_args = list(args)
                if mutate:
                    mutate(active, actual_args)
                unit = units.before(guard, actual_args, root, 'head', root, active)
                self.assertEqual(unit['graph']['generation']['producer'], producer)
                return 0

            proof = dict(kind='workspace', input=str(code), input_sha256=file_hash(code),
                         manifest=str(manifest), manifest_sha256=file_hash(manifest))
            with patch.dict(os.environ, env), patch.object(units, 'verify', return_value=producer), \
                    patch.object(units, 'source_proof', return_value=proof), \
                    patch.object(builds.subprocess, 'call', side_effect=child):
                self.assertEqual(builds.run(guard, [str(wrapper)]), 0)

    def test_first_probe_has_observed_parent_without_future_output(self):
        self.probe_during_first_producer()

    def test_probe_cannot_authorize_other_manifest(self):
        with self.assertRaises(RuntimeError):
            self.probe_during_first_producer(lambda env, args: env.update(CARGO_MANIFEST_DIR='/other'))

    def test_probe_cannot_authorize_linked_executable(self):
        with self.assertRaises(RuntimeError):
            self.probe_during_first_producer(lambda env, args: args.append('--test'))

    def test_probe_cannot_authorize_extern(self):
        with self.assertRaises(RuntimeError):
            self.probe_during_first_producer(lambda env, args: args.extend(['--extern', 'foreign=lib']))

    def test_probe_cannot_emit_link_output(self):
        with self.assertRaises(RuntimeError):
            self.probe_during_first_producer(lambda env, args: args.append('--emit=link'))

    def test_probe_cannot_authorize_output_outside_producer(self):
        def mutate(env, args):
            args[args.index('--out-dir') + 1] = env['CARGO_MANIFEST_DIR']
        with self.assertRaises(RuntimeError):
            self.probe_during_first_producer(mutate)


if __name__ == '__main__':
    unittest.main()
