#!/usr/bin/env python3
"""Validate the structured tmux compatibility divergence allowlist."""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from pathlib import Path


LEDGER = Path("tests/reference/tmux_compat/divergences.toml")
COMMAND_INVENTORY = Path("crates/rmux-core/src/command_inventory/signatures.rs")
OPTIONS_REGISTRY = Path("crates/rmux-core/src/options/table.rs")
PRODUCT_DIVERGENCE_TEST = re.compile(
    r"(?m)^\s*(?:async\s+)?fn\s+([A-Za-z][A-Za-z0-9_]*_product_divergence)\s*\("
)
ENTRY_ID = re.compile(r"C-D[1-9][0-9]*")
FORBIDDEN_PLANNING_LABEL = re.compile(r"\b(?:lot|round|step)\s*[0-9]+\b|finale", re.I)
TRACKED_REFERENCE_PREFIXES = (
    "benches/",
    "crates/",
    "scripts/",
    "src/",
    "tests/",
)


def fail(message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    return 1


def git_tracks(path: Path) -> bool:
    return subprocess.run(
        ["git", "ls-files", "--error-unmatch", "--", str(path)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    ).returncode == 0


def tracked_rust_sources() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "--", "*.rs"],
        check=True,
        capture_output=True,
        text=True,
    )
    return [Path(line) for line in result.stdout.splitlines() if line]


def product_divergence_tests() -> dict[str, Path]:
    tests: dict[str, Path] = {}
    for path in tracked_rust_sources():
        source = path.read_text(encoding="utf-8")
        for name in PRODUCT_DIVERGENCE_TEST.findall(source):
            previous = tests.get(name)
            if previous is not None:
                raise ValueError(
                    f"duplicate product-divergence test {name}: {previous} and {path}"
                )
            tests[name] = path
    return tests


def require_string_list(entry: dict[str, object], field: str) -> list[str]:
    value = entry.get(field, [])
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError(f"{entry.get('id', '<unknown>')}: {field} must be a string list")
    return value


def validate_reference(reference: str) -> tuple[Path, str | None]:
    path_text, separator, symbol = reference.partition("::")
    if not path_text.startswith(TRACKED_REFERENCE_PREFIXES):
        raise ValueError(
            f"reference must use a tracked evidence prefix {TRACKED_REFERENCE_PREFIXES}: "
            f"{reference}"
        )
    path = Path(path_text)
    if not path.exists():
        raise ValueError(f"reference points to a missing path: {path}")
    if not git_tracks(path):
        raise ValueError(f"reference points to an untracked path: {path}")
    return path, symbol if separator else None


def load_entries() -> tuple[dict[str, object], dict[str, dict[str, object]]]:
    payload = tomllib.loads(LEDGER.read_text(encoding="utf-8"))
    raw_entries = payload.get("entry")
    if not isinstance(raw_entries, list) or not raw_entries:
        raise ValueError("entry must be a non-empty array of tables")

    entries: dict[str, dict[str, object]] = {}
    for raw in raw_entries:
        if not isinstance(raw, dict):
            raise ValueError("each entry must be a table")
        entry_id = raw.get("id")
        summary = raw.get("summary")
        inventory = raw.get("inventory")
        if not isinstance(entry_id, str) or ENTRY_ID.fullmatch(entry_id) is None:
            raise ValueError(f"invalid entry id: {entry_id!r}")
        if entry_id in entries:
            raise ValueError(f"duplicate entry id: {entry_id}")
        if not isinstance(summary, str) or not summary.strip():
            raise ValueError(f"{entry_id}: summary must be a non-empty string")
        if FORBIDDEN_PLANNING_LABEL.search(summary):
            raise ValueError(f"{entry_id}: summary contains an internal planning label")
        if inventory not in {"none", "bounded", "deferred"}:
            raise ValueError(f"{entry_id}: invalid inventory value {inventory!r}")

        references = require_string_list(raw, "evidence") + require_string_list(raw, "tests")
        if not references:
            raise ValueError(f"{entry_id}: at least one evidence or test reference is required")
        for reference in references:
            validate_reference(reference)
        entries[entry_id] = raw

    return payload, entries


def validate_product_tests(entries: dict[str, dict[str, object]]) -> tuple[int, str | None]:
    try:
        discovered = product_divergence_tests()
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        return 0, f"cannot inventory tracked product-divergence tests: {error}"

    references: dict[str, list[tuple[str, Path]]] = {}
    for entry_id, entry in entries.items():
        for reference in require_string_list(entry, "tests"):
            path, symbol = validate_reference(reference)
            if symbol is None or not symbol.endswith("_product_divergence"):
                continue
            references.setdefault(symbol, []).append((entry_id, path))

    for name, path in discovered.items():
        matches = references.get(name, [])
        if not matches:
            return 0, f"{LEDGER}: tracked test {path}::{name} has no allowlist entry"
        if len(matches) != 1:
            ids = ", ".join(entry_id for entry_id, _ in matches)
            return 0, f"{LEDGER}: tracked test {name} is cited by multiple entries: {ids}"
        entry_id, cited_path = matches[0]
        if cited_path != path:
            return 0, f"{LEDGER}: {entry_id} cites {name} from {cited_path}, expected {path}"

    stale = sorted(set(references).difference(discovered))
    if stale:
        return 0, f"{LEDGER}: stale product-divergence references: {', '.join(stale)}"
    return len(discovered), None


def main() -> int:
    try:
        payload, entries = load_entries()
    except (OSError, tomllib.TOMLDecodeError, ValueError) as error:
        return fail(f"{LEDGER}: {error}")

    policy = payload.get("policy")
    if not isinstance(policy, dict):
        return fail(f"{LEDGER}: missing policy table")
    if policy.get("oracle") != "tmux 3.7b":
        return fail(f"{LEDGER}: oracle must be tmux 3.7b")
    if policy.get("unlisted_divergences_are_bugs") is not True:
        return fail(f"{LEDGER}: unlisted divergences must be classified as bugs")

    product_count, error = validate_product_tests(entries)
    if error is not None:
        return fail(error)

    inventory = COMMAND_INVENTORY.read_text(encoding="utf-8")
    options = OPTIONS_REGISTRY.read_text(encoding="utf-8")
    deferred = require_string_list(entries.get("C-D32", {}), "deferred")
    if not {"floating-pane", "new-pane"}.issubset(deferred):
        return fail(f"{LEDGER}: C-D32 must defer floating-pane and new-pane")
    if re.search(r'"new-pane"', inventory) is not None or "floating" in inventory:
        return fail(f"{COMMAND_INVENTORY}: deferred pane commands are advertised")

    copy_entry = entries.get("C-D34", {})
    accepted_options = require_string_list(copy_entry, "accepted_options")
    limitations = require_string_list(copy_entry, "limitations")
    if "copy-mode-line-numbers" in options:
        if "copy-mode-line-numbers" not in accepted_options:
            return fail(f"{LEDGER}: C-D34 must cover copy-mode-line-numbers")
        if "line-number-gutter-rendering" not in limitations:
            return fail(f"{LEDGER}: C-D34 must bound line-number gutter rendering")

    print(
        "tmux-release-ledger=ok "
        f"entries={len(entries)} product_divergence_tests={product_count}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
