#!/usr/bin/env python3
"""Fail closed on repository-local Actions and reusable workflows."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_ROOT = ".github/workflows"
ACTION_ROOT = ".github/actions"
USES_LINE = re.compile(r"^\s*(?:-\s*)?uses:\s*(?P<value>[^#]+?)(?:\s+#.*)?$")


def tracked_paths(repository_root: Path) -> set[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=repository_root,
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        raise ValueError("cannot enumerate tracked files for local Action policy")
    return {value.decode("utf-8") for value in result.stdout.split(b"\x00") if value}


def workflow_sources(tracked: set[str]) -> set[str]:
    return {
        path
        for path in tracked
        if Path(path).parent.as_posix() == WORKFLOW_ROOT
        and Path(path).suffix in {".yml", ".yaml"}
    }


def action_manifests(tracked: set[str]) -> set[str]:
    return {
        path
        for path in tracked
        if Path(path).name.casefold() in {"action.yml", "action.yaml"}
    }


def reject_gitlinks(repository_root: Path) -> None:
    result = subprocess.run(
        ["git", "ls-files", "--stage", "-z"],
        cwd=repository_root,
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        raise ValueError("cannot inspect Git object modes for local Action policy")
    gitlinks: list[str] = []
    for record in result.stdout.split(b"\x00"):
        if not record:
            continue
        metadata, separator, raw_path = record.partition(b"\t")
        if not separator:
            raise ValueError("cannot parse staged Git entry")
        mode = metadata.split(b" ", 1)[0]
        if mode == b"160000":
            gitlinks.append(raw_path.decode("utf-8"))
    if gitlinks:
        raise ValueError(
            f"gitlinks cannot participate in local Action resolution: {sorted(gitlinks)}"
        )


def local_uses(source: str, text: str) -> list[str]:
    references: list[str] = []
    for line_number, line in enumerate(text.splitlines(), start=1):
        match = USES_LINE.match(line)
        if not match:
            continue
        value = match.group("value").strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
            value = value[1:-1]
        if value.startswith("./"):
            if any(character.isspace() for character in value):
                raise ValueError(
                    f"{source}:{line_number}: local uses path contains whitespace"
                )
            references.append(value)
    return references


def canonical_local_path(source: str, reference: str) -> str:
    if "\\" in reference or "//" in reference or "@" in reference:
        raise ValueError(f"{source}: non-canonical local uses path {reference!r}")
    parts = reference[2:].split("/")
    if not parts or any(part in {"", ".", ".."} for part in parts):
        raise ValueError(f"{source}: non-canonical local uses path {reference!r}")
    return "/".join(parts)


def validate_local_action_policy(repository_root: Path, tracked: set[str]) -> None:
    reject_gitlinks(repository_root)
    workflows = workflow_sources(tracked)
    all_manifests = action_manifests(tracked)
    noncanonical_manifests = sorted(
        manifest
        for manifest in all_manifests
        if Path(manifest).name not in {"action.yml", "action.yaml"}
    )
    if noncanonical_manifests:
        raise ValueError(
            "Action manifests must use the exact lowercase name action.yml or "
            f"action.yaml: {noncanonical_manifests}"
        )
    outside_action_root = sorted(
        manifest
        for manifest in all_manifests
        if not Path(manifest).parent.as_posix().startswith(f"{ACTION_ROOT}/")
    )
    if outside_action_root:
        raise ValueError(
            "all tracked action.yml/action.yaml manifests must live below "
            f".github/actions: {outside_action_root}"
        )
    manifests = all_manifests
    manifests_by_directory: dict[str, list[str]] = defaultdict(list)
    for manifest in manifests:
        manifests_by_directory[Path(manifest).parent.as_posix()].append(manifest)
    ambiguous = {
        directory: sorted(paths)
        for directory, paths in manifests_by_directory.items()
        if len(paths) != 1
    }
    if ambiguous:
        raise ValueError(
            f"local Action directories have ambiguous manifests: {ambiguous}"
        )

    for source in sorted(workflows | manifests):
        source_path = repository_root / source
        if source_path.is_symlink() or not source_path.is_file():
            raise ValueError(f"local uses source must be a regular file: {source}")
        text = source_path.read_text(encoding="utf-8")
        for reference in local_uses(source, text):
            target = canonical_local_path(source, reference)
            if target.startswith(f"{ACTION_ROOT}/"):
                if target not in manifests_by_directory:
                    raise ValueError(
                        f"{source}: local Action {reference!r} has no single tracked "
                        "action.yml or action.yaml"
                    )
                continue
            if target.startswith(f"{WORKFLOW_ROOT}/"):
                target_path = Path(target)
                if (
                    target_path.parent.as_posix() != WORKFLOW_ROOT
                    or target_path.suffix not in {".yml", ".yaml"}
                    or target not in workflows
                ):
                    raise ValueError(
                        f"{source}: reusable workflow path is not tracked and canonical: "
                        f"{reference!r}"
                    )
                continue
            raise ValueError(
                f"{source}: local actions must live under .github/actions; "
                f"rejected {reference!r}"
            )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-root", type=Path, default=ROOT)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repository_root = args.repository_root.resolve()
    try:
        validate_local_action_policy(repository_root, tracked_paths(repository_root))
    except (OSError, UnicodeError, ValueError) as error:
        print(f"local-action-policy: {error}", file=sys.stderr)
        return 1
    print("local-action-policy-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
