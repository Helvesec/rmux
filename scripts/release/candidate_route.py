"""Separate canonical evidence from an explicitly permitted premerge rehearsal.

The permit is supplied by repository configuration after the maintainer has
verified the squash and independent scrub review. It is never a workflow_dispatch input.
It grants no publication, tag, promotion, or production signing authority.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
from typing import Any

PROFILE = 'premerge-rehearsal-v1'
REF = 'refs/heads/release/0.11.0'
REPOSITORY_ID = 1239918790
RC_REFS = ('v0.11.0-rc.1', 'v0.11.0-rc.2')
PERMIT_KEYS = {
    'schema_version', 'profile', 'repository_id', 'source_git_sha',
    'source_ref', 'release_intent_id', 'allowed_release_refs',
    'scrub_report_sha256', 'independent_review_sha256', 'publication_authority',
}


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f'duplicate permit key: {key}')
        result[key] = value
    return result


def permit_from_text(raw: str) -> dict[str, Any]:
    try:
        value = json.loads(raw, object_pairs_hook=unique_object)
    except (TypeError, json.JSONDecodeError) as error:
        raise ValueError('invalid rehearsal permit JSON') from error
    if not isinstance(value, dict) or set(value) != PERMIT_KEYS:
        raise ValueError('rehearsal permit keys changed')
    if (type(value['schema_version']) is not int or value['schema_version'] != 1
            or type(value['repository_id']) is not int
            or value['repository_id'] != REPOSITORY_ID
            or value['profile'] != PROFILE or value['source_ref'] != REF
            or value['publication_authority'] is not False):
        raise ValueError('rehearsal permit identity or authority changed')
    for field, pattern in (
        ('source_git_sha', r'[0-9a-f]{40}'),
        ('release_intent_id', r'[A-Za-z0-9._:-]{8,128}'),
        ('scrub_report_sha256', r'[0-9a-f]{64}'),
        ('independent_review_sha256', r'[0-9a-f]{64}'),
    ):
        if not isinstance(value[field], str) or not re.fullmatch(pattern, value[field]):
            raise ValueError(f'invalid rehearsal permit {field}')
    if value['allowed_release_refs'] != list(RC_REFS):
        raise ValueError('rehearsal is bounded to the first two 0.11.0 RC intents')
    return value


@dataclass(frozen=True)
class CandidateRoute:
    profile: str = 'canonical'
    source_sha: str = ''
    intent_id: str = ''
    permit_sha256: str = ''

    def __post_init__(self) -> None:
        if self.profile not in {'canonical', PROFILE}:
            raise ValueError('unknown candidate route')

    @property
    def rehearsal(self) -> bool:
        return self.profile == PROFILE

    @property
    def source_ref(self) -> str:
        return REF if self.rehearsal else 'refs/heads/main'

    @property
    def branch(self) -> str:
        return self.source_ref.removeprefix('refs/heads/')

    @property
    def fields(self) -> dict[str, Any]:
        if not self.rehearsal:
            return {'schema_version': 1}
        return {'schema_version': 2, 'candidate_profile': PROFILE,
                'source_ref': REF, 'rehearsal_permit_sha256': self.permit_sha256,
                'publication_authority': False}

    @property
    def extra_keys(self) -> set[str]:
        return set(self.fields) - {'schema_version'}

    def validate_record(self, value: dict[str, Any]) -> None:
        for key, expected in self.fields.items():
            if type(value.get(key)) is not type(expected) or value[key] != expected:
                raise ValueError(f'candidate route {key} differs')
        if not self.rehearsal and self.extra_fields_present(value):
            raise ValueError('canonical record cannot carry rehearsal authority')
        if self.rehearsal:
            for field in ('repository_id', 'run_id', 'fast_run_id', 'candidate_run_id',
                          'run_attempt', 'candidate_run_attempt'):
                if field in value and (type(value[field]) is not int or value[field] <= 0):
                    raise ValueError(f'rehearsal {field} must be a positive integer')
        if 'source_git_sha' in value:
            self.validate_source(value['source_git_sha'])

    @staticmethod
    def extra_fields_present(value: dict[str, Any]) -> bool:
        return bool({'candidate_profile', 'source_ref', 'rehearsal_permit_sha256',
                     'publication_authority'} & set(value))

    def validate_source(self, source_sha: str) -> None:
        if self.rehearsal and source_sha != self.source_sha:
            raise ValueError('source SHA is outside the rehearsal permit')

    def validate_intent(self, source_sha: str, intent_id: str,
                        release_ref: str, release_kind: str) -> None:
        self.validate_source(source_sha)
        if self.rehearsal and (
            intent_id != self.intent_id or release_kind != 'rc'
            or release_ref not in RC_REFS
        ):
            raise ValueError('release intent is outside the rehearsal permit')


CANONICAL = CandidateRoute()


def from_args(args: argparse.Namespace) -> CandidateRoute:
    profile = getattr(args, 'candidate_profile', 'canonical')
    permit = getattr(args, 'rehearsal_permit', None)
    if profile == 'canonical':
        if permit:
            raise ValueError('canonical route does not consume a rehearsal permit')
        return CANONICAL
    if profile != PROFILE or not permit:
        raise ValueError('explicit rehearsal profile and permit are required')
    try:
        value = permit_from_text(Path(permit).read_text(encoding='utf-8'))
    except OSError as error:
        raise ValueError('rehearsal permit cannot be read') from error
    digest = hashlib.sha256(json.dumps(value, sort_keys=True,
                           separators=(',', ':')).encode()).hexdigest()
    return CandidateRoute(PROFILE, value['source_git_sha'],
                          value['release_intent_id'], digest)


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument('--candidate-profile', choices=('canonical', PROFILE),
                        default='canonical')
    parser.add_argument('--rehearsal-permit', type=lambda value: Path(value) if value else None)


def validate_contract(root: Path) -> None:
    value = json.loads((root / '.github/release/premerge-rehearsal-contract.json').read_text())
    expected = {
        'schema_version': 1, 'profile': PROFILE, 'repository_id': REPOSITORY_ID,
        'source_ref': REF, 'package_version': '0.11.0',
        'allowed_release_refs': list(RC_REFS),
        'permit_repository_variable': 'RMUX_PREMERGE_REHEARSAL_PERMIT',
        'permit_requires': ['exact-squash-sha', 'automatic-scrub-report-sha256',
                            'independent-scrub-review-sha256'],
        'public_main_parent_required': True, 'run_attempt': 1, 'proof_schema_version': 2,
        'fast_run_event': 'push', 'fast_run_branch': 'release/0.11.0',
        'entry_workflows': ['.github/workflows/ci.yml', '.github/workflows/release-shadow.yml',
                            '.github/workflows/release-promotion-simulation.yml'],
        'verification_workflow': '.github/workflows/premerge-rehearsal.yml',
        'publication_authority': False, 'production_signing_authority': False,
        'technical_rc_created': False,
        'promotion_simulation': 'revalidate-live-candidate-and-reject-production-promotion',
        'hosted_obligations': ['complete-fast-run', 'complete-release-delta',
                               'five-canonical-platforms', 'build-provenance-signatures',
                               'shadow-seal', 'premerge-promotion-boundary'],
        'separate_release_obligations': ['technical-rc-and-assets', 'live-production-policy-audit',
                                         'production-tag-signature', 'sponsor-main-merge-and-tag'],
    }
    # Compare serialized JSON too: bool/int equality must not relax the contract.
    if json.dumps(value, sort_keys=True) != json.dumps(expected, sort_keys=True):
        raise ValueError('premerge rehearsal contract drifted')
