"""Gate-local proofs, reusable only while every observed file stamp is unchanged."""
import hashlib
import marshal
import os
from pathlib import Path
import stat
import sys
from threading import RLock

_observations = []
_MISSING = object()
# One process-local lock also protects the nested observation stack. Every
# cached publisher/reuser holds it through validation, replacement and cleanup.
# This does not serialize independent processes or external filesystem writers.
_cache_lock = RLock()


class Observations(dict):
    def __init__(self):
        super().__init__()
        self.contents = {}


def content_hash(path):
    with open(native_path(path), 'rb') as stream:
        digest = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.digest()


def record_observation(g, evidence, digest):
    for observations in _observations:
        previous = observations.setdefault(evidence['path'], evidence)
        contents = observations.contents.setdefault(evidence['path'], digest)
        if previous != evidence or contents != digest:
            raise g.IdentityError('identity evidence changed during verification')


def verify_built(g, binary, source, head, target):
    source, _ = g.source_identity(source, head)
    target = g.target_identity(source, head, target)
    return cached(g, 'built', binary, source, head, target,
                  lambda: g.support('candidate_identity_retention').verify_built(
                      g, binary, source, head, target))


def native_path(path):
    if os.name != 'nt':
        return os.fspath(path)
    text = os.fspath(path)
    if text.startswith('\\\\?\\'):
        return text
    if text.startswith('\\\\'):
        return '\\\\?\\UNC\\' + text[2:]
    return '\\\\?\\' + text


def stable_path(path):
    if os.name != 'nt':
        return os.fspath(path)
    text = os.fspath(path)
    if text.startswith('\\\\?\\UNC\\'):
        return '\\\\' + text[8:]
    if text.startswith('\\\\?\\'):
        return text[4:]
    return text


def binding_payload(binding):
    parts = []
    for key in ('version', 'kind', 'binary', 'source', 'head', 'target', 'os'):
        value = str(binding[key])
        parts.append(f'{key}\0{len(value)}\0{value}\0')
    return ''.join(parts).encode()


def binding_key(payload):
    # The proof body rechecks the full binding; this key only selects a cache
    # candidate without importing hashlib on every hot launch.
    first = 0xcbf29ce484222325
    second = 0x84222325cbf29ce4
    for byte in payload:
        first = ((first ^ byte) * 0x100000001b3) & 0xffffffffffffffff
        second = ((second + byte) * 0x100000001b3) & 0xffffffffffffffff
    return f'{first:016x}{second:016x}'


