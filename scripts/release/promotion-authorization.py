#!/usr/bin/env python3
"""Create or verify disarmed promotion authorization predicates and envelopes."""

from __future__ import annotations

import argparse
from datetime import timedelta
from pathlib import Path
from typing import Any

from release_evidence import (
    REPOSITORY,
    REPOSITORY_ID,
    STATUS,
    exact_keys,
    file_hash,
    positive_integer,
    publishable_assets,
    read_object,
    timestamp,
    validate_artifact_reference,
    validate_candidate_manifest,
    validate_candidate_reference,
    validate_file,
    validate_policy_audit,
    validate_signed_tag,
    write_object,
)

PREDICATE_TYPE = "https://rmux.io/attestations/release-promotion-authorization/v1"
ENVELOPE_TYPE = "https://rmux.io/envelopes/release-promotion-authorization/v1"
WORKFLOW_PATH = ".github/workflows/release-promote.yml"


def authorization_identity(args: argparse.Namespace) -> dict[str, Any]:
    positive_integer(args.authorization_run_id, "authorization run ID")
    positive_integer(args.authorization_workflow_id, "authorization workflow ID")
    if args.authorization_run_attempt != 1:
        raise ValueError("promotion authorization requires Actions attempt 1")
    return {
        "run_id": args.authorization_run_id,
        "run_attempt": 1,
        "workflow_id": args.authorization_workflow_id,
        "workflow_path": WORKFLOW_PATH,
    }


def expected_predicate(args: argparse.Namespace) -> dict[str, Any]:
    manifest = read_object(args.candidate_manifest, "candidate manifest")
    validate_candidate_manifest(manifest)
    candidate_reference = validate_candidate_reference(
        read_object(args.candidate_reference, "candidate reference"),
        args.candidate_manifest,
        manifest,
    )
    signed_tag = validate_signed_tag(
        read_object(args.signed_tag, "signed tag proof"),
        release_ref=manifest["planned_release_ref"],
        source_git_sha=manifest["source_git_sha"],
        release_intent_id=manifest["release_intent_id"],
        release_kind=manifest["release_kind"],
        candidate=candidate_reference,
        release_policy_sha256=manifest["release_policy"]["sha256"],
    )
    policy_path = validate_file(args.policy_audit_reference, "policy audit reference")
    policy_audit = validate_policy_audit(
        read_object(policy_path, "policy audit reference"), manifest
    )
    authorization = authorization_identity(args)
    if policy_audit["policy_audit_run_id"] != authorization["run_id"]:
        raise ValueError("policy audit and authorization must share one promoter run")
    issued = timestamp(args.issued_at, "authorization issued_at")
    expires = timestamp(args.expires_at, "authorization expires_at")
    candidate_created = timestamp(manifest["created_at"], "candidate created_at")
    candidate_expires = timestamp(manifest["expires_at"], "candidate expires_at")
    tag_verified = timestamp(signed_tag["verified_at"], "tag verified_at")
    audit_emitted = timestamp(policy_audit["emitted_at"], "policy audit emitted_at")
    audit_expires = timestamp(policy_audit["expires_at"], "policy audit expires_at")
    if not (
        candidate_created <= tag_verified <= issued
        and audit_emitted <= issued < audit_expires
        and issued < candidate_expires
    ):
        raise ValueError("authorization evidence timestamps are not causally ordered")
    if expires <= issued or expires - issued > timedelta(minutes=10):
        raise ValueError("authorization TTL must be positive and at most ten minutes")
    if expires > audit_expires or expires > candidate_expires:
        raise ValueError("authorization outlives its candidate or policy audit")
    assets = publishable_assets(manifest, args.sha256sums)
    return {
        "schema_version": 1,
        "predicate_type": PREDICATE_TYPE,
        "status": STATUS,
        "publication_authority": False,
        "repository": {"id": REPOSITORY_ID, "full_name": REPOSITORY},
        "source_git_sha": manifest["source_git_sha"],
        "release": {
            "intent_id": manifest["release_intent_id"],
            "ref": manifest["planned_release_ref"],
            "kind": manifest["release_kind"],
            "version": manifest["release_version"],
            "is_prerelease": manifest["is_prerelease"],
        },
        "candidate": {
            **candidate_reference,
            "manifest_created_at": manifest["created_at"],
            "manifest_expires_at": manifest["expires_at"],
        },
        "signed_tag": signed_tag,
        "policy_audit": {
            **policy_audit,
            "reference_sha256": file_hash(policy_path),
        },
        "release_policy_sha256": manifest["release_policy"]["sha256"],
        "authorization": authorization,
        "issued_at": args.issued_at,
        "expires_at": args.expires_at,
        "asset_count": len(assets),
        "assets": assets,
        "sha256sums_sha256": file_hash(validate_file(args.sha256sums, "SHA256SUMS")),
    }


def validate_predicate_shape(predicate: dict[str, Any]) -> None:
    exact_keys(
        predicate,
        {
            "schema_version",
            "predicate_type",
            "status",
            "publication_authority",
            "repository",
            "source_git_sha",
            "release",
            "candidate",
            "signed_tag",
            "policy_audit",
            "release_policy_sha256",
            "authorization",
            "issued_at",
            "expires_at",
            "asset_count",
            "assets",
            "sha256sums_sha256",
        },
        "promotion authorization predicate",
    )
    if (
        predicate["schema_version"] != 1
        or predicate["predicate_type"] != PREDICATE_TYPE
        or predicate["status"] != STATUS
        or predicate["publication_authority"] is not False
    ):
        raise ValueError("promotion authorization must remain disarmed")
    forbidden = {
        "attestation_id",
        "authorization_bundle_artifact_id",
        "authorization_bundle_artifact_digest",
        "envelope_sha256",
    }
    if forbidden & set(predicate):
        raise ValueError("authorization predicate contains post-signature identifiers")


