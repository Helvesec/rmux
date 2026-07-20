#!/usr/bin/env python3
"""Resolve or verify one immutable GitHub Actions artifact by numeric ID."""

from __future__ import annotations

import argparse
import json
import re
import time
from pathlib import Path
from typing import Any

from github_actions import GitHubApiError, gh_api, read_json

DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
SHA = re.compile(r"[0-9a-f]{40}")
REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790


def get_artifact_by_id(args: argparse.Namespace) -> dict[str, Any]:
    if args.artifact_json:
        value = read_json(args.artifact_json)
        if isinstance(value, dict) and "artifacts" in value:
            value = value["artifacts"]
        if isinstance(value, list):
            matches = [item for item in value if item.get("id") == args.artifact_id]
            if len(matches) != 1:
                raise ValueError(
                    "artifact fixture must contain exactly one requested artifact ID"
                )
            value = matches[0]
        if not isinstance(value, dict):
            raise ValueError("artifact fixture must be an artifact object")
        return value

    for attempt in range(1, args.max_attempts + 1):
        try:
            value = gh_api(
                f"repos/{args.repository}/actions/artifacts/{args.artifact_id}"
            )
        except GitHubApiError:
            if attempt == args.max_attempts:
                raise
            time.sleep(args.retry_delay_seconds)
            continue
        if not isinstance(value, dict):
            raise ValueError("artifact response must be an object")
        return value
    raise AssertionError("bounded artifact lookup exhausted without returning")


def verify_run_identity(args: argparse.Namespace) -> None:
    expected = (
        args.expected_workflow_id,
        args.expected_workflow_path,
        args.expected_event,
        args.expected_head_branch,
    )
    if all(value is None for value in expected):
        return
    if any(value is None for value in expected):
        raise ValueError("exact run identity options must be supplied together")
    run = (
        read_json(args.run_json)
        if args.run_json
        else gh_api(f"repos/{args.repository}/actions/runs/{args.run_id}")
    )
    if not isinstance(run, dict):
        raise ValueError("workflow run response must be an object")
    wanted = {
        "id": args.run_id,
        "workflow_id": args.expected_workflow_id,
        "path": args.expected_workflow_path,
        "event": args.expected_event,
        "run_attempt": 1,
        "head_sha": args.expected_source_sha,
        "head_branch": args.expected_head_branch,
        "status": "completed",
        "conclusion": "success",
    }
    for field, value in wanted.items():
        if run.get(field) != value:
            raise ValueError(f"workflow run {field} mismatch")
    if (
        run.get("repository", {}).get("id") != REPOSITORY_ID
        or run.get("head_repository", {}).get("id") != REPOSITORY_ID
    ):
        raise ValueError("workflow run repository identity mismatch")


def load_artifacts(args: argparse.Namespace) -> list[dict[str, Any]]:
    if args.command in {"resolve-id", "verify"}:
        return [get_artifact_by_id(args)]
    if args.artifact_json:
        value = read_json(args.artifact_json)
        if isinstance(value, dict) and "artifacts" in value:
            value = value["artifacts"]
        if not isinstance(value, list) or not all(
            isinstance(item, dict) for item in value
        ):
            raise ValueError("artifact fixture must be an artifact array")
        return value
    artifacts: list[dict[str, Any]] = []
    page = 1
    expected_total: int | None = None
    while True:
        value = gh_api(
            f"repos/{args.repository}/actions/runs/{args.run_id}/artifacts"
            f"?per_page=100&page={page}"
        )
        if not isinstance(value, dict) or not isinstance(value.get("artifacts"), list):
            raise ValueError("run artifact response has no artifacts array")
        total = value.get("total_count")
        if type(total) is not int or total < 0:
            raise ValueError("run artifact response has no valid total_count")
        if expected_total is None:
            expected_total = total
        elif expected_total != total:
            raise ValueError("artifact total changed during pagination")
        current = value["artifacts"]
        artifacts.extend(current)
        if len(artifacts) >= expected_total:
            break
        if not current:
            raise ValueError("artifact pagination ended early")
        page += 1
    if len(artifacts) != expected_total:
        raise ValueError("artifact pagination returned the wrong cardinality")
    return artifacts


