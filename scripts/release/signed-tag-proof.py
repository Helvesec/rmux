#!/usr/bin/env python3
"""Create or verify the typed proof of one exact signed annotated tag."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from release_evidence import (
    REPOSITORY_ID,
    file_hash,
    read_object,
    timestamp,
    write_object,
)
from release_tag_policy import load_signer_policy

STATUS = "verified-signed-annotated-tag"
ROOT = Path(__file__).resolve().parents[2]
SIGNER_POLICY = ROOT / ".github/release/release-signers.json"


def expected(args: argparse.Namespace) -> dict[str, Any]:
    verification = read_object(args.verification, "tag verification")
    required = {
        "mode",
        "ref",
        "release_ref",
        "release_intent_id",
        "release_kind",
        "source_git_sha",
        "tag_object_sha",
        "candidate_run_id",
        "candidate_manifest_artifact_id",
        "candidate_manifest_artifact_digest",
        "candidate_manifest_sha256",
        "release_policy_root_sha256",
        "signer_principal",
        "key_fingerprint",
        "signature_format",
    }
    if set(verification) != required:
        raise ValueError("tag verification fields changed")
    if (
        verification["mode"] != "github-json-verified"
        or verification["ref"] != f"refs/tags/{verification['release_ref']}"
        or verification["signature_format"] != "ssh"
    ):
        raise ValueError("tag verification is not a GitHub-verified SSH tag")
    policy = load_signer_policy(SIGNER_POLICY)
    policy.require_enabled()
    matching = [
        signer
        for signer in policy.allowed_signers
        if signer.principal == verification["signer_principal"]
        and signer.fingerprint == verification["key_fingerprint"]
    ]
    if len(matching) != 1:
        raise ValueError("tag signer is not uniquely allowlisted")
    timestamp(args.verified_at, "tag proof verified_at")
    return {
        "schema_version": 1,
        "status": STATUS,
        "repository_id": REPOSITORY_ID,
        "release_ref": verification["release_ref"],
        "release_intent_id": verification["release_intent_id"],
        "release_kind": verification["release_kind"],
        "tag_object_sha": verification["tag_object_sha"],
        "target_git_sha": verification["source_git_sha"],
        "candidate_run_id": verification["candidate_run_id"],
        "candidate_manifest_artifact_id": verification[
            "candidate_manifest_artifact_id"
        ],
        "candidate_manifest_artifact_digest": verification[
            "candidate_manifest_artifact_digest"
        ],
        "candidate_manifest_sha256": verification["candidate_manifest_sha256"],
        "release_policy_root_sha256": verification["release_policy_root_sha256"],
        "object_type": "tag",
        "annotated": True,
        "signature": {
            "verified": True,
            "format": "ssh",
            "key_fingerprint": verification["key_fingerprint"],
            "signing_principal": verification["signer_principal"],
        },
        "verified_at": args.verified_at,
    }


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--verification", type=Path, required=True)
    parser.add_argument("--verified-at", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    add_arguments(create)
    create.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser("verify")
    add_arguments(verify)
    verify.add_argument("--document", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    value = expected(args)
    if args.command == "create":
        write_object(args.output, value)
        print(file_hash(args.output))
        return 0
    if read_object(args.document, "signed tag proof") != value:
        raise ValueError("signed tag proof changed")
    print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, UnicodeError, ValueError) as error:
        print(f"signed-tag-proof: {error}", file=sys.stderr)
        raise SystemExit(1) from error
