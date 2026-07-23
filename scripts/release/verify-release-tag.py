#!/usr/bin/env python3
"""Verify an annotated RMUX release tag against the dedicated SSH allowlist."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

from release_tag_policy import (
    PolicyError,
    ReleaseTagIdentity,
    load_signer_policy,
    parse_message,
    verify_ssh_signature,
)

ROOT = Path(__file__).resolve().parents[2]
SIGNER_POLICY = ROOT / ".github" / "release" / "release-signers.json"
SSH_MARKER = b"-----BEGIN SSH SIGNATURE-----\n"


def add_identity_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--release-intent-id", required=True)
    parser.add_argument("--release-kind", choices=("rc", "stable"), required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--candidate-run-id", type=int, required=True)
    parser.add_argument("--candidate-manifest-artifact-id", type=int, required=True)
    parser.add_argument("--candidate-manifest-artifact-digest", required=True)
    parser.add_argument("--candidate-manifest-sha256", required=True)
    parser.add_argument("--release-policy-root-sha256", required=True)


def identity_from_arguments(args: argparse.Namespace) -> ReleaseTagIdentity:
    identity = ReleaseTagIdentity(
        release_ref=args.release_ref,
        release_intent_id=args.release_intent_id,
        release_kind=args.release_kind,
        source_git_sha=args.source_sha,
        candidate_run_id=args.candidate_run_id,
        candidate_manifest_artifact_id=args.candidate_manifest_artifact_id,
        candidate_manifest_artifact_digest=args.candidate_manifest_artifact_digest,
        candidate_manifest_sha256=args.candidate_manifest_sha256,
        release_policy_root_sha256=args.release_policy_root_sha256,
    )
    identity.validate()
    return identity


def run_git(repository: Path, *arguments: str, text: bool = False) -> bytes | str:
    result = subprocess.run(
        ["git", "-C", str(repository), *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        text=text,
    )
    if result.returncode != 0:
        stderr = result.stderr if text else result.stderr.decode("utf-8", "replace")
        raise PolicyError(f"git {' '.join(arguments)} failed: {stderr.strip()}")
    return result.stdout


def split_signed_tag(raw: bytes) -> tuple[bytes, bytes, bytes]:
    if b"\r" in raw or b"\0" in raw:
        raise PolicyError("tag object must be LF-only and cannot contain NUL")
    if raw.count(SSH_MARKER) != 1:
        raise PolicyError("tag object must contain exactly one SSH signature")
    marker = raw.index(SSH_MARKER)
    payload = raw[:marker]
    signature = raw[marker:]
    if not signature.endswith(b"-----END SSH SIGNATURE-----\n"):
        raise PolicyError("tag SSH signature is not canonically terminated")
    separator = payload.find(b"\n\n")
    if separator < 0:
        raise PolicyError("tag object has no header/message separator")
    message = payload[separator + 2 :]
    return payload, signature, message


def parse_tag_headers(payload: bytes, identity: ReleaseTagIdentity) -> None:
    header_bytes, separator, _ = payload.partition(b"\n\n")
    if not separator:
        raise PolicyError("tag object has no header block")
    try:
        lines = header_bytes.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise PolicyError("tag headers are not UTF-8") from error
    if len(lines) != 4:
        raise PolicyError("tag object must have exactly four canonical headers")
    if lines[0] != f"object {identity.source_git_sha}" or lines[1] != "type commit":
        raise PolicyError("tag must point directly to the expected commit")
    if lines[2] != f"tag {identity.release_ref}":
        raise PolicyError("tag object's internal name differs from the release ref")
    if not lines[3].startswith("tagger ") or " <" not in lines[3]:
        raise PolicyError("tag object has no canonical tagger identity")


def object_sha(raw: bytes) -> str:
    header = f"tag {len(raw)}\0".encode()
    return hashlib.sha1(header + raw, usedforsecurity=False).hexdigest()


def verify_raw_tag(
    raw: bytes,
    expected_object_sha: str,
    identity: ReleaseTagIdentity,
) -> dict[str, Any]:
    actual_sha = object_sha(raw)
    if actual_sha != expected_object_sha:
        raise PolicyError(
            f"tag object hash mismatch: expected {expected_object_sha}, got {actual_sha}"
        )
    payload, signature, message_bytes = split_signed_tag(raw)
    parse_tag_headers(payload, identity)
    try:
        message = message_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        raise PolicyError("tag message is not UTF-8") from error
    actual_identity = parse_message(message)
    if actual_identity != identity:
        raise PolicyError("signed tag identity differs from the expected release")
    policy = load_signer_policy(SIGNER_POLICY)
    signer = verify_ssh_signature(policy, payload, signature)
    signer_policy = next(
        item for item in policy.allowed_signers if item.principal == signer
    )
    return {
        "mode": "verified",
        "ref": identity.full_ref,
        "release_ref": identity.release_ref,
        "release_intent_id": identity.release_intent_id,
        "release_kind": identity.release_kind,
        "source_git_sha": identity.source_git_sha,
        "tag_object_sha": actual_sha,
        "candidate_run_id": identity.candidate_run_id,
        "candidate_manifest_artifact_id": identity.candidate_manifest_artifact_id,
        "candidate_manifest_artifact_digest": identity.candidate_manifest_artifact_digest,
        "candidate_manifest_sha256": identity.candidate_manifest_sha256,
        "release_policy_root_sha256": identity.release_policy_root_sha256,
        "signer_principal": signer,
        "key_fingerprint": signer_policy.fingerprint,
        "signature_format": "ssh",
    }


def verify_local(
    args: argparse.Namespace, identity: ReleaseTagIdentity
) -> dict[str, Any]:
    repository = args.repository.resolve(strict=True)
    full_ref = identity.full_ref
    ref_type = run_git(repository, "cat-file", "-t", full_ref, text=True)
    if ref_type.strip() != "tag":
        raise PolicyError("release ref must point to an annotated tag object")
    tag_sha = run_git(repository, "rev-parse", "--verify", full_ref, text=True).strip()
    if not isinstance(tag_sha, str) or len(tag_sha) != 40:
        raise PolicyError("release ref did not resolve to a SHA-1 tag object")
    raw = run_git(repository, "cat-file", "tag", tag_sha)
    if not isinstance(raw, bytes):
        raise PolicyError("internal error while reading tag object")
    commit_type = run_git(
        repository, "cat-file", "-t", identity.source_git_sha, text=True
    )
    if commit_type.strip() != "commit":
        raise PolicyError("expected source object is not a commit")
    return verify_raw_tag(raw, tag_sha, identity)


def load_json_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PolicyError(f"cannot read {label}: {error}") from error
    if not isinstance(value, dict):
        raise PolicyError(f"{label} must contain an object")
    return value


def require_string(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise PolicyError(f"{label} must be a string")
    return value


def verify_github_json(
    args: argparse.Namespace, identity: ReleaseTagIdentity
) -> dict[str, Any]:
    ref = load_json_object(args.ref_json, "GitHub ref JSON")
    tag = load_json_object(args.tag_json, "GitHub tag JSON")
    if ref.get("ref") != identity.full_ref:
        raise PolicyError("GitHub ref name differs from the expected release ref")
    ref_object = ref.get("object")
    if not isinstance(ref_object, dict) or ref_object.get("type") != "tag":
        raise PolicyError("GitHub ref must point to an annotated tag object")
    tag_sha = require_string(ref_object.get("sha"), "GitHub ref object SHA").lower()
    if tag.get("sha") != tag_sha or tag.get("tag") != identity.release_ref:
        raise PolicyError("GitHub tag object identity differs from its ref")
    target = tag.get("object")
    if (
        not isinstance(target, dict)
        or target.get("type") != "commit"
        or target.get("sha") != identity.source_git_sha
    ):
        raise PolicyError("GitHub tag must point directly to the expected commit")
    verification = tag.get("verification")
    if not isinstance(verification, dict) or verification.get("verified") is not True:
        reason = verification.get("reason") if isinstance(verification, dict) else None
        raise PolicyError(
            f"GitHub did not verify the tag signature (reason={reason!r})"
        )
    payload_text = require_string(verification.get("payload"), "signature payload")
    signature_text = require_string(verification.get("signature"), "signature envelope")
    raw = payload_text.encode("utf-8") + signature_text.encode("utf-8")
    result = verify_raw_tag(raw, tag_sha, identity)
    github_message = require_string(tag.get("message"), "GitHub tag message")
    if github_message != identity.message() + signature_text:
        raise PolicyError(
            "GitHub tag message differs from the canonical message and signature"
        )
    result["mode"] = "github-json-verified"
    return result


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    local = subparsers.add_parser("local")
    local.add_argument("--repository", type=Path, required=True)
    add_identity_arguments(local)
    github_json = subparsers.add_parser("github-json")
    github_json.add_argument("--ref-json", type=Path, required=True)
    github_json.add_argument("--tag-json", type=Path, required=True)
    add_identity_arguments(github_json)
    subparsers.add_parser("policy")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    if args.command == "policy":
        policy = load_signer_policy(SIGNER_POLICY)
        policy.require_enabled()
        print(
            json.dumps(
                {
                    "mode": "signer-policy-enabled",
                    "signer_count": len(policy.allowed_signers),
                },
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return 0
    identity = identity_from_arguments(args)
    if args.command == "local":
        result = verify_local(args, identity)
    else:
        result = verify_github_json(args, identity)
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, UnicodeError, PolicyError) as error:
        print(f"release tag verification error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
