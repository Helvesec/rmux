#!/usr/bin/env python3
"""Measure absolute D65 rules at FF; settle production at campaign/lot closure."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys

from compare import compare
from inventory import scan
from model import canonical, git, read_tree
import ledger_evidence
from production_policy import (GENERIC, TOKENS, REFERENCE_LINES, REFERENCE_REVISION,
                               check_envelope, check_freeze, check_ratchets, scan_blacklist)

TOOL = Path(__file__).resolve().parent
PREFIX = 'scripts/source-inventory/'
ENVELOPE_KEYS = ('production_growth_envelope', 'production_growth_justification',
                 'production_growth_decision')
MODES = ('ff', 'campaign', 'closure')


def document(tree, name):
    return json.loads(tree[PREFIX + name]['data'])


def envelope_fields(mandate):
    return {key: mandate[key] for key in ENVELOPE_KEYS if key in mandate}


# One authorized redaction: the archived lot-108 mandate quoted one personal home.
REDACTED_LOT = 'S1-L064-R1380-COLLECTOR-F1-F2-F3-CORRECTION-108'
REDACTED_FIELD = 'production_growth_justification'
PERSONAL_HOME = re.compile(r'/Users/[A-Za-z0-9._-]+/')
NEUTRAL_HOME = '$HOME/'


def redact_closed_entry(entry):
    """Return entry with only the authorized home prefix rewritten, if present."""
    mandate = entry.get('mandate')
    if entry.get('lot') != REDACTED_LOT or not isinstance(mandate, dict):
        return entry
    text = mandate.get(REDACTED_FIELD)
    if not isinstance(text, str) or len(PERSONAL_HOME.findall(text)) != 1:
        return entry
    return dict(entry, mandate=dict(mandate, **{REDACTED_FIELD: PERSONAL_HOME.sub(NEUTRAL_HOME, text)}))


def check_closed_entries(old_entries, entries):
    """Closed entries are immutable except for the single authorized redaction."""
    closed = entries[:len(old_entries)]
    if closed != old_entries and closed != [redact_closed_entry(row) for row in old_entries]:
        raise ValueError('closed production ledger entries changed')


def check_lot_bounds(repo, before, after, rows, mandates):
    """Bind settlements to reviewed ranges covering the exact composition."""
    read_tree(repo, before)
    read_tree(repo, after)
    if git(repo, 'merge-base', before, after).decode().strip() != before:
        raise ValueError('reviewed bounds leave the composition ancestry')
    if len({row['lot'] for row in rows}) != len(rows):
        raise ValueError('duplicate reviewed lot')
    composition = set(git(repo, 'rev-list', after, '^' + before).decode().splitlines())
    covered = set()
    for row in rows:
        lot = row['lot']
        mandate = mandates[lot]
        left, right = mandate['reviewed_before'], mandate.get('reviewed_after')
        if any(not isinstance(sha, str) or len(sha) != 40
               or any(c not in '0123456789abcdef' for c in sha) for sha in (left, right)):
            raise ValueError('reviewed bounds require exact full commit SHAs: ' + lot)
        # read_tree validates exact full commit SHAs, not mutable refs or trees.
        read_tree(repo, left)
        read_tree(repo, right)
        if left == right or git(repo, 'merge-base', left, right).decode().strip() != left:
            raise ValueError('reviewed bounds are not an advancing lot range: ' + lot)
        if (row['before'], row['after']) != (left, right):
            raise ValueError('settlement differs from reviewed bounds: ' + lot)
        revisions = set(git(repo, 'rev-list', right, '^' + left).decode().splitlines())
        if not revisions or not revisions <= composition or revisions & covered:
            raise ValueError('reviewed bounds do not uniquely cover composition: ' + lot)
        covered.update(revisions)
    # A settlement necessarily names a commit preceding its own ledger write.
    # Only ledger-only commits may follow the last reviewed source range.
    for revision in git(repo, 'rev-list', '--reverse', after, '^' + before).decode().splitlines():
        if revision in covered:
            continue
        parents = git(repo, 'rev-list', '--parents', '-n', '1', revision).decode().split()[1:]
        if len(parents) != 1:
            raise ValueError('uncovered composition merge')
        paths = git(repo, 'diff', '--name-only', parents[0], revision).decode().splitlines()
        if paths != [PREFIX + 'production-ledger.json'] or any(
                git(repo, 'merge-base', row['after'], revision).decode().strip() != row['after']
                for row in rows):
            raise ValueError('reviewed bounds leave uncovered composition changes')


def coverage_witness(repo, ledger, name, evidence_root=None):
    """Select a sealed historical witness; never extend it to current HEAD."""
    witness = ledger['coverage_witnesses'][name]
    bounds = json.loads(ledger_evidence.resolve(repo, ledger, witness['bounds'], evidence_root))
    ledger_evidence.resolve(repo, ledger, witness['review'], evidence_root)
    if (witness['before'], witness['after']) != (bounds['base'], bounds['head']):
        raise ValueError('witness differs from measured reviewed bounds')
    lots = witness['lots']
    if len(set(lots)) != len(lots) or len(witness['mandates']) != len(lots):
        raise ValueError('duplicate or missing witness lot')
    rows = [row for row in ledger['entries'] if row['lot'] in lots]
    if [row['lot'] for row in rows] != lots:
        raise ValueError('witness settlements missing or duplicated')
    measured_bounds = {row['id']: row for row in bounds['mandates_for_independent_review']}
    mandates = {}
    for row, locator in zip(rows, witness['mandates']):
        mandate = json.loads(ledger_evidence.resolve(repo, ledger, locator, evidence_root))
        measured = measured_bounds[row['lot']]
        if (mandate['id'] != row['lot'] or envelope_fields(mandate) != row['mandate']
                or (row['before'], row['after']) != (measured['before'], measured['after'])):
            raise ValueError('witness settlement differs from original mandate/bounds')
        mandates[row['lot']] = dict(mandate, reviewed_before=measured['before'],
                                   reviewed_after=measured['after'])
    check_lot_bounds(repo, witness['before'], witness['after'], rows, mandates)
    return witness['before'], witness['after'], rows, mandates


def validate_reference(repo, reference):
    if (reference['schema'] != 1 or reference['revision'] != REFERENCE_REVISION
            or reference['production_lines'] != REFERENCE_LINES):
        raise ValueError('immutable production reference changed')
    tree = read_tree(repo, REFERENCE_REVISION)
    names = [p[len(PREFIX):] for p in tree if p.startswith(PREFIX)
             and (('/' not in p[len(PREFIX):] and p.endswith('.py'))
                  or p.startswith(PREFIX + 'src/') and p.endswith('.rs'))]
    names += ['Cargo.toml', 'Cargo.lock']
    expected = {name: hashlib.sha256(tree[PREFIX + name]['data']).hexdigest()
                for name in sorted(names)}
    parser = {name: expected[name] for name in
              ('src/main.rs', 'src/cfg.rs', 'src/template.rs', 'Cargo.toml', 'Cargo.lock')}
    if reference['tool_inputs_sha256'] != expected or reference['parser_inputs_sha256'] != parser:
        raise ValueError('D57 reference input fingerprints changed')


def evaluate(repo, before, after, parser, mandates=(), *, freeze=False, record=None,
             mode='closure', evidence_root=None):
    """Read exact Git trees and rescan; never trust caller-supplied inventory totals."""
    if mode not in MODES:
        raise ValueError('unknown production gate mode')
    if mode == 'ff' and (mandates or record is not None or freeze):
        raise ValueError('FF mode cannot settle mandates, record a lot or check freeze')
    repo = Path(repo).resolve()
    trees = {after: read_tree(repo, after)}
    current = trees[after]
    policy_bytes = current[PREFIX + 'declarations.json']['data']
    declarations = json.loads(policy_bytes)
    reference = document(current, 'production-reference.json')
    validate_reference(repo, reference)
    blacklist = document(current, 'blacklist-policy.json')
    if (blacklist.get('adopted') is not True or blacklist.get('token_sha256') != [list(token) for token in TOKENS]
            or blacklist['generic_pattern'] != GENERIC):
        raise ValueError('adopted blacklist policy changed')
    ledger = document(current, 'production-ledger.json')
    ledger_evidence.validate(repo, ledger, evidence_root)
    if ledger['schema'] != 1 or not ledger['entries']:
        raise ValueError('missing production ledger')
    reports = {}

    def measured(revision):
        if revision not in reports:
            trees.setdefault(revision, read_tree(repo, revision))
            report = scan(repo, revision, parser, declarations, policy_bytes)
            if report['errors']:
                raise ValueError('inventory RED: ' + canonical(report['errors']))
            reports[revision] = report
        return reports[revision]

    def production(revision):
        return measured(revision)['totals'].get('production', 0)

    baseline = production(REFERENCE_REVISION)
    if baseline != REFERENCE_LINES:
        raise ValueError('reference production remeasurement differs')
    left, right = measured(before), measured(after)
    delta = compare(repo, left, right)
    check_ratchets(delta)
    hits = scan_blacklist(right, current)
    if hits:
        raise ValueError('absolute production blacklist RED: ' + canonical(hits))
    net = production(after) - production(before)
    old_tree = trees[before]
    ledger_path = PREFIX + 'production-ledger.json'
    old_entries = document(old_tree, 'production-ledger.json')['entries'] if ledger_path in old_tree else []
    entries = ledger['entries']
    check_closed_entries(old_entries, entries)
    for name in ('production-reference.json', 'blacklist-policy.json'):
        if PREFIX + name in old_tree and old_tree[PREFIX + name]['data'] != current[PREFIX + name]['data']:
            raise ValueError('adopted reference/policy changed')
    first = entries[0]
    parent = git(repo, 'rev-parse', REFERENCE_REVISION + '^').decode().strip()
    if (first['lot'] != 'INT145' or first['before'] != parent
            or first['after'] != REFERENCE_REVISION or first['delta'] != 0
            or first['before_production'] != REFERENCE_LINES
            or first['after_production'] != REFERENCE_LINES or first['mandate'] != {'production_growth_envelope': 0}):
        raise ValueError('INT145 initial settlement changed')
    if record is not None:
        if mandates:
            raise ValueError('record and closure mandates are mutually exclusive')
        if any(row['lot'] == record['id'] for row in entries):
            raise ValueError('lot already recorded')
        check_envelope(net, record)
        entries = [*entries, dict(lot=record['id'], before=before, after=after,
                                  before_production=production(before), after_production=production(after),
                                  delta=net, mandate=envelope_fields(record))]
        mandates = (dict(record, reviewed_before=before, reviewed_after=after),)
    seen = set()
    for row in entries:
        if any(type(row[key]) is not int for key in ('before_production', 'after_production', 'delta')):
            raise ValueError('ledger measurements must be integers')
        if row['lot'] in seen:
            raise ValueError('duplicate closed lot')
        seen.add(row['lot'])
        actual_left, actual_right = production(row['before']), production(row['after'])
        if (row['before_production'] != actual_left or row['after_production'] != actual_right
                or row['delta'] != actual_right - actual_left):
            raise ValueError('ledger delta does not match fresh measurements: ' + row['lot'])
        check_envelope(row['delta'], row['mandate'])
    if mode == 'ff':
        # D69/D89 defer unsettled composition accounting, never absolute rules.
        # Closed ledger facts above still have their measurements verified.
        result = dict(schema='rmux-production-gate/2', mode=mode,
                      before=before, after=after,
                      tree=git(repo, 'rev-parse', after + '^{tree}').decode().strip(),
                      production_before=production(before), production_after=production(after),
                      delta=net, envelope=None, reference=REFERENCE_LINES, balance=None,
                      settlement_checks='deferred', freeze=False, blacklist_matches=hits,
                      ledger_entries=len(entries), exit=0)
        return result, ledger, reports, delta
    new_entries = entries[len(old_entries):]
    # INT145 bootstraps the baseline; it is not a newly consumed candidate lot.
    new_entries = [row for row in new_entries if row['lot'] != 'INT145']
    supplied = {mandate['id']: mandate for mandate in mandates}
    if len(supplied) != len(mandates) or set(supplied) != {row['lot'] for row in new_entries}:
        raise ValueError('every new settlement requires its exact lot mandate')
    for row in new_entries:
        if row['mandate'] != envelope_fields(supplied[row['lot']]):
            raise ValueError('settlement envelope differs from lot mandate')
    check_lot_bounds(repo, before, after, new_entries, supplied)
    for row in new_entries:
        # Measure each reviewed lot separately: opposing train deltas cannot
        # pay for growth that exceeds this lot's own mandate.
        lot_delta = compare(repo, measured(row['before']), measured(row['after']))
        check_ratchets(lot_delta)
        check_envelope(production(row['after']) - production(row['before']),
                       supplied[row['lot']])
    envelope = sum(check_envelope(row['delta'], supplied[row['lot']]) for row in new_entries)
    if net > envelope:
        raise ValueError(f'train production RED: net delta {net} exceeds lot envelopes {envelope}')
    if sum(row['delta'] for row in new_entries) != net:
        raise ValueError('train delta is not fully settled by new lot deltas')
    balance = sum(row['delta'] for row in entries)
    if production(after) != REFERENCE_LINES + balance:
        raise ValueError('production balance does not reconcile with immutable reference')
    if freeze:
        check_freeze(production(after))
    result = dict(schema='rmux-production-gate/1', mode=mode, before=before, after=after,
                  production_before=production(before), production_after=production(after),
                  delta=net, envelope=envelope, reference=REFERENCE_LINES, balance=balance,
                  settlement_checks='verified', freeze=freeze, blacklist_matches=hits,
                  ledger_entries=len(entries), exit=0)
    return result, dict(ledger, entries=entries), reports, delta


def qualified_parser(repo, after, parser):
    sys.path.insert(0, str(repo / 'scripts'))
    import candidate_identity as ci
    owners = [parent for parent in parser.parents
              if (parent / 'identity-compilation-unit.json').is_file()]
    if len(owners) != 1:
        raise ValueError('parser must have one attested standalone target root')
    target = owners[0]
    identity = ci.verify_built(parser, repo, after, target)
    binding = ci.support('candidate_identity_manifest').binding(ci, repo, after, target)
    if not binding or binding['manifest'] != str(TOOL / 'Cargo.toml'):
        raise ValueError('parser lacks D57 standalone compilation binding')
    return identity


def main():
    args = argparse.ArgumentParser(description=__doc__)
    args.add_argument('--repo', type=Path, required=True)
    args.add_argument('--before', required=True)
    args.add_argument('--after', required=True)
    args.add_argument('--parser', type=Path, required=True)
    args.add_argument('--mandate', type=Path, action='append', default=[])
    args.add_argument('--record-mandate', type=Path)
    args.add_argument('--freeze', action='store_true')
    args.add_argument('--mode', choices=MODES, default='closure',
                      help='FF checks absolute rules; campaign/closure also settle production')
    args.add_argument('--evidence-root', type=Path,
                      help='hash-addressed ledger proofs (default .rmux-audit/ledger-evidence/objects)')
    args.add_argument('--output', type=Path, required=True)
    options = args.parse_args()
    try:
        repo, parser = options.repo.resolve(strict=True), options.parser.resolve(strict=True)
        if repo != TOOL.parents[1] or git(repo, 'rev-parse', 'HEAD').decode().strip() != options.after:
            raise ValueError('production gate must run from the exact candidate checkout')
        if git(repo, 'status', '--porcelain').strip():
            raise ValueError('dirty candidate source')
        identity = qualified_parser(repo, options.after, parser)
        mandates = [json.loads(path.read_text()) for path in options.mandate]
        record = json.loads(options.record_mandate.read_text()) if options.record_mandate else None
        evidence_options = dict(evidence_root=options.evidence_root) if options.evidence_root else {}
        result, ledger, reports, delta = evaluate(repo, options.before, options.after, parser,
                                                  mandates, freeze=options.freeze, record=record,
                                                  mode=options.mode, **evidence_options)
        if qualified_parser(repo, options.after, parser) != identity:
            raise ValueError('parser identity changed during measurement')
        options.output.mkdir(parents=True, exist_ok=False)
        for revision, report in reports.items():
            (options.output / (revision + '.json')).write_text(canonical(report) + '\n')
        (options.output / 'delta.json').write_text(canonical(delta) + '\n')
        result['parser_identity'] = identity
        (options.output / 'production.json').write_text(canonical(result) + '\n')
        if record is not None:
            (TOOL / 'production-ledger.json').write_text(json.dumps(ledger, indent=2, sort_keys=True) + '\n')
        print(canonical(result))
        return 0
    except (ValueError, KeyError, OSError) as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
