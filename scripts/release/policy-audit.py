#!/usr/bin/env python3
"""Collect or verify one exact, short-lived, disarmed policy-audit proof."""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from policy_audit_contract import API_GETS, validate_contract
from policy_audit_model import (
    AUDIT_WORKFLOW_STATE_KEYS,
    DIGEST,
    INTENT,
    RELEASE_REF,
    REPOSITORY,
    SHA40,
    SHA64,
    build_predicate,
    build_reference,
    normalize_state,
    read_object,
    validate_policy_root,
    validate_predicate_against_contract,
    validate_reference,
    write_object,
)

API_ROOT = "https://api.github.com"
API_VERSION = "2026-03-10"
MAX_RESPONSE_BYTES = 4 * 1024 * 1024


def get_json(path: str, token: str) -> dict[str, Any]:
    request = Request(
        f"{API_ROOT}{path}",
        method="GET",
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "User-Agent": "rmux-release-policy-audit",
            "X-GitHub-Api-Version": API_VERSION,
        },
    )
    try:
        with urlopen(request, timeout=30) as response:
            payload = response.read(MAX_RESPONSE_BYTES + 1)
    except (HTTPError, URLError, TimeoutError) as error:
        raise ValueError(f"GitHub API GET {path} failed closed") from error
    if len(payload) > MAX_RESPONSE_BYTES:
        raise ValueError(f"GitHub API GET {path} exceeded the response-size limit")
    try:
        value = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"GitHub API GET {path} did not return UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"GitHub API GET {path} did not return an object")
    return value


def load_responses(fixture_dir: Path | None) -> dict[str, dict[str, Any]]:
    paths = dict(API_GETS)
    if fixture_dir is not None:
        return {
            key: read_object(fixture_dir / f"{key}.json", f"{key} fixture")
            for key in paths
        }
    token = os.environ.get("RMUX_POLICY_AUDIT_TOKEN")
    if not token:
        raise ValueError("RMUX_POLICY_AUDIT_TOKEN is required for the live audit")
    return {key: get_json(path, token) for key, path in paths.items()}


def parse_now(value: str | None, fixture_dir: Path | None) -> datetime:
    if value is not None and fixture_dir is None:
        raise ValueError("--now is restricted to offline fixtures")
    if value is None:
        return datetime.now(timezone.utc)
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError("--now is not a valid ISO-8601 timestamp") from error
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ValueError("--now must carry a UTC offset")
    return parsed.astimezone(timezone.utc)


def candidate_binding(args: argparse.Namespace) -> dict[str, Any]:
    return {
        "run_id": args.candidate_run_id,
        "run_attempt": args.candidate_run_attempt,
        "manifest_run_id": args.candidate_manifest_run_id,
        "manifest_run_attempt": args.candidate_manifest_run_attempt,
        "manifest_artifact_id": args.candidate_manifest_artifact_id,
        "manifest_artifact_digest": args.candidate_manifest_artifact_digest,
        "manifest_sha256": args.candidate_manifest_sha256,
        "manifest_created_at": args.candidate_manifest_created_at,
        "manifest_expires_at": args.candidate_manifest_expires_at,
    }


def audit_app_binding(args: argparse.Namespace) -> dict[str, Any]:
    return {
        "app_id": args.audit_app_id,
        "installation_id": args.audit_installation_id,
        "app_slug": args.audit_app_slug,
    }


def collect(args: argparse.Namespace) -> None:
    if args.repository != REPOSITORY:
        raise ValueError(f"repository must be exactly {REPOSITORY}")
    if SHA40.fullmatch(args.source_sha) is None:
        raise ValueError("source SHA must be 40 lowercase hex characters")
    contract = read_object(args.contract, "policy audit contract")
    validate_contract(contract, require_disarmed=False)
    if not contract["audit_app"]["configured"]:
        raise ValueError(
            "policy audit App is unconfigured; live audit remains disarmed"
        )
    app_binding = audit_app_binding(args)
    expected_app = {
        "app_id": contract["audit_app"]["app_id"],
        "installation_id": contract["audit_app"]["installation_id"],
        "app_slug": contract["audit_app"]["app_slug"],
    }
    if app_binding != expected_app:
        raise ValueError("audit App action identity differs from its contract")
    responses = load_responses(args.api_fixture_dir)
    state = normalize_state(responses, contract, app_binding)
    policy = validate_policy_root(
        read_object(args.policy_root, "release policy root"), args.source_sha
    )
    predicate = build_predicate(
        contract=contract,
        source_sha=args.source_sha,
        candidate=candidate_binding(args),
        release={
            "intent_id": args.release_intent_id,
            "planned_ref": args.planned_release_ref,
            "kind": args.release_kind,
        },
        policy=policy,
        audit_run_id=args.audit_run_id,
        audit_run_attempt=args.audit_run_attempt,
        audit_workflow_id=args.audit_workflow_id,
        audit_workflow_path=args.audit_workflow_path,
        observed_state=state,
        now=parse_now(args.now, args.api_fixture_dir),
    )
    validate_predicate_against_contract(predicate, contract)
    write_object(args.output, predicate)


