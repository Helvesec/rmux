"""Bind original workspace locks and keep their compiler closures distinct."""
import json
import os
from pathlib import Path
import subprocess

PARSER = 'scripts/source-inventory/Cargo.toml'
_parsed_toml = {}


def parsed_toml(g, path):
    """Parse the text read now; byte-identical text is parsed once per process.

    A transitive verification revisits the same original locks for every
    dependency. Each visit still reads the current file, so changed bytes are
    parsed afresh; only the pure parse of equal text is shared. Read-only.
    """
    text = Path(path).read_text()
    if text not in _parsed_toml:
        _parsed_toml[text] = g.support('toml_reader').loads(text)
    return _parsed_toml[text]


def root_binding(g, source, head):
    manifest, lock = source / 'Cargo.toml', source / 'Cargo.lock'
    for path in (manifest, lock):
        if path.resolve(strict=True) != path:
            raise g.IdentityError('workspace binding is redirected')
        g.tracked_input(path, source, head)
    return dict(head=head, source=str(source), manifest=str(manifest),
                manifest_sha256=g.file_hash(manifest), lock=str(lock), lock_sha256=g.file_hash(lock))


def available(g, source, head, target):
    """The common context admits exactly the root and the tracked source parser.

    This is not a search for other lockfiles. Explicit with-manifest contexts
    retain their single original workspace and their existing isolation.
    """
    unit = binding(g, source, head, target)
    if unit:
        return [unit]
    result = [root_binding(g, source, head)]
    parser = source / PARSER
    if parser.exists() or parser.is_symlink():
        result.append(describe(g, parser, source, head))
    return result


def registry_pin(g, package, source, head, target):
    """Resolve a package without merging or rewriting either original lock."""
    locks = [lock_path(g, source, head, target)]
    parser = source / PARSER
    if not binding(g, source, head, target) and (parser.exists() or parser.is_symlink()):
        locks.append(Path(describe(g, parser, source, head)['lock']))
    matches = []
    for lock in locks:
        packages = parsed_toml(g, lock)['package']
        found = [item for item in packages if item['name'] == package['name']
                 and item['version'] == package['version']
                 and item.get('source', '').startswith('registry+')]
        if len(found) > 1:
            raise g.IdentityError('external package has an ambiguous candidate lock pin')
        matches.extend(found)
    pins = {(item['source'], item['checksum']) for item in matches}
    if len(pins) != 1:
        raise g.IdentityError('external package is not pinned by the candidate lock')
    return matches[0]


def workspace_owner(g, path, source, head):
    for directory in (path.parent, *path.parent.parents):
        if not directory.is_relative_to(source):
            break
        owner = directory / 'Cargo.toml'
        if owner.is_file():
            g.tracked_input(owner, source, head)
            if directory == source or 'workspace' in parsed_toml(g, owner):
                return owner
    raise g.IdentityError('compiler input has no original workspace')


def input_contexts(g, proof, source, head, target):
    contexts = available(g, source, head, target)
    if proof['kind'] == 'registry':
        qualified = []
        for context in contexts:
            packages = parsed_toml(g, context['lock'])['package']
            matches = [p for p in packages if p['name'] == proof['package']
                       and p['version'] == proof['version']
                       and p.get('source') == proof['registry_source']
                       and p.get('checksum') == proof['archive_sha256']]
            if len(matches) == 1:
                qualified.append(context)
        return qualified
    unit = binding(g, source, head, target)
    if unit:
        return contexts
    if proof['kind'] != 'workspace':
        # Generated root consumers retain the root lock contract.
        return contexts[:1]
    manifest = Path(proof['manifest'])
    g.tracked_input(manifest, source, head)
    # Cargo's environment cannot move a tracked source into a different lock.
    owner = workspace_owner(g, Path(proof['input']), source, head)
    if owner != workspace_owner(g, manifest, source, head):
        raise g.IdentityError('compiler source and manifest belong to different workspaces')
    return [c for c in contexts if c['manifest'] == str(owner)]


def dependency_contexts(g, dependency, source, head, target):
    if 'lock_contexts' in dependency:
        return dependency['lock_contexts']
    # Legacy receipts keep their original single-lock interpretation.
    graph = json.loads(Path(dependency['receipt']).read_text())['graph']
    unit = binding(g, source, head, target)
    if graph.get('compilation_unit') != unit:
        raise g.IdentityError('legacy compiler workspace differs')
    return [unit or root_binding(g, source, head)]


