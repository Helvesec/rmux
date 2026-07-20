#!/usr/bin/env python3
"""Plan or execute one fail-closed, idempotent GitHub Release publication."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from github_release_api import (
    API_ROOT,
    ApiClient,
    inspect_assets,
    release_identity,
    validate_freshness,
    verify_live_evidence,
    verify_live_inputs,
)

from release_publish_security import (
    require_publication_activation,
    verify_promotion_attestation,
)

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
MAX_NOTES_BYTES = 1024 * 1024
SHA40 = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
SAFE_NAME = re.compile(r"[A-Za-z0-9._+@=-]+")
PREDICATE_TYPE = "https://rmux.io/attestations/release-promotion-authorization/v1"
ENVELOPE_TYPE = "https://rmux.io/envelopes/release-promotion-authorization/v1"


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_time(value: Any, label: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be a canonical UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"{label} is not a valid timestamp") from error
    if parsed.tzinfo is None or parsed.utcoffset() != timedelta(0):
        raise ValueError(f"{label} must use UTC")
    canonical = parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
    if canonical != value:
        raise ValueError(f"{label} is not canonically encoded")
    return parsed


def positive(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def matches(value: Any, pattern: re.Pattern[str], label: str) -> str:
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise ValueError(f"{label} is not canonical")
    return value


@dataclass(frozen=True)
class Asset:
    name: str
    size: int
    sha256: str
    path: Path


@dataclass(frozen=True)
class Evidence:
    predicate: dict[str, Any]
    envelope: dict[str, Any]
    title: str
    notes: str
    assets: tuple[Asset, ...]

    @property
    def release_ref(self) -> str:
        return self.predicate["release"]["ref"]

    @property
    def source_sha(self) -> str:
        return self.predicate["source_git_sha"]

    @property
    def prerelease(self) -> bool:
        return self.predicate["release"]["is_prerelease"]


def validate_authorization(
    predicate: dict[str, Any], envelope: dict[str, Any], predicate_sha256: str
) -> None:
    predicate_keys = {
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
    }
    envelope_keys = {
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
    }
    if set(predicate) != predicate_keys or predicate.get("schema_version") != 1:
        raise ValueError("promotion predicate shape changed")
    if set(envelope) != envelope_keys or envelope.get("schema_version") != 1:
        raise ValueError("promotion envelope shape changed")
    if predicate.get("predicate_type") != PREDICATE_TYPE:
        raise ValueError("promotion predicate type changed")
    if predicate.get("repository") != {"id": REPOSITORY_ID, "full_name": REPOSITORY}:
        raise ValueError("promotion predicate repository changed")
    authority = predicate.get("publication_authority")
    expected_status = (
        "promotion-authorized" if authority is True else "disarmed-non-authoritative"
    )
    if type(authority) is not bool or predicate.get("status") != expected_status:
        raise ValueError("promotion predicate authority state is inconsistent")
    source = matches(predicate.get("source_git_sha"), SHA40, "authorization source SHA")
    release = predicate.get("release")
    if not isinstance(release, dict):
        raise ValueError("authorization release identity is missing")
    release_ref = matches(release.get("ref"), RELEASE_REF, "authorization release ref")
    if release.get("version") != release_ref[1:]:
        raise ValueError("authorization version differs from its release ref")
    is_rc = "-rc." in release_ref
    if release.get("is_prerelease") is not is_rc or release.get("kind") != (
        "rc" if is_rc else "stable"
    ):
        raise ValueError("authorization prerelease identity changed")
    candidate = predicate.get("candidate")
    candidate_keys = {
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
    }
    if (
        not isinstance(candidate, dict)
        or set(candidate) != candidate_keys
        or candidate.get("schema_version") != 1
        or candidate.get("status") != "shadow-non-authoritative"
        or candidate.get("repository_id") != REPOSITORY_ID
        or candidate.get("source_git_sha") != source
        or candidate.get("candidate_run_attempt") != 1
        or candidate.get("manifest_run_attempt") != 1
        or candidate.get("manifest_workflow_id") != 316223904
        or candidate.get("manifest_workflow_path")
        != ".github/workflows/release-shadow.yml"
    ):
        raise ValueError("authorization candidate identity changed")
    for field in ("candidate_run_id", "manifest_run_id", "manifest_artifact_id"):
        positive(candidate.get(field), f"candidate {field}")
    matches(
        candidate.get("manifest_artifact_digest"), DIGEST, "candidate artifact digest"
    )
    matches(candidate.get("manifest_sha256"), SHA256, "candidate manifest digest")
    authorization = predicate.get("authorization")
    if not isinstance(authorization, dict) or authorization.get("run_attempt") != 1:
        raise ValueError("promotion authorization must come from attempt 1")
    for field in ("run_id", "workflow_id"):
        positive(authorization.get(field), f"authorization {field}")
    if authorization.get("workflow_path") != ".github/workflows/release-promote.yml":
        raise ValueError("authorization workflow path changed")
    tag = predicate.get("signed_tag")
    if not isinstance(tag, dict) or any(
        (
            tag.get("status") != "verified-signed-annotated-tag",
            tag.get("repository_id") != REPOSITORY_ID,
            tag.get("release_ref") != release_ref,
            tag.get("release_intent_id") != release.get("intent_id"),
            tag.get("release_kind") != release.get("kind"),
            tag.get("target_git_sha") != source,
            tag.get("candidate_run_id") != candidate.get("candidate_run_id"),
            tag.get("candidate_manifest_artifact_id")
            != candidate.get("manifest_artifact_id"),
            tag.get("candidate_manifest_artifact_digest")
            != candidate.get("manifest_artifact_digest"),
            tag.get("candidate_manifest_sha256") != candidate.get("manifest_sha256"),
            tag.get("release_policy_root_sha256")
            != predicate.get("release_policy_sha256"),
            tag.get("object_type") != "tag",
            tag.get("annotated") is not True,
            not isinstance(tag.get("signature"), dict),
            tag.get("signature", {}).get("verified") is not True,
        )
    ):
        raise ValueError("authorization does not bind a verified signed annotated tag")
    matches(tag.get("tag_object_sha"), SHA40, "signed tag object SHA")
    audit = predicate.get("policy_audit")
    if not isinstance(audit, dict) or any(
        (
            audit.get("repository_id") != REPOSITORY_ID,
            audit.get("source_git_sha") != source,
            audit.get("release_intent_id") != release.get("intent_id"),
            audit.get("policy_audit_run_attempt") != 1,
            audit.get("workflow_id") != 316435346,
            audit.get("workflow_path") != ".github/workflows/release-promote.yml",
            audit.get("release_policy_sha256")
            != predicate.get("release_policy_sha256"),
        )
    ):
        raise ValueError("authorization policy audit binding changed")
    for field in ("policy_audit_run_id", "predicate_artifact_id", "workflow_id"):
        positive(audit.get(field), f"policy audit {field}")
    if audit.get("policy_audit_run_id") != authorization.get("run_id"):
        raise ValueError(
            "policy audit and authorization must share one exact promoter run"
        )
    matches(
        audit.get("predicate_artifact_digest"), DIGEST, "policy audit artifact digest"
    )
    for field in ("predicate_sha256", "reference_sha256", "release_policy_sha256"):
        matches(audit.get(field), SHA256, f"policy audit {field}")
    if envelope.get("envelope_type") != ENVELOPE_TYPE:
        raise ValueError("promotion envelope type changed")
    if (
        envelope.get("publication_authority") is not authority
        or envelope.get("status") != expected_status
    ):
        raise ValueError("promotion envelope authority differs from its predicate")
    bindings = {
        "repository_id": REPOSITORY_ID,
        "source_git_sha": source,
        "release_ref": release_ref,
        "release_intent_id": release.get("intent_id"),
        "authorization": authorization,
        "predicate_sha256": predicate_sha256,
        "sha256sums_sha256": predicate.get("sha256sums_sha256"),
    }
    if any(envelope.get(key) != value for key, value in bindings.items()):
        raise ValueError("promotion envelope does not bind the exact predicate")
    bundle = envelope.get("authorization_bundle")
    if (
        not isinstance(bundle, dict)
        or positive(bundle.get("artifact_id"), "authorization bundle artifact ID") <= 0
        or bundle.get("name") != f"rmux-promotion-authorization-{source}"
        or matches(bundle.get("archive_digest"), DIGEST, "authorization bundle digest")
        == ""
        or positive(bundle.get("size_in_bytes"), "authorization bundle size") <= 0
    ):
        raise ValueError("promotion authorization bundle identity changed")
    issued = parse_time(predicate.get("issued_at"), "authorization issued_at")
    created = parse_time(
        envelope.get("created_at"), "authorization envelope created_at"
    )
    expires = parse_time(predicate.get("expires_at"), "authorization expires_at")
    if not issued <= created <= expires:
        raise ValueError("promotion envelope was created outside its authorization TTL")


def expected_asset_records(
    predicate: dict[str, Any], envelope: dict[str, Any]
) -> list[dict[str, Any]]:
    records = predicate.get("assets")
    metadata = envelope.get("public_metadata_assets")
    if (
        not isinstance(records, list)
        or len(records) < 2
        or not isinstance(metadata, list)
        or len(metadata) != 1
    ):
        raise ValueError("authorization public asset cardinality changed")
    combined = records + metadata
    if predicate.get("asset_count") != len(records):
        raise ValueError("authorization asset count changed")
    names: set[str] = set()
    for record in combined:
        if not isinstance(record, dict):
            raise ValueError("authorized asset must be an object")
        name = matches(record.get("name"), SAFE_NAME, "authorized asset name")
        if name in names:
            raise ValueError(f"authorized asset is duplicated: {name}")
        names.add(name)
        positive(record.get("size"), f"authorized asset {name} size")
        matches(record.get("sha256"), SHA256, f"authorized asset {name} SHA-256")
    if "SHA256SUMS" not in names or "SHA256SUMS.sigstore.json" not in names:
        raise ValueError("authorization metadata asset set is incomplete")
    sums = next(item for item in combined if item["name"] == "SHA256SUMS")
    public = metadata[0]
    attestation = envelope.get("attestation")
    if (
        sums["sha256"] != predicate.get("sha256sums_sha256")
        or public.get("name") != "SHA256SUMS.sigstore.json"
        or not isinstance(attestation, dict)
        or attestation.get("bundle_file") != public["name"]
        or attestation.get("bundle_sha256") != public["sha256"]
    ):
        raise ValueError("authorization metadata hashes changed")
    return combined


def load_evidence(args: argparse.Namespace) -> Evidence:
    if args.predicate.is_symlink() or args.envelope.is_symlink():
        raise ValueError("promotion evidence cannot be a symlink")
    predicate_path = args.predicate.resolve(strict=True)
    envelope_path = args.envelope.resolve(strict=True)
    predicate = read_object(predicate_path, "promotion predicate")
    envelope = read_object(envelope_path, "promotion envelope")
    validate_authorization(predicate, envelope, file_hash(predicate_path))
    records = expected_asset_records(predicate, envelope)
    if args.assets_dir.is_symlink():
        raise ValueError("assets directory cannot be a symlink")
    assets_dir = args.assets_dir.resolve(strict=True)
    if not assets_dir.is_dir():
        raise ValueError("assets directory must be one real directory")
    actual_names = {item.name for item in assets_dir.iterdir()}
    expected_names = {item["name"] for item in records}
    if actual_names != expected_names:
        raise ValueError("assets directory does not contain the exact authorized set")
    assets: list[Asset] = []
    for record in records:
        path = assets_dir / record["name"]
        if path.is_symlink() or not path.is_file():
            raise ValueError(
                f"authorized asset is not a regular file: {record['name']}"
            )
        if path.stat().st_size != record["size"] or file_hash(path) != record["sha256"]:
            raise ValueError(f"authorized asset bytes changed: {record['name']}")
        assets.append(Asset(record["name"], record["size"], record["sha256"], path))
    title = args.title
    if not title or len(title) > 256 or any(ord(char) < 32 for char in title):
        raise ValueError("release title is invalid")
    try:
        notes_bytes = args.notes_file.read_bytes()
        notes = notes_bytes.decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise ValueError("release notes must be valid UTF-8") from error
    if not notes or len(notes_bytes) > MAX_NOTES_BYTES:
        raise ValueError("release notes are empty or too large")
    return Evidence(
        predicate,
        envelope,
        title,
        notes,
        tuple(sorted(assets, key=lambda item: item.name)),
    )


def parse_now(value: str | None, *, test_api: bool) -> datetime:
    if value is not None and not test_api:
        raise ValueError("--now is restricted to loopback API tests")
    return (
        datetime.now(timezone.utc)
        if value is None
        else parse_time(value, "publisher --now")
    )


def execute(args: argparse.Namespace, evidence: Evidence) -> dict[str, Any]:
    if evidence.predicate.get("publication_authority") is not True:
        raise ValueError("promotion authorization remains non-authoritative")
    require_publication_activation(args.activation_ledger)
    if not args.token or "\n" in args.token or "\r" in args.token:
        raise ValueError(
            "--execute requires one explicit GitHub token; environment fallback is forbidden"
        )
    now = parse_now(args.now, test_api=args.test_only_loopback_api)
    validate_freshness(evidence, now)
    if args.gh_verifier is None:
        raise ValueError("--execute requires one explicit pinned gh verifier")
    assets = {asset.name: asset.path for asset in evidence.assets}
    attested_assets = {
        asset["name"]: assets[asset["name"]]
        for asset in evidence.predicate["assets"]
        if asset["role"] != "checksums"
    }
    verify_promotion_attestation(
        gh=args.gh_verifier,
        assets=attested_assets,
        bundle=assets["SHA256SUMS.sigstore.json"],
        predicate=evidence.predicate,
        source_git_sha=evidence.source_sha,
        release_ref=evidence.release_ref,
    )
    client = ApiClient(args.api_root, args.token, test_api=args.test_only_loopback_api)
    verify_live_evidence(client, evidence, now)
    release = client.get_release(evidence.release_ref)
    if release is None:
        release = client.create_draft(evidence)
    elif release.get("draft") is not True:
        raise ValueError(
            "an existing public GitHub Release is never resumed or overwritten"
        )
    release_id = release_identity(release, evidence, draft=True)
    missing = inspect_assets(client.list_assets(release_id), evidence, complete=False)
    for asset in missing:
        client.upload(release_id, asset)
    verify_live_inputs(
        client,
        evidence,
        parse_now(args.now, test_api=args.test_only_loopback_api),
        release_id,
    )
    published = client.publish(release_id, prerelease=evidence.prerelease)
    release_identity(published, evidence, draft=False)
    if published.get("immutable") is not True or client.patch_count != 1:
        raise ValueError(
            "GitHub Release did not become immutable in one state transition"
        )
    return {
        "mode": "execute",
        "release_id": release_id,
        "release_ref": evidence.release_ref,
        "published": True,
    }


def plan(args: argparse.Namespace, evidence: Evidence) -> dict[str, Any]:
    client = ApiClient(args.api_root, None, test_api=args.test_only_loopback_api)
    release = client.get_release(evidence.release_ref)
    if release is None:
        missing = list(evidence.assets)
        action = "create-draft"
    else:
        if release.get("draft") is not True:
            raise ValueError(
                "an existing public GitHub Release is never resumed or overwritten"
            )
        release_id = release_identity(release, evidence, draft=True)
        missing = inspect_assets(
            client.list_assets(release_id), evidence, complete=False
        )
        action = "resume-exact-draft"
    return {
        "mode": "plan",
        "action": action,
        "release_ref": evidence.release_ref,
        "missing_assets": [asset.name for asset in missing],
        "mutations": False,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--predicate", type=Path, required=True)
    parser.add_argument("--envelope", type=Path, required=True)
    parser.add_argument("--assets-dir", type=Path, required=True)
    parser.add_argument("--notes-file", type=Path, required=True)
    parser.add_argument("--title", required=True)
    parser.add_argument(
        "--activation-ledger",
        type=Path,
        default=Path(".github/release/release-activation.json"),
    )
    parser.add_argument(
        "--execute",
        action="store_true",
        help="Perform the otherwise read-only publication plan",
    )
    parser.add_argument(
        "--token", help="Explicit short-lived token; no environment fallback"
    )
    parser.add_argument(
        "--gh-verifier",
        type=Path,
        help="Explicit pinned gh binary used for offline attestation verification",
    )
    parser.add_argument("--api-root", default=API_ROOT)
    parser.add_argument(
        "--test-only-loopback-api", action="store_true", help=argparse.SUPPRESS
    )
    parser.add_argument("--now", help=argparse.SUPPRESS)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    evidence = load_evidence(args)
    result = execute(args, evidence) if args.execute else plan(args, evidence)
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"release-publisher: {error}", file=sys.stderr)
        raise SystemExit(1) from error
