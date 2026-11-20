"""Lexical queue creation census; comments, literals and test bodies are excluded."""
import re
from functools import lru_cache
from pathlib import Path

from module_tree import cfg_test_attributes, test_scopes

WRAPPERS = (
    "OutputRing|RecentLiveBuffer|PaneStateJournal|PaneRenameQueue|"
    "DeferredPassthroughQueue|ControlOutputQueue|LifecycleDispatchOutbox|"
    "InputOperationQueue|AttachOutputQueue|AppliedWindowResizeQueue|"
    "CommandQueue|BufferStore|Transcript"
)
CALL = re.compile(
    r"\b(?P<callee>(?:[A-Za-z_]\w*\s*::\s*)*"
    r"(?:channel|sync_channel|unbounded_channel|bounded|unbounded|duplex))"
    r"\s*(?:::\s*<[^;{}]*?>\s*)?\("
    r"|\b(?P<deque>(?:std\s*::\s*collections\s*::\s*)?VecDeque\s*::\s*"
    r"(?:new|default|with_capacity|from))\s*(?:::\s*<[^;{}]*?>\s*)?\("
    r"|\b(?P<wrapper>(?:" + WRAPPERS + r")\s*::\s*"
    r"(?:new|default|with_capacity|with_limits|with_default_capacities|"
    r"with_limits_and_byte_capacity|spawn|spawn_with_fence_flush|from_parsed))"
    r"\s*(?:::\s*<[^;{}]*?>\s*)?\("
)
DECL = re.compile(r"\b(?P<field>\w+)\s*:\s*(?:std\s*::\s*collections\s*::\s*)?VecDeque\s*<")


def require(condition, message):
    if not condition:
        raise ValueError(message)

