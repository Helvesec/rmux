"""Validate queue limits and source/test references against current source."""
import ast
import re
from pathlib import PurePosixPath
from source_scan import require, mask_rust, test_symbols

def check_anchor(anchor, sources):
    require(isinstance(anchor, dict) and anchor.get("path") in sources, "invalid evidence path")
    snippet = anchor.get("snippet")
    require(isinstance(snippet, str) and snippet and snippet in sources[anchor["path"]],
            f"evidence anchor absent: {anchor.get('path')}: {snippet}")


def validate_capacity(capacity, sources):
    require(isinstance(capacity, dict) and set(capacity) == {"elements", "bytes"}, "capacity needs elements and bytes")
    bounded = False
    for unit, bound in capacity.items():
        require(isinstance(bound, dict), f"invalid {unit} capacity")
        kind = bound.get("kind")
        require(kind in ("fixed", "parameter", "derived", "not_bounded"), f"unknown {unit} capacity")
        require(isinstance(bound.get("description"), str) and bound["description"].strip(), f"missing {unit} explanation")
        if kind == "fixed":
            require(type(bound.get("value")) is int and bound["value"] >= 0, f"invalid {unit} fixed capacity")
        elif kind in ("parameter", "derived"):
            require(isinstance(bound.get("expression"), str) and bound["expression"].strip(), f"unknown {unit} expression")
            require(not re.search(r"\b(?:unknown|inconnue?|tbd|todo)\b|\?", bound["expression"], re.I), f"unknown {unit} expression")
        require(bound.get("evidence"), f"capacity lacks source evidence: {unit}")
        for anchor in bound["evidence"]:
            check_anchor(anchor, sources)
        bounded |= kind != "not_bounded"
    require(bounded, "unbounded storage must be an explicit exemption, not a bounded entry")


def parent_module(path, sources):
    """Return the file declaring `path` as a plain `mod child;`, if unique."""
    child = PurePosixPath(path)
    if child.name in ('lib.rs', 'main.rs', 'mod.rs'):
        return None
    folder = child.parent
    parents = [p for p in (str(folder) + '.rs', str(folder / 'mod.rs')) if p in sources]
    if len(parents) != 1:
        return None
    declaration = (r'((?:#\s*\[[^\]]*\]\s*)*)'
                   r'(?:pub(?:\([^)]*\))?\s+)?mod\s+' + re.escape(child.stem) + r'\s*;')
    bindings = re.findall(declaration, mask_rust(sources[parents[0]]))
    if len(bindings) != 1 or re.search(r'\bpath\b', bindings[0]):
        return None
    return parents[0]


def binds_crate_module(path, module, sources, seen=()):
    """True when `module` names crate::module in `path`, directly or through
    a sole `use super::*;` that re-exposes the parent module's own binding."""
    local = mask_rust(sources[path])
    if re.search(r'\bmod\s+' + re.escape(module) + r'\b', local):
        return False
    imported = r'\buse\s+crate\s*::\s*' + re.escape(module) + r'\s*(?:;|::\s*\{\s*self\s*[,}])'
    direct = len(re.findall(imported, local))
    if direct:
        return direct == 1
    uses = re.findall(r'\buse\b[^;]*;', local)
    if any(re.search(r'\b' + re.escape(module) + r'\b', u) for u in uses):
        return False  # another explicit binding would shadow the parent's
    globs = [u for u in uses if '*' in u]
    if len(globs) != 1 or not re.fullmatch(r'use\s+super\s*::\s*\*\s*;', globs[0]):
        return False
    parent = parent_module(path, sources)
    return (parent is not None and parent not in seen
            and binds_crate_module(parent, module, sources, (*seen, path)))


