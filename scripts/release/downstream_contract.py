#!/usr/bin/env python3
"""Validate the static downstream release contract."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from strict_json import read_json_object
from downstream_workflow_contract import validate_downstream_workflows


CHANNELS = (
    "apt_rpm",
    "chocolatey",
    "crates_io",
    "homebrew_core",
    "homebrew_tap",
    "rmux_io",
    "scoop",
    "snap_candidate",
    "snap_stable",
    "web_share",
    "winget",
)
SCHEMAS = (
    "downstream-channel-payload.schema.json",
    "downstream-channel-plan.schema.json",
    "downstream-channel-request.schema.json",
    "downstream-channel-result-envelope.schema.json",
    "downstream-channel-result-predicate.schema.json",
    "downstream-channel-result-reference.schema.json",
    "downstream-channel-summary.schema.json",
)
REPOSITORIES = {
    "homebrew-core": (
        52855516,
        "Homebrew/homebrew-core",
        "public",
        "main",
        "Formula/r/rmux.rb",
        "external",
    ),
    "homebrew-rmux": (
        1259133629,
        "Helvesec/homebrew-rmux",
        "public",
        "main",
        "Formula/rmux.rb",
        "rmux-owned",
    ),
    "rmux-packages": (
        1258602064,
        "Helvesec/rmux-packages",
        "public",
        "main",
        ".",
        "rmux-owned",
    ),
    "rmux-web-share": (
        1249553407,
        "Helvesec/rmux-web-share",
        "public",
        "main",
        "src/scripts/share/wasm",
        "rmux-owned",
    ),
    "rmux.io": (
        1240176583,
        "Helvesec/rmux.io",
        "private",
        "main",
        "public",
        "rmux-owned",
    ),
    "scoop-rmux": (
        1259135161,
        "Helvesec/scoop-rmux",
        "public",
        "main",
        "bucket/rmux.json",
        "rmux-owned",
    ),
    "winget-pkgs": (
        197275551,
        "microsoft/winget-pkgs",
        "public",
        "master",
        "manifests/h/Helvesec/RMUX",
        "external",
    ),
}
SEALED_CANONICAL_CHANNEL_ROLES = {
    "chocolatey": {"chocolatey-package"},
    "crates_io": {"crate-package-set"},
    "snap_candidate": {"snap-amd64", "snap-arm64"},
    "web_share": {"wasm-byte-set", "wasm-provenance"},
}
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


def _read(path: Path) -> dict[str, Any]:
    return read_json_object(path, str(path))


def _policy_decision(value: Any) -> str:
    if value is True:
        return "allow"
    if value is False:
        return "deny"
    if value == "explicit_opt_in":
        return "explicit-opt-in"
    raise ValueError(f"unsupported downstream policy value: {value!r}")


def _validate_policy(release: Path) -> dict[str, Any]:
    policy = _read(release / "channel-policy.json")
    if policy.get("schema_version") != 1 or policy.get("default_decision") != "deny":
        raise ValueError("downstream channel policy must remain schema-v1 default-deny")
    kinds = policy.get("release_kinds")
    if not isinstance(kinds, dict) or set(kinds) != {"shadow", "rc", "stable"}:
        raise ValueError("downstream policy release kinds changed")
    expected_policy_keys = set(CHANNELS) | {"github_public_release"}
    for kind, values in kinds.items():
        if not isinstance(values, dict) or set(values) != expected_policy_keys:
            raise ValueError(f"{kind} downstream channel inventory changed")
    if any(value is not False for value in kinds["shadow"].values()):
        raise ValueError("shadow releases must deny every downstream channel")
    for kind in ("rc", "stable"):
        if kinds[kind]["snap_candidate"] != "explicit_opt_in":
            raise ValueError(f"{kind} Snap candidate must require explicit opt-in")
        if kinds[kind]["snap_stable"] is not False:
            raise ValueError(f"{kind} Snap stable must remain denied")
    return policy


def _validate_channel_contract(release: Path, policy: dict[str, Any]) -> None:
    contract = _read(release / "downstream-channel-contract.json")
    if (
        contract.get("schema_version") != 1
        or contract.get("status") != "atomic-authority-bound"
        or contract.get("repository")
        != {"id": 1239918790, "full_name": "Helvesec/rmux"}
    ):
        raise ValueError("downstream channel contract identity changed")
    activation = contract.get("activation", {})
    if activation != {
        "ledger": ".github/release/release-activation.json",
        "capability": "downstream_channels",
        "required_value": "matches-atomic-ledger",
        "runtime_override_allowed": False,
    }:
        raise ValueError("downstream activation contract lost its atomic binding")
    receipt = contract.get("receipt_gate", {})
    if (
        receipt.get("workflow_path") != ".github/workflows/release-receipt.yml"
        or receipt.get("workflow_id") != 316435347
        or receipt.get("required_run_attempt") != 1
        or receipt.get("download_by_artifact_id") is not True
        or receipt.get("exact_artifact_digest_required") is not True
        or receipt.get("attestation_required") is not True
        or receipt.get("attestation_subject_name") != "release-state.json"
        or receipt.get("attestation_subject_in_bundle") is not True
        or receipt.get("activation_blockers") != []
        or receipt.get("required_downstream_authority") != "matches-atomic-ledger"
    ):
        raise ValueError("downstream receipt gate is not exact and authority-bound")
    execution = contract.get("execution", {})
    if (
        execution.get("only_trigger") != "workflow_call"
        or execution.get("public_callers") != 0
        or execution.get("required_caller_repository") != "Helvesec/rmux"
        or execution.get("required_caller_repository_id") != 1239918790
        or execution.get("privileged_job_condition") != "release-activation-ledger"
        or execution.get("maximum_parallel_channels") != 4
        or execution.get("github_hosted_only") is not True
        or execution.get("native_rebuild_allowed") is not False
        or execution.get("rmux_io_last") is not True
    ):
        raise ValueError("downstream execution contract is not fail-closed")
    retry = contract.get("retry", {})
    for key in (
        "same_receipt_required",
        "same_request_required",
        "same_idempotency_key_required",
        "same_payload_bytes_required",
    ):
        if retry.get(key) is not True:
            raise ValueError(f"downstream retry lost {key}")
    if (
        retry.get("actions_run_attempt") != 1
        or retry.get("actions_rerun_allowed") is not False
        or retry.get("native_rebuild_allowed") is not False
    ):
        raise ValueError("downstream retries must be fresh and build-free")
    payload = contract.get("payload_evidence", {})
    if payload != {
        "schema": ".github/release/schemas/downstream-channel-payload.schema.json",
        "predicate_type": "https://rmux.io/attestations/release-downstream-channel-payload/v1",
        "subject_name": "downstream-channel-payload-subject.json",
        "canonical_provenance_ready": True,
        "actions_expiry_bound": True,
        "producer_workflow_allowlist_ready": True,
        "producer": {
            "workflow_id": 316435347,
            "workflow_path": ".github/workflows/release-receipt.yml",
            "job_workflow_paths": {
                "default": ".github/workflows/release-downstream-prepare.yml",
                "rmux_io": ".github/workflows/release-rmux-io-payload.yml",
            },
            "required_run_attempt": 1,
        },
        "activation_blockers": [],
    }:
        raise ValueError("downstream payload provenance is not exact")
    result = contract.get("result_evidence", {})
    if (
        result.get("result_reference_ready") is not True
        or result.get("result_reference_schema")
        != ".github/release/schemas/downstream-channel-result-reference.schema.json"
        or result.get("attestation_verification_ready") is not True
        or result.get("attestation_verifier")
        != "scripts/release/verify-channel-result-attestation.py"
        or result.get("result_aggregation_ready") is not True
        or result.get("aggregation_blockers") != []
        or result.get("summary_schema")
        != ".github/release/schemas/downstream-channel-summary.schema.json"
        or result.get("summary_phases") != ["pre-site", "final"]
        or result.get("pre_site_result_count") != len(CHANNELS) - 1
        or result.get("final_result_count") != len(CHANNELS)
        or result.get("rmux_io_pre_site_digest_field") != "pre_site_summary_sha256"
    ):
        raise ValueError("downstream result evidence readiness changed")
    channels = contract.get("channels")
    if not isinstance(channels, list):
        raise ValueError("downstream channel inventory is missing")
    if (
        tuple(item.get("name") for item in channels if isinstance(item, dict))
        != CHANNELS
    ):
        raise ValueError("downstream channels must be sorted, unique, and exhaustive")
    canonical = _read(release / "canonical-build-contract.json")
    canonical_roles = {
        role
        for platform in canonical.get("platforms", [])
        if isinstance(platform, dict)
        for role in platform.get("supplemental_roles", [])
        if isinstance(role, str)
    }
    for entry in channels:
        name = entry["name"]
        for kind in ("rc", "stable"):
            expected = _policy_decision(policy["release_kinds"][kind][name])
            if entry.get(f"{kind}_policy") != expected:
                raise ValueError(f"{name} {kind} policy differs from channel policy")
        if name in SEALED_CANONICAL_CHANNEL_ROLES:
            expected_roles = SEALED_CANONICAL_CHANNEL_ROLES[name]
            if (
                entry.get("payload_ready") is not True
                or set(entry.get("payload_roles", [])) != expected_roles
                or not expected_roles <= canonical_roles
            ):
                raise ValueError(f"{name} must consume its sealed canonical payload")
    snap_stable = next(item for item in channels if item["name"] == "snap_stable")
    if (
        snap_stable.get("payload_ready") is not True
        or snap_stable.get("payload_roles") != ["policy-decision"]
        or snap_stable.get("blockers") != ["denied_until_support_decision"]
    ):
        raise ValueError("Snap stable must retain explicit denial evidence")
    rmux_io = next(item for item in channels if item["name"] == "rmux_io")
    if (
        rmux_io.get("phase") != 3
        or rmux_io.get("target_repository_key") != "rmux.io"
        or rmux_io.get("blockers")
        != [
            "private_repository_protection_unavailable_on_current_plan",
            "manual_site_update_required",
        ]
    ):
        raise ValueError("rmux.io must remain the blocked final channel")
    by_name = {item["name"]: item for item in channels}
    for name in ("apt_rpm", "homebrew_tap", "scoop", "web_share"):
        if by_name[name].get("blockers") != []:
            raise ValueError(f"configured channel still blocked: {name}")


def _validate_repositories(release: Path) -> None:
    registry = _read(release / "downstream-repositories.json")
    if registry.get("writer_app", {}) != {
        "configured": True,
        "app_id": 4352876,
        "installation_id": 147959477,
        "repository_selection": "selected",
        "pat_fallback": False,
        "required_permissions": {
            "actions": "read",
            "contents": "write",
            "metadata": "read",
        },
    }:
        raise ValueError("downstream writer App identity or permissions changed")
    protection = registry.get("required_protection", {})
    if (
        protection.get("enforce_admins") is not True
        or protection.get("allow_force_pushes") is not False
        or protection.get("allow_deletions") is not False
        or protection.get("required_signatures") is not True
        or protection.get("self_hosted_runner_count") != 0
        or protection.get("environment_admin_bypass") is not False
        or protection.get("second_person_review_required") is not False
    ):
        raise ValueError("downstream repository protection requirements changed")
    values = registry.get("repositories")
    if not isinstance(values, list):
        raise ValueError("downstream repository inventory is missing")
    records = {item.get("key"): item for item in values if isinstance(item, dict)}
    if set(records) != set(REPOSITORIES) or len(values) != len(records):
        raise ValueError("downstream repository inventory changed")
    for key, expected in REPOSITORIES.items():
        record = records[key]
        actual = (
            record.get("id"),
            record.get("full_name"),
            record.get("visibility"),
            record.get("default_branch"),
            record.get("required_path"),
            record.get("ownership"),
        )
        expected_ready = key in {
            "homebrew-rmux",
            "rmux-packages",
            "rmux-web-share",
            "scoop-rmux",
        }
        if actual != expected or record.get("activation_ready") is not expected_ready:
            raise ValueError(f"downstream repository identity drifted: {key}")
    rmux_io = records["rmux.io"]
    if (
        rmux_io.get("protection_api_supported") is not False
        or "private_repository_protection_unavailable_on_current_plan"
        not in rmux_io.get("blockers", [])
        or "manual_site_update_required" not in rmux_io.get("blockers", [])
        or "downstream_writer_app_missing" in rmux_io.get("blockers", [])
    ):
        raise ValueError("private rmux.io manual-update contract drifted")
    for key in (
        "homebrew-rmux",
        "rmux-packages",
        "rmux-web-share",
        "scoop-rmux",
    ):
        record = records[key]
        blockers = set(record.get("blockers", []))
        if (
            record.get("protection_api_supported") is not True
            or record.get("branch_protected") is not True
            or record.get("ruleset_count") != 1
            or record.get("environment_count") != 1
            or "repository_protection_missing" in blockers
            or "environment_admin_bypass_enabled" in blockers
            or "downstream_writer_app_missing" in blockers
        ):
            raise ValueError(f"public downstream protection snapshot drifted: {key}")
    for key in ("homebrew-rmux", "rmux-packages", "rmux-web-share", "scoop-rmux"):
        if records[key].get("blockers") != []:
            raise ValueError(f"ready downstream repository still has blockers: {key}")


def _validate_schemas(release: Path) -> None:
    schemas = release / "schemas"
    for filename in SCHEMAS:
        schema = _read(schemas / filename)
        if (
            schema.get("x-rmux-status") != "atomic-authority-bound"
            or schema.get("additionalProperties") is not False
            or "authority" not in schema.get("description", "").lower()
        ):
            raise ValueError(f"{filename} is not atomically authority-bound")
        required = schema.get("required")
        properties = schema.get("properties", {})
        if not isinstance(required, list) or not isinstance(properties, dict):
            raise ValueError(f"{filename} has no closed top-level shape")
        for field in ("downstream_authority", "execution_authority"):
            if (
                field not in required
                or properties.get(field, {}).get("type") != "boolean"
            ):
                raise ValueError(f"{filename} lost boolean {field}")
        authority_rules = json.dumps(schema.get("oneOf", []), sort_keys=True)
        for expected in (
            "disarmed-non-authoritative",
            "downstream-authorized",
            '"const": false',
            '"const": true',
        ):
            if expected not in authority_rules:
                raise ValueError(f"{filename} lost atomic authority rule {expected}")
    request = _read(schemas / "downstream-channel-request.schema.json")
    if "pre_site_summary_sha256" not in request[
        "required"
    ] or "pre_site_summary_sha256" not in json.dumps(
        request.get("allOf", []), sort_keys=True
    ):
        raise ValueError("rmux.io request lost its pre-site summary binding")
    if request["$defs"]["payload"].get("$ref") != (
        "https://rmux.io/schemas/downstream-channel-payload-v1.json"
    ):
        raise ValueError("downstream request lost its exact payload schema")
    payload = _read(schemas / "downstream-channel-payload.schema.json")
    producer = payload["$defs"]["producer"]["properties"]
    if (
        payload["properties"]["predicate_type"].get("const")
        != "https://rmux.io/attestations/release-downstream-channel-payload/v1"
        or producer["workflow_id"].get("const") != 316435347
        or producer["workflow_path"].get("const")
        != ".github/workflows/release-receipt.yml"
        or producer["job_workflow_path"].get("const")
        != ".github/workflows/release-downstream-prepare.yml"
        or producer["runner_group_id"].get("const") != 0
        or payload["properties"]["retention_expires_at"].get("format") != "date-time"
    ):
        raise ValueError("downstream payload schema lost provenance or retention")
    previous = request["properties"]["previous_result"]
    encoded = json.dumps(previous, sort_keys=True)
    for field in (
        "predicate_artifact_id",
        "predicate_artifact_digest",
        "envelope_artifact_id",
        "envelope_artifact_digest",
        "predicate_sha256",
        "envelope_sha256",
    ):
        if field not in encoded:
            raise ValueError(f"downstream retry schema lost {field}")
    result = _read(schemas / "downstream-channel-result-predicate.schema.json")
    if set(result.get("$defs", {}).get("state", {}).get("enum", [])) != RESULT_STATES:
        raise ValueError("downstream result states changed")
    producer = result["$defs"]["producer"]["properties"]
    if (
        producer["run_attempt"].get("const") != 1
        or producer["runner_group_id"].get("const") != 0
        or producer["runner_group_name"].get("const") != "GitHub Actions"
        or "self-hosted" in producer["runner_image"].get("enum", [])
    ):
        raise ValueError("downstream result producer is not GitHub-hosted attempt 1")
    summary = _read(schemas / "downstream-channel-summary.schema.json")
    if (
        summary["properties"]["result_aggregation_ready"].get("const") is not True
        or summary["properties"]["rmux_io_two_phase_ready"].get("const") is not True
        or summary["properties"]["aggregation_blockers"].get("maxItems") != 0
    ):
        raise ValueError("downstream two-phase summary contract is incomplete")
    if summary["properties"]["rmux_io_last"].get("const") is not True:
        raise ValueError("downstream summary no longer keeps rmux.io last")


def validate_downstream_contracts(root: Path) -> None:
    release = root / ".github/release"
    policy = _validate_policy(release)
    _validate_channel_contract(release, policy)
    _validate_repositories(release)
    _validate_schemas(release)
    validate_downstream_workflows(root)
