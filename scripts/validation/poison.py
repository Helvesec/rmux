"""Conservative poison witnesses and a ratchet; not a Rust type checker."""
import hashlib
from pathlib import Path
import re


def require(condition, message):
    if not condition:
        raise ValueError(message)


def witnesses(text):
    """Preserve source offsets. Comments are ignored, unknown into_inner stays debt.

    The inventory deliberately over-approximates recovery: an into_inner call
    may consume a non-lock wrapper. Such a candidate is not a confirmed defect.
    It cannot disappear from this inventory without a reviewed disposition.
    """
    sites = []
    pattern = re.compile(r'\b(?:into_inner|clear_poison|is_poisoned)\s*\(')
    for match in pattern.finditer(text):
        line = text.count("\n", 0, match.start()) + 1
        start = text.rfind("\n", 0, match.start()) + 1
        raw = text[start:text.find("\n", match.end()) if "\n" in text[match.end():] else len(text)]
        if raw.lstrip().startswith(("//", "*")):
            continue
        kind = "recovery-candidate" if match.group().startswith("into_inner") else "poison-state-operation"
        sites.append({"line": line, "kind": kind, "text_sha256": hashlib.sha256(raw.strip().encode()).hexdigest()})
    return sites


def scan(root):
    result = []
    for base in ("src", "crates", "tests"):
        for path in sorted((root / base).rglob("*.rs")):
            raw = path.read_bytes()
            text = raw.decode()
            file_sha = hashlib.sha256(raw).hexdigest()
            test_module = text.find("#[cfg(test)]")
            for site in witnesses(text):
                offset = sum(len(line) + 1 for line in text.splitlines()[:site["line"] - 1])
                result.append({"path": path.relative_to(root).as_posix(), "source_sha256": file_sha,
                               **site, "scope_hint": "test-or-conditional" if
                               base == "tests" or "/tests" in path.as_posix() or
                               (test_module >= 0 and offset > test_module) else "production-candidate",
                               "disposition": "REVIEW_REQUIRED_NOT_AN_APPROVED_RECOVERY"})
    return result


def validate_policy(root, policy):
    require(policy["schema"] == 1 and policy["default"] == "fail-closed-before-state-use", "poison default")
    require(policy["scope"] == ["std::sync::Mutex", "std::sync::RwLock", "Condvar-reacquisition"],
            "poison scope changed")
    require(policy["silent_recovery_allowed"] is False, "silent recovery")
    require(policy["clear_poison_is_repair"] is False, "clear_poison is not invariant repair")
    require(policy["async_mutex_rule"] == "no-poison-mechanism; cancellation-invariants-still-required",
            "async mutex is not an implicit recovery exception")
    require(policy["observed_sites"] == scan(root), "poison inventory drift/new or silently removed witness")
    # v1 approves none. A future exception requires a reviewed gate/policy delta,
    # not just author-supplied text shaped like an approval receipt.
    require(policy["approved_exceptions"] == [], "unreviewed poison exception; v1 approves none")
    require(policy["product_policy_conformance"] == "OPEN_NOT_ESTABLISHED", "source inventory is not enforcement proof")
    require(policy["positive_control"]["path"] == "crates/rmux-client/src/attach_lock_state.rs",
            "existing poison test owner")
    control = root / policy["positive_control"]["path"]
    require(hashlib.sha256(control.read_bytes()).hexdigest() == policy["positive_control"]["sha256"],
            "stale poison positive control")
    text = control.read_text()
    for name in policy["positive_control"]["tests"]:
        require("fn " + name + "(" in text, "missing existing poison canary")
    return len(policy["observed_sites"])