def verify(args: argparse.Namespace) -> None:
    value = read_object(args.predicate, "policy audit predicate")
    contract = read_object(args.contract, "policy audit contract")
    validate_contract(contract, require_disarmed=False)
    validate_predicate_against_contract(value, contract)
    fixture_marker = Path("offline-verification") if args.now is not None else None
    now = parse_now(args.now, fixture_marker)
    expires = datetime.fromisoformat(value["expires_at"].replace("Z", "+00:00"))
    if now >= expires:
        raise ValueError("policy audit predicate is expired")


def create_reference(args: argparse.Namespace) -> None:
    predicate = read_object(args.predicate, "policy audit predicate")
    reference = build_reference(
        predicate, args.predicate_artifact_id, args.predicate_artifact_digest
    )
    write_object(args.output, reference)


def verify_reference(args: argparse.Namespace) -> None:
    predicate = read_object(args.predicate, "policy audit predicate")
    contract = read_object(args.contract, "policy audit contract")
    validate_contract(contract, require_disarmed=False)
    validate_predicate_against_contract(predicate, contract)
    reference = read_object(args.reference, "policy audit reference")
    validate_reference(reference, predicate)
    fixture_marker = Path("offline-verification") if args.now is not None else None
    now = parse_now(args.now, fixture_marker)
    expires = datetime.fromisoformat(reference["expires_at"].replace("Z", "+00:00"))
    if now >= expires:
        raise ValueError("policy audit reference is expired")


def add_identity_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repository", default=REPOSITORY)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--candidate-run-id", required=True, type=int)
    parser.add_argument("--candidate-run-attempt", required=True, type=int)
    parser.add_argument("--candidate-manifest-run-id", required=True, type=int)
    parser.add_argument("--candidate-manifest-run-attempt", required=True, type=int)
    parser.add_argument("--candidate-manifest-artifact-id", required=True, type=int)
    parser.add_argument("--candidate-manifest-artifact-digest", required=True)
    parser.add_argument("--candidate-manifest-sha256", required=True)
    parser.add_argument("--candidate-manifest-created-at", required=True)
    parser.add_argument("--candidate-manifest-expires-at", required=True)
    parser.add_argument("--release-intent-id", required=True)
    parser.add_argument("--planned-release-ref", required=True)
    parser.add_argument("--release-kind", choices=("rc", "stable"), required=True)
    parser.add_argument("--audit-run-id", required=True, type=int)
    parser.add_argument("--audit-run-attempt", required=True, type=int)
    parser.add_argument("--audit-workflow-id", required=True, type=int)
    parser.add_argument("--audit-workflow-path", required=True)
    parser.add_argument("--audit-app-id", required=True, type=int)
    parser.add_argument("--audit-installation-id", required=True, type=int)
    parser.add_argument("--audit-app-slug", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    collect_parser = commands.add_parser("collect")
    add_identity_arguments(collect_parser)
    collect_parser.add_argument("--contract", type=Path, required=True)
    collect_parser.add_argument("--policy-root", type=Path, required=True)
    collect_parser.add_argument("--api-fixture-dir", type=Path)
    collect_parser.add_argument("--now")
    collect_parser.add_argument("--output", type=Path, required=True)
    verify_parser = commands.add_parser("verify")
    verify_parser.add_argument("--predicate", type=Path, required=True)
    verify_parser.add_argument("--contract", type=Path, required=True)
    verify_parser.add_argument("--now")
    reference_parser = commands.add_parser("reference")
    reference_parser.add_argument("--predicate", type=Path, required=True)
    reference_parser.add_argument("--predicate-artifact-id", type=int, required=True)
    reference_parser.add_argument("--predicate-artifact-digest", required=True)
    reference_parser.add_argument("--output", type=Path, required=True)
    verify_reference_parser = commands.add_parser("verify-reference")
    verify_reference_parser.add_argument("--predicate", type=Path, required=True)
    verify_reference_parser.add_argument("--reference", type=Path, required=True)
    verify_reference_parser.add_argument("--contract", type=Path, required=True)
    verify_reference_parser.add_argument("--now")
    return parser.parse_args()


def validate_argument_shapes(args: argparse.Namespace) -> None:
    if args.command != "collect":
        return
    if DIGEST.fullmatch(args.candidate_manifest_artifact_digest) is None:
        raise ValueError("candidate manifest artifact digest is invalid")
    if SHA64.fullmatch(args.candidate_manifest_sha256) is None:
        raise ValueError("candidate manifest SHA-256 is invalid")
    if INTENT.fullmatch(args.release_intent_id) is None:
        raise ValueError("release intent ID is invalid")
    if RELEASE_REF.fullmatch(args.planned_release_ref) is None:
        raise ValueError("planned release ref is invalid")
    if args.audit_workflow_path not in AUDIT_WORKFLOW_STATE_KEYS:
        raise ValueError("policy audit workflow path is not allowed")


def main() -> int:
    args = parse_args()
    validate_argument_shapes(args)
    handlers = {
        "collect": collect,
        "verify": verify,
        "reference": create_reference,
        "verify-reference": verify_reference,
    }
    handlers[args.command](args)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"policy-audit: {error}", file=sys.stderr)
        raise SystemExit(1) from error
