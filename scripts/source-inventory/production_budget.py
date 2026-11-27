#!/usr/bin/env python3
"""Production line budget: the deferred G target and the enforced ratchet.

The ratchet is the measured production of a reviewed commit. A candidate may
not exceed it unless the budget declares a growth envelope, which follows the
same justification and decision rules as a lot envelope. G is recorded only as
a target; this tool never claims it is met.
"""
import argparse
import json
from pathlib import Path
import sys

from inventory import TOOL, scan
from model import canonical
from production_policy import check_envelope

SCHEMA = 'rmux-source-budget/1'
BUDGET = TOOL / 'budgets.json'
G_STATUSES = ('met', 'not met')
RATCHET_KEYS = {'production_lines', 'measured_revision', 'production_growth_envelope'}
OPTIONAL_RATCHET_KEYS = {'production_growth_justification', 'production_growth_decision'}


def is_count(value):
    return type(value) is int and value >= 0


def is_commit(value):
    return (isinstance(value, str) and len(value) == 40
            and all(c in '0123456789abcdef' for c in value))


def load_budget(data):
    """Parse and validate budget bytes; reject any unknown or malformed field."""
    budget = json.loads(data)
    if not isinstance(budget, dict) or set(budget) != {'schema', 'unit', 'G', 'production_ratchet'}:
        raise ValueError('unsupported production budget fields')
    if budget['schema'] != SCHEMA or not isinstance(budget['unit'], str) or not budget['unit']:
        raise ValueError('unsupported production budget schema')
    target = budget['G']
    if (not isinstance(target, dict)
            or set(target) != {'production_lines', 'status', 'deferred_to', 'basis', 'decision'}
            or not is_count(target['production_lines']) or target['status'] not in G_STATUSES
            or any(not isinstance(target[k], str) or not target[k]
                   for k in ('deferred_to', 'basis', 'decision'))):
        raise ValueError('invalid G target')
    ratchet = budget['production_ratchet']
    if (not isinstance(ratchet, dict) or not RATCHET_KEYS <= set(ratchet)
            or set(ratchet) - RATCHET_KEYS - OPTIONAL_RATCHET_KEYS
            or not is_count(ratchet['production_lines'])
            or not is_commit(ratchet['measured_revision'])):
        raise ValueError('invalid production ratchet')
    check_envelope(0, ratchet)
    return budget


def production_of(report):
    if report.get('errors'):
        raise ValueError('production budget requires a fully classified inventory')
    return report['totals'].get('production', 0)


def budget_summary(report, budget):
    """Compare report with the ratchet and G; never raises on a ratchet excess."""
    ratchet = budget['production_ratchet']
    measured = production_of(report)
    delta = measured - ratchet['production_lines']
    try:
        check_envelope(delta, ratchet)
        violation = None
    except ValueError as error:
        violation = (f'production ratchet RED: {measured} lines at {report["revision"]}, '
                     f'ratchet {ratchet["production_lines"]}: {error}')
    return {'revision': report['revision'], 'production_lines': measured,
            'ratchet': ratchet['production_lines'], 'ratchet_revision': ratchet['measured_revision'],
            'envelope': ratchet['production_growth_envelope'], 'delta_to_ratchet': delta,
            'G': budget['G']['production_lines'], 'G_status': budget['G']['status'],
            'delta_to_G': measured - budget['G']['production_lines'],
            'ratchet_violation': violation}


def enforce(summary):
    if summary['ratchet_violation']:
        raise ValueError(summary['ratchet_violation'])
    return summary


def lowered_budget(report, budget):
    """Reset the ratchet to report; it may only stay equal or move down."""
    measured = production_of(report)
    ratchet = budget['production_ratchet']
    if measured > ratchet['production_lines']:
        raise ValueError(f'ratchet cannot rise: {measured} > {ratchet["production_lines"]}')
    lowered = {'production_lines': measured, 'measured_revision': report['revision'],
               'production_growth_envelope': 0}
    return dict(budget, production_ratchet=lowered)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path('.'))
    parser.add_argument('--revision', required=True)
    parser.add_argument('--parser', type=Path, required=True)
    parser.add_argument('--declarations', type=Path, default=TOOL / 'declarations.json')
    parser.add_argument('--budget', type=Path, default=BUDGET)
    parser.add_argument('--write', action='store_true',
                        help='record the revision as the new ratchet (never higher)')
    args = parser.parse_args()
    try:
        budget = load_budget(args.budget.read_bytes())
        policy_bytes = args.declarations.read_bytes()
        report = scan(args.repo, args.revision, args.parser, json.loads(policy_bytes), policy_bytes)
        if args.write:
            budget = lowered_budget(report, budget)
            args.budget.write_text(json.dumps(budget, indent=2, ensure_ascii=False) + '\n')
        print(canonical(enforce(budget_summary(report, budget))))
        return 0
    except (ValueError, KeyError, OSError) as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
