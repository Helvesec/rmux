#!/usr/bin/env python3
"""Validate release-facing CHANGELOG claims for the current RMUX release."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


TMUX_CLAIM = re.compile(r"\bmatches\s+tmux\b", re.IGNORECASE)
FIXTURE_LINK = re.compile(r"\[[^\]]+\]\(((?:tests/|crates/|scripts/|docs/)[^)]+)\)")
RELEASE_HEADING = re.compile(r"^## ([^\r\n]+)\r?$", re.MULTILINE)
REPO_ROOT = Path(__file__).resolve().parent.parent


def fail(message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    return 1


def workspace_release_heading() -> str:
    cargo_toml = REPO_ROOT / "Cargo.toml"
    with cargo_toml.open("rb") as handle:
        manifest = tomllib.load(handle)
    try:
        version = manifest["workspace"]["package"]["version"]
    except (KeyError, TypeError) as error:
        raise ValueError(f"{cargo_toml}: missing workspace.package.version") from error
    if not isinstance(version, str) or not version:
        raise ValueError(f"{cargo_toml}: invalid workspace.package.version")
    return f"## {version}"


def changelog_bullets(text: str) -> list[tuple[int, str]]:
    bullets: list[tuple[int, str]] = []
    current_line = 0
    current: list[str] = []
    for line_number, line in enumerate(text.splitlines(), start=1):
        if line.startswith("- "):
            if current:
                bullets.append((current_line, "\n".join(current)))
            current_line = line_number
            current = [line]
        elif current and (line.startswith("  ") or line == ""):
            current.append(line)
        else:
            if current:
                bullets.append((current_line, "\n".join(current)))
                current = []
                current_line = 0
    if current:
        bullets.append((current_line, "\n".join(current)))
    return bullets


def main(argv: list[str]) -> int:
    path = Path(argv[1]) if len(argv) > 1 else Path("CHANGELOG.md")
    text = path.read_text(encoding="utf-8")

    try:
        current_release = workspace_release_heading()
    except ValueError as error:
        return fail(str(error))
    current_version = current_release.removeprefix("## ")
    required_versions = dict.fromkeys(
        (current_version, "0.9.0", "0.8.0", "0.7.1", "0.7.0")
    )
    headings = RELEASE_HEADING.findall(text)
    if not headings or headings[0] != current_version:
        actual = headings[0] if headings else "<none>"
        return fail(
            f"{path}: first release section must be ## {current_version}, found ## {actual}"
        )
    for version in required_versions:
        count = headings.count(version)
        if count == 0:
            return fail(f"{path}: missing changelog section ## {version}")
        if count != 1:
            return fail(f"{path}: duplicate changelog section ## {version}")

    for line_number, bullet in changelog_bullets(text):
        if TMUX_CLAIM.search(bullet) and FIXTURE_LINK.search(bullet) is None:
            return fail(
                f"{path}:{line_number}: tmux compatibility claim lacks a fixture/test link"
            )

    for line_number, line in enumerate(text.splitlines(), start=1):
        for match in FIXTURE_LINK.finditer(line):
            target = match.group(1).split("#", 1)[0]
            if not Path(target).exists():
                return fail(
                    f"{path}:{line_number}: changelog link target does not exist: {target}"
                )

    print(f"changelog={path} release={current_version} tmux-claims=linked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
