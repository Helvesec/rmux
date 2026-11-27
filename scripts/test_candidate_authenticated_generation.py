"""Frozen G1 regression: authenticate generated-input lock authority on replay.

Only Git and inert fixture data are used; no image or compiler is executed.
"""
import copy
import json
import unittest

from test_candidate_identity_lock_closure import LockClosure


class AuthenticatedGeneration(unittest.TestCase):
    setUp = LockClosure.setUp
    git = LockClosure.git
    unit = LockClosure.unit
    publish = LockClosure.publish
    verify = LockClosure.verify

    def generation(self, code, label):
        unit = self.unit(code, kind='bin')
        unit['kind'] = 'build-script'
        producer = self.verify(self.publish(unit, label + '-producer.inert'))
        directory = self.target / (label + '-generated')
        directory.mkdir()
        generated = directory / 'generated.rs'
        generated.write_text('pub const GENERATED: u32 = 1;\n')
        builds = self.g.support('candidate_identity_buildscripts')
        files, retained = builds.retain_files(self.g, directory, self.target)
        record = dict(head=self.head, source=str(self.repo), directory=str(directory),
                      manifest=str(code.with_name('Cargo.toml')), producer=producer,
                      files=files, retained_files=retained)
        receipt = self.units.preserve(self.g, self.target / 'identity-generators', record)
        descriptor = builds.verify_receipt(self.g, receipt, self.repo, self.head, self.target)
        return descriptor, generated

    def check_case(self, consumer, producer, version):
        label = f'{consumer}-{producer}-v{version}'
        codes = {'root': self.repo / 'main.rs', 'parser': self.code}
        unit = self.unit(codes[consumer], kind='bin')
        contexts = copy.deepcopy(unit['graph']['lock_contexts'])
        if version == 1:
            for key in ('lock_context_version', 'lock_contexts', 'manifest_scope'):
                unit['graph'].pop(key)
        same, _ = self.generation(codes[consumer], label + '-same')
        unit['graph']['generation'] = same
        self.verify(self.publish(unit, label + '-same.inert'))
        cross, generated = self.generation(codes[producer], label + '-cross')
        receipt_hash = self.g.file_hash(cross['receipt'])
        unit['graph']['generation'] = cross
        with self.assertRaisesRegex(self.g.IdentityError, 'cross .*workspace locks'):
            self.verify(self.publish(unit, label + '-cross.inert'))
        forged = copy.deepcopy(unit)
        forged['graph']['generation']['producer']['lock_contexts'] = contexts
        # A valid generated input and immutable producer receipt are retained.
        # Only the producer declaration copied into the consumer graph changes.
        forged['depinfo'].parent.mkdir(exist_ok=True)
        forged['depinfo'].write_text(str(codes[consumer]) + ':\n' + str(generated) + ':\n')
        image = self.publish(forged, label + '-relabelled.inert')
        record = dict(case=label, version=version, source=str(self.repo), head=self.head,
                      os=self.g.host_os(), target=str(self.target), image=str(image),
                      generation=cross, forged_generation=forged['graph']['generation'],
                      same_scope='ACCEPTED', authentic_cross_scope='REJECTED')
        (self.fixture / 'G1-CASE.json').write_text(json.dumps(record, indent=2) + '\n')
        with self.subTest(entry='replay'), self.assertRaises(self.g.IdentityError):
            self.verify(image)
        self.assertEqual(self.g.file_hash(cross['receipt']), receipt_hash)
        with self.subTest(entry='depinfo'), self.assertRaises(self.g.IdentityError):
            self.units.check_depinfo(self.g, forged, self.repo, self.head, self.target)
        self.assertEqual(self.g.file_hash(cross['receipt']), receipt_hash)

    def test_v2_root_rejects_relabelled_parser_generation(self):
        self.check_case('root', 'parser', 2)

    def test_v2_parser_rejects_relabelled_root_generation(self):
        self.check_case('parser', 'root', 2)

    def test_v1_root_rejects_relabelled_parser_generation(self):
        self.check_case('root', 'parser', 1)


if __name__ == '__main__':
    unittest.main()
