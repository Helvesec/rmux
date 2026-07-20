#!/usr/bin/env python3
"""Validate pinned downstream repositories from explicit read-only API fixtures."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import REPOSITORY_CONTRACT, exact_keys, read_object

EXPECTED_REPOSITORIES = {
    "homebrew-core": (52855516, "Homebrew/homebrew-core", "external"),
    "homebrew-rmux": (1259133629, "Helvesec/homebrew-rmux", "rmux-owned"),
    "rmux-packages": (1258602064, "Helvesec/rmux-packages", "rmux-owned"),
    "rmux-web-share": (1249553407, "Helvesec/rmux-web-share", "rmux-owned"),
    "rmux.io": (1240176583, "Helvesec/rmux.io", "rmux-owned"),
    "scoop-rmux": (1259135161, "Helvesec/scoop-rmux", "rmux-owned"),
    "winget-pkgs": (197275551, "microsoft/winget-pkgs", "external"),
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
    if (
        app.get("configured") is not False
        or app.get("app_id") is not None
        or app.get("installation_id") is not None
        or app.get("pat_fallback") is not False
    ):
        raise ValueError("downstream writer identity must remain unconfigured")
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
    if any(item.get("activation_ready") is not False for item in repositories):
        raise ValueError("downstream repositories must remain activation-blocked")
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


def require_fixture(path: Path | None, label: str) -> dict[str, Any]:
    if path is None:
        raise ValueError(f"owned downstream repository requires {label} fixture")
    return read_object(path, label)


def validate_owned_fixtures(expected: dict[str, Any], args: argparse.Namespace) -> None:
    if expected["protection_api_supported"] is not True:
        raise ValueError(
            "private downstream repository protection is unavailable on the current plan"
        )
    protection = require_fixture(args.protection, "branch protection")
    enforce_admins = protection.get("enforce_admins", {})
    if (
        not isinstance(enforce_admins, dict)
        or enforce_admins.get("enabled") is not True
        or protection.get("allow_force_pushes", {}).get("enabled") is not False
        or protection.get("allow_deletions", {}).get("enabled") is not False
        or protection.get("required_signatures", {}).get("enabled") is not True
    ):
        raise ValueError("owned downstream branch protection is incomplete")
    rulesets = require_fixture(args.rulesets, "repository rulesets")
    if not isinstance(rulesets, list) or not any(
        item.get("enforcement") == "active"
        for item in rulesets
        if isinstance(item, dict)
    ):
        raise ValueError("owned downstream repository has no active ruleset")
    environments = require_fixture(args.environments, "repository environments")
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
    runners = require_fixture(args.runners, "self-hosted runners")
    if runners.get("total_count") != 0:
        raise ValueError("self-hosted downstream runners are forbidden")
    installation = require_fixture(args.installation, "writer App installation")
    contract = load_contract()
    app = contract["writer_app"]
    if app["configured"] is not True:
        raise ValueError("downstream writer App is intentionally not configured")
    if installation.get("id") != app["installation_id"]:
        raise ValueError("downstream writer App installation identity changed")
    repository_ids = installation.get("repository_ids")
    owned_ids = sorted(
        item["id"]
        for item in contract["repositories"]
        if item["ownership"] == "rmux-owned"
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