def expected_envelope(args: argparse.Namespace) -> dict[str, Any]:
    predicate_path = validate_file(args.predicate, "authorization predicate")
    predicate = read_object(predicate_path, "authorization predicate")
    validate_predicate_shape(predicate)
    bundle_path = validate_file(
        args.attestation_bundle, "SHA256SUMS attestation bundle"
    )
    if bundle_path.name != "SHA256SUMS.sigstore.json":
        raise ValueError("authorization attestation bundle name changed")
    if not args.attestation_id or len(args.attestation_id) > 256:
        raise ValueError("authorization attestation ID is invalid")
    created = timestamp(args.created_at, "authorization envelope created_at")
    issued = timestamp(predicate["issued_at"], "authorization issued_at")
    expires = timestamp(predicate["expires_at"], "authorization expires_at")
    if not issued <= created <= expires:
        raise ValueError("authorization envelope was created outside its TTL")
    artifact = {
        "artifact_id": args.bundle_artifact_id,
        "name": args.bundle_artifact_name,
        "archive_digest": args.bundle_artifact_digest,
        "size_in_bytes": args.bundle_artifact_size,
    }
    validate_artifact_reference(artifact, "authorization bundle artifact")
    expected_name = f"rmux-promotion-authorization-{predicate['source_git_sha']}"
    if artifact["name"] != expected_name:
        raise ValueError("authorization bundle artifact name changed")
    return {
        "schema_version": 1,
        "envelope_type": ENVELOPE_TYPE,
        "status": STATUS,
        "publication_authority": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": predicate["source_git_sha"],
        "release_ref": predicate["release"]["ref"],
        "release_intent_id": predicate["release"]["intent_id"],
        "authorization": predicate["authorization"],
        "predicate_sha256": file_hash(predicate_path),
        "sha256sums_sha256": predicate["sha256sums_sha256"],
        "attestation": {
            "attestation_id": args.attestation_id,
            "bundle_file": "SHA256SUMS.sigstore.json",
            "bundle_sha256": file_hash(bundle_path),
        },
        "authorization_bundle": artifact,
        "public_metadata_assets": [
            {
                "name": "SHA256SUMS.sigstore.json",
                "role": "authorization-attestation",
                "size": bundle_path.stat().st_size,
                "sha256": file_hash(bundle_path),
            }
        ],
        "created_at": args.created_at,
    }


def validate_envelope_shape(envelope: dict[str, Any]) -> None:
    exact_keys(
        envelope,
        {
            "schema_version",
            "envelope_type",
            "status",
            "publication_authority",
            "repository_id",
            "source_git_sha",
            "release_ref",
            "release_intent_id",
            "authorization",
            "predicate_sha256",
            "sha256sums_sha256",
            "attestation",
            "authorization_bundle",
            "public_metadata_assets",
            "created_at",
        },
        "promotion authorization envelope",
    )
    if (
        envelope["schema_version"] != 1
        or envelope["envelope_type"] != ENVELOPE_TYPE
        or envelope["status"] != STATUS
        or envelope["publication_authority"] is not False
    ):
        raise ValueError("promotion authorization envelope must remain disarmed")
    if "envelope_sha256" in envelope or "envelope_artifact_id" in envelope:
        raise ValueError("authorization envelope cannot refer to its own upload")


def predicate_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--candidate-manifest", type=Path, required=True)
    parser.add_argument("--candidate-reference", type=Path, required=True)
    parser.add_argument("--signed-tag", type=Path, required=True)
    parser.add_argument("--policy-audit-reference", type=Path, required=True)
    parser.add_argument("--sha256sums", type=Path, required=True)
    parser.add_argument("--authorization-run-id", type=int, required=True)
    parser.add_argument("--authorization-run-attempt", type=int, default=1)
    parser.add_argument("--authorization-workflow-id", type=int, required=True)
    parser.add_argument("--issued-at", required=True)
    parser.add_argument("--expires-at", required=True)


def envelope_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--predicate", type=Path, required=True)
    parser.add_argument("--attestation-id", required=True)
    parser.add_argument("--attestation-bundle", type=Path, required=True)
    parser.add_argument("--bundle-artifact-id", type=int, required=True)
    parser.add_argument("--bundle-artifact-name", required=True)
    parser.add_argument("--bundle-artifact-digest", required=True)
    parser.add_argument("--bundle-artifact-size", type=int, required=True)
    parser.add_argument("--created-at", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("create-predicate", "verify-predicate"):
        child = subparsers.add_parser(command)
        predicate_arguments(child)
        child.add_argument(
            "--output" if command.startswith("create") else "--document",
            type=Path,
            required=True,
        )
    for command in ("create-envelope", "verify-envelope"):
        child = subparsers.add_parser(command)
        envelope_arguments(child)
        child.add_argument(
            "--output" if command.startswith("create") else "--document",
            type=Path,
            required=True,
        )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.command.endswith("predicate"):
        expected = expected_predicate(args)
        validate_predicate_shape(expected)
    else:
        expected = expected_envelope(args)
        validate_envelope_shape(expected)
    if args.command.startswith("create"):
        write_object(args.output, expected)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, args.command.removeprefix("verify-"))
        if actual != expected:
            raise ValueError(f"{args.command.removeprefix('verify-')} changed")
        print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
