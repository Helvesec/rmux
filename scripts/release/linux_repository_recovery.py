#!/usr/bin/env python3
"""Fail-closed evidence checks for manual Linux repository signing recovery."""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    read_object,
    validate_receipt_reference,
    write_object,
)
from downstream_plan import validate_plan
from linux_repository_recovery_model import (
    RECEIPT_WORKFLOW_ID,
    RECEIPT_WORKFLOW_PATH,
    REPOSITORY,
    REPOSITORY_ID,
    SIGNER_WORKFLOW_PATH,
    canonical_digest,
    canonical_release_ref,
    canonical_sha,
    load_artifact,
    load_result_artifacts,
    load_run,
    load_run_artifacts,
    matching_artifact,
    positive,
    reject_prior_linux_publication,
    reject_prior_recovery_result,
    validate_artifact,
    validate_failed_receipt_run,
    validate_signer_run,
)


def github_output(path: Path, values: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8") as output:
        for name, value in values.items():
            output.write(f"{name}={value}\n")


def inspect_original(args: argparse.Namespace) -> None:
    source_sha = canonical_sha(args.source_sha, "release source SHA")
    release_ref = canonical_release_ref(args.release_ref)
    run = load_run(args.receipt_run_id, args.run_json)
    validate_failed_receipt_run(
        run,
        run_id=args.receipt_run_id,
        source_sha=source_sha,
        release_ref=release_ref,
    )
    artifacts = load_run_artifacts(args.receipt_run_id, args.artifacts_json)
    reject_prior_linux_publication(
        artifacts, source_sha=source_sha, release_id=args.release_id
    )
    result_artifacts = load_result_artifacts(
        source_sha, args.release_id, args.result_artifacts_json
    )
    reject_prior_recovery_result(
        result_artifacts, source_sha=source_sha, release_id=args.release_id
    )
    plan_artifact = matching_artifact(artifacts, args.plan_artifact_id)
    validate_artifact(
        plan_artifact,
        artifact_id=args.plan_artifact_id,
        name=f"rmux-downstream-plan-{source_sha}-{args.release_id}",
        digest=canonical_digest(args.plan_artifact_digest, "plan artifact digest"),
        run_id=args.receipt_run_id,
        source_sha=source_sha,
    )
    payload_artifact = matching_artifact(artifacts, args.payload_artifact_id)
    validate_artifact(
        payload_artifact,
        artifact_id=args.payload_artifact_id,
        name=f"rmux-downstream-apt_rpm-payload-{source_sha}-{args.release_id}",
        digest=canonical_digest(args.payload_artifact_digest, "payload artifact digest"),
        run_id=args.receipt_run_id,
        source_sha=source_sha,
    )

    plan = validate_plan(read_object(args.plan, "recovery downstream plan"))
    reference = validate_receipt_reference(
        read_object(args.receipt_reference, "recovery receipt reference")
    )
    apt = next(
        (item for item in plan["channels"] if item.get("name") == "apt_rpm"), None
    )
    if (
        plan["source_git_sha"] != source_sha
        or plan["release"]["id"] != args.release_id
        or plan["release"]["ref"] != release_ref
        or plan["release"]["kind"] != "stable"
        or plan["receipt"]["run_id"] != args.receipt_run_id
        or plan["receipt"]["workflow_id"] != RECEIPT_WORKFLOW_ID
        or reference["source_git_sha"] != source_sha
        or reference["release"]["id"] != args.release_id
        or reference["release"]["ref"] != release_ref
        or reference["receipt"]["run_id"] != args.receipt_run_id
        or reference["receipt"]["workflow_id"] != RECEIPT_WORKFLOW_ID
        or plan["receipt"]
        != {
            **reference["receipt"],
            "predicate_bundle": reference["predicate_bundle"],
            "predicate_sha256": reference["predicate_sha256"],
            "envelope_bundle": reference["envelope_bundle"],
            "envelope_sha256": reference["envelope_sha256"],
            "attestation": reference["attestation"],
            "verified_at": reference["verified_at"],
        }
        or not isinstance(apt, dict)
        or apt.get("policy_decision") != "allow"
        or apt.get("execution_decision") != "enabled"
        or apt.get("execution_enabled") is not True
        or apt.get("payload_ready") is not True
    ):
        raise ValueError("failed receipt did not authorize this Linux repository channel")
    github_output(
        args.github_output,
        {
            "receipt_artifact_id": reference["predicate_bundle"]["artifact_id"],
            "receipt_artifact_digest": reference["predicate_bundle"][
                "archive_digest"
            ],
            "receipt_envelope_artifact_id": reference["envelope_bundle"][
                "artifact_id"
            ],
            "receipt_envelope_artifact_digest": reference["envelope_bundle"][
                "archive_digest"
            ],
            "receipt_predicate_sha256": reference["predicate_sha256"],
            "receipt_envelope_sha256": reference["envelope_sha256"],
        },
    )


def create_manifest(args: argparse.Namespace) -> None:
    protected_main_sha = canonical_sha(args.protected_main_sha, "protected main SHA")
    source_sha = canonical_sha(args.source_sha, "release source SHA")
    release_ref = canonical_release_ref(args.release_ref)
    repository_base = canonical_sha(
        args.repository_base.read_text(encoding="utf-8").strip(),
        "package repository base",
    )
    value = {
        "schema_version": 1,
        "status": "signed-linux-repository-recovery",
        "repository_id": REPOSITORY_ID,
        "source_git_sha": source_sha,
        "release": {"id": args.release_id, "ref": release_ref},
        "receipt": {
            "run_id": args.receipt_run_id,
            "workflow_id": RECEIPT_WORKFLOW_ID,
            "workflow_path": RECEIPT_WORKFLOW_PATH,
        },
        "plan_artifact": {
            "artifact_id": args.plan_artifact_id,
            "archive_digest": canonical_digest(
                args.plan_artifact_digest, "plan artifact digest"
            ),
        },
        "payload_artifact": {
            "artifact_id": args.payload_artifact_id,
            "archive_digest": canonical_digest(
                args.payload_artifact_digest, "payload artifact digest"
            ),
        },
        "producer": {
            "run_id": args.run_id,
            "run_attempt": 1,
            "workflow_path": SIGNER_WORKFLOW_PATH,
            "event": "workflow_dispatch",
            "head_branch": "main",
            "head_sha": protected_main_sha,
        },
        "package_repository_base": repository_base,
    }
    positive(args.run_id, "signer run ID")
    positive(args.receipt_run_id, "receipt run ID")
    positive(args.release_id, "release ID")
    positive(args.plan_artifact_id, "plan artifact ID")
    positive(args.payload_artifact_id, "payload artifact ID")
    if args.output.exists():
        raise ValueError("recovery manifest output already exists")
    write_object(args.output, value)


def current_context(args: argparse.Namespace) -> None:
    expected = {
        "GITHUB_REPOSITORY": REPOSITORY,
        "GITHUB_REPOSITORY_ID": str(REPOSITORY_ID),
        "GITHUB_RUN_ID": str(args.run_id),
        "GITHUB_RUN_ATTEMPT": "1",
        "GITHUB_SHA": args.protected_main_sha,
        "GITHUB_REF": "refs/heads/main",
        "GITHUB_REF_NAME": "main",
        "GITHUB_EVENT_NAME": "workflow_dispatch",
    }
    for name, value in expected.items():
        if os.environ.get(name) != value:
            raise ValueError(f"current signer context {name} mismatch")


def verify_signer_run(args: argparse.Namespace) -> None:
    protected_main_sha = canonical_sha(args.protected_main_sha, "protected main SHA")
    current_context(args)
    workflow_id = validate_signer_run(
        load_run(args.run_id, args.run_json),
        run_id=args.run_id,
        protected_main_sha=protected_main_sha,
    )
    github_output(args.github_output, {"workflow_id": workflow_id})


def verify_signer_artifact(args: argparse.Namespace) -> None:
    source_sha = canonical_sha(args.source_sha, "release source SHA")
    protected_main_sha = canonical_sha(args.protected_main_sha, "protected main SHA")
    release_ref = canonical_release_ref(args.release_ref)
    current_context(args)
    run = load_run(args.run_id, args.run_json)
    workflow_id = validate_signer_run(
        run, run_id=args.run_id, protected_main_sha=protected_main_sha
    )
    artifact = load_artifact(args.artifact_id, args.artifact_json)
    validate_artifact(
        artifact,
        artifact_id=args.artifact_id,
        name=f"rmux-downstream-apt_rpm-signed-{source_sha}-{args.release_id}",
        digest=canonical_digest(args.artifact_digest, "signed artifact digest"),
        run_id=args.run_id,
        source_sha=protected_main_sha,
    )
    proof = {
        "schema_version": 1,
        "status": "verified-linux-repository-recovery-artifact",
        "repository_id": REPOSITORY_ID,
        "source_git_sha": source_sha,
        "release": {"id": args.release_id, "ref": release_ref},
        "receipt_run_id": args.receipt_run_id,
        "plan_artifact": {
            "artifact_id": args.plan_artifact_id,
            "archive_digest": canonical_digest(
                args.plan_artifact_digest, "plan artifact digest"
            ),
        },
        "payload_artifact": {
            "artifact_id": args.payload_artifact_id,
            "archive_digest": canonical_digest(
                args.payload_artifact_digest, "payload artifact digest"
            ),
        },
        "producer": {
            "run_id": args.run_id,
            "run_attempt": 1,
            "workflow_id": workflow_id,
            "workflow_path": SIGNER_WORKFLOW_PATH,
            "event": "workflow_dispatch",
            "head_branch": "main",
            "head_sha": protected_main_sha,
        },
        "artifact": {
            "artifact_id": args.artifact_id,
            "name": artifact["name"],
            "archive_digest": artifact["digest"],
            "size_in_bytes": artifact["size_in_bytes"],
        },
    }
    write_object(args.proof, proof)
    github_output(args.github_output, {"workflow_id": workflow_id})


def verify_bundle(args: argparse.Namespace) -> None:
    proof = read_object(args.proof, "recovery artifact proof")
    manifest_path = args.repository_dir / "LINUX_REPOSITORY_RECOVERY.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("recovery manifest must be one regular file")
    manifest = read_object(manifest_path, "Linux repository recovery manifest")
    repository_base = canonical_sha(
        (args.repository_dir / "PACKAGE_REPOSITORY_BASE")
        .read_text(encoding="utf-8")
        .strip(),
        "package repository base",
    )
    expected_manifest = {
        "schema_version": 1,
        "status": "signed-linux-repository-recovery",
        "repository_id": REPOSITORY_ID,
        "source_git_sha": proof["source_git_sha"],
        "release": proof["release"],
        "receipt": {
            "run_id": proof["receipt_run_id"],
            "workflow_id": RECEIPT_WORKFLOW_ID,
            "workflow_path": RECEIPT_WORKFLOW_PATH,
        },
        "plan_artifact": proof["plan_artifact"],
        "payload_artifact": proof["payload_artifact"],
        "producer": {
            key: proof["producer"][key]
            for key in (
                "run_id",
                "run_attempt",
                "workflow_path",
                "event",
                "head_branch",
                "head_sha",
            )
        },
        "package_repository_base": repository_base,
    }
    if manifest != expected_manifest:
        raise ValueError("signed repository manifest differs from verified recovery proof")
    if proof.get("artifact", {}).get("artifact_id") != args.artifact_id:
        raise ValueError("downloaded recovery artifact ID differs from needs.build")


def add_identity_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-id", type=int, required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--receipt-run-id", type=int, required=True)
    parser.add_argument("--plan-artifact-id", type=int, required=True)
    parser.add_argument("--plan-artifact-digest", required=True)
    parser.add_argument("--payload-artifact-id", type=int, required=True)
    parser.add_argument("--payload-artifact-digest", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)

    inspect = commands.add_parser("inspect-original")
    add_identity_arguments(inspect)
    inspect.add_argument("--plan", type=Path, required=True)
    inspect.add_argument("--receipt-reference", type=Path, required=True)
    inspect.add_argument("--run-json", type=Path)
    inspect.add_argument("--artifacts-json", type=Path)
    inspect.add_argument("--result-artifacts-json", type=Path)
    inspect.add_argument("--github-output", type=Path, required=True)

    create = commands.add_parser("create-manifest")
    add_identity_arguments(create)
    create.add_argument("--run-id", type=int, required=True)
    create.add_argument("--protected-main-sha", required=True)
    create.add_argument("--repository-base", type=Path, required=True)
    create.add_argument("--output", type=Path, required=True)

    signer_run = commands.add_parser("verify-signer-run")
    signer_run.add_argument("--run-id", type=int, required=True)
    signer_run.add_argument("--protected-main-sha", required=True)
    signer_run.add_argument("--run-json", type=Path)
    signer_run.add_argument("--github-output", type=Path, required=True)

    signer = commands.add_parser("verify-signer-artifact")
    add_identity_arguments(signer)
    signer.add_argument("--run-id", type=int, required=True)
    signer.add_argument("--protected-main-sha", required=True)
    signer.add_argument("--artifact-id", type=int, required=True)
    signer.add_argument("--artifact-digest", required=True)
    signer.add_argument("--run-json", type=Path)
    signer.add_argument("--artifact-json", type=Path)
    signer.add_argument("--proof", type=Path, required=True)
    signer.add_argument("--github-output", type=Path, required=True)

    bundle = commands.add_parser("verify-bundle")
    bundle.add_argument("--repository-dir", type=Path, required=True)
    bundle.add_argument("--proof", type=Path, required=True)
    bundle.add_argument("--artifact-id", type=int, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.command == "inspect-original":
        inspect_original(args)
    elif args.command == "create-manifest":
        create_manifest(args)
    elif args.command == "verify-signer-run":
        verify_signer_run(args)
    elif args.command == "verify-signer-artifact":
        verify_signer_artifact(args)
    else:
        verify_bundle(args)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, OSError, ValueError) as error:
        print(f"linux-repository-recovery: {error}", file=sys.stderr)
        raise SystemExit(1) from error
