#!/usr/bin/env python3
"""Authorize one receipt replay from protected main after a startup failure."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Any

from github_actions import gh_api, read_json

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
RECEIPT_WORKFLOW_ID = 316435347
CI_WORKFLOW_ID = 277622540
SHA40 = re.compile(r"[0-9a-f]{40}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
ACTIVE = frozenset({"queued", "in_progress", "requested", "waiting", "pending"})


def object_fixture(path: Path | None, endpoint: str) -> dict[str, Any]:
    value = read_json(path) if path else gh_api(endpoint)
    if not isinstance(value, dict):
        raise ValueError(f"{endpoint} did not return one object")
    return value


def exact_repository(value: dict[str, Any], label: str) -> None:
    if (
        value.get("repository", {}).get("id") != REPOSITORY_ID
        or value.get("head_repository", {}).get("id") != REPOSITORY_ID
    ):
        raise ValueError(f"{label} repository identity differs")


def exact_run(value: dict[str, Any], expected: dict[str, Any], label: str) -> None:
    for field, wanted in expected.items():
        if value.get(field) != wanted:
            raise ValueError(f"{label} {field} differs")
    exact_repository(value, label)


def empty_collection(value: dict[str, Any], field: str, label: str) -> None:
    items = value.get(field)
    if value.get("total_count") != 0 or items != []:
        raise ValueError(f"{label} is not empty")


def verify(args: argparse.Namespace) -> None:
    if args.failed_run_id <= 0 or args.current_run_id <= 0 or args.release_id <= 0:
        raise ValueError("run and release IDs must be positive")
    if args.failed_run_id == args.current_run_id:
        raise ValueError("recovery and failed receipt run IDs must differ")
    if SHA40.fullmatch(args.control_sha) is None:
        raise ValueError("recovery control SHA is invalid")
    if SHA40.fullmatch(args.source_sha) is None or args.source_sha == args.control_sha:
        raise ValueError("release source SHA is invalid or not distinct")
    if RELEASE_REF.fullmatch(args.release_ref) is None:
        raise ValueError("release ref is invalid")

    failed = object_fixture(
        args.failed_run_json,
        f"repos/{REPOSITORY}/actions/runs/{args.failed_run_id}",
    )
    exact_run(
        failed,
        {
            "id": args.failed_run_id,
            "workflow_id": RECEIPT_WORKFLOW_ID,
            "path": ".github/workflows/release-receipt.yml",
            "event": "workflow_dispatch",
            "run_attempt": 1,
            "head_sha": args.source_sha,
            "head_branch": args.release_ref,
            "status": "completed",
            "conclusion": "startup_failure",
        },
        "failed receipt run",
    )
    jobs = object_fixture(
        args.failed_jobs_json,
        f"repos/{REPOSITORY}/actions/runs/{args.failed_run_id}/jobs?filter=all&per_page=100",
    )
    empty_collection(jobs, "jobs", "failed receipt job set")
    artifacts = object_fixture(
        args.failed_artifacts_json,
        f"repos/{REPOSITORY}/actions/runs/{args.failed_run_id}/artifacts?per_page=100",
    )
    empty_collection(artifacts, "artifacts", "failed receipt artifact set")

    current = object_fixture(
        args.current_run_json,
        f"repos/{REPOSITORY}/actions/runs/{args.current_run_id}",
    )
    exact_run(
        current,
        {
            "id": args.current_run_id,
            "workflow_id": RECEIPT_WORKFLOW_ID,
            "path": ".github/workflows/release-receipt.yml",
            "event": "workflow_dispatch",
            "run_attempt": 1,
            "head_sha": args.control_sha,
            "head_branch": "main",
        },
        "recovery receipt run",
    )
    if current.get("status") not in ACTIVE or current.get("conclusion") is not None:
        raise ValueError("recovery receipt run is not active")

    main_ref = object_fixture(
        args.main_ref_json,
        f"repos/{REPOSITORY}/git/ref/heads/main",
    )
    if (
        main_ref.get("ref") != "refs/heads/main"
        or main_ref.get("object", {}).get("type") != "commit"
        or main_ref.get("object", {}).get("sha") != args.control_sha
    ):
        raise ValueError("protected main no longer points at the recovery control SHA")
    commit = object_fixture(
        args.control_commit_json,
        f"repos/{REPOSITORY}/commits/{args.control_sha}",
    )
    verification = commit.get("commit", {}).get("verification", {})
    if (
        commit.get("sha") != args.control_sha
        or verification.get("verified") is not True
        or verification.get("reason") != "valid"
    ):
        raise ValueError("recovery control commit is not GitHub-verified")

    ci_runs = object_fixture(
        args.ci_runs_json,
        f"repos/{REPOSITORY}/actions/workflows/ci.yml/runs"
        f"?branch=main&event=push&status=success&head_sha={args.control_sha}&per_page=100",
    )
    runs = ci_runs.get("workflow_runs")
    if not isinstance(runs, list):
        raise ValueError("CI run query has no workflow_runs array")
    matches = [
        run
        for run in runs
        if isinstance(run, dict)
        and run.get("workflow_id") == CI_WORKFLOW_ID
        and run.get("head_sha") == args.control_sha
        and run.get("head_branch") == "main"
        and run.get("event") == "push"
        and run.get("run_attempt") == 1
        and run.get("status") == "completed"
        and run.get("conclusion") == "success"
        and run.get("repository", {}).get("id") == REPOSITORY_ID
    ]
    if len(matches) != 1:
        raise ValueError("recovery control SHA lacks one exact successful main CI run")

    name = f"rmux-publication-receipt-{args.source_sha}-{args.release_id}"
    existing = object_fixture(
        args.existing_receipts_json,
        f"repos/{REPOSITORY}/actions/artifacts?name={name}&per_page=100",
    )
    receipts = existing.get("artifacts")
    if not isinstance(receipts, list) or existing.get("total_count") != len(receipts):
        raise ValueError("existing receipt artifact query is malformed")
    if any(
        isinstance(item, dict) and item.get("expired") is False for item in receipts
    ):
        raise ValueError("a live receipt artifact already exists for this release")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--failed-run-id", type=int, required=True)
    parser.add_argument("--current-run-id", type=int, required=True)
    parser.add_argument("--control-sha", required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-id", type=int, required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--failed-run-json", type=Path)
    parser.add_argument("--failed-jobs-json", type=Path)
    parser.add_argument("--failed-artifacts-json", type=Path)
    parser.add_argument("--current-run-json", type=Path)
    parser.add_argument("--main-ref-json", type=Path)
    parser.add_argument("--control-commit-json", type=Path)
    parser.add_argument("--ci-runs-json", type=Path)
    parser.add_argument("--existing-receipts-json", type=Path)
    return parser.parse_args()


if __name__ == "__main__":
    try:
        verify(parse_args())
        print("receipt-recovery-ok")
    except (KeyError, OSError, ValueError) as error:
        print(f"receipt-recovery: {error}", file=sys.stderr)
        raise SystemExit(1) from error
