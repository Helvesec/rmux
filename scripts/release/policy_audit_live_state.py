#!/usr/bin/env python3
"""Normalize live GitHub release-environment state."""

from __future__ import annotations

from typing import Any


def _enabled(value: dict[str, Any], field: str) -> bool:
    item = value.get(field)
    if not isinstance(item, dict) or type(item.get("enabled")) is not bool:
        raise ValueError(f"branch protection {field} state is unavailable")
    return item["enabled"]


def normalize_required_checks(value: Any) -> list[dict[str, Any]]:
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


def normalize_main(value: dict[str, Any]) -> dict[str, Any]:
    required_status_checks = value.get("required_status_checks")
    reviews = value.get("required_pull_request_reviews")
    review_count = (
        0 if reviews is None else reviews.get("required_approving_review_count")
    )
    return {
        "enforce_admins": _enabled(value, "enforce_admins"),
        "strict": required_status_checks.get("strict")
        if isinstance(required_status_checks, dict)
        else None,
        "required_checks": normalize_required_checks(required_status_checks),
        "required_approving_review_count": review_count,
        "required_signatures": _enabled(value, "required_signatures"),
        "allow_force_pushes": _enabled(value, "allow_force_pushes"),
        "allow_deletions": _enabled(value, "allow_deletions"),
    }


def normalize_ruleset(value: dict[str, Any], *, immutable: bool) -> dict[str, Any]:
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


def normalize_environment(
    value: dict[str, Any], policies: dict[str, Any], label: str
) -> dict[str, Any]:
    rules = value.get("protection_rules")
    if not isinstance(rules, list) or sorted(item.get("type") for item in rules) != [
        "branch_policy",
        "required_reviewers",
    ]:
        raise ValueError(f"{label} protection rules changed")
    reviewer_rule = next(
        item for item in rules if item.get("type") == "required_reviewers"
    )
    reviewers = reviewer_rule.get("reviewers")
    if not isinstance(reviewers, list) or len(reviewers) != 1:
        raise ValueError(f"{label} must have exactly one solo-maintainer reviewer")
    reviewer = reviewers[0]
    actor = reviewer.get("reviewer", {})
    deployment = value.get("deployment_branch_policy", {})
    raw_policies = policies.get("branch_policies")
    if policies.get("total_count") != 2 or not isinstance(raw_policies, list):
        raise ValueError(f"{label} deployment policy inventory changed")
    normalized_policies = []
    for policy in raw_policies:
        if not isinstance(policy, dict):
            raise ValueError(f"{label} deployment policy is invalid")
        name = policy.get("name")
        policy_type = policy.get("type")
        if not isinstance(name, str) or policy_type not in {"branch", "tag"}:
            raise ValueError(f"{label} deployment policy identity is invalid")
        normalized_policies.append({"name": name, "type": policy_type})
    normalized_policies.sort(key=lambda policy: (policy["type"], policy["name"]))
    if len({(item["type"], item["name"]) for item in normalized_policies}) != 2:
        raise ValueError(f"{label} deployment policies are duplicated")
    return {
        "environment_id": value.get("id"),
        "can_admins_bypass": value.get("can_admins_bypass"),
        "protected_branches": deployment.get("protected_branches"),
        "custom_branch_policies": deployment.get("custom_branch_policies"),
        "deployment_policies": normalized_policies,
        "prevent_self_review": reviewer_rule.get("prevent_self_review"),
        "reviewer_type": reviewer.get("type"),
        "reviewer_id": actor.get("id"),
        "reviewer_login": actor.get("login"),
    }


def normalize_workflow(value: dict[str, Any], label: str) -> dict[str, Any]:
    workflow_id = value.get("id")
    if type(workflow_id) is not int or workflow_id <= 0:
        raise ValueError(f"{label} ID must be a positive integer")
    return {
        "id": workflow_id,
        "path": value.get("path"),
        "state": value.get("state"),
    }
