"""Preserve qualified consumer versions before validation, metadata and Rust."""
import json
import os
from pathlib import Path
import re
import secrets
import time


def enabled(g, env=None):
    env = os.environ if env is None else env
    return g.host_os() == 'windows' and bool(env.get('RMUX_NATIVE_INPUT_MANIFEST'))


def session(g, ticket, env=None):
    import native_input_windows as native
    env = os.environ if env is None else env
    manifest = env.get('RMUX_NATIVE_INPUT_MANIFEST')
    digest = env.get('RMUX_NATIVE_INPUT_MANIFEST_SHA256')
    if not manifest or not digest:
        raise g.IdentityError('consumer native session missing before use')
    for receipt in ticket.get('native_inputs', {}).values():
        if (receipt.get('manifest') != manifest
                or receipt.get('manifest_sha256') != digest):
            raise g.IdentityError('consumer native session differs from retained inputs')
    record = native.verify_manifest(manifest, digest)
    if (record['head'] != ticket['commit'] or record['source'] != ticket['source']
            or env.get('RMUX_IDENTITY_HEAD', record['head']) != record['head']
            or env.get('RMUX_IDENTITY_SOURCE', record['source']) != record['source']):
        raise g.IdentityError('consumer native session belongs to another candidate')
    return native, manifest, digest, record


def retain(g, ticket, paths, phase):
    if not enabled(g):
        return
    native, manifest, digest, record = session(g, ticket)
    boundary = Path(record['audit']) / ('consumer-' + secrets.token_hex(16))
    boundary.mkdir()
    snapshot = native.new_snapshot(boundary, record['reserve_bytes'])
    directory = Path(ticket['directory'])
    from native_input_collector import stamp
    for path in paths:
        path = Path(path).absolute()
        if path.is_symlink() or not path.resolve(strict=True).is_relative_to(directory):
            raise g.IdentityError('consumer input outside observed project')
        if phase == 'resolved' and path.name != 'Cargo.lock':
            verify(g, ticket, path)
        before = list(stamp(path.stat()))
        snapshot.retain(path)
        if list(stamp(path.stat())) != before:
            raise g.IdentityError('consumer alias changed during retention')
        snapshot.files[str(path)]['windows_stamp'] = before
        if phase == 'resolved' and path.name != 'Cargo.lock':
            verify(g, ticket, path)
    for path in snapshot.files:
        snapshot.verify(path)
    receipt = dict(os='windows', cell=record['cell'], case=record['case'],
                   head=ticket['commit'], source=ticket['source'], phase=phase,
                   session=ticket['session'], directory=ticket['directory'],
                   generator=ticket['generator'], generator_sha256=ticket['generator_sha256'],
                   origin=ticket['origin'], blob=ticket['blob'],
                   manifest=manifest, manifest_sha256=digest,
                   at_ns=time.time_ns(), files=snapshot.files)
    path = boundary / 'before.json'
    native.write_once(path, receipt)
    ticket.setdefault('native_inputs', {})[phase] = dict(receipt, receipt=str(path))


def verify(g, ticket, key=None, env=None):
    if not enabled(g, env):
        if ticket.get('native_inputs'):
            raise g.IdentityError('consumer native session missing before use')
        return
    native, manifest, digest, record = session(g, ticket, env)
    phase = 'resolved' if ticket['phase'] == 'frozen' else 'initial'
    versions = ticket.get('native_inputs', {})
    if 'initial' not in versions or phase not in versions:
        raise g.IdentityError('consumer bytes were not retained before first use')
    expected_paths = {str(Path(ticket['directory']) / name) for name in ticket['files']}
    from native_input_collector import sha
    for name in ('initial', 'resolved') if phase == 'resolved' else ('initial',):
        receipt = versions[name]
        stored = dict(receipt)
        path = Path(stored.pop('receipt'))
        if (path.is_symlink() or not path.resolve(strict=True).is_relative_to(Path(record['audit']))
                or json.loads(path.read_text()) != stored
                or any(receipt.get(k) != v for k, v in dict(
                    head=ticket['commit'], source=ticket['source'], session=ticket['session'],
                    directory=ticket['directory'], generator=ticket['generator'],
                    generator_sha256=ticket['generator_sha256'], origin=ticket['origin'],
                    blob=ticket['blob'], phase=name, case=record['case'], cell=record['cell'],
                    manifest=manifest, manifest_sha256=digest).items())
                or set(receipt['files']) != expected_paths):
            raise g.IdentityError('consumer before-use boundary differs')
        for row in receipt['files'].values():
            obj = Path(row['object'])
            if obj.is_symlink() or sha(obj) != row['sha256']:
                raise g.IdentityError('consumer retained bytes changed or missing')
    active = versions[phase]
    snapshot = native.restore(active)
    paths = [str(key)] if key is not None else active['files']
    for path in paths:
        if path not in active['files']:
            raise g.IdentityError('consumer alias was not retained before use')
        snapshot.verify(path)


def launch_inputs(args, env, cwd):
    """Use only the active observed project, never an arbitrary cwd tree."""
    token = env.get('RMUX_IDENTITY_DOC_SESSION')
    if not token or not env.get('RMUX_IDENTITY_TARGET'):
        return ()
    if not re.fullmatch('[0-9a-f]{64}', token):
        raise RuntimeError('consumer launch has no qualified session token')
    import candidate_identity as g
    target = Path(env['RMUX_IDENTITY_TARGET'])
    path = target / 'identity-generators' / (token + '.json')
    # Generator tickets also cover build scripts and rustdoc. Their existing
    # retention paths remain authoritative when this is not a consumer ticket.
    ticket = json.loads(path.read_text())
    if (ticket.get('kind') != 'compile-consumer'
            or not ticket.get('native_inputs') and not env.get('RMUX_NATIVE_INPUT_MANIFEST')):
        return ()
    executable = Path(args[0]).resolve(strict=True)
    # Identity validation itself invokes Git. That command consumes no project
    # manifest and must not recursively validate the same consumer ticket.
    if executable not in (Path(ticket['cargo']), Path(ticket['compiler'])):
        return ()
    # A qualified consumer cannot opt out by dropping its child manifest.
    # Check the actual child environment before generation can validate using
    # the parent's environment, and before any launch boundary is allocated.
    verify(g, ticket, env=env)
    source, head, actual_target = g.context()
    if actual_target != target:
        raise g.IdentityError('consumer launch target differs')
    ticket = g.generation(source, head, target, token=token)
    cargo = executable == Path(ticket['cargo'])
    if cargo and Path(cwd or Path.cwd()).resolve(strict=True) != Path(ticket['directory']):
        raise g.IdentityError('consumer Cargo cwd differs')
    if cargo and ticket['phase'] == 'resolving' and list(args[1:]) != ['metadata', '--offline', '--format-version', '1']:
        raise g.IdentityError('consumer initial lock is only qualified for metadata')
    verify(g, ticket, env=env)
    return tuple(Path(ticket['directory']) / name for name in ticket['files'])