def lstat_stamp(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def resolve_unix(path, parents, record=True):
    """Resolve shared ancestors once within this single evidence scan."""
    path = os.fspath(path)
    parent, name = os.path.split(path)
    if parent == path:
        return path
    parent_entry = parents.get(parent)
    if parent_entry is None:
        resolved_parent = resolve_unix(parent, parents)
    else:
        resolved_parent = parent_entry[0]
    if name == '..':
        return os.path.dirname(resolved_parent)
    resolved = os.path.join(resolved_parent, name)
    info = os.lstat(resolved)
    is_symlink = stat.S_ISLNK(info.st_mode)
    if is_symlink:
        target = os.path.realpath(resolved)
        if record:
            parents[path] = (target, lstat_stamp(info), is_symlink)
        return target
    if record:
        parents[path] = (resolved, lstat_stamp(info), is_symlink)
    return resolved


def snapshot(g, path, parents=None):
    path = os.path.abspath(os.fspath(path))
    resolved = (resolve_unix(path, parents, record=False) if parents is not None and sys.platform != 'win32'
                else stable_path(os.path.realpath(native_path(path))))
    stamp = list(g.stamp(os.stat(native_path(resolved))))
    if sys.platform == 'win32':
        stamp[-1] = g.support('candidate_identity_windows').change_time(Path(native_path(resolved)))
    return dict(path=str(path), resolved=str(resolved), stamp=stamp)


def parent_records(parents):
    return [dict(path=path, resolved=resolved, stamp=list(stamp), symlink=is_symlink)
            for path, (resolved, stamp, is_symlink) in sorted(parents.items())]


def observe(g, path):
    evidence = snapshot(g, path)
    if _observations:
        digest = content_hash(path)
        if snapshot(g, path) != evidence:
            raise g.IdentityError('identity evidence changed during verification')
        record_observation(g, evidence, digest)
    return evidence


def read_bytes(g, path):
    try:
        before = snapshot(g, path)
        try:
            data = Path(path).read_bytes()
        except OSError:
            if os.name != 'nt':
                raise
            data = Path(native_path(path)).read_bytes()
        digest = hashlib.sha256(data).digest()
        unchanged = (snapshot(g, path) == before and content_hash(path) == digest
                     and snapshot(g, path) == before)
    except OSError as error:
        raise g.IdentityError('identity evidence changed during read') from error
    if not unchanged:
        raise g.IdentityError('identity evidence changed during read')
    record_observation(g, before, digest)
    return data


def valid(g, evidence, parents=None):
    try:
        return snapshot(g, evidence['path'], parents) == evidence
    except (OSError, ValueError, KeyError, TypeError):
        return False


def valid_current(g, evidence):
    try:
        if sys.platform != 'win32' and evidence['path'] == evidence['resolved']:
            return list(g.stamp(os.lstat(evidence['path']))) == evidence['stamp']
        return snapshot(g, evidence['path']) == evidence
    except (OSError, ValueError, KeyError, TypeError):
        return False


def valid_parents(parents):
    try:
        for item in parents:
            path = item['path']
            info = os.lstat(path)
            is_symlink = stat.S_ISLNK(info.st_mode)
            if item['symlink']:
                if not is_symlink or list(lstat_stamp(info)) != item['stamp']:
                    return False
            elif is_symlink:
                return False
            if os.path.realpath(path) != item['resolved']:
                return False
        return True
    except (OSError, ValueError, KeyError, TypeError):
        return False


def validated_parents(g, evidence):
    # Never reuse pathname resolutions across checks or cache publications.
    parents = {}
    if not all(valid(g, item, parents) for item in evidence):
        return _MISSING
    records = parent_records(parents)
    # A shared ancestor may redirect while its later leaves are checked.
    return records if valid_parents(records) else _MISSING


def valid_all(g, evidence, parents=None):
    if parents is not None:
        return all(valid_current(g, item) for item in evidence) and valid_parents(parents)
    return validated_parents(g, evidence) is not _MISSING


def complete_parents(g, evidence):
    parents = validated_parents(g, evidence)
    if parents is _MISSING:
        return False
    return parents


def validate_publication(g, observations, parents):
    # Native clocks can give successive same-size writes identical stamps.
    # Compare byte witnesses only for a cold verification; warm reuse keeps
    # its existing metadata-only contract and never rehashes executables.
    try:
        unchanged = all(content_hash(path) == digest
                        for path, digest in observations.contents.items())
    except OSError as error:
        raise g.IdentityError('identity evidence changed before publication') from error
    if not unchanged or not valid_all(g, list(observations.values()), parents):
        raise g.IdentityError('identity evidence changed before publication')


def discard_publications(publications):
    errors = []
    for path, identity in publications:
        try:
            info = os.lstat(native_path(path))
            if (info.st_dev, info.st_ino) == identity:
                os.unlink(native_path(path))
        except FileNotFoundError:
            pass
        except OSError as error:
            errors.append(error)
    return errors


def discard_files(paths):
    errors = []
    for path in paths:
        try:
            os.unlink(native_path(path))
        except FileNotFoundError:
            pass
        except OSError as error:
            errors.append(error)
    return errors


# A reader holds a pending marker only while pending_proof loads it.
_COMMIT_RETRY_DELAYS = (0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.25, 0.5)


def commit_marker(marker):
    """Unlink the commit marker; False if Windows still refuses it as held.

    Windows refuses to delete a file another process holds open (WinError 32).
    That refusal is retried; any other error is raised unchanged. A marker that
    stays held keeps the published proof pending, so it is never reused.
    """
    import time

    for delay in _COMMIT_RETRY_DELAYS + (None,):
        try:
            os.unlink(native_path(marker))
            return True
        except PermissionError as error:
            if getattr(error, 'winerror', None) != 32:
                raise
            if delay is None:
                return False
        time.sleep(delay)


def pending_path(cache, transaction):
    if (not isinstance(transaction, str) or len(transaction) != 32
            or any(char not in '0123456789abcdef' for char in transaction)):
        raise ValueError('invalid proof transaction')
    return cache.with_name('.' + cache.stem + '-' + transaction + '.pending')


def pending_proof(cache, proof, identity):
    if not isinstance(proof, dict):
        raise ValueError('invalid identity proof')
    transaction = proof.get('transaction')
    if transaction is None:
        return False  # Existing committed proofs predate transaction markers.
    try:
        with open(native_path(pending_path(cache, transaction)), 'rb') as stream:
            identities = marshal.load(stream)
    except FileNotFoundError:
        return False
    # An injected concurrent replacement may carry the same proof body. Only
    # the inodes staged by this transaction are invalidated by its marker.
    return identity in identities


def reusable(g, cache, binding, committing=()):
    if Path(native_path(cache)).is_symlink():
        raise g.IdentityError('redirected identity proof')
    fast = cache.with_suffix('.marshal')
    if Path(native_path(fast)).is_symlink():
        raise g.IdentityError('redirected identity proof')
    for path in (fast, cache):
        try:
            with open(native_path(path), 'rb') as stream:
                info = os.fstat(stream.fileno())
                identity = (info.st_dev, info.st_ino)
                if path == fast:
                    proof = marshal.load(stream)
                else:
                    import json

                    proof = json.load(stream)
            if identity not in committing and pending_proof(cache, proof, identity):
                continue
        except (OSError, EOFError, ValueError, TypeError):
            continue
        evidence = proof['evidence']
        if (proof['binding'] == binding and evidence
                and valid_all(g, evidence, proof.get('parents'))):
            if _observations:
                for item in evidence:
                    if observe(g, item['path']) != item:
                        raise g.IdentityError('identity evidence changed during verification')
            return proof['record']
        return _MISSING
    return _MISSING


def cached(g, kind, binary, source, head, target, calculate):
    with _cache_lock:
        return _cached_locked(g, kind, binary, source, head, target, calculate)


def _cached_locked(g, kind, binary, source, head, target, calculate):
    if not Path(binary).is_absolute():
        raise g.IdentityError('absolute executable path required')
    # The command-specific target is created once by prepare(), so a later gate
    # cannot inherit this proof, even for the same HEAD/source/executable bytes.
    binding = dict(version=4, kind=kind, binary=str(Path(binary).absolute()),
                   source=str(source), head=head, target=str(target), os=g.host_os())
    key = binding_key(binding_payload(binding))
    directory = Path(target) / 'identity-cache'
    if sys.platform == 'win32':
        os.makedirs(native_path(directory), mode=0o700, exist_ok=True)
    else:
        directory.mkdir(exist_ok=True, mode=0o700)
    if directory.resolve() != directory:
        raise g.IdentityError('redirected identity cache')
    cache = directory / (key + '.json')
    try:
        record = reusable(g, cache, binding)
        if record is not _MISSING:
            return record
    except (OSError, ValueError, KeyError, TypeError):
        pass

    def reuse_after_winerror5(error):
        # Windows refuses to replace a proof another process holds open. The
        # visible proof may still belong to an uncommitted publisher; this
        # record was just revalidated, so only this publication is skipped.
        if getattr(error, 'winerror', None) != 5:
            raise error
        validate_publication(g, observations, parents)
        reused = reusable(g, cache, binding, [identity for _, identity in publications])
        return record if reused is _MISSING else reused

    observations = Observations()
    _observations.append(observations)
    try:
        observe(g, binary)
        record = calculate()
        evidence = list(observations.values())
        parents = complete_parents(g, evidence)
        if parents is False:
            raise g.IdentityError('identity evidence changed before publication')
        import json
        import secrets

        transaction = secrets.token_hex(16)
        marker = pending_path(cache, transaction)
        proof = dict(binding=binding, record=record, evidence=evidence, parents=parents,
                     transaction=transaction)
        temporary = directory / ('.' + secrets.token_hex(16))
        fast = cache.with_suffix('.marshal')
        fast_temporary = directory / ('.' + secrets.token_hex(16) + '.marshal')
        publications = []
        try:
            with open(native_path(temporary), 'xb') as stream:
                stream.write(json.dumps(proof, sort_keys=True).encode())
            with open(native_path(fast_temporary), 'xb') as stream:
                stream.write(marshal.dumps(proof))
            for temporary_path, published_path in ((temporary, cache), (fast_temporary, fast)):
                info = os.lstat(native_path(temporary_path))
                publications.append((published_path, (info.st_dev, info.st_ino)))
            validate_publication(g, observations, parents)
            # Persist the ownership fence before either proof can become visible.
            # A failed cleanup leaves it available to a fresh module/process.
            with open(native_path(marker), 'xb') as stream:
                marshal.dump([identity for _, identity in publications], stream)
            try:
                os.replace(native_path(temporary), native_path(cache))
            except PermissionError as error:
                reuse_after_winerror5(error)
            try:
                os.replace(native_path(fast_temporary), native_path(fast))
            except PermissionError as error:
                record = reuse_after_winerror5(error)
            validate_publication(g, observations, parents)
            errors = discard_files((temporary, fast_temporary))
            if errors:
                raise errors[0]
            # This single unlink commits both formats after validation/cleanup.
            # A marker still held on Windows leaves the verified proof pending
            # instead of letting cleanup erase it; this record stays verified.
            commit_marker(marker)
        except BaseException as error:
            errors = discard_publications(publications)
            errors.extend(discard_files((temporary, fast_temporary)))
            if not errors:
                errors.extend(discard_files((marker,)))
            # Keep the original object and traceback; cleanup never replaces it.
            # The marker stays if any owned cleanup could not be completed.
            error.cleanup_errors = tuple(errors)
            if hasattr(error, 'add_note'):
                for cleanup_error in errors:
                    error.add_note(f'identity proof cleanup failed: {cleanup_error}')
            raise
        return record
    finally:
        _observations.pop()