def qualified_constant(expression, path, sources):
    """Resolve a crate module binding, rejecting unsupported bindings."""
    match = re.fullmatch(r'(crate::)?([A-Za-z_]\w*)::([A-Za-z_]\w*)', expression)
    if not match or '/src/' not in path:
        return None
    absolute, module, name = match.groups()
    if not absolute and not binds_crate_module(path, module, sources):
        return None
    root = path.split('/src/', 1)[0] + '/src/'
    roots = [p for p in (root + 'lib.rs', root + 'main.rs') if p in sources]
    if len(roots) != 1:
        return None
    declaration = (r'((?:#\s*\[[^\]]*\]\s*)*)'
                   r'(?:pub(?:\([^)]*\))?\s+)?mod\s+' + re.escape(module) + r'\s*;')
    bindings = re.findall(declaration, mask_rust(sources[roots[0]]))
    if len(bindings) != 1 or re.search(r'\bpath\b', bindings[0]):
        return None
    targets = [p for p in (root + module + '.rs', root + module + '/mod.rs') if p in sources]
    if len(targets) != 1:
        return None
    target = targets[0]
    text = mask_rust(sources[target])
    constants = list(re.finditer(r'\bconst\s+' + re.escape(name) +
                                r'\s*:\s*(?:usize|u16|u32|u64)\s*=\s*([^;]+);', text))
    if len(constants) != 1:
        return None
    item = constants[0]
    prefix = text[:item.start()]
    if prefix.count('{') != prefix.count('}'):
        return None
    return target, item.group(1)


def numeric_capacity(expression, path, sources, seen=()):
    """Resolve small integer constant expressions without eval or compiling Rust."""
    expression = expression.strip().rstrip(',').strip()
    if '::' in expression:
        key = (path, expression)
        binding = qualified_constant(expression, path, sources)
        if binding is None or key in seen:
            return None
        target, value = binding
        return numeric_capacity(value, target, sources, (*seen, key))
    try:
        node = ast.parse(expression, mode='eval').body
    except SyntaxError:
        return None
    def resolve(n):
        if isinstance(n, ast.Constant) and type(n.value) is int:
            return n.value
        if isinstance(n, ast.BinOp) and isinstance(n.op, (ast.Add, ast.Sub, ast.Mult, ast.Div, ast.FloorDiv)):
            a, b = resolve(n.left), resolve(n.right)
            if a is None or b is None or (isinstance(n.op, (ast.Div, ast.FloorDiv)) and b == 0): return None
            if isinstance(n.op, ast.Add): return a + b
            if isinstance(n.op, ast.Sub): return a - b
            if isinstance(n.op, ast.Mult): return a * b
            return a // b
        if isinstance(n, ast.Name) and n.id not in seen:
            pattern = r"\bconst\s+" + re.escape(n.id) + r"\s*:\s*(?:usize|u16|u32|u64)\s*=\s*([^;]+);"
            local = re.findall(pattern, mask_rust(sources[path]))
            choices = [(path, e) for e in local]
            if not choices:
                # Imported constants must be unambiguous across the current source.
                choices = [(p, e) for p, text in sources.items() if n.id in text
                           for e in re.findall(pattern, mask_rust(text))]
            values = {numeric_capacity(e, p, sources, (*seen, n.id)) for p, e in choices}
            if len(values) == 1 and None not in values: return values.pop()
        return None
    return resolve(node)


def validate_primitive_capacity(site, profile, sources):
    if site['kind'] != 'channel': return
    expression = site['expression']
    unit = 'bytes' if 'duplex(' in expression else 'elements'
    if 'oneshot::channel' in expression or 'watch::channel' in expression:
        expected = 1
    elif 'unbounded' in expression:
        return  # A separately documented owner can impose a logical limit.
    else:
        argument = expression[expression.rfind('(') + 1:-1]
        expected = numeric_capacity(argument, site['path'], sources)
    bound = profile['capacity'][unit]
    require(expected is not None or bound.get('kind') != 'fixed',
            f"cannot resolve fixed capacity: {site['id']}")
    if expected is not None:
        require(bound.get('kind') == 'fixed' and bound.get('value') == expected,
                f"capacity differs from source ({expected} {unit}): {site['id']}")


