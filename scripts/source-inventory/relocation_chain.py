"""Explicit source epochs for the one independently proved module relocation."""
from datetime import datetime
from functools import lru_cache
import json
from pathlib import Path
import re
import subprocess

from model import canonical, digest, expand_declarations, git, read_tree

TOOL = Path(__file__).resolve().parent
CHAIN = 'scripts/source-inventory/relocation-revisions.json'
EPOCH = 'D161(1b)-normal-modules'
ACTIVATION = '330141c0eb1eeb3f709dc12172795a040dd21f05'
# Independent proof and every individual source identity are bound by this root.
# The published tree reseals ROOTS on the tracked public projection of its evidence.
ROOTS = ('9010d27f14a2cf271b0ecfb9e97a3f0efcd25aa376a51e4acbb315532303f30a',
         '3b58077631a03e6b5c24a4edb2a0896529fd445a85377e04e850c18d07dd8f9e')
FIELDS = ('kind', 'value', 'owner', 'condition', 'classification')
PROOF_REGISTRY = 'scripts/source-inventory/proof-epochs-d570.json'
# D572 activates explicit selection, not new semantic/size exemptions.
PROOF_ROOTS = ('f43421851ea63496ed662da171704baa5c39871ca4625807cca5cb621706426c',
               '2aadbb31759fd526cfc8aee6292eee7898fdc68b8959c31170408be77db8c78f',
               'dda052e58b0fb6a475909ac734967f099237f46be01c4b0dd31b3ec391da56c1',
               'b0502a03c0884824d12b956cd28ff8781312d72e4d811b693e5473eaa4d8a20d',
               'd0410d2bac913c69919b108a1f4fc7415dfbf63f7cb3be3aec5de69481093a1b')
PROOFS = 'scripts/source-inventory/relocation-proofs/'
# D573(3) option (i), coordination 2026-11-13T01:49:20Z: append-only runtime
# activations of sealed semantic transitions. Each root pins one entry, linked to the
# previous one; any undeclared transition still refuses.
SEMANTIC_ACTIVATIONS = (
    {'epoch': 'D573(3)-option-i-dfe42e88',
     'revision': 'dfe42e88ebe2c33b4bd5df50fbce5d82cd1ca1f0',
     'decision': 'D573(3) option (i), coordination 2026-11-13T01:49:20Z',
     'admission': ('RUNTIME_ACTIVATED for descendants of revision; whole transition only; '
                   'source signal closed as non-product-defect on S1763 evidence'),
     'previous_sha256': None,
     'proof_epoch': 'D570-f7f744fb',
     'proof_sha256': 'dda052e58b0fb6a475909ac734967f099237f46be01c4b0dd31b3ec391da56c1',
     'proof_admission': ('D573(2): declared 4A/3R (ProcessChild cfg_attr allow(dead_code) '
                         'and helper consumer lists)'),
     'consumer_additions': [
         {'path': 'tests/common/mod.rs', 'module_dir': 'tests/common',
          'condition': 'target_os = "linux"', 'consumer': 'tests/empty_socket_lock.rs:mod',
          'introduced_commit': 'ad9c897b3b271bd8fe460492ae9c0d27e8d49b7d',
          'reason': ('test(cli): restore cross-worktree empty endpoint regression; '
                     'Linux-only test crate using the shared allow(dead_code) helpers')},
         {'path': 'tests/common/mod.rs', 'module_dir': 'tests/common',
          'condition': 'unix', 'consumer': 'tests/window_json_layout.rs:mod',
          'introduced_commit': 'f6b223c4acf6d952bcff9825052e2f5b2352c2b7',
          'reason': ('test(cli): restore canonical window JSON layout regression and '
                     'neighbors; Unix test crate using the shared allow(dead_code) helpers')},
         {'path': 'tests/common_cross/mod.rs', 'module_dir': 'tests/common_cross',
          'condition': True, 'consumer': 'tests/server_invalid_flag.rs:mod',
          'introduced_commit': '046d96dc2961d5fdc1958966a200e5cd3e826bed',
          'reason': ('fix(server): restore invalid colon diagnostics on current head; '
                     'cross-platform test crate using the shared allow(dead_code) helpers')},
         {'path': 'tests/common/mod.rs', 'module_dir': 'tests/common',
          'condition': 'unix', 'consumer': 'tests/cli_command_lock_long_wait.rs:mod',
          'introduced_commit': '02525f59ec328a6ba51475847e0d5304c8cae531',
          'reason': ('test(harness): release the CLI command lock during long waits; '
                     'Unix test crate using the shared allow(dead_code) helpers; '
                     'D573 activation, coordination 2026-11-14T16:49:52Z')},
         {'path': 'crates/rmux-server/tests/common/mod.rs',
          'module_dir': 'crates/rmux-server/tests/common',
          'condition': 'unix', 'consumer': 'crates/rmux-server/tests/idle_exit_endpoint.rs:mod',
          'introduced_commit': '8ba18e2ed0b75ba268c77dabf8e5801e74729965',
          'reason': ('fix(server): release an idle daemon endpoint when it stops accepting; '
                     'Unix test crate using the shared allow(dead_code) helpers; '
                     'PROPOSED by S2030, awaiting a D573 decision')},
     ]},
)
ACTIVATION_ROOTS = ('fa405dcc84fedf00c7a12b61568427c33e5f610ab196750637e16892f49c8e19',)
# Tracked evidence equal to its pinned bytes is admitted as is. Otherwise only the exact
# "~/" home-prefix view of a committed original with the pinned hash is admitted.
HOME_PREFIX = re.compile(rb'/(?:Users|home)/[A-Za-z0-9._-]+/')


