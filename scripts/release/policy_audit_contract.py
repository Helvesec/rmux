#!/usr/bin/env python3
"""Validate the disarmed policy-audit configuration and its schemas."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
WORKFLOW_PATH = ".github/workflows/release-policy-audit.yml"
GITHUB_ACTIONS_APP_ID = 15368
REQUIRED_CHECK_CONTEXTS = (
    "Cross-target Windows build check",
    "Linux SDK v1 daemon smoke",
    "Linux dependency audit",
    "Linux format, lint, and docs",
    "Linux non-final performance smoke",
    "Linux source boundary gates",
    "Linux tests (app-ui)",
    "Linux tests (core)",
    "Linux tests (pty-sdk)",
    "Linux tests (server)",
    "Linux workspace build",
    "Platform build and smoke on macos-15",
    "Platform build and smoke on macos-15-intel",
    "Platform build and smoke on windows-latest",
    "WASM crypto build and supply-chain gate",
)
API_GETS = (
    ("branch_protection", "/repos/Helvesec/rmux/branches/main/protection"),
    ("immutable_releases", "/repos/Helvesec/rmux/immutable-releases"),
    ("installation", "/installation"),
    (
        "installation_repositories",
        "/installation/repositories?per_page=100&page=1",
    ),
    (
        "policy_workflow",
        "/repos/Helvesec/rmux/actions/workflows/release-policy-audit.yml",
    ),
    ("release_tagging", "/repos/Helvesec/rmux/environments/release-tagging"),
    ("repository", "/repositories/1239918790"),
    (
        "self_hosted_runners",
        "/repos/Helvesec/rmux/actions/runners?per_page=100&page=1",
    ),
    ("tag_creation_ruleset", "/repos/Helvesec/rmux/rulesets/19174415"),
    (
        "tag_immutability_ruleset",
        "/repos/Helvesec/rmux/rulesets/18792083",
    ),
)
CAPABILITIES = {
    "downstream_channels",
    "github_release_publication",
    "policy_audit",
    "promotion_authorization",
    "publication_receipt",
    "signed_tag_creation",
}


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON: {path}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def _positive(value: Any, label: str) -> None:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")


def validate_activation(value: dict[str, Any]) -> None:
    expected_keys = {
        "schema_version",
        "status",
        "description",
        "cutover_pr",
        "runtime_override_allowed",
        "capabilities",
    }
    if set(value) != expected_keys:
        raise ValueError("release activation ledger keys changed")
    if (
        value["schema_version"] != 1
        or value["status"] != "disarmed"
        or value["cutover_pr"] != "PR8"
        or value["runtime_override_allowed"] is not False
        or not isinstance(value["description"], str)
        or not value["description"]
    ):
        raise ValueError("release activation ledger is not fail-closed")
    capabilities = value["capabilities"]
    if not isinstance(capabilities, dict) or set(capabilities) != CAPABILITIES:
        raise ValueError("release activation capabilities changed")
    if any(capabilities[name] is not False for name in CAPABILITIES):
        raise ValueError("every release capability must remain false before PR8")


def validate_contract(value: dict[str, Any], *, require_disarmed: bool) -> None:
    if (
        value.get("schema_version") != 1
        or value.get("status") != "review-only-disarmed"
        or value.get("repository")
        != {
            "id": REPOSITORY_ID,
            "full_name": REPOSITORY,
            "default_branch": "main",
            "visibility": "public",
        }
    ):
        raise ValueError("policy audit contract identity changed")
    if value.get("activation") != {
        "ledger": ".github/release/release-activation.json",
        "capability": "policy_audit",
        "required_value": False,
        "runtime_override_allowed": False,
    }:
        raise ValueError("policy audit activation binding changed")
    workflow = value.get("workflow")
    expected_workflow = {
        "path": WORKFLOW_PATH,
        "only_trigger": "workflow_call",
        "caller_count": 0,
        "required_run_attempt": 1,
        "privileged_job": "policy-audit",
        "privileged_job_condition": "false",
        "environment": "release-policy-audit",
        "runner_image": "ubuntu-22.04",
    }
    if not isinstance(workflow, dict) or any(
        workflow.get(key) != expected for key, expected in expected_workflow.items()
    ):
        raise ValueError("policy audit workflow must remain uncalled and disabled")
    if workflow.get("workflow_id") is not None:
        _positive(workflow["workflow_id"], "policy workflow ID")
    app = value.get("audit_app")
    if not isinstance(app, dict) or app.get("pat_fallback") is not False:
        raise ValueError("policy audit must never fall back to a PAT")
    if app.get("permissions") != {
        "actions": "read",
        "administration": "write",
        "metadata": "read",
    }:
        raise ValueError("policy audit App permissions changed")
    configured = app.get("configured")
    identity = (app.get("app_id"), app.get("installation_id"), app.get("app_slug"))
    if type(configured) is not bool:
        raise ValueError("policy audit App configured state must be boolean")
    if configured:
        _positive(identity[0], "policy audit App ID")
        _positive(identity[1], "policy audit installation ID")
        if not isinstance(identity[2], str) or not re.fullmatch(
            r"[a-z0-9-]+", identity[2]
        ):
            raise ValueError("policy audit App slug is invalid")
    elif identity != (None, None, None):
        raise ValueError("unconfigured policy audit App cannot carry an identity")
    if require_disarmed and configured is not False:
        raise ValueError("repository contract must keep the audit App unconfigured")
    lifecycle = value.get("token_lifecycle")
    if not isinstance(lifecycle, dict) or lifecycle.get("collector_methods") != ["GET"]:
        raise ValueError("policy collector must be GET-only")
    if lifecycle.get("repository_state_mutations") is not False:
        raise ValueError("token lifecycle cannot mutate repository state")
    if lifecycle.get("action_sha") != "fee1f7d63c2ff003460e3d139729b119787bc349":
        raise ValueError("GitHub App token Action pin changed")
    if (
        lifecycle.get("issuance", {}).get("method") != "POST"
        or lifecycle.get("revocation", {}).get("method") != "DELETE"
    ):
        raise ValueError("token issuance and revocation must stay explicitly bounded")
    expected_gets = [{"key": key, "path": path} for key, path in API_GETS]
    if value.get("api_gets") != expected_gets:
        raise ValueError("policy audit GETs differ from the exact ordered allowlist")
    expected_state = value.get("expected_state")
    main = expected_state.get("main") if isinstance(expected_state, dict) else None
    expected_checks = [
        {"context": context, "app_id": GITHUB_ACTIONS_APP_ID}
        for context in REQUIRED_CHECK_CONTEXTS
    ]
    if not isinstance(main, dict) or main.get("required_checks") != expected_checks:
        raise ValueError("required checks lost their exact GitHub Actions binding")
    proof = value.get("proof")
    if not isinstance(proof, dict) or proof.get("freshness_seconds") != 900:
        raise ValueError("policy audit proof must expire after fifteen minutes")
    if proof.get("authorizes_publication") is not False:
        raise ValueError("policy audit proof cannot authorize publication")
    if value.get("external_blockers") != [
        "audit_app_not_installed",
        "release_policy_audit_environment_missing",
        "release_publication_environment_missing",
    ]:
        raise ValueError("policy audit external blockers changed")


def validate_repository_contracts(root: Path) -> None:
    release = root / ".github" / "release"
    activation = read_object(release / "release-activation.json", "activation ledger")
    contract = read_object(release / "policy-audit-contract.json", "audit contract")
    validate_activation(activation)
    validate_contract(contract, require_disarmed=True)
    candidate_workflow = read_object(
        release / "candidate-workflow-contract.json", "candidate workflow contract"
    )
    if candidate_workflow.get("policy_audit") != {
        "activation_ledger": ".github/release/release-activation.json",
        "capability_enabled": False,
        "contract": ".github/release/policy-audit-contract.json",
        "audit_app_configured": False,
        "workflow": WORKFLOW_PATH,
        "workflow_callers": 0,
        "privileged_job_condition": "false",
        "pat_fallback": False,
        "authorizes_publication": False,
    }:
        raise ValueError("candidate workflow policy audit binding changed")
    workflow_path = root / WORKFLOW_PATH
    workflow = workflow_path.read_text(encoding="utf-8")
    if "on:\n  workflow_call:" not in workflow:
        raise ValueError("policy audit must remain workflow_call-only")
    for trigger in (
        "\n  push:",
        "\n  pull_request:",
        "\n  workflow_dispatch:",
        "\n  workflow_run:",
        "\n  repository_dispatch:",
        "\n  schedule:",
    ):
        if trigger in workflow:
            raise ValueError(f"policy audit gained public trigger {trigger.strip()}")
    if workflow.count("if: ${{ false }}") != 1:
        raise ValueError("privileged policy audit job must be literally disabled")
    required_markers = (
        "assert-release-capability.py policy_audit",
        "environment: release-policy-audit",
        "actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349",
        "permission-administration: write",
        "permission-actions: read",
        "permission-metadata: read",
        "RMUX_POLICY_AUDIT_APP_PRIVATE_KEY",
        "--predicate-artifact-id",
        "--predicate-artifact-digest",
    )
    if any(marker not in workflow for marker in required_markers):
        raise ValueError("policy audit workflow lost a disarm or identity binding")
    for authority in (
        "contents: write",
        "id-token: write",
        "packages: write",
        "attestations: write",
        "PAT_TOKEN",
    ):
        if authority in workflow:
            raise ValueError(f"policy audit gained forbidden authority {authority}")
    callers = 0
    for candidate in sorted((root / ".github" / "workflows").glob("*.y*ml")):
        if candidate == workflow_path:
            continue
        callers += candidate.read_text(encoding="utf-8").count(
            "uses: ./.github/workflows/release-policy-audit.yml"
        )
    if callers != 0:
        raise ValueError("policy audit reusable workflow acquired a caller")
    schema_names = (
        "release-activation.schema.json",
        "policy-audit-predicate.schema.json",
        "policy-audit-reference.schema.json",
    )
    for name in schema_names:
        schema = read_object(release / "schemas" / name, name)
        if (
            schema.get("x-rmux-status") != "disarmed-non-authoritative"
            or schema.get("additionalProperties") is not False
            or "MUST NOT authorize publication" not in schema.get("description", "")
        ):
            raise ValueError(f"{name} is not explicitly disarmed")
