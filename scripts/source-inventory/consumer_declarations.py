"""Explicit additions of newly introduced test consumers; no semantic exemptions."""
from functools import lru_cache
import json
from pathlib import Path, PurePosixPath
import re
import subprocess

from model import canonical, git, read_tree, source_syntax_parser

DECLARATIONS = Path(__file__).with_name('consumer-additions.jsonl')
FIELDS = {'path', 'module_dir', 'condition', 'consumer', 'introduced_commit', 'reason'}


def relative_path(value):
    return (isinstance(value, str) and bool(value)
            and not PurePosixPath(value).is_absolute()
            and all(part not in ('', '.', '..') for part in value.split('/'))
            and '\\' not in value and ':' not in value)


def parse_declarations(data):
    rows, seen = [], set()
    for number, line in enumerate(data.decode('utf-8').splitlines(), 1):
        if line.startswith('#'):
            continue
        try:
            row = json.loads(line)
        except ValueError as error:
            raise ValueError(f'consumer declaration line {number}: invalid JSON') from error
        if not isinstance(row, dict) or set(row) != FIELDS:
            raise ValueError(f'consumer declaration line {number}: invalid fields')
        if (not relative_path(row['path']) or not row['path'].endswith('.rs')
                or not relative_path(row['module_dir'])
                or not isinstance(row['consumer'], str) or not row['consumer'].endswith('.rs:mod')
                or not relative_path(row['consumer'][:-4])
                or not isinstance(row['introduced_commit'], str)
                or not re.fullmatch('[0-9a-f]{40}', row['introduced_commit'])
                or not isinstance(row['reason'], str) or not row['reason'].strip()
                or not isinstance(row['condition'], (bool, str, dict))):
            raise ValueError(f'consumer declaration line {number}: malformed provenance')
        identity = canonical([row[key] for key in ('path', 'module_dir', 'condition', 'consumer')])
        if identity in seen:
            raise ValueError('duplicate consumer declaration')
        seen.add(identity)
        rows.append(row)
    return rows


@lru_cache(maxsize=64)
def introduction_contexts(repo, commit, parser):
    """Resolve actual Rust module edges in the immutable introducing tree."""
    from inventory import contexts, parser_scan, targets
    files = read_tree(repo, commit)
    policy = json.loads(files['scripts/source-inventory/declarations.json']['data'])
    parsed = parser_scan(Path(parser), files, policy)
    roots, _ = targets(files, policy)
    errors = []
    result = contexts(files, parsed, roots, policy, errors)
    if errors:
        raise ValueError('consumer introduction source scan failed: ' + '; '.join(errors))
    return result


def validate_introduction(repo, head, row):
    from relocation_chain import ancestor
    commit = row['introduced_commit']
    resolved = subprocess.run(['git', '-C', str(repo), 'rev-parse', '--verify', commit + '^{commit}'],
                              capture_output=True)
    if resolved.returncode or resolved.stdout.decode().strip() != commit:
        raise ValueError('consumer introduction does not resolve to an exact commit')
    if not ancestor(repo, commit, head):
        raise ValueError('consumer introduction is not an ancestor of HEAD')
    source = row['consumer'][:-4]
    parents = git(repo, 'rev-list', '--parents', '-n', '1', commit).decode().split()[1:]
    # This bounded mechanism admits consumers from NEW source files only. A merge
    # cannot redeclare a consumer already present in either parent.
    if not parents:
        raise ValueError('consumer introduction must have a parent')
    for parent in parents:
        added = git(repo, 'diff', parent, commit, '--name-only', '--no-renames',
                    '--diff-filter=A', '--', source).decode().splitlines()
        if added != [source]:
            raise ValueError('consumer introduction does not add ' + source)
    groups = introduction_contexts(str(repo), commit, str(source_syntax_parser())).get(row['path'], {})
    matches = [g for g in groups.values() if g['module_dir'] == row['module_dir']
               and g['condition'] == row['condition'] and g['role'] == 'tests'
               and row['consumer'] in g['consumers']]
    if len(matches) != 1:
        raise ValueError('consumer introduction provenance differs')


def declared_consumers(repo, revision):
    """Validate current declarations; activate each only below its introduction."""
    from relocation_chain import ancestor
    rows = parse_declarations(DECLARATIONS.read_bytes())
    head = git(repo, 'rev-parse', 'HEAD').decode().strip()
    active = []
    for row in rows:
        validate_introduction(repo, head, row)
        if ancestor(repo, row['introduced_commit'], revision):
            active.append(row)
    return active
