#!/usr/bin/env python3
"""Validate the disarmed policy-audit configuration and its schemas."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
PREPARER_WORKFLOW_PATH = ".github/workflows/release-policy-audit.yml"
PRIVILEGED_ACTION_PATH = ".github/actions/release-policy-audit/action.yml"
AUDIT_WORKFLOWS = (
    {"id": 316435346, "path": ".github/workflows/release-promote.yml"},
    {
        "id": 316591947,
        "path": ".github/workflows/release-promotion-simulation.yml",
    },
)
AUDIT_WORKFLOW_STATE_KEYS = {
    ".github/workflows/release-promote.yml": "policy_promoter_workflow",
    ".github/workflows/release-promotion-simulation.yml": "policy_simulation_workflow",
}
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
    ("audit_app", "/apps/helvesec-rmux-policy-audit"),
    ("branch_protection", "/repos/Helvesec/rmux/branches/main/protection"),
    ("immutable_releases", "/repos/Helvesec/rmux/immutable-releases"),
    (
        "installation_repositories",
        "/installation/repositories?per_page=100&page=1",
    ),
    (
        "policy_preparer_workflow",
        "/repos/Helvesec/rmux/actions/workflows/release-policy-audit.yml",
    ),
    (
        "policy_promoter_workflow",
        "/repos/Helvesec/rmux/actions/workflows/release-promote.yml",
    ),
    (
        "policy_simulation_workflow",
        "/repos/Helvesec/rmux/actions/workflows/release-promotion-simulation.yml",
    ),
    ("release_environment", "/repos/Helvesec/rmux/environments/release"),
    (
        "release_policy_audit",
        "/repos/Helvesec/rmux/environments/release-policy-audit",
    ),
    (
        "release_publication",
        "/repos/Helvesec/rmux/environments/release-publication",
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
        "path": PREPARER_WORKFLOW_PATH,
        "only_trigger": "workflow_call",
        "caller_count": 2,
        "required_run_attempt": 1,
        "preparer_job": "prepare-policy-audit",
        "privileged_action": PRIVILEGED_ACTION_PATH,
        "privileged_job": "policy-audit",
        "privileged_job_condition": "disabled-in-promoter",
        "environment": "release-policy-audit",
        "runner_image": "ubuntu-22.04",
        "workflow_id": 316425885,
        "audit_workflows": list(AUDIT_WORKFLOWS),
    }
    if workflow != expected_workflow:
        raise ValueError("policy audit workflow call boundary changed")
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
    if require_disarmed and configured is not True:
        raise ValueError("repository contract must bind the installed audit App")
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
    expected_workflow_states = {
        "policy_preparer_workflow": {
            "id": 316425885,
            "path": PREPARER_WORKFLOW_PATH,
            "state": "active",
        },
        "policy_promoter_workflow": {
            "id": 316435346,
            "path": ".github/workflows/release-promote.yml",
            "state": "active",
        },
        "policy_simulation_workflow": {
            "id": 316591947,
            "path": ".github/workflows/release-promotion-simulation.yml",
            "state": "active",
        },
    }
    if any(
        expected_state.get(key) != expected
        for key, expected in expected_workflow_states.items()
    ):
        raise ValueError("policy audit workflow identities changed")
    proof = value.get("proof")
    if not isinstance(proof, dict) or proof.get("freshness_seconds") != 900:
        raise ValueError("policy audit proof must expire after fifteen minutes")
    if proof.get("authorizes_publication") is not False:
        raise ValueError("policy audit proof cannot authorize publication")
    if value.get("external_blockers") != []:
        raise ValueError("policy audit external blockers changed")


def _job_block(workflow: str, name: str, next_name: str) -> str:
    marker = f"\n  {name}:\n"
    next_marker = f"\n  {next_name}:\n"
    if workflow.count(marker) != 1 or workflow.count(next_marker) != 1:
        raise ValueError(f"workflow job boundary changed for {name}")
    return workflow.split(marker, 1)[1].split(next_marker, 1)[0]


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
        "audit_app_configured": True,
        "preparer_workflow": PREPARER_WORKFLOW_PATH,
        "preparer_callers": 2,
        "privileged_action": PRIVILEGED_ACTION_PATH,
        "privileged_job_condition": "disabled-in-promoter",
        "pat_fallback": False,
        "authorizes_publication": False,
    }:
        raise ValueError("candidate workflow policy audit binding changed")
    workflow_path = root / PREPARER_WORKFLOW_PATH
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
    required_markers = (
        "assert-release-capability.py policy_audit",
        "scripts/release/policy-root.py",
        "--expected-workflow-id 316223904",
        "Upload the unprivileged exact policy bundle",
    )
    if any(marker not in workflow for marker in required_markers):
        raise ValueError("policy audit preparer lost a disarm or input binding")
    for authority in (
        "environment:",
        "RMUX_POLICY_AUDIT_APP_PRIVATE_KEY",
        "actions/create-github-app-token@",
        "contents: write",
        "id-token: write",
        "packages: write",
        "attestations: write",
        "PAT_TOKEN",
    ):
        if authority in workflow:
            raise ValueError(f"policy audit preparer gained forbidden authority {authority}")
    caller_paths: list[str] = []
    for candidate in sorted((root / ".github" / "workflows").glob("*.y*ml")):
        if candidate == workflow_path:
            continue
        count = candidate.read_text(encoding="utf-8").count(
            "uses: ./.github/workflows/release-policy-audit.yml"
        )
        caller_paths.extend([candidate.name] * count)
    if caller_paths != ["release-promote.yml", "release-promotion-simulation.yml"]:
        raise ValueError("policy audit callers differ from the exact allowlist")
    action_path = root / PRIVILEGED_ACTION_PATH
    action = action_path.read_text(encoding="utf-8")
    action_markers = (
        "actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349",
        "private-key: ${{ inputs.audit-app-private-key }}",
        "permission-actions: read",
        "permission-administration: write",
        "permission-metadata: read",
        "--audit-workflow-id",
        "--audit-workflow-path",
        "scripts/release/policy-audit.py collect",
        "--predicate-artifact-id",
        "--predicate-artifact-digest",
    )
    if any(marker not in action for marker in action_markers):
        raise ValueError("privileged policy audit action lost an identity binding")
    for authority in (
        "contents: write",
        "id-token: write",
        "packages: write",
        "attestations: write",
        "PAT_TOKEN",
    ):
        if authority in action:
            raise ValueError(f"privileged policy audit action gained {authority}")
    promote = (root / ".github/workflows/release-promote.yml").read_text(
        encoding="utf-8"
    )
    simulation = (root / ".github/workflows/release-promotion-simulation.yml").read_text(
        encoding="utf-8"
    )
    promote_prepare = _job_block(promote, "prepare-policy-audit", "policy-audit")
    promote_audit = _job_block(promote, "policy-audit", "authorize-promotion")
    simulation_prepare = _job_block(simulation, "prepare-policy-audit", "policy-audit")
    simulation_audit = _job_block(simulation, "policy-audit", "verify-simulation")
    caller_contracts = (
        (
            "release-promote.yml",
            promote_prepare,
            promote_audit,
            316435346,
            ".github/workflows/release-promote.yml",
        ),
        (
            "release-promotion-simulation.yml",
            simulation_prepare,
            simulation_audit,
            316591947,
            ".github/workflows/release-promotion-simulation.yml",
        ),
    )
    for caller_name, prepare, audit, workflow_id, caller_path in caller_contracts:
        if prepare.count("uses: ./.github/workflows/release-policy-audit.yml") != 1:
            raise ValueError(f"policy preparer call changed in {caller_name}")
        required_audit = (
            "environment: release-policy-audit",
            "runs-on: ubuntu-22.04",
            "uses: ./.github/actions/release-policy-audit",
            "audit-app-id: ${{ vars.RMUX_POLICY_AUDIT_APP_ID }}",
            "audit-app-private-key: ${{ secrets.RMUX_POLICY_AUDIT_APP_PRIVATE_KEY }}",
            f"audit-workflow-id: {workflow_id}",
            f"audit-workflow-path: {caller_path}",
        )
        if any(marker not in audit for marker in required_audit):
            raise ValueError(f"privileged audit boundary changed in {caller_name}")
        if "secrets: inherit" in prepare or "secrets: inherit" in audit:
            raise ValueError(f"policy audit secret inheritance appeared in {caller_name}")
    if "if: ${{ false }}" not in promote_prepare or "if: ${{ false }}" not in promote_audit:
        raise ValueError("release promoter policy audit must remain disabled")
    if (
        "on:\n  workflow_dispatch:" not in simulation
        or "\n  workflow_call:" in simulation
        or "simulation: true" not in simulation_prepare
        or "permissions: {}" not in simulation
    ):
        raise ValueError("release promotion simulation entrypoint changed")
    for authority in (
        "contents: write",
        "id-token: write",
        "packages: write",
        "attestations: write",
    ):
        if authority in simulation:
            raise ValueError(f"release simulation gained forbidden authority {authority}")
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