def validate_inventory(inventory, sites, sources):
    require(inventory.get("schema") == 1, "inventory schema must be 1")
    found = {s["id"]: s for s in sites}
    require(len(found) == len(sites), "scanner generated duplicate sites")
    entries, exemptions = inventory.get("entries"), inventory.get("exemptions")
    require(isinstance(entries, list) and isinstance(exemptions, list), "entries/exemptions must be arrays")
    seen = set()
    profiles = inventory.get("profiles", {})
    used = set()
    for row in entries + exemptions:
        sid = row.get("site", {}).get("id")
        require(sid not in seen, f"duplicate creation: {sid}")
        seen.add(sid)
        require(sid in found, f"invented/stale creation: {sid}")
        require(row["site"] == found[sid], f"site differs from rescan: {sid}")
    require(seen == set(found), "missing creations: " + ", ".join(sorted(set(found) - seen)))
    for row in entries:
        pid = row.get("profile")
        require(pid in profiles, f"missing capacity/profile: {row['site']['id']}")
        require(not row["site"]["test_only"], "test site incorrectly counted as production")
        used.add(pid)
    for row in exemptions:
        require(row.get("reason") and row.get("category") in (
            "test_only", "declaration", "wrapper", "unbounded", "temporary", "not_queue"),
            f"unjustified exemption: {row['site']['id']}")
        if row["category"] == "test_only":
            require(row["site"]["test_only"], "production site exempted as test")
        if row["category"] == "declaration":
            require(row["site"]["kind"] == "deque-declaration", "non-declaration exempted as declaration")
        if row['category'] == 'wrapper':
            require(row['site']['kind'] == 'wrapper' or row['site']['expression'] == 'output::channel()',
                    'non-wrapper exempted as delegation')
        if row['category'] == 'temporary':
            require(row['site']['kind'] == 'deque', 'non-worklist exempted as temporary')
        if row['category'] == 'unbounded' and row['site']['kind'] == 'channel':
            expression = row['site']['expression']
            require('unbounded_channel' in expression or re.fullmatch(
                    r'(?:std\s*::\s*sync\s*::\s*)?(?:mpsc|std_mpsc)\s*::\s*channel\s*'
                    r'(?:::\s*<[^;{}]+>\s*)?\(\s*\)', expression),
                    'bounded primitive exempted as unbounded')
        if row.get("profile"):
            require(row["profile"] in profiles, "exemption references missing profile")
            used.add(row["profile"])
        if row["category"] in ("unbounded", "temporary", "not_queue", "wrapper"):
            require(row.get("evidence"), "production exemption needs source evidence")
            for a in row["evidence"]:
                check_anchor(a, sources)
    require(used == set(profiles), "unused profile or missing profile")
    for pid in sorted(used):
        p = profiles[pid]
        for key in ("owner", "producer", "consumer", "saturation", "closure"):
            require(isinstance(p.get(key), str) and p[key].strip(), f"{pid}: missing {key}")
        validate_capacity(p.get("capacity"), sources)
        require(p.get("evidence"), f"{pid}: missing policy evidence")
        for a in p["evidence"]:
            check_anchor(a, sources)
        require(isinstance(p.get("tests"), list), f"{pid}: missing tests list")
        require(p["tests"], f"{pid}: no linked test")
        for t in p["tests"]:
            require(t.get("package") and t.get("target"), "test needs a Cargo target")
            path, symbol = t.get("path"), t.get("symbol")
            require(path in sources and isinstance(symbol, str), "invalid test reference")
            require(isinstance(t.get("platforms"), list) and t["platforms"]
                    and set(t["platforms"]) <= {"linux", "macos", "windows"},
                    f"invalid native test platforms: {symbol}")
            parts = PurePosixPath(path).parts
            package = parts[1] if parts[0] == "crates" else "rmux"
            subpath = parts[2:] if parts[0] == "crates" else parts
            target = ("test:" + PurePosixPath(subpath[1]).stem if subpath[0] == "tests"
                      else "bin:rmux" if package == "rmux" else "lib")
            require((t["package"], t["target"]) == (package, target),
                    f"test reference points at the wrong Cargo target: {symbol}")
            require(symbol in test_symbols(sources[path]), f"annotated test definition absent: {symbol}")

    for row in entries:
        validate_primitive_capacity(row['site'], profiles[row['profile']], sources)
    return {"verdict": "PASS", "sites": len(sites), "bounded_entries": len(entries),
            "exemptions": len(exemptions), "profiles": len(profiles),
            "product_tests_executed": False}
