#!/usr/bin/env python3
"""Create or verify disarmed publication receipt predicates and envelopes."""

from __future__ import annotations
import argparse
from datetime import timedelta
from pathlib import Path
from typing import Any

from release_evidence import (
    DIGEST,
    PROMOTION_WORKFLOW_ID,
    PROMOTION_WORKFLOW_PATH,
    REPOSITORY_ID,
    RELEASE_REF,
    SHA40,
    SHA256,
    SIMULATION_WORKFLOW_ID,
    SIMULATION_WORKFLOW_PATH,
    STATUS,
    exact_keys,
    file_hash,
    positive_integer,
    read_object,
    require_match,
    timestamp,
    validate_artifact_reference,
    validate_file,
    validate_policy_audit,
    validate_signed_tag,
    write_object,
)
from publication_release_state import validate_release_state

PREDICATE_TYPE = "https://rmux.io/attestations/release-publication-receipt/v1"
ENVELOPE_TYPE = "https://rmux.io/envelopes/release-publication-receipt/v1"
AUTHORIZATION_TYPE = "https://rmux.io/attestations/release-promotion-authorization/v1"
AUTHORIZATION_ENVELOPE_TYPE = AUTHORIZATION_TYPE.replace("attestations", "envelopes")
RECEIPT_WORKFLOW_ID = 316435347
RECEIPT_WORKFLOW_PATH = ".github/workflows/release-receipt.yml"


