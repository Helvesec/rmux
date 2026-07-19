#!/usr/bin/env python3
"""Emit stable JSON timings for one explicitly selected Actions run."""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from github_actions import get_jobs, get_run, jobs_from_json, read_json


def parse_timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str) or not value:
        raise ValueError(f"missing timestamp {label}")
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"invalid timestamp {label}: {value}") from error


def seconds(start: datetime, end: datetime) -> int:
    if end < start:
        raise ValueError(
            f"timestamp order is negative: {start.isoformat()} > {end.isoformat()}"
        )
    return int((end - start).total_seconds())


def load_inputs(
    args: argparse.Namespace,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    if bool(args.run_json) != bool(args.jobs_json):
        raise ValueError("--run-json and --jobs-json must be provided together")
    if args.run_json:
        run_before = read_json(args.run_json)
        run_after = (
            read_json(args.run_after_json) if args.run_after_json else run_before
        )
        jobs = jobs_from_json(read_json(args.jobs_json))
        if not isinstance(run_before, dict) or not isinstance(run_after, dict):
            raise ValueError("run fixtures must be objects")
    else:
        if args.run_after_json:
            raise ValueError("--run-after-json is only valid with both fixtures")
        run_before = get_run(args.repository, args.run_id)
        jobs = get_jobs(args.repository, args.run_id, args.attempt)
        run_after = get_run(args.repository, args.run_id)
    for field in (
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
    ):
        if run_before.get(field) != run_after.get(field):
            raise ValueError(f"run changed while jobs were paginated: {field}")
    return run_after, jobs


def step_record(step: dict[str, Any]) -> dict[str, Any]:
    record: dict[str, Any] = {
        "name": step.get("name"),
        "number": step.get("number"),
        "status": step.get("status"),
        "conclusion": step.get("conclusion"),
    }
    if step.get("started_at") and step.get("completed_at"):
        start = parse_timestamp(step["started_at"], "step.started_at")
        end = parse_timestamp(step["completed_at"], "step.completed_at")
        record["duration_sec"] = seconds(start, end)
    else:
        record["duration_sec"] = None
    return record


def utilization_curve(
    run_start: datetime,
    running_events: dict[datetime, int],
    queue_events: dict[datetime, int],
) -> tuple[list[dict[str, int]], int, int]:
    running = 0
    queued = 0
    max_running = 0
    max_queued = 0
    curve: list[dict[str, int]] = []
    for event_time in sorted(set(running_events) | set(queue_events)):
        running += running_events.get(event_time, 0)
        queued += queue_events.get(event_time, 0)
        if running < 0 or queued < 0:
            raise ValueError("job timestamps produced a negative utilization count")
        max_running = max(max_running, running)
        max_queued = max(max_queued, queued)
        curve.append(
            {
                "t_seconds": seconds(run_start, event_time),
                "running_jobs": running,
                "queued_jobs": queued,
            }
        )
    return curve, max_running, max_queued


def build_report(
    repository: str,
    run: dict[str, Any],
    jobs: list[dict[str, Any]],
    required_checks: list[str],
) -> dict[str, Any]:
    if len(required_checks) != len(set(required_checks)):
        raise ValueError("required check arguments must be unique")
    run_id = int(run["id"])
    run_start = parse_timestamp(run["run_started_at"], "run.run_started_at")
    job_records: list[dict[str, Any]] = []
    running_events: dict[datetime, int] = {}
    queue_events: dict[datetime, int] = {}
    macos_running_events: dict[datetime, int] = {}
    macos_queue_events: dict[datetime, int] = {}
    completed: list[datetime] = []
    started: list[datetime] = []

    for job in jobs:
        job_start = (
            parse_timestamp(job["started_at"], f"job {job.get('name')} started_at")
            if job.get("started_at")
            else None
        )
        job_end = (
            parse_timestamp(job["completed_at"], f"job {job.get('name')} completed_at")
            if job.get("completed_at")
            else None
        )
        job_created = (
            parse_timestamp(job["created_at"], f"job {job.get('name')} created_at")
            if job.get("created_at")
            else None
        )
        timestamps_ordered = not (
            job_created
            and job_start
            and job_end
            and not job_created <= job_start <= job_end
        )
        skipped_api_anomaly = bool(
            not timestamps_ordered
            and job.get("status") == "completed"
            and job.get("conclusion") == "skipped"
            and job_created
            and job_start
            and job_end
            and not job.get("steps")
            and max(
                abs((job_start - job_created).total_seconds()),
                abs((job_end - job_created).total_seconds()),
                abs((job_end - job_start).total_seconds()),
            )
            <= 5
        )
        if not timestamps_ordered and not skipped_api_anomaly:
            raise ValueError(f"job {job.get('name')} timestamps are not ordered")
        labels = job.get("labels", [])
        is_macos = isinstance(labels, list) and any(
            isinstance(label, str) and label.startswith("macos-") for label in labels
        )
        if job_start and timestamps_ordered:
            started.append(job_start)
            running_events[job_start] = running_events.get(job_start, 0) + 1
            if is_macos:
                macos_running_events[job_start] = (
                    macos_running_events.get(job_start, 0) + 1
                )
        if job_end and timestamps_ordered:
            completed.append(job_end)
            running_events[job_end] = running_events.get(job_end, 0) - 1
            if is_macos:
                macos_running_events[job_end] = macos_running_events.get(job_end, 0) - 1
        if job_created and job_start and timestamps_ordered:
            queue_events[job_created] = queue_events.get(job_created, 0) + 1
            queue_events[job_start] = queue_events.get(job_start, 0) - 1
            if is_macos:
                macos_queue_events[job_created] = (
                    macos_queue_events.get(job_created, 0) + 1
                )
                macos_queue_events[job_start] = macos_queue_events.get(job_start, 0) - 1
        job_records.append(
            {
                "id": job.get("id"),
                "name": job.get("name"),
                "status": job.get("status"),
                "conclusion": job.get("conclusion"),
                "started_at": job.get("started_at"),
                "completed_at": job.get("completed_at"),
                "runner_name": job.get("runner_name"),
                "runner_group_name": job.get("runner_group_name"),
                "labels": labels,
                "created_at": job.get("created_at"),
                "timestamp_anomaly": not timestamps_ordered,
                "timestamp_anomaly_kind": (
                    "github_skipped_job_clock" if skipped_api_anomaly else None
                ),
                "runner_wait_from_run_start_sec": (
                    seconds(run_start, job_start)
                    if job_start and timestamps_ordered
                    else None
                ),
                "runner_queue_sec": (
                    seconds(job_created, job_start)
                    if job_created and job_start and timestamps_ordered
                    else None
                ),
                "duration_sec": (
                    seconds(job_start, job_end)
                    if job_start and job_end and timestamps_ordered
                    else None
                ),
                "steps": [step_record(step) for step in job.get("steps", [])],
            }
        )

    observed: dict[str, dict[str, Any]] = {}
    for job in jobs:
        name = str(job.get("name"))
        if name in observed:
            raise ValueError(
                f"duplicate job name prevents unambiguous measurement: {name}"
            )
        observed[name] = job
    missing = sorted(set(required_checks) - observed.keys())
    if missing:
        raise ValueError(
            f"required checks missing from run {run_id}: {', '.join(missing)}"
        )
    invalid = sorted(
        name
        for name in required_checks
        if observed[name].get("conclusion") != "success"
    )
    if invalid:
        raise ValueError(f"required checks not successful: {', '.join(invalid)}")

    end_candidates = (
        [
            parse_timestamp(observed[name]["completed_at"], f"required job {name}")
            for name in required_checks
        ]
        if required_checks
        else completed
    )
    if not end_candidates:
        raise ValueError("run has no completed jobs")
    measured_end = max(end_candidates)
    all_jobs_end = max(completed)

    curve, max_running, max_queued = utilization_curve(
        run_start, running_events, queue_events
    )
    macos_curve, max_macos_running, max_macos_queued = utilization_curve(
        run_start, macos_running_events, macos_queue_events
    )

    return {
        "schema_version": 1,
        "repository": repository,
        "run": {
            "id": run_id,
            "workflow_id": run.get("workflow_id"),
            "path": run.get("path"),
            "event": run.get("event"),
            "head_branch": run.get("head_branch"),
            "head_sha": run.get("head_sha"),
            "run_attempt": run.get("run_attempt"),
            "status": run.get("status"),
            "conclusion": run.get("conclusion"),
            "run_started_at": run.get("run_started_at"),
            "measured_end_at": measured_end.isoformat().replace("+00:00", "Z"),
            "all_jobs_end_at": all_jobs_end.isoformat().replace("+00:00", "Z"),
        },
        "summary": {
            "wallclock_sec": seconds(run_start, measured_end),
            "all_jobs_wallclock_sec": seconds(run_start, all_jobs_end),
            "first_job_wait_sec": seconds(run_start, min(started)) if started else None,
            "max_running_jobs": max_running,
            "max_queued_jobs": max_queued,
            "max_macos_running_jobs": max_macos_running,
            "max_macos_queued_jobs": max_macos_queued,
            "job_count": len(jobs),
            "required_check_count": len(required_checks),
            "total_job_runtime_sec": sum(
                record["duration_sec"] or 0 for record in job_records
            ),
        },
        "utilization": curve,
        "utilization_macos": macos_curve,
        "jobs": sorted(
            job_records, key=lambda record: (str(record["name"]), record["id"] or 0)
        ),
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--attempt", required=True, type=int)
    parser.add_argument("--expected-sha")
    parser.add_argument("--expected-event")
    parser.add_argument("--expected-branch")
    parser.add_argument("--expected-workflow-id", type=int)
    parser.add_argument("--expected-workflow-path")
    parser.add_argument("--expected-conclusion")
    parser.add_argument("--required-check", action="append", default=[])
    parser.add_argument("--run-json", type=Path)
    parser.add_argument("--run-after-json", type=Path)
    parser.add_argument("--jobs-json", type=Path)
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    run, jobs = load_inputs(args)
    if int(run.get("id", -1)) != args.run_id:
        raise ValueError("selected run ID does not match the run response")
    if int(run.get("run_attempt", -1)) != args.attempt:
        raise ValueError("selected attempt does not match the run response")
    if run.get("status") != "completed":
        raise ValueError("selected run is not completed")
    names = [job.get("name") for job in jobs]
    ids = [job.get("id") for job in jobs]
    if not all(isinstance(name, str) and name for name in names):
        raise ValueError("every measured job must have a non-empty name")
    if len(names) != len(set(names)):
        raise ValueError("measured jobs contain duplicate names")
    if not all(isinstance(job_id, int) and job_id > 0 for job_id in ids):
        raise ValueError("every measured job must have a positive ID")
    if len(ids) != len(set(ids)):
        raise ValueError("measured jobs contain duplicate IDs")
    for job in jobs:
        if job.get("status") != "completed" or not job.get("completed_at"):
            raise ValueError(f"job {job.get('name')} is not completed")
        if job.get("run_id") != args.run_id:
            raise ValueError(f"job {job.get('name')} belongs to another run")
        if job.get("run_attempt") != args.attempt:
            raise ValueError(f"job {job.get('name')} belongs to another attempt")
        if job.get("head_sha") != run.get("head_sha"):
            raise ValueError(f"job {job.get('name')} belongs to another SHA")
    expectations = {
        "head_sha": args.expected_sha,
        "event": args.expected_event,
        "head_branch": args.expected_branch,
        "workflow_id": args.expected_workflow_id,
        "path": args.expected_workflow_path,
        "conclusion": args.expected_conclusion,
    }
    for field, expected in expectations.items():
        if expected is not None and run.get(field) != expected:
            raise ValueError(
                f"run {field} mismatch: expected {expected!r}, got {run.get(field)!r}"
            )
    report = build_report(args.repository, run, jobs, args.required_check)
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
