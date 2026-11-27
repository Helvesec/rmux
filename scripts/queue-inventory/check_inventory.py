#!/usr/bin/env python3
"""Check the current queue inventory; --execute runs its referenced native tests."""
import argparse
import collections
import copy
import json
import subprocess
import sys
from pathlib import Path

from source_scan import require, scan_sources
from validation import validate_inventory

HERE = Path(__file__).resolve().parent


def load_sources(root):
    paths = subprocess.check_output(
        ['git', '-C', str(root), 'ls-files', '-z', '--cached', '--others',
         '--exclude-standard', '--', '*.rs'], text=True).split('\0')
    return {p: (root / p).read_text(encoding='utf-8') for p in sorted(set(paths)) if p}


def site_key(site):
    return site['path'], site['kind'], site['expression'], site['occurrence']


def current_inventory(sources, profiles, rows, stores):
    sites = scan_sources(sources, stores)
    counts = collections.Counter()
    by_key = {}
    inventory = {'schema': 1, 'profiles': profiles, 'entries': [], 'exemptions': []}
    for site in sorted(sites, key=lambda s: (s['path'], s['line'], s['column'])):
        if site['test_only']:
            inventory['exemptions'].append({'site': site, 'category': 'test_only',
                                             'reason': 'test body or fixture source'})
            continue
        key = site['path'], site['kind'], site['expression']
        counts[key] += 1
        by_key[(*key, counts[key])] = site
    for row in rows:
        key = site_key(row['site'])
        require(key in by_key, f'stale inventory site: {key}')
        materialized = dict(row, site=by_key[key])
        destination = 'exemptions' if 'category' in row else 'entries'
        inventory[destination].append(materialized)
    return inventory, sites


def validate_gate_links(root):
    for name in ('gate-unix-fast.sh', 'gate-windows-fast.ps1'):
        lines = (root / 'scripts' / name).read_text(encoding='utf-8').splitlines()
        require(any('scripts/queue-inventory/check_inventory.py --source . --execute' in line
                    for line in lines if not line.lstrip().startswith('#')),
                f'{name} must execute the queue references')


def negative_checks(inventory, sites, sources):
    checks = []
    def rejects(label, candidate, population=sites, texts=sources):
        try:
            validate_inventory(candidate, population, texts)
        except ValueError:
            checks.append(label)
        else:
            raise ValueError(f'negative control passed: {label}')
    absent = copy.deepcopy(inventory)
    absent['entries'].pop()
    rejects('missing entry', absent)
    extra_sources = {'src/queue_inventory_negative.rs':
                     'fn new_queue() { let (tx, rx) = tokio::sync::mpsc::channel::<u8>(17); }'}
    rejects('uncovered production queue', inventory,
            sites + scan_sources(extra_sources), {**sources, **extra_sources})
    missing_test = copy.deepcopy(inventory)
    next(iter(missing_test['profiles'].values()))['tests'][0]['symbol'] = 'absent_queue_test'
    rejects('absent referenced test', missing_test)
    false_bound = copy.deepcopy(inventory)
    row = next(row for row in false_bound['entries']
               if row['site']['kind'] == 'channel' and
               false_bound['profiles'][row['profile']]['capacity']['elements']['kind'] == 'fixed')
    false_bound['profiles'][row['profile']]['capacity']['elements']['value'] += 1
    rejects('false primitive capacity', false_bound)
    qualified_bound = copy.deepcopy(inventory)
    qualified_bound['profiles']['channel:server/listener.rs:501']['capacity']['elements']['value'] += 1
    rejects('false qualified primitive capacity', qualified_bound)
    return checks


def check(root, self_test=False):
    sources = load_sources(root)
    profiles = json.loads((HERE / 'profiles.json').read_text(encoding='utf-8'))
    rows = json.loads((HERE / 'sites.json').read_text(encoding='utf-8'))
    stores = json.loads((HERE / 'stores.json').read_text(encoding='utf-8'))
    inventory, sites = current_inventory(sources, profiles, rows, stores)
    report = validate_inventory(inventory, sites, sources)
    validate_gate_links(root)
    if self_test:
        report['negative_controls'] = negative_checks(inventory, sites, sources)
    return report, profiles


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--target', help='native Cargo target used by the enclosing gate')
    args = parser.parse_args()
    try:
        root = args.source.resolve()
        report, profiles = check(root, args.self_test)
        if args.execute:
            from run_references import execute
            report.update(execute(root, profiles, args.target))
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
        return 0
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        print(json.dumps({'verdict': 'FAIL', 'error': str(exc)}, ensure_ascii=False))
        return 1


if __name__ == '__main__':
    sys.exit(main())
