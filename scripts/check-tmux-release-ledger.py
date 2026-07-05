#!/usr/bin/env python3
"""Validate the RMUX 0.9.0 tmux divergence ledger and inventory boundary."""

from __future__ import annotations

import re
import sys
from pathlib import Path


LEDGER = Path("docs/compat/tmux-3.7-divergences.md")
COMMAND_INVENTORY = Path("src/cli/command_inventory.rs")
OPTIONS_REGISTRY = Path("crates/rmux-core/src/options/registry.rs")


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
