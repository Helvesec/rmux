"""RC evidence and neighboring rejection boundaries; synthetic verifier only."""
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import installed_job_objects_spec as collector_fixture
import installed_job_objects_provenance as provenance
from release.release_evidence import file_hash


class ProvenanceSpec(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.archive, self.checksums = self.root / 'rmux.zip', self.root / 'SHA256SUMS.txt'
        self.archive.write_bytes(b'package bytes')
        self.checksums.write_text(file_hash(self.archive))
        self.record_path = self.root / 'canonical-build-record.json'
        self.bundle, self.gh = self.root / 'build-provenance.sigstore.json', self.root / 'gh.exe'
        self.bundle.write_bytes(b'received bundle fixture')
        self.gh.write_bytes(b'qualified verifier fixture')
        self.plan = {'sha': 'a' * 40, 'release_ref': 'v0.10.0-rc.1',
                     'archive_sha256': file_hash(self.archive),
                     'checksums_sha256': file_hash(self.checksums)}
        self.record = {'schema_version': 1, 'repository_id': 1239918790,
            'source_git_sha': self.plan['sha'], 'release_kind': 'rc',
            'planned_release_ref': self.plan['release_ref'],
            'platform': {'key': 'windows-x86_64', 'target_triple': 'x86_64-pc-windows-msvc',
                         'archive_format': 'zip'},
            'runner': {'image': 'windows-latest', 'os': 'Windows', 'arch': 'X64',
                       'environment': 'github-hosted'},
            'files': [{'path': subject.name, 'role': role, 'sha256': file_hash(subject),
                       'size': subject.stat().st_size} for subject, role in
                      ((self.archive, 'archive'), (self.checksums, 'checksums'))]}

    def freeze(self):
        self.record_path.write_text(json.dumps(self.record))
        for name, subject in (('provenance_receipt', self.record_path),
                              ('provenance_bundle', self.bundle), ('gh', self.gh)):
            self.plan.update({name: str(subject), name + '_sha256': file_hash(subject)})
        self.verified = {'verificationResult': {
            'statement': {'subject': [{'name': self.record_path.name,
                                      'digest': {'sha256': file_hash(self.record_path)}}]},
            'signature': {'certificate': {}}, 'verifiedTimestamps': [{}]}}

    def verify(self, result=None):
        result = result or subprocess.CompletedProcess([], 0, stdout=json.dumps([self.verified]))
        with patch.object(provenance.subprocess, 'run', return_value=result) as run:
            proof = provenance.verify(self.plan, self.archive, self.checksums, self.root)
        return proof, run.call_args.args[0]

    def test_verified_record_binds_exact_sha_ref_and_archive_digest(self):
        self.freeze()
        proof, command = self.verify()
        self.assertEqual(proof['source_sha'], self.plan['sha'])
        self.assertEqual(proof['release_ref'], self.plan['release_ref'])
        self.assertEqual(proof['archive_sha256'], file_hash(self.archive))
        self.assertIn('--deny-self-hosted-runners', command)
        for flag, expected in (('--repo', 'Helvesec/rmux'),
                               ('--signer-workflow', provenance.WORKFLOW),
                               ('--source-digest', self.plan['sha']),
                               ('--source-ref', 'refs/heads/main'), ('--bundle', str(self.bundle))):
            self.assertEqual(command[command.index(flag) + 1], expected)
        provenance.unchanged(proof)
        self.bundle.write_bytes(b'changed after qualification')
        with self.assertRaisesRegex(ValueError, 'changed during qualification'):
            provenance.unchanged(proof)

    def test_missing_changed_and_relative_provenance_inputs_are_rejected(self):
        for name in ('provenance_receipt', 'provenance_bundle', 'gh'):
            for change in ('missing', 'digest', 'relative', 'absent'):
                with self.subTest(name=name, change=change):
                    self.freeze()
                    if change == 'missing':
                        self.plan.pop(name)
                    elif change == 'digest':
                        self.plan[name + '_sha256'] = '0' * 64
                    elif change == 'relative':
                        self.plan[name] = 'relative.json'
                    else:
                        self.plan[name] = str(self.root / 'absent.json')
                    with patch.object(provenance.subprocess, 'run') as run:
                        with self.assertRaises(ValueError):
                            provenance.verify(self.plan, self.archive, self.checksums, self.root)
                        run.assert_not_called()

    def test_wrong_hosted_identity_and_subjects_fail_before_verifier(self):
        original = copy.deepcopy(self.record)
        variants = [dict(original, source_git_sha='b' * 40),
                    dict(original, planned_release_ref='v0.10.0-rc.2'),
                    dict(original, release_kind='shadow'),
                    dict(original, repository_id=1), dict(original, schema_version=2),
                    dict(original, platform={**original['platform'], 'key': 'linux-x86_64'}),
                    dict(original, runner={**original['runner'], 'environment': 'self-hosted'})]
        for field, value in (('path', 'other.zip'), ('size', 0), ('sha256', '0' * 64)):
            variant = copy.deepcopy(original)
            variant['files'][0][field] = value
            variants.append(variant)
        variants += [dict(original, files=original['files'][:1]),
                     dict(original, files=original['files'] + [original['files'][0]])]
        for variant in variants:
            with self.subTest(variant=variant):
                self.record = variant
                self.freeze()
                with patch.object(provenance.subprocess, 'run') as run:
                    with self.assertRaises(ValueError):
                        provenance.verify(self.plan, self.archive, self.checksums, self.root)
                    run.assert_not_called()

    def test_local_claim_or_failed_unsigned_verifier_never_authorizes_rc(self):
        self.freeze()
        altered = copy.deepcopy(self.verified)
        altered['verificationResult']['statement']['subject'][0]['digest']['sha256'] = '0' * 64
        unsigned = copy.deepcopy(self.verified)
        unsigned['verificationResult'].pop('signature')
        no_timestamp = copy.deepcopy(self.verified)
        no_timestamp['verificationResult']['verifiedTimestamps'] = []
        for payload in ('invalid', '[]', '{}', '[{"status":"pass","hosted":true}]',
                        json.dumps([altered]), json.dumps([unsigned]), json.dumps([no_timestamp]),
                        json.dumps([self.verified, self.verified])):
            with self.subTest(payload=payload):
                with self.assertRaises(ValueError):
                    self.verify(subprocess.CompletedProcess([], 0, stdout=payload))
        with self.assertRaisesRegex(ValueError, 'verification failed'):
            self.verify(subprocess.CompletedProcess([], 1, stdout=json.dumps([self.verified])))


class QualificationNeighborsSpec(unittest.TestCase):
    def fixture(self):
        fixture = collector_fixture.InstalledJobObjectsSpec(methodName='runTest')
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        return fixture

    def test_metadata_ref_and_frozen_package_rejections_never_execute_cli(self):
        for rejection in ('metadata', 'authority', 'rc-ref', 'archive', 'checksums'):
            with self.subTest(rejection=rejection):
                fixture = self.fixture()
                if rejection == 'metadata':
                    fixture.metadata['git_commit'] = 'b' * 40
                elif rejection == 'authority':
                    fixture.plan['authority'] = 'fixture'
                elif rejection == 'rc-ref':
                    fixture.plan.update(authority='rc', release_ref='v0.9.0-rc.1')
                else:
                    fixture.plan[rejection + '_sha256'] = '0' * 64
                with self.assertRaises(ValueError):
                    fixture.collect()
                self.assertEqual(fixture.launches, [])
                self.assertEqual(fixture.cli_commands(), [])

    def test_rc_provenance_refusal_never_triggers_cli_cleanup(self):
        fixture = self.fixture()
        fixture.plan.update(authority='rc', release_ref='v0.10.0-rc.1')
        with patch.object(provenance, 'verify', side_effect=ValueError('untrusted receipt')):
            with self.assertRaisesRegex(ValueError, 'untrusted receipt'):
                fixture.collect()
        self.assertEqual(fixture.launches, [])
        self.assertEqual(fixture.cli_commands(), [])
        receipt = json.loads((Path(fixture.plan['output']) / 'receipt.json').read_text())
        self.assertEqual(receipt['status'], 'not-played')
        self.assertEqual(receipt['cells'], [])


if __name__ == '__main__':
    unittest.main()
