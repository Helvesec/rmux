#!/usr/bin/env python3
"""Exercise the disarmed promotion protocol against exact candidate bytes."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import threading
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from release_evidence import (
    SIMULATION_WORKFLOW_ID,
    SIMULATION_WORKFLOW_PATH,
    file_hash,
    render_timestamp,
    timestamp,
    validate_candidate_manifest,
    validate_policy_audit,
)

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts" / "release"
TAG_PRINCIPAL = "rmux-release-simulation@rmux.io"
TAG_FINGERPRINT = f"SHA256:{'A' * 43}"


def read_object(path: Path, label: str) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"{label} must be one regular file")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be one JSON object")
    return value


def write_object(path: Path, value: dict[str, Any]) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def digest_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def run_script(
    script: str, arguments: list[str], *, rejected_by: str | None = None
) -> str:
    result = subprocess.run(
        [sys.executable, str(SCRIPTS / script), *arguments],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if rejected_by is None:
        if result.returncode != 0:
            raise ValueError(
                f"{script} failed: {result.stderr.strip() or result.stdout.strip()}"
            )
        return result.stdout.strip()
    if result.returncode == 0 or rejected_by not in result.stderr:
        raise ValueError(
            f"{script} did not fail closed with {rejected_by!r}: "
            f"{result.stderr.strip() or result.stdout.strip()}"
        )
    return result.stderr.strip()


def copy_assets(source: Path, destination: Path) -> None:
    if source.is_symlink() or not source.is_dir():
        raise ValueError("staged assets must be one real directory")
    destination.mkdir()
    entries = sorted(source.iterdir(), key=lambda item: item.name)
    if not entries:
        raise ValueError("staged assets are empty")
    for entry in entries:
        if entry.is_symlink() or not entry.is_file():
            raise ValueError(f"staged asset is not one regular file: {entry.name}")
        shutil.copy2(entry, destination / entry.name)


def synthetic_tag(
    manifest: dict[str, Any], candidate: dict[str, Any], verified_at: str
) -> dict[str, Any]:
    seed = (
        f"rmux-release-simulation:{manifest['source_git_sha']}:"
        f"{manifest['planned_release_ref']}"
    ).encode()
    return {
        "schema_version": 1,
        "status": "verified-signed-annotated-tag",
        "repository_id": 1239918790,
        "release_ref": manifest["planned_release_ref"],
        "release_intent_id": manifest["release_intent_id"],
        "release_kind": manifest["release_kind"],
        "tag_object_sha": hashlib.sha1(seed, usedforsecurity=False).hexdigest(),
        "target_git_sha": manifest["source_git_sha"],
        "candidate_run_id": manifest["candidate_run_id"],
        "candidate_manifest_artifact_id": candidate["manifest_artifact_id"],
        "candidate_manifest_artifact_digest": candidate["manifest_artifact_digest"],
        "candidate_manifest_sha256": candidate["manifest_sha256"],
        "release_policy_root_sha256": manifest["release_policy"]["sha256"],
        "object_type": "tag",
        "annotated": True,
        "signature": {
            "verified": True,
            "format": "ssh",
            "key_fingerprint": TAG_FINGERPRINT,
            "signing_principal": TAG_PRINCIPAL,
        },
        "verified_at": verified_at,
    }


def authorization_arguments(
    args: argparse.Namespace,
    output: Path,
    candidate: Path,
    signed_tag: Path,
    audit: Path,
    issued_at: str,
    expires_at: str,
) -> list[str]:
    return [
        "create-predicate",
        "--simulation",
        "--candidate-manifest",
        str(args.candidate_manifest),
        "--candidate-reference",
        str(candidate),
        "--signed-tag",
        str(signed_tag),
        "--policy-audit-reference",
        str(audit),
        "--sha256sums",
        str(args.staged_assets / "SHA256SUMS"),
        "--authorization-run-id",
        str(args.simulation_run_id),
        "--authorization-workflow-id",
        str(SIMULATION_WORKFLOW_ID),
        "--issued-at",
        issued_at,
        "--expires-at",
        expires_at,
        "--output",
        str(output),
    ]


class ReadOnlyApi(BaseHTTPRequestHandler):
    requests: list[str] = []

    def do_GET(self) -> None:  # noqa: N802
        self.requests.append(f"GET {self.path}")
        body = b'{"message":"Not Found"}'
        self.send_response(404)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _reject_mutation(self) -> None:
        self.requests.append(f"{self.command} {self.path}")
        self.send_error(405)

    do_DELETE = _reject_mutation  # type: ignore[assignment]
    do_PATCH = _reject_mutation  # type: ignore[assignment]
    do_POST = _reject_mutation  # type: ignore[assignment]
    do_PUT = _reject_mutation  # type: ignore[assignment]

    def log_message(self, _format: str, *_arguments: object) -> None:
        return


def publisher_plan(
    predicate: Path, envelope: Path, assets: Path, notes: Path
) -> tuple[dict[str, Any], list[str]]:
    ReadOnlyApi.requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), ReadOnlyApi)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        result = run_script(
            "publish-github-release.py",
            [
                "--simulation",
                "--predicate",
                str(predicate),
                "--envelope",
                str(envelope),
                "--assets-dir",
                str(assets),
                "--notes-file",
                str(notes),
                "--title",
                "RMUX release simulation",
                "--api-root",
                f"http://127.0.0.1:{server.server_port}",
                "--test-only-loopback-api",
            ],
        )
    finally:
        server.shutdown()
        worker.join()
        server.server_close()
    plan = json.loads(result)
    requests = list(ReadOnlyApi.requests)
    if (
        plan.get("mode") != "plan"
        or plan.get("mutations") is not False
        or not requests
        or any(not request.startswith("GET ") for request in requests)
    ):
        raise ValueError("publisher simulation was not strictly read-only")
    return plan, requests


def release_state(
    predicate: dict[str, Any], envelope: dict[str, Any], published_at: str
) -> dict[str, Any]:
    records = [*predicate["assets"], *envelope["public_metadata_assets"]]
    assets = [
        {
            "id": 10_000 + index,
            "name": record["name"],
            "size": record["size"],
            "digest": f"sha256:{record['sha256']}",
        }
        for index, record in enumerate(sorted(records, key=lambda item: item["name"]))
    ]
    return {
        "schema_version": 1,
        "status": "verified-immutable-release",
        "repository_id": 1239918790,
        "release_id": 9001,
        "release_ref": predicate["release"]["ref"],
        "source_git_sha": predicate["source_git_sha"],
        "tag_object_sha": predicate["signed_tag"]["tag_object_sha"],
        "draft": False,
        "prerelease": predicate["release"]["is_prerelease"],
        "immutable": True,
        "created_at": published_at,
        "published_at": published_at,
        "assets": assets,
    }


def receipt_arguments(
    predicate: Path,
    envelope: Path,
    state: Path,
    output: Path,
    run_id: int,
    verified_at: str,
) -> list[str]:
    return [
        "create-predicate",
        "--simulation",
        "--authorization-predicate",
        str(predicate),
        "--authorization-envelope",
        str(envelope),
        "--release-state",
        str(state),
        "--receipt-run-id",
        str(run_id),
        "--receipt-workflow-id",
        str(SIMULATION_WORKFLOW_ID),
        "--verified-at",
        verified_at,
        "--output",
        str(output),
    ]


def stable_receipt_fields(value: dict[str, Any]) -> dict[str, Any]:
    return {key: item for key, item in value.items() if key != "receipt"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--candidate-manifest", type=Path, required=True)
    parser.add_argument("--candidate-resolution", type=Path, required=True)
    parser.add_argument("--verified-candidate-artifacts", type=Path, required=True)
    parser.add_argument("--manifest-run-id", type=int, required=True)
    parser.add_argument("--manifest-artifact-id", type=int, required=True)
    parser.add_argument("--manifest-artifact-digest", required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--policy-audit-reference", type=Path, required=True)
    parser.add_argument("--staged-assets", type=Path, required=True)
    parser.add_argument("--simulation-id", required=True)
    parser.add_argument("--simulation-run-id", type=int, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.output_dir.exists() or args.output_dir.is_symlink():
        raise ValueError("simulation output directory already exists")
    args.output_dir.mkdir(parents=True)
    manifest = read_object(args.candidate_manifest, "candidate manifest")
    validate_candidate_manifest(manifest)
    if file_hash(args.candidate_manifest) != args.manifest_sha256:
        raise ValueError("candidate manifest digest differs")
    resolution = read_object(args.candidate_resolution, "candidate resolution")
    verified = read_object(
        args.verified_candidate_artifacts, "verified candidate artifacts"
    )
    if len(resolution.get("artifacts", [])) != 11:
        raise ValueError("simulation requires exactly eleven candidate artifacts")
    verified_identity = {
        "schema_version": 1,
        "status": "verified-for-shadow-sealing",
        "repository_id": 1239918790,
        "source_git_sha": manifest["source_git_sha"],
        "candidate_run_id": manifest["candidate_run_id"],
        "candidate_run_attempt": 1,
        "fast_run_id": manifest["fast_run_id"],
        "release_intent_id": manifest["release_intent_id"],
        "planned_release_ref": manifest["planned_release_ref"],
        "release_kind": manifest["release_kind"],
        "resolution_sha256": file_hash(args.candidate_resolution),
    }
    if any(verified.get(key) != value for key, value in verified_identity.items()):
        raise ValueError("verified candidate artifact identity differs")
    if len(verified.get("source_artifacts", [])) != 11:
        raise ValueError("verified candidate artifact set is not exhaustive")
    audit = read_object(args.policy_audit_reference, "policy audit reference")
    validate_policy_audit(
        audit,
        manifest,
        workflow_id=SIMULATION_WORKFLOW_ID,
        workflow_path=SIMULATION_WORKFLOW_PATH,
    )
    if audit["policy_audit_run_id"] != args.simulation_run_id:
        raise ValueError("policy audit does not belong to this simulation run")

    candidate_path = args.output_dir / "candidate-reference.json"
    run_script(
        "candidate-reference.py",
        [
            "create",
            "--manifest",
            str(args.candidate_manifest),
            "--manifest-run-id",
            str(args.manifest_run_id),
            "--manifest-workflow-id",
            "316223904",
            "--manifest-artifact-id",
            str(args.manifest_artifact_id),
            "--manifest-artifact-digest",
            args.manifest_artifact_digest,
            "--output",
            str(candidate_path),
        ],
    )
    candidate = read_object(candidate_path, "candidate reference")
    now = datetime.now(timezone.utc)
    audit_emitted = timestamp(audit["emitted_at"], "audit emitted_at")
    audit_expires = timestamp(audit["expires_at"], "audit expires_at")
    candidate_expires = timestamp(manifest["expires_at"], "candidate expires_at")
    issued = max(now, audit_emitted)
    expires = min(issued + timedelta(minutes=5), audit_expires, candidate_expires)
    if issued >= expires:
        raise ValueError("simulation evidence expired before authorization")
    issued_at = render_timestamp(issued)
    expires_at = render_timestamp(expires)

    tag_path = args.output_dir / "synthetic-signed-tag.json"
    write_object(tag_path, synthetic_tag(manifest, candidate, issued_at))
    predicate_path = args.output_dir / "promotion-authorization-predicate.json"
    auth_args = authorization_arguments(
        args,
        predicate_path,
        candidate_path,
        tag_path,
        args.policy_audit_reference,
        issued_at,
        expires_at,
    )
    run_script("promotion-authorization.py", auth_args)

    expired_audit = dict(audit)
    expired_audit["expires_at"] = expired_audit["emitted_at"]
    expired_path = args.output_dir / "expired-policy-audit-reference.json"
    write_object(expired_path, expired_audit)
    expired_args = authorization_arguments(
        args,
        args.output_dir / "expired-authorization.json",
        candidate_path,
        tag_path,
        expired_path,
        issued_at,
        expires_at,
    )
    run_script(
        "promotion-authorization.py",
        expired_args,
        rejected_by="policy audit TTL must be positive",
    )

    assets = args.output_dir / "assets"
    copy_assets(args.staged_assets, assets)
    authorization_bundle = assets / "SHA256SUMS.sigstore.json"
    bundle_value = {
        "simulation": True,
        "source_git_sha": manifest["source_git_sha"],
        "simulation_run_id": args.simulation_run_id,
    }
    write_object(authorization_bundle, bundle_value)
    envelope_path = args.output_dir / "promotion-authorization-envelope.json"
    run_script(
        "promotion-authorization.py",
        [
            "create-envelope",
            "--predicate",
            str(predicate_path),
            "--attestation-id",
            f"simulation-authorization-{args.simulation_run_id}",
            "--attestation-bundle",
            str(authorization_bundle),
            "--bundle-artifact-id",
            str(args.simulation_run_id + 1),
            "--bundle-artifact-name",
            f"rmux-promotion-authorization-{manifest['source_git_sha']}",
            "--bundle-artifact-digest",
            f"sha256:{file_hash(authorization_bundle)}",
            "--bundle-artifact-size",
            str(authorization_bundle.stat().st_size),
            "--created-at",
            issued_at,
            "--output",
            str(envelope_path),
        ],
    )
    notes = args.output_dir / "release-notes.md"
    notes.write_text("Nonpublishing RMUX release simulation.\n", encoding="utf-8")
    plan, api_requests = publisher_plan(predicate_path, envelope_path, assets, notes)

    original_bundle = authorization_bundle.read_bytes()
    authorization_bundle.write_bytes(original_bundle + b"corrupt")
    try:
        run_script(
            "publish-github-release.py",
            [
                "--simulation",
                "--predicate",
                str(predicate_path),
                "--envelope",
                str(envelope_path),
                "--assets-dir",
                str(assets),
                "--notes-file",
                str(notes),
                "--title",
                "RMUX release simulation",
                "--api-root",
                "http://127.0.0.1:1",
                "--test-only-loopback-api",
            ],
            rejected_by="authorized asset bytes changed",
        )
    finally:
        authorization_bundle.write_bytes(original_bundle)

    predicate = read_object(predicate_path, "authorization predicate")
    envelope = read_object(envelope_path, "authorization envelope")
    state_path = args.output_dir / "synthetic-release-state.json"
    write_object(state_path, release_state(predicate, envelope, issued_at))
    broken_state = read_object(state_path, "synthetic release state")
    broken_state["assets"] = broken_state["assets"][:-1]
    broken_state_path = args.output_dir / "broken-release-state.json"
    write_object(broken_state_path, broken_state)
    run_script(
        "publication-receipt.py",
        receipt_arguments(
            predicate_path,
            envelope_path,
            broken_state_path,
            args.output_dir / "failed-receipt.json",
            args.simulation_run_id,
            issued_at,
        ),
        rejected_by="asset cardinality changed",
    )

    receipts: list[dict[str, Any]] = []
    for offset in (0, 1):
        receipt_path = args.output_dir / f"publication-receipt-{offset + 1}.json"
        receipt_args = receipt_arguments(
            predicate_path,
            envelope_path,
            state_path,
            receipt_path,
            args.simulation_run_id + offset,
            issued_at,
        )
        run_script("publication-receipt.py", receipt_args)
        verify_args = receipt_args.copy()
        verify_args[0] = "verify-predicate"
        verify_args[-2] = "--document"
        run_script("publication-receipt.py", verify_args)
        receipts.append(read_object(receipt_path, "publication receipt"))
    if stable_receipt_fields(receipts[0]) != stable_receipt_fields(receipts[1]):
        raise ValueError("receipt-only recovery changed stable publication evidence")
    if receipts[0]["receipt"]["run_id"] == receipts[1]["receipt"]["run_id"]:
        raise ValueError("receipt-only recovery did not use a fresh run identity")

    record = {
        "schema_version": 2,
        "status": "nonpublishing-e2e-simulation",
        "simulation_id": args.simulation_id,
        "simulation_run_id": args.simulation_run_id,
        "source_git_sha": manifest["source_git_sha"],
        "candidate_run_id": manifest["candidate_run_id"],
        "manifest_run_id": args.manifest_run_id,
        "candidate_artifact_count": len(resolution["artifacts"]),
        "candidate_resolution_sha256": file_hash(args.candidate_resolution),
        "verified_candidate_artifacts_sha256": file_hash(
            args.verified_candidate_artifacts
        ),
        "policy_audit_reference_sha256": file_hash(args.policy_audit_reference),
        "authorization_predicate_sha256": file_hash(predicate_path),
        "authorization_envelope_sha256": file_hash(envelope_path),
        "publisher_plan": plan,
        "publisher_api_requests": api_requests,
        "publication_authority": False,
        "downstream_authority": False,
        "repository_mutations": False,
        "exact_candidate_bytes_exercised": True,
        "policy_audit_exercised": True,
        "promotion_authorization_exercised": True,
        "promotion_workflow_exercised": False,
        "github_publication_plan_exercised": True,
        "receipt_failure_exercised": True,
        "receipt_recovery_exercised": True,
        "receipt_workflow_exercised": False,
        "expired_audit_drill_exercised": True,
        "corrupt_asset_drill_exercised": True,
        "synthetic_tag_boundary": True,
        "synthetic_release_state": True,
        "cryptographic_tag_signature_exercised": False,
        "oidc_attestations_exercised": False,
    }
    write_object(args.output_dir / "promotion-simulation-record.json", record)
    print(json.dumps(record, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, UnicodeError, ValueError) as error:
        print(f"promotion-simulation: {error}", file=sys.stderr)
        raise SystemExit(1) from error
