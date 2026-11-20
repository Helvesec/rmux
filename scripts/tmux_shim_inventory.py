#!/usr/bin/env python3
"""Extract and check the source/ledger joins; never certify runtime compatibility."""
import argparse
from collections import Counter
import json
from pathlib import Path
import sys

from tmux_shim_extract import (CONTRACT, LEDGER, LEGACY, PIN, discover, ledger,
                               local_references, read, reference, require,
                               require_no_untracked_rust, sha)

ROOT = Path(__file__).resolve().parents[1]
CATALOGUE = 'scripts/tmux-shims/catalogue.json'
MANIFEST = 'scripts/tmux-shims/manifest.json'
CLASSES = ['contrat-utile', 'bugtmux-confirme', 'extensionRMUX', 'indetermine']
# An installation facility has no tmux parity claim. All other source candidates
# remain indeterminate, including files with mixed test and production code.
INSTALLERS = {'src/cli/tmux_dropin.rs', 'src/cli/claude_launcher.rs',
              'crates/rmux-server/src/tmux_shim.rs'}
LIMITS = ('Static conservative inventory, not a semantic Rust parser or runtime '
          'qualification. Unmarked Rust is fingerprinted, not certified shim-free. '
          'No ledger item is promoted to confirmed tmux bug. Existing tests and '
          'route obligations are linked, not claimed executed or exhaustive. '
          'External ledger evidence remains unverified.')


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, indent=2) + '\n').encode()


def load_catalogue(root):
    return json.loads(read(root, CATALOGUE))


def generate(root):
    entries = ledger(root)
    population, sources = discover(root, entries)
    contract = json.loads(read(root, CONTRACT))
    routes = next(row['routes'] for row in contract['families'] if row['id'] == 'conformance')
    historical = json.loads(read(root, LEGACY))
    collection = next(row for row in historical['collections'] if row['source']['path'] == LEDGER)
    for row in sources:
        row['classification'] = 'extensionRMUX' if row['path'] in INSTALLERS else 'indetermine'
        row['basis'] = ('RMUX shim installation facility; no tmux parity asserted'
                        if row['path'] in INSTALLERS else
                        'Conservative lexical/ledger candidate; semantic disposition unproven')
    divergences = []
    for row in entries:
        tests = row.get('tests', [])
        # The published ledger explicitly calls these product extensions.
        extension = row['id'] in {'C-D85', 'C-D86'}
        divergences.append({
            'id': row['id'], 'ledger_entry_sha256': sha(encoded(row)),
            'classification': 'extensionRMUX' if extension else 'indetermine',
            'basis': 'Published ledger extension' if extension else 'No new oracle/counterproof qualification',
            'source_paths': [s['path'] for s in sources if row['id'] in s['ledger_ids']],
            'oracle_case': {'status': 'not-requalified', 'candidate_tests': tests},
            'canary': {'status': 'not-requalified', 'candidate_tests': [
                ref for ref in tests if ref.split('::')[-1].endswith('_product_divergence')]},
            'route_matrix': [{'route': route, 'status': 'indeterminate', 'evidence': []}
                             for route in routes],
            'historical_manifest_id': (collection['id'] + '/' + row['id']
                                       if row['id'] in collection['legacy_ids'] else None),
        })
    return {'schema': 1, 'scope': 'STATIC_SOURCE_LEDGER_INVENTORY',
            'classifications': CLASSES, 'limits': LIMITS,
            'inputs': [reference(root, p) for p in (LEDGER, CONTRACT, LEGACY, PIN)],
            'rust_and_ledger_population': population, 'sources': sources,
            'ledger_references': local_references(root, entries),
            'route_authority': {'path': CONTRACT, 'family': 'conformance', 'routes': routes},
            'historical_manifest': {'path': LEGACY, 'collection_id': collection['id'],
                                    'legacy_ids': collection['legacy_ids'],
                                    'status': collection['retained_status']},
            'divergences': divergences}


def unique(rows, key, label):
    require(isinstance(rows, list) and all(isinstance(r, dict) and key in r for r in rows),
            'malformed ' + label)
    keys = [r[key] for r in rows]
    require(len(keys) == len(set(keys)), 'duplicate ' + label)
    return {r[key]: r for r in rows}


def validate(root, catalogue):
    expected = generate(root)
    actual_sources = unique(catalogue.get('sources'), 'path', 'source')
    expected_sources = {s['path']: s for s in expected['sources']}
    orphan = set(actual_sources) - set(expected_sources)
    omitted = set(expected_sources) - set(actual_sources)
    require(not orphan, 'orphan source: ' + ', '.join(sorted(orphan)))
    require(not omitted, 'omitted source: ' + ', '.join(sorted(omitted)))
    actual_ids = unique(catalogue.get('divergences'), 'id', 'divergence')
    expected_ids = {r['id'] for r in expected['divergences']}
    require(set(actual_ids) == expected_ids, 'omitted/orphan divergence ID')
    # A regenerate-and-compare contract also checks hashes, every witness, joins,
    # case/canary links, historical IDs, classifications and unknown route states.
    require(catalogue == expected, 'source/ledger inventory drift or unsupported qualification')
    return {'sources': len(actual_sources), 'population': len(expected['rust_and_ledger_population']),
            'witnesses': sum(len(s['witnesses']) for s in actual_sources.values()),
            'divergences': len(actual_ids),
            'classifications': dict(Counter(s['classification'] for s in actual_sources.values()))}


def manifest(root, catalogue):
    return {'schema': 1, 'scope': 'STATIC_SOURCE_LEDGER_INVENTORY',
            'catalogue': {'path': CATALOGUE, 'sha256': sha(encoded(catalogue))},
            'inputs': catalogue['inputs'],
            'counts': {'sources': len(catalogue['sources']),
                       'divergences': len(catalogue['divergences'])},
            'check': 'python3 scripts/tmux_shim_inventory.py check',
            'runtime_credit': False}


def check(root):
    catalogue = load_catalogue(root)
    counts = validate(root, catalogue)
    require(json.loads(read(root, MANIFEST)) == manifest(root, catalogue), 'stale shim manifest')
    require(read(root, CATALOGUE) == encoded(catalogue), 'noncanonical shim catalogue')
    missing = [r['locator'] for r in catalogue['ledger_references']
               if r['status'] in {'missing-source', 'missing-symbol'}]
    require(not missing, 'orphan ledger reference: ' + ', '.join(missing))
    return counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['check', 'extract'])
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        root = args.root.resolve()
        # An untracked .rs passes check today and breaks the catalogue at commit.
        require_no_untracked_rust(root)
        if args.command == 'extract':
            catalogue = generate(root)
            (root / CATALOGUE).parent.mkdir(parents=True, exist_ok=True)
            (root / CATALOGUE).write_bytes(encoded(catalogue))
            (root / MANIFEST).write_bytes(encoded(manifest(root, catalogue)))
        print(json.dumps(check(root), sort_keys=True))
    except (OSError, ValueError, KeyError, TypeError) as error:
        print('tmux shim inventory: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
