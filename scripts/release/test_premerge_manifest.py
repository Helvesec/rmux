"""Synthetic protocol tests; these never constitute a hosted RC or signature."""
import argparse
import copy
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
import unittest
from unittest.mock import patch

import test_premerge_evidence as fixture
from candidate_manifest_evidence import canonical_hash, encode_field, POLICY_DOMAIN
from candidate_route import CANONICAL
import candidate_artifact_resolution as resolution

SHA = fixture.SHA


class ManifestTest(unittest.TestCase):
    setUp = fixture.EvidenceTest.setUp
    run_fixture = fixture.EvidenceTest.run_fixture
    verify = fixture.EvidenceTest.verify

    def protocol(self, route=None):
        route = route or self.route
        module = fixture.load('candidate-manifest')
        directory = self.root / ('rehearsal' if route.rehearsal else 'canonical')
        directory.mkdir()
        def write(name, value):
            path = directory / name
            path.write_text(json.dumps(value, sort_keys=True))
            return path
        def proof(kind):
            value = self.verify(self.run_fixture(kind, route))
            value['test_fixture'] = False  # Synthetic protocol fixture, never a hosted verdict.
            value['proof_sha256'] = canonical_hash({k: v for k, v in value.items() if k != 'proof_sha256'})
            return value
        candidate, fast = proof('candidate'), proof('fast')
        intent_args = argparse.Namespace(**vars(self.args))
        intent_args.candidate_profile = route.profile
        intent_args.rehearsal_permit = self.permit if route.rehearsal else None
        intent_args.github_ref = route.source_ref
        intent = fixture.acceptance.intent.validate(intent_args)
        identity = dict(source_git_sha=SHA, fast_run_id=1234, candidate_run_id=5678,
            release_intent_id=intent['release_intent_id'], planned_release_ref=intent['planned_release_ref'], release_kind='rc')
        artifacts = [dict(role=spec.role, platform_key=spec.platform_key,
            name=spec.name, artifact_id=index, archive_digest='sha256:' + 'd' * 64, size_in_bytes=42)
            for index, spec in enumerate(resolution.expected_specs(SHA), 1)]
        by_platform = {(item['role'], item['platform_key']): item for item in artifacts}
        metadata = dict(route.fields, status='verified-for-shadow-sealing', repository_id=1239918790,
            **identity, candidate_run_attempt=1, resolution_sha256='d' * 64,
            fast_evidence=dict(proof_sha256=fast['proof_sha256'], nextest_artifact={}),
            source_artifacts=artifacts, canonical_platforms=[])
        records, bindings = [], []
        for platform, policy in module.PLATFORMS.items():
            files = [dict(path=role, role=role, size=42, sha256='d' * 64) for role in sorted(policy['roles'])]
            record = dict(schema_version=1, repository_id=1239918790, **identity,
                candidate_run_attempt=1,
                platform=dict(key=platform, target_triple=policy['target']),
                runner=dict(policy['runner'], environment='github-hosted'),
                toolchain=dict(requested='1.96.1', release='1.96.1', host=policy['target'],
                               commit_hash='c' * 40, rustc_verbose_sha256='d' * 64),
                build_policy=module.BUILD_POLICY, created_at='2026-10-26T01:30:00Z', files=files)
            path = write(platform + '-record.json', record)
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            assets = by_platform[('canonical-assets', platform)]
            binding = dict(schema_version=1, repository_id=1239918790, source_git_sha=SHA,
                candidate_run_id=5678, platform_key=platform,
                assets=dict(artifact_id=assets['artifact_id'], artifact_name=assets['name'], artifact_digest=assets['archive_digest']),
                build_record_sha256=digest,
                attestation=dict(attestation_id='fixture-only', bundle_file='build-provenance.sigstore.json', bundle_sha256='d' * 64))
            binding_path = write(platform + '-binding.json', binding)
            records.append(platform + '=' + str(path))
            bindings.append(platform + '=' + str(binding_path))
            metadata['canonical_platforms'].append(dict(platform_key=platform,
                assets_artifact=assets, provenance_artifact=by_platform[('canonical-provenance', platform)],
                build_record_sha256=digest, attestation_id='fixture-only', attestation_bundle_sha256='d' * 64,
                runner=record['runner'], toolchain=record['toolchain'], files=files))
        contract_record = dict(path='.github/release/candidate-contract.json', mode='100644', type='blob',
            size=len(self.contract_bytes), blob_oid='e' * 40, sha256=hashlib.sha256(self.contract_bytes).hexdigest())
        digest = hashlib.sha256(POLICY_DOMAIN)
        for field in ('path', 'mode', 'type', 'blob_oid'):
            digest.update(encode_field(contract_record[field].encode()))
        digest.update(encode_field(contract_record['size'].to_bytes(8, 'big')))
        digest.update(encode_field(contract_record['sha256'].encode()))
        policy = dict(schema_version=1, algorithm='sha256-length-delimited-v2', source_git_sha=SHA,
            contract_path=contract_record['path'], contract_mode='100644', contract_type='blob',
            contract_blob_oid='e' * 40, release_policy_sha256=digest.hexdigest(), records=[contract_record])
        args = argparse.Namespace(candidate_profile=route.profile,
            rehearsal_permit=self.permit if route.rehearsal else None,
            candidate_proof=write('candidate-run-proof.json', candidate),
            fast_proof=write('fast-proof.json', fast), candidate_intent=write('candidate-intent.json', intent),
            policy_root=write('policy-root.json', policy), canonical_artifacts=write('metadata.json', metadata),
            build_record=records, artifact_binding=bindings)
        return module, args, directory

    def test_full_manifest_schema_and_canonical_neighborhood(self):
        for route in (CANONICAL, self.route):
            with self.subTest(route=route.profile):
                module, args, _ = self.protocol(route)
                value = module.build_manifest(args)
                self.assertEqual(value['schema_version'], 2 if route.rehearsal else 1)
                self.assertEqual(value['producer']['branch'], route.branch)
                self.assertEqual(len(value['artifacts']), 5)
                if route.rehearsal:
                    self.assertIs(value['publication_authority'], False)
                    for field, replacement in [('candidate_profile', 'canonical'), ('rehearsal_permit', None)]:
                        changed = copy.copy(args)
                        setattr(changed, field, replacement)
                        with self.assertRaises(ValueError):
                            module.build_manifest(changed)
                else:
                    self.assertNotIn('publication_authority', value)

    def test_simulation_reproduces_sealed_bytes_without_a_synthetic_release(self):
        module, args, directory = self.protocol()
        manifest = module.build_manifest(args)
        manifest_path = directory / 'candidate-manifest.json'
        manifest_path.write_text(json.dumps(manifest))
        args.sealed_dir = directory
        args.manifest_sha256 = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
        current = json.loads(args.candidate_proof.read_text())
        current['verified_at'] = '2026-10-26T03:00:00Z'
        current['proof_sha256'] = canonical_hash({k: v for k, v in current.items() if k != 'proof_sha256'})
        args.current_candidate_proof = directory / 'current.json'
        args.current_candidate_proof.write_text(json.dumps(current))
        simulation = fixture.load('premerge-simulation')
        with patch.object(simulation, 'datetime') as clock:
            clock.now.return_value = datetime(2026, 10, 26, 4, tzinfo=timezone.utc)
            result = simulation.simulate(args)
            for key in ('publication_authority', 'technical_rc_created', 'production_signature_exercised', 'live_policy_audit_exercised'):
                self.assertIs(result[key], False)
            self.assertIs(result['production_promotion_rejected'], True)
            for mutation in ('digest', 'run', 'expired', 'record'):
                with self.subTest(mutation=mutation):
                    changed = copy.copy(args)
                    if mutation == 'digest':
                        changed.manifest_sha256 = 'f' * 64
                    elif mutation == 'run':
                        mutated = dict(current, run_id=9999)
                        mutated['proof_sha256'] = canonical_hash({k: v for k, v in mutated.items() if k != 'proof_sha256'})
                        changed.current_candidate_proof = directory / 'changed.json'
                        changed.current_candidate_proof.write_text(json.dumps(mutated))
                    elif mutation == 'expired':
                        clock.now.return_value = datetime(2026, 10, 29, 4, tzinfo=timezone.utc)
                    else:
                        record_path = Path(args.build_record[0].split('=', 1)[1])
                        record = json.loads(record_path.read_text())
                        record['files'][0]['sha256'] = 'f' * 64
                        record_path.write_text(json.dumps(record))
                    with self.assertRaises(ValueError):
                        simulation.simulate(changed)
                    clock.now.return_value = datetime(2026, 10, 26, 4, tzinfo=timezone.utc)


if __name__ == '__main__':
    unittest.main()