def closure(g, proof, dependencies, generation, source, head, target):
    contexts = input_contexts(g, proof, source, head, target)
    producers = [d for d in dependencies if d.get('kind') != 'toolchain']
    if generation:
        producers.append(generation['producer'])
    for producer in producers:
        allowed = dependency_contexts(g, producer, source, head, target)
        contexts = [c for c in contexts if c in allowed]
    if not contexts:
        raise g.IdentityError('compiler dependencies cross original workspace locks')
    return contexts


def graph_binding(g, graph, source, head, target):
    if 'lock_contexts' not in graph:
        if 'lock_context_version' in graph or 'manifest_scope' in graph:
            raise g.IdentityError('compiler lock closure missing')
        return binding(g, source, head, target)
    contexts = graph['lock_contexts']
    admitted = available(g, source, head, target)
    scope = 'standalone' if binding(g, source, head, target) else 'common'
    if (graph.get('lock_context_version') != 1 or not contexts
            or graph.get('manifest_scope') != scope
            or contexts != [c for c in admitted if c in contexts]):
        raise g.IdentityError('compiler lock closure binding differs')
    return contexts[0] if contexts[0]['manifest'] != str(source / 'Cargo.toml') else None


def describe(g, manifest, source, head):
    manifest = Path(manifest).absolute()
    if (manifest.resolve(strict=True) != manifest or not manifest.is_relative_to(source)
            or manifest.name != 'Cargo.toml' or manifest == source / 'Cargo.toml'):
        raise g.IdentityError('standalone manifest is not a canonical candidate input')
    g.tracked_input(manifest, source, head)
    data = parsed_toml(g, manifest)
    if ('package' not in data or data.get('workspace') != {}
            or 'workspace' in data['package']):
        raise g.IdentityError('standalone manifest must own a single-package workspace')
    lock = manifest.with_name('Cargo.lock')
    if lock.resolve(strict=True) != lock:
        raise g.IdentityError('standalone lock is redirected')
    g.tracked_input(lock, source, head)
    return dict(head=head, source=str(source), manifest=str(manifest),
                manifest_sha256=g.file_hash(manifest), lock=str(lock), lock_sha256=g.file_hash(lock))


def binding(g, source, head, target):
    ticket = target / 'identity-compilation-unit.json'
    if not ticket.exists():
        return None
    try:
        if ticket.is_symlink():
            raise g.IdentityError('standalone compilation binding is redirected')
        record = json.loads(ticket.read_text())
        expected = describe(g, record['manifest'], source, head)
        if record != expected:
            raise g.IdentityError('standalone compilation binding differs')
        return expected
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise g.IdentityError('standalone compilation binding is invalid') from error


def lock_path(g, source, head, target):
    unit = binding(g, source, head, target)
    return Path(unit['lock']) if unit else source / 'Cargo.lock'


def initialize(g, unit, source, head, target):
    target.mkdir(parents=True, exist_ok=False)
    if unit is None:
        return
    if unit != describe(g, unit['manifest'], source, head):
        raise g.IdentityError('standalone compilation binding differs')
    with (target / 'identity-compilation-unit.json').open('x') as stream:
        stream.write(json.dumps(unit, sort_keys=True) + '\n')


def action(g, args):
    if len(args) < 3 or args[1] != '--':
        raise g.IdentityError('usage: candidate_identity.py with-manifest MANIFEST -- command ...')
    if any(os.environ.get(k) for k in ('RMUX_IDENTITY_HEAD', 'RMUX_IDENTITY_SOURCE',
                                      'RMUX_IDENTITY_TARGET')):
        raise g.IdentityError('standalone compilation requires a fresh identity context')
    source = Path(g.git(Path.cwd(), 'rev-parse', '--show-toplevel')).resolve()
    head = g.git(source, 'rev-parse', 'HEAD')
    g.source_identity(source, head)
    unit = describe(g, args[0], source, head)
    env = g.prepare(compilation_unit=unit)
    return subprocess.call(args[2:], env=env)
