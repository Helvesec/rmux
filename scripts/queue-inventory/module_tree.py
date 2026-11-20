"""Follow `mod x;` and `#[path]` declarations from crate roots and propagate cfg(test).

A file is test-only when every declaration chain reaching it from a crate root
crosses a cfg(test) gate (or starts at a test root); one ungated chain makes it
product. A file reached by no chain is seeded as its own root, classified by
its path name, so an unlisted Cargo target cannot hide its children.
"""
import re
from collections import deque
from pathlib import PurePosixPath

ROOT = re.compile(
    r"(?:^|/)(?:src/(?:lib|main)\.rs|src/bin/[^/]+\.rs|src/bin/[^/]+/main\.rs|build\.rs"
    r"|(?:tests|benches|examples)/[^/]+\.rs|(?:tests|benches|examples)/[^/]+/main\.rs)$")
MOD_ITEM = re.compile(r"\bmod\s+(?:r#)?([A-Za-z_]\w*)\s*([;{])")
PATH_ATTR = re.compile(r'#\s*\[\s*path\s*=\s*"([^"\\]*)"\s*\]')
CFG_ATTR = re.compile(r"#\s*(!?)\s*\[\s*cfg\s*\(")
CFG_TOKEN = re.compile(r"\s*(?:([A-Za-z_]\w*)|([(),=]))")


def _require(condition, message):
    if not condition:
        raise ValueError(message)


def cfg_requires_test(predicate):
    """True when a masked cfg predicate can only hold with `test` enabled.

    String values are blank in masked text, so `key = "v"` reads as `key =`.
    Unknown shapes answer False: product is the safe default.
    """
    tokens = [m.group(1) or m.group(2) for m in CFG_TOKEN.finditer(predicate)]
    if "".join(tokens) != re.sub(r"\s+", "", predicate):
        return False
    position = 0

    def parse():
        nonlocal position
        if position >= len(tokens) or not re.fullmatch(r"[A-Za-z_]\w*", tokens[position]):
            raise ValueError("cfg predicate")
        name = tokens[position]
        position += 1
        if position < len(tokens) and tokens[position] == "=":
            position += 1
            return False
        if position >= len(tokens) or tokens[position] != "(":
            return name == "test"
        position += 1
        children = []
        while tokens[position] != ")":
            children.append(parse())
            if tokens[position] == ",":
                position += 1
        position += 1
        if name == "all":
            return any(children)
        if name == "any":
            return bool(children) and all(children)
        return False  # not(...) and unknown operators never prove test-only

    try:
        result = parse()
    except (ValueError, IndexError):
        return False
    return result and position == len(tokens)


def cfg_test_attributes(masked):
    """Yield (start, end, inner) for each #[cfg(..)]/#![cfg(..)] requiring test."""
    for m in CFG_ATTR.finditer(masked):
        depth, i = 1, m.end()
        while i < len(masked) and depth:
            depth += {"(": 1, ")": -1}.get(masked[i], 0)
            i += 1
        close = re.match(r"\s*\]", masked[i:])
        if depth or not close:
            continue
        if cfg_requires_test(masked[m.end():i - 1]):
            yield m.start(), i + close.end(), bool(m.group(1))


def _file_is_test_gated(masked):
    """Only a file-level #![cfg(..)] gates the file; one inside a nested body does not."""
    return any(inner and masked.count("{", 0, start) == masked.count("}", 0, start)
               for start, _, inner in cfg_test_attributes(masked))


def is_crate_root(path):
    """Cargo's default targets; tests/, benches/, examples/ and build.rs never sit under src/."""
    m = ROOT.search(path)
    return bool(m) and "src" not in PurePosixPath(path[:m.start()]).parts


def _outer_attributes(masked, text, start):
    """Return the original text of the outer attributes stacked before `start`."""
    visibility = re.search(r"\bpub\s*(?:\([^()]*\))?\s*$", masked[max(0, start - 64):start])
    pos = start - len(visibility.group()) if visibility else start
    attributes = []
    while True:
        stripped = masked[:pos].rstrip()
        if not stripped.endswith("]"):
            return attributes
        depth, i = 0, len(stripped) - 1
        while i >= 0:
            depth += {"]": 1, "[": -1}.get(stripped[i], 0)
            if depth == 0:
                break
            i -= 1
        hash_pos = stripped[:i].rstrip()
        if not hash_pos.endswith("#"):
            return attributes
        pos = len(hash_pos) - 1
        attributes.append(text[pos:len(stripped)])


