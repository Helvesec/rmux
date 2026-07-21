#!/usr/bin/env python3
"""Fail-closed validation primitives for downstream result evidence."""

from __future__ import annotations

from urllib.parse import urlsplit
from typing import Any

from downstream_channels import (
    CHANNELS,
    RUNNER_IMAGES,
    SAFE_EXTERNAL_ID,
    SHA40,
    exact_keys,
    load_contract,
    match,
    positive,
    timestamp,
)

RESULT_STATES = {
    "blocked",
    "denied-by-policy",
    "failed-terminal",
    "failed-transient",
    "no-op-exact",
    "pending-moderation",
    "prepared",
    "public-live",
    "submitted",
}
REMOTE_MUTATION_STATES = {"submitted", "pending-moderation", "public-live"}
NO_MUTATION_STATES = {"blocked", "denied-by-policy", "prepared"}
OBSERVED_TARGET_STATES = REMOTE_MUTATION_STATES | {"no-op-exact"}
RETRYABLE_STATES = {"prepared", "failed-transient"}


def result_state(value: Any) -> str:
    if value not in RESULT_STATES:
        raise ValueError("unknown downstream result state")
    return value


def _result_contract() -> dict[str, Any]:
    value = load_contract().get("result_evidence")
    if not isinstance(value, dict):
        raise ValueError("downstream result evidence contract is missing")
    return value


def validate_producer(value: Any, channel: str) -> dict[str, Any]:
    if channel not in CHANNELS or not isinstance(value, dict):
        raise ValueError("channel result producer identity is invalid")
    exact_keys(
        value,
        {
            "run_id",
            "run_attempt",
            "workflow_id",
            "workflow_path",
            "runner_group_id",
            "runner_group_name",
            "runner_image",
        },
        "channel result producer",
    )
    positive(value["run_id"], "result run ID")
    positive(value["workflow_id"], "result workflow ID")
    workflows = _result_contract().get("producer_workflows", {}).get(channel)
    if not isinstance(workflows, list) or value["workflow_path"] not in workflows:
        raise ValueError("result producer workflow is not allowlisted for its channel")
    expected_image = "windows-latest" if channel == "chocolatey" else "ubuntu-22.04"
    if (
        value["run_attempt"] != 1
        or value["runner_group_id"] != 0
        or value["runner_group_name"] != "GitHub Actions"
        or value["runner_image"] not in RUNNER_IMAGES
        or value["runner_image"] != expected_image
    ):
        raise ValueError("result producer is not the expected GitHub-hosted runner")
    return value


def validate_mutation_state(
    state: str, mutation_started: Any, remote_request_id: Any
) -> None:
    result_state(state)
    if type(mutation_started) is not bool:
        raise ValueError("mutation_started must be boolean")
    if remote_request_id is not None:
        match(remote_request_id, SAFE_EXTERNAL_ID, "remote request ID")
    if state in NO_MUTATION_STATES and (mutation_started or remote_request_id is not None):
        raise ValueError(f"{state} cannot claim a remote mutation")
    if state in REMOTE_MUTATION_STATES and (
        not mutation_started or remote_request_id is None
    ):
        raise ValueError(f"{state} requires an identified remote mutation")
    if state == "no-op-exact" and (mutation_started or remote_request_id is None):
        raise ValueError("no-op-exact requires an existing remote identity")
    if state in {"failed-terminal", "failed-transient"}:
        if mutation_started is (remote_request_id is None):
            raise ValueError("failed result mutation and remote identity disagree")


def validate_retryable_previous(value: dict[str, Any]) -> None:
    state = result_state(value["state"])
    validate_mutation_state(
        state, value["mutation_started"], value["remote_request_id"]
    )
    if (
        state not in RETRYABLE_STATES
        or value["mutation_started"] is not False
        or value["remote_request_id"] is not None
    ):
        raise ValueError("previous result is not safely retryable without mutation")


def validate_remote_identity(target_evidence: dict[str, Any], remote_request_id: Any) -> None:
    if target_evidence.get("external_id") != remote_request_id:
        raise ValueError("target evidence and remote mutation identity differ")


def validate_target_evidence(
    value: Any,
    *,
    channel: str,
    state: str,
    expected_target: dict[str, Any],
    expected_version: str,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("target evidence must be an object")
    exact_keys(
        value,
        {
            "schema_version",
            "channel",
            "target_kind",
            "repository_id",
            "external_id",
            "url",
            "version",
            "commit_sha",
            "public_live",
            "observed_at",
        },
        "target evidence",
    )
    state = result_state(state)
    if (
        value["schema_version"] != 1
        or value["channel"] != channel
        or value["target_kind"] != expected_target["target_kind"]
        or value["repository_id"] != expected_target["repository_id"]
        or value["version"] != expected_version
    ):
        raise ValueError("target evidence differs from its exact request")
    if value["external_id"] is not None:
        match(value["external_id"], SAFE_EXTERNAL_ID, "target external ID")
    if value["commit_sha"] is not None:
        match(value["commit_sha"], SHA40, "target commit SHA")
    policy = _result_contract().get("target_policies", {}).get(channel)
    if not isinstance(policy, dict):
        raise ValueError("target evidence policy is missing for channel")
    try:
        url = urlsplit(value["url"])
        port = url.port
    except (TypeError, ValueError) as error:
        raise ValueError("target evidence URL is invalid") from error
    allowed_hosts = policy.get("allowed_hosts")
    if (
        url.scheme != "https"
        or url.hostname not in allowed_hosts
        or url.username is not None
        or url.password is not None
        or port is not None
        or url.query
        or url.fragment
    ):
        raise ValueError("target evidence URL differs from the channel allowlist")
    full_name = expected_target["repository_full_name"]
    if url.hostname == "github.com" and (
        not isinstance(full_name, str) or not url.path.startswith(f"/{full_name}/")
    ):
        raise ValueError("GitHub target URL differs from the pinned repository")
    if (
        state in OBSERVED_TARGET_STATES
        and policy.get("commit_sha_required") is True
        and value["commit_sha"] is None
    ):
        raise ValueError("observed repository target requires an exact commit SHA")
    if value["public_live"] is not (state in {"no-op-exact", "public-live"}):
        raise ValueError("channel state and public-live evidence disagree")
    timestamp(value["observed_at"], "target observed_at")
    return value
