#!/usr/bin/env python3
"""Authorize one fail-closed receipt replay from protected main."""

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
EARLY_SUCCESS_JOBS = frozenset(
    {
        "Verify immutable Release and create receipt",
        "Audit live downstream repository authority",
        "Receipt-gated downstream publication / Prepare non-authoritative downstream plan",
        "Receipt-gated downstream publication / Verify exact downstream repository authority audit",
        "Receipt-gated downstream publication / Prepare exact downstream payloads / Materialize exact channel payloads",
    }
)
LINUX_SIGNING_JOB = (
    "Receipt-gated downstream publication / Build exact signed Linux repository trees / "
    "Sign retained APT and RPM repository trees"
)
OWNED_WRITER_JOBS = frozenset(
    {
        "Receipt-gated downstream publication / Publish exact Homebrew tap formula / Publish owned channel homebrew_tap",
        "Receipt-gated downstream publication / Publish exact Scoop manifest / Publish owned channel scoop",
        "Receipt-gated downstream publication / Publish exact Web Share WASM bytes / Publish owned channel web_share",
    }
)
SKIPPED_AFTER_FAILURE = frozenset(
    {
        "Receipt-gated downstream publication / Build exact signed Linux repository trees / Publish only this run's authorized recovery artifact",
        "Receipt-gated downstream publication / Publish exact signed APT and RPM repositories",
        "Receipt-gated downstream publication / Publish exact crates.io package set",
        "Receipt-gated downstream publication / Record denied RC Linux repository channel",
        "Receipt-gated downstream publication / Submit exact Chocolatey package",
        "Receipt-gated downstream publication / Record disabled Snap stable channel",
        "Receipt-gated downstream publication / Record manual Homebrew Core submission",
        "Receipt-gated downstream publication / Record manual WinGet submission",
        "Receipt-gated downstream publication / Publish exact Snap candidate revisions",
        "Receipt-gated downstream publication / Aggregate ten exact pre-site results",
        "Receipt-gated downstream publication / Record blocked automated rmux.io update",
        "Receipt-gated downstream publication / Prepare manual rmux.io handoff",
        "Receipt-gated downstream publication / Aggregate all eleven exact channel results",
    }
)
PAYLOAD_CHANNELS = (
    "apt_rpm",
    "chocolatey",
    "crates_io",
    "homebrew_core",
    "homebrew_tap",
    "scoop",
    "snap_candidate",
    "snap_stable",
    "web_share",
    "winget",
)


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


def verified_commit(value: dict[str, Any], sha: str, label: str) -> None:
    verification = value.get("commit", {}).get("verification", {})
    if (
        value.get("sha") != sha
        or verification.get("verified") is not True
        or verification.get("reason") != "valid"
    ):
        raise ValueError(f"{label} is not GitHub-verified")


def job_steps(job: dict[str, Any], label: str) -> dict[str, str]:
    steps = job.get("steps")
    if not isinstance(steps, list):
        raise ValueError(f"{label} has no step list")
    result: dict[str, str] = {}
    for step in steps:
        if not isinstance(step, dict):
            raise ValueError(f"{label} has a malformed step")
        name = step.get("name")
        conclusion = step.get("conclusion")
        if not isinstance(name, str) or not isinstance(conclusion, str):
            raise ValueError(f"{label} step identity is malformed")
        if name in result and result[name] != conclusion:
            raise ValueError(f"{label} duplicate step conclusions differ")
        result[name] = conclusion
    return result


def exact_step(steps: dict[str, str], name: str, conclusion: str, label: str) -> None:
    if steps.get(name) != conclusion:
        raise ValueError(f"{label} step {name} did not conclude {conclusion}")


def verify_failed_downstream_jobs(value: dict[str, Any]) -> None:
    jobs = value.get("jobs")
    if not isinstance(jobs, list) or value.get("total_count") != len(jobs):
        raise ValueError("failed downstream job set is malformed")
    by_name: dict[str, dict[str, Any]] = {}
    for job in jobs:
        if not isinstance(job, dict) or not isinstance(job.get("name"), str):
            raise ValueError("failed downstream job identity is malformed")
        name = job["name"]
        if name in by_name:
            raise ValueError("failed downstream job names are not unique")
        by_name[name] = job
    expected_names = (
        EARLY_SUCCESS_JOBS
        | {LINUX_SIGNING_JOB}
        | OWNED_WRITER_JOBS
        | SKIPPED_AFTER_FAILURE
    )
    if set(by_name) != expected_names:
        raise ValueError("failed downstream job topology differs")
    for name in EARLY_SUCCESS_JOBS:
        if by_name[name].get("conclusion") != "success":
            raise ValueError(f"failed downstream prerequisite {name} is not successful")
    for name in SKIPPED_AFTER_FAILURE:
        if (
            by_name[name].get("conclusion") != "skipped"
            or by_name[name].get("steps") != []
        ):
            raise ValueError(f"post-failure downstream job {name} was not untouched")

    linux = by_name[LINUX_SIGNING_JOB]
    if linux.get("conclusion") != "failure":
        raise ValueError("Linux repository signing did not fail closed")
    linux_steps = job_steps(linux, LINUX_SIGNING_JOB)
    exact_step(
        linux_steps,
        "Import distinct repository signing keys",
        "failure",
        LINUX_SIGNING_JOB,
    )
    for name in (
        "Authenticate retained history and generate signed repositories",
        "Add static package host files and exact checksum inventory",
        "Upload the exact signed repository tree",
    ):
        exact_step(linux_steps, name, "skipped", LINUX_SIGNING_JOB)

    for name in OWNED_WRITER_JOBS:
        job = by_name[name]
        if job.get("conclusion") != "failure":
            raise ValueError(f"owned repository job {name} did not fail closed")
        steps = job_steps(job, name)
        exact_step(
            steps,
            "Mint a repository-scoped downstream writer token",
            "failure",
            name,
        )
        exact_step(
            steps,
            "Publish and reread exact repository bytes",
            "skipped",
            name,
        )
        exact_step(
            steps,
            "Seal exact owned repository result evidence",
            "skipped",
            name,
        )


