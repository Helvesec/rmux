"""Frozen G2 witness: a relocated receipt cannot become legacy authority.

Git and inert byte fixtures only; no compiler or generated image is executed.
"""
import copy
import json
from pathlib import Path
import unittest

import test_candidate_authenticated_generation as fixtures


class LegacyGeneration(unittest.TestCase):
    setUp = fixtures.AuthenticatedGeneration.setUp
    git = fixtures.AuthenticatedGeneration.git
    unit = fixtures.AuthenticatedGeneration.unit
    publish = fixtures.AuthenticatedGeneration.publish
    verify = fixtures.AuthenticatedGeneration.verify
    generation = fixtures.AuthenticatedGeneration.generation

    def test_v1_rejects_relocated_parser_receipt_without_declared_producer(self):
        unit = self.unit(self.repo / 'main.rs', kind='bin')
        for key in ('lock_context_version', 'lock_contexts', 'manifest_scope'):
            unit['graph'].pop(key)
        same, _ = self.generation(self.repo / 'main.rs', 'same')
        unit['graph']['generation'] = same
        self.verify(self.publish(unit, 'same.inert'))
        cross, generated = self.generation(self.code, 'cross')
        unit['graph']['generation'] = cross
        with self.assertRaisesRegex(self.g.IdentityError, 'cross .*workspace locks'):
            self.verify(self.publish(unit, 'canonical-cross.inert'))
        original = Path(cross['receipt'])
        raw = original.read_bytes()
        relocated = self.target / 'legacy-generated-observation.json'
        relocated.write_bytes(raw)
        descriptor = copy.deepcopy(cross)
        descriptor['receipt'] = str(relocated)
        unit['graph']['generation'] = descriptor
        with self.assertRaises(self.g.IdentityError):
            self.verify(self.publish(unit, 'relocated-with-producer.inert'))
        del descriptor['producer']
        unit['depinfo'].parent.mkdir(exist_ok=True)
        unit['depinfo'].write_text(str(self.repo / 'main.rs') + ':\n' + str(generated) + ':\n')
        image = self.publish(unit, 'relocated-without-producer.inert')
        witness = dict(source=str(self.repo), head=self.head, target=str(self.target),
                       os=self.g.host_os(), original_receipt=str(original),
                       original_sha256=self.g.file_hash(original), relocated=str(relocated),
                       image=str(image), descriptor=descriptor, depinfo=str(unit['depinfo']),
                       same_scope='ACCEPTED', canonical_cross='REJECTED',
                       relocated_with_producer='REJECTED')
        (self.fixture / 'G2-CASE.json').write_text(json.dumps(witness, indent=2) + '\n')
        with self.subTest(entry='depinfo'), self.assertRaises(self.g.IdentityError):
            self.units.check_depinfo(self.g, unit, self.repo, self.head, self.target)
        with self.subTest(entry='replay'), self.assertRaises(self.g.IdentityError):
            self.verify(image)
        self.assertEqual(original.read_bytes(), raw)
        self.assertEqual(relocated.read_bytes(), raw)


    def test_v1_root_rejects_relocated_parser_receipt_without_producer(self):
        root = self.repo / 'main.rs'
        unit = self.unit(root, kind='bin')
        for key in ('lock_context_version', 'lock_contexts', 'manifest_scope'):
            unit['graph'].pop(key)
        same, _ = self.generation(root, 'same')
        unit['graph']['generation'] = same
        self.verify(self.publish(unit, 'same.inert'))
        cross, generated = self.generation(self.code, 'cross')
        original = Path(cross['receipt'])
        raw = original.read_bytes()
        unit['graph']['generation'] = cross
        with self.assertRaises(self.g.IdentityError):
            self.verify(self.publish(unit, 'canonical-cross.inert'))
        relocated = self.target / 'legacy-generated-observation.json'
        relocated.write_bytes(raw)
        moved = dict(cross, receipt=str(relocated))
        unit['graph']['generation'] = moved
        with self.assertRaises(self.g.IdentityError):
            self.verify(self.publish(unit, 'relocated-with-producer.inert'))
        del moved['producer']
        unit['depinfo'].parent.mkdir(exist_ok=True)
        unit['depinfo'].write_text(str(root) + ':\n' + str(generated) + ':\n')
        image = self.publish(unit, 'relocated-without-producer.inert')
        (self.fixture / 'G2-CASE.json').write_text(json.dumps(dict(
            source=str(self.repo), head=self.head, target=str(self.target),
            image=str(image), depinfo=str(unit['depinfo']), original_receipt=str(original),
            relocated_receipt=str(relocated), receipt_sha256=self.g.file_hash(original),
            generation=moved), indent=2) + '\n')
        with self.subTest(entry='depinfo'), self.assertRaises(self.g.IdentityError):
            self.units.check_depinfo(self.g, unit, self.repo, self.head, self.target)
        with self.subTest(entry='replay'), self.assertRaises(self.g.IdentityError):
            self.verify(image)
        self.assertEqual(original.read_bytes(), raw)
        self.assertEqual(relocated.read_bytes(), raw)


if __name__ == '__main__':
    unittest.main()
