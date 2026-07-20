#!/usr/bin/env python3
"""Normalize live GitHub release-environment state."""

from __future__ import annotations

from typing import Any


def normalize_environment(value: dict[str, Any], label: str) -> dict[str, Any]:
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
    return {
        "environment_id": value.get("id"),
        "can_admins_bypass": value.get("can_admins_bypass"),
        "protected_branches": deployment.get("protected_branches"),
        "custom_branch_policies": deployment.get("custom_branch_policies"),
        "prevent_self_review": reviewer_rule.get("prevent_self_review"),
        "reviewer_type": reviewer.get("type"),
        "reviewer_id": actor.get("id"),
        "reviewer_login": actor.get("login"),
    }
