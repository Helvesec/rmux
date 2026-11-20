#!/usr/bin/env python3
"""Offline corpus integrity only; no runtime/native conformance verdict."""
import argparse
import csv
import hashlib
import json
from pathlib import Path
import re
import sys

from kitty_corpus import (encoding, event_shape, finite_policies, flags, integer,
                          matches_event, negotiation, output_fields, require,
                          state_shape, unhex)

ROOT = Path(__file__).resolve().parents[1]
CORPUS = Path('tests/conformance/kitty_keyboard')
FILES = ('obligations.jsonl', 'vectors.jsonl', 'coverage.json', 'policies.json')
# Explicit obligation-level selections in this corpus schema. Case-level choices
# do not imply a selection for their parent (e.g. R-FRAME and R-PRODUCER).
OBLIGATION_POLICIES = {
    'K-REPORT-TEXT': ('pinned', 'A-TEXT16'),
    'K-ERRATA': ('pinned', 'A-TAB'),
    'K-MOK': ('integration', 'A-MOK'),
    'R-BOUNDS': ('policy', 'A-BOUNDS'),
    'R-ROUTE': ('integration', 'A-ROUTE'),
    'R-LIFE': ('integration', 'A-LIFE'),
    'R-OUTER': ('integration', 'A-OUTER'),
}


def object_pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'schema: duplicate JSON key')
        result[key] = value
    return result


def read_json(raw):
    return json.loads(raw, object_pairs_hook=object_pairs)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def unique(rows, required):
    require(isinstance(rows, list) and rows, 'schema: nonempty population')
    result = {}
    for row in rows:
        require(isinstance(row, dict) and required <= row.keys(), 'schema: required fields')
        identity = row['id']
        require(isinstance(identity, str) and identity and identity not in result,
                'schema: duplicate/invalid ID')
        result[identity] = row
    return result


def text(value):
    return isinstance(value, str) and bool(value.strip())


def links(value, population, label):
    require(isinstance(value, list) and value and all(text(v) for v in value) and
            len(value) == len(set(value)) and set(value) <= population.keys(), label)


def anchor(value, pin, reference, seen):
    require(isinstance(value, dict) and {'lines', 'uri', 'sha256'} <= value.keys(),
            'source: complete anchor required')
    span = value['lines']
    require(isinstance(span, list) and len(span) == 2 and all(
        integer(n, 1, pin['lines']) for n in span) and span[0] <= span[1],
        'source: anchor range')
    a, b = span
    require(isinstance(value['uri'], str) and
            value['uri'] == pin['provenance']['immutable_blob_uri'] + f'#L{a}-L{b}',
            'source: immutable anchor URI')
    require(isinstance(value['sha256'], str) and re.fullmatch('[0-9a-f]{64}', value['sha256']),
            'source: anchor digest')
    old = seen.setdefault((a,b), value['sha256'])
    require(old == value['sha256'], 'source: conflicting anchor digests')
    if reference is not None:
        require(digest(b''.join(reference[a-1:b])) == value['sha256'],
                'source: anchor digest mismatch')


def anchors(value, pin, reference, seen):
    if isinstance(value, list):
        for child in value:
            anchors(child, pin, reference, seen)
    elif isinstance(value, dict):
        if {'lines', 'uri', 'sha256'} <= value.keys():
            anchor(value, pin, reference, seen)
        for key, child in value.items():
            if key == 'source' and child is not None:
                anchor(child, pin, reference, seen)
            elif key == 'sources':
                require(isinstance(child, list) and child, 'source: anchor list')
                for item in child:
                    anchor(item, pin, reference, seen)
            anchors(child, pin, reference, seen)


