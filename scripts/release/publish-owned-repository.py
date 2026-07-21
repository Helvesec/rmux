#!/usr/bin/env python3
"""Publish one exact RMUX-owned repository payload with a GitHub App token."""

from __future__ import annotations

import argparse
import os
import re
import sys
from datetime import UTC, datetime
from pathlib import Path

from downstream_channels import target_for_channel, write_object
from downstream_result import validate_target_evidence
from github_repository_writer import GitHubApi, publish, repository_identity
from owned_repository_payload import repository_updates
from web_share_live import wait_for_live

SOURCE_SHA = re.compile(r"[0-9a-f]{40}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--channel", choices=("homebrew_tap", "scoop", "web_share"), required=True
    )
    parser.add_argument("--payload-dir", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--target-evidence", type=Path, required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    return parser.parse_args()


def execute(args: argparse.Namespace) -> None:
    if (
        SOURCE_SHA.fullmatch(args.source_sha) is None
        or RELEASE_REF.fullmatch(args.release_ref) is None
    ):
        raise ValueError("owned repository release identity is malformed")
    version = args.release_ref.removeprefix("v")
    target = target_for_channel(args.channel)
    full_name = target["repository_full_name"]
    repository_id = target["repository_id"]
    if not isinstance(full_name, str) or not isinstance(repository_id, int):
        raise ValueError("owned repository target identity is incomplete")
    updates = repository_updates(
        args.channel, args.payload_dir, source_sha=args.source_sha, version=version
    )
    api = GitHubApi(os.environ.get("RMUX_DOWNSTREAM_TOKEN", ""))
    repository_identity(api, full_name, repository_id, "main")
    outcome = publish(
        api,
        full_name=full_name,
        branch="main",
        updates=updates,
        message=f"release: publish rmux {args.release_ref}",
    )
    target_url = f"https://github.com/{full_name}/commit/{outcome.commit_sha}"
    if args.channel == "web_share":
        provenance = next(args.payload_dir.glob("*.provenance.json"), None)
        if provenance is None:
            raise ValueError("web share provenance payload is missing")
        target_url = wait_for_live(
            provenance_path=provenance,
            source_sha=args.source_sha,
            version=version,
            commit_sha=outcome.commit_sha,
        )
    observed_at = (
        datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    )
    evidence = {
        "schema_version": 1,
        "channel": args.channel,
        "target_kind": target["target_kind"],
        "repository_id": repository_id,
        "external_id": outcome.commit_sha,
        "url": target_url,
        "version": version,
        "commit_sha": outcome.commit_sha,
        "public_live": True,
        "observed_at": observed_at,
    }
    validate_target_evidence(
        evidence,
        channel=args.channel,
        state=outcome.state,
        expected_target=target,
        expected_version=version,
    )
    write_object(args.target_evidence, evidence)
    with args.github_output.open("a", encoding="utf-8") as output:
        output.write(f"state={outcome.state}\n")
        output.write(f"mutation_started={str(outcome.mutation_started).lower()}\n")
        output.write(f"remote_request_id={outcome.commit_sha}\n")
        output.write(f"observed_at={observed_at}\n")


if __name__ == "__main__":
    try:
        execute(parse_args())
    except (OSError, ValueError) as error:
        print(f"publish-owned-repository: {error}", file=sys.stderr)
        raise SystemExit(1) from error
