#!/usr/bin/env python3
"""Validate the static, disarmed downstream release contract."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from strict_json import read_json_object


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
    "downstream-channel-plan.schema.json",
    "downstream-channel-request.schema.json",
    "downstream-channel-result-envelope.schema.json",
    "downstream-channel-result-predicate.schema.json",
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
        "private",
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
BLOCKED_PAYLOADS = {
    "chocolatey",
    "crates_io",
    "snap_candidate",
    "snap_stable",
    "web_share",
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
        or contract.get("status") != "review-only-disarmed"
        or contract.get("repository")
        != {"id": 1239918790, "full_name": "Helvesec/rmux"}
    ):
        raise ValueError("downstream channel contract identity changed")
    activation = contract.get("activation", {})
    if activation != {
        "ledger": ".github/release/release-activation.json",
        "capability": "downstream_channels",
        "required_value": False,
        "runtime_override_allowed": False,
    }:
        raise ValueError("downstream activation contract must remain false")
    receipt = contract.get("receipt_gate", {})
    if (
        receipt.get("workflow_path") != ".github/workflows/release-receipt.yml"
        or receipt.get("required_run_attempt") != 1
        or receipt.get("download_by_artifact_id") is not True
        or receipt.get("exact_artifact_digest_required") is not True
        or receipt.get("attestation_required") is not True
        or receipt.get("attestation_subject_name") != "release-state.json"
        or receipt.get("attestation_subject_in_bundle") is not True
        or receipt.get("activation_blockers")
        != ["receipt_workflow_id_unset_until_merge"]
        or receipt.get("required_downstream_authority") is not False
    ):
        raise ValueError("downstream receipt gate is not exact and disarmed")
    execution = contract.get("execution", {})
    if (
        execution.get("only_trigger") != "workflow_call"
        or execution.get("public_callers") != 0
        or execution.get("required_caller_repository") != "Helvesec/rmux"
        or execution.get("required_caller_repository_id") != 1239918790
        or execution.get("privileged_job_condition") != "false"
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
        "canonical_provenance_ready": False,
        "actions_expiry_bound": False,
        "producer_workflow_allowlist_ready": False,
        "activation_blockers": [
            "channel_payload_producer_contract_missing",
            "actions_artifact_expiry_binding_missing",
        ],
    }:
        raise ValueError("downstream payload provenance must remain explicitly blocked")
    channels = contract.get("channels")
    if not isinstance(channels, list):
        raise ValueError("downstream channel inventory is missing")
    if (
        tuple(item.get("name") for item in channels if isinstance(item, dict))
        != CHANNELS
    ):
        raise ValueError("downstream channels must be sorted, unique, and exhaustive")
    for entry in channels:
        name = entry["name"]
        for kind in ("rc", "stable"):
            expected = _policy_decision(policy["release_kinds"][kind][name])
            if entry.get(f"{kind}_policy") != expected:
                raise ValueError(f"{name} {kind} policy differs from channel policy")
        if name in BLOCKED_PAYLOADS:
            if entry.get("payload_ready") is not False or not entry.get("blockers"):
                raise ValueError(f"{name} must retain its exact-payload blocker")
    rmux_io = next(item for item in channels if item["name"] == "rmux_io")
    if (
        rmux_io.get("phase") != 3
        or rmux_io.get("target_repository_key") != "rmux.io"
        or "channel_truth_summary_not_available" not in rmux_io.get("blockers", [])
    ):
        raise ValueError("rmux.io must remain the blocked final channel")


def _validate_repositories(release: Path) -> None:
    registry = _read(release / "downstream-repositories.json")
    writer = registry.get("writer_app", {})
    if (
        writer.get("configured") is not False
        or writer.get("app_id") is not None
        or writer.get("installation_id") is not None
        or writer.get("pat_fallback") is not False
    ):
        raise ValueError("downstream writer identity must remain unconfigured")
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
        actual = tuple(
            record.get(field)
            for field in (
                "id",
                "full_name",
                "visibility",
                "default_branch",
                "required_path",
                "ownership",
            )
        )
        if actual != expected or record.get("activation_ready") is not False:
            raise ValueError(f"downstream repository identity drifted: {key}")
    for key in ("rmux-packages", "rmux.io"):
        record = records[key]
        if (
            record.get("protection_api_supported") is not False
            or "private_repository_protection_unavailable_on_current_plan"
            not in record.get("blockers", [])
        ):
            raise ValueError(f"private Free-plan blocker disappeared: {key}")
    web_blockers = set(records["rmux-web-share"].get("blockers", []))
    if (
        not {
            "floating_actions_present",
            "unprotected_secret_deployment_workflow",
        }
        <= web_blockers
    ):
        raise ValueError("Web Share legacy deployment blockers disappeared")


def _validate_schemas(release: Path) -> None:
    schemas = release / "schemas"
    for filename in SCHEMAS:
        schema = _read(schemas / filename)
        if (
            schema.get("x-rmux-status") != "disarmed-non-authoritative"
            or schema.get("additionalProperties") is not False
            or "MUST NOT authorize" not in schema.get("description", "")
        ):
            raise ValueError(f"{filename} is not explicitly disarmed")
        required = schema.get("required")
        properties = schema.get("properties", {})
        if not isinstance(required, list) or not isinstance(properties, dict):
            raise ValueError(f"{filename} has no closed top-level shape")
        for field in ("downstream_authority", "execution_authority"):
            if (
                field not in required
                or properties.get(field, {}).get("const") is not False
            ):
                raise ValueError(f"{filename} lost false {field}")
    request = _read(schemas / "downstream-channel-request.schema.json")
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
    if summary["properties"]["result_count"].get("const") != len(CHANNELS):
        raise ValueError("downstream summary must remain exhaustive")
    if summary["properties"]["rmux_io_last"].get("const") is not True:
        raise ValueError("downstream summary no longer keeps rmux.io last")


def _validate_workflows(root: Path) -> None:
    paths = (
        root / ".github/workflows/release-downstream.yml",
        root / ".github/workflows/release-chocolatey-retry.yml",
        root / ".github/workflows/release-snap-retry.yml",
    )
    for path in paths:
        text = path.read_text(encoding="utf-8")
        if "on:\n  workflow_call:" not in text or "permissions: {}" not in text:
            raise ValueError(f"{path.name} must remain reusable and default-deny")
        for forbidden in (
            "\n  push:",
            "\n  workflow_dispatch:",
            "contents: write",
            "id-token: write",
            "secrets:",
            "environment:",
            "larger-runner",
        ):
            if forbidden in text:
                raise ValueError(f"{path.name} contains forbidden value {forbidden}")
        if "runs-on: self-hosted" in text or "\n      - self-hosted" in text:
            raise ValueError(f"{path.name} gained a self-hosted runner label")
    workflows = root / ".github/workflows"
    for path in workflows.glob("*.y*ml"):
        text = path.read_text(encoding="utf-8")
        for line in text.splitlines():
            normalized = line.strip().removeprefix("- ").strip()
            if not normalized.startswith("uses:"):
                continue
            target = normalized.split(":", 1)[1].strip().split()[0].strip("'\"")
            lowered = target.lower()
            for downstream in paths:
                relative = f"./.github/workflows/{downstream.name}".lower()
                absolute = f"helvesec/rmux/.github/workflows/{downstream.name}@".lower()
                if lowered == relative or lowered.startswith(absolute):
                    raise ValueError(f"{path.name} calls disarmed {downstream.name}")
    main = paths[0].read_text(encoding="utf-8")
    receipt_origin = "--expected-workflow-path .github/workflows/release-receipt.yml"
    promotion_origin = "--expected-workflow-path .github/workflows/release-promote.yml"
    workflow_identity_binding = (
        'test "$RMUX_RECEIPT_RUN_WORKFLOW_ID" = "$RMUX_RECEIPT_WORKFLOW_ID"'
    )
    if (
        main.count(receipt_origin) != 2
        or main.count(promotion_origin) != 0
        or main.count(workflow_identity_binding) != 1
    ):
        raise ValueError("downstream receipt artifacts must come from release-receipt")
    for path in paths:
        workflow = path.read_text(encoding="utf-8")
        if (
            'test "$GITHUB_REPOSITORY" = "Helvesec/rmux"' not in workflow
            or 'test "$GITHUB_REPOSITORY_ID" = "1239918790"' not in workflow
        ):
            raise ValueError(f"{path.name} does not reject external callers")
    if "max-parallel: 3" not in main:
        raise ValueError("downstream Linux fanout must leave one slot for Chocolatey")
    for required in (
        "Disabled pre-site result aggregation",
        "Disabled post-aggregation rmux.io deployment",
        "Disabled final result aggregation",
    ):
        if required not in main:
            raise ValueError(
                "downstream unavailable phase acquired an authoritative name"
            )
    for overstated in ("Require ten exact", "Require eleven exact"):
        if overstated in main:
            raise ValueError(
                "downstream unavailable aggregation overstates its evidence"
            )
    summary = main.find("\n  channel-summary:\n")
    site = main.find("\n  deploy-rmux-io:\n")
    final = main.find("\n  final-channel-summary:\n")
    if not 0 < summary < site < final:
        raise ValueError(
            "disabled rmux.io placeholder must remain between aggregation placeholders"
        )
    if "needs: [prepare-plan, channel-summary]" not in main[site:final]:
        raise ValueError(
            "disabled rmux.io placeholder lost its pre-site aggregation dependency"
        )
    if main.count("verify-receipt-attestation.py") != 1:
        raise ValueError("downstream plan must verify one exact signed receipt")
    for path in paths[1:]:
        retry = path.read_text(encoding="utf-8")
        if (
            retry.count("verify-receipt-attestation.py") != 1
            or retry.count(receipt_origin) != 2
            or retry.count(promotion_origin) != 2
            or retry.count(workflow_identity_binding) != 1
            or retry.count("--deny-self-hosted-runners") != 1
            or retry.count("live payload artifact identity differs") != 1
            or retry.count(".github/workflows/release-downstream.yml") != 1
            or retry.count("prior result is not safe for one exact retry") != 1
            or retry.count("prior result started outside the original request TTL") != 1
            or retry.count('request["retry_depth"] != 0') != 1
            or retry.count('"request_sha256": result["request_sha256"]') != 1
            or retry.count('"mutation_started": result["mutation_started"]') != 1
            or retry.count('"remote_request_id": result["remote_request_id"]') != 1
        ):
            raise ValueError(
                f"{path.name} must verify receipt, result, and payload provenance"
            )
        if f".github/workflows/{path.name}" in retry:
            raise ValueError(f"{path.name} must allow at most one retry")
    receipt_verifier = (
        root / "scripts/release/verify-receipt-attestation.py"
    ).read_text(encoding="utf-8")
    if "--deny-self-hosted-runners" not in receipt_verifier:
        raise ValueError("receipt verifier lost its GitHub-hosted runner gate")
    for path in paths:
        for script in (
            "channel-policy.py",
            "channel-request.py",
            "channel-result.py",
            "channel-summary.py",
            "downstream_channels.py",
            "verify-downstream-repository.py",
        ):
            script_path = root / "scripts/release" / script
            if len(script_path.read_text(encoding="utf-8").splitlines()) >= 600:
                raise ValueError(f"{script} exceeds the release helper size budget")


def validate_downstream_contracts(root: Path) -> None:
    release = root / ".github/release"
    policy = _validate_policy(release)
    _validate_channel_contract(release, policy)
    _validate_repositories(release)
    _validate_schemas(release)
    _validate_workflows(root)
