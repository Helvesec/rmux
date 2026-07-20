#!/usr/bin/env python3
"""Render or validate the byte-canonical message for an RMUX release tag."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from release_tag_policy import PolicyError, ReleaseTagIdentity, parse_message


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
    return ReleaseTagIdentity(
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


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    render = subparsers.add_parser("render")
    add_identity_arguments(render)
    render.add_argument("--output", type=Path)
    verify = subparsers.add_parser("verify")
    verify.add_argument("--message", type=Path, required=True)
    add_identity_arguments(verify)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    expected = identity_from_arguments(args)
    expected.validate()
    if args.command == "render":
        message = expected.message()
        if args.output is None:
            sys.stdout.write(message)
        else:
            args.output.write_text(message, encoding="utf-8", newline="\n")
        return 0
    message = args.message.read_text(encoding="utf-8")
    actual = parse_message(message)
    if actual != expected:
        raise PolicyError("tag message identity differs from the expected release")
    print(f"release-tag-message-ok ref={actual.release_ref}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, UnicodeError, PolicyError) as error:
        print(f"release tag message error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