def validate_authorization_predicate(
    value: dict[str, Any], *, simulation: bool = False
) -> None:
    exact_keys(
        value,
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
        "authorization predicate",
    )
    if (
        value["schema_version"] != 1
        or value["predicate_type"] != AUTHORIZATION_TYPE
        or value["status"] != STATUS
        or value["publication_authority"] is not False
        or value["repository"] != {"id": REPOSITORY_ID, "full_name": "Helvesec/rmux"}
    ):
        raise ValueError("authorization predicate is not exact disarmed RMUX evidence")
    require_match(value["source_git_sha"], SHA40, "authorization source SHA")
    release = value["release"]
    if not isinstance(release, dict):
        raise ValueError("authorization release identity must be an object")
    exact_keys(
        release,
        {"intent_id", "ref", "kind", "version", "is_prerelease"},
        "authorization release identity",
    )
    require_match(release["ref"], RELEASE_REF, "authorization release ref")
    if (
        release["kind"] not in {"rc", "stable"}
        or release["version"] != release["ref"][1:]
        or release["is_prerelease"] is not ("-rc." in release["ref"])
        or (release["kind"] == "rc") is not release["is_prerelease"]
    ):
        raise ValueError("authorization release identity fields disagree")
    candidate = value["candidate"]
    if not isinstance(candidate, dict):
        raise ValueError("authorization candidate reference must be an object")
    exact_keys(
        candidate,
        {
            "schema_version",
            "status",
            "repository_id",
            "source_git_sha",
            "candidate_run_id",
            "candidate_run_attempt",
            "manifest_run_id",
            "manifest_run_attempt",
            "manifest_workflow_id",
            "manifest_workflow_path",
            "manifest_artifact_id",
            "manifest_artifact_digest",
            "manifest_sha256",
            "manifest_created_at",
            "manifest_expires_at",
        },
        "authorization candidate reference",
    )
    if (
        candidate["schema_version"] != 1
        or candidate["status"] != "shadow-non-authoritative"
        or candidate["repository_id"] != REPOSITORY_ID
        or candidate["source_git_sha"] != value["source_git_sha"]
        or candidate["candidate_run_attempt"] != 1
        or candidate["manifest_run_attempt"] != 1
        or candidate["manifest_workflow_id"] != 316223904
        or candidate["manifest_workflow_path"] != ".github/workflows/release-shadow.yml"
    ):
        raise ValueError("authorization candidate reference identity changed")
    for field in ("candidate_run_id", "manifest_run_id", "manifest_artifact_id"):
        positive_integer(candidate[field], f"authorization candidate {field}")
    require_match(
        candidate["manifest_artifact_digest"], DIGEST, "candidate artifact digest"
    )
    require_match(candidate["manifest_sha256"], SHA256, "candidate manifest digest")
    candidate_created = timestamp(
        candidate["manifest_created_at"], "candidate created_at"
    )
    candidate_expires = timestamp(
        candidate["manifest_expires_at"], "candidate expires_at"
    )
    validate_signed_tag(
        value["signed_tag"],
        release_ref=release["ref"],
        source_git_sha=value["source_git_sha"],
        release_intent_id=release["intent_id"],
        release_kind=release["kind"],
        candidate=candidate,
        release_policy_sha256=value["release_policy_sha256"],
    )
    policy = value["policy_audit"]
    if not isinstance(policy, dict) or "reference_sha256" not in policy:
        raise ValueError("authorization policy audit reference is incomplete")
    require_match(policy["reference_sha256"], SHA256, "policy reference digest")
    expected_workflow_id = (
        SIMULATION_WORKFLOW_ID if simulation else PROMOTION_WORKFLOW_ID
    )
    expected_workflow_path = (
        SIMULATION_WORKFLOW_PATH if simulation else PROMOTION_WORKFLOW_PATH
    )
    validate_policy_audit(
        {key: item for key, item in policy.items() if key != "reference_sha256"},
        {
            "source_git_sha": value["source_git_sha"],
            "candidate_run_id": candidate["candidate_run_id"],
            "release_intent_id": release["intent_id"],
            "release_policy": {"sha256": value["release_policy_sha256"]},
        },
        workflow_id=expected_workflow_id,
        workflow_path=expected_workflow_path,
    )
    identity = value["authorization"]
    if not isinstance(identity, dict):
        raise ValueError("authorization run identity must be an object")
    exact_keys(
        identity,
        {"run_id", "run_attempt", "workflow_id", "workflow_path"},
        "authorization run identity",
    )
    if (
        identity["run_attempt"] != 1
        or identity["workflow_id"] != expected_workflow_id
        or identity["workflow_path"] != expected_workflow_path
    ):
        raise ValueError("authorization run identity changed")
    positive_integer(identity["run_id"], "authorization run ID")
    positive_integer(identity["workflow_id"], "authorization workflow ID")
    if policy["policy_audit_run_id"] != identity["run_id"]:
        raise ValueError("policy audit and authorization must share one promoter run")
    issued = timestamp(value["issued_at"], "authorization issued_at")
    expires = timestamp(value["expires_at"], "authorization expires_at")
    if (
        not candidate_created <= issued < candidate_expires
        or expires <= issued
        or expires - issued > timedelta(minutes=10)
        or expires > timestamp(policy["expires_at"], "policy audit expires_at")
    ):
        raise ValueError("authorization evidence TTL changed")
    require_match(value["release_policy_sha256"], SHA256, "release policy root")
    require_match(value["sha256sums_sha256"], SHA256, "SHA256SUMS digest")
    assets = value["assets"]
    if (
        not isinstance(assets, list)
        or value["asset_count"] != len(assets)
        or not assets
    ):
        raise ValueError("authorization asset cardinality changed")
    names: list[str] = []
    for asset in assets:
        if not isinstance(asset, dict):
            raise ValueError("authorization asset must be an object")
        exact_keys(
            asset,
            {"name", "platform_key", "role", "size", "sha256"},
            "authorization asset",
        )
        names.append(asset["name"])
        positive_integer(asset["size"], f"authorization asset {asset['name']} size")
        require_match(asset["sha256"], SHA256, "authorization asset digest")
    if names != sorted(names) or len(names) != len(set(names)):
        raise ValueError("authorization assets must be sorted and unique")