def public_view(data):
    """Tracked form of recorded evidence: each home-directory prefix becomes ``~/``."""
    return HOME_PREFIX.sub(b'~/', data)


def public_row(row):
    return json.loads(public_view(canonical(row).encode()))


@lru_cache(maxsize=None)
def committed_bytes(repo, revision, tracked):
    """Immutable bytes of ``tracked`` at an exact commit, or None where it is absent."""
    raw = subprocess.run(['git', '-C', repo, 'show', revision + ':' + tracked],
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return None if raw.returncode else raw.stdout


def committed_versions(repo, tracked):
    """Bytes of ``tracked`` at each HEAD commit touching it, oldest first (None: removed)."""
    return [committed_bytes(str(repo), revision, tracked) for revision in
            git(repo, 'log', '--format=%H', '--reverse', 'HEAD', '--', tracked).decode().split()]


def recorded_bytes(repo, tracked, current, expected):
    """Return the hashed recorded evidence whose public view is exactly ``current``."""
    if digest(current) == expected:
        return current
    for data in committed_versions(repo, tracked):
        if data is not None and digest(data) == expected and public_view(data) == current:
            return data
    raise ValueError('relocation independent proof hash differs: ' + Path(tracked).name)


def recorded_rows(historical, committed):
    """Rows for ROOTS: each tracked row, or the committed original it is the public view of."""
    rows = list(historical)
    pending = {index for index, row in enumerate(rows[:len(ROOTS)])
               if digest(canonical(row).encode()) != ROOTS[index]}
    for version in committed if pending else ():
        for index in sorted(pending):
            if (isinstance(version, list) and index < len(version)
                    and digest(canonical(version[index]).encode()) == ROOTS[index]
                    and public_row(version[index]) == rows[index]):
                rows[index] = version[index]
                pending.discard(index)
    return rows


def explicit_proof_epochs(repo, names):
    """Authenticate the append-only registry before selecting named proof inputs."""
    try:
        registry = json.loads((TOOL / Path(PROOF_REGISTRY).name).read_bytes())
        entries = registry['entries']
        if (registry['schema'] != 'rmux-d570-recorded-proof-epochs/1'
                or len(entries) != len(PROOF_ROOTS)):
            raise ValueError('altered or unsupported proof epochs')
        previous = None
        for row, expected in zip(entries, PROOF_ROOTS):
            content = {k: v for k, v in row.items() if k != 'sha256'}
            if (digest(canonical(content).encode()) != expected
                    or row['sha256'] != expected or row['previous_sha256'] != previous):
                raise ValueError('altered proof epoch or append-only link')
            previous = expected
        available = {row['epoch'] for row in entries}
        if len(set(names)) != len(names) or not set(names) <= available:
            raise ValueError('unknown or duplicate explicit proof epoch')
        history = git(repo, 'log', '--format=%H', '--reverse', 'HEAD', '--',
                      PROOF_REGISTRY).decode().split()
        prior = []
        for revision in history:
            raw = subprocess.run(['git', '-C', str(repo), 'show', revision + ':' + PROOF_REGISTRY],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if raw.returncode:
                raise ValueError('proof epoch registry removed from history')
            current = json.loads(raw.stdout)['entries']
            if current[:len(prior)] != prior or current != entries[:len(current)]:
                raise ValueError('proof epoch history is not append-only')
            prior = current
        if not history or prior != entries:
            raise ValueError('proof epochs differ from tracked history')
        return [row for row in entries if row['epoch'] in names]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        raise ValueError('missing or unreadable proof epochs') from error


def epoch_declarations(repo, revision, *, epochs):
    """Load only the selected revision's declarations; never change current policy."""
    selected = load_chain(repo, epochs=epochs).get('proof_epochs', [])
    if not selected or any(row['revision'] != revision for row in selected):
        raise ValueError('proof epoch selection must match the exact revision')
    path = 'scripts/source-inventory/declarations.json'
    raw = git(repo, 'show', selected[0]['declaration_base_revision'] + ':' + path)
    if any(digest(raw) != row['declaration_base_sha256'] for row in selected):
        raise ValueError('proof epoch declaration base differs')
    policy = json.loads(raw)
    for row in selected:
        for path, blobs in row['declaration_additions'].items():
            if blobs != [git(repo, 'rev-parse', revision + ':' + path).decode().strip()]:
                raise ValueError('proof epoch declaration source differs: ' + path)
            policy['documents'][path]['source_blobs'].extend(blobs)
    return canonical(policy).encode() if any(row['declaration_additions'] for row in selected) else raw


def ancestor(repo, left, right):
    result = subprocess.run(['git', '-C', str(repo), 'merge-base', '--is-ancestor',
                             left, right], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode not in (0, 1, 128):
        raise ValueError('cannot validate relocation activation ancestry')
    return result.returncode == 0


def attributes(row):
    return sorted(({k: a[k] for k in FIELDS} for a in row['semantic_attributes']),
                  key=canonical)


def semantic_key(path, attribute, consumers):
    return canonical([path, {k: attribute[k] for k in FIELDS}, consumers])


def semantic_identities(population):
    """Guarded semantic keys of ``{path: {semantic_attributes, consumers}}``."""
    from collections import Counter
    return Counter(semantic_key(path, attribute, row['consumers'])
                   for path, row in population.items()
                   for attribute in row['semantic_attributes']
                   if not (attribute['kind'] == 'path'
                           and attribute['classification'] == 'tests'))


def proved_semantics(record):
    """Guarded semantic keys of the independently proved activation snapshot."""
    return semantic_identities({path: dict(semantic_attributes=row['attributes'],
                                           consumers=row['consumers'])
                                for path, row in record['after'].items()})


def semantic_activations():
    """Authenticate the append-only activations against their pinned roots."""
    if len(SEMANTIC_ACTIVATIONS) != len(ACTIVATION_ROOTS):
        raise ValueError('altered or unsupported semantic activation')
    previous = None
    for row, expected in zip(SEMANTIC_ACTIVATIONS, ACTIVATION_ROOTS):
        if digest(canonical(row).encode()) != expected or row['previous_sha256'] != previous:
            raise ValueError('altered semantic activation or append-only link')
        previous = expected
    return SEMANTIC_ACTIVATIONS


def add_consumer(population, addition):
    """Insert one documented consumer into its single module group on every key of a path."""
    from collections import Counter
    result, matched = Counter(), False
    for key, count in population.items():
        path, attribute, groups = json.loads(key)
        if path == addition['path']:
            group = [g for g in groups if g['module_dir'] == addition['module_dir']
                     and g['condition'] == addition['condition']]
            if len(group) != 1 or addition['consumer'] in group[0]['consumers']:
                raise ValueError('semantic activation consumer group differs: ' + path)
            group[0]['consumers'] = sorted(group[0]['consumers'] + [addition['consumer']])
            key, matched = semantic_key(path, attribute, groups), True
        result[key] += count
    if not matched:
        raise ValueError('semantic activation consumer path absent: ' + addition['path'])
    return result


def admitted_semantics(repo, revision, record):
    """The one population admitted on ``revision``: the last activation on its ancestry.

    Each activation applies whole to the population before it; with none on the
    ancestry the proved population stands. Earlier populations are never admitted.
    """
    from collections import Counter
    population = proved_semantics(record)
    for entry in semantic_activations():
        if not ancestor(repo, entry['revision'], revision):
            continue
        proof, = explicit_proof_epochs(repo, (entry['proof_epoch'],))
        if proof['sha256'] != entry['proof_sha256']:
            raise ValueError('semantic activation proof epoch differs')
        removals, additions = (Counter({canonical(item['key']): item['count']
                                        for item in proof[name]})
                               for name in ('semantic_removals', 'semantic_additions'))
        if removals - population:
            raise ValueError('semantic activation removes an unadmitted identity')
        population = population - removals + additions
        for addition in entry['consumer_additions']:
            population = add_consumer(population, addition)
    from consumer_declarations import declared_consumers
    for addition in declared_consumers(repo, revision):
        population = add_consumer(population, addition)
    return population


def load_chain(repo, path=None, *, epochs=()):
    """The recognized hash is a trust anchor; a rewritten chain cannot self-certify."""
    path = Path(path) if path is not None else TOOL / 'relocation-revisions.json'
    try:
        tracked = path.read_bytes()
        historical = json.loads(tracked)
        if not isinstance(historical, list):
            raise ValueError('altered or unsupported relocation chain')
        proofs = explicit_proof_epochs(repo, epochs) if epochs else []
        committed = [historical if data == tracked else None if data is None else json.loads(data)
                     for data in committed_versions(repo, CHAIN)]
        recorded = recorded_rows(historical, committed)
        chain = recorded + proofs
        roots = ROOTS + tuple(digest(canonical(row).encode()) for row in proofs)
        if (len(chain) != len(roots)
                or any(digest(canonical(item).encode()) != expected
                       for item, expected in zip(chain, roots))):
            raise ValueError('altered or unsupported relocation chain')
        if (chain[1]['previous'] != ROOTS[0] or chain[1]['activation'] != ACTIVATION
                or chain[1]['epoch'] != EPOCH):
            raise ValueError('invalid append-only relocation supplement')
        record = dict(chain[0], **{key: chain[1][key] for key in
                                  ('manifest_sha256_before', 'manifest_sha256_after')})
        if (record['epoch'] != EPOCH or record['activation'] != ACTIVATION
                or record['candidate'] != ACTIVATION or record['previous'] is not None
                or record['decision'] != 'D161(1b)'):
            raise ValueError('invalid relocation epoch or activation')
        datetime.fromisoformat(record['date'])
        if not ancestor(repo, record['base'], record['activation']):
            raise ValueError('relocation activation leaves source ancestry')
        evidence = {name: recorded_bytes(repo, PROOFS + name,
                                         (path.parent / 'relocation-proofs' / name).read_bytes(),
                                         expected)
                    for name, expected in record['proofs'].items()}
        proof = json.loads(evidence['pass264.json'])
        if (proof['role'] != 'R2' or proof['verdict'] != 'PASS'
                or proof['candidate'] != record['candidate']
                or proof['base'] != record['base'] or proof['C2']['verdict'] != 'PASS'
                or proof['review_sha256'] != record['proofs']['review264.txt']):
            raise ValueError('relocation independent proof is not the admitted PASS')
        # Check every historical version, including a deletion followed by restoration.
        # A committed row may only be its recorded form or that row's tracked public view.
        prior = None
        for current in committed:
            if current is None:
                raise ValueError('relocation chain was removed from history')
            if (len(current) < len(prior or ()) or len(current) > len(historical)
                    or any(row not in (recorded[index], historical[index])
                           for index, row in enumerate(current))):
                raise ValueError('relocation chain history is not append-only')
            prior = current
        if prior is None:
            raise ValueError('missing committed relocation chain')
        if prior != historical:
            raise ValueError('relocation chain differs from tracked history')
        moves = record['moves']
        if (len(moves) != len(set(moves.values()))
                or len(record['sizes']) != 51
                or len({r['old'] for r in record['sizes']}) != 51
                or len({r['new'] for r in record['sizes']}) != 51):
            raise ValueError('relocation identity collision')
        if proofs:
            record['proof_epochs'] = proofs
        return record
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        raise ValueError('missing or unreadable relocation chain/proof') from error


def source_epoch(repo, revision, epoch=None):
    if epoch not in (None, 'historical', EPOCH):
        raise ValueError('unsupported relocation epoch')
    active = ancestor(repo, ACTIVATION, revision)
    if active and epoch != EPOCH:
        raise ValueError('relocation epoch and activation must be explicit')
    if epoch == EPOCH:
        record = load_chain(repo)
        if active and not ancestor(repo, record['activation'], revision):
            raise ValueError('relocation activation absent from ancestry')
    return active


def validate_snapshot(report, tree, record, side):
    """Retain each cfg/context/consumer occurrence; never compare an OR or total."""
    from inventory import targets
    expected = record[side]
    rust = [r for r in report['files'] if r['path'].endswith('.rs')]
    rows = {r['path']: r for r in rust}
    if (len(rows) != len(rust) or rows.keys() != expected.keys()
            or rows.keys() != {p for p in tree if p.endswith('.rs')}):
        raise ValueError('relocation file population missing, substituted or colliding')
    for path, row in rows.items():
        original = expected[path]
        if (attributes(row) != original['attributes']
                or row['consumers'] != original['consumers']
                or digest(tree[path]['data']) != original['sha256']
                or row['counts'] != original['counts']
                or row['physical_lines'] != original['physical_lines']):
            raise ValueError('relocation source/attribute/cfg/class/consumer identity differs: ' + path)
    manifests_hashes = {p: digest(v['data']) for p, v in tree.items()
                        if Path(p).name == 'Cargo.toml'}
    if manifests_hashes != record['manifest_sha256_' + side]:
        raise ValueError('relocation Cargo root/manifest source identity differs')
    raw = tree['scripts/source-inventory/declarations.json']['data']
    roots, manifests = targets(tree, expand_declarations(json.loads(raw)))
    if (canonical(roots) != canonical(record['roots_' + side])
            or manifests != record['manifests_' + side]
            or report['manifests'] != manifests):
        raise ValueError('relocation Cargo root or manifest identity differs')
    return rows


def activation_snapshot(repo, record):
    """Bind the independently proved semantics to their exact activation Git tree."""
    tree = read_tree(repo, record['activation'])
    report = {'manifests': record['manifests_after'], 'files': [
        dict(path=path, counts=row['counts'], physical_lines=row['physical_lines'],
             semantic_attributes=row['attributes'], consumers=row['consumers'])
        for path, row in record['after'].items()]}
    return validate_snapshot(report, tree, record, 'after'), tree


def validate_descendant(report, tree, record, admitted=None):
    """Apply ordinary semantic and size ratchets after the authenticated move.

    ``admitted`` is the one exact population allowed; by default the proved one.
    """
    rust = [row for row in report['files'] if row['path'].endswith('.rs')]
    rows = {row['path']: row for row in rust}
    if (len(rows) != len(rust)
            or rows.keys() != {p for p in tree if p.endswith('.rs')}):
        raise ValueError('descendant source population missing or colliding')
    if admitted is None:
        admitted = proved_semantics(record)
    if semantic_identities(rows) != admitted:
        raise ValueError('descendant semantic attribute/cfg/consumer identity differs')
    for item in record['sizes']:
        if item['new'] not in rows or rows[item['new']]['counts'].get('production', 0) <= 0:
            raise ValueError('descendant transported size identity missing or substituted')
    for path, row in rows.items():
        original = record['after'].get(path)
        count = row['counts'].get('production', 0)
        if original is None and count >= 600:
            raise ValueError('descendant new production file reaches 600: ' + path)
        if original and original['counts'].get('production', 0) >= 600:
            if count > original['counts']['production']:
                raise ValueError('descendant existing oversize growth: ' + path)


def transport(repo, before, after, old_tree, new_tree, epoch=None):
    """Return exact old keys to new keys (None means a proved normal module edge)."""
    old_active, new_active = [source_epoch(repo, r['revision'], epoch) for r in (before, after)]
    if old_active and not new_active:
        raise ValueError('reverse relocation epoch transport is unsupported')
    if not new_active:
        return {}, {}
    record = load_chain(repo)
    if after['revision'] == record['activation']:
        right = validate_snapshot(after, new_tree, record, 'after')
        activation_tree = new_tree
    else:
        right, activation_tree = activation_snapshot(repo, record)
        validate_descendant(after, new_tree, record,
                            admitted_semantics(repo, after['revision'], record))
    if old_active:
        return {}, {}
    if before['revision'] == record['base']:
        validate_snapshot(before, old_tree, record, 'before')
    mapping = {}
    for old_path, original in record['before'].items():
        new_path = record['moves'].get(old_path, old_path)
        current = record['after'][new_path]
        for attribute in original['attributes']:
            if attribute['kind'] == 'path' and attribute['classification'] == 'tests':
                continue
            old_key = semantic_key(old_path, attribute, original['consumers'])
            if attribute['kind'] == 'path' and attribute['classification'] == 'production':
                # Exact parent source bytes, all module consumers and all Cargo roots
                # have just been validated against the independently proved normal graph.
                destination, count = mapping.get(old_key, (None, 0))
                mapping[old_key] = (None, count + 1)
                continue
            moved = dict(attribute)
            if old_path == 'xtask/src/main.rs' and attribute['kind'] == 'path':
                if attribute['value'] == '../../crates/rmux-core/src/options/registry.rs':
                    moved['value'] = '../../crates/rmux-core/src/options/registry/mod.rs'
            if moved not in current['attributes']:
                raise ValueError('unproved semantic relocation identity')
            new_key = semantic_key(new_path, moved, current['consumers'])
            destination, count = mapping.get(old_key, (new_key, 0))
            if destination != new_key:
                raise ValueError('semantic relocation collision')
            mapping[old_key] = (new_key, count + 1)
    sizes = {}
    left = {r['path']: r for r in before['files']}
    for item in record['sizes']:
        old, new = item['old'], item['new']
        row = left.get(old)
        if (row and digest(old_tree[old]['data']) == item['sha256_before']
                and row['counts'] == item['counts_before']
                and row['physical_lines'] == item['physical_before']):
            current = right[new]
            if (digest(activation_tree[new]['data']) != item['sha256_after']
                    or current['counts'] != item['counts_after']
                    or current['physical_lines'] != item['physical_after']
                    or current['counts']['production'] > row['counts']['production']):
                raise ValueError('unproved relocation size/hash/count or oversize growth')
            sizes[new] = old
    return mapping, sizes


def core_after_paths(repo, revision, bounds, core, baseline, epoch=None):
    """The original twelve identities and bounds stay at their historical addresses."""
    if not source_epoch(repo, revision, epoch):
        return {path: path for path in bounds}
    record = load_chain(repo)
    original = {
        'crates/rmux-core/src/input/mod.rs': 811,
        'crates/rmux-core/src/screen.rs': 1293,
        'crates/rmux-core/src/screen/writer.rs': 720,
        'crates/rmux-server/src/handler.rs': 1050,
        'crates/rmux-server/src/handler_attach.rs': 1934,
        'crates/rmux-server/src/handler_client_runtime.rs': 703,
        'crates/rmux-server/src/handler_overlay.rs': 1261,
        'crates/rmux-server/src/handler_pane/attached_input.rs': 1309,
        'crates/rmux-server/src/pane_transcript.rs': 1195,
        'crates/rmux-server/src/renderer/pane_delta.rs': 981,
        'crates/rmux-server/src/terminal.rs': 891,
        'src/cli/dispatch.rs': 862,
    }
    if (bounds != original or tuple(core) != tuple(list(original)[:3])
            or baseline != 'f54ae83954c368c5c4e3706806690e0388c5414e'):
        raise ValueError('historical CORE identity or bound changed')
    result = {path: record['core_after'].get(path, path) for path in bounds}
    if len(result) != 12 or len(set(result.values())) != 12:
        raise ValueError('relocation CORE identity collision')
    return result
