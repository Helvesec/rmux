"""Inert sibling coverage for legacy observations and authenticated receipts."""
import copy
import json
from pathlib import Path
import unittest

import test_candidate_authenticated_generation as fixtures


class LegacyBoundary(unittest.TestCase):
    setUp = fixtures.AuthenticatedGeneration.setUp
    git = fixtures.AuthenticatedGeneration.git
    unit = fixtures.AuthenticatedGeneration.unit
    publish = fixtures.AuthenticatedGeneration.publish
    verify = fixtures.AuthenticatedGeneration.verify
    generation = fixtures.AuthenticatedGeneration.generation

    def consumer(self, scope='root', version=1):
        code = self.repo / 'main.rs' if scope == 'root' else self.code
        unit = self.unit(code, kind='bin')
        if version == 1:
            for key in ('lock_context_version', 'lock_contexts', 'manifest_scope'):
                unit['graph'].pop(key)
        unit['depinfo'].parent.mkdir(exist_ok=True)
        unit['depinfo'].write_text(str(code) + ':\n')
        return unit

    def observe(self, record, name='historical.json'):
        path = self.target / name
        path.write_text(json.dumps(record))
        return dict(receipt=str(path), receipt_sha256=self.g.file_hash(path))

    def check_depinfo(self, unit):
        self.units.check_depinfo(self.g, unit, self.repo, self.head, self.target)

    def authenticate(self, descriptor):
        return self.g.support('candidate_identity_buildscripts').verify_generation(
            self.g, descriptor, self.repo, self.head, self.target)

    def test_empty_legacy_receipt_grants_no_generation_authority(self):
        descriptor = self.observe({'files': {}})
        self.assertIsNone(self.authenticate(descriptor))
        for version in (1, 2):
            with self.subTest(version=version):
                unit = self.consumer(version=version)
                unit['graph']['generation'] = descriptor
                self.check_depinfo(unit)
                self.verify(self.publish(unit, f'empty-v{version}.inert'))
                generated = self.target / 'unauthorized.rs'
                generated.write_text('pub const UNAUTHORIZED: u8 = 1;\n')
                with unit['depinfo'].open('a') as stream:
                    stream.write(str(generated) + ':\n')
                with self.assertRaisesRegex(self.g.IdentityError, 'unqualified generated input'):
                    self.check_depinfo(unit)

    def test_legacy_receipt_rejects_producer_files_retention_and_malformed_body(self):
        unit = self.consumer()
        records = [{'files': {}, 'producer': value} for value in (None, {}, False)]
        records += [{'files': {'generated.rs': '0' * 64}},
                    {'files': {}, 'retained_files': {'generated.rs': {}}},
                    {'files': None}, {'files': []}, {}, [], None]
        for index, record in enumerate(records):
            descriptor = self.observe(record)
            unit['graph']['generation'] = descriptor
            with self.subTest(record=record, entry='authentication'), self.assertRaises(self.g.IdentityError):
                self.authenticate(descriptor)
            with self.subTest(record=record, entry='depinfo'), self.assertRaises(self.g.IdentityError):
                self.check_depinfo(unit)
            with self.subTest(record=record, entry='replay'), self.assertRaises(self.g.IdentityError):
                self.verify(self.publish(unit, f'bad-legacy-{index}.inert'))
        Path(descriptor['receipt']).write_text('invalid JSON')
        descriptor['receipt_sha256'] = self.g.file_hash(descriptor['receipt'])
        with self.assertRaises(self.g.IdentityError):
            self.authenticate(descriptor)

    def test_empty_receipt_remains_hash_pinned_and_cannot_declare_a_producer(self):
        descriptor = self.observe({'files': {}})
        for producer in (None, {}, False):
            with self.subTest(producer=producer), self.assertRaises(self.g.IdentityError):
                self.authenticate(dict(descriptor, producer=producer))
        Path(descriptor['receipt']).write_text('{"files": {}}\n')
        with self.assertRaisesRegex(self.g.IdentityError, 'provenance changed'):
            self.authenticate(descriptor)

    def test_relocated_generation_rejected_for_v1_v2_same_and_cross_scope(self):
        canonical, _ = self.generation(self.code, 'parser')
        original = Path(canonical['receipt']).read_bytes()
        moved = self.target / 'moved-generator.json'
        moved.write_bytes(original)
        for version in (1, 2):
            for consumer in ('root', 'parser'):
                for declared in (True, False):
                    unit = self.consumer(consumer, version)
                    descriptor = copy.deepcopy(canonical)
                    descriptor['receipt'] = str(moved)
                    if not declared:
                        del descriptor['producer']
                    unit['graph']['generation'] = descriptor
                    label = f'{consumer}-v{version}-{declared}'
                    with self.subTest(case=label, entry='depinfo'), self.assertRaises(self.g.IdentityError):
                        self.check_depinfo(unit)
                    with self.subTest(case=label, entry='replay'), self.assertRaises(self.g.IdentityError):
                        self.verify(self.publish(unit, label + '.inert'))
        self.assertEqual(Path(canonical['receipt']).read_bytes(), original)
        self.assertEqual(moved.read_bytes(), original)

    def test_canonical_empty_receipt_cannot_use_legacy_fallback(self):
        for directory in ('identity-generators', 'identity-build-sessions'):
            path = self.target / directory / ('0' * 32 + '.json')
            path.parent.mkdir(exist_ok=True)
            path.write_text('{"files": {}}\n')
            descriptor = dict(receipt=str(path), receipt_sha256=self.g.file_hash(path))
            with self.subTest(directory=directory), self.assertRaises(self.g.IdentityError):
                self.authenticate(descriptor)

    def test_probe_receipt_cannot_be_relocated_into_legacy_authority(self):
        canonical, _ = self.generation(self.code, 'probe')
        record = json.loads(Path(canonical['receipt']).read_text())
        record['files'] = {}
        record.pop('retained_files')
        path = self.target / 'identity-build-sessions' / ('1' * 32 + '.json')
        path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps(record))
        descriptor = dict(receipt=str(path), receipt_sha256=self.g.file_hash(path),
                          producer=record['producer'])
        self.assertEqual(self.authenticate(descriptor), descriptor)
        moved = self.target / 'moved-probe.json'
        moved.write_bytes(path.read_bytes())
        descriptor = dict(receipt=str(moved), receipt_sha256=self.g.file_hash(moved))
        for version in (1, 2):
            unit = self.consumer(version=version)
            unit['graph']['generation'] = descriptor
            with self.subTest(version=version, entry='depinfo'), self.assertRaises(self.g.IdentityError):
                self.check_depinfo(unit)
            with self.subTest(version=version, entry='replay'), self.assertRaises(self.g.IdentityError):
                self.verify(self.publish(unit, f'moved-probe-v{version}.inert'))


if __name__ == '__main__':
    unittest.main()
