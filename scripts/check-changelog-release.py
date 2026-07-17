#!/usr/bin/env python3
"""Validate release-facing CHANGELOG claims for the current RMUX release."""

from __future__ import annotations

import re
import sys
from pathlib import Path


TMUX_CLAIM = re.compile(r"\bmatches\s+tmux\b", re.IGNORECASE)
FIXTURE_LINK = re.compile(r"\[[^\]]+\]\(((?:tests/|crates/|scripts/|docs/)[^)]+)\)")


def fail(message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    return 1


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

    for release in ("## 0.9.0", "## 0.8.0", "## 0.7.1", "## 0.7.0"):
        if release not in text:
            return fail(f"{path}: missing changelog section {release}")

    for line_number, bullet in changelog_bullets(text):
        if TMUX_CLAIM.search(bullet) and FIXTURE_LINK.search(bullet) is None:
            return fail(
                f"{path}:{line_number}: tmux compatibility claim lacks a fixture/test link"
            )

    for line_number, line in enumerate(text.splitlines(), start=1):
        for match in FIXTURE_LINK.finditer(line):
            target = match.group(1).split("#", 1)[0]
            if not Path(target).exists():
                return fail(f"{path}:{line_number}: changelog link target does not exist: {target}")

    print(f"changelog={path} tmux-claims=linked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
