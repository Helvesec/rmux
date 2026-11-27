#!/usr/bin/env python3
"""Reconcile raw physical-line changes; never award semantic reduction credit."""
import argparse
import collections
import difflib
import json
from pathlib import Path, PurePosixPath
import re
import sys
from model import CLASSES, MAINTAINED, canonical, digest, read_tree
from production_budget import budget_summary, is_count, load_budget
from relocation_chain import semantic_key, transport

MARKDOWN_DECISIONS = 'scripts/source-inventory/markdown-decisions.json'


def markdown_decisions(tree):
    """Read only the candidate's tracked, decision-backed Markdown paths."""
    if MARKDOWN_DECISIONS not in tree:
        return {}
    policy = json.loads(tree[MARKDOWN_DECISIONS]['data'])
    if (not isinstance(policy, dict) or set(policy) != {'schema', 'paths'}
            or type(policy['schema']) is not int or policy['schema'] != 1
            or not isinstance(policy['paths'], dict)):
        raise ValueError('unsupported Markdown decisions schema')
    for path, decision in policy['paths'].items():
        if (not isinstance(path, str) or not path.endswith('.md')
                or PurePosixPath(path).is_absolute() or '\\' in path
                or any(part in ('', '.', '..') for part in path.split('/'))
                or not isinstance(decision, str) or not re.fullmatch(r'D[1-9][0-9]*', decision)):
            raise ValueError('invalid Markdown path or decision: ' + path)
        if path not in tree:
            raise ValueError('decided Markdown path absent: ' + path)
    return policy['paths']


def line_roles(row):
    roles = []
    for region in row['regions']:
        if region['start'] != len(roles) + 1:
            raise ValueError('non-contiguous physical-line regions')
        roles.extend([region['class']] * (region['end'] - region['start'] + 1))
    if len(roles) != row['physical_lines']:
        raise ValueError('physical-line reconciliation failed')
    return roles


def semantic_population(report):
    if report.get('semantic_attributes_schema') != 1:
        raise ValueError('missing or unsupported semantic attribute inventory')
    counts = collections.Counter()
    examples = {}
    for row in report['files']:
        if not row['path'].endswith('.rs'):
            continue
        for attribute in row['semantic_attributes']:
            kind = attribute['kind']
            if kind not in ('path', 'allow_dead_code'):
                raise ValueError('unsupported semantic attribute kind')
            if kind == 'path' and attribute['classification'] == 'tests':
                continue
            key = semantic_key(row['path'], attribute, row.get('consumers', []))
            counts[key] += 1
            examples[key] = dict(attribute, path=row['path'])
    return counts, examples


def checked_tree(repo, report):
    if report['schema'] != 'rmux-source-inventory/1' or report['errors']:
        raise ValueError('cannot compare unsupported or unclassified inventory')
    tree = read_tree(repo, report['revision'])
    if {p: v['blob'] for p, v in tree.items()} != {r['path']: r['blob'] for r in report['files']}:
        raise ValueError('report is not bound to the exact Git input population')
    return tree


def population(key, values):
    return len(values) if key == 'markdown' else sum(r['occurrences'] for r in values)


def release_baseline(report):
    """Freeze the populations used by --check-ratchets. CI enforces the absolute size limit."""
    if report['errors'] or any(r['counts'].get('production', 0) >= 600 for r in report['files']):
        raise ValueError('release baseline needs a fully classified inventory below the size limit')
    counts, _ = semantic_population(report)
    return {'schema': 'rmux-source-ratchets/2', 'revision': report['revision'],
            'literals': {k: population(k, v) for k, v in report['ratchets'].items() if k != 'markdown'},
            'markdown_path_sha256': sorted(digest(path.encode()) for path in report['ratchets']['markdown']),
            'semantic_occurrences': sorted(digest(key.encode()) for key in counts.elements())}


