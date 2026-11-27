"""Resolve portable ledger locators without changing historical receipt bytes."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from provenance_archive import archived_bytes

COORDINATES = {'source', 'target', 'parser_retained_path', 'standalone_manifest',
               'compiler', 'local_evidence_mirror'}
# D-S1059-A: proof-consuming D65/D124 checks are an explicit external audit.
AUDIT_VARIABLE = 'RMUX_LEDGER_EVIDENCE'
AUDIT_SKIP = ('D-S1059-A: external D65/D124 ledger audit not run; set '
              'RMUX_LEDGER_EVIDENCE to the hash-addressed proof root and supply the '
              'ledger history (for example GIT_ALTERNATE_OBJECT_DIRECTORIES)')


def ledger_objects(value):
    """Every exact 40-digit Git object named by the ledger."""
    if isinstance(value, dict):
        return set().union(*map(ledger_objects, value.values()))
    if isinstance(value, list):
        return set().union(*map(ledger_objects, value))
    if isinstance(value, str) and re.fullmatch('[0-9a-f]{40}', value):
        return {value}
    return set()


def audit_root(repo, ledger):
    """Return the supplied external proof root, or None when no audit is requested.

    The ignored proof store and the out-of-HEAD history cannot exist in a fresh
    clone. Once requested, partial inputs fail; they never count as a rejection.
    """
    if AUDIT_VARIABLE not in os.environ:
        return None
    root = Path(os.environ[AUDIT_VARIABLE])
    if not root.is_absolute() or not root.is_dir():
        raise ValueError(AUDIT_VARIABLE + ' must name an absolute proof directory')
    missing = sorted(sha for sha in ledger_objects(ledger)
                     if subprocess.run(['git', '-C', str(repo), 'cat-file', '-e', sha],
                                       capture_output=True, check=False).returncode != 0)
    if missing:
        raise ValueError(f'ledger history unavailable: {len(missing)} objects, '
                         f'first {missing[0]}')
    return root


def origin(repo, ledger):
    return json.loads(archived_bytes(ledger['evidence_origin'], repo))


def coordinate(repo, ledger, locator):
    """Recover an original historical coordinate, not a current native image."""
    if not isinstance(locator, str) or not locator.startswith('ledger-origin:/'):
        raise ValueError('invalid ledger origin locator')
    if locator.rsplit('/', 1)[-1] not in COORDINATES:
        raise ValueError('ledger origin cannot replace proof bytes')
    value = origin(repo, ledger)
    for key in locator.removeprefix('ledger-origin:').split('/')[1:]:
        if not key or '~' in key:
            raise ValueError('invalid ledger origin pointer')
        try:
            value = value[int(key)] if isinstance(value, list) else value[key]
        except (KeyError, IndexError, ValueError, TypeError) as error:
            raise ValueError('unresolvable ledger origin locator') from error
    if not isinstance(value, str) or not value.startswith('/'):
        raise ValueError('ledger origin is not a historical coordinate')
    return value


def resolve(repo, ledger, locator, evidence_root=None):
    """Read a content-addressed proof, or resolve a sealed origin coordinate."""
    if isinstance(locator, str) and locator.startswith('ledger-origin:'):
        return coordinate(repo, ledger, locator)
    if not isinstance(locator, str) or not re.fullmatch('sha256:[0-9a-f]{64}', locator):
        raise ValueError('invalid ledger evidence locator')
    root = Path(evidence_root) if evidence_root is not None else (
        Path(repo) / '.rmux-audit/ledger-evidence/objects')
    path = root / locator.removeprefix('sha256:')
    try:
        if path.is_symlink() or root.resolve() != root.absolute():
            raise ValueError('redirected ledger evidence locator')
        data = path.read_bytes()
    except OSError as error:
        raise ValueError('unresolvable ledger evidence locator') from error
    if hashlib.sha256(data).hexdigest() != locator.removeprefix('sha256:'):
        raise ValueError('ledger evidence hash differs')
    return data


def validate(repo, ledger, evidence_root=None):
    """Verify projection and all proof links against the archived original."""
    if 'evidence_origin' not in ledger:
        validate_composed_growth(repo, ledger,
                                 lambda locator: resolve(repo, ledger, locator, evidence_root))
        return
    original = origin(repo, ledger)
    resolved = {}

    def proof(locator):
        if locator not in resolved:
            resolved[locator] = resolve(repo, ledger, locator, evidence_root)
        return resolved[locator]

    def references(value):
        if isinstance(value, dict):
            for child in value.values():
                references(child)
        elif isinstance(value, list):
            for child in value:
                references(child)
        elif isinstance(value, str) and value.startswith(('sha256:', 'ledger-origin:')):
            proof(value)

    references(ledger)

    def compare(old, new, pointer=''):
        if isinstance(old, dict):
            if not isinstance(new, dict) or old.keys() != new.keys():
                raise ValueError('historical ledger facts changed')
            for key in old:
                compare(old[key], new[key], pointer + '/' + key)
        elif isinstance(old, list):
            if not isinstance(new, list) or len(old) != len(new):
                raise ValueError('historical ledger facts changed')
            for index, (left, right) in enumerate(zip(old, new)):
                compare(left, right, pointer + '/' + str(index))
        elif isinstance(old, str) and old.startswith('/'):
            if new == 'ledger-origin:' + pointer:
                coordinate(repo, ledger, new)
            else:
                proof(new)
                # The original receipt's adjacent digest binds this exact link.
                parent = original
                for key in pointer.split('/')[1:-1]:
                    parent = parent[int(key)] if isinstance(parent, list) else parent[key]
                field = pointer.rsplit('/', 1)[-1]
                if field == 'build':
                    build = json.loads(proof(new))
                    receipt = json.loads(proof('sha256:' + parent['unit']['receipt_sha256']))
                    if (any(build.get(key) != parent[key]
                            for key in ('commit', 'os', 'source', 'target', 'sha256'))
                            or any(build.get(key) != receipt.get(key)
                                   for key in ('compiler_output', 'path', 'kind', 'sha256'))
                            or build.get('exit') != 0):
                        raise ValueError('ledger build differs from origin receipt')
                    return
                digest = parent.get('receipt_sha256' if field == 'receipt' else 'sha256', '')
                if new != 'sha256:' + digest:
                    raise ValueError('ledger evidence differs from origin digest')
        elif old != new or type(old) is not type(new):
            raise ValueError('historical ledger facts changed')

    for key in original:
        if key == 'entries':
            # Additional settlements remain subject to the normal D65 checks.
            compare(original[key], ledger[key][:len(original[key])], '/' + key)
        else:
            compare(original[key], ledger[key], '/' + key)
    ranges = ledger.get('reviewed_ranges', [])
    if ranges:
        rows = {row['lot']: row for row in ledger['entries'][1:]}
        required = {row['lot'] for row in original['entries'][1:]}
        supplied = {item['lot'] for item in ranges}
        if (len(supplied) != len(ranges) or not required <= supplied or not supplied <= set(rows)):
            raise ValueError('reviewed range bindings missing or duplicated')
        for item in ranges:
            row = rows[item['lot']]
            if (item['before'], item['after']) != (row['before'], row['after']):
                raise ValueError('reviewed range differs from closed settlement')
            mandate = json.loads(proof(item['mandate']))
            if mandate['id'] != row['lot']:
                raise ValueError('reviewed range mandate differs from closed lot')
            proof(item['review'])
            if row['lot'] not in required:
                authority = (json.loads(proof(item['inherited_mandate']))
                             if 'inherited_mandate' in item else mandate)
                envelope = authority.get('production_growth_envelope',
                                         authority.get('production_envelope_lines', 0))
                if (type(envelope) is not int
                        or row['mandate'].get('production_growth_envelope', 0) != envelope):
                    raise ValueError('settlement envelope differs from archived mandate')

    validate_composed_growth(repo, ledger, proof)


def validate_composed_growth(repo, ledger, proof):
    """Check the bounded D124 attribution; it never changes closed settlements."""
    from model import git

    growth = ledger.get('composed_growth')
    if growth is None:
        return
    before = 'f54ae83954c368c5c4e3706806690e0388c5414e'
    after = '7a63d529d7e6213f2e1410266734531e0b7555b8'
    if (growth.get('schema') != 1 or growth.get('total') != 913
            or (growth.get('before'), growth.get('after')) != (before, after)
            or growth.get('status') != 'OUVERT'
            or growth.get('campaign_consumed') is not False
            or growth.get('independent_review_pending') is not True):
        raise ValueError('invalid bounded D124 register')

    def ancestor(left, right):
        return git(repo, 'merge-base', left, right).decode().strip() == left

    inventories = {}

    def inventory(locator, revision):
        if locator not in inventories:
            inventories[locator] = json.loads(proof(locator))
        value = inventories[locator]
        if value.get('revision') != revision or value.get('errors') != []:
            raise ValueError('D124 inventory revision or qualification differs')
        return value

    def production_paths(value):
        return {row['path']: row['counts'].get('production', 0) for row in value['files']}

    old_total = inventory(growth['inventory_before'], before)['totals']['production']
    new_total = inventory(growth['inventory_after'], after)['totals']['production']
    if new_total - old_total != 913:
        raise ValueError('D124 total differs from its measured endpoints')
    total = 0
    covered = set()
    for row in growth['attachments']:
        if (row.get('status') != 'PASS' or row.get('owner') not in ('S1', 'S2', 'S3', 'S4', 'S6')
                or not row.get('lane') or not row['lane'].startswith(row['owner'] + '-')
                or type(row.get('delta')) is not int):
            raise ValueError('D124 attachment lacks its lane owner or PASS')
        mandate = json.loads(proof(row['mandate']))
        if mandate.get('role') != row['owner'] or mandate.get('id') != row['lane']:
            raise ValueError('D124 lane owner differs from its archived mandate')
        left, right, reviewed = row['before'], row['after'], row['reviewed_composition']
        if (any(not isinstance(sha, str) or not re.fullmatch('[0-9a-f]{40}', sha)
                for sha in (left, right, reviewed))
                or not ancestor(before, left) or not ancestor(left, right) or left == right
                or not ancestor(right, reviewed) or not ancestor(reviewed, after)):
            raise ValueError('D124 attachment leaves its reviewed composition')
        revisions = set(git(repo, 'rev-list', right, '^' + left).decode().splitlines())
        if revisions & covered:
            raise ValueError('duplicate D124 attachment')
        covered.update(revisions)
        receipt = proof(row['review']).decode()
        if (receipt.strip().splitlines()[-1] != 'PASS'
                or row['review_id'] not in receipt or not row['review_id'].startswith('R1-')):
            raise ValueError('D124 independent composition PASS missing')
        composition = json.loads(proof(row['composition']))
        endpoint = composition.get('candidate', composition.get('head'))
        if endpoint != reviewed:
            raise ValueError('D124 reviewed composition binding differs')
        # The unaltered composition manifest is named by the independent review.
        if 'COMPOSITION' not in receipt or reviewed[:8] not in receipt:
            raise ValueError('D124 PASS does not cite the exact composition')
        if 'chains' in composition:
            matches = [lane for lane in composition['chains']
                       if lane['label'] == row['composition_lane']
                       and (lane['start'], lane['end']) == (left, right)]
            if (len(matches) != 1 or composition.get('adaptations')
                    or matches[0].get('exit') != 0):
                raise ValueError('D124 lane differs from reviewed composition chain')
        elif 'source_commits' in row:
            current = git(repo, 'rev-list', '--reverse', reviewed,
                          '^' + composition['base']).decode().splitlines()
            source = composition['chain']
            start = 0 if left == composition['base'] else current.index(left) + 1
            stop = current.index(right) + 1
            source_before = composition['source_base'] if start == 0 else source[start - 1]
            if (len(current) != len(source) or composition.get('range_diff_equal') != len(source)
                    or row['source_commits'] != source[start:stop]
                    or row['source_before'] != source_before or mandate.get('base') != source_before):
                raise ValueError('D124 source lane differs from the reviewed recomposition')
        elif composition.get('base') != left or right != reviewed:
            raise ValueError('D124 range differs from reviewed composition')
        old = inventory(row['inventory_before'], left)
        new = inventory(row['inventory_after'], right)
        old_paths, new_paths = production_paths(old), production_paths(new)
        paths = {path: new_paths.get(path, 0) - old_paths.get(path, 0)
                 for path in old_paths.keys() | new_paths.keys()
                 if new_paths.get(path, 0) != old_paths.get(path, 0)}
        if (row['paths'] != paths or sum(paths.values()) != row['delta']
                or new['totals']['production'] - old['totals']['production'] != row['delta']):
            raise ValueError('D124 attachment differs from measured production lines')
        if row['delta'] > 0:
            authority = json.loads(proof(row['growth_authority']))
            if (authority.get('role') != row['owner']
                    or authority.get('production_growth_envelope', 0) < row['delta']):
                raise ValueError('D124 positive attribution exceeds its unchanged envelope')
        total += row['delta']
    remainder = growth.get('open_remainder')
    if type(remainder) is not int or remainder < 0 or total + remainder != 913:
        raise ValueError('D124 913 differs from attachments plus open remainder')
    if (any(row.get('status') != 'OUVERT' for row in growth['open_ranges'])
            or sum(row['delta'] for row in growth['open_ranges']) != remainder):
        raise ValueError('D124 open remainder lacks its explicit ranges')
    for row in [*growth['zero_unsettled_ranges'], *growth['open_ranges']]:
        left, right = row['before'], row['after']
        if (row.get('status') != 'OUVERT' or not ancestor(before, left)
                or not ancestor(left, right) or left == right or not ancestor(right, after)):
            raise ValueError('D124 unreviewed range must remain open within its bounds')
        net = (inventory(row['inventory_after'], right)['totals']['production']
               - inventory(row['inventory_before'], left)['totals']['production'])
        if net != row['delta'] or row in growth['zero_unsettled_ranges'] and net != 0:
            raise ValueError('D124 open range differs from its measured remainder')
        revisions = set(git(repo, 'rev-list', right, '^' + left).decode().splitlines())
        if revisions & covered:
            raise ValueError('duplicate D124 open range')
        covered.update(revisions)
    if covered != set(git(repo, 'rev-list', after, '^' + before).decode().splitlines()):
        raise ValueError('D124 leaves an unaccounted composition range')
    histories = growth['historical_outside_ancestry']
    expected = {row['after']: row for row in ledger['reviewed_ranges']
                if not ancestor(row['after'], after)}
    if len(histories) != 8 or {row['after'] for row in histories} != set(expected):
        raise ValueError('D124 eight historical heads differ')
    for row in histories:
        closed = expected[row['after']]
        if (row.get('status') != 'HISTORIQUE_HORS_ASCENDANCE'
                or row.get('counted_in_913') is not False
                or row['lot'] != closed['lot'] or row['review'] != closed['review']):
            raise ValueError('D124 historical branch head cannot credit the central')
