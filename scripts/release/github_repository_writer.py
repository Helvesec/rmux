"""Minimal GitHub API client for one signed atomic repository update."""

from __future__ import annotations

import base64
import json
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any


CREATE_COMMIT_MUTATION = """
mutation CreateSignedCommit($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) {
    commit {
      oid
      signature {
        isValid
        state
        wasSignedByGitHub
      }
    }
    ref {
      target {
        oid
      }
    }
  }
}
"""


@dataclass(frozen=True)
class PublishOutcome:
    state: str
    mutation_started: bool
    commit_sha: str


class GitHubApi:
    def __init__(self, token: str) -> None:
        if not token or "\n" in token or "\r" in token:
            raise ValueError("GitHub App token is missing or malformed")
        self._token = token

    def request(
        self, method: str, path: str, payload: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        if not path.startswith("/") or "//" in path:
            raise ValueError("GitHub API path is invalid")
        data = None
        if payload is not None:
            data = json.dumps(payload, separators=(",", ":")).encode()
        request = urllib.request.Request(
            f"https://api.github.com{path}",
            data=data,
            method=method,
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "rmux-release-writer/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read(8 * 1024 * 1024 + 1)
        except urllib.error.HTTPError as error:
            detail = error.read(4096).decode("utf-8", errors="replace")
            raise ValueError(
                f"GitHub API {method} {path} failed: {error.code} {detail}"
            ) from error
        if len(raw) > 8 * 1024 * 1024:
            raise ValueError("GitHub API response exceeds the release limit")
        try:
            value = json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError("GitHub API returned invalid JSON") from error
        if not isinstance(value, dict):
            raise ValueError("GitHub API returned a non-object")
        return value

    def get(self, path: str) -> dict[str, Any]:
        return self.request("GET", path)

    def get_bytes(self, path: str, *, limit: int) -> bytes:
        if not path.startswith("/") or "//" in path or limit <= 0:
            raise ValueError("GitHub raw API request is invalid")
        request = urllib.request.Request(
            f"https://api.github.com{path}",
            method="GET",
            headers={
                "Accept": "application/vnd.github.raw+json",
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "rmux-release-writer/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read(limit + 1)
        except urllib.error.HTTPError as error:
            detail = error.read(4096).decode("utf-8", errors="replace")
            raise ValueError(
                f"GitHub API GET {path} failed: {error.code} {detail}"
            ) from error
        if len(raw) > limit:
            raise ValueError("GitHub raw file exceeds the release limit")
        return raw

    def post(self, path: str, payload: dict[str, Any]) -> dict[str, Any]:
        return self.request("POST", path, payload)

    def graphql(self, query: str, variables: dict[str, Any]) -> dict[str, Any]:
        value = self.post("/graphql", {"query": query, "variables": variables})
        errors = value.get("errors")
        data = value.get("data")
        if errors is not None or not isinstance(data, dict):
            raise ValueError("GitHub GraphQL commit mutation failed")
        return data


def object_sha(value: dict[str, Any], label: str) -> str:
    sha = value.get("sha")
    if not isinstance(sha, str) or len(sha) != 40:
        raise ValueError(f"GitHub {label} has no canonical SHA")
    return sha


def object_oid(value: dict[str, Any], label: str) -> str:
    oid = value.get("oid")
    if (
        not isinstance(oid, str)
        or len(oid) != 40
        or any(character not in "0123456789abcdef" for character in oid)
    ):
        raise ValueError(f"GitHub {label} has no canonical OID")
    return oid


def create_signed_commit(
    api: GitHubApi,
    *,
    full_name: str,
    branch: str,
    base_commit: str,
    additions: dict[str, bytes],
    deletions: set[str],
    message: str,
) -> str:
    data = api.graphql(
        CREATE_COMMIT_MUTATION,
        {
            "input": {
                "branch": {
                    "repositoryNameWithOwner": full_name,
                    "branchName": branch,
                },
                "expectedHeadOid": base_commit,
                "message": {"headline": message},
                "fileChanges": {
                    "additions": [
                        {
                            "path": path,
                            "contents": base64.b64encode(contents).decode("ascii"),
                        }
                        for path, contents in sorted(additions.items())
                    ],
                    "deletions": [{"path": path} for path in sorted(deletions)],
                },
            }
        },
    )
    mutation = data.get("createCommitOnBranch")
    if not isinstance(mutation, dict):
        raise ValueError("GitHub signed commit mutation returned no result")
    commit = mutation.get("commit")
    reference = mutation.get("ref")
    if not isinstance(commit, dict) or not isinstance(reference, dict):
        raise ValueError("GitHub signed commit mutation returned incomplete objects")
    commit_sha = object_oid(commit, "signed commit")
    target = reference.get("target")
    if not isinstance(target, dict) or object_oid(target, "updated ref") != commit_sha:
        raise ValueError("GitHub signed commit mutation advanced an unexpected ref")
    signature = commit.get("signature")
    if (
        not isinstance(signature, dict)
        or signature.get("isValid") is not True
        or signature.get("state") != "VALID"
        or signature.get("wasSignedByGitHub") is not True
    ):
        raise ValueError("GitHub did not create a valid platform-signed commit")
    verification = api.get(f"/repos/{full_name}/git/commits/{commit_sha}").get(
        "verification"
    )
    if (
        not isinstance(verification, dict)
        or verification.get("verified") is not True
        or verification.get("reason") != "valid"
        or not isinstance(verification.get("signature"), str)
        or not verification["signature"]
    ):
        raise ValueError("GitHub REST verification rejected the signed commit")
    return commit_sha


def repository_identity(
    api: GitHubApi, full_name: str, repository_id: int, default_branch: str
) -> None:
    value = api.get(f"/repos/{full_name}")
    if (
        value.get("id") != repository_id
        or value.get("full_name") != full_name
        or value.get("visibility") != "public"
        or value.get("default_branch") != default_branch
        or value.get("archived") is not False
    ):
        raise ValueError("downstream repository identity changed")


def branch_head(api: GitHubApi, full_name: str, branch: str) -> str:
    value = api.get(f"/repos/{full_name}/git/ref/heads/{branch}")
    target = value.get("object")
    if not isinstance(target, dict) or target.get("type") != "commit":
        raise ValueError("downstream branch does not resolve to one commit")
    return object_sha(target, "branch")


def file_at(api: GitHubApi, full_name: str, path: str, ref: str) -> bytes | None:
    encoded_path = urllib.parse.quote(path, safe="/")
    encoded_ref = urllib.parse.quote(ref, safe="")
    try:
        return api.get_bytes(
            f"/repos/{full_name}/contents/{encoded_path}?ref={encoded_ref}",
            limit=64 * 1024 * 1024,
        )
    except ValueError as error:
        if "failed: 404 " in str(error):
            return None
        raise


def tree_paths(api: GitHubApi, full_name: str, commit_sha: str) -> set[str]:
    value = api.get(f"/repos/{full_name}/git/trees/{commit_sha}?recursive=1")
    if value.get("truncated") is not False or not isinstance(value.get("tree"), list):
        raise ValueError("downstream repository tree is missing or truncated")
    paths: set[str] = set()
    for entry in value["tree"]:
        if not isinstance(entry, dict) or entry.get("type") != "blob":
            continue
        path = entry.get("path")
        if not isinstance(path, str) or not path or path in paths:
            raise ValueError("downstream repository tree path is invalid")
        paths.add(path)
    return paths


def publish(
    api: GitHubApi,
    *,
    full_name: str,
    branch: str,
    updates: dict[str, bytes],
    message: str,
    managed_prefixes: tuple[str, ...] = (),
    expected_base: str | None = None,
) -> PublishOutcome:
    base_commit = branch_head(api, full_name, branch)
    if expected_base is not None and base_commit != expected_base:
        raise ValueError("downstream repository changed after payload preparation")
    existing_paths = (
        tree_paths(api, full_name, base_commit) if managed_prefixes else set()
    )
    managed_existing = {
        path
        for path in existing_paths
        if any(
            path == prefix or path.startswith(f"{prefix}/")
            for prefix in managed_prefixes
        )
    }
    managed_expected = {
        path
        for path in updates
        if any(
            path == prefix or path.startswith(f"{prefix}/")
            for prefix in managed_prefixes
        )
    }
    if managed_prefixes and not managed_expected:
        raise ValueError("managed repository update has no files under its prefixes")
    if managed_existing == managed_expected and all(
        file_at(api, full_name, path, base_commit) == data
        for path, data in updates.items()
    ):
        return PublishOutcome("no-op-exact", False, base_commit)
    commit_sha = create_signed_commit(
        api,
        full_name=full_name,
        branch=branch,
        base_commit=base_commit,
        additions=updates,
        deletions=managed_existing - managed_expected,
        message=message,
    )
    if branch_head(api, full_name, branch) != commit_sha:
        raise ValueError("downstream branch did not advance to the exact commit")
    for path, expected in updates.items():
        if file_at(api, full_name, path, commit_sha) != expected:
            raise ValueError("downstream repository bytes differ after publication")
    if managed_prefixes:
        final_paths = tree_paths(api, full_name, commit_sha)
        managed_final = {
            path
            for path in final_paths
            if any(
                path == prefix or path.startswith(f"{prefix}/")
                for prefix in managed_prefixes
            )
        }
        if managed_final != managed_expected:
            raise ValueError("managed repository paths differ after publication")
    return PublishOutcome("public-live", True, commit_sha)
