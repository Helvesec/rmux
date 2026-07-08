#!/usr/bin/env python3
"""Validate the RMUX 0.9.0 tmux divergence ledger and inventory boundary."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path


LEDGER = Path("docs/compat/tmux-3.7-divergences.md")
COMMAND_INVENTORY = Path("src/cli/command_inventory.rs")
OPTIONS_REGISTRY = Path("crates/rmux-core/src/options/registry.rs")
GITIGNORED_AUDIT_REFERENCE = ".rmux-audit/"
TRACKED_REFERENCE_PREFIXES = (
    "benches/",
    "crates/",
    "docs/",
    "scripts/",
    "src/",
    "tests/",
)


def backticked_path_references(block: str) -> list[str]:
    paths: list[str] = []
    for reference in re.findall(r"`([^`]+)`", block):
        path_text = reference.split("::", 1)[0].strip()
        candidate = path_text.split(None, 1)[0] if path_text else ""
        if "..." in candidate:
            # Ellipses are fine in quoted transcript prose, but an ellipsized
            # citation inside the auditable evidence namespace cannot be
            # existence-checked and must not silently bypass the gate.
            if candidate.startswith(TRACKED_REFERENCE_PREFIXES):
                raise ValueError(
                    f"ellipsized ledger evidence citation is not auditable: {candidate}"
                )
            continue
        if looks_like_path_reference(candidate):
            paths.append(candidate)
    return paths


def looks_like_path_reference(path_text: str) -> bool:
    if path_text.startswith(("/", "./", "../")):
        return True
    if path_text.startswith(TRACKED_REFERENCE_PREFIXES):
        return True
    return "/" in path_text and not path_text.startswith(("#", "#{"))


def tracked_reference_path(path_text: str) -> Path:
    if path_text.startswith(("/", "./", "../")):
        raise ValueError(f"path must be repo-relative and tracked: {path_text}")
    if not path_text.startswith(TRACKED_REFERENCE_PREFIXES):
        raise ValueError(
            f"path is outside tracked ledger evidence prefixes {TRACKED_REFERENCE_PREFIXES}: {path_text}"
        )
    return Path(path_text)


def git_tracks(path: Path) -> bool:
    return subprocess.run(
        ["git", "ls-files", "--error-unmatch", "--", str(path)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    ).returncode == 0


def fail(message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    return 1


def ledger_entries(text: str) -> list[tuple[str, str]]:
    matches = list(re.finditer(r"^### (C-D\d+): .*$", text, re.MULTILINE))
    entries: list[tuple[str, str]] = []
    for index, match in enumerate(matches):
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        entries.append((match.group(1), text[match.start() : end]))
    return entries


def require_entry(entries: dict[str, str], entry_id: str) -> str:
    try:
        return entries[entry_id]
    except KeyError:
        raise AssertionError(f"missing ledger entry {entry_id}") from None


def main() -> int:
    ledger = LEDGER.read_text(encoding="utf-8")
    inventory = COMMAND_INVENTORY.read_text(encoding="utf-8")
    options = OPTIONS_REGISTRY.read_text(encoding="utf-8")

    if "only allowlist source" not in ledger:
        return fail(f"{LEDGER}: missing allowlist-source policy")
    if "Unlisted divergences found by differential tests are bugs" not in ledger:
        return fail(f"{LEDGER}: missing unlisted-divergence policy")

    entries = dict(ledger_entries(ledger))
    if not entries:
        return fail(f"{LEDGER}: no C-D ledger entries found")

    for entry_id, block in entries.items():
        if "Test/fixture:" not in block:
            return fail(f"{LEDGER}: {entry_id} has no Test/fixture line")
        if GITIGNORED_AUDIT_REFERENCE in block:
            return fail(
                f"{LEDGER}: {entry_id} cites gitignored {GITIGNORED_AUDIT_REFERENCE}; "
                "ledger evidence must be tracked"
            )
        try:
            references = backticked_path_references(block)
        except ValueError as error:
            return fail(f"{LEDGER}: {entry_id} cites invalid ledger evidence path: {error}")
        for path_text in references:
            try:
                path = tracked_reference_path(path_text)
            except ValueError as error:
                return fail(f"{LEDGER}: {entry_id} cites invalid ledger evidence path: {error}")
            if not path.exists():
                return fail(f"{LEDGER}: {entry_id} cites missing tracked fixture {path}")
            if not git_tracks(path):
                return fail(f"{LEDGER}: {entry_id} cites untracked fixture {path}")
        if "Inventory impact:" not in block:
            return fail(f"{LEDGER}: {entry_id} has no Inventory impact line")

    try:
        deferred_floating = require_entry(entries, "C-D32")
        copy_line_numbers = require_entry(entries, "C-D34")
    except AssertionError as error:
        return fail(str(error))

    if "`new-pane`" not in deferred_floating or "floating-pane" not in deferred_floating:
        return fail(f"{LEDGER}: C-D32 must document both floating-pane and new-pane")
    if re.search(r'"new-pane"', inventory) is not None:
        return fail(f"{COMMAND_INVENTORY}: new-pane is advertised but C-D32 defers it")
    if "floating" in inventory:
        return fail(f"{COMMAND_INVENTORY}: floating panes are advertised but C-D32 defers them")

    if "copy-mode-line-numbers" in options:
        if "copy-mode-line-numbers" not in copy_line_numbers:
            return fail(f"{LEDGER}: C-D34 must mention copy-mode-line-numbers")
        if "line-number gutter rendering" not in copy_line_numbers:
            return fail(f"{LEDGER}: C-D34 must bound line-number gutter rendering claims")

    print(f"tmux-release-ledger=ok entries={len(entries)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