def validate_authorization_envelope(
    value: dict[str, Any], predicate_path: Path, predicate: dict[str, Any]
) -> None:
    exact_keys(
        value,
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
        "authorization envelope",
    )
    if (
        value["schema_version"] != 1
        or value["envelope_type"] != AUTHORIZATION_ENVELOPE_TYPE
        or value["status"] != STATUS
        or value["publication_authority"] is not False
        or value["repository_id"] != REPOSITORY_ID
        or value["source_git_sha"] != predicate["source_git_sha"]
        or value["release_ref"] != predicate["release"]["ref"]
        or value["release_intent_id"] != predicate["release"]["intent_id"]
        or value["authorization"] != predicate["authorization"]
        or value["predicate_sha256"] != file_hash(predicate_path)
        or value["sha256sums_sha256"] != predicate["sha256sums_sha256"]
    ):
        raise ValueError("authorization envelope does not bind its exact predicate")
    validate_artifact_reference(value["authorization_bundle"], "authorization bundle")
    attestation = value["attestation"]
    if not isinstance(attestation, dict):
        raise ValueError("authorization attestation is missing")
    exact_keys(
        attestation,
        {"attestation_id", "bundle_file", "bundle_sha256"},
        "authorization attestation",
    )
    if attestation["bundle_file"] != "SHA256SUMS.sigstore.json":
        raise ValueError("authorization attestation bundle name changed")
    require_match(attestation["bundle_sha256"], SHA256, "authorization bundle digest")
    metadata = value["public_metadata_assets"]
    if not isinstance(metadata, list) or len(metadata) != 1:
        raise ValueError("authorization must declare exactly one public metadata asset")
    item = metadata[0]
    if not isinstance(item, dict):
        raise ValueError("authorization metadata asset must be an object")
    exact_keys(item, {"name", "role", "size", "sha256"}, "metadata asset")
    if (
        item["name"] != "SHA256SUMS.sigstore.json"
        or item["role"] != "authorization-attestation"
        or item["sha256"] != attestation["bundle_sha256"]
    ):
        raise ValueError("authorization public metadata differs from its attestation")
    positive_integer(item["size"], "authorization metadata size")


def receipt_identity(args: argparse.Namespace) -> dict[str, Any]:
    positive_integer(args.receipt_run_id, "receipt run ID")
    positive_integer(args.receipt_workflow_id, "receipt workflow ID")
    if args.receipt_run_attempt != 1:
        raise ValueError("publication receipt requires Actions attempt 1")
    expected_id = SIMULATION_WORKFLOW_ID if args.simulation else RECEIPT_WORKFLOW_ID
    expected_path = (
        SIMULATION_WORKFLOW_PATH if args.simulation else RECEIPT_WORKFLOW_PATH
    )
    if args.receipt_workflow_id != expected_id:
        raise ValueError("publication receipt workflow ID changed")
    return {
        "run_id": args.receipt_run_id,
        "run_attempt": 1,
        "workflow_id": args.receipt_workflow_id,
        "workflow_path": expected_path,
    }


def expected_predicate(args: argparse.Namespace) -> dict[str, Any]:
    auth_path = validate_file(args.authorization_predicate, "authorization predicate")
    authorization = read_object(auth_path, "authorization predicate")
    validate_authorization_predicate(authorization, simulation=args.simulation)
    envelope_path = validate_file(args.authorization_envelope, "authorization envelope")
    envelope = read_object(envelope_path, "authorization envelope")
    validate_authorization_envelope(envelope, auth_path, authorization)
    state = read_object(args.release_state, "GitHub Release state")
    assets = validate_release_state(state, authorization, envelope)
    verified = timestamp(args.verified_at, "publication receipt verified_at")
    if verified < timestamp(state["published_at"], "GitHub Release published_at"):
        raise ValueError("publication receipt predates publication")
    policy_audit = authorization["policy_audit"]
    return {
        "schema_version": 1,
        "predicate_type": PREDICATE_TYPE,
        "status": STATUS,
        "downstream_authority": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": authorization["source_git_sha"],
        "release": {
            "id": state["release_id"],
            "ref": state["release_ref"],
            "intent_id": authorization["release"]["intent_id"],
            "kind": authorization["release"]["kind"],
            "tag_object_sha": state["tag_object_sha"],
            "immutable": True,
            "created_at": state["created_at"],
            "published_at": state["published_at"],
        },
        "candidate": authorization["candidate"],
        "policy_audit": policy_audit,
        "authorization": {
            "identity": authorization["authorization"],
            "predicate_sha256": file_hash(auth_path),
            "envelope_sha256": file_hash(envelope_path),
            "attestation": envelope["attestation"],
            "bundle_artifact": envelope["authorization_bundle"],
        },
        "receipt": receipt_identity(args),
        "verified_at": args.verified_at,
        "asset_count": len(assets),
        "assets": assets,
        "sha256sums_sha256": authorization["sha256sums_sha256"],
    }


