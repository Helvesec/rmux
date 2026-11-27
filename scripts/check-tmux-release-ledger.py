#!/usr/bin/env python3
"""Validate the structured tmux compatibility divergence allowlist."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import toml_reader  # noqa: E402  (path bootstrap must run first)


LEDGER = Path("tests/reference/tmux_compat/divergences.toml")
COMMAND_INVENTORY = Path("crates/rmux-core/src/command_inventory/signatures.rs")
PRODUCT_DIVERGENCE_TEST = re.compile(
    r"(?m)^\s*(?:async\s+)?fn\s+([A-Za-z][A-Za-z0-9_]*_product_divergence)\s*\("
)
PRODUCT_DIVERGENCE_SUFFIX = "_product_divergence"
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
    return (
        subprocess.run(
            ["git", "ls-files", "--error-unmatch", "--", str(path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        ).returncode
        == 0
    )


def tracked_sources() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files"],
        check=True,
        capture_output=True,
        text=True,
    )
    return [Path(line) for line in result.stdout.splitlines() if line]


def product_divergence_tests() -> dict[str, Path]:
    tests: dict[str, Path] = {}
    for path in tracked_sources():
        if path.suffix == ".rs":
            names = PRODUCT_DIVERGENCE_TEST.findall(path.read_text(encoding="utf-8"))
        elif path.stem.endswith(PRODUCT_DIVERGENCE_SUFFIX):
            names = [path.stem]
        else:
            continue
        for name in names:
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
        raise ValueError(
            f"{entry.get('id', '<unknown>')}: {field} must be a string list"
        )
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
    payload = toml_reader.loads(LEDGER.read_text(encoding="utf-8"))
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

        references = require_string_list(raw, "evidence") + require_string_list(
            raw, "tests"
        )
        if not references:
            raise ValueError(
                f"{entry_id}: at least one evidence or test reference is required"
            )
        for reference in references:
            validate_reference(reference)
        entries[entry_id] = raw

    return payload, entries


def validate_product_tests(
    entries: dict[str, dict[str, object]],
) -> tuple[int, str | None]:
    try:
        discovered = product_divergence_tests()
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        return 0, f"cannot inventory tracked product-divergence tests: {error}"

    references: dict[str, list[tuple[str, Path]]] = {}
    for entry_id, entry in entries.items():
        for reference in require_string_list(entry, "tests"):
            path, symbol = validate_reference(reference)
            if symbol is not None and symbol.endswith(PRODUCT_DIVERGENCE_SUFFIX):
                name = symbol
            elif symbol is None and path.stem.endswith(PRODUCT_DIVERGENCE_SUFFIX):
                name = path.stem
            else:
                continue
            references.setdefault(name, []).append((entry_id, path))

    for name, path in discovered.items():
        matches = references.get(name, [])
        if not matches:
            return 0, f"{LEDGER}: tracked test {path}::{name} has no allowlist entry"
        if len(matches) != 1:
            ids = ", ".join(entry_id for entry_id, _ in matches)
            return (
                0,
                f"{LEDGER}: tracked test {name} is cited by multiple entries: {ids}",
            )
        entry_id, cited_path = matches[0]
        if cited_path != path:
            return (
                0,
                f"{LEDGER}: {entry_id} cites {name} from {cited_path}, expected {path}",
            )

    stale = sorted(set(references).difference(discovered))
    if stale:
        return 0, f"{LEDGER}: stale product-divergence references: {', '.join(stale)}"
    return len(discovered), None


def _blank_trivia(text: str) -> str:
    # Keep source offsets and line boundaries for exhaustive matching/diagnostics.
    return "".join(character if character in "\r\n" else " " for character in text)


_RUST_STRING = re.compile(r'"(?:\\[\s\S]|[^"\\])*"')
_INVENTORY_LINT_ATTRIBUTE = re.compile(
    r"#\[\s*(?:allow|warn|deny|forbid)\s*\(\s*"
    r"[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*"
    r"(?:\s*,\s*[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)*\s*,?\s*\)\s*\]"
)


def normalize_inventory_source(source: str) -> str:
    """Mask lexical trivia, not strings or code; this is not a Rust parser.

    Only lint-level and rustfmt::skip outer attributes are inert for this
    literal inventory. Conditional and unknown attributes fail closed.
    """
    masked = list(source)
    position = 0
    while position < len(source):
        if source[position] == '"':
            string = _RUST_STRING.match(source, position)
            if string is None:
                raise ValueError("unterminated inventory string")
            position = string.end()
            continue
        if re.match(r'(?:br|cr|r)#*"', source[position:]):
            raise ValueError("unsupported raw inventory string")
        start = position
        if source.startswith("//", position):
            end = source.find("\n", position)
            position = len(source) if end == -1 else end
        elif source.startswith("/*", position):
            depth = 1
            position += 2
            while position < len(source) and depth:
                if source.startswith("/*", position):
                    depth += 1
                    position += 2
                elif source.startswith("*/", position):
                    depth -= 1
                    position += 2
                else:
                    position += 1
            if depth:
                raise ValueError("unterminated inventory block comment")
        else:
            position += 1
            continue
        masked[start:position] = _blank_trivia(source[start:position])

    normalized = "".join(masked)
    # Strings are consumed as whole tokens, so attribute-looking usage text
    # cannot be mistaken for an annotation.
    for token in re.finditer(_RUST_STRING.pattern + r"|#\[[^\]]*\]", normalized):
        attribute = token.group()
        if attribute.startswith('"'):
            continue
        if not (
            _INVENTORY_LINT_ATTRIBUTE.fullmatch(attribute)
            or re.fullmatch(r"#\[\s*rustfmt::skip\s*\]", attribute)
        ):
            raise ValueError("unsupported inventory attribute; conditional or unknown annotation")
        masked[token.start():token.end()] = _blank_trivia(attribute)
    return "".join(masked)


def pane_inventory_entries(inventory: str) -> list[tuple[str, str]]:
    """Consume the complete literal table, or reject its unsupported format.

    This intentionally accepts only the declaration on its own line and one
    tuple per logical line (tuples may span lines). No search may skip an
    unrecognized entry and turn partial extraction into evidence of absence.
    """
    inventory = normalize_inventory_source(inventory)
    code = _RUST_STRING.sub(lambda match: _blank_trivia(match.group()), inventory)
    if len(re.findall(r"\bconst\s+LIST_COMMAND_SIGNATURES\b", code)) != 1:
        raise ValueError("expected exactly one LIST_COMMAND_SIGNATURES declaration")
    declaration = re.search(
        r"(?m)^[ \t]*pub\s*\(\s*super\s*\)\s+const\s+LIST_COMMAND_SIGNATURES\s*:\s*"
        r"&\s*\[\s*\(\s*&\s*str\s*,\s*&\s*str\s*\)\s*\]\s*=\s*&\s*\[[ \t]*\r?\n"
        r"(?P<body>[\s\S]*)^[ \t]*\]\s*;[ \t\r\n]*\Z",
        inventory,
    )
    if declaration is None:
        raise ValueError("unsupported declaration layout; opening and closing delimiters need their own lines")
    # Trivia has been masked; wrappers/macros remain visible and unsupported.
    prologue = inventory[:declaration.start()].strip()
    lookup = """pub(super) fn list_command_usage(name: &str) -> &'static str {
    LIST_COMMAND_SIGNATURES
        .iter()
        .find_map(|(command_name, usage)| (*command_name == name).then_some(*usage))
        .unwrap_or("")
}"""
    def tokens(text: str) -> list[str]:
        return re.findall(_RUST_STRING.pattern + r"|\S", text)

    block_lookup = lookup.replace(
        "|(command_name, usage)| (*command_name == name).then_some(*usage)",
        "|(command_name, usage)| { (*command_name == name).then_some(*usage) }",
    )
    if tokens(prologue) not in ([], tokens(lookup), tokens(block_lookup)):
        raise ValueError("unsupported inventory prologue; only the literal table and known lookup helper are supported")
    body = declaration.group("body")
    # Rust ordinary strings only; unsupported escapes, remaining attributes,
    # expressions and macro entries fail closed rather than being skipped.
    entry = re.compile(
        r'\s*\(\s*"([a-z][a-z0-9-]*)"\s*,\s*'
        r'"((?:[^"\\\r\n]|\\["\\nrt0])*)"\s*,?\s*\)\s*,[ \t]*(?:\r?\n|\Z)'
    )
    entries: list[tuple[str, str]] = []
    escapes = {'"': '"', "\\": "\\", "n": "\n", "r": "\r", "t": "\t", "0": "\0"}
    position = 0
    while body[position:].strip():
        match = entry.match(body, position)
        if match is None:
            line = inventory.count("\n", 0, declaration.start("body") + position) + 1
            raise ValueError(f"unsupported or incomplete entry near line {line}; expected one tuple per logical line")
        usage = re.sub(r'\\(["\\nrt0])', lambda escape: escapes[escape.group(1)], match.group(2))
        entries.append((match.group(1), usage))
        position = match.end()
    return entries


def pane_inventory_names(inventory: str) -> set[str]:
    return {name for name, _usage in pane_inventory_entries(inventory)}


def validate_pane_inventory(
    entries: dict[str, dict[str, object]], inventory: str
) -> str | None:
    entry = entries.get("C-D32", {})
    classification = entry.get("inventory")
    deferred = require_string_list(entry, "deferred")
    try:
        names = pane_inventory_names(inventory)
    except ValueError as error:
        return f"{COMMAND_INVENTORY}: inventory extraction failed: {error}"
    # floating-pane is the historical feature name; its public placement
    # command is set-pane-placement.
    required = {"new-pane", "set-pane-placement"}
    whole_commands = required | {"floating-pane", "newp"}
    if classification == "deferred":
        advertised = sorted(names & whole_commands)
        if advertised:
            return f"{COMMAND_INVENTORY}: deferred pane commands are advertised: {advertised}"
        if not {"floating-pane", "new-pane"}.issubset(deferred):
            return f"{LEDGER}: C-D32 must defer floating-pane and new-pane"
    elif classification == "bounded":
        missing = sorted(required - names)
        if missing:
            return f"{COMMAND_INVENTORY}: bounded C-D32 commands are missing: {missing}"
        whole_deferred = sorted({item.strip() for item in deferred} & whole_commands)
        if whole_deferred:
            return (
                f"{LEDGER}: bounded C-D32 may defer only subfeatures, "
                f"not whole commands: {whole_deferred}"
            )
    else:
        return f"{LEDGER}: C-D32 inventory must be bounded or deferred"
    return None


def main() -> int:
    try:
        payload, entries = load_entries()
    except (OSError, toml_reader.TOMLDecodeError, ValueError) as error:
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
    error = validate_pane_inventory(entries, inventory)
    if error is not None:
        return fail(error)

    print(
        "tmux-release-ledger=ok "
        f"entries={len(entries)} product_divergence_tests={product_count}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
