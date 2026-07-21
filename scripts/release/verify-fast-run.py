#!/usr/bin/env python3
"""Fail-closed verification of an explicitly selected fast or qualification run."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from github_actions import (
    get_jobs,
    get_repository,
    get_run,
    jobs_from_json,
    read_json,
)

ROOT = Path(__file__).resolve().parents[2]
CONTRACT_PATH = ".github/release/candidate-contract.json"
STANDARD_RUNNER_LABELS = {
    "macos-15",
    "macos-15-intel",
    "ubuntu-22.04",
    "ubuntu-22.04-arm",
    "ubuntu-latest",
    "windows-latest",
}


def timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be an ISO-8601 string")
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"invalid {label}: {value}") from error


def git(root: Path, *arguments: str) -> bytes:
    completed = subprocess.run(
        ["git", *arguments], cwd=root, check=False, capture_output=True
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise ValueError(f"git {' '.join(arguments)} failed: {detail}")
    return completed.stdout


def load_committed_contract(root: Path, source_sha: str) -> tuple[bytes, str]:
    resolved = git(root, "rev-parse", f"{source_sha}^{{commit}}").decode().strip()
    if resolved != source_sha:
        raise ValueError(f"source SHA did not resolve exactly: {resolved}")
    entry = git(root, "ls-tree", "-z", source_sha, "--", CONTRACT_PATH)
    if not entry.endswith(b"\x00") or entry.count(b"\x00") != 1:
        raise ValueError("committed candidate contract did not resolve exactly once")
    try:
        metadata, encoded_path = entry[:-1].split(b"\t", 1)
        encoded_mode, encoded_type, encoded_oid = metadata.split(b" ")
        mode = encoded_mode.decode("ascii")
        object_type = encoded_type.decode("ascii")
        blob_oid = encoded_oid.decode("ascii")
        path = encoded_path.decode("utf-8")
    except (UnicodeDecodeError, ValueError) as error:
        raise ValueError("invalid committed candidate contract tree entry") from error
    if path != CONTRACT_PATH or mode != "100644" or object_type != "blob":
        raise ValueError(
            "committed candidate contract must be one regular non-executable blob"
        )
    return git(root, "cat-file", "blob", blob_oid), blob_oid


def load_inputs(
    args: argparse.Namespace,
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]]]:
    fixture_paths = [args.repository_json, args.run_json, args.jobs_json]
    if any(fixture_paths) and not all(fixture_paths):
        raise ValueError("repository, run, and jobs fixtures must be provided together")
    if all(fixture_paths):
        repository = read_json(args.repository_json)
        run = read_json(args.run_json)
        jobs = jobs_from_json(read_json(args.jobs_json))
        if not isinstance(repository, dict) or not isinstance(run, dict):
            raise ValueError("repository and run fixtures must be objects")
        return repository, run, jobs
    repository = get_repository(args.repository)
    run = get_run(args.repository, args.run_id)
    jobs = get_jobs(args.repository, args.run_id, 1)
    run_after = get_run(args.repository, args.run_id)
    stable_fields = (
        "id",
        "workflow_id",
        "path",
        "name",
        "event",
        "head_branch",
        "head_sha",
        "run_attempt",
        "status",
        "conclusion",
        "updated_at",
    )
    for field in stable_fields:
        if run.get(field) != run_after.get(field):
            raise ValueError(f"run changed while jobs were paginated: {field}")
    return repository, run_after, jobs


def require_equal(actual: Any, expected: Any, label: str) -> None:
    if actual != expected:
        raise ValueError(f"{label} mismatch: expected {expected!r}, got {actual!r}")


def contracted_runner_labels(
    contract: dict[str, Any], expected_job_names: set[str]
) -> tuple[dict[str, Any], dict[str, str], set[str]]:
    policy = contract.get("runner_policy")
    if not isinstance(policy, dict):
        raise ValueError("runner policy is missing")
    require_equal(policy.get("provider"), "github_standard_hosted", "runner provider")
    runner_group_id = policy.get("runner_group_id")
    if type(runner_group_id) is not int or runner_group_id != 0:
        raise ValueError("runner group ID must be the integer zero")
    require_equal(
        policy.get("runner_group_name"), "GitHub Actions", "runner group name"
    )
    jobs_by_label = policy.get("jobs_by_label")
    if not isinstance(jobs_by_label, dict):
        raise ValueError("runner jobs_by_label must be an object")
    require_equal(
        set(jobs_by_label), STANDARD_RUNNER_LABELS, "standard runner label set"
    )
    non_runner_jobs_value = policy.get("non_runner_jobs")
    if not isinstance(non_runner_jobs_value, list) or not all(
        isinstance(name, str) and name for name in non_runner_jobs_value
    ):
        raise ValueError("runner non_runner_jobs must contain job names")
    non_runner_jobs = set(non_runner_jobs_value)
    if len(non_runner_jobs) != len(non_runner_jobs_value):
        raise ValueError("runner non_runner_jobs contains duplicates")
    job_labels: dict[str, str] = {}
    for label, names in jobs_by_label.items():
        if not isinstance(names, list) or not all(
            isinstance(name, str) and name for name in names
        ):
            raise ValueError(f"runner jobs for {label} must be non-empty strings")
        for name in names:
            if name in job_labels:
                raise ValueError(f"runner policy assigns {name!r} more than once")
            job_labels[name] = label
    contracted_job_names: set[str] = set()
    for run_kind in ("fast_run", "qualification_run", "candidate_run"):
        run_contract = contract.get(run_kind)
        if not isinstance(run_contract, dict):
            raise ValueError(f"{run_kind} contract is missing")
        for field in ("success_jobs", "skipped_jobs"):
            names = run_contract.get(field)
            if not isinstance(names, list) or not all(
                isinstance(name, str) and name for name in names
            ):
                raise ValueError(f"{run_kind}.{field} must contain job names")
            contracted_job_names.update(names)
        allowed_jobs = run_contract.get("allowed_jobs")
        if not isinstance(allowed_jobs, dict):
            raise ValueError(f"{run_kind}.allowed_jobs must be an object")
        contracted_job_names.update(allowed_jobs)
    if set(job_labels) & non_runner_jobs:
        raise ValueError("a non-runner job also has a runner label")
    require_equal(
        set(job_labels) | non_runner_jobs,
        contracted_job_names,
        "runner policy job set",
    )
    if not expected_job_names <= (set(job_labels) | non_runner_jobs):
        raise ValueError("current run has a job without a runner policy assignment")
    return policy, job_labels, non_runner_jobs


def verify_non_runner_job(job: dict[str, Any]) -> None:
    name = job["name"]
    require_equal(job.get("labels"), [], f"job {name} runner labels")
    for field in ("runner_id", "runner_name", "runner_group_id", "runner_group_name"):
        if field not in job:
            raise ValueError(f"non-runner job {name} is missing {field}")
        require_equal(job[field], None, f"non-runner job {name} {field}")


def verify_job_runner(
    job: dict[str, Any], policy: dict[str, Any], expected_label: str
) -> None:
    name = job["name"]
    labels = job.get("labels")
    require_equal(labels, [expected_label], f"job {name} runner labels")
    if "self-hosted" in labels:
        raise ValueError(f"job {name} used a self-hosted runner")
    if job.get("conclusion") == "skipped":
        for field in (
            "runner_id",
            "runner_name",
            "runner_group_id",
            "runner_group_name",
        ):
            if field not in job:
                raise ValueError(f"skipped job {name} is missing {field}")
            require_equal(job[field], None, f"skipped job {name} {field}")
        return
    runner_id = job.get("runner_id")
    if type(runner_id) is not int or runner_id <= 0:
        raise ValueError(f"job {name} runner ID must be a positive integer")
    runner_group_id = job.get("runner_group_id")
    if type(runner_group_id) is not int or runner_group_id != policy["runner_group_id"]:
        raise ValueError(f"job {name} group ID must be the integer zero")
    require_equal(
        job.get("runner_group_name"),
        policy["runner_group_name"],
        f"job {name} group name",
    )
    require_equal(
        job.get("runner_name"), f"GitHub Actions {runner_id}", f"job {name} runner name"
    )


def verify(
    args: argparse.Namespace,
    contract_bytes: bytes,
    contract: dict[str, Any],
    repository: dict[str, Any],
    run: dict[str, Any],
    jobs: list[dict[str, Any]],
) -> dict[str, Any]:
    repository_contract = contract["repository"]
    require_equal(
        args.repository, repository_contract["full_name"], "requested repository"
    )
    require_equal(repository.get("id"), repository_contract["id"], "repository ID")
    require_equal(
        repository.get("full_name"), repository_contract["full_name"], "repository name"
    )
    require_equal(
        repository.get("default_branch"),
        repository_contract["default_branch"],
        "default branch",
    )
    require_equal(
        repository.get("visibility"), repository_contract["visibility"], "visibility"
    )

    run_contract = contract[f"{args.kind}_run"]
    require_equal(run.get("id"), args.run_id, "run ID")
    require_equal(run.get("workflow_id"), run_contract["workflow_id"], "workflow ID")
    require_equal(run.get("path"), run_contract["workflow_path"], "workflow path")
    require_equal(run.get("name"), run_contract["workflow_name"], "workflow name")
    require_equal(run.get("event"), run_contract["event"], "run event")
    require_equal(run.get("head_branch"), run_contract["branch"], "head branch")
    require_equal(run.get("head_sha"), args.expected_source_sha, "source SHA")
    require_equal(
        run.get("run_attempt"), run_contract["required_run_attempt"], "run attempt"
    )
    require_equal(run.get("status"), "completed", "run status")
    require_equal(run.get("conclusion"), "success", "run conclusion")
    for field in ("repository", "head_repository"):
        identity = run.get(field)
        if not isinstance(identity, dict):
            raise ValueError(f"run {field} identity is missing")
        require_equal(identity.get("id"), repository_contract["id"], f"run {field} ID")
        require_equal(
            identity.get("full_name"),
            repository_contract["full_name"],
            f"run {field} name",
        )

    now = timestamp(args.now, "--now") if args.now else datetime.now(timezone.utc)
    started = timestamp(run.get("run_started_at"), "run_started_at")
    age_seconds = int((now - started).total_seconds())
    if age_seconds < -300:
        raise ValueError("run_started_at is more than five minutes in the future")
    freshness_hours = int(
        run_contract.get("freshness_hours", contract["fast_run"]["freshness_hours"])
    )
    if age_seconds > freshness_hours * 3600:
        raise ValueError(
            f"run proof is stale: {age_seconds}s exceeds {freshness_hours}h policy"
        )

    expected: dict[str, tuple[str, ...]] = {
        **{name: ("success",) for name in run_contract["success_jobs"]},
        **{name: ("skipped",) for name in run_contract["skipped_jobs"]},
        **{
            name: tuple(conclusions)
            for name, conclusions in run_contract["allowed_jobs"].items()
        },
    }
    names = [job.get("name") for job in jobs]
    if not all(isinstance(name, str) and name for name in names):
        raise ValueError("every job must have a non-empty name")
    duplicate_names = sorted(
        name for name, count in Counter(names).items() if count != 1
    )
    if duplicate_names:
        raise ValueError(f"run contains duplicate job names: {duplicate_names}")
    job_ids = [job.get("id") for job in jobs]
    if not all(type(job_id) is int and job_id > 0 for job_id in job_ids):
        raise ValueError("every job must have a positive integer ID")
    if len(job_ids) != len(set(job_ids)):
        raise ValueError("run contains duplicate job IDs")
    actual_names = set(names)
    expected_names = set(expected)
    if actual_names != expected_names:
        missing = sorted(expected_names - actual_names)
        unexpected = sorted(actual_names - expected_names)
        raise ValueError(
            f"job set mismatch; missing={missing}, unexpected={unexpected}"
        )
    runner_policy, job_labels, non_runner_jobs = contracted_runner_labels(
        contract, expected_names
    )
    for job in jobs:
        require_equal(job.get("status"), "completed", f"job {job['name']} status")
        require_equal(job.get("run_id"), args.run_id, f"job {job['name']} run ID")
        require_equal(
            job.get("run_attempt"),
            run_contract["required_run_attempt"],
            f"job {job['name']} run attempt",
        )
        require_equal(
            job.get("head_sha"), args.expected_source_sha, f"job {job['name']} SHA"
        )
        require_equal(
            job.get("workflow_name"),
            run_contract["workflow_name"],
            f"job {job['name']} workflow",
        )
        if job.get("conclusion") not in expected[job["name"]]:
            raise ValueError(
                f"job {job['name']} conclusion must be one of {expected[job['name']]!r}, "
                f"got {job.get('conclusion')!r}"
            )
        if job["name"] in non_runner_jobs:
            verify_non_runner_job(job)
        else:
            verify_job_runner(job, runner_policy, job_labels[job["name"]])
        created = timestamp(job.get("created_at"), f"job {job['name']} created_at")
        job_started = timestamp(job.get("started_at"), f"job {job['name']} started_at")
        completed = timestamp(
            job.get("completed_at"), f"job {job['name']} completed_at"
        )
        if (
            job.get("conclusion") == "success"
            and not created <= job_started <= completed
        ):
            raise ValueError(f"job {job['name']} timestamps are not ordered")

    job_proof = sorted(
        (
            {
                "id": job["id"],
                "name": job["name"],
                "conclusion": job["conclusion"],
                "labels": job["labels"],
                "runner_id": job.get("runner_id"),
                "runner_name": job.get("runner_name"),
                "runner_group_id": job.get("runner_group_id"),
                "runner_group_name": job.get("runner_group_name"),
            }
            for job in jobs
        ),
        key=lambda item: item["name"],
    )
    proof: dict[str, Any] = {
        "schema_version": 1,
        "kind": args.kind,
        "repository_id": repository["id"],
        "run_id": run["id"],
        "run_attempt": run["run_attempt"],
        "source_git_sha": run["head_sha"],
        "run_started_at": run["run_started_at"],
        "verified_at": now.isoformat().replace("+00:00", "Z"),
        "contract_sha256": hashlib.sha256(contract_bytes).hexdigest(),
        "contract_blob_oid": args.contract_blob_oid,
        "test_fixture": args.fixture_mode,
        "jobs": job_proof,
    }
    canonical = json.dumps(proof, sort_keys=True, separators=(",", ":")).encode()
    proof["proof_sha256"] = hashlib.sha256(canonical).hexdigest()
    return proof


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--expected-source-sha", required=True)
    parser.add_argument(
        "--kind", choices=("fast", "qualification", "candidate"), required=True
    )
    parser.add_argument("--repository-root", type=Path, default=ROOT)
    parser.add_argument("--now")
    parser.add_argument("--repository-json", type=Path)
    parser.add_argument("--run-json", type=Path)
    parser.add_argument("--jobs-json", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not re_full_sha(args.expected_source_sha):
        raise ValueError(
            "--expected-source-sha must be exactly 40 lowercase hex characters"
        )
    fixture_paths = [args.repository_json, args.run_json, args.jobs_json]
    args.fixture_mode = all(fixture_paths)
    if args.now and not args.fixture_mode:
        raise ValueError("--now is restricted to complete offline test fixtures")
    contract_bytes, args.contract_blob_oid = load_committed_contract(
        args.repository_root.resolve(), args.expected_source_sha
    )
    contract = json.loads(contract_bytes)
    repository, run, jobs = load_inputs(args)
    proof = verify(args, contract_bytes, contract, repository, run, jobs)
    print(json.dumps(proof, indent=2, sort_keys=True))
    return 0


def re_full_sha(value: str) -> bool:
    return len(value) == 40 and all(
        character in "0123456789abcdef" for character in value
    )


if __name__ == "__main__":
    raise SystemExit(main())