def verify_artifact(
    artifact: dict[str, Any], args: argparse.Namespace
) -> dict[str, Any]:
    artifact_id = artifact.get("id")
    if type(artifact_id) is not int or artifact_id <= 0:
        raise ValueError("artifact ID must be a positive integer")
    if args.command in {"resolve-id", "verify"} and artifact_id != args.artifact_id:
        raise ValueError("artifact ID does not match the requested ID")
    if artifact.get("name") != args.name:
        raise ValueError("artifact name does not match the exact expected name")
    digest = artifact.get("digest")
    if not isinstance(digest, str) or DIGEST.fullmatch(digest) is None:
        raise ValueError("artifact digest is not a SHA-256 digest")
    if args.command == "verify" and digest != args.expected_digest:
        raise ValueError("artifact digest changed")
    if artifact.get("expired") is not False:
        raise ValueError("artifact is expired")
    size = artifact.get("size_in_bytes")
    if type(size) is not int or size <= 0:
        raise ValueError("artifact size must be a positive integer")
    workflow = artifact.get("workflow_run")
    if not isinstance(workflow, dict):
        raise ValueError("artifact workflow identity is missing")
    expected_workflow = {
        "id": args.run_id,
        "repository_id": REPOSITORY_ID,
        "head_repository_id": REPOSITORY_ID,
        "head_sha": args.expected_source_sha,
    }
    for field, expected in expected_workflow.items():
        if workflow.get(field) != expected:
            raise ValueError(f"artifact workflow {field} mismatch")
    return {
        "artifact_id": artifact_id,
        "name": artifact["name"],
        "digest": digest,
        "size_in_bytes": size,
        "run_id": args.run_id,
        "source_git_sha": args.expected_source_sha,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("resolve", "resolve-id", "verify"):
        command = commands.add_parser(name)
        command.add_argument("--repository", default=REPOSITORY)
        command.add_argument("--run-id", type=int, required=True)
        command.add_argument("--name", required=True)
        command.add_argument("--expected-source-sha", required=True)
        command.add_argument("--artifact-json", type=Path)
        command.add_argument("--github-output", type=Path)
        command.add_argument("--run-json", type=Path)
        command.add_argument("--expected-workflow-id", type=int)
        command.add_argument("--expected-workflow-path")
        command.add_argument(
            "--expected-event", choices=("workflow_call", "workflow_dispatch")
        )
        command.add_argument("--expected-head-branch")
        if name in {"resolve-id", "verify"}:
            command.add_argument("--artifact-id", type=int, required=True)
            command.add_argument("--max-attempts", type=int, default=1)
            command.add_argument("--retry-delay-seconds", type=int, default=0)
        if name == "verify":
            command.add_argument("--expected-digest", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.repository != REPOSITORY:
        raise ValueError(f"repository must be exactly {REPOSITORY}")
    if args.run_id <= 0:
        raise ValueError("run ID must be positive")
    if SHA.fullmatch(args.expected_source_sha) is None:
        raise ValueError("expected source SHA must be 40 lowercase hex characters")
    if args.command == "verify" and (
        not isinstance(args.expected_digest, str)
        or DIGEST.fullmatch(args.expected_digest) is None
    ):
        raise ValueError("expected digest is not canonical")
    if args.command in {"resolve-id", "verify"}:
        if args.artifact_id <= 0:
            raise ValueError("artifact ID must be positive")
        if not 1 <= args.max_attempts <= 10:
            raise ValueError("artifact lookup max attempts must be between 1 and 10")
        if not 0 <= args.retry_delay_seconds <= 10:
            raise ValueError("artifact retry delay must be between 0 and 10 seconds")
    artifacts = load_artifacts(args)
    verify_run_identity(args)
    if args.command == "resolve":
        matches = [
            artifact for artifact in artifacts if artifact.get("name") == args.name
        ]
        if len(matches) != 1:
            raise ValueError(
                f"expected exactly one artifact named {args.name!r}, got {len(matches)}"
            )
        artifact = matches[0]
    elif args.command == "verify":
        if len(artifacts) != 1:
            raise ValueError("artifact verification requires exactly one artifact")
        artifact = artifacts[0]
    else:
        if len(artifacts) != 1:
            raise ValueError("direct artifact resolution requires exactly one artifact")
        artifact = artifacts[0]
    result = verify_artifact(artifact, args)
    if args.github_output:
        with args.github_output.open("a", encoding="utf-8") as output:
            output.write(f"artifact_id={result['artifact_id']}\n")
            output.write(f"artifact_digest={result['digest']}\n")
            output.write(f"artifact_name={result['name']}\n")
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
