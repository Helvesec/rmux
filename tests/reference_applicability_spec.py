"""Prospective receipt/source falsifiers; no harness import or native effects."""
import copy
import hashlib
import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/validation'))
import reference_applicability as applicability
import contracts


class ProspectiveApplicability(unittest.TestCase):
    def setUp(self):
        self.policy = json.loads(contracts.read_reference(ROOT, applicability.POLICY))
        self.entry = self.policy['prospective']
        self.binding = self.entry['binding']

    def check(self):
        return applicability.validate_prospective(self.policy)

    def reject(self, error):
        with self.assertRaisesRegex(ValueError, error):
            self.check()

    def receipt_change(self, name, change):
        ref = self.entry['evidence'][name]
        value = json.loads(ref['text'])
        change(value)
        raw = (json.dumps(value, indent=2) + '\n').encode()
        ref.update(text=raw.decode(), bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())

    def test_exact_reviewed_binding_is_not_native_or_integration_credit(self):
        self.assertEqual(self.check(), self.binding)
        self.assertFalse(self.binding['native_credit'])
        self.assertEqual(self.binding['integration'],
                         'PENDING_INDEPENDENT_APPLICABILITY_REVIEW_AND_UNIQUE_INTEGRATOR')

    def test_complete_previous_policy_and_receipts_preserved(self):
        applicability.validate_policy(self.policy)
        previous = {k: v for k, v in self.policy.items() if k != 'prospective'}
        digest = hashlib.sha256((json.dumps(previous, indent=2) + '\n').encode()).hexdigest()
        self.assertEqual(digest, '7285d26d2553325aafc7450f087ada1a083d77a8cd87c6bdac3aca8a713813aa')

    def test_rewritten_review_bytes_rejected(self):
        self.entry['evidence']['review']['text'] += ' '
        self.reject('receipt bytes differ')

    def test_wrong_source_same_digest_rejected(self):
        self.binding['candidate']['commit'] = '0' * 40
        self.reject('prospective candidate source differs')

    def test_wrong_tree_rejected(self):
        self.binding['candidate']['tree'] = '0' * 40
        self.reject('prospective candidate source differs')

    def test_wrong_authority_rejected(self):
        self.binding['review']['job_id'] = 'R2-999'
        self.reject('prospective authority differs')

    def test_wrong_review_receipt_rejected(self):
        self.binding['review']['receipt_sha256'] = '0' * 64
        self.reject('prospective review date or digest differs')

    def test_wrong_review_date_rejected(self):
        self.binding['review']['completed_at'] = '2030-01-01T00:00:00+00:00'
        self.reject('prospective review date or digest differs')

    def test_rehashed_author_receipt_cannot_break_author_result_join(self):
        self.receipt_change('author_candidate', lambda v: v.update(commit='0' * 40))
        self.reject('prospective receipt authority differs')

    def test_rehashed_review_cannot_upgrade_acceptance(self):
        self.receipt_change('review', lambda v: v.update(whole_criterion_closed=True))
        self.reject('prospective acceptance scope')

    def test_prior_chain_is_not_replaceable(self):
        self.policy['binding']['review']['job_id'] = 'R2-999'
        self.reject('prospective prior policy differs')

    def test_companion_population_required(self):
        self.binding['source_files'].pop()
        self.reject('prospective source population differs')

    def test_companion_digest_must_equal_source_proof(self):
        self.binding['source_files'][0]['sha256'] = '0' * 64
        self.reject('prospective file proof differs')

    def test_qualified_base_cannot_be_reattributed(self):
        self.binding['qualified_base']['commit'] = '0' * 40
        self.reject('prospective qualified source differs')

    def test_source_proof_cannot_postdate_review(self):
        self.entry['source_proof']['candidate_files'][0]['commit_date'] = '2030-01-01T00:00:00+00:00'
        self.reject('prospective chronology')

    def test_semantic_mapping_must_include_new_controls(self):
        self.entry['semantic_mapping'].pop()
        self.reject('prospective semantic mapping differs')

    def test_native_failures_cannot_be_promoted(self):
        for key, value in [('harness_PASS', 12), ('harness_FAIL', 0), ('R5', 'PASS'),
                           ('R6', 'PASS'), ('complete_history', True), ('release_artifact', True),
                           ('package_source', self.binding['candidate']['commit'])]:
            with self.subTest(key=key):
                original = self.entry['limits'][key]
                self.entry['limits'][key] = value
                self.reject('prospective native limits differ')
                self.entry['limits'][key] = original

    def test_attempt_units_and_gross_calls_remain_distinct(self):
        for key, value in [('qualification_attempts', 162), ('driver_calls', 24),
                           ('qualification_cap', 162), ('attempt_unit_rewritten', True)]:
            with self.subTest(key=key):
                original = self.entry['limits'][key]
                self.entry['limits'][key] = value
                self.reject('prospective attempt accounting differs')
                self.entry['limits'][key] = original

    def test_false_future_integration_cannot_be_claimed(self):
        self.binding['integration'] = 'INTEGRATED'
        self.reject('prospective acceptance scope')

    def test_full_policy_digest_stays_owned_by_checker(self):
        raw = contracts.read_reference(ROOT, applicability.POLICY).encode()
        self.assertEqual(hashlib.sha256(raw).hexdigest(), applicability.POLICY['sha256'])


if __name__ == '__main__':
    unittest.main()
