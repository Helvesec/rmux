#!/usr/bin/env python3
"""Bounded native checks for the acquired full RPM pair; no package creation."""
import argparse
import copy
import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/packaging'))
import sbom


class FullRpmSbom(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest = sbom.loads(Path(ARGS.inputs).read_bytes())
        cls.expected, cls.schema = sbom.reconstruct_repro(
            ARGS.descriptor, ARGS.inputs, sbom.file_hash(ARGS.inputs), cls.manifest['archives'][0]['path'])
        cls.parent = sbom.loads(sbom.repro_reference(cls.manifest['parent_sbom']))
        cls.binding = cls.expected['repro_binding']
        cls.freeze = sbom.loads(sbom.repro_reference(cls.binding['freeze']))
        cls.descriptor, cls.policy, _, _ = sbom.tool_policy(ARGS.descriptor, sbom.FULL_RPM_PROFILE)
        # Oracle is the prospective package015 freeze, never the generated sidecar.
        cls.files = {f['path']: {k: f[k] for k in ('path', 'bytes', 'sha256', 'mode')}
                     for f in cls.freeze['expected_files']}
        cls.metadata = cls.freeze['expected_metadata']
        cls.proofs = Path(ARGS.proofs)
        cls.proofs.mkdir(parents=True, exist_ok=True)

    def save(self, name, value):
        (self.proofs / (name + '.json')).write_bytes(sbom.canonical(value))

    def closure_reject(self, name, edit, diagnostic):
        values = {k: copy.deepcopy(v) for k, v in dict(
            files=self.files, metadata=self.metadata, parent=self.parent,
            descriptor=self.descriptor, freeze=self.freeze, binding=self.binding, policy=self.policy).items()}
        edit(values)
        self.save(name + '-input', values)
        with self.assertRaisesRegex(sbom.Error, diagnostic) as caught:
            sbom.full_rpm_file_closure(**values)
        self.save(name + '-receipt', dict(expected=diagnostic, actual=str(caught.exception),
                                        boundary='pure reader inventory/metadata boundary; no physical RPM mutation'))

    def document_reject(self, name, edit, diagnostic):
        value = copy.deepcopy(self.expected)
        edit(value)
        value['closure_sha256'] = sbom.digest(sbom.canonical(sbom.closure_fields(value)))
        self.save(name + '-document', value)
        with self.assertRaisesRegex(sbom.Error, diagnostic) as caught:
            sbom.validate_document(value, self.expected, self.schema)
        self.save(name + '-receipt', dict(expected=diagnostic, actual=str(caught.exception)))

    def manifest_reject(self, name, edit, diagnostic):
        value = copy.deepcopy(self.manifest)
        edit(value)
        self.save(name + '-inputs', value)
        path = self.proofs / (name + '-inputs.json')
        output = self.proofs / (name + '-must-not-exist.json')
        argv = [sys.executable, '-B', str(ROOT / 'scripts/packaging/sbom.py'), 'generate',
                '--profile', sbom.FULL_RPM_PROFILE, '--descriptor', ARGS.descriptor,
                '--inputs', str(path), '--inputs-sha256', sbom.file_hash(path),
                '--subject', self.manifest['archives'][0]['path'], '--output', str(output)]
        result = subprocess.run(argv, capture_output=True, text=True, timeout=30)
        self.save(name + '-receipt', dict(argv=argv, rc=result.returncode, stdout=result.stdout,
                                        stderr=result.stderr, expected_rc=2, expected_diagnostic=diagnostic))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(diagnostic, result.stderr)
        self.assertFalse(output.exists())

    def test_01_positive_prospective_files_and_complete_cargo(self):
        actual = self.expected
        self.assertEqual(actual['files'], sorted(self.files.values(), key=lambda f: f['path']))
        self.assertEqual((len(actual['files']), len(actual['binary_roots']), len(actual['packages']),
                          len(actual['build_units']), sum(r['kind'] == 'depends_on' for r in actual['relationships'])),
                         (13, 3, 145, 198, 379))
        self.assertEqual(sbom.digest(sbom.canonical(sbom.cargo_projection(actual))),
                         '2491e6845a5421b8d6546f9d5920423c9852c04efd38275ddc07cfb8bca042b9')
        self.assertEqual(sbom.cargo_projection(actual), sbom.cargo_projection(self.parent))
        self.assertEqual(actual['full_rpm_binding']['metadata']['sha256'],
                         '4ec2129b36a398f1dbc7be2f4414815c2ed9d210a1b5a08346d508cd2e6b5685')
        self.assertEqual(actual['full_rpm_binding']['metadata']['bytes'], 2014)
        self.assertEqual(actual['repro_binding']['receipt']['sha256'],
                         '9b360a1b1c144dda8ce1049aa11c6f2fd31abbda73d3a517b823eec4fc644cc4')
        self.assertEqual(actual['repro_binding']['signature_status'], 'unsigned')
        self.assertFalse(actual['full_rpm_binding']['release_artifact'])
        self.assertNotIn('rpm_signed', {r['name'] for r in actual['recipe_evidence']})
        self.assertNotIn('reuse_evidence', actual)
        self.assertNotEqual(actual['rpm_binding'], self.parent['rpm_binding'])

    def test_02_subject_sha_cli(self):
        self.manifest_reject('subject-sha', lambda m: m['subject'].update(sha256='0' * 64),
                             'reproduction subject differs')

    def test_03_subject_size_cli(self):
        self.manifest_reject('subject-size', lambda m: m['subject'].update(bytes=1),
                             'reproduction subject differs')

    def test_04_foreign_pair_cli(self):
        self.manifest_reject('foreign-pair', lambda m: m['archives'][1].update(path=m['archives'][0]['path']),
                             'foreign A/B archive binding')

    def test_05_foreign_receipt_cli(self):
        self.manifest_reject('foreign-receipt', lambda m: m['packaging_receipt'].update(sha256='0' * 64),
                             'foreign packaging receipt')

    def test_06_metadata_semantic_divergence(self):
        self.closure_reject('metadata', lambda v: v['metadata'].update(generated_at_utc='2000-01-01T00:00:00Z'),
                            'full RPM metadata differs')

    def test_07_false_packager_source(self):
        self.closure_reject('packager', lambda v: v['binding']['packager_source'].update(commit='0' * 40),
                            'full RPM packager source differs')

    def test_08_false_payload_source(self):
        self.closure_reject('payload-source', lambda v: v['metadata']['payload_provenance'].update(source='0' * 40),
                            'full RPM payload source differs')

    def test_09_changed_binary_old_cargo(self):
        def edit(v):
            path = 'usr/bin/rmux'
            v['files'][path]['sha256'] = '0' * 64
            next(f for f in v['freeze']['expected_files'] if f['path'] == path)['sha256'] = '0' * 64
            v['metadata']['binary_sha256'] = '0' * 64
            next(f for f in v['metadata']['payload_provenance']['files'] if f['role'] == 'cli')['sha256'] = '0' * 64
            v['freeze']['expected_metadata'] = copy.deepcopy(v['metadata'])
        self.closure_reject('changed-binary-old-cargo', edit, 'full RPM binary Cargo closure differs')

    def test_10_unsigned_schema_and_no_inherited_evidence(self):
        self.document_reject('signed', lambda d: d['repro_binding'].update(signature_status='signed'), 'schema enum')
        self.document_reject('signed-evidence', lambda d: d['recipe_evidence'].append(
            dict(name='rpm_signed', bytes=1, sha256='0' * 64)), 'SBOM differs')

    def test_11_captured_metadata_provenance_and_tools(self):
        self.document_reject('capture-pin', lambda d: d['full_rpm_binding']['captures'][0]['metadata'].update(
            sha256='0' * 64), 'SBOM differs')
        self.document_reject('tools-pin', lambda d: d['repro_binding']['historical_tools'][0].update(
            sha256='0' * 64), 'SBOM differs')
        self.document_reject('source-pin', lambda d: d['full_rpm_binding']['source_files'][0]['source'].update(
            sha256='0' * 64), 'SBOM differs')

    def test_12_modes_set_release_and_context(self):
        self.closure_reject('mode', lambda v: v['files']['usr/bin/rmux'].update(mode=420), 'full RPM file inventory differs')
        self.closure_reject('missing', lambda v: v['files'].pop('usr/bin/rmux'), 'full RPM file inventory differs')
        self.closure_reject('release', lambda v: v['metadata'].update(release_artifact=True), 'full RPM build context differs')
        self.closure_reject('features', lambda v: v['parent']['resolution_contexts'][0].update(features=[]),
                            'full RPM Cargo context differs')

    def test_13_old_014_strict_file_closure(self):
        with self.assertRaisesRegex(sbom.Error, 'reproduction payload differs'):
            sbom.repro_file_closure(self.files, self.parent, self.descriptor, 'rpm')

    def test_14_shared_reproduction_paths_preserve_014(self):
        for kind in ('tar', 'deb', 'rpm'):
            with self.subTest(kind=kind):
                path = Path(ARGS.previous_fixture) / 'inputs' / (kind + '.json')
                manifest = sbom.loads(path.read_bytes())
                current, _ = sbom.reconstruct_repro(ARGS.descriptor, path, sbom.file_hash(path),
                                                    manifest['archives'][0]['path'])
                old = sbom.loads((Path(ARGS.previous_fixture) / 'lot1/a' / (kind + '.sbom.json')).read_bytes())
                project = lambda d: {k: v for k, v in d.items() if k not in ('tool_pins', 'closure_sha256')}
                self.assertEqual(project(current), project(old))
                self.save('regression-014-' + kind, dict(subject=current['subject'],
                    old_sidecar_sha256=sbom.digest(sbom.canonical(old)),
                    current_sidecar_sha256=sbom.digest(sbom.canonical(current)),
                    equal_except_tool_pins_and_closure=True))

    def test_15_old_schema_and_layout_preserved(self):
        def original(path):
            return sbom.loads(subprocess.check_output(['git', '-C', str(ROOT), 'show', ARGS.base + ':' + path]))
        old_layout = original('scripts/packaging/rmux-package-layout.json')
        strip = lambda d: {k: v for k, v in d.items() if k != 'sbom'}
        self.assertEqual(strip(self.descriptor), strip(old_layout))
        old_schema = original('scripts/packaging/sbom-schema-v1.json')
        current = sbom.loads((ROOT / 'scripts/packaging/sbom-schema-v1.json').read_bytes())
        current['repro_roots'].pop(sbom.FULL_RPM_PROFILE)
        self.assertEqual(current, old_schema)
        for profile, old in old_layout['sbom']['profiles'].items():
            omit = lambda p: {k: v for k, v in p.items() if k not in ('generator', 'validator', 'schema_sha256')}
            self.assertEqual(omit(self.descriptor['sbom']['profiles'][profile]), omit(old))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inputs', required=True)
    parser.add_argument('--proofs', required=True)
    parser.add_argument('--previous-fixture', required=True)
    parser.add_argument('--base', default='5678af61c7b3a688f12633bb6fa6a35aace1fbae')
    parser.add_argument('--descriptor', default=str(ROOT / 'scripts/packaging/rmux-package-layout.json'))
    ARGS, remaining = parser.parse_known_args()
    unittest.main(argv=[sys.argv[0], *remaining], verbosity=2)
