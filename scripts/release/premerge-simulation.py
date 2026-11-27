#!/usr/bin/env python3
"""Recheck a sealed rehearsal and prove it cannot enter real promotion.

This profile exercises live candidate bytes and production rejection. It does
not synthesize a signed tag, RC release, policy-audit approval or publication.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
from datetime import datetime, timezone

from candidate_route import PROFILE, add_arguments, from_args
from candidate_manifest_evidence import validate_proof, timestamp
from release_evidence import validate_candidate_manifest

ROOT = Path(__file__).resolve().parents[2]


def read(path: Path) -> dict:
    value = json.loads(path.read_text(encoding='utf-8'))
    if not isinstance(value, dict):
        raise ValueError('simulation evidence must be a JSON object')
    return value


def simulate(args: argparse.Namespace) -> dict:
    route = from_args(args)
    if not route.rehearsal:
        raise ValueError('premerge simulation requires an explicit rehearsal permit')
    manifest_path = args.sealed_dir / 'candidate-manifest.json'
    raw = manifest_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != args.manifest_sha256:
        raise ValueError('sealed manifest digest differs')
    manifest = read(manifest_path)
    route.validate_record(manifest)
    route.validate_intent(manifest['source_git_sha'], manifest['release_intent_id'],
                          manifest['planned_release_ref'], manifest['release_kind'])
    original = read(args.sealed_dir / 'candidate-run-proof.json')
    current = read(args.current_candidate_proof)
    validate_proof(original, 'candidate', 'sealed candidate proof', route)
    validate_proof(current, 'candidate', 'current candidate proof', route)
    # Verification time may advance; the run, job set and all provenance stay exact.
    ignored = {'verified_at', 'proof_sha256'}
    if ({k: v for k, v in current.items() if k not in ignored}
            != {k: v for k, v in original.items() if k not in ignored}):
        raise ValueError('candidate run changed after shadow sealing')
    if timestamp(current['verified_at'], 'current verification') < timestamp(original['verified_at'], 'original verification'):
        raise ValueError('current proof predates sealing')
    if datetime.now(timezone.utc) > timestamp(manifest['expires_at'], 'manifest expiry'):
        raise ValueError('sealed rehearsal has expired')
    spec = importlib.util.spec_from_file_location('rehearsal_manifest',
                        ROOT / 'scripts/release/candidate-manifest.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    args.candidate_proof = args.sealed_dir / 'candidate-run-proof.json'
    expected = module.build_manifest(args)
    if manifest != expected:
        raise ValueError('sealed rehearsal differs from the original evidence and current bytes')
    try:
        validate_candidate_manifest(manifest)
    except ValueError:
        pass
    else:
        raise ValueError('production promotion unexpectedly accepted rehearsal evidence')
    return {
        **route.fields,
        'status': 'premerge-promotion-boundary-verified',
        'source_git_sha': manifest['source_git_sha'],
        'release_intent_id': manifest['release_intent_id'],
        'planned_release_ref': manifest['planned_release_ref'],
        'candidate_run_id': manifest['candidate_run_id'],
        'manifest_sha256': args.manifest_sha256,
        'production_promotion_rejected': True,
        'repository_mutations': False,
        'technical_rc_created': False,
        'production_signature_exercised': False,
        'live_policy_audit_exercised': False,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    add_arguments(parser)
    for name in ('sealed-dir', 'current-candidate-proof', 'fast-proof',
                 'candidate-intent', 'policy-root', 'canonical-artifacts', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--manifest-sha256', required=True)
    parser.add_argument('--build-record', action='append')
    parser.add_argument('--artifact-binding', action='append')
    args = parser.parse_args()
    args.output.write_text(json.dumps(simulate(args), indent=2, sort_keys=True) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