def compare_release(repo, baseline, after):
    """Use the reviewed bounds when only the candidate Git tree is available."""
    if baseline['schema'] == 'rmux-source-ratchets/1':
        known_paths = {digest(path.encode()) for path in baseline['markdown']}
    elif baseline['schema'] == 'rmux-source-ratchets/2':
        known_paths = set(baseline['markdown_path_sha256'])
    else:
        raise ValueError('unsupported release ratchet baseline')
    tree = checked_tree(repo, after)
    decisions = markdown_decisions(tree)
    unapproved = sorted(path for path in set(after['ratchets']['markdown']) - decisions.keys()
                        if digest(path.encode()) not in known_paths)
    limits = baseline['literals']
    if set(limits) != after['ratchets'].keys() - {'markdown'} or not all(map(is_count, limits.values())):
        raise ValueError('release literal ratchet population mismatch')
    increases = [k for k, limit in limits.items() if population(k, after['ratchets'][k]) > limit]
    if not isinstance(baseline['semantic_occurrences'], list):
        raise ValueError('invalid release semantic occurrences')
    allowed = collections.Counter(baseline['semantic_occurrences'])
    counts, examples = semantic_population(after)
    additions = []
    for key, count in sorted(counts.items()):
        excess = count - allowed[digest(key.encode())]
        if excess > 0:
            additions.append(dict(examples[key], occurrences=excess))
    return {'schema': 'rmux-release-ratchet-check/1', 'before': baseline['revision'],
            'after': after['revision'], 'unapproved_markdown_additions': unapproved,
            'literal_ratchet_increases': increases + (['markdown'] if unapproved else []),
            'semantic_attribute_additions': additions,
            'production_size_violations': [r['path'] for r in after['files']
                                           if r['counts'].get('production', 0) >= 600]}


