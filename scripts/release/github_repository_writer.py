"""Minimal GitHub API client for one signed atomic repository update."""

from __future__ import annotations

import hashlib
import json
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

from github_repository_commit_plan import (
    CREATE_COMMIT_MUTATION,
    GRAPHQL_COMMIT_MAX_BYTES,
    commit_variables,
    signed_commit_batches,
    staging_branch_name,
)


@dataclass(frozen=True)
class PublishOutcome:
    state: str
    mutation_started: bool
    commit_sha: str


TRANSIENT_GRAPHQL_ERRORS = (
    "GitHub API POST /graphql failed: 429 ",
    "GitHub API POST /graphql failed: 500 ",
    "GitHub API POST /graphql failed: 502 ",
    "GitHub API POST /graphql failed: 503 ",
    "GitHub API POST /graphql failed: 504 ",
)


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

    def patch(self, path: str, payload: dict[str, Any]) -> dict[str, Any]:
        return self.request("PATCH", path, payload)

    def delete(self, path: str) -> None:
        if not path.startswith("/") or "//" in path:
            raise ValueError("GitHub API path is invalid")
        request = urllib.request.Request(
            f"https://api.github.com{path}",
            method="DELETE",
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "rmux-release-writer/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read(1)
        except urllib.error.HTTPError as error:
            detail = error.read(4096).decode("utf-8", errors="replace")
            raise ValueError(
                f"GitHub API DELETE {path} failed: {error.code} {detail}"
            ) from error
        if raw:
            raise ValueError("GitHub API DELETE returned an unexpected response")

    def graphql(self, query: str, variables: dict[str, Any]) -> dict[str, Any]:
        value = self.post("/graphql", {"query": query, "variables": variables})
        errors = value.get("errors")
        data = value.get("data")
        if errors is not None or not isinstance(data, dict):
            messages = []
            if isinstance(errors, list):
                for error in errors[:3]:
                    if not isinstance(error, dict):
                        continue
                    message = error.get("message")
                    if isinstance(message, str):
                        messages.append(" ".join(message.split())[:500])
            detail = f": {'; '.join(messages)}" if messages else ""
            raise ValueError(f"GitHub GraphQL commit mutation failed{detail}")
        return data


def object_sha(value: dict[str, Any], label: str) -> str:
    sha = value.get("sha")
    if (
        not isinstance(sha, str)
        or len(sha) != 40
        or any(character not in "0123456789abcdef" for character in sha)
    ):
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


def git_blob_sha(contents: bytes) -> str:
    digest = hashlib.sha1(usedforsecurity=False)
    digest.update(f"blob {len(contents)}\0".encode())
    digest.update(contents)
    return digest.hexdigest()


def verify_rest_commit(
    api: GitHubApi,
    full_name: str,
    commit_sha: str,
    *,
    expected_parent: str,
) -> None:
    commit = api.get(f"/repos/{full_name}/git/commits/{commit_sha}")
    verification = commit.get("verification")
    parents = commit.get("parents")
    if (
        not isinstance(verification, dict)
        or verification.get("verified") is not True
        or verification.get("reason") != "valid"
        or not isinstance(verification.get("signature"), str)
        or not verification["signature"]
    ):
        raise ValueError("GitHub REST verification rejected the signed commit")
    if (
        not isinstance(parents, list)
        or len(parents) != 1
        or not isinstance(parents[0], dict)
        or object_sha(parents[0], "commit parent") != expected_parent
    ):
        raise ValueError("GitHub signed commit parent differs")


def recover_exact_signed_commit(
    api: GitHubApi,
    *,
    full_name: str,
    commit_sha: str,
    base_commit: str,
    additions: dict[str, bytes],
    deletions: set[str],
) -> bool:
    verify_rest_commit(
        api,
        full_name,
        commit_sha,
        expected_parent=base_commit,
    )
    expected = tree_blobs(api, full_name, base_commit)
    for path in deletions:
        expected.pop(path, None)
    for path, contents in additions.items():
        expected[path] = git_blob_sha(contents)
    if tree_blobs(api, full_name, commit_sha) != expected:
        return False
    return all(
        file_at(api, full_name, path, commit_sha) == contents
        for path, contents in additions.items()
    )


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
    variables = commit_variables(
        full_name=full_name,
        branch=branch,
        base_commit=base_commit,
        additions=additions,
        deletions=deletions,
        message=message,
    )
    request_size = len(
        json.dumps(
            {"query": CREATE_COMMIT_MUTATION, "variables": variables},
            separators=(",", ":"),
        ).encode()
    )
    if request_size > GRAPHQL_COMMIT_MAX_BYTES:
        raise ValueError("signed GitHub commit request exceeds its safe payload limit")
    for attempt in range(3):
        try:
            data = api.graphql(CREATE_COMMIT_MUTATION, variables)
            break
        except (OSError, ValueError) as error:
            transient = isinstance(error, OSError) or any(
                marker in str(error) for marker in TRANSIENT_GRAPHQL_ERRORS
            )
            if not transient:
                raise
            current = branch_head(api, full_name, branch)
            if current != base_commit:
                if recover_exact_signed_commit(
                    api,
                    full_name=full_name,
                    commit_sha=current,
                    base_commit=base_commit,
                    additions=additions,
                    deletions=deletions,
                ):
                    return current
                raise ValueError(
                    "signed commit outcome advanced to unexpected repository bytes"
                ) from error
            if attempt == 2:
                raise
            time.sleep(2**attempt)
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
    verify_rest_commit(
        api,
        full_name,
        commit_sha,
        expected_parent=base_commit,
    )
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


def tree_blobs(api: GitHubApi, full_name: str, commit_sha: str) -> dict[str, str]:
    value = api.get(f"/repos/{full_name}/git/trees/{commit_sha}?recursive=1")
    if value.get("truncated") is not False or not isinstance(value.get("tree"), list):
        raise ValueError("downstream repository tree is missing or truncated")
    blobs: dict[str, str] = {}
    for entry in value["tree"]:
        if not isinstance(entry, dict) or entry.get("type") != "blob":
            continue
        path = entry.get("path")
        if not isinstance(path, str) or not path or path in blobs:
            raise ValueError("downstream repository tree path is invalid")
        blobs[path] = object_sha(entry, "tree blob")
    return blobs


def tree_paths(api: GitHubApi, full_name: str, commit_sha: str) -> set[str]:
    return set(tree_blobs(api, full_name, commit_sha))


def managed_paths(paths: set[str], prefixes: tuple[str, ...]) -> set[str]:
    return {
        path
        for path in paths
        if any(path == prefix or path.startswith(f"{prefix}/") for prefix in prefixes)
    }


def repository_state_is_exact(
    api: GitHubApi,
    *,
    full_name: str,
    commit_sha: str,
    updates: dict[str, bytes],
    managed_prefixes: tuple[str, ...],
    managed_expected: set[str],
) -> bool:
    if managed_prefixes:
        existing = managed_paths(
            tree_paths(api, full_name, commit_sha), managed_prefixes
        )
        if existing != managed_expected:
            return False
    return all(
        file_at(api, full_name, path, commit_sha) == data
        for path, data in updates.items()
    )


def delete_branch_if_present(api: GitHubApi, full_name: str, branch: str) -> None:
    encoded = urllib.parse.quote(branch, safe="")
    get_path = f"/repos/{full_name}/git/ref/heads/{encoded}"
    try:
        api.get(get_path)
    except ValueError as error:
        if "failed: 404 " in str(error):
            return
        raise
    api.delete(f"/repos/{full_name}/git/refs/heads/{encoded}")


def create_staging_branch(
    api: GitHubApi, full_name: str, branch: str, base_commit: str
) -> None:
    value = api.post(
        f"/repos/{full_name}/git/refs",
        {"ref": f"refs/heads/{branch}", "sha": base_commit},
    )
    target = value.get("object")
    if (
        value.get("ref") != f"refs/heads/{branch}"
        or not isinstance(target, dict)
        or object_sha(target, "created staging branch") != base_commit
    ):
        raise ValueError("GitHub created an unexpected staging branch")


def advance_branch(
    api: GitHubApi, full_name: str, branch: str, commit_sha: str
) -> None:
    encoded = urllib.parse.quote(branch, safe="")
    value = api.patch(
        f"/repos/{full_name}/git/refs/heads/{encoded}",
        {"sha": commit_sha, "force": False},
    )
    target = value.get("object")
    if (
        value.get("ref") != f"refs/heads/{branch}"
        or not isinstance(target, dict)
        or object_sha(target, "advanced branch") != commit_sha
    ):
        raise ValueError("GitHub advanced an unexpected downstream branch")


def publish_batched_signed_commits(
    api: GitHubApi,
    *,
    full_name: str,
    branch: str,
    base_commit: str,
    batches: list[tuple[dict[str, bytes], set[str]]],
    updates: dict[str, bytes],
    managed_prefixes: tuple[str, ...],
    managed_expected: set[str],
    message: str,
) -> str:
    staging = staging_branch_name(
        base_commit,
        updates,
        set().union(*(deletions for _, deletions in batches)),
    )
    delete_branch_if_present(api, full_name, staging)
    create_staging_branch(api, full_name, staging, base_commit)
    current = base_commit
    try:
        for index, (additions, deletions) in enumerate(batches, start=1):
            current = create_signed_commit(
                api,
                full_name=full_name,
                branch=staging,
                base_commit=current,
                additions=additions,
                deletions=deletions,
                message=f"{message} (part {index}/{len(batches)})",
            )
        if not repository_state_is_exact(
            api,
            full_name=full_name,
            commit_sha=current,
            updates=updates,
            managed_prefixes=managed_prefixes,
            managed_expected=managed_expected,
        ):
            raise ValueError("staged downstream repository bytes differ")
        if branch_head(api, full_name, branch) != base_commit:
            raise ValueError("downstream repository changed during signed staging")
        advance_branch(api, full_name, branch, current)
    except (OSError, ValueError):
        try:
            delete_branch_if_present(api, full_name, staging)
        except (OSError, ValueError):
            pass
        raise
    delete_branch_if_present(api, full_name, staging)
    return current


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
    existing_paths = (
        tree_paths(api, full_name, base_commit) if managed_prefixes else set()
    )
    managed_existing = managed_paths(existing_paths, managed_prefixes)
    managed_expected = managed_paths(set(updates), managed_prefixes)
    if managed_prefixes and not managed_expected:
        raise ValueError("managed repository update has no files under its prefixes")
    if repository_state_is_exact(
        api,
        full_name=full_name,
        commit_sha=base_commit,
        updates=updates,
        managed_prefixes=managed_prefixes,
        managed_expected=managed_expected,
    ):
        return PublishOutcome("no-op-exact", False, base_commit)
    if expected_base is not None and base_commit != expected_base:
        raise ValueError("downstream repository changed after payload preparation")
    deletions = managed_existing - managed_expected
    changed_updates = {
        path: data
        for path, data in updates.items()
        if file_at(api, full_name, path, base_commit) != data
    }
    batches = signed_commit_batches(
        full_name=full_name,
        branch=branch,
        base_commit=base_commit,
        additions=changed_updates,
        deletions=deletions,
        message=message,
    )
    if len(batches) == 1:
        additions, batch_deletions = batches[0]
        commit_sha = create_signed_commit(
            api,
            full_name=full_name,
            branch=branch,
            base_commit=base_commit,
            additions=additions,
            deletions=batch_deletions,
            message=message,
        )
    else:
        commit_sha = publish_batched_signed_commits(
            api,
            full_name=full_name,
            branch=branch,
            base_commit=base_commit,
            batches=batches,
            updates=updates,
            managed_prefixes=managed_prefixes,
            managed_expected=managed_expected,
            message=message,
        )
    if branch_head(api, full_name, branch) != commit_sha:
        raise ValueError("downstream branch did not advance to the exact commit")
    if not repository_state_is_exact(
        api,
        full_name=full_name,
        commit_sha=commit_sha,
        updates=updates,
        managed_prefixes=managed_prefixes,
        managed_expected=managed_expected,
    ):
        raise ValueError("downstream repository bytes differ after publication")
    return PublishOutcome("public-live", True, commit_sha)