def verify_failed_downstream_artifacts(
    value: dict[str, Any], args: argparse.Namespace, failed_control_sha: str
) -> int:
    artifacts = value.get("artifacts")
    if not isinstance(artifacts, list) or value.get("total_count") != len(artifacts):
        raise ValueError("failed downstream artifact set is malformed")
    expected_names = {
        f"rmux-publication-receipt-{args.source_sha}-{args.release_id}",
        f"rmux-publication-receipt-envelope-{args.source_sha}-{args.release_id}",
        f"rmux-downstream-authority-{args.source_sha}-{args.failed_run_id}",
        f"rmux-downstream-plan-{args.source_sha}-{args.release_id}",
        *(
            f"rmux-downstream-{channel}-payload-{args.source_sha}-{args.release_id}"
            for channel in PAYLOAD_CHANNELS
        ),
    }
    by_name: dict[str, dict[str, Any]] = {}
    for artifact in artifacts:
        if not isinstance(artifact, dict) or not isinstance(artifact.get("name"), str):
            raise ValueError("failed downstream artifact identity is malformed")
        name = artifact["name"]
        if name in by_name:
            raise ValueError("failed downstream artifact names are not unique")
        by_name[name] = artifact
    if set(by_name) != expected_names:
        raise ValueError("failed downstream artifact topology differs")
    for name, artifact in by_name.items():
        workflow_run = artifact.get("workflow_run", {})
        if (
            type(artifact.get("id")) is not int
            or artifact["id"] <= 0
            or artifact.get("expired") is not False
            or not isinstance(artifact.get("digest"), str)
            or re.fullmatch(r"sha256:[0-9a-f]{64}", artifact["digest"]) is None
            or workflow_run.get("id") != args.failed_run_id
            or workflow_run.get("head_sha") != failed_control_sha
            or workflow_run.get("head_branch") != "main"
            or workflow_run.get("repository_id") != REPOSITORY_ID
            or workflow_run.get("head_repository_id") != REPOSITORY_ID
        ):
            raise ValueError(f"failed downstream artifact {name} identity differs")
    return by_name[f"rmux-publication-receipt-{args.source_sha}-{args.release_id}"][
        "id"
    ]


def verify_failed_attempt(
    args: argparse.Namespace,
    failed: dict[str, Any],
    jobs: dict[str, Any],
    artifacts: dict[str, Any],
) -> int | None:
    common = {
        "id": args.failed_run_id,
        "workflow_id": RECEIPT_WORKFLOW_ID,
        "path": ".github/workflows/release-receipt.yml",
        "event": "workflow_dispatch",
        "run_attempt": 1,
        "status": "completed",
    }
    if failed.get("conclusion") == "startup_failure":
        exact_run(
            failed,
            common
            | {
                "head_sha": args.source_sha,
                "head_branch": args.release_ref,
                "conclusion": "startup_failure",
            },
            "failed receipt run",
        )
        empty_collection(jobs, "jobs", "failed receipt job set")
        empty_collection(artifacts, "artifacts", "failed receipt artifact set")
        return None
    if failed.get("conclusion") != "failure":
        raise ValueError("failed receipt conclusion is not recoverable")
    failed_control_sha = failed.get("head_sha")
    if (
        not isinstance(failed_control_sha, str)
        or SHA40.fullmatch(failed_control_sha) is None
        or failed_control_sha == args.source_sha
    ):
        raise ValueError("failed downstream control SHA is invalid")
    exact_run(
        failed,
        common
        | {
            "head_sha": failed_control_sha,
            "head_branch": "main",
            "conclusion": "failure",
        },
        "failed downstream receipt run",
    )
    failed_commit = object_fixture(
        args.failed_control_commit_json,
        f"repos/{REPOSITORY}/commits/{failed_control_sha}",
    )
    verified_commit(
        failed_commit, failed_control_sha, "failed downstream control commit"
    )
    verify_failed_downstream_jobs(jobs)
    return verify_failed_downstream_artifacts(artifacts, args, failed_control_sha)


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
    jobs = object_fixture(
        args.failed_jobs_json,
        f"repos/{REPOSITORY}/actions/runs/{args.failed_run_id}/jobs?filter=all&per_page=100",
    )
    artifacts = object_fixture(
        args.failed_artifacts_json,
        f"repos/{REPOSITORY}/actions/runs/{args.failed_run_id}/artifacts?per_page=100",
    )
    failed_receipt_artifact_id = verify_failed_attempt(args, failed, jobs, artifacts)

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
    verified_commit(commit, args.control_sha, "recovery control commit")

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
    live = [
        item
        for item in receipts
        if isinstance(item, dict) and item.get("expired") is False
    ]
    if failed_receipt_artifact_id is None:
        if live:
            raise ValueError("a live receipt artifact already exists for this release")
    elif (
        len(live) != 1
        or live[0].get("id") != failed_receipt_artifact_id
        or live[0].get("name") != name
        or live[0].get("workflow_run", {}).get("id") != args.failed_run_id
    ):
        raise ValueError("the recoverable failed run is not the sole live receipt")


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
    parser.add_argument("--failed-control-commit-json", type=Path)
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