def metadata(pin, vectors, obs, policies):
    """Typed metadata boundaries; source observations are not Kitty anchors."""
    require(OBLIGATION_POLICIES.keys() <= obs.keys(), 'policy: selected obligations')
    for population in (obs, vectors):
        for row in population.values():
            require(text(row['authority']) and row['authority'] in pin['schema']['authority_values'],
                    row['id'] + ': schema: authority')
            decision = row['decision']
            require(decision is None or (text(decision) and decision in policies),
                    row['id'] + ': policy: unknown link')
    for ob in obs.values():
        require(text(ob['title']) and text(ob['claim']), 'schema: obligation title/claim')
        links(ob['cases'], vectors, 'schema: obligation case links')
        if ob['authority'] in ('pinned', 'ambiguity'):
            require(isinstance(ob['source'], dict), 'source: obligation anchor required')
        selected = OBLIGATION_POLICIES.get(ob['id'])
        require(ob['decision'] == (selected[1] if selected else None),
                ob['id'] + ': policy: obligation selection')
        if selected:
            require(ob['authority'] == selected[0], 'schema: selected obligation authority')
    for row in vectors.values():
        links(row['obligations'], obs, 'schema: obligation links')
        require(text(row['target']) and text(row['capability']), 'schema: target/capability')
        if row.get('policy_output_hex') is not None:
            require(row['authority'] == 'ambiguity', 'policy: selected output authority')
        observed = row['authority'] == 'source-observation'
        require(observed == (row['direction'] == 'current-source-control'),
                'source: observation authority/direction')
        if observed:
            proof = row.get('proof')
            require(row['decision'] is None and isinstance(proof, dict) and
                    proof.get('owner') == pin['authority']['owner'] + '#/source_observations' and
                    proof.get('status') == 'preserved original execution; no new runtime execution',
                    'source: observation proof link')


def load(directory, reference=None):
    pin = read_json((directory / 'pin.json').read_bytes())
    require(pin['schema_version'] == 1 and pin['version'] == 'rmux-kitty-keyboard-v1',
            'pin: schema/version')
    require(pin['scope'] == 'reviewed corpus integrity and selected design policies; runtime/native conformance unimplemented/unqualified',
            'pin: qualification scope')
    require(pin['authority']['owner'] == (CORPUS / 'pin.json').as_posix(), 'pin: owner')
    require(set(pin['population']) == set(FILES), 'pin: exact fixture population')
    source = pin['pin']
    require(re.fullmatch('[0-9a-f]{40}', source['revision']) and
            re.fullmatch('[0-9a-f]{64}', source['sha256']) and
            integer(source['bytes'], 1, 1000000) and integer(source['lines'], 1, 10000),
            'pin: source identity')
    require(source['official_repository'] == 'https://github.com/kovidgoyal/kitty' and
            source['path'] == 'docs/keyboard-protocol.rst' and
            source['canonical_uri'] == 'https://sw.kovidgoyal.net/kitty/keyboard-protocol/',
            'pin: official provenance')
    suffix = source['revision'] + '/' + source['path']
    require(source['provenance']['immutable_blob_uri'] == source['official_repository'] + '/blob/' + suffix
            and source['provenance']['immutable_raw_uri'] == 'https://raw.githubusercontent.com/kovidgoyal/kitty/' + suffix,
            'pin: immutable provenance')
    data = {}
    for name in FILES:
        raw = (directory / name).read_bytes()
        require(pin['population'][name] == {'sha256':digest(raw), 'bytes':len(raw)},
                'digest: fixture bytes differ: ' + name)
        data[name] = ([read_json(line) for line in raw.splitlines()] if name.endswith('.jsonl') else read_json(raw))
    source_lines = None
    if reference is not None:
        raw = reference.read_bytes()
        require(len(raw) == source['bytes'] and digest(raw) == source['sha256'], 'source: raw pin mismatch')
        source_lines = raw.splitlines(keepends=True)
        require(len(source_lines) == source['lines'], 'source: line count')
    return pin, data, source_lines


def reference_tables(lines, rows):
    """Optional source-table comparison, using already acquired bytes only."""
    text = b''.join(lines).decode('utf-8')
    table = text.split('.. start functional key table',1)[1].split('.. end functional key table',1)[0]
    expected = set()
    for line in table.splitlines():
        if not line.strip().startswith('"'):
            continue
        cols = next(csv.reader([line.strip()], skipinitialspace=True))
        for name, value in zip(cols[::2], cols[1::2]):
            for form in value.replace('`','').split(' or '):
                number, final = form.split()
                expected.add((name, int(number), final))
    actual = {(r['event']['key'], r['wire_identity']['number'], r['wire_identity']['final'])
              for r in rows if 'wire_identity' in r}
    require(actual == expected, 'source: functional table differs')
    table = text.split('.. start ctrl mapping',1)[1].split('.. end ctrl mapping',1)[0]
    expected = {}
    for line in table.splitlines():
        if line.strip().startswith('"'):
            cols = next(csv.reader([line.strip()], skipinitialspace=True))
            for key, value in zip(cols[::2], cols[1::2]):
                key = ' ' if key.strip() == 'SPC' else key.replace('\\\\','\\')
                expected[ord(key)] = bytes([int(value)]).hex()
    for key, value in expected.items():
        require(any(r.get('event',{}).get('key') == key and
                    r['event']['modifiers'] == 4 and r['event']['event_type'] == 1 and
                    r['direction'] == 'rmux-to-application' and flags(r['initial']) == 0 and
                    r['output_hex'] == value for r in rows), 'source: Ctrl table differs')


