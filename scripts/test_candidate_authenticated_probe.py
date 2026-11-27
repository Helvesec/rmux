"""Inert sibling regressions for the producer of a compiler probe session."""
import copy
import json
from pathlib import Path
import unittest

import test_candidate_authenticated_generation as fixtures


class AuthenticatedProbe(unittest.TestCase):
    setUp = fixtures.AuthenticatedGeneration.setUp
    git = fixtures.AuthenticatedGeneration.git
    unit = fixtures.AuthenticatedGeneration.unit
    publish = fixtures.AuthenticatedGeneration.publish
    verify = fixtures.AuthenticatedGeneration.verify
    generation = fixtures.AuthenticatedGeneration.generation

    def probe(self, scope, version=2):
        code = self.repo / 'main.rs' if scope == 'root' else self.code
        generation, generated = self.generation(code, f'{scope}-v{version}')
        record = json.loads(Path(generation['receipt']).read_text())
        record['files'] = {}
        record.pop('retained_files')
        receipt = self.target / 'identity-build-sessions' / ('1' * 32 + '.json')
        receipt.parent.mkdir(exist_ok=True)
        receipt.write_text(json.dumps(record))
        generation = dict(receipt=str(receipt), receipt_sha256=self.g.file_hash(receipt),
                          producer=record['producer'])
        unit = self.unit(code)
        unit['graph']['generation'] = generation
        unit['graph']['arguments'] += ['--emit=metadata']
        self.verify(self.publish(unit, scope + '-same.rmeta'))
        return unit, receipt, record

    def test_probe_replay_refuses_relabelled_cross_scope_producer(self):
        for consumer, producer, version in (('root', 'parser', 2),
                                             ('parser', 'root', 2),
                                             ('root', 'parser', 1)):
            with self.subTest(consumer=consumer, version=version):
                probe, receipt, record = self.probe(producer, version)
                code = self.repo / 'main.rs' if consumer == 'root' else self.code
                unit = self.unit(code)
                contexts = copy.deepcopy(unit['graph']['lock_contexts'])
                if version == 1:
                    for key in ('lock_contexts', 'lock_context_version', 'manifest_scope'):
                        unit['graph'].pop(key)
                unit['graph']['generation'] = copy.deepcopy(probe['graph']['generation'])
                unit['graph']['generation']['producer']['lock_contexts'] = contexts
                with self.assertRaises(self.g.IdentityError):
                    self.verify(self.publish(unit, f'{consumer}-{version}-forged.rmeta'))

    def test_probe_replay_requalifies_session_producer_even_when_graph_matches(self):
        unit, receipt, record = self.probe('root')
        record['producer']['receipt_sha256'] = 'f' * 64
        receipt.write_text(json.dumps(record))
        unit['graph']['generation'].update(producer=record['producer'],
                                          receipt_sha256=self.g.file_hash(receipt))
        with self.assertRaises(self.g.IdentityError):
            self.verify(self.publish(unit, 'session-forged.rmeta'))

    def test_probe_replay_pins_immutable_producer_receipt(self):
        unit, receipt, record = self.probe('root')
        producer = record['producer']
        # Historical replay must keep the exact observed receipt, even when a
        # subsequent observation replaces the current SHA lookup alias.
        alias = self.target / 'identity-units' / (producer['sha256'] + '.json')
        alias.write_text('{}\n')
        self.verify(self.publish(unit, 'historical.rmeta'))

    def test_probe_replay_refuses_changed_producer_bytes(self):
        unit, receipt, record = self.probe('root')
        Path(record['producer']['path']).write_bytes(b'changed inert producer')
        with self.assertRaises(self.g.IdentityError):
            self.verify(self.publish(unit, 'changed-producer.rmeta'))


if __name__ == '__main__':
    unittest.main()
