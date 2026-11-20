"""Inert legacy/canonical boundary and sibling consumer coverage for G2."""
import copy
import json
from pathlib import Path
import unittest

from test_candidate_legacy_generation import LegacyGeneration


class LegacyGenerationBoundary(unittest.TestCase):
    setUp = LegacyGeneration.setUp
    git = LegacyGeneration.git
    unit = LegacyGeneration.unit
    publish = LegacyGeneration.publish
    verify = LegacyGeneration.verify
    generation = LegacyGeneration.generation

    def root_unit(self, version=1):
        unit = self.unit(self.repo / 'main.rs', kind='bin')
        if version == 1:
            for key in ('lock_context_version', 'lock_contexts', 'manifest_scope'):
                unit['graph'].pop(key)
        unit['depinfo'].parent.mkdir(exist_ok=True)
        unit['depinfo'].write_text(str(self.repo / 'main.rs') + ':\n')
        return unit

    def observe(self, unit, label, record):
        receipt = self.target / (label + '.json')
        receipt.write_text(json.dumps(record))
        unit['graph']['generation'] = dict(receipt=str(receipt),
                                           receipt_sha256=self.g.file_hash(receipt))
        return receipt

    def reject(self, unit, label):
        builds = self.g.support('candidate_identity_buildscripts')
        for entry in ('descriptor', 'depinfo', 'replay'):
            with self.subTest(case=label, entry=entry), self.assertRaises(self.g.IdentityError):
                if entry == 'descriptor':
                    builds.verify_generation(self.g, unit['graph']['generation'],
                                             self.repo, self.head, self.target)
                elif entry == 'depinfo':
                    self.units.check_depinfo(self.g, unit, self.repo, self.head, self.target)
                else:
                    self.verify(self.publish(unit, label + '.inert'))

    def test_empty_legacy_observation_has_no_generation_authority(self):
        builds = self.g.support('candidate_identity_buildscripts')
        for version in (1, 2):
            unit = self.root_unit(version)
            receipt = self.observe(unit, f'empty-{version}', {'files': {}})
            self.assertIsNone(builds.verify_generation(
                self.g, unit['graph']['generation'], self.repo, self.head, self.target))
            self.units.check_depinfo(self.g, unit, self.repo, self.head, self.target)
            self.verify(self.publish(unit, f'empty-{version}.inert'))
            receipt.write_text('{"files": {}, "changed": true}')
            self.reject(unit, f'changed-empty-{version}')

    def test_legacy_empty_observation_cannot_authorize_generated_input(self):
        unit = self.root_unit()
        generated = self.target / 'unqualified.rs'
        generated.write_text('pub const UNQUALIFIED: u32 = 1;\n')
        self.observe(unit, 'empty-with-directory', dict(files={}, directory=str(self.target)))
        unit['depinfo'].write_text(str(self.repo / 'main.rs') + ':\n' + str(generated) + ':\n')
        with self.assertRaises(self.g.IdentityError):
            self.units.check_depinfo(self.g, unit, self.repo, self.head, self.target)

    def test_legacy_record_cannot_hide_producer_or_generated_files(self):
        unit = self.root_unit()
        descriptor, generated = self.generation(self.code, 'parser-boundary')
        authentic = json.loads(Path(descriptor['receipt']).read_text())
        records = {
            'authentic-copy': authentic,
            'producer-only': dict(authentic, files={}, retained_files={}),
            'files-only': {k: v for k, v in authentic.items() if k != 'producer'},
            'retained-only': dict(files={}, retained_files=authentic['retained_files']),
            'null-producer': dict(files={}, producer=None),
            'missing-files': {},
            'null-files': dict(files=None),
            'list-files': dict(files=[]),
            'list-record': [],
        }
        for label, record in records.items():
            self.observe(unit, label, record)
            self.reject(unit, label)

    def test_legacy_descriptor_cannot_carry_unverified_fields(self):
        unit = self.root_unit()
        for key in ('producer', 'files', 'lock_contexts'):
            self.observe(unit, 'descriptor-' + key, {'files': {}})
            unit['graph']['generation'][key] = {}
            self.reject(unit, 'descriptor-' + key)

    def test_canonical_receipts_require_complete_authenticated_descriptor(self):
        unit = self.root_unit()
        descriptor, generated = self.generation(self.repo / 'main.rs', 'canonical')
        for mutation in ('missing-producer', 'extra-field'):
            altered = copy.deepcopy(descriptor)
            if mutation == 'missing-producer':
                altered.pop('producer')
            else:
                altered['files'] = {}
            unit['graph']['generation'] = altered
            self.reject(unit, mutation)
        unit['graph']['generation'] = descriptor
        unit['depinfo'].write_text(str(self.repo / 'main.rs') + ':\n' + str(generated) + ':\n')
        self.units.check_depinfo(self.g, unit, self.repo, self.head, self.target)
        self.verify(self.publish(unit, 'canonical-valid.inert'))

    def test_relocated_probe_cannot_drop_its_producer(self):
        unit = self.root_unit()
        descriptor, _ = self.generation(self.code, 'probe-producer')
        record = json.loads(Path(descriptor['receipt']).read_text())
        record['files'] = {}
        record.pop('retained_files')
        receipt = self.target / 'identity-build-sessions' / ('2' * 32 + '.json')
        receipt.parent.mkdir(exist_ok=True)
        receipt.write_text(json.dumps(record))
        builds = self.g.support('candidate_identity_buildscripts')
        actual = builds.verify_probe_receipt(self.g, receipt, self.repo, self.head, self.target)
        self.observe(unit, 'relocated-probe', record)
        self.assertEqual(unit['graph']['generation']['receipt_sha256'], actual['receipt_sha256'])
        self.reject(unit, 'relocated-probe')


if __name__ == '__main__':
    unittest.main()
