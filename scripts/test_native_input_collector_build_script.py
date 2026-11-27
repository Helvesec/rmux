"""The native launch boundary must accept the witnessed build-script producer."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import candidate_identity as guard
import candidate_identity_units as units
import native_input_windows as native
from native_input_collector import Snapshot


class BuildScriptBoundary(unittest.TestCase):
    def fixture(self, mutate=None):
        audit = Path(__file__).resolve().parent.parent / '.rmux-audit'
        with tempfile.TemporaryDirectory(dir=audit) as temporary:
            source = Path(temporary).resolve()
            target = source / 'target'
            target.mkdir()
            head = 'a' * 40
            source_id = hashlib.sha256(os.fsencode(str(source))).hexdigest()
            (source / 'Cargo.lock').write_bytes(b'locked archive fixture')
            compiler = source / 'compiler'
            compiler.write_bytes(b'compiler fixture')
            graph = dict(head=head, source=str(source),
                         lock_sha256=guard.file_hash(source / 'Cargo.lock'),
                         compiler=str(compiler), compiler_sha256=guard.file_hash(compiler),
                         dependencies=[])
            graph_sha = units.digest(graph)
            payload = target / 'producer.payload.exe'
            payload.write_bytes(f'RMUX_BUILD_IDENTITY_V1:{head}:{source_id}:windows:END'.encode()
                                + units.GRAPH + graph_sha.encode() + b':END')
            record = dict(path=str(payload), sha256=guard.file_hash(payload),
                          kind='build-script', graph=graph, graph_sha256=graph_sha)
            directory = target / 'identity-units'
            immutable = units.preserve(guard, directory, record)
            (directory / (units.receipt_key(payload, record['sha256']) + '.json')).write_bytes(
                immutable.read_bytes())
            producer = units.verify(guard, payload, source, head, target)
            token = 'b' * 32
            sessions = target / 'identity-build-sessions'
            sessions.mkdir()
            ticket = dict(head=head, source=str(source), producer=producer)
            ticket_path = sessions / (token + '.json')
            output = source / 'audit'
            output.mkdir()
            manifest = dict(os='windows', cell='sdk', source=str(source), head=head,
                            audit=str(output), case='build-script', reserve_bytes=1, files={})
            env = dict(RMUX_NATIVE_INPUT_MANIFEST=str(output / 'manifest.json'),
                       RMUX_NATIVE_INPUT_MANIFEST_SHA256='digest',
                       RMUX_IDENTITY_TARGET=str(target), RMUX_IDENTITY_BUILD_SESSION=token)
            if mutate:
                mutate(ticket, env, payload, immutable)
            ticket_path.write_text(json.dumps(ticket))
            with patch.object(native, 'os', SimpleNamespace(name='nt')), \
                    patch.object(native, 'verify_manifest', return_value=manifest), \
                    patch.object(native, 'new_snapshot', side_effect=lambda p, r: Snapshot(p)), \
                    patch.object(guard, 'host_os', return_value='windows'), \
                    patch.object(guard, 'source_identity', return_value=(source, source_id)), \
                    patch.object(guard, 'target_identity', return_value=target):
                before = native.before_launch([str(payload)], env)
                self.assertEqual(before['executable_sha256'], guard.file_hash(payload))
                saved = before['files'][str(payload)]['object']
                self.assertEqual(Path(saved).read_bytes(), payload.read_bytes())

    def test_completed_unit_is_retained_before_build_script_launch(self):
        self.fixture()

    def test_other_observed_producer_is_rejected(self):
        with self.assertRaises(RuntimeError):
            self.fixture(lambda ticket, *_: ticket['producer'].update(sha256='c' * 64))

    def test_other_candidate_session_is_rejected(self):
        with self.assertRaises(RuntimeError):
            self.fixture(lambda ticket, *_: ticket.update(head='d' * 40))

    def test_missing_completed_unit_is_rejected(self):
        with self.assertRaises(RuntimeError):
            self.fixture(lambda ticket, env, payload, immutable: immutable.unlink())

    def test_missing_build_session_is_rejected(self):
        with self.assertRaises(RuntimeError):
            self.fixture(lambda ticket, env, *_: env.pop('RMUX_IDENTITY_BUILD_SESSION'))


if __name__ == '__main__':
    unittest.main()
