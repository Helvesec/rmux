"""Conservative source discovery for the versioned tmux shim catalogue.

Every tracked Rust file is fingerprinted, including unmarked files and tests.
Explicit tmux/shim/quirk/compatibility witnesses and ledger owners are retained.
Witnesses include comments and strings: they are candidates, never proof of a
shim, absence of a shim, a tmux bug, or runtime parity. No cfg is evaluated.
"""
import hashlib
from pathlib import PurePosixPath
import re
import subprocess

import toml_reader

LEDGER = 'tests/reference/tmux_compat/divergences.toml'
CONTRACT = 'docs/validation/reference-contracts.json'
LEGACY = 'docs/validation/legacy-scenarios.json'
PIN = 'tests/reference/tmux_compat/frozen_reference.yaml'
MARKER = re.compile(r'tmux|shim|quirk|compatib|product_divergence', re.I)
FUNCTION = re.compile(r'\b(?:async\s+)?fn\s+([A-Za-z_][A-Za-z_0-9]*)\s*\(')
RAW_STRING = re.compile(r'r(\#*)"')
CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\r\n])'")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def tracked_paths(root):
    raw = subprocess.check_output(['git', '-C', str(root), 'ls-files', '-z'])
    return sorted(set(filter(None, raw.decode().split('\0'))))


def is_rust_source(path):
    """The one Rust filter shared by discover and the untracked-source guard."""
    return path.endswith('.rs')


def require_no_untracked_rust(root):
    """Refuse while any unignored Rust file would be silently left out of discover."""
    raw = subprocess.check_output(['git', '-C', str(root), 'ls-files', '--others',
                                   '--exclude-standard', '-z'])
    untracked = sorted(set(filter(is_rust_source, raw.decode().split('\0'))))
    require(not untracked, 'untracked source: ' + ', '.join(untracked))


def read(root, relative):
    path = PurePosixPath(relative)
    require(not path.is_absolute() and '..' not in path.parts and str(path) == relative,
            'unsafe source path: ' + relative)
    target = root / relative
    require(target.resolve().is_relative_to(root.resolve()) and not target.is_symlink(),
            'source symlink: ' + relative)
    return target.read_bytes()


def reference(root, relative):
    return {'path': relative, 'sha256': sha(read(root, relative))}


def ledger(root):
    payload = toml_reader.loads(read(root, LEDGER).decode())
    require(payload.get('schema') == 1 and payload.get('policy') == {
        'oracle': 'tmux 3.7b', 'unlisted_divergences_are_bugs': True}, 'ledger policy')
    rows = payload['entry']
    ids = [row['id'] for row in rows]
    require(len(ids) == len(set(ids)) and all(re.fullmatch(r'C-D[1-9][0-9]*', x)
                                           for x in ids), 'duplicate/invalid ledger ID')
    return rows


def ledger_references(row):
    return row.get('evidence', []) + row.get('tests', [])


def witnesses(path, raw):
    rows = []
    for number, line in enumerate(raw.decode().splitlines(), 1):
        markers = sorted(set(m.group().lower() for m in MARKER.finditer(line)))
        if markers:
            rows.append({'line': number, 'markers': markers,
                         'text_sha256': sha(line.encode())})
    # A shim-bearing filename remains discoverable even with no comment/identifier.
    if MARKER.search(path):
        rows.insert(0, {'line': 0, 'markers': ['path'], 'text_sha256': sha(path.encode())})
    return rows


def discover(root, entries):
    paths = tracked_paths(root)
    owners = {}
    for entry in entries:
        for ref in ledger_references(entry):
            path = ref.split('::', 1)[0]
            owners.setdefault(path, set()).add(entry['id'])
    population, sources = [], []
    # All Rust, not only selected host modules or keyword-bearing filenames.
    # Include non-Rust ledger owners when they are tracked local references.
    for path in paths:
        if not is_rust_source(path) and path not in owners:
            continue
        raw = read(root, path)
        item = {'path': path, 'sha256': sha(raw)}
        population.append(item)
        sites = witnesses(path, raw)
        if sites or path in owners:
            sources.append({**item, 'witnesses': sites,
                            'ledger_ids': sorted(owners.get(path, []))})
    return population, sources


def rust_code(text):
    """Mask Rust comments/literals before looking for function declarations.

    This lexer does not expand macros, evaluate cfg or claim runtime reachability.
    """
    blank = list(text)
    i = 0
    while i < len(text):
        start = i
        if text.startswith('//', i):
            end = text.find('\n', i)
            i = len(text) if end < 0 else end
        elif text.startswith('/*', i):
            i += 2
            depth = 1
            while i < len(text) and depth:
                if text.startswith('/*', i):
                    depth += 1
                    i += 2
                elif text.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            require(depth == 0, 'unterminated Rust comment')
        else:
            raw = RAW_STRING.match(text, i) if text[i] == 'r' else None
            char = CHAR_LITERAL.match(text, i) if text[i] == "'" else None
            if raw:
                closing = '"' + raw[1]
                end = text.find(closing, raw.end())
                require(end >= 0, 'unterminated Rust raw string')
                i = end + len(closing)
            elif char:
                i = char.end()
            elif text[i] == '"':
                i += 1
                while i < len(text) and text[i] != '"':
                    i += 2 if text[i] == '\\' else 1
                require(i < len(text), 'unterminated Rust string')
                i += 1
            else:
                i += 1
                continue
        blank[start:i] = [c if c in '\r\n' else ' ' for c in text[start:i]]
    return ''.join(blank)


def local_references(root, entries):
    """Preserve external ledger locators without pretending they are local proof."""
    paths = set(tracked_paths(root))
    result = []
    source_cache = {}
    symbol_cache = {}
    for ref in sorted(set(r for row in entries for r in ledger_references(row))):
        path, sep, symbol = ref.partition('::')
        if path not in paths:
            external = ref.startswith('commit:') or not ref.startswith(
                ('crates/', 'src/', 'tests/', 'scripts/', 'benches/'))
            result.append({'locator': ref, 'status': 'external-unverified' if external
                           else 'missing-source'})
            continue
        if path not in source_cache:
            source_cache[path] = read(root, path)
        raw = source_cache[path]
        if sep and path not in symbol_cache:
            symbol_cache[path] = set(FUNCTION.findall(rust_code(raw.decode())))
        present = not sep or symbol in symbol_cache[path]
        result.append({'locator': ref, 'status': 'source-only-not-executed' if present
                       else 'missing-symbol',
                       'source': {'path': path, 'sha256': sha(raw)}})
    return result
