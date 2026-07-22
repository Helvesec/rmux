#!/usr/bin/env python3
"""Validate pinned downstream repositories from explicit read-only API fixtures."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import REPOSITORY_CONTRACT, exact_keys, read_object
from strict_json import read_json

EXPECTED_REPOSITORIES = {
    "homebrew-core": (52855516, "Homebrew/homebrew-core", "external"),
    "homebrew-rmux": (1259133629, "Helvesec/homebrew-rmux", "rmux-owned"),
    "rmux-packages": (1258602064, "Helvesec/rmux-packages", "rmux-owned"),
    "rmux-web-share": (1249553407, "Helvesec/rmux-web-share", "rmux-owned"),
    "rmux.io": (1240176583, "Helvesec/rmux.io", "rmux-owned"),
    "scoop-rmux": (1259135161, "Helvesec/scoop-rmux", "rmux-owned"),
    "winget-pkgs": (197275551, "microsoft/winget-pkgs", "external"),
}
WRITER_APP = {
    "configured": True,
    "app_id": 4352876,
    "installation_id": 147959477,
    "repository_selection": "selected",
    "pat_fallback": False,
    "required_permissions": {
        "actions": "read",
        "administration": "write",
        "contents": "write",
        "metadata": "read",
    },
}
READY_REPOSITORIES = {
    "homebrew-rmux",
    "rmux-packages",
    "rmux-web-share",
    "scoop-rmux",
}


def load_contract() -> dict[str, Any]:
    contract = read_object(REPOSITORY_CONTRACT, "downstream repository contract")
    exact_keys(
        contract,
        {
            "schema_version",
            "status",
            "description",
            "observed_at",
            "writer_app",
            "required_protection",
            "repositories",
        },
        "downstream repository contract",
    )
    if contract["schema_version"] != 1 or contract["status"] != "review-only-disarmed":
        raise ValueError("downstream repository contract must remain disarmed")
    app = contract["writer_app"]
    if app != WRITER_APP:
        raise ValueError("downstream writer App identity or permissions changed")
    repositories = contract["repositories"]
    if not isinstance(repositories, list):
        raise ValueError("downstream repository inventory must be an array")
    actual = {
        item.get("key"): (item.get("id"), item.get("full_name"), item.get("ownership"))
        for item in repositories
        if isinstance(item, dict)
    }
    if actual != EXPECTED_REPOSITORIES or len(actual) != len(repositories):
        raise ValueError("downstream repository identities changed")
    if sum(item["ownership"] == "rmux-owned" for item in repositories) != 5:
        raise ValueError("downstream registry must contain exactly five owned repos")
    if sum(item["ownership"] == "external" for item in repositories) != 2:
        raise ValueError("downstream registry must contain exactly two external repos")
    for item in repositories:
        expected_ready = item["key"] in READY_REPOSITORIES
        if item.get("activation_ready") is not expected_ready:
            raise ValueError("downstream repository readiness changed")
    return contract


def repository_by_key(contract: dict[str, Any], key: str) -> dict[str, Any]:
    matches = [item for item in contract["repositories"] if item["key"] == key]
    if len(matches) != 1:
        raise ValueError("repository key is not uniquely pinned")
    return matches[0]


def validate_metadata(expected: dict[str, Any], value: dict[str, Any]) -> None:
    for field, wanted in (
        ("id", expected["id"]),
        ("full_name", expected["full_name"]),
        ("visibility", expected["visibility"]),
        ("default_branch", expected["default_branch"]),
        ("archived", False),
    ):
        if value.get(field) != wanted:
            raise ValueError(f"downstream repository metadata {field} changed")


def require_object_fixture(path: Path | None, label: str) -> dict[str, Any]:
    if path is None:
        raise ValueError(f"owned downstream repository requires {label} fixture")
    return read_object(path, label)


def require_array_fixture(path: Path | None, label: str) -> list[Any]:
    if path is None:
        raise ValueError(f"owned downstream repository requires {label} fixture")
    value = read_json(path, label)
    if not isinstance(value, list):
        raise ValueError(f"{label} must be a JSON array")
    return value


def validate_owned_fixtures(expected: dict[str, Any], args: argparse.Namespace) -> None:
    if expected["protection_api_supported"] is not True:
        raise ValueError(
            "private downstream repository protection is unavailable on the current plan"
        )
    protection = require_object_fixture(args.protection, "branch protection")
    enforce_admins = protection.get("enforce_admins", {})
    if (
        not isinstance(enforce_admins, dict)
        or enforce_admins.get("enabled") is not True
        or protection.get("allow_force_pushes", {}).get("enabled") is not False
        or protection.get("allow_deletions", {}).get("enabled") is not False
        or protection.get("required_signatures", {}).get("enabled") is not True
    ):
        raise ValueError("owned downstream branch protection is incomplete")
    rulesets = require_array_fixture(args.rulesets, "repository rulesets")
    if len(rulesets) != 1 or not isinstance(rulesets[0], dict):
        raise ValueError("owned downstream repository ruleset count changed")
    ruleset = rulesets[0]
    conditions = ruleset.get("conditions", {})
    reference_names = conditions.get("ref_name", {})
    rule_types = {
        item.get("type") for item in ruleset.get("rules", []) if isinstance(item, dict)
    }
    if (
        ruleset.get("enforcement") != "active"
        or ruleset.get("bypass_actors") != []
        or reference_names.get("include") != ["refs/heads/main"]
        or reference_names.get("exclude") != []
        or rule_types != {"deletion", "non_fast_forward"}
    ):
        raise ValueError("owned downstream repository ruleset changed")
    environments = require_object_fixture(args.environments, "repository environments")
    matches = [
        item
        for item in environments.get("environments", [])
        if isinstance(item, dict) and item.get("name") == expected["environment"]
    ]
    if (
        len(matches) != 1
        or matches[0].get("can_admins_bypass") is not False
        or not matches[0].get("protection_rules")
    ):
        raise ValueError("owned downstream environment is not protected")
    runners = require_object_fixture(args.runners, "self-hosted runners")
    if runners.get("total_count") != 0:
        raise ValueError("self-hosted downstream runners are forbidden")
    installation = require_object_fixture(args.installation, "writer App installation")
    contract = load_contract()
    app = contract["writer_app"]
    if app["configured"] is not True:
        raise ValueError("downstream writer App is intentionally not configured")
    if (
        installation.get("id") != app["installation_id"]
        or installation.get("app_id") != app["app_id"]
        or installation.get("repository_selection") != "selected"
        or installation.get("permissions") != app["required_permissions"]
        or installation.get("events") != []
    ):
        raise ValueError("downstream writer App installation identity changed")
    repository_ids = installation.get("repository_ids")
    owned_ids = sorted(
        item["id"]
        for item in contract["repositories"]
        if item["ownership"] == "rmux-owned" and item["key"] != "rmux.io"
    )
    if repository_ids != owned_ids:
        raise ValueError("downstream writer App repository scope is not exact")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("contract")
    fixtures = commands.add_parser("fixtures")
    fixtures.add_argument("--repository-key", required=True)
    fixtures.add_argument("--metadata", type=Path, required=True)
    fixtures.add_argument("--protection", type=Path)
    fixtures.add_argument("--rulesets", type=Path)
    fixtures.add_argument("--environments", type=Path)
    fixtures.add_argument("--runners", type=Path)
    fixtures.add_argument("--installation", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    contract = load_contract()
    if args.command == "contract":
        print("downstream-repository-contract=disarmed")
        return 0
    expected = repository_by_key(contract, args.repository_key)
    metadata = read_object(args.metadata, "repository metadata")
    validate_metadata(expected, metadata)
    if expected["ownership"] == "rmux-owned":
        validate_owned_fixtures(expected, args)
    print(f"downstream-repository-verified={args.repository_key}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"verify-downstream-repository: {error}", file=sys.stderr)
        raise SystemExit(1) from error
