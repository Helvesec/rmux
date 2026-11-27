"""Retention boundaries for the two observed public API Windows consumers."""
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import secrets
import shutil
import time

CELLS = ('sdk', 'proto')
MISSING_LOADS = {'kernel.appcore.dll', 'ntmarta.dll', 'rsaenh.dll', 'vcruntime140.dll'}


def observed_bootstrap(snapshot, observation, bindings):
    """Retain each measured alias, then verify all bindings before any consumer."""
    loads = observation['loads']
    paths = [row['path'] for row in loads]
    if (not paths or len(set(p.casefold() for p in paths)) != len(paths)
            or set(paths) != set(bindings)
            or not MISSING_LOADS <= {PureWindowsPath(p).name.casefold() for p in paths}
            or any(not PureWindowsPath(p).is_absolute() for p in paths)):
        raise RuntimeError('Windows observed load profile differs from bounded bindings')
    stable = []
    aliases = {}
    for name in paths:
        path = Path(bindings[name]).absolute()
        snapshot.retain(path)
        key = str(path)
        stable.append(key)
        aliases[name] = dict(path=key, resolved=snapshot.files[key]['resolved'])
    for path in stable:
        snapshot.verify(path)
    snapshot.windows_aliases = aliases
    return stable


def profile_bindings(profile, roots):
    """Use exact observed roles; no system-wide DLL discovery or search fallback."""
    if profile.get('cells') != list(CELLS):
        raise RuntimeError('Windows profile is not the SDK/proto public API pair')
    bindings = {}
    for row in profile['loads']:
        role, relative = row['role'].split('/', 1)
        parts = relative.split('/')
        if role not in roots or any(p in ('', '.', '..') for p in parts):
            raise RuntimeError('Windows observed input role is not canonical')
        root = Path(roots[role]).resolve(strict=True)
        path = root.joinpath(*parts).absolute()
        if not path.resolve(strict=True).is_relative_to(root):
            raise RuntimeError('Windows observed input redirected outside qualified root')
        bindings[row['path']] = str(path)
    return bindings


def write_once(path, record):
    with Path(path).open('x', encoding='utf-8') as stream:
        json.dump(record, stream, sort_keys=True, indent=2)
        stream.write('\n')


def read_manifest(path, expected=None):
    path = Path(path).absolute()
    if path.resolve(strict=True) != path or path.is_symlink():
        raise RuntimeError('redirected Windows native input manifest')
    raw = path.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    if expected is not None and digest != expected:
        raise RuntimeError('Windows native input manifest changed')
    record = json.loads(raw)
    if record.get('os') != 'windows' or record.get('cell') not in CELLS:
        raise RuntimeError('unqualified Windows native input observation')
    return record, digest


def restore(record):
    from native_input_collector import Snapshot
    snapshot = object.__new__(Snapshot)
    snapshot.files = record['files']
    snapshot.absent_observed_aliases = {}
    snapshot.mutable_inputs = {}
    return snapshot


def new_snapshot(audit, reserve):
    from native_input_collector import Snapshot, stamp
    def floor():
        if shutil.disk_usage(audit).free < 50 * 2**30 + reserve:
            raise RuntimeError('Windows retention floor plus reserve unavailable')
    class WindowsSnapshot(Snapshot):
        def retain(self, path):
            if shutil.disk_usage(audit).free - Path(path).stat().st_size < 50 * 2**30 + reserve:
                raise RuntimeError('Windows input exceeds remaining retention budget')
            super().retain(path)
            self.files[str(Path(path).absolute())]['windows_stamp'] = list(stamp(Path(path).stat()))
            floor()
    return WindowsSnapshot(audit, floor)


def verify_manifest(path, expected):
    record, _ = read_manifest(path, expected)
    snapshot = restore(record)
    for key in snapshot.files:
        snapshot.verify(key)
    for alias in record['aliases'].values():
        if snapshot.files[alias['path']]['resolved'] != alias['resolved']:
            raise RuntimeError('Windows native alias binding changed')
    return record


def before_launch(args, env, generated=(), cwd=None):
    """An explicit Windows session protects inputs before each Python launch.

    An active native consumer cannot drop its opt-in in the child environment.
    Other non-opt-in and Unix launches preserve their existing semantics. This
    receipt is a boundary, never evidence of earlier interpreter loader reads.
    """
    manifest = env.get('RMUX_NATIVE_INPUT_MANIFEST')
    if not manifest:
        if os.name == 'nt':
            from native_input_windows_consumer import launch_inputs
            launch_inputs(args, env, cwd)
        return None
    if os.name != 'nt':
        raise RuntimeError('Windows native input session on another OS')
    digest = env.get('RMUX_NATIVE_INPUT_MANIFEST_SHA256')
    if not digest:
        raise RuntimeError('Windows native input manifest digest missing')
    record = verify_manifest(manifest, digest)
    if (env.get('RMUX_IDENTITY_HEAD', record['head']) != record['head']
            or env.get('RMUX_IDENTITY_SOURCE', record['source']) != record['source']):
        raise RuntimeError('Windows native input session has another candidate')
    executable = Path(args[0]).absolute()
    # A compiler-generated image needs its identity guard as well as byte
    # retention. Do not admit an arbitrary executable simply by copying it.
    target = env.get('RMUX_IDENTITY_TARGET')
    qualified = next((row for key, row in record['files'].items()
                      if key.casefold() == str(executable).casefold()), None)
    if qualified is None:
        if not target or not executable.resolve(strict=True).is_relative_to(Path(target)):
            raise RuntimeError('Windows launch image lacks before-use qualification')
        import candidate_identity as guard
        from native_input_windows_buildscript import qualified
        source = Path(record['source'])
        if not qualified(guard, executable, source, record['head'], Path(target), env):
            guard.verify_built(executable, source, record['head'], Path(target))
    from native_input_collector import now
    audit = Path(record['audit'])
    boundary = audit / ('boundary-' + secrets.token_hex(16))
    boundary.mkdir()
    snapshot = new_snapshot(boundary, record['reserve_bytes'])
    for path in (executable, *generated):
        snapshot.retain(path)
    at_ns = time.time_ns()
    receipt = dict(os='windows', cell=record['cell'], head=record['head'],
                   source=record['source'], case=record['case'], at=now(), at_ns=at_ns,
                   manifest=manifest, manifest_sha256=env['RMUX_NATIVE_INPUT_MANIFEST_SHA256'],
                   files=snapshot.files, executable=str(executable),
                   executable_sha256=snapshot.files[str(executable)]['sha256'])
    write_once(boundary / 'before.json', receipt)
    return receipt


def files_before_use(record, additional=()):
    """Index immutable byte objects and their original first-use boundaries."""
    indexed = {}
    for entry in (record, *additional):
        for key, row in entry['files'].items():
            indexed.setdefault(key.casefold(), []).append(dict(
                **row, before_ns=entry['at_ns'], case=entry['case'], head=entry['head']))
    return indexed


def require_retained(indexed, path, at_ns, case, head):
    from native_input_collector import sha
    candidates = sorted(indexed.get(path.casefold(), []),
                        key=lambda row: row['before_ns'], reverse=True)
    for row in candidates:
        obj = Path(row['object'])
        if (row['before_ns'] <= at_ns and row['case'] == case and row['head'] == head
                and not obj.is_symlink() and sha(obj) == row['sha256']):
            return row
    raise RuntimeError('Windows image was not retained before its native use: ' + path)