def validate(pin, data, source_lines=None):
    rows, obligations = data['vectors.jsonl'], data['obligations.jsonl']
    vectors = unique(rows, set(pin['schema']['vector_required']) |
                     {'id','obligations','direction','authority','decision','input_hex','output_hex','initial','next','target','capability'})
    obs = unique(obligations, {'id','title','source','claim','authority','decision','cases'})
    require(len(obs) == 32, 'coverage: obligation population')
    policies = unique(data['policies.json'], {'id','topic','selected_disposition','status'})
    required_policies = {'A-TEXT16','A-STACK','A-BOUNDS','A-UNKNOWN','A-TAB','A-OPTIONAL',
                         'A-LEGACY','A-MOK','A-ROUTE','A-LIFE','A-OUTER','A-TEXT-AVAIL',
                         'A-LICENSE','A-EVENT-LEGACY','A-C0-REPEAT'}
    require(set(policies) == required_policies, 'policy: required decisions')
    for policy in policies.values():
        require(isinstance(policy['selected_disposition'], str) and policy['selected_disposition'] and
                policy['status'] == 'sponsor-selected for candidate; distinct review and orchestrator acceptance pending',
                'policy: missing candidate disposition')
    metadata(pin, vectors, obs, policies)
    seen = {}
    for obj in (pin, data):
        anchors(obj, pin['pin'], source_lines, seen)
    identities = {}
    functional = [r for r in rows if 'wire_identity' in r]
    for row in functional:
        event, wire = row['event'], row['wire_identity']
        require(isinstance(event['key'], str) and set(wire) == {'number','final'}, 'schema: functional record')
        identities.setdefault(event['key'], set()).add((wire['number'],wire['final']))
        parsed = encoding(unhex(row['input_hex']))
        require((parsed['number'],parsed['final']) == (wire['number'],wire['final']),
                'semantic: functional wire identity')
    require(len(functional) == sum(map(len, identities.values())) == 117 and
            len(identities) == 111, 'partition: functional population')
    ctrl = [r for r in rows if r['direction'] == 'rmux-to-application' and
            (r.get('source') or {}).get('lines',[0])[0] in range(690,706)]
    require(len(ctrl) == 46 and len({r['event']['key'] for r in ctrl}) == 46,
            'partition: Ctrl table population')
    stream = b'\x1b[97;1:1u\x1b[97;1:2u\x1b[97;1:3u'
    splits = {len(unhex(r['chunks_hex'][0])) for r in rows
              if r['input_hex'] == stream.hex() and len(r.get('chunks_hex',[])) == 2}
    require(splits == set(range(1,len(stream))), 'partition: three-event stream splits')
    for ob in obligations:
        require(ob['cases'] and len(ob['cases']) == len(set(ob['cases'])) and
                set(ob['cases']) == {r['id'] for r in rows if ob['id'] in r['obligations']},
                'coverage: obligation cases differ')
    for row in rows:
        try:
            require(row['direction'] in {'event-reencode','terminal-to-rmux','application-negotiation',
                    'rmux-to-application','rmux-integration','rmux-to-outer-terminal',
                    'outer-terminal-to-client','current-source-control'}, 'schema: direction')
            decision = row['decision']
            if row['authority'] in ('policy','integration','ambiguity') or 'profile' in row:
                require(decision in policies, 'policy: required non-null link')
            if row['authority'] == 'ambiguity':
                require(row['output_hex'] is None and row.get('policy_output_hex') is not None,
                        'policy: normative and selected outputs must be separate')
            state_shape(row['initial']); state_shape(row['next'])
            raw = unhex(row['input_hex'])
            for field in ('output_hex','policy_output_hex'):
                if row.get(field) is not None:
                    unhex(row[field])
            if 'chunks_hex' in row:
                require(b''.join(unhex(x) for x in row['chunks_hex']) == raw, 'schema: chunk reconstruction')
            if row.get('input_validation') in ('reject-encoding', 'policy-limit-encoding'):
                try:
                    encoding(raw)
                except ValueError as error:
                    require(row['output_hex'] == '' and row['initial'] == row['next'],
                            'semantic: atomic frame rejection')
                    if row['input_validation'] == 'policy-limit-encoding':
                        require(str(error) == 'policy: text scalar limit' and
                                row.get('source_validity') == 'valid-but-policy-limited',
                                'policy: valid text limit differs from invalid syntax')
                else:
                    raise ValueError('semantic: invalid encoding accepted')
            for event in ([row['event']] if 'event' in row else []) + row.get('events',[]):
                event_shape(event, identities)
            if row['direction'] in ('terminal-to-rmux','event-reencode') and 'event' in row and raw.startswith(b'\x1b['):
                matches_event(encoding(raw), row['event'], identities)
            if raw == stream and 'events' in row:
                require(len(row['events']) == 3, 'schema: stream events')
                for frame, event in zip(raw.split(b'u')[:-1], row['events']):
                    matches_event(encoding(frame+b'u'), event, identities)
            if row['direction'] in ('event-reencode','rmux-to-application'):
                output_fields(row, identities)
            if row['direction'] == 'application-negotiation' and re.fullmatch(rb'(?:\x1b\[[=<>?][0-9;]*u)+', raw):
                if row.get('input_validation') == 'reject':
                    try:
                        negotiation(row['initial'], raw)
                    except ValueError:
                        require(row['initial'] == row['next'] and row['output_hex'] == '',
                                'semantic: atomic negotiation rejection')
                    else:
                        raise ValueError('semantic: invalid negotiation accepted')
                else:
                    state, output = negotiation(row['initial'], raw)
                    require(state == row['next'] and output.hex() == row['output_hex'], 'semantic: query/state mismatch')
        except (ValueError, KeyError, TypeError) as error:
            raise ValueError(row['id'] + ': ' + str(error)) from error
    coverage = data['coverage.json']
    require(coverage['vectors'] == len(rows) and coverage['obligations'] == len(obs), 'coverage: counts')
    require(coverage['authority_counts'] == {a:sum(r['authority']==a for r in rows)
            for a in {r['authority'] for r in rows}}, 'coverage: authority counts')
    require(len(coverage['sections']) == 22 and len({s['line'] for s in coverage['sections']}) == 22,
            'coverage: source sections')
    for section in coverage['sections']:
        links(section['obligations'], obs, 'coverage: section links')
        if source_lines is not None:
            require(source_lines[section['line']-1].decode().strip() == section['title'], 'source: section title')
    require(len(coverage['flags']) == 32 and {r['mask'] for r in coverage['flags']} == set(range(32)), 'partition: flag masks')
    for entry in coverage['flags']:
        f = entry['mask']
        require(entry['bits'] == [b for b in (1,2,4,8,16) if f&b], 'coverage: flag bits')
        cells = [vectors[i] for i in entry['cases']]
        require(len(cells) == 7 and len(set(entry['cases'])) == 7, 'partition: flag cases')
        query = [r for r in cells if r['direction']=='application-negotiation']
        require(len(query) == 1 and flags(query[0]['next']) == f, 'partition: flag query')
        events = [r for r in cells if r['direction']=='event-reencode']
        require({(r['event']['key'],r['event']['event_type']) for r in events} ==
                {(k,e) for k in (97,'UP') for e in (1,2,3)}, 'partition: flag events')
        for row in events:
            require(flags(row['initial']) == f, 'partition: event flags')
            require((row['output_hex'] is None) == (16 <= f <= 23), 'policy: undefined16 normative output')
            if 16 <= f <= 23:
                require(row['decision']=='A-TEXT16', 'policy: undefined16 link')
                peer = next(r for r in rows if r['direction']=='event-reencode' and r.get('event')==row['event'] and flags(r['initial'])==(f&~16))
                require(row['policy_output_hex']==peer['output_hex'], 'policy: undefined16 interpretation')
    partition = coverage['c0_repeat_partition']
    require(len(partition['cases']) == 54 and len(set(partition['cases'])) == 54 and
            set(partition['cases']) <= vectors.keys() and partition['reason'], 'coverage: C0 partition links')
    finite_policies(rows)
    if source_lines is not None:
        reference_tables(source_lines, rows)
    return len(rows), len(obs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus', type=Path, default=ROOT / CORPUS)
    parser.add_argument('--reference', type=Path, help='pre-acquired raw pinned source, verified offline')
    args = parser.parse_args()
    try:
        count, obligations = validate(*load(args.corpus, args.reference))
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        print('kitty-corpus-integrity-failed: ' + str(error), file=sys.stderr)
        return 1
    print(f'kitty-corpus-integrity-ok: {count} vectors, {obligations} obligations; runtime/native unqualified')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
