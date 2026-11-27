"""Version 1 cfg algebra and Git-only input/manifest validation (Python >=3.11)."""
import hashlib
import itertools
import json
import os
import subprocess
from pathlib import Path, PurePosixPath

CLASSES = ('production', 'tests', 'specifications', 'tooling', 'generated',
           'vendored', 'documentation', 'assets', 'non-code')
MAINTAINED = set(CLASSES) - {'generated', 'vendored'}


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args])


def source_syntax_parser():
    """The SOURCE_SYNTAX parser path, or an explicit error naming what is wrong."""
    value = os.environ.get('SOURCE_SYNTAX', '')
    if not value:
        raise RuntimeError('SOURCE_SYNTAX must name the rmux-source-syntax parser '
                           '(build it as scripts/source-inventory/README.md shows)')
    path = Path(value)
    if not path.is_absolute():
        raise RuntimeError(f'SOURCE_SYNTAX must be an absolute path: {value}')
    path = path.resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise RuntimeError(f'SOURCE_SYNTAX must name an executable file: {path}')
    return path


def borrow_objects(git_dir, objects):
    """Let a fixture repository read another object store.

    Git reads alternates as bytes; a Windows text-mode write would end the path
    with CR and Git would look for '.../objects\\r'.
    """
    (Path(git_dir) / 'objects/info/alternates').write_bytes(os.fsencode(str(objects)) + b'\n')


def read_tree(repo, revision):
    if len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
        raise ValueError('revision must be an exact lowercase 40-digit Git commit')
    if git(repo, 'rev-parse', revision + '^{commit}').decode().strip() != revision:
        raise ValueError('not a commit')
    entries = git(repo, 'ls-tree', '-rz', '--full-tree', revision).split(b'\0')
    files, objects = {}, []
    for entry in filter(None, entries):
        header, path = entry.split(b'\t', 1)
        mode, kind, oid = header.decode().split()
        path = path.decode('utf-8', errors='strict')
        if mode not in ('100644', '100755') or kind != 'blob':
            raise ValueError(f'unsupported Git entry: {path} {mode} {kind}')
        files[path] = {'blob': oid, 'mode': mode}
        objects.append(oid)
    output = subprocess.run(['git', '-C', str(repo), 'cat-file', '--batch'],
                            input=('\n'.join(objects) + '\n').encode(),
                            stdout=subprocess.PIPE, check=True).stdout
    offset = 0
    for path in files:
        end = output.index(b'\n', offset)
        oid, kind, size = output[offset:end].split()
        size = int(size)
        data = output[end + 1:end + 1 + size]
        if oid.decode() != files[path]['blob'] or kind != b'blob' or len(data) != size:
            raise ValueError('invalid cat-file response')
        files[path]['data'] = data
        offset = end + 2 + size
    return files


def simplify(formula):
    if isinstance(formula, (bool, str)):
        return formula
    op, args = next(iter(formula.items()))
    args = [simplify(a) for a in args]
    if op == 'not':
        return not args[0] if isinstance(args[0], bool) else {'not': args}
    args = [child for a in args for child in (a[op] if isinstance(a, dict) and op in a else [a])]
    identity = op == 'all'
    if (not identity) in [a for a in args if isinstance(a, bool)]:
        return not identity
    args = [a for a in args if not isinstance(a, bool)]
    unique = {canonical(a): a for a in args}
    args = [unique[k] for k in sorted(unique)]
    if not args:
        return identity
    return args[0] if len(args) == 1 else {op: args}


def conjunction(*args):
    return simplify({'all': list(args)})


def possible(formula, test):
    """Exact Boolean SAT over independent cfg atoms; no selected host/features.

    Treating platform atoms independently is conservative: it can retain extra
    production, never manufacture test-only code. Complexity is explicitly bound.
    """
    atoms = set()

    def collect(value):
        if isinstance(value, str):
            if value != 'test':
                atoms.add(value)
        elif isinstance(value, dict):
            for child in next(iter(value.values())):
                collect(child)
    collect(formula)
    if len(atoms) > 16:
        raise ValueError('unsupported cfg formula: more than 16 independent atoms')
    atoms = sorted(atoms)

    def evaluate(value, env):
        if isinstance(value, bool):
            return value
        if isinstance(value, str):
            return env[value]
        op, args = next(iter(value.items()))
        values = [evaluate(a, env) for a in args]
        return all(values) if op == 'all' else any(values) if op == 'any' else not values[0]
    for values in itertools.product((False, True), repeat=len(atoms)):
        if evaluate(formula, dict(zip(atoms, values), test=test)):
            return True
    return False


def normalize(path):
    parts = []
    for part in str(path).split('/'):
        if part == '..':
            if not parts:
                raise ValueError(f'path escapes repository: {path}')
            parts.pop()
        elif part not in ('', '.'):
            parts.append(part)
    return '/'.join(parts)


def expand_declarations(policy):
    """Resolve v2 shared responsibilities to the unchanged v1 document contract.

    Members explicitly retain all accepted blobs; their source is their own path.
    No patterns, implicit membership, overrides or inherited classifications.
    """
    if policy.get('version') != 2:
        return policy
    expanded = dict(policy, version=1, documents=dict(policy['documents']))
    for group in expanded.pop('document_groups'):
        if set(group) != {'responsibility', 'members'} or not group['members']:
            raise ValueError('invalid document group')
        responsibility = group['responsibility']
        if set(responsibility) != {'role', 'owner', 'consumers', 'freshness', 'basis'}:
            raise ValueError('invalid shared responsibility')
        for path, blobs in group['members'].items():
            if path in expanded['documents']:
                raise ValueError(f'duplicate document declaration: {path}')
            if not isinstance(blobs, list) or not blobs or any(
                    not isinstance(b, str) or len(b) != 40
                    or any(c not in '0123456789abcdef' for c in b) for b in blobs):
                raise ValueError(f'invalid document blobs: {path}')
            expanded['documents'][path] = dict(responsibility, source=[path], source_blobs=blobs)
    return expanded


def validate_declarations(policy, files):
    policy = expand_declarations(policy)
    if policy.get('version') != 1 or set(policy) != {
        'version', 'files', 'documents', 'rust_roots', 'crate_roles', 'includes', 'provenance'
    }:
        raise ValueError('unsupported declaration schema')
    assigned = {}
    for role, paths in policy['files'].items():
        if role not in CLASSES:
            raise ValueError(f'unknown class {role}')
        for path in paths:
            if path in assigned:
                raise ValueError(f'duplicate declaration: {path}')
            assigned[path] = role
    for path, row in policy['documents'].items():
        required = {'role', 'source', 'consumers', 'owner', 'freshness', 'basis', 'source_blobs'}
        if set(row) != required or any(not row[k] for k in required):
            raise ValueError(f'incomplete document declaration: {path}')
        if not isinstance(row['consumers'], list) or row['basis'] != 'maintained declaration':
            raise ValueError(f'invalid document declaration: {path}')
        if path in files:
            if files[path]['blob'] not in row['source_blobs']:
                raise ValueError(f'stale document declaration: {path}')
            for source in row['source']:
                if source not in files:
                    raise ValueError(f'document source absent: {path}: {source}')
    for path in files:
        if path.endswith(('.md', '.html', '.1')) or path in (
                'DCO', 'LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE'):
            if path not in policy['documents']:
                raise ValueError(f'undocumented role/owner/consumer: {path}')
        if assigned.get(path) in ('generated', 'vendored'):
            if path not in policy['provenance'] or not policy['provenance'][path]:
                raise ValueError(f'missing actual provenance: {path}')
    return assigned
