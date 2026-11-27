"""Frozen acceptance for the bounded, nonpublishing premerge route."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/release'))
spec = importlib.util.spec_from_file_location('candidate_intent', ROOT / 'scripts/release/validate-candidate-intent.py')
intent = importlib.util.module_from_spec(spec)
spec.loader.exec_module(intent)
SHA = '0123456789abcdef0123456789abcdef01234567'


class PremergeRehearsalTest(unittest.TestCase):
    def setUp(self):
        audit = ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.directory.cleanup)
        self.permit = Path(self.directory.name) / 'permit.json'
        self.permit.write_text(json.dumps({
            'schema_version': 1, 'profile': 'premerge-rehearsal-v1',
            'repository_id': 1239918790, 'source_git_sha': SHA,
            'source_ref': 'refs/heads/release/0.11.0',
            'release_intent_id': 'rehearsal-0.11.0',
            'allowed_release_refs': ['v0.11.0-rc.1', 'v0.11.0-rc.2'],
            'scrub_report_sha256': 'a' * 64,
            'independent_review_sha256': 'b' * 64,
            'publication_authority': False,
        }))
        self.args = argparse.Namespace(repository_root=ROOT,
            expected_source_sha=SHA, actual_source_sha=SHA,
            fast_run_id=1234, release_intent_id='rehearsal-0.11.0',
            planned_release_ref='v0.11.0-rc.1', release_kind='rc',
            github_ref='refs/heads/release/0.11.0', github_run_attempt=1,
            candidate_profile='premerge-rehearsal-v1', rehearsal_permit=self.permit)

    def test_exact_authorized_rehearsal_is_explicitly_nonpublishing(self):
        for ref in ('v0.11.0-rc.1', 'v0.11.0-rc.2'):
            with self.subTest(ref=ref):
                self.args.planned_release_ref = ref
                result = intent.validate(self.args)
                self.assertEqual(result['schema_version'], 2)
                self.assertEqual(result['candidate_profile'], 'premerge-rehearsal-v1')
                self.assertIs(result['publication_authority'], False)
                self.assertEqual(result['source_git_sha'], SHA)

    def test_mutated_branch_sha_ref_intent_or_authority_is_rejected(self):
        mutations = {
            'github_ref': ['refs/heads/main', 'refs/heads/other', 'refs/tags/v0.11.0', 'refs/pull/1/merge'],
            'expected_source_sha': ['f' * 40],
            'actual_source_sha': ['f' * 40],
            'planned_release_ref': ['v0.11.0', 'v0.11.0-rc.3', 'v0.12.0-rc.1'],
            'release_intent_id': ['different-intent'],
            'release_kind': ['stable', 'shadow'],
            'github_run_attempt': [2],
            'fast_run_id': [0],
            'rehearsal_permit': [None],
        }
        for field, values in mutations.items():
            for value in values:
                with self.subTest(field=field, value=value):
                    args = argparse.Namespace(**vars(self.args))
                    setattr(args, field, value)
                    with self.assertRaises(ValueError):
                        intent.validate(args)
        args = argparse.Namespace(**vars(self.args))
        args.expected_source_sha = args.actual_source_sha = 'f' * 40
        with self.assertRaises(ValueError):
            intent.validate(args)

    def test_canonical_main_keeps_version_one_and_rejects_other_refs(self):
        self.args.candidate_profile = 'canonical'
        self.args.rehearsal_permit = None
        self.args.github_ref = 'refs/heads/main'
        result = intent.validate(self.args)
        self.assertEqual(result['schema_version'], 1)
        self.assertNotIn('candidate_profile', result)
        for ref in ('refs/heads/release/0.11.0', 'refs/tags/v0.11.0', 'refs/pull/1/merge'):
            self.args.github_ref = ref
            with self.assertRaises(ValueError):
                intent.validate(self.args)


if __name__ == '__main__':
    unittest.main()