def compare(repo, before, after, *, epoch=None):
    if before['tool_inputs_sha256'] != after['tool_inputs_sha256']:
        raise ValueError('compare requires identical versioned measurement tool inputs')
    old_tree, new_tree = (checked_tree(repo, r) for r in (before, after))
    old_rows, new_rows = ({r['path']: r for r in report['files']} for report in (before, after))
    # Equal source/hash/counts never imply authority to move an existing guard.
    if epoch is None:
        removed = {entry['blob'] for path, entry in old_tree.items() if path not in new_tree}
        if any(path not in old_tree and row['blob'] in removed
               and row['counts'].get('production', 0) >= 600
               for path, row in new_rows.items()):
            raise ValueError('relocation requires an explicit epoch and activation')
    semantic_transport, size_transport = transport(repo, before, after, old_tree, new_tree, epoch)
    decided_markdown = markdown_decisions(new_tree)
    new_markdown = {path for path in new_tree.keys() - old_tree.keys() if path.endswith('.md')}
    unapproved_markdown = sorted(new_markdown - decided_markdown.keys())
    rows = []
    additions, removals = collections.Counter(), collections.Counter()
    reclassified = []
    transfers = collections.Counter()

    def transfer(left, right):
        if len(left) != len(right):
            raise ValueError('equal source lines have inconsistent physical populations')
        transfers.update((a, b) for a, b in zip(left, right) if a != b)
    for path in sorted(set(old_tree) | set(new_tree)):
        old = old_tree.get(path, {}).get('data', b'')
        new = new_tree.get(path, {}).get('data', b'')
        left, right = old_rows.get(path), new_rows.get(path)
        if old == new:
            if left and right and left['regions'] != right['regions']:
                reclassified.append(path)
                transfer(line_roles(left), line_roles(right))
            continue
        added, removed = collections.Counter(), collections.Counter()
        if (left and left.get('binary')) or (right and right.get('binary')):
            rows.append({'path': path, 'binary_byte_delta': len(new) - len(old)})
            continue
        old_lines, new_lines = old.split(b'\n'), new.split(b'\n')
        if old_lines[-1] == b'':
            old_lines.pop()
        if new_lines[-1] == b'':
            new_lines.pop()
        old_roles = line_roles(left) if left else []
        new_roles = line_roles(right) if right else []
        for tag, a, b, c, d in difflib.SequenceMatcher(None, old_lines, new_lines, autojunk=False).get_opcodes():
            if tag != 'equal':
                removed.update(old_roles[a:b])
                added.update(new_roles[c:d])
            else:
                transfer(old_roles[a:b], new_roles[c:d])
        additions.update(added)
        removals.update(removed)
        rows.append({'path': path, 'added': added, 'removed': removed})
    reconciled = collections.Counter(before['totals'])
    reconciled.update(additions)
    reconciled.subtract(removals)
    for (old_class, new_class), count in transfers.items():
        reconciled[old_class] -= count
        reconciled[new_class] += count
    if any(reconciled[c] != after['totals'].get(c, 0) for c in CLASSES):
        raise ValueError('raw changes and class transfers do not reconcile totals')
    size_violations = []
    for path, right in sorted(new_rows.items()):
        left = old_rows.get(size_transport.get(path, path))
        new_production = right['counts'].get('production', 0)
        old_production = left['counts'].get('production', 0) if left else 0
        if left is None and new_production >= 600:
            size_violations.append({'path': path, 'reason': 'new_production_file_ge_600',
                                    'before': None, 'after': new_production})
        elif old_production >= 600 and new_production > old_production:
            size_violations.append({'path': path, 'reason': 'existing_oversize_growth',
                                    'before': old_production, 'after': new_production})
    rats = {k: {'before': population(k, v), 'after': population(k, after['ratchets'][k])}
            for k, v in before['ratchets'].items()}
    old_attributes, old_examples = semantic_population(before)
    new_attributes, examples = semantic_population(after)
    normalized = old_attributes.copy()
    for key, (destination, limit) in semantic_transport.items():
        count = min(old_attributes[key], limit)
        normalized[key] -= count
        if destination is not None:
            normalized[destination] += count
    old_attributes = +normalized
    removals_semantic = []
    for key, count in sorted((old_attributes - new_attributes).items()):
        path, attribute, consumers = json.loads(key)
        removals_semantic.append(dict(attribute, path=path, occurrences=count))
    additions_semantic = [dict(examples[key], occurrences=count)
                          for key, count in sorted((new_attributes - old_attributes).items())]
    return {'schema': 'rmux-source-delta/1', 'before': before['revision'], 'after': after['revision'],
            'raw_added': additions, 'raw_removed': removals,
            'maintained_added_outside_production': sum(n for r, n in additions.items() if r in MAINTAINED and r != 'production'),
            'maintained_added_total': sum(n for r, n in additions.items() if r in MAINTAINED),
            'source_unchanged_reclassified': reclassified,
            'unchanged_line_class_transfers': [{'from': a, 'to': b, 'lines': n}
                for (a, b), n in sorted(transfers.items())],
            'class_totals_reconciled': True,
            'declaration_changed': before['declarations_sha256'] != after['declarations_sha256'],
            'ratchets': rats, 'production_size_violations': size_violations,
            'markdown_decisions': decided_markdown,
            'unapproved_markdown_additions': unapproved_markdown,
            'files': rows, 'credited_reduction': 0, 'G': None,
            'semantic_attribute_additions': additions_semantic,
            'semantic_attribute_removals': removals_semantic,
            'relocation_epoch': epoch,
            'size_identity_transports': size_transport,
            'literal_ratchet_increases': [key for key, count in rats.items()
                                         if (unapproved_markdown if key == 'markdown'
                                             else count['after'] > count['before'])],
            'credit_contract': 'Raw diffs are not semantic deletion. Reclassification, splitting, moving, formatting, minifying and generation award zero. Disjoint lower bounds, generated expansion evidence and a dated decision remain external requirements.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path('.'))
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--before', type=Path)
    source.add_argument('--release-baseline', type=Path,
                        help='frozen source ratchets for a single candidate inventory')
    parser.add_argument('--after', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--check-ratchets', action='store_true')
    parser.add_argument('--budget', type=Path,
                        help='production budget; with --check-ratchets, its ratchet is enforced')
    parser.add_argument('--relocation-epoch', choices=('historical', 'D161(1b)-normal-modules'))
    parser.add_argument('--claim-reduction', type=int, default=0)
    args = parser.parse_args()
    try:
        after = json.loads(args.after.read_text())
        if args.release_baseline is not None:
            if args.relocation_epoch:
                raise ValueError('relocation requires two complete inventories')
            result = compare_release(args.repo, json.loads(args.release_baseline.read_bytes()), after)
        else:
            result = compare(args.repo, json.loads(args.before.read_text()), after, epoch=args.relocation_epoch)
        if args.budget is not None:
            result['production_budget'] = budget_summary(after, load_budget(args.budget.read_bytes()))
        args.output.write_text(canonical(result) + '\n')
        if args.claim_reduction != 0:
            raise ValueError('this measurement tool cannot certify semantic reduction credit')
        if args.check_ratchets:
            reasons = []
            if result['literal_ratchet_increases']:
                reasons.append('literal ratchet increased: ' + ', '.join(result['literal_ratchet_increases']))
            if result['semantic_attribute_additions']:
                reasons.append('new semantic attribute: ' + ', '.join(sorted({
                    a['kind'] for a in result['semantic_attribute_additions']})))
            if args.relocation_epoch == 'D161(1b)-normal-modules' and result['semantic_attribute_removals']:
                reasons.append('removed semantic attribute')
            if args.budget is not None and result['production_budget']['ratchet_violation']:
                reasons.append(result['production_budget']['ratchet_violation'])
            if reasons:
                raise ValueError('; '.join(reasons))
        if args.check_ratchets and result['production_size_violations']:
            raise ValueError('production size ratchet violated: ' +
                             canonical(result['production_size_violations']))
        print(canonical({k: v for k, v in result.items() if k != 'files'}))
        return 0
    except (ValueError, KeyError) as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
