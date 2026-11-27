"""Offline regressions for route authority, real-run checks and wire isolation."""
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

import test_premerge_rehearsal as acceptance
from candidate_route import CANONICAL, PROFILE, from_args, permit_from_text
from candidate_manifest_evidence import canonical_hash, validate_intent, validate_proof
import candidate_artifact_resolution as resolution
from release_evidence import validate_candidate_manifest
from workflow_run_input_boundary import validate_no_direct_input_expressions

ROOT = acceptance.ROOT
SHA = acceptance.SHA


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/release' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class EvidenceTest(unittest.TestCase):
    def setUp(self):
        acceptance.PremergeRehearsalTest.setUp(self)
        self.root = Path(self.directory.name)
        self.route = from_args(self.args)
        self.contract_bytes = (ROOT / '.github/release/candidate-contract.json').read_bytes()
        self.contract = json.loads(self.contract_bytes)
        self.verifier = load('verify-fast-run')

    def run_fixture(self, kind='fast', route=None):
        route = route or self.route
        contracted = self.contract[kind + '_run']
        repo = dict(self.contract['repository'])
        run = dict(id=1234 if kind == 'fast' else 5678, workflow_id=contracted['workflow_id'],
            path=contracted['workflow_path'], name=contracted['workflow_name'],
            event=contracted['event'], head_branch=route.branch, head_sha=SHA,
            run_attempt=1, status='completed', conclusion='success',
            repository=repo, head_repository=repo, run_started_at='2026-10-26T01:00:00Z')
        conclusions = {**dict.fromkeys(contracted['success_jobs'], 'success'),
                       **dict.fromkeys(contracted['skipped_jobs'], 'skipped'),
                       **{key: values[0] for key, values in contracted['allowed_jobs'].items()}}
        labels = {name: label for label, names in self.contract['runner_policy']['jobs_by_label'].items() for name in names}
        jobs = []
        for identifier, (name, conclusion) in enumerate(conclusions.items(), 1):
            virtual = name in self.contract['runner_policy']['non_runner_jobs']
            assigned = conclusion != 'skipped' and not virtual
            jobs.append(dict(id=identifier, name=name, status='completed',
                conclusion=conclusion, run_id=run['id'], run_attempt=1,
                head_sha=SHA, workflow_name=run['name'],
                labels=[] if virtual else [labels[name]],
                runner_id=identifier if assigned else None,
                runner_name=f'GitHub Actions {identifier}' if assigned else None,
                runner_group_id=0 if assigned else None,
                runner_group_name='GitHub Actions' if assigned else None,
                created_at='2026-10-26T01:00:00Z', started_at='2026-10-26T01:01:00Z',
                completed_at='2026-10-26T01:02:00Z'))
        args = argparse.Namespace(repository='Helvesec/rmux', run_id=run['id'],
            expected_source_sha=SHA, kind=kind, now='2026-10-26T02:00:00Z',
            contract_blob_oid='e' * 40, fixture_mode=True,
            candidate_profile=route.profile,
            rehearsal_permit=self.permit if route.rehearsal else None)
        return args, repo, run, jobs

    def verify(self, fixture):
        args, repo, run, jobs = fixture
        return self.verifier.verify(args, self.contract_bytes, self.contract, repo, run, jobs)

    def test_permit_is_exact_typed_and_never_self_authorizing(self):
        value = json.loads(self.permit.read_text())
        mutations = {'schema_version': [True, 2], 'repository_id': [0, True],
            'profile': ['canonical', 'other'], 'source_ref': ['refs/heads/main', 'refs/tags/v0.11.0'],
            'source_git_sha': ['bad', 'A' * 40], 'publication_authority': [True, 0],
            'scrub_report_sha256': ['', None], 'independent_review_sha256': ['', False],
            'allowed_release_refs': [['v0.11.0-rc.1'], ['v0.11.0-rc.1', 'v0.11.0-rc.3']]}
        for key, values in mutations.items():
            for replacement in values:
                with self.subTest(key=key, value=replacement):
                    changed = dict(value, **{key: replacement})
                    with self.assertRaises(ValueError):
                        permit_from_text(json.dumps(changed))
        for key in value:
            changed = dict(value)
            del changed[key]
            with self.subTest(missing=key), self.assertRaises(ValueError):
                permit_from_text(json.dumps(changed))
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            permit_from_text(self.permit.read_text().replace('{', '{"profile":"canonical",', 1))
        with self.assertRaises(ValueError):
            from_args(argparse.Namespace(candidate_profile='canonical', rehearsal_permit=self.permit))

    def test_entry_checks_approved_identity_before_any_fetch(self):
        prepare = load('prepare-candidate-route')
        env = dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_ATTEMPT='1',
            GITHUB_REPOSITORY='Helvesec/rmux', GITHUB_REPOSITORY_ID='1239918790',
            GITHUB_SHA=SHA, GITHUB_REF=self.route.source_ref,
            RMUX_REHEARSAL_PERMIT=self.permit.read_text())
        args = argparse.Namespace(**vars(self.args), permit_output=self.root / 'admitted.json',
                                  github_output=self.root / 'outputs')
        outputs = [SHA + '\n', SHA + ' ' + 'c' * 40 + '\n', 'c' * 40 + '\n']
        with patch.dict(os.environ, env, clear=True), \
             patch.object(prepare.subprocess, 'check_output', side_effect=outputs), \
             patch.object(prepare.subprocess, 'run') as fetch:
            prepare.prepare(args)
        fetch.assert_called_once_with(['git', 'fetch', '--no-tags', '--depth=1', 'origin', 'refs/heads/main'], check=True)
        self.assertIn(self.route.source_ref, args.github_output.read_text())
        for field, value in [('GITHUB_REF', 'refs/heads/main'),
                             ('GITHUB_REF', 'refs/pull/1/merge'),
                             ('GITHUB_REPOSITORY_ID', '99'),
                             ('GITHUB_EVENT_NAME', 'pull_request'),
                             ('GITHUB_RUN_ATTEMPT', '2'),
                             ('GITHUB_SHA', 'f' * 40), ('RMUX_REHEARSAL_PERMIT', '')]:
            with self.subTest(field=field, value=value), \
                 patch.dict(os.environ, dict(env, **{field: value}), clear=True), \
                 patch.object(prepare.subprocess, 'check_output', return_value=SHA), \
                 patch.object(prepare.subprocess, 'run') as fetch:
                with self.assertRaises(ValueError):
                    prepare.prepare(args)
                fetch.assert_not_called()
        for parents, public in [(SHA, 'c' * 40),
                                (SHA + ' ' + 'c' * 40 + ' ' + 'd' * 40, 'c' * 40),
                                (SHA + ' ' + 'c' * 40, 'd' * 40)]:
            with self.subTest(parents=parents, public=public), \
                 patch.dict(os.environ, env, clear=True), \
                 patch.object(prepare.subprocess, 'check_output', side_effect=[SHA, parents, public]), \
                 patch.object(prepare.subprocess, 'run'):
                with self.assertRaises(ValueError):
                    prepare.prepare(args)

    def test_complete_run_inventory_keeps_fixture_status_and_main_wire(self):
        for kind in ('fast', 'candidate'):
            for route in (CANONICAL, self.route):
                with self.subTest(kind=kind, route=route.profile):
                    proof = self.verify(self.run_fixture(kind, route))
                    self.assertEqual(proof['schema_version'], 2 if route.rehearsal else 1)
                    self.assertIs(proof['test_fixture'], True)
                    with self.assertRaisesRegex(ValueError, 'live GitHub evidence'):
                        validate_proof(proof, kind, 'fixture', route)
                    if route.rehearsal:
                        with self.assertRaises(ValueError):
                            validate_proof(proof, kind, 'canonical consumer')
                    else:
                        self.assertNotIn('candidate_profile', proof)

    def test_runs_reject_neighbor_refs_forks_reruns_stale_and_incomplete_jobs(self):
        for kind in ('fast', 'candidate'):
            original = self.run_fixture(kind)
            mutations = [
                lambda f: f[2].update(head_branch='main'),
                lambda f: f[2].update(head_branch='other'),
                lambda f: f[2].update(head_branch='v0.11.0-rc.1'),
                lambda f: f[2].update(event='pull_request'),
                lambda f: f[2].update(head_repository={'id': 99, 'full_name': 'example/rmux'}),
                lambda f: f[2].update(head_sha='f' * 40),
                lambda f: f[2].update(run_attempt=2),
                lambda f: f[2].update(conclusion='failure'),
                lambda f: setattr(f[0], 'now', '2026-10-29T02:00:00Z'),
                lambda f: f[3].pop(),
                lambda f: f[3].append(copy.deepcopy(f[3][0])),
                lambda f: f[3][0].update(labels=['self-hosted']),
                lambda f: f[3][0].update(head_sha='f' * 40),
                lambda f: f[3][0].update(run_attempt=2),
            ]
            for index, mutate in enumerate(mutations):
                fixture = copy.deepcopy(original)
                mutate(fixture)
                with self.subTest(kind=kind, mutation=index), self.assertRaises(ValueError):
                    self.verify(fixture)
        main = self.run_fixture(route=CANONICAL)
        main[2]['head_branch'] = 'release/0.11.0'
        with self.assertRaises(ValueError):
            self.verify(main)

    def test_resolution_version_two_cannot_enter_canonical_downloads(self):
        _, repo, run, _ = self.run_fixture('candidate')
        artifacts = [dict(id=index, name=spec.name, digest='sha256:' + 'd' * 64,
            expired=False, size_in_bytes=42, workflow_run=dict(id=5678,
                repository_id=1239918790, head_repository_id=1239918790, head_sha=SHA))
            for index, spec in enumerate(resolution.expected_specs(SHA), 1)]
        request = resolution.ResolutionRequest('Helvesec/rmux', 5678, SHA, route=self.route)
        with patch.object(resolution, 'load_resolution_inputs', return_value=(repo, run, artifacts, True)):
            value = resolution.resolve_candidate_artifacts(request)
        path = self.root / 'resolution.json'
        path.write_text(json.dumps(value))
        self.assertEqual(resolution.load_candidate_resolution(path, 5678, SHA, self.route), value)
        with self.assertRaises(ValueError):
            resolution.load_candidate_resolution(path, 5678, SHA)
        for field, replacement in [('schema_version', 1), ('publication_authority', True),
                                   ('rehearsal_permit_sha256', 'a' * 64), ('source_ref', 'refs/heads/main')]:
            with self.subTest(field=field):
                path.write_text(json.dumps(dict(value, **{field: replacement})))
                with self.assertRaises(ValueError):
                    resolution.load_candidate_resolution(path, 5678, SHA, self.route)

    def test_intent_and_fast_directory_bind_profile_digest_and_live_status(self):
        verifier = load('verify-candidate-artifacts')
        value = acceptance.intent.validate(self.args)
        validate_intent(value, self.route)
        with self.assertRaises(ValueError):
            validate_intent(value)
        proof = self.verify(self.run_fixture())
        # Only an in-memory protocol fixture: no hosted run or RC is claimed.
        proof['test_fixture'] = False
        proof['proof_sha256'] = canonical_hash({k: v for k, v in proof.items() if k != 'proof_sha256'})
        directory = self.root / 'fast'
        directory.mkdir()
        intent_path = directory / 'candidate-intent.json'
        proof_path = directory / 'fast-proof.json'
        intent_path.write_text(json.dumps(value))
        proof_path.write_text(json.dumps(proof))
        (directory / 'fast-nextest-artifact.json').write_text(json.dumps(dict(
            artifact_id=8, name='rmux-windows-nextest-' + SHA, digest='sha256:' + 'd' * 64,
            size_in_bytes=42, run_id=1234, source_git_sha=SHA)))
        def verify(route=self.route):
            return verifier.verify_fast_directory(directory, SHA, 1234,
                self.args.release_intent_id, self.args.planned_release_ref, 'rc', route)
        verify()
        with self.assertRaises(ValueError):
            verify(CANONICAL)
        for field, replacement in [('test_fixture', True), ('source_git_sha', 'f' * 40),
                                   ('schema_version', 1), ('source_ref', 'refs/heads/main'),
                                   ('rehearsal_permit_sha256', 'a' * 64)]:
            changed = dict(proof, **{field: replacement})
            changed['proof_sha256'] = canonical_hash({k: v for k, v in changed.items() if k != 'proof_sha256'})
            proof_path.write_text(json.dumps(changed))
            with self.subTest(field=field), self.assertRaises(ValueError):
                verify()

    def test_rehearsal_has_no_production_capability_and_safe_workflow_inputs(self):
        paths = [ROOT / '.github/workflows' / name for name in (
            'premerge-rehearsal.yml', 'ci.yml', 'release-shadow.yml')]
        validate_no_direct_input_expressions(paths)
        text = paths[0].read_text()
        for forbidden in ('secrets:', 'secrets: inherit', 'contents: write', 'id-token: write',
                          'packages: write', 'release-promote.yml', 'sign-and-push-release-tag.sh'):
            self.assertNotIn(forbidden, text)
        for required in ('--deny-self-hosted-runners', '--source-digest "$source_sha"',
                         '--signer-workflow Helvesec/rmux/.github/workflows/canonical-native-build.yml'):
            self.assertIn(required, (ROOT / 'scripts/release/verify-candidate-attestations.sh').read_text())
        for name in ('release-shadow.yml', 'release-promotion-simulation.yml'):
            text = (ROOT / '.github/workflows' / name).read_text()
            self.assertIn("if: inputs.candidate_profile == 'canonical'", text)
            self.assertIn('test "$GITHUB_REF" = refs/heads/main', text)
            self.assertIn("if: inputs.candidate_profile == 'premerge-rehearsal-v1'", text)
        manifest = dict(self.route.fields, repository_id=1239918790,
            source_git_sha=SHA, candidate_run_id=5678, candidate_run_attempt=1,
            release_intent_id=self.args.release_intent_id, planned_release_ref='v0.11.0-rc.1',
            release_kind='rc', release_version='0.11.0-rc.1', package_version='0.11.0',
            is_prerelease=True, release_policy={}, created_at='', expires_at='', artifacts=[])
        with self.assertRaisesRegex(ValueError, 'cannot enter production'):
            validate_candidate_manifest(manifest)
        manifest['schema_version'] = 1
        with self.assertRaisesRegex(ValueError, 'cannot enter production'):
            validate_candidate_manifest(manifest)


if __name__ == '__main__':
    unittest.main()
