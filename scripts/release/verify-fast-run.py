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
    blob_oid = git(root, "rev-parse", f"{source_sha}:{CONTRACT_PATH}").decode().strip()
    object_type = git(root, "cat-file", "-t", blob_oid).decode().strip()
    if object_type != "blob":
        raise ValueError(f"committed candidate contract is not a blob: {object_type}")
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
    freshness_hours = int(contract["fast_run"]["freshness_hours"])
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
    if not all(isinstance(job_id, int) and job_id > 0 for job_id in job_ids):
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
            {"id": job["id"], "name": job["name"], "conclusion": job["conclusion"]}
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
    parser.add_argument("--kind", choices=("fast", "qualification"), required=True)
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