def _closing_brace(masked, brace):
    depth = 0
    for i in range(brace, len(masked)):
        depth += {"{": 1, "}": -1}.get(masked[i], 0)
        if depth == 0:
            return i + 1
    raise ValueError("unbalanced module braces")


def declarations(path, text, masked, test_ranges, loaded_by_path=False):
    """Yield (candidate child paths, cfg(test)-gated, has #[path]) per out-of-line module."""
    _require(not re.search(r"\bcfg_attr\s*\([^\]]*\bpath\s*=", masked),
             f"cfg_attr(path) is unsupported by the module tree: {path}")
    file = PurePosixPath(path)
    # rustc treats crate roots, mod.rs and #[path]-loaded files alike.
    mod_rs_like = loaded_by_path or file.name == "mod.rs" or is_crate_root(path)
    inline = []  # (start, end, directory component) of inline module bodies
    items = []
    for m in MOD_ITEM.finditer(masked):
        attributes = _outer_attributes(masked, text, m.start())
        paths = [p for a in attributes for p in PATH_ATTR.findall(a)]
        _require(len(paths) <= 1, f"conflicting #[path] attributes: {path}:{m.group(1)}")
        explicit = paths[0] if paths else None
        if m.group(2) == "{":
            brace = m.end() - 1
            inline.append((brace, _closing_brace(masked, brace), explicit or m.group(1)))
        else:
            items.append((m.start(), m.group(1), explicit))
    for start, name, explicit in items:
        nested = [component for a, b, component in sorted(inline) if a < start < b]
        base = file.parent
        if not mod_rs_like and (nested or explicit is None):
            base = base / file.stem
        for component in nested:
            base = base / component
        if explicit is not None:
            candidates = [str(_normalize(base / explicit))]
        else:
            candidates = [str(base / (name + ".rs")), str(base / name / "mod.rs")]
        gated = any(a <= start < b for a, b in test_ranges)  # own or enclosing cfg(test)
        yield candidates, gated, explicit is not None


def _normalize(path):
    parts = []
    for part in PurePosixPath(path).parts:
        if part == "..":
            _require(parts and parts[-1] != "..", f"module path escapes the tree: {path}")
            parts.pop()
        elif part != ".":
            parts.append(part)
    return PurePosixPath(*parts)


def test_scopes(sources, masked, test_ranges, test_file):
    """Map each module-tree file to True (test-only) or False (product).

    `masked` and `test_ranges` are per-path maps; `test_file` classifies roots by name.
    """
    product, visited = set(), set()
    queue = deque()
    for path in sorted(sources):
        if is_crate_root(path):
            queue.append((path, not test_file(path), False))
    _walk(queue, sources, masked, test_ranges, product, visited)
    # A file no chain reaches may be a Cargo `path =` target (src/daemon_main.rs) or an
    # include!: seed it as a root by name so its children cannot be hidden as test-only.
    reached = {path for path, _, _ in visited}
    queue.extend((path, not test_file(path), True) for path in sorted(sources) if path not in reached)
    _walk(queue, sources, masked, test_ranges, product, visited)
    return {path: path not in product for path, _, _ in visited}


def _walk(queue, sources, masked, test_ranges, product, visited):
    inner_gate = {}
    while queue:
        path, as_product, loaded_by_path = queue.popleft()
        if path not in inner_gate:
            inner_gate[path] = _file_is_test_gated(masked[path])
        as_product = as_product and not inner_gate[path]
        state = (path, as_product, loaded_by_path)
        if state in visited or (path in product and not as_product):
            continue
        visited.add(state)
        if as_product:
            product.add(path)
        for candidates, gated, explicit in declarations(
                path, sources[path], masked[path], test_ranges[path], loaded_by_path):
            present = [c for c in candidates if c in sources]
            _require(len(present) <= 1, f"ambiguous module file: {' / '.join(present)}")
            if present:
                queue.append((present[0], as_product and not gated, explicit))