def validate_predicate_shape(value: dict[str, Any]) -> None:
    exact_keys(
        value,
        {
            "schema_version",
            "predicate_type",
            "status",
            "downstream_authority",
            "repository_id",
            "source_git_sha",
            "release",
            "candidate",
            "policy_audit",
            "authorization",
            "receipt",
            "verified_at",
            "asset_count",
            "assets",
            "sha256sums_sha256",
        },
        "publication receipt predicate",
    )
    if (
        value["schema_version"] != 1
        or value["predicate_type"] != PREDICATE_TYPE
        or value["status"] != STATUS
        or value["downstream_authority"] is not False
    ):
        raise ValueError("publication receipt predicate must remain disarmed")
    if "attestation_id" in value or "receipt_bundle_artifact_id" in value:
        raise ValueError("receipt predicate contains post-signature identifiers")


def expected_envelope(args: argparse.Namespace) -> dict[str, Any]:
    predicate_path = validate_file(args.predicate, "publication receipt predicate")
    predicate = read_object(predicate_path, "publication receipt predicate")
    validate_predicate_shape(predicate)
    bundle_path = validate_file(args.attestation_bundle, "receipt attestation bundle")
    if bundle_path.name != "publication-receipt.sigstore.json":
        raise ValueError("receipt attestation bundle name changed")
    if not args.attestation_id or len(args.attestation_id) > 256:
        raise ValueError("receipt attestation ID is invalid")
    created = timestamp(args.created_at, "receipt envelope created_at")
    if created < timestamp(predicate["verified_at"], "receipt verified_at"):
        raise ValueError("receipt envelope predates its predicate")
    artifact = {
        "artifact_id": args.bundle_artifact_id,
        "name": args.bundle_artifact_name,
        "archive_digest": args.bundle_artifact_digest,
        "size_in_bytes": args.bundle_artifact_size,
    }
    validate_artifact_reference(artifact, "receipt bundle artifact")
    expected_name = (
        f"rmux-publication-receipt-{predicate['source_git_sha']}-"
        f"{predicate['release']['id']}"
    )
    if artifact["name"] != expected_name:
        raise ValueError("receipt bundle artifact name changed")
    return {
        "schema_version": 1,
        "envelope_type": ENVELOPE_TYPE,
        "status": STATUS,
        "downstream_authority": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": predicate["source_git_sha"],
        "release_id": predicate["release"]["id"],
        "release_ref": predicate["release"]["ref"],
        "release_intent_id": predicate["release"]["intent_id"],
        "receipt": predicate["receipt"],
        "predicate_sha256": file_hash(predicate_path),
        "attestation": {
            "attestation_id": args.attestation_id,
            "bundle_file": "publication-receipt.sigstore.json",
            "bundle_sha256": file_hash(bundle_path),
        },
        "receipt_bundle": artifact,
        "created_at": args.created_at,
    }


def validate_envelope_shape(value: dict[str, Any]) -> None:
    exact_keys(
        value,
        {
            "schema_version",
            "envelope_type",
            "status",
            "downstream_authority",
            "repository_id",
            "source_git_sha",
            "release_id",
            "release_ref",
            "release_intent_id",
            "receipt",
            "predicate_sha256",
            "attestation",
            "receipt_bundle",
            "created_at",
        },
        "publication receipt envelope",
    )
    if (
        value["schema_version"] != 1
        or value["envelope_type"] != ENVELOPE_TYPE
        or value["status"] != STATUS
        or value["downstream_authority"] is not False
    ):
        raise ValueError("publication receipt envelope must remain disarmed")
    if "envelope_sha256" in value or "envelope_artifact_id" in value:
        raise ValueError("publication receipt envelope cannot refer to its own upload")


def predicate_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--simulation", action="store_true")
    parser.add_argument("--authorization-predicate", type=Path, required=True)
    parser.add_argument("--authorization-envelope", type=Path, required=True)
    parser.add_argument("--release-state", type=Path, required=True)
    parser.add_argument("--receipt-run-id", type=int, required=True)
    parser.add_argument("--receipt-run-attempt", type=int, default=1)
    parser.add_argument("--receipt-workflow-id", type=int, required=True)
    parser.add_argument("--verified-at", required=True)


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