@lru_cache(maxsize=256)
def mask_rust(text):
    """Blank comments and literals while preserving offsets/newlines; keep lifetimes."""
    out = list(text)
    i = 0
    while i < len(text):
        end = None
        if text.startswith("//", i):
            end = text.find("\n", i)
            if end < 0:
                end = len(text)
        elif text.startswith("/*", i):
            depth, end = 1, i + 2
            while depth and end < len(text):
                if text.startswith("/*", end):
                    depth, end = depth + 1, end + 2
                elif text.startswith("*/", end):
                    depth, end = depth - 1, end + 2
                else:
                    end += 1
            require(depth == 0, "unterminated block comment")
        else:
            raw = re.match(r'(?:br|cr|r)(#*)"', text[i:]) if text[i] in "brc" else None
            if raw and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
                marker = '"' + raw.group(1)
                finish = text.find(marker, i + len(raw.group()))
                require(finish >= 0, "unterminated raw string")
                end = finish + len(marker)
            elif text[i] == '"':
                end = i + 1
                while end < len(text):
                    if text[end] == "\\":
                        end += 2
                    elif text[end] == '"':
                        end += 1
                        break
                    else:
                        end += 1
            elif text[i] == "'":
                char = re.match(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'", text[i:])
                if char:
                    end = i + len(char.group())
        if end is None:
            i += 1
        else:
            for j in range(i, end):
                if out[j] != "\n":
                    out[j] = " "
            i = end
    return "".join(out)


def closing(text, start, opening="(", ending=")"):
    depth = 0
    for i in range(start, len(text)):
        if text[i] == opening:
            depth += 1
        elif text[i] == ending:
            depth -= 1
            if depth == 0:
                return i + 1
    raise ValueError("unbalanced source delimiters")


def test_ranges(masked):
    ranges = []
    # any(test, windows) is deliberately NOT exempted: it also compiles in product.
    attr = re.compile(r"#\s*\[\s*(?:tokio::)?test(?:\([^\]]*\))?\s*\]")
    starts = sorted([(m.start(), m.end()) for m in attr.finditer(masked)] +
                    [(a, b) for a, b, inner in cfg_test_attributes(masked) if not inner])
    for start, pos in starts:
        # Walk stacked attributes; their braces are not item bodies.
        while True:
            a = re.match(r"\s*#\s*\[", masked[pos:])
            if not a:
                break
            pos = closing(masked, pos + a.end() - 1, "[", "]")
        tail = masked[pos:]
        # Attributes on a field/use/expression must not absorb the next impl.
        prefix = r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?"
        if re.match(prefix + r"(?:use|type|const|static)\b", tail):
            end = masked.find(";", pos) + 1
        elif re.match(prefix + r"(?:fn|mod|impl|struct|enum|union)\b", tail) or tail.lstrip().startswith("{"):
            brace, semi = masked.find("{", pos), masked.find(";", pos)
            if semi >= 0 and (brace < 0 or semi < brace):
                end = semi + 1
            elif brace >= 0:
                end = closing(masked, brace, "{", "}")
            else:
                continue
        else:
            # A test-only field cannot contain a product queue constructor.
            boundary = re.search(r"[,;\n}]", tail.lstrip())
            end = pos + len(tail) - len(tail.lstrip()) + (boundary.end() if boundary else 0)
        ranges.append((start, end))
    return ranges


def test_file(path):
    parts = Path(path).parts
    return (any(x in ("tests", "benches", "examples", "fixtures") for x in parts)
            or any(x.endswith("_tests") for x in parts)
            or Path(path).stem in ("test", "tests")
            or Path(path).stem.endswith(("_test", "_tests", "_spec", "_test_support")))


@lru_cache(maxsize=256)
def test_symbols(text):
    return re.findall(
        r"#\s*\[\s*(?:tokio::)?test(?:\([^\]]*\))?\s*\]\s*"
        r"(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)",
        mask_rust(text))


def scan_file(path, text, test_source=None, masked=None, ranges=None):
    """`test_source` is the module-tree scope; None falls back to the path name."""
    masked = mask_rust(text) if masked is None else masked
    test_source = test_file(path) if test_source is None else test_source
    ranges = test_ranges(masked) if ranges is None else ranges
    sites = []
    for pattern, fallback in ((CALL, None), (DECL, "deque-declaration")):
        for m in pattern.finditer(masked):
            kind = fallback or ("channel" if m.group("callee") else "deque" if m.group("deque") else "wrapper")
            if kind == "channel":
                # Bare function definitions are not calls; imported bare calls are.
                if re.search(r"\bfn\s+$", masked[max(0, m.start() - 8):m.start()]):
                    continue
            end = m.end()
            if kind != "deque-declaration":
                end = closing(masked, m.end() - 1)
            line = text.count("\n", 0, m.start()) + 1
            column = m.start() - text.rfind("\n", 0, m.start())
            site = {
                "id": f"{path}:{line}:{column}:{kind}", "path": path,
                "line": line, "column": column, "kind": kind,
                "expression": text[m.start():end],
                "test_only": test_source or any(a <= m.start() < b for a, b in ranges),
            }
            sites.append(site)
    return sorted(sites, key=lambda x: (x["line"], x["column"], x["kind"]))


def scan_sources(sources, supplementary=()):
    masked = {path: mask_rust.__wrapped__(text) for path, text in sources.items()}
    ranges = {path: test_ranges(text) for path, text in masked.items()}
    scopes = test_scopes(sources, masked, ranges, test_file)
    sites = [site for path, text in sorted(sources.items())
             for site in scan_file(path, text, scopes.get(path), masked[path], ranges[path])]
    for spec in supplementary:
        path, anchor = spec["path"], spec["anchor"]
        require(path in sources and sources[path].count(anchor) == 1,
                f"supplementary anchor missing/ambiguous: {spec['name']}")
        text = sources[path]
        pos = text.index(anchor)
        line = text.count("\n", 0, pos) + 1
        column = pos - text.rfind("\n", 0, pos)
        sites.append({"id": f"{path}:{line}:{column}:store", "path": path,
                      "line": line, "column": column, "kind": "store",
                      "expression": anchor, "test_only": False})
    return sorted(sites, key=lambda s: s["id"])
