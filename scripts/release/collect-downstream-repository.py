#!/usr/bin/env python3
"""Collect exact live protection fixtures for RMUX-owned downstream repositories."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

from downstream_channels import REPOSITORY_CONTRACT, read_object

APP_SLUG = "helvesec-rmux-downstream"
OWNED_KEYS = ("homebrew-rmux", "rmux-packages", "rmux-web-share", "scoop-rmux")
RULESET_PATH = re.compile(r"/repos/Helvesec/[A-Za-z0-9._-]+/rulesets/[1-9][0-9]*")


class GitHubReadApi:
    def __init__(self, token: str) -> None:
        if not token or "\n" in token or "\r" in token:
            raise ValueError("downstream audit token is missing or malformed")
        self._token = token

    def get(self, path: str) -> Any:
        if not path.startswith("/") or "//" in path:
            raise ValueError("downstream audit API path is invalid")
        request = urllib.request.Request(
            f"https://api.github.com{path}",
            method="GET",
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "rmux-downstream-audit/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read(8 * 1024 * 1024 + 1)
        except urllib.error.HTTPError as error:
            raise ValueError(
                f"downstream audit GET {path} failed with HTTP {error.code}"
            ) from error
        if len(raw) > 8 * 1024 * 1024:
            raise ValueError("downstream audit response exceeds the size limit")
        try:
            return json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError("downstream audit response is not valid JSON") from error


def write_json(path: Path, value: Any) -> None:
    if path.exists() or path.is_symlink():
        raise ValueError(f"downstream audit output already exists: {path.name}")
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, separators=(",", ": ")) + "\n",
        encoding="utf-8",
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app-id", type=int, required=True)
    parser.add_argument("--app-slug", required=True)
    parser.add_argument("--installation-id", type=int, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def repository_contract() -> tuple[dict[str, Any], list[dict[str, Any]]]:
    contract = read_object(REPOSITORY_CONTRACT, "downstream repository contract")
    app = contract.get("writer_app")
    repositories = contract.get("repositories")
    if not isinstance(app, dict) or not isinstance(repositories, list):
        raise ValueError("downstream repository contract is incomplete")
    owned = [item for item in repositories if item.get("key") in OWNED_KEYS]
    if [item.get("key") for item in owned] != list(OWNED_KEYS):
        raise ValueError("downstream owned repository order changed")
    return app, owned


def exact_installation_repositories(
    api: GitHubReadApi, repositories: list[dict[str, Any]]
) -> list[int]:
    value = api.get("/installation/repositories?per_page=100")
    if not isinstance(value, dict) or not isinstance(value.get("repositories"), list):
        raise ValueError("downstream App repository selection is unavailable")
    selected = {
        (item.get("id"), item.get("full_name"))
        for item in value["repositories"]
        if isinstance(item, dict)
    }
    expected = {(item["id"], item["full_name"]) for item in repositories}
    if value.get("total_count") != len(expected) or selected != expected:
        raise ValueError("downstream App repository scope changed")
    return sorted(item[0] for item in expected)


def collect_rulesets(api: GitHubReadApi, full_name: str) -> list[dict[str, Any]]:
    value = api.get(f"/repos/{full_name}/rulesets?per_page=100")
    if not isinstance(value, list):
        raise ValueError("downstream ruleset listing is not an array")
    details: list[dict[str, Any]] = []
    for item in value:
        identifier = item.get("id") if isinstance(item, dict) else None
        path = f"/repos/{full_name}/rulesets/{identifier}"
        if not isinstance(identifier, int) or RULESET_PATH.fullmatch(path) is None:
            raise ValueError("downstream ruleset identity is invalid")
        detail = api.get(path)
        if not isinstance(detail, dict):
            raise ValueError("downstream ruleset detail is not an object")
        details.append(detail)
    return details


def execute(args: argparse.Namespace) -> None:
    app, repositories = repository_contract()
    if (
        args.app_id != app.get("app_id")
        or args.installation_id != app.get("installation_id")
        or args.app_slug != APP_SLUG
    ):
        raise ValueError("downstream App runtime identity changed")
    if args.output_dir.exists() or args.output_dir.is_symlink():
        raise ValueError("downstream audit output directory must start absent")
    args.output_dir.mkdir(parents=True)
    api = GitHubReadApi(os.environ.get("RMUX_DOWNSTREAM_TOKEN", ""))
    repository_ids = exact_installation_repositories(api, repositories)
    write_json(
        args.output_dir / "installation.json",
        {
            "id": args.installation_id,
            "app_id": args.app_id,
            "repository_selection": "selected",
            "permissions": app["required_permissions"],
            "events": [],
            "repository_ids": repository_ids,
        },
    )
    for repository in repositories:
        key = repository["key"]
        full_name = repository["full_name"]
        root = args.output_dir / key
        root.mkdir()
        values = {
            "metadata.json": api.get(f"/repos/{full_name}"),
            "protection.json": api.get(f"/repos/{full_name}/branches/main/protection"),
            "rulesets.json": collect_rulesets(api, full_name),
            "environments.json": api.get(
                f"/repos/{full_name}/environments?per_page=100"
            ),
            "runners.json": api.get(f"/repos/{full_name}/actions/runners?per_page=100"),
        }
        for filename, value in values.items():
            write_json(root / filename, value)
    print("downstream-live-fixtures=collected")


if __name__ == "__main__":
    try:
        execute(parse_args())
    except (OSError, ValueError) as error:
        print(f"collect-downstream-repository: {error}", file=sys.stderr)
        raise SystemExit(1) from error
