"""Two-phase aggregation for exact downstream result references."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from downstream_channels import (
    CHANNELS,
    REPOSITORY_ID,
    SHA256,
    exact_keys,
    file_hash,
    match,
    read_object,
    timestamp,
    validate_downstream_authority,
    validate_execution_authority,
    validate_embedded_receipt,
    validate_release,
)
from downstream_plan import validate_plan
from downstream_result_reference import validate_reference

PRE_SITE_CHANNELS = tuple(channel for channel in CHANNELS if channel != "rmux_io")
PHASES = {"pre-site", "final"}
RESOLVED_STATES = {"denied-by-policy", "no-op-exact", "public-live"}


def _validate_result_entry(
    value: Any,
    *,
    channel: str,
    source_sha: str,
    release: dict[str, Any],
    receipt: dict[str, Any],
    plan_sha256: str,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("channel summary result must be an object")
    exact_keys(
        value,
        {"channel", "reference_sha256", "reference"},
        "channel summary result",
    )
    if value["channel"] != channel:
        raise ValueError("channel summary result ordering changed")
    match(value["reference_sha256"], SHA256, "result reference digest")
    reference = validate_reference(value["reference"])
    expected = {
        "source_git_sha": source_sha,
        "release": release,
        "receipt": receipt,
        "plan_sha256": plan_sha256,
        "channel": channel,
    }
    for field, expected_value in expected.items():
        if reference[field] != expected_value:
            raise ValueError(f"result reference changed summary field {field}")
    return reference


def validate_summary(value: dict[str, Any]) -> dict[str, Any]:
    exact_keys(
        value,
        {
            "schema_version",
            "phase",
            "status",
            "downstream_authority",
            "execution_authority",
            "repository_id",
            "source_git_sha",
            "release",
            "receipt",
            "plan_sha256",
            "channel_policy_sha256",
            "result_aggregation_ready",
            "aggregation_blockers",
            "result_count",
            "results",
            "advertised_channels",
            "unresolved_channels",
            "rmux_io_last",
            "rmux_io_two_phase_ready",
            "rmux_io_authority",
            "pre_site_summary_sha256",
            "created_at",
        },
        "downstream channel summary",
    )
    phase = value["phase"]
    downstream_active = validate_downstream_authority(value)
    execution_active = validate_execution_authority(
        value, downstream_active=downstream_active
    )
    if (
        value["schema_version"] != 1
        or phase not in PHASES
        or value["repository_id"] != REPOSITORY_ID
        or value["result_aggregation_ready"] is not True
        or value["aggregation_blockers"] != []
        or value["rmux_io_last"] is not True
        or value["rmux_io_two_phase_ready"] is not True
    ):
        raise ValueError("channel summary identity or two-phase contract changed")
    if type(value["rmux_io_authority"]) is not bool or (
        value["rmux_io_authority"] and not execution_active
    ):
        raise ValueError("rmux.io authority exceeds downstream execution authority")
    source_sha = value["source_git_sha"]
    release = validate_release(value["release"], source_sha)
    receipt = validate_embedded_receipt(value["receipt"], source_sha, release)
    match(value["plan_sha256"], SHA256, "summary plan digest")
    match(value["channel_policy_sha256"], SHA256, "summary policy digest")
    expected_channels = PRE_SITE_CHANNELS if phase == "pre-site" else CHANNELS
    if value["result_count"] != len(expected_channels):
        raise ValueError("channel summary result count changed")
    results = value["results"]
    if not isinstance(results, list) or len(results) != len(expected_channels):
        raise ValueError("channel summary results are not exhaustive")
    references = [
        _validate_result_entry(
            item,
            channel=channel,
            source_sha=source_sha,
            release=release,
            receipt=receipt,
            plan_sha256=value["plan_sha256"],
        )
        for channel, item in zip(expected_channels, results, strict=True)
    ]
    if any(
        reference["downstream_authority"] is not downstream_active
        for reference in references
    ):
        raise ValueError("channel summary mixes authority states")
    advertised = [
        reference["channel"]
        for reference in references
        if reference["channel"] != "rmux_io" and reference["public_live"]
    ]
    unresolved = [
        reference["channel"]
        for reference in references
        if reference["state"] not in RESOLVED_STATES
    ]
    if value["advertised_channels"] != advertised:
        raise ValueError("summary advertised channels differ from public evidence")
    if value["unresolved_channels"] != unresolved:
        raise ValueError("summary unresolved channels differ from result states")
    parent = value["pre_site_summary_sha256"]
    if phase == "pre-site":
        if parent is not None:
            raise ValueError("pre-site summary cannot name a parent summary")
    else:
        match(parent, SHA256, "pre-site summary digest")
    created = timestamp(value["created_at"], "channel summary created_at")
    for reference in references:
        if created < timestamp(reference["verified_at"], "result verified_at"):
            raise ValueError("channel summary predates a verified result")
    return value


def _read_reference(path: Path, channel: str) -> tuple[dict[str, Any], str]:
    if path.is_symlink():
        raise ValueError(f"result reference for {channel} cannot be a symlink")
    resolved = path.resolve(strict=True)
    reference = read_object(resolved, f"result reference for {channel}")
    validate_reference(reference)
    if reference["channel"] != channel:
        raise ValueError("result reference channel differs from its mapping")
    return reference, file_hash(resolved)


def _validate_planned_state(reference: dict[str, Any], planned: dict[str, Any]) -> None:
    decision = planned["execution_decision"]
    state = reference["state"]
    if decision == "denied" and state != "denied-by-policy":
        raise ValueError("denied channel result differs from its exact plan")
    if decision == "blocked" and state != "blocked":
        raise ValueError("blocked channel result differs from its exact plan")
    if decision in {"disarmed", "enabled"} and state in {
        "blocked",
        "denied-by-policy",
    }:
        raise ValueError("unblocked channel result contradicts its exact plan")


def validate_summary_for_plan(
    value: dict[str, Any],
    *,
    plan: dict[str, Any],
    plan_sha256: str,
    expected_phase: str,
) -> dict[str, Any]:
    validate_summary(value)
    expected = {
        "phase": expected_phase,
        "source_git_sha": plan["source_git_sha"],
        "release": plan["release"],
        "receipt": plan["receipt"],
        "plan_sha256": plan_sha256,
        "channel_policy_sha256": plan["channel_policy"]["sha256"],
    }
    for field, expected_value in expected.items():
        if value[field] != expected_value:
            raise ValueError(f"channel summary changed exact field {field}")
    planned = {entry["name"]: entry for entry in plan["channels"]}
    for item in value["results"]:
        _validate_planned_state(item["reference"], planned[item["channel"]])
    return value


def create_summary(
    *,
    plan_path: Path,
    phase: str,
    result_paths: dict[str, Path],
    pre_site_summary_path: Path | None,
    created_at: str,
) -> dict[str, Any]:
    if plan_path.is_symlink():
        raise ValueError("downstream channel plan cannot be a symlink")
    plan_path = plan_path.resolve(strict=True)
    plan = read_object(plan_path, "downstream channel plan")
    validate_plan(plan)
    if phase not in PHASES:
        raise ValueError("channel summary phase is invalid")
    expected_inputs = set(PRE_SITE_CHANNELS if phase == "pre-site" else ("rmux_io",))
    if set(result_paths) != expected_inputs:
        raise ValueError("channel summary result reference set changed")
    plan_sha256 = file_hash(plan_path)
    planned = {entry["name"]: entry for entry in plan["channels"]}
    if phase == "pre-site":
        if pre_site_summary_path is not None:
            raise ValueError("pre-site phase cannot consume a prior summary")
        entries = []
        for channel in PRE_SITE_CHANNELS:
            reference, digest = _read_reference(result_paths[channel], channel)
            _validate_planned_state(reference, planned[channel])
            entries.append(
                {
                    "channel": channel,
                    "reference_sha256": digest,
                    "reference": reference,
                }
            )
        parent_digest = None
    else:
        if pre_site_summary_path is None or pre_site_summary_path.is_symlink():
            raise ValueError("final phase requires one regular pre-site summary")
        pre_site_summary_path = pre_site_summary_path.resolve(strict=True)
        prior = read_object(pre_site_summary_path, "pre-site channel summary")
        validate_summary_for_plan(
            prior,
            plan=plan,
            plan_sha256=plan_sha256,
            expected_phase="pre-site",
        )
        if timestamp(created_at, "channel summary created_at") < timestamp(
            prior["created_at"], "pre-site summary created_at"
        ):
            raise ValueError("final summary predates its pre-site summary")
        reference, digest = _read_reference(result_paths["rmux_io"], "rmux_io")
        _validate_planned_state(reference, planned["rmux_io"])
        entries = [
            *prior["results"],
            {
                "channel": "rmux_io",
                "reference_sha256": digest,
                "reference": reference,
            },
        ]
        entries.sort(key=lambda item: CHANNELS.index(item["channel"]))
        parent_digest = file_hash(pre_site_summary_path)
    value = {
        "schema_version": 1,
        "phase": phase,
        "status": plan["status"],
        "downstream_authority": plan["downstream_authority"],
        "execution_authority": plan["execution_authority"],
        "repository_id": REPOSITORY_ID,
        "source_git_sha": plan["source_git_sha"],
        "release": plan["release"],
        "receipt": plan["receipt"],
        "plan_sha256": plan_sha256,
        "channel_policy_sha256": plan["channel_policy"]["sha256"],
        "result_aggregation_ready": True,
        "aggregation_blockers": [],
        "result_count": len(entries),
        "results": entries,
        "advertised_channels": [
            item["channel"]
            for item in entries
            if item["channel"] != "rmux_io" and item["reference"]["public_live"]
        ],
        "unresolved_channels": [
            item["channel"]
            for item in entries
            if item["reference"]["state"] not in RESOLVED_STATES
        ],
        "rmux_io_last": True,
        "rmux_io_two_phase_ready": True,
        "rmux_io_authority": planned["rmux_io"]["execution_enabled"],
        "pre_site_summary_sha256": parent_digest,
        "created_at": created_at,
    }
    created = timestamp(created_at, "channel summary created_at")
    if created < timestamp(plan["created_at"], "plan created_at"):
        raise ValueError("channel summary predates its exact plan")
    return validate_summary(value)
