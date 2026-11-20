#!/usr/bin/env python3
"""Bounded native checks for the acquired full TAR pair; no package creation."""
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


class FullTarSbom(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest = sbom.loads(Path(ARGS.inputs).read_bytes())
        cls.expected, cls.schema = sbom.reconstruct_repro(
            ARGS.descriptor, ARGS.inputs, sbom.file_hash(ARGS.inputs), cls.manifest['archives'][0]['path'])
        cls.parent = sbom.loads(sbom.repro_reference(cls.manifest['parent_sbom']))
        cls.binding = cls.expected['repro_binding']
        cls.freeze = sbom.loads(sbom.repro_reference(cls.binding['freeze']))
        cls.descriptor, cls.policy, _, _ = sbom.tool_policy(ARGS.descriptor, sbom.FULL_TAR_PROFILE)
        # Oracle is the prospective package019 freeze, never the generated sidecar.
        cls.files = {f['path']: {k: f[k] for k in ('path', 'bytes', 'sha256', 'mode')}
                     for f in cls.freeze['files']}
        cls.metadata = cls.freeze['expected_metadata']
        cls.proofs = Path(ARGS.proofs)
        cls.proofs.mkdir(parents=True, exist_ok=True)

    def save(self, name, value):
        (self.proofs / (name + '.json')).write_text(json.dumps(
            value, sort_keys=True, separators=(',', ':'),
            default=lambda v: dict(bytes_hex=v.hex()) if isinstance(v, bytes) else str(v)) + '\n')

    def closure_reject(self, name, edit, diagnostic):
        values = {k: copy.deepcopy(v) for k, v in dict(
            files=self.files, metadata=self.metadata, parent=self.parent,
            descriptor=self.descriptor, freeze=self.freeze, binding=self.binding, policy=self.policy).items()}
        edit(values)
        self.save(name + '-input', values)
        with self.assertRaisesRegex(sbom.Error, diagnostic) as caught:
            sbom.full_tar_file_closure(**values)
        self.save(name + '-receipt', dict(expected=diagnostic, actual=str(caught.exception),
                                        boundary='pure reader inventory/metadata boundary; no physical TAR mutation'))

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
                '--profile', sbom.FULL_TAR_PROFILE, '--descriptor', ARGS.descriptor,
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
                         (16, 3, 145, 198, 379))
        self.assertEqual(sbom.digest(sbom.canonical(sbom.cargo_projection(actual))),
                         '2491e6845a5421b8d6546f9d5920423c9852c04efd38275ddc07cfb8bca042b9')
        self.assertEqual(sbom.cargo_projection(actual), sbom.cargo_projection(self.parent))
        self.assertEqual(actual['full_tar_binding']['metadata']['sha256'],
                         'fccba4b053c0329ea36fbaae616e6781bb48f342d48367aa2673d431da33eb30')
        self.assertEqual(actual['full_tar_binding']['metadata']['bytes'], 2061)
        self.assertEqual(actual['repro_binding']['receipt']['sha256'],
                         '7d428991ef6b2e923315dffbfae2d5623dc727359eea09fc649892c8f6f15db7')
        self.assertEqual(actual['repro_binding']['signature_status'], 'unsigned')
        self.assertFalse(actual['full_tar_binding']['release_artifact'])
        self.assertTrue(actual['full_tar_binding']['skip_build'])
        self.assertEqual(actual['full_tar_binding']['generated_at_utc'], '2026-09-27T07:58:11+02:00')
        self.assertEqual(actual['full_tar_binding']['manifest']['sha256'],
                         '9a77bd861dc73884c821ff725ec208a134907b076747f66ef8dc36d044a39b54')
        self.assertEqual(len(actual['tar_binding']['members']), 34)
        self.assertEqual(sum(r['kind'] == 'directory' for r in actual['tar_binding']['members']), 18)
        self.assertNotEqual(actual['payload_source'], actual['repro_binding']['packager_source'])
        self.assertNotIn('rpm_signed', {r['name'] for r in actual['recipe_evidence']})
        self.assertNotIn('reuse_evidence', actual)
        self.assertNotIn('tar_binding', self.parent)

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
                            'full TAR metadata differs')

    def test_07_false_packager_source(self):
        self.closure_reject('packager', lambda v: v['binding']['packager_source'].update(commit='0' * 40),
                            'full TAR packager source differs')

    def test_08_false_payload_source(self):
        self.closure_reject('payload-source', lambda v: v['metadata']['payload_provenance'].update(source='0' * 40),
                            'full TAR payload source differs')

    def test_09_changed_binary_old_cargo(self):
        def edit(v):
            path = 'bin/rmux'
            v['files'][path]['sha256'] = '0' * 64
            next(f for f in v['freeze']['files'] if f['path'] == path)['sha256'] = '0' * 64
            v['metadata']['binary_sha256'] = '0' * 64
            next(f for f in v['metadata']['payload_provenance']['files'] if f['role'] == 'cli')['sha256'] = '0' * 64
            v['freeze']['expected_metadata'] = copy.deepcopy(v['metadata'])
        self.closure_reject('changed-binary-old-cargo', edit, 'full TAR binary Cargo closure differs')

    def test_10_unsigned_schema_and_no_inherited_evidence(self):
        self.document_reject('signed', lambda d: d['repro_binding'].update(signature_status='signed'), 'schema enum')
        self.document_reject('signed-evidence', lambda d: d['recipe_evidence'].append(
            dict(name='rpm_signed', bytes=1, sha256='0' * 64)), 'SBOM differs')

    def test_11_captured_metadata_provenance_and_tools(self):
        self.document_reject('capture-pin', lambda d: d['full_tar_binding']['observations'][0]['metadata'].update(
            sha256='0' * 64), 'SBOM differs')
        self.document_reject('tools-pin', lambda d: d['repro_binding']['historical_tools'][0].update(
            sha256='0' * 64), 'SBOM differs')
        self.document_reject('source-pin', lambda d: d['full_tar_binding']['source_files'][0]['source'].update(
            sha256='0' * 64), 'SBOM differs')

    def test_12_modes_set_release_and_context(self):
        self.closure_reject('mode', lambda v: v['files']['bin/rmux'].update(mode=420), 'full TAR file inventory differs')
        self.closure_reject('missing', lambda v: v['files'].pop('bin/rmux'), 'full TAR file inventory differs')
        self.closure_reject('release', lambda v: v['metadata'].update(release_artifact=True), 'full TAR build context differs')
        self.closure_reject('features', lambda v: v['parent']['resolution_contexts'][0].update(features=[]),
                            'full TAR Cargo context differs')

    def test_13_old_014_strict_file_closure(self):
        with self.assertRaisesRegex(sbom.Error, 'reproduction payload differs'):
            sbom.repro_file_closure(self.files, self.parent, self.descriptor, 'unix-archive')

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
        current['repro_roots'].pop(sbom.FULL_TAR_PROFILE)
        self.assertEqual(current, old_schema)
        for profile, old in old_layout['sbom']['profiles'].items():
            omit = lambda p: {k: v for k, v in p.items() if k not in ('generator', 'validator', 'schema_sha256')}
            self.assertEqual(omit(self.descriptor['sbom']['profiles'][profile]), omit(old))


    def test_16_roles_raw_metadata_and_context(self):
        self.closure_reject('missing-role', lambda v: v['metadata']['payload_provenance']['files'].pop(),
                            'full TAR binary roles differ')
        self.closure_reject('duplicate-role', lambda v: v['metadata']['payload_provenance']['files'].append(
            v['metadata']['payload_provenance']['files'][0]), 'full TAR binary roles differ')
        self.closure_reject('target', lambda v: v['parent']['resolution_contexts'][0].update(target='alien'),
                            'full TAR Cargo context differs')
        self.closure_reject('cargo', lambda v: v['parent']['build_units'].pop(),
                            'full TAR historical Cargo projection differs')
        captured = {path: sbom.repro_reference(self.freeze[key]) for path, key in [
            ('share/rmux/artifact-metadata.json', 'expected_metadata_bytes'),
            ('share/rmux/package-layout.tsv', 'expected_manifest'), ('SHA256SUMS.txt', 'expected_checksums')]}
        captured['share/rmux/artifact-metadata.json'] += b'\n'
        with self.assertRaisesRegex(sbom.Error, 'full TAR prospective bytes differ') as caught:
            sbom.full_tar_evidence(self.files, captured, self.expected['tar_binding'], self.parent,
                                   self.descriptor, self.binding, self.policy)
        self.save('raw-metadata-receipt', dict(actual=str(caught.exception), mutation='additional newline with identical semantics'))

    def test_17_missing_success_observations(self):
        receipt = sbom.loads(sbom.repro_reference(self.binding['receipt']))
        row = receipt['runs'][0]
        values = dict(row=row, tools=self.binding['historical_tools'])
        for key, name in [('entry', 'ENTRY-CALL.json'), ('tar', 'TAR-CALL.json'), ('gzip', 'GZIP-CALL.json')]:
            values[key] = sbom.loads(sbom.repro_reference(row['captures'][name]))
        sbom.full_tar_observation(**values)
        for name in ('row', 'entry', 'tar', 'gzip'):
            with self.subTest(observation=name):
                changed = copy.deepcopy(values); changed[name].pop('rc')
                self.save('missing-success-' + name + '-input', changed)
                with self.assertRaisesRegex(sbom.Error, 'full TAR missing successful observation') as caught:
                    sbom.full_tar_observation(**changed)
                self.save('missing-success-' + name + '-receipt', dict(actual=str(caught.exception)))

    def test_18_original_009_and_full_016_018_preserved(self):
        project = lambda d: {k: v for k, v in d.items() if k not in ('tool_pins', 'closure_sha256')}
        deb_manifest = sbom.loads((Path(ARGS.full_deb_fixture) / 'INPUTS.json').read_bytes())
        historical = deb_manifest['historical_inputs']
        current, _ = sbom.reconstruct_deb(ARGS.descriptor, historical['path'], historical['sha256'],
                                           deb_manifest['historical_subject'])
        old = sbom.loads(sbom.repro_reference(deb_manifest['parent_sbom']))
        self.assertEqual(project(current), project(old))
        self.save('regression-009', dict(equal_except_tool_pins_and_closure=True,
            parent=deb_manifest['parent_sbom'], current_sidecar_sha256=sbom.digest(sbom.canonical(current))))
        for job, fixture, oldname in [('016', ARGS.full_rpm_fixture, 'lot2/A.sbom.json'),
                                      ('018', ARGS.full_deb_fixture, 'lot1/A.sbom.json')]:
            path = Path(fixture) / 'INPUTS.json'; manifest = sbom.loads(path.read_bytes())
            current, _ = sbom.reconstruct_repro(ARGS.descriptor, path, sbom.file_hash(path), manifest['archives'][0]['path'])
            old_path = Path(fixture) / oldname; old = sbom.loads(old_path.read_bytes())
            self.assertEqual(project(current), project(old))
            self.save('regression-' + job, dict(equal_except_tool_pins_and_closure=True,
                old_sidecar_sha256=sbom.file_hash(old_path), current_sidecar_sha256=sbom.digest(sbom.canonical(current))))

    def test_19_actual_header_boundary_and_false_container(self):
        import base64
        row = sbom.loads(sbom.repro_reference(self.binding['receipt']))['runs'][0]
        inventory = sbom.loads(sbom.repro_reference(row['inventory']))
        member = sbom.FullTarInfo.frombuf(base64.b64decode(inventory['entries'][0]['header_base64']), 'utf-8', 'surrogateescape')
        sbom.full_tar_header(member, '')
        for field, value, reason in [('uid', 1, 'owner/link'), ('mtime', 0, 'epoch/extensions'),
                                     ('header_magic', '00', 'header format'), ('mode', 0o700, 'directory mode')]:
            changed = copy.copy(member); setattr(changed, field, value)
            with self.assertRaisesRegex(sbom.Error, reason) as caught:
                sbom.full_tar_header(changed, '')
            self.save('header-' + field + '-receipt', dict(actual=str(caught.exception), mutation={field:value}))
        self.document_reject('gzip-header', lambda d: d['tar_binding'].update(gzip_header_hex='00'), 'schema enum')
        self.document_reject('order', lambda d: d['tar_binding']['members'].reverse(), 'SBOM differs')
        self.document_reject('false-closure', lambda d: d['build_units'].pop(), 'SBOM differs')



if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inputs', required=True)
    parser.add_argument('--proofs', required=True)
    parser.add_argument('--previous-fixture', required=True)
    parser.add_argument('--full-rpm-fixture', required=True)
    parser.add_argument('--full-deb-fixture', required=True)
    parser.add_argument('--base', default='859219ca1792eca131c13feeb4b339913cfd1877')
    parser.add_argument('--descriptor', default=str(ROOT / 'scripts/packaging/rmux-package-layout.json'))
    ARGS, remaining = parser.parse_known_args()
    unittest.main(argv=[sys.argv[0], *remaining], verbosity=2)
