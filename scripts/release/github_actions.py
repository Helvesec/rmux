#!/usr/bin/env python3
"""Small read-only helpers for inspecting GitHub Actions REST objects."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

API_VERSION = "2026-03-10"


class GitHubApiError(RuntimeError):
    """Raised when a read-only GitHub API request cannot be completed."""


def _decode_json(payload: str, source: str) -> Any:
    try:
        return json.loads(payload)
    except json.JSONDecodeError as error:
        raise GitHubApiError(f"invalid JSON from {source}: {error}") from error


def read_json(path: Path) -> Any:
    return _decode_json(path.read_text(encoding="utf-8"), str(path))


def gh_api(path: str) -> Any:
    if os.environ.get("RMUX_GITHUB_API_ANONYMOUS") == "1":
        request = Request(
            f"https://api.github.com/{path}",
            headers={
                "Accept": "application/vnd.github+json",
                "User-Agent": "rmux-release-shadow",
                "X-GitHub-Api-Version": API_VERSION,
            },
        )
        try:
            with urlopen(request, timeout=30) as response:
                payload = response.read().decode("utf-8")
        except (HTTPError, URLError, TimeoutError) as error:
            raise GitHubApiError(
                f"anonymous GitHub API GET {path} failed: {error}"
            ) from error
        return _decode_json(payload, f"GitHub API {path}")
    command = [
        "gh",
        "api",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        f"X-GitHub-Api-Version: {API_VERSION}",
        path,
    ]
    completed = subprocess.run(command, check=False, capture_output=True, text=True)
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise GitHubApiError(f"GitHub API GET {path} failed: {detail}")
    return _decode_json(completed.stdout, f"GitHub API {path}")


def get_repository(repository: str) -> dict[str, Any]:
    result = gh_api(f"repos/{repository}")
    if not isinstance(result, dict):
        raise GitHubApiError("repository response is not an object")
    return result


def get_run(repository: str, run_id: int) -> dict[str, Any]:
    result = gh_api(f"repos/{repository}/actions/runs/{run_id}")
    if not isinstance(result, dict):
        raise GitHubApiError("workflow run response is not an object")
    return result


def get_jobs(repository: str, run_id: int, attempt: int) -> list[dict[str, Any]]:
    jobs: list[dict[str, Any]] = []
    expected_total: int | None = None
    page = 1
    while True:
        result = gh_api(
            f"repos/{repository}/actions/runs/{run_id}/attempts/{attempt}/jobs"
            f"?per_page=100&page={page}"
        )
        if not isinstance(result, dict) or not isinstance(result.get("jobs"), list):
            raise GitHubApiError("workflow jobs response has no jobs array")
        total = result.get("total_count")
        if not isinstance(total, int) or total < 0:
            raise GitHubApiError("workflow jobs response has no valid total_count")
        if expected_total is None:
            expected_total = total
        elif total != expected_total:
            raise GitHubApiError("workflow jobs total_count changed during pagination")
        current = result["jobs"]
        jobs.extend(current)
        if len(jobs) >= expected_total:
            break
        if not current:
            raise GitHubApiError("workflow jobs pagination ended before total_count")
        page += 1
    if len(jobs) != expected_total:
        raise GitHubApiError(
            f"workflow jobs pagination returned {len(jobs)} jobs, expected {expected_total}"
        )
    job_ids = [job.get("id") for job in jobs]
    if any(not isinstance(job_id, int) for job_id in job_ids) or len(job_ids) != len(
        set(job_ids)
    ):
        raise GitHubApiError("workflow jobs contain missing or duplicate IDs")
    return jobs


def jobs_from_json(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, dict):
        value = value.get("jobs")
    if not isinstance(value, list) or not all(isinstance(job, dict) for job in value):
        raise GitHubApiError(
            "jobs fixture must be a jobs array or an object containing one"
        )
    return value
