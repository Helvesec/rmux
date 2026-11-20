#!/usr/bin/env python3
"""Bind a workflow entry to canonical main or a reviewed squash rehearsal."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess

from candidate_route import CANONICAL, PROFILE, from_args, permit_from_text


def prepare(args: argparse.Namespace) -> None:
    if (os.environ.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
            or os.environ.get('GITHUB_RUN_ATTEMPT') != '1'
            or os.environ.get('GITHUB_REPOSITORY') != 'Helvesec/rmux'
            or os.environ.get('GITHUB_REPOSITORY_ID') != '1239918790'):
        raise ValueError('candidate entry requires the original repository and attempt 1 dispatch')
    if os.environ.get('GITHUB_SHA') != args.expected_source_sha:
        raise ValueError('dispatch source SHA differs')
    actual = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if actual != args.expected_source_sha:
        raise ValueError('checkout source SHA differs')
    route = CANONICAL
    permit_path = ''
    if args.candidate_profile == PROFILE:
        # Only a repository configuration variable populates this environment
        # value in the workflows. A dispatch cannot supply or replace it.
        value = permit_from_text(os.environ.get('RMUX_REHEARSAL_PERMIT', ''))
        if (value['source_git_sha'] != actual
                or value['release_intent_id'] != args.release_intent_id
                or args.release_kind != 'rc'
                or args.planned_release_ref not in value['allowed_release_refs']
                or os.environ.get('GITHUB_REF') != value['source_ref']):
            raise ValueError('dispatch is outside the reviewed rehearsal permit')
        parents = subprocess.check_output(
            ['git', 'rev-list', '--parents', '-n', '1', 'HEAD'], text=True).split()
        if len(parents) != 2:
            raise ValueError('rehearsal requires exactly one squash parent')
        subprocess.run(['git', 'fetch', '--no-tags', '--depth=1', 'origin',
                        'refs/heads/main'], check=True)
        public_main = subprocess.check_output(
            ['git', 'rev-parse', 'FETCH_HEAD'], text=True).strip()
        if parents[1] != public_main:
            raise ValueError('rehearsal squash must be directly on public main')
        args.permit_output.write_text(json.dumps(value, sort_keys=True) + '\n', encoding='utf-8')
        permit_path = str(args.permit_output)
        route = from_args(argparse.Namespace(candidate_profile=PROFILE,
                                             rehearsal_permit=args.permit_output))
    if os.environ.get('GITHUB_REF') != route.source_ref:
        raise ValueError('dispatch ref differs from the selected candidate route')
    route.validate_intent(actual, args.release_intent_id,
                          args.planned_release_ref, args.release_kind)
    with args.github_output.open('a', encoding='utf-8') as output:
        output.write(f'source_ref={route.source_ref}\nrehearsal_permit_file={permit_path}\n')


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--candidate-profile', choices=('canonical', PROFILE), required=True)
    for name in ('expected-source-sha', 'release-intent-id', 'planned-release-ref', 'release-kind'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--permit-output', type=Path, required=True)
    parser.add_argument('--github-output', type=Path, required=True)
    prepare(parser.parse_args())


if __name__ == '__main__':
    main()
