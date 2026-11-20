#!/usr/bin/env python3
"""Bounded native checks for the acquired full Debian pair; no package creation."""
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


class FullDebianSbom(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest = sbom.loads(Path(ARGS.inputs).read_bytes())
        cls.expected, cls.schema = sbom.reconstruct_repro(
            ARGS.descriptor, ARGS.inputs, sbom.file_hash(ARGS.inputs), cls.manifest['archives'][0]['path'])
        cls.parent = sbom.loads(sbom.repro_reference(cls.manifest['parent_sbom']))
        cls.binding = cls.expected['repro_binding']
        cls.freeze = sbom.loads(sbom.repro_reference(cls.binding['freeze']))
        cls.descriptor, cls.policy, _, _ = sbom.tool_policy(ARGS.descriptor, sbom.FULL_DEB_PROFILE)
        # Oracle is the prospective package017 freeze, never the generated sidecar.
        cls.files = {f['path']: {k: f[k] for k in ('path', 'bytes', 'sha256', 'mode')}
                     for f in cls.freeze['expected_files']}
        cls.metadata = cls.freeze['expected_metadata']
        cls.control = sbom.repro_reference(cls.freeze['expected_control'])
        cls.container = cls.expected['deb_binding']
        cls.proofs = Path(ARGS.proofs)
        cls.proofs.mkdir(parents=True, exist_ok=True)

    def save(self, name, value):
        (self.proofs / (name + '.json')).write_text(json.dumps(
            value, sort_keys=True, separators=(',', ':'),
            default=lambda v: dict(bytes_hex=v.hex()) if isinstance(v, bytes) else str(v)) + '\n')

    def closure_reject(self, name, edit, diagnostic):
        values = {k: copy.deepcopy(v) for k, v in dict(
            files=self.files, metadata=self.metadata, parent=self.parent,
            descriptor=self.descriptor, freeze=self.freeze, binding=self.binding, policy=self.policy,
            control=self.control, container=self.container).items()}
        edit(values)
        self.save(name + '-input', values)
        with self.assertRaisesRegex(sbom.Error, diagnostic) as caught:
            sbom.full_deb_file_closure(**values)
        self.save(name + '-receipt', dict(expected=diagnostic, actual=str(caught.exception),
                                        boundary='pure reader inventory/metadata boundary; no physical DEB mutation'))

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
                '--profile', sbom.FULL_DEB_PROFILE, '--descriptor', ARGS.descriptor,
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
                         (14, 3, 145, 198, 379))
        self.assertEqual(sbom.digest(sbom.canonical(sbom.cargo_projection(actual))),
                         '2491e6845a5421b8d6546f9d5920423c9852c04efd38275ddc07cfb8bca042b9')
        self.assertEqual(sbom.cargo_projection(actual), sbom.cargo_projection(self.parent))
        self.assertEqual(actual['full_deb_binding']['metadata']['sha256'],
                         'f556eba2e775dbaf2cee8a1efdbb848192c8ec96ec152f888d4989c758371b01')
        self.assertEqual(actual['full_deb_binding']['metadata']['bytes'], 2017)
        self.assertEqual(actual['repro_binding']['receipt']['sha256'],
                         '8b7922f35e9de31f58de42d9e59f28c46a009f68715056008f7cd7a12b912035')
        self.assertEqual(actual['repro_binding']['signature_status'], 'unsigned')
        self.assertFalse(actual['full_deb_binding']['release_artifact'])
        self.assertFalse(actual['full_deb_binding']['reuse_release_binary'])
        self.assertTrue(actual['full_deb_binding']['skip_build'])
        self.assertEqual(actual['full_deb_binding']['generated_at_utc'], '2026-09-27T06:37:01+02:00')
        self.assertEqual(actual['full_deb_binding']['control']['sha256'],
                         '26bf129222316a3678f4b560de74f303a3c009a07ade05a606e4e80d5374049d')
        self.assertNotEqual(actual['payload_source'], actual['repro_binding']['packager_source'])
        self.assertNotIn('rpm_signed', {r['name'] for r in actual['recipe_evidence']})
        self.assertNotIn('reuse_evidence', actual)
        self.assertNotEqual(actual['deb_binding'], self.parent['deb_binding'])

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
                            'full Debian metadata differs')

    def test_07_false_packager_source(self):
        self.closure_reject('packager', lambda v: v['binding']['packager_source'].update(commit='0' * 40),
                            'full Debian packager source differs')

    def test_08_false_payload_source(self):
        self.closure_reject('payload-source', lambda v: v['metadata']['payload_provenance'].update(source='0' * 40),
                            'full Debian payload source differs')

    def test_09_changed_binary_old_cargo(self):
        def edit(v):
            path = 'usr/bin/rmux'
            v['files'][path]['sha256'] = '0' * 64
            next(f for f in v['freeze']['expected_files'] if f['path'] == path)['sha256'] = '0' * 64
            v['metadata']['binary_sha256'] = '0' * 64
            next(f for f in v['metadata']['payload_provenance']['files'] if f['role'] == 'cli')['sha256'] = '0' * 64
            v['freeze']['expected_metadata'] = copy.deepcopy(v['metadata'])
        self.closure_reject('changed-binary-old-cargo', edit, 'full Debian binary Cargo closure differs')

    def test_10_unsigned_schema_and_no_inherited_evidence(self):
        self.document_reject('signed', lambda d: d['repro_binding'].update(signature_status='signed'), 'schema enum')
        self.document_reject('signed-evidence', lambda d: d['recipe_evidence'].append(
            dict(name='rpm_signed', bytes=1, sha256='0' * 64)), 'SBOM differs')

    def test_11_captured_metadata_provenance_and_tools(self):
        self.document_reject('capture-pin', lambda d: d['full_deb_binding']['observations'][0]['metadata'].update(
            sha256='0' * 64), 'SBOM differs')
        self.document_reject('tools-pin', lambda d: d['repro_binding']['historical_tools'][0].update(
            sha256='0' * 64), 'SBOM differs')
        self.document_reject('source-pin', lambda d: d['full_deb_binding']['source_files'][0]['source'].update(
            sha256='0' * 64), 'SBOM differs')

    def test_12_modes_set_release_and_context(self):
        self.closure_reject('mode', lambda v: v['files']['usr/bin/rmux'].update(mode=420), 'full Debian file inventory differs')
        self.closure_reject('missing', lambda v: v['files'].pop('usr/bin/rmux'), 'full Debian file inventory differs')
        self.closure_reject('release', lambda v: v['metadata'].update(release_artifact=True), 'full Debian build context differs')
        self.closure_reject('features', lambda v: v['parent']['resolution_contexts'][0].update(features=[]),
                            'full Debian Cargo context differs')

    def test_13_old_014_strict_file_closure(self):
        with self.assertRaisesRegex(sbom.Error, 'reproduction payload differs'):
            sbom.repro_file_closure(self.files, self.parent, self.descriptor, 'debian')

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
        current['repro_roots'].pop(sbom.FULL_DEB_PROFILE)
        self.assertEqual(current, old_schema)
        for profile, old in old_layout['sbom']['profiles'].items():
            omit = lambda p: {k: v for k, v in p.items() if k not in ('generator', 'validator', 'schema_sha256')}
            self.assertEqual(omit(self.descriptor['sbom']['profiles'][profile]), omit(old))


    def test_16_control_bytes_and_copyright(self):
        self.closure_reject('control-bytes', lambda v: v.update(control=v['control'] + b'\n'),
                            'full Debian control differs')
        self.closure_reject('copyright', lambda v: v['files']['usr/share/doc/rmux/copyright'].update(sha256='0' * 64),
                            'full Debian file inventory differs')

    def test_17_missing_success_observations(self):
        receipt = sbom.loads(sbom.repro_reference(self.binding['receipt']))
        row = receipt['runs'][0]
        entry = sbom.loads(sbom.repro_reference(row['captures']['ENTRY-CALL.json']))
        call = sbom.loads(sbom.repro_reference(row['captures']['DPKG-CALL.json']))
        values = dict(row=row, entry=entry, call=call, tools=self.binding['historical_tools'])
        sbom.full_deb_observation(**values)
        for name in ('row', 'entry', 'call'):
            with self.subTest(observation=name):
                changed = copy.deepcopy(values)
                changed[name].pop('rc')
                self.save('missing-success-' + name + '-input', changed)
                with self.assertRaisesRegex(sbom.Error, 'full Debian missing successful observation') as caught:
                    sbom.full_deb_observation(**changed)
                self.save('missing-success-' + name + '-receipt', dict(actual=str(caught.exception)))

    def test_18_original_009_and_full_016_preserved(self):
        historical = self.manifest['historical_inputs']
        current, _ = sbom.reconstruct_deb(ARGS.descriptor, historical['path'], historical['sha256'],
                                           self.manifest['historical_subject'])
        project = lambda d: {k: v for k, v in d.items() if k not in ('tool_pins', 'closure_sha256')}
        self.assertEqual(project(current), project(self.parent))
        self.save('regression-009', dict(equal_except_tool_pins_and_closure=True,
            parent=self.manifest['parent_sbom'], current_sidecar_sha256=sbom.digest(sbom.canonical(current))))
        path = Path(ARGS.full_rpm_fixture) / 'INPUTS.json'
        manifest = sbom.loads(path.read_bytes())
        current, _ = sbom.reconstruct_repro(ARGS.descriptor, path, sbom.file_hash(path), manifest['archives'][0]['path'])
        old_path = Path(ARGS.full_rpm_fixture) / 'lot2/A.sbom.json'
        old = sbom.loads(old_path.read_bytes())
        self.assertEqual(project(current), project(old))
        self.save('regression-016', dict(equal_except_tool_pins_and_closure=True,
            old_sidecar_sha256=sbom.file_hash(old_path), current_sidecar_sha256=sbom.digest(sbom.canonical(current))))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inputs', required=True)
    parser.add_argument('--proofs', required=True)
    parser.add_argument('--previous-fixture', required=True)
    parser.add_argument('--full-rpm-fixture', required=True)
    parser.add_argument('--base', default='43fe9c78bd5dc6cf38e5eac7e6fe761e531afc05')
    parser.add_argument('--descriptor', default=str(ROOT / 'scripts/packaging/rmux-package-layout.json'))
    ARGS, remaining = parser.parse_known_args()
    unittest.main(argv=[sys.argv[0], *remaining], verbosity=2)
