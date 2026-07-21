#!/usr/bin/env python3
"""Deterministic validation for disarmed downstream channel plans."""

from __future__ import annotations

from typing import Any

from downstream_channels import (
    CHANNELS,
    CHANNEL_POLICY,
    REPOSITORY_ID,
    SHA40,
    STATUS,
    exact_keys,
    file_hash,
    load_contract,
    match,
    read_object,
    timestamp,
    validate_embedded_receipt,
    validate_release,
)


def _policy() -> dict[str, Any]:
    value = read_object(CHANNEL_POLICY, "downstream channel policy")
    exact_keys(
        value,
        {
            "schema_version",
            "default_decision",
            "release_kinds",
            "downstream_gate",
            "snap_stable_publication",
        },
        "downstream channel policy",
    )
    if value["schema_version"] != 1 or value["default_decision"] != "deny":
        raise ValueError("downstream channel policy must remain default-deny")
    kinds = value["release_kinds"]
    if not isinstance(kinds, dict) or set(kinds) != {"shadow", "rc", "stable"}:
        raise ValueError("downstream channel policy release kinds changed")
    expected = set(CHANNELS) | {"github_public_release"}
    for kind, entries in kinds.items():
        if not isinstance(entries, dict) or set(entries) != expected:
            raise ValueError(f"downstream {kind} channel policy is not exhaustive")
    return value


def expected_channel_entries(
    release_kind: str, snap_candidate_opt_in: bool
) -> list[dict[str, Any]]:
    if release_kind not in {"rc", "stable"}:
        raise ValueError("downstream plan release kind must be rc or stable")
    if type(snap_candidate_opt_in) is not bool:
        raise ValueError("Snap candidate opt-in must be boolean")
    policy_entries = _policy()["release_kinds"][release_kind]
    contract_entries = {entry["name"]: entry for entry in load_contract()["channels"]}
    result: list[dict[str, Any]] = []
    for channel in CHANNELS:
        contracted = contract_entries[channel]
        raw = policy_entries[channel]
        if raw is True:
            decision = "allow"
            opted_in = False
            allowed = True
        elif raw is False:
            decision = "deny"
            opted_in = False
            allowed = False
        elif raw == "explicit_opt_in" and channel == "snap_candidate":
            decision = "explicit-opt-in"
            opted_in = snap_candidate_opt_in
            allowed = opted_in
        else:
            raise ValueError(f"downstream policy value is not fail-closed: {channel}")
        if contracted[f"{release_kind}_policy"] != decision:
            raise ValueError(f"channel contract and policy disagree for {channel}")
        blockers = contracted["blockers"]
        if not isinstance(blockers, list) or not all(
            isinstance(item, str) and item for item in blockers
        ):
            raise ValueError(f"channel blockers are not explicit for {channel}")
        if not allowed:
            execution_decision = "denied"
        elif contracted["payload_ready"] is not True or blockers:
            execution_decision = "blocked"
        else:
            execution_decision = "disarmed"
        result.append(
            {
                "name": channel,
                "phase": contracted["phase"],
                "policy_decision": decision,
                "explicit_opt_in": opted_in,
                "execution_decision": execution_decision,
                "execution_enabled": False,
                "payload_ready": contracted["payload_ready"],
                "payload_roles": contracted["payload_roles"],
                "depends_on": (
                    [name for name in CHANNELS if name != "rmux_io"]
                    if channel == "rmux_io"
                    else []
                ),
                "blockers": blockers,
            }
        )
    return result


def validate_plan(value: dict[str, Any]) -> dict[str, Any]:
    exact_keys(
        value,
        {
            "schema_version",
            "status",
            "downstream_authority",
            "execution_authority",
            "execution_enabled",
            "repository_id",
            "source_git_sha",
            "release",
            "receipt",
            "channel_policy",
            "snap_candidate_opt_in",
            "created_at",
            "channel_count",
            "channels",
        },
        "downstream plan",
    )
    if (
        value["schema_version"] != 1
        or value["status"] != STATUS
        or value["downstream_authority"] is not False
        or value["execution_authority"] is not False
        or value["execution_enabled"] is not False
        or value["repository_id"] != REPOSITORY_ID
        or value["channel_count"] != len(CHANNELS)
    ):
        raise ValueError("downstream plan must remain exactly disarmed")
    source_sha = match(value["source_git_sha"], SHA40, "plan source SHA")
    release = validate_release(value["release"], source_sha)
    receipt = validate_embedded_receipt(value["receipt"], source_sha, release)
    channel_policy = value["channel_policy"]
    if not isinstance(channel_policy, dict):
        raise ValueError("plan channel policy must be an object")
    exact_keys(
        channel_policy, {"path", "schema_version", "sha256"}, "channel policy"
    )
    if channel_policy != {
        "path": ".github/release/channel-policy.json",
        "schema_version": 1,
        "sha256": file_hash(CHANNEL_POLICY),
    }:
        raise ValueError("plan does not bind the exact channel policy")
    created = timestamp(value["created_at"], "plan created_at")
    if created < timestamp(receipt["verified_at"], "receipt verified_at"):
        raise ValueError("downstream plan predates its receipt")
    expected = expected_channel_entries(
        release["kind"], value["snap_candidate_opt_in"]
    )
    if value["channels"] != expected:
        raise ValueError("downstream plan entries differ from policy and contract")
    return value
