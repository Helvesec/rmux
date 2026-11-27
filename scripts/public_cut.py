#!/usr/bin/env python3
"""Verify a reviewed public projection without rewriting either Git history.

The rule is a trusted, reviewed input, never extracted from the candidate at
verification time. Blob substitutions authorize only their exact byte pairs;
ledger substitutions additionally authorize only named JSON leaves. Neither
object availability nor equal trees is treated as Git ancestry.
"""
import argparse
import fnmatch
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys


LEDGER = 'scripts/source-inventory/production-ledger.json'


class ProjectionError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise ProjectionError(message)


def git(repo, *args):
    # Replacement refs must not counterfeit either historical authority.
    return subprocess.check_output(['git', '--no-replace-objects', '-C', str(repo),
                                    *args], stderr=subprocess.PIPE)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate JSON key: ' + key)
        result[key] = value
    return result


def decode(data):
    return json.loads(data, object_pairs_hook=unique_object)


def sha(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'),
                                     ensure_ascii=True).encode()).hexdigest()


def leaves(before, after, pointer=''):
    """Structural differences; changing a list's length is not a leaf edit."""
    if type(before) is type(after) and isinstance(before, dict):
        require(before.keys() == after.keys(), 'ledger keys changed: ' + pointer)
        for key in before:
            token = key.replace('~', '~0').replace('/', '~1')
            yield from leaves(before[key], after[key], pointer + '/' + token)
    elif type(before) is type(after) and isinstance(before, list):
        require(len(before) == len(after), 'ledger population changed: ' + pointer)
        for index, (left, right) in enumerate(zip(before, after)):
            yield from leaves(left, right, pointer + '/' + str(index))
    elif type(before) is not type(after) or before != after:
        yield pointer, {'before': sha(before), 'after': sha(after)}


def tree(repo, revision):
    require(bool(re.fullmatch('[0-9a-f]{40}', revision)), 'exact commit required')
    require(git(repo, 'rev-parse', revision + '^{commit}').decode().strip() == revision,
            'not an exact commit')
    result = {}
    for item in git(repo, 'ls-tree', '-rz', '--full-tree', revision).split(b'\0'):
        if not item:
            continue
        header, name = item.split(b'\t', 1)
        mode, kind, blob = header.decode().split()
        require(mode in ('100644', '100755') and kind == 'blob', 'nonregular tree entry')
        result[name.decode()] = {'mode': mode, 'blob': blob}
    return result


def verify(repo, rule, candidate, anchor):
    require(rule['version'] == 1, 'unsupported projection rule')
    source = rule['source']['commit']
    require(anchor == rule['trunk_anchor'], 'wrong trunk authority')
    require(git(repo, 'merge-base', source, anchor).decode().strip() == source,
            'source is not an ancestor of the trunk authority')
    before, after = tree(repo, source), tree(repo, candidate)
    require(git(repo, 'rev-parse', source + '^{tree}').decode().strip() ==
            rule['source']['tree'], 'wrong source tree')
    require(git(repo, 'show', '-s', '--format=%P', candidate).decode().strip() ==
            rule['public_parent'], 'wrong public parent')
    delta = {path for path in before.keys() | after.keys()
             if before.get(path) != after.get(path)}
    require(delta == rule['paths'].keys(), 'changed path population differs from rule')
    for path, change in rule['paths'].items():
        require(bool(change['reason'].strip()), 'missing transformation reason: ' + path)
        require(before.get(path) == change['before'], 'wrong source blob/mode: ' + path)
        require(after.get(path) == change['after'], 'unruled public blob/mode: ' + path)
        if change['after'] is None:
            require(change['exclusion'] in rule['exclusions'] and
                    fnmatch.fnmatchcase(path, change['exclusion']),
                    'deletion outside exclusion rule: ' + path)
    for path in after:
        require(not any(fnmatch.fnmatchcase(path, pattern) for pattern in rule['exclusions']),
                'excluded path in public tree: ' + path)
    old = decode(git(repo, 'show', source + ':' + LEDGER))
    new = decode(git(repo, 'show', candidate + ':' + LEDGER))
    actual = dict(leaves(old, new))
    expected = {pointer: {key: row[key] for key in ('before', 'after')}
                for pointer, row in rule['ledger'].items()}
    require(all(row['reason'].strip() for row in rule['ledger'].values()),
            'missing ledger redaction reason')
    require(actual == expected, 'ledger difference outside exact leaf rule')
    public_tree = git(repo, 'rev-parse', candidate + '^{tree}').decode().strip()
    require(public_tree == rule['public']['tree'], 'wrong public tree')
    require(candidate == rule['public']['commit'], 'wrong public commit')
    return dict(source=source, source_tree=rule['source']['tree'], cut=candidate,
                cut_tree=public_tree, trunk_anchor=anchor, paths=len(delta),
                exclusions=sum(row['after'] is None for row in rule['paths'].values()),
                ledger_leaves=sorted(actual), rule_sha256=sha(rule))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--rule', type=Path, required=True,
                        help='external, independently reviewed projection rule')
    parser.add_argument('--cut', required=True)
    parser.add_argument('--trunk', required=True)
    args = parser.parse_args()
    print(json.dumps(verify(args.repo, decode(args.rule.read_bytes()), args.cut, args.trunk),
                     sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (ProjectionError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
        print('public projection RED: ' + str(error), file=sys.stderr)
        sys.exit(1)
