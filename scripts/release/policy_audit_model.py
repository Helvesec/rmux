#!/usr/bin/env python3
"""Typed, fail-closed model for the disarmed live release-policy audit."""

from __future__ import annotations

import hashlib
import json
import re
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from policy_audit_contract import API_GETS, AUDIT_WORKFLOW_STATE_KEYS
from policy_audit_live_state import normalize_environment, normalize_workflow

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
PREDICATE_TYPE = "https://rmux.io/attestations/release-policy-audit/v1"
SHA40 = re.compile(r"[0-9a-f]{40}")
SHA64 = re.compile(r"[0-9a-f]{64}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
INTENT = re.compile(r"[A-Za-z0-9._:-]{8,128}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON: {path}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def write_object(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def canonical_hash(value: dict[str, Any]) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ValueError(
            f"{label} keys differ: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


def positive_integer(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def canonical_timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be a canonical UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"{label} is not a valid timestamp") from error
    if parsed.tzinfo is None or parsed.utcoffset() != timedelta(0):
        raise ValueError(f"{label} must use UTC")
    if render_timestamp(parsed) != value:
        raise ValueError(f"{label} is not canonically encoded")
    return parsed


def render_timestamp(value: datetime) -> str:
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def _enabled(value: dict[str, Any], field: str) -> bool:
    item = value.get(field)
    if not isinstance(item, dict) or type(item.get("enabled")) is not bool:
        raise ValueError(f"branch protection {field} state is unavailable")
    return item["enabled"]


def _normalize_ruleset(value: dict[str, Any], *, immutable: bool) -> dict[str, Any]:
    conditions = value.get("conditions", {}).get("ref_name", {})
    rules = value.get("rules")
    if not isinstance(rules, list):
        raise ValueError("ruleset rules must be an array")
    normalized: dict[str, Any] = {
        "id": value.get("id"),
        "name": value.get("name"),
        "target": value.get("target"),
        "enforcement": value.get("enforcement"),
        "exclude": sorted(conditions.get("exclude", [])),
        "include": sorted(conditions.get("include", [])),
        "rules": sorted(item.get("type") for item in rules if isinstance(item, dict)),
    }
    bypass = value.get("bypass_actors")
    if not isinstance(bypass, list):
        raise ValueError("ruleset bypass actors must be an array")
    if immutable:
        normalized["bypass_actor_count"] = len(bypass)
    else:
        if len(bypass) != 1 or not isinstance(bypass[0], dict):
            raise ValueError("tag creation ruleset needs exactly one bypass actor")
        normalized.update(
            {
                "bypass_actor_id": bypass[0].get("actor_id"),
                "bypass_actor_type": bypass[0].get("actor_type"),
                "bypass_mode": bypass[0].get("bypass_mode"),
            }
        )
    return normalized


def _normalize_required_checks(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, dict):
        raise ValueError("required status checks are unavailable")
    contexts = value.get("contexts")
    checks = value.get("checks")
    if not isinstance(contexts, list) or not all(
        isinstance(context, str) and context for context in contexts
    ):
        raise ValueError("required status check contexts are unavailable")
    if not isinstance(checks, list):
        raise ValueError("required status check App bindings are unavailable")
    normalized: list[dict[str, Any]] = []
    for check in checks:
        if not isinstance(check, dict):
            raise ValueError("required status check App binding is invalid")
        context = check.get("context")
        app_id = check.get("app_id")
        if not isinstance(context, str) or not context:
            raise ValueError("required status check context is invalid")
        if type(app_id) is not int or app_id <= 0:
            raise ValueError("required status check app_id must be positive")
        normalized.append({"context": context, "app_id": app_id})
    names = [check["context"] for check in normalized]
    if len(names) != len(set(names)):
        raise ValueError("required status check contexts must be unique")
    if sorted(contexts) != sorted(names):
        raise ValueError("required status check contexts and App bindings differ")
    return sorted(normalized, key=lambda check: check["context"])


def normalize_state(
    responses: dict[str, dict[str, Any]],
    contract: dict[str, Any],
    audit_identity: dict[str, Any],
) -> dict[str, Any]:
    if set(responses) != {key for key, _path in API_GETS}:
        raise ValueError("policy audit response set differs from the GET allowlist")
    repository = responses["repository"]
    security = repository.get("security_and_analysis")
    if not isinstance(security, dict):
        raise ValueError("repository security settings are unavailable")
    normalized_repository = {
        "id": repository.get("id"),
        "full_name": repository.get("full_name"),
        "default_branch": repository.get("default_branch"),
        "visibility": repository.get("visibility"),
        "archived": repository.get("archived"),
        "secret_scanning": security.get("secret_scanning", {}).get("status"),
        "push_protection": security.get("secret_scanning_push_protection", {}).get(
            "status"
        ),
    }
    branch = responses["branch_protection"]
    required_status_checks = branch.get("required_status_checks")
    required_checks = _normalize_required_checks(required_status_checks)
    reviews = branch.get("required_pull_request_reviews")
    review_count = (
        0 if reviews is None else reviews.get("required_approving_review_count")
    )
    normalized_main = {
        "enforce_admins": _enabled(branch, "enforce_admins"),
        "strict": required_status_checks.get("strict"),
        "required_checks": required_checks,
        "required_approving_review_count": review_count,
        "required_signatures": _enabled(branch, "required_signatures"),
        "allow_force_pushes": _enabled(branch, "allow_force_pushes"),
        "allow_deletions": _enabled(branch, "allow_deletions"),
    }
    runners = responses["self_hosted_runners"]
    if runners.get("runners") != []:
        raise ValueError("self-hosted runner inventory must be empty")
    exact_keys(
        audit_identity,
        {"app_id", "installation_id", "app_slug"},
        "audit App action identity",
    )
    for field in ("app_id", "installation_id"):
        positive_integer(audit_identity[field], f"audit App {field}")
    app_response = responses["audit_app"]
    if app_response.get("owner", {}).get("login") != "Helvesec":
        raise ValueError("audit App owner changed")
    if app_response.get("events") != []:
        raise ValueError("audit App webhook events changed")
    normalized_installation = {
        "app_id": app_response.get("id"),
        "installation_id": audit_identity["installation_id"],
        "app_slug": app_response.get("slug"),
        "repository_selection": "selected",
        "permissions": app_response.get("permissions"),
    }
    if audit_identity != {
        "app_id": normalized_installation["app_id"],
        "installation_id": normalized_installation["installation_id"],
        "app_slug": normalized_installation["app_slug"],
    }:
        raise ValueError("audit App action identity differs from the live App")
    normalized_workflows = {
        key: normalize_workflow(responses[key], key)
        for key in (
            "policy_preparer_workflow",
            "policy_promoter_workflow",
            "policy_simulation_workflow",
        )
    }
    installation_repositories = responses["installation_repositories"]
    accessible = installation_repositories.get("repositories")
    if not isinstance(accessible, list) or not all(
        isinstance(repository, dict) for repository in accessible
    ):
        raise ValueError("audit App repository inventory is unavailable")
    accessible_ids = [repository.get("id") for repository in accessible]
    if any(
        type(identifier) is not int or identifier <= 0 for identifier in accessible_ids
    ):
        raise ValueError("audit App repository inventory contains an invalid ID")
    accessible_ids.sort()
    state = {
        "repository": normalized_repository,
        "main": normalized_main,
        "tag_creation_ruleset": _normalize_ruleset(
            responses["tag_creation_ruleset"], immutable=False
        ),
        "tag_immutability_ruleset": _normalize_ruleset(
            responses["tag_immutability_ruleset"], immutable=True
        ),
        "release": normalize_environment(responses["release_environment"], "release"),
        "release_policy_audit": normalize_environment(
            responses["release_policy_audit"], "release-policy-audit"
        ),
        "release_publication": normalize_environment(
            responses["release_publication"], "release-publication"
        ),
        "release_tagging": normalize_environment(
            responses["release_tagging"], "release-tagging"
        ),
        "immutable_releases": responses["immutable_releases"],
        "self_hosted_runners": {"total_count": runners.get("total_count")},
        "audit_app": normalized_installation,
        "audit_app_repositories": {
            "total_count": installation_repositories.get("total_count"),
            "repository_ids": accessible_ids,
        },
        **normalized_workflows,
    }
    expected = contract["expected_state"]
    for key, wanted in expected.items():
        if state.get(key) != wanted:
            raise ValueError(f"live policy state {key} differs from its contract")
    app = contract["audit_app"]
    if normalized_installation != {
        "app_id": app["app_id"],
        "installation_id": app["installation_id"],
        "app_slug": app["app_slug"],
        "repository_selection": app["repository_selection"],
        "permissions": app["permissions"],
    }:
        raise ValueError("live audit App identity or permissions changed")
    return state


def validate_policy_root(value: dict[str, Any], source_sha: str) -> dict[str, str]:
    if value.get("source_git_sha") != source_sha:
        raise ValueError("release policy root source SHA mismatch")
    root = value.get("release_policy_sha256")
    blob = value.get("contract_blob_oid")
    if not isinstance(root, str) or SHA64.fullmatch(root) is None:
        raise ValueError("release policy root hash is invalid")
    if not isinstance(blob, str) or SHA40.fullmatch(blob) is None:
        raise ValueError("release policy contract blob OID is invalid")
    return {"root_sha256": root, "contract_blob_oid": blob}


def build_predicate(
    *,
    contract: dict[str, Any],
    source_sha: str,
    candidate: dict[str, Any],
    release: dict[str, Any],
    policy: dict[str, str],
    audit_run_id: int,
    audit_run_attempt: int,
    audit_workflow_id: int,
    audit_workflow_path: str,
    observed_state: dict[str, Any],
    now: datetime,
) -> dict[str, Any]:
    if not contract["audit_app"]["configured"]:
        raise ValueError("policy audit App is not configured; audit remains disarmed")
    positive_integer(audit_workflow_id, "policy audit workflow ID")
    workflow_state_key = AUDIT_WORKFLOW_STATE_KEYS.get(audit_workflow_path)
    if workflow_state_key is None:
        raise ValueError("policy audit workflow path is not allowed")
    if observed_state.get(workflow_state_key) != {
        "id": audit_workflow_id,
        "path": audit_workflow_path,
        "state": "active",
    }:
        raise ValueError("policy audit workflow identity differs from live state")
    value = {
        "schema_version": 1,
        "predicate_type": PREDICATE_TYPE,
        "status": "shadow-non-authoritative",
        "repository_id": REPOSITORY_ID,
        "source_git_sha": source_sha,
        "candidate": candidate,
        "release": release,
        "release_policy": policy,
        "audit_identity": {
            "run_id": audit_run_id,
            "run_attempt": audit_run_attempt,
            "workflow_id": audit_workflow_id,
            "workflow_path": audit_workflow_path,
            "app_id": contract["audit_app"]["app_id"],
            "installation_id": contract["audit_app"]["installation_id"],
            "app_slug": contract["audit_app"]["app_slug"],
            "repository_selection": contract["audit_app"]["repository_selection"],
            "permissions": contract["audit_app"]["permissions"],
        },
        "emitted_at": render_timestamp(now),
        "expires_at": render_timestamp(
            now + timedelta(seconds=contract["proof"]["freshness_seconds"])
        ),
        "observed_state": observed_state,
    }
    validate_predicate(value)
    return value


def validate_predicate(value: dict[str, Any]) -> None:
    exact_keys(
        value,
        {
            "schema_version",
            "predicate_type",
            "status",
            "repository_id",
            "source_git_sha",
            "candidate",
            "release",
            "release_policy",
            "audit_identity",
            "emitted_at",
            "expires_at",
            "observed_state",
        },
        "policy audit predicate",
    )
    if (
        value["schema_version"] != 1
        or value["predicate_type"] != PREDICATE_TYPE
        or value["status"] != "shadow-non-authoritative"
        or value["repository_id"] != REPOSITORY_ID
        or not isinstance(value["source_git_sha"], str)
        or SHA40.fullmatch(value["source_git_sha"]) is None
    ):
        raise ValueError("policy audit predicate identity changed")
    candidate = value["candidate"]
    exact_keys(
        candidate,
        {
            "run_id",
            "run_attempt",
            "manifest_run_id",
            "manifest_run_attempt",
            "manifest_artifact_id",
            "manifest_artifact_digest",
            "manifest_sha256",
            "manifest_created_at",
            "manifest_expires_at",
        },
        "policy audit candidate binding",
    )
    for field in ("run_id", "manifest_run_id", "manifest_artifact_id"):
        positive_integer(candidate[field], f"candidate {field}")
    if candidate["run_attempt"] != 1 or candidate["manifest_run_attempt"] != 1:
        raise ValueError("candidate and manifest runs must be attempt 1")
    if (
        DIGEST.fullmatch(candidate["manifest_artifact_digest"]) is None
        or SHA64.fullmatch(candidate["manifest_sha256"]) is None
    ):
        raise ValueError("candidate manifest hashes are invalid")
    manifest_created = canonical_timestamp(
        candidate["manifest_created_at"], "candidate manifest created_at"
    )
    manifest_expires = canonical_timestamp(
        candidate["manifest_expires_at"], "candidate manifest expires_at"
    )
    if (
        not manifest_created < manifest_expires
        or manifest_expires - manifest_created > timedelta(hours=48)
    ):
        raise ValueError("candidate manifest freshness window is invalid")
    release = value["release"]
    exact_keys(release, {"intent_id", "planned_ref", "kind"}, "release binding")
    if (
        INTENT.fullmatch(release["intent_id"]) is None
        or RELEASE_REF.fullmatch(release["planned_ref"]) is None
        or release["kind"] not in {"rc", "stable"}
    ):
        raise ValueError("release binding is invalid")
    is_rc_ref = "-rc." in release["planned_ref"]
    if (release["kind"] == "rc") is not is_rc_ref:
        raise ValueError("release kind and planned ref disagree")
    policy = value["release_policy"]
    if (
        set(policy) != {"root_sha256", "contract_blob_oid"}
        or SHA64.fullmatch(policy["root_sha256"]) is None
        or SHA40.fullmatch(policy["contract_blob_oid"]) is None
    ):
        raise ValueError("release policy binding is invalid")
    identity = value["audit_identity"]
    exact_keys(
        identity,
        {
            "run_id",
            "run_attempt",
            "workflow_id",
            "workflow_path",
            "app_id",
            "installation_id",
            "app_slug",
            "repository_selection",
            "permissions",
        },
        "policy audit identity",
    )
    for field in ("run_id", "workflow_id", "app_id", "installation_id"):
        positive_integer(identity[field], f"audit {field}")
    if (
        identity["run_attempt"] != 1
        or identity["workflow_path"] not in AUDIT_WORKFLOW_STATE_KEYS
        or identity["repository_selection"] != "selected"
        or identity["permissions"]
        != {"actions": "read", "administration": "write", "metadata": "read"}
    ):
        raise ValueError("policy audit identity or permissions changed")
    emitted = canonical_timestamp(value["emitted_at"], "policy audit emitted_at")
    expires = canonical_timestamp(value["expires_at"], "policy audit expires_at")
    if expires - emitted != timedelta(seconds=900):
        raise ValueError(
            "policy audit predicate must expire exactly after fifteen minutes"
        )
    if not manifest_created <= emitted < manifest_expires:
        raise ValueError("policy audit was not emitted during candidate freshness")
    if (
        not isinstance(value["observed_state"], dict)
        or len(value["observed_state"]) < 7
    ):
        raise ValueError("policy audit predicate has no exhaustive live state")


def validate_predicate_against_contract(
    value: dict[str, Any], contract: dict[str, Any]
) -> None:
    validate_predicate(value)
    state = value["observed_state"]
    expected_state_keys = set(contract["expected_state"]) | {"audit_app"}
    if set(state) != expected_state_keys:
        raise ValueError(
            "predicate live state keys differ from the exhaustive contract"
        )
    for key, expected in contract["expected_state"].items():
        if state.get(key) != expected:
            raise ValueError(f"predicate policy state {key} differs from its contract")
    app = contract["audit_app"]
    expected_app = {
        "app_id": app["app_id"],
        "installation_id": app["installation_id"],
        "app_slug": app["app_slug"],
        "repository_selection": app["repository_selection"],
        "permissions": app["permissions"],
    }
    if state.get("audit_app") != expected_app:
        raise ValueError("predicate audit App differs from its contract")
    identity = value["audit_identity"]
    if any(
        identity.get(field) != expected_app[field]
        for field in (
            "app_id",
            "installation_id",
            "app_slug",
            "repository_selection",
            "permissions",
        )
    ):
        raise ValueError("predicate audit identity differs from the live App")
    allowed_workflows = contract["workflow"]["audit_workflows"]
    workflow_binding = dict(id=identity["workflow_id"], path=identity["workflow_path"])
    if workflow_binding not in allowed_workflows:
        raise ValueError("predicate workflow identity is not allowed by the contract")
    state_key = AUDIT_WORKFLOW_STATE_KEYS[identity["workflow_path"]]
    workflow = state.get(state_key)
    if not isinstance(workflow, dict) or workflow != {
        "id": identity["workflow_id"],
        "path": identity["workflow_path"],
        "state": "active",
    }:
        raise ValueError("predicate workflow identity differs from live state")


def build_reference(
    predicate: dict[str, Any], artifact_id: int, artifact_digest: str
) -> dict[str, Any]:
    validate_predicate(predicate)
    positive_integer(artifact_id, "policy audit predicate artifact ID")
    if DIGEST.fullmatch(artifact_digest) is None:
        raise ValueError("policy audit predicate artifact digest is invalid")
    identity = predicate["audit_identity"]
    value = {
        "schema_version": 1,
        "status": "shadow-non-authoritative",
        "repository_id": REPOSITORY_ID,
        "source_git_sha": predicate["source_git_sha"],
        "candidate_run_id": predicate["candidate"]["run_id"],
        "release_intent_id": predicate["release"]["intent_id"],
        "policy_audit_run_id": identity["run_id"],
        "policy_audit_run_attempt": identity["run_attempt"],
        "predicate_artifact_id": artifact_id,
        "predicate_artifact_digest": artifact_digest,
        "predicate_sha256": canonical_hash(predicate),
        "emitted_at": predicate["emitted_at"],
        "expires_at": predicate["expires_at"],
        "app_id": identity["app_id"],
        "installation_id": identity["installation_id"],
        "workflow_id": identity["workflow_id"],
        "workflow_path": identity["workflow_path"],
        "release_policy_sha256": predicate["release_policy"]["root_sha256"],
    }
    validate_reference(value, predicate)
    return value


def validate_reference(value: dict[str, Any], predicate: dict[str, Any]) -> None:
    expected = build_reference_fields(predicate)
    if set(value) != set(expected) or any(
        value.get(key) != wanted
        for key, wanted in expected.items()
        if key not in {"predicate_artifact_id", "predicate_artifact_digest"}
    ):
        raise ValueError("policy audit reference does not bind the exact predicate")
    positive_integer(value.get("predicate_artifact_id"), "predicate artifact ID")
    if (
        not isinstance(value.get("predicate_artifact_digest"), str)
        or DIGEST.fullmatch(value["predicate_artifact_digest"]) is None
    ):
        raise ValueError("predicate artifact digest is invalid")


def build_reference_fields(predicate: dict[str, Any]) -> dict[str, Any]:
    validate_predicate(predicate)
    identity = predicate["audit_identity"]
    return {
        "schema_version": 1,
        "status": "shadow-non-authoritative",
        "repository_id": REPOSITORY_ID,
        "source_git_sha": predicate["source_git_sha"],
        "candidate_run_id": predicate["candidate"]["run_id"],
        "release_intent_id": predicate["release"]["intent_id"],
        "policy_audit_run_id": identity["run_id"],
        "policy_audit_run_attempt": identity["run_attempt"],
        "predicate_artifact_id": None,
        "predicate_artifact_digest": None,
        "predicate_sha256": canonical_hash(predicate),
        "emitted_at": predicate["emitted_at"],
        "expires_at": predicate["expires_at"],
        "app_id": identity["app_id"],
        "installation_id": identity["installation_id"],
        "workflow_id": identity["workflow_id"],
        "workflow_path": identity["workflow_path"],
        "release_policy_sha256": predicate["release_policy"]["root_sha256"],
    }
