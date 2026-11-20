"""Immutable compiler objects and per-consumer checks before exec."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import secrets
import re
import subprocess
import time


def native_path(path):
    if os.name != 'nt':
        return path
    text = str(path)
    if text.startswith('\\\\?\\'):
        return text
    if text.startswith('\\\\'):
        return '\\\\?\\UNC\\' + text[2:]
    return '\\\\?\\' + text


CARGO_METADATA_SUFFIX = re.compile(r'-[0-9a-f]{16}$')


def program_name(path):
    """Cargo's program name without its metadata suffix.

    Unix programs key on their basename: rmux derives its tmux personality
    from argv[0] and its tmux shim from the current executable. Retained
    objects and launchers therefore keep that name.
    """
    return CARGO_METADATA_SUFFIX.sub('', Path(path).name) or 'executable'


def retain(g, path, target, expected):
    """Copy bytes, never hard-link a compiler's mutable output inode."""
    path = Path(path).resolve(strict=True)
    before = g.stamp(path.stat())
    data = path.read_bytes()
    if g.stamp(path.stat()) != before or hashlib.sha256(data).hexdigest() != expected:
        raise g.IdentityError('executable changed before retention')
    directory = target / 'identity-objects' / expected
    if os.name == 'nt':
        os.makedirs(native_path(directory), mode=0o700, exist_ok=True)
    else:
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    if directory.resolve() != directory:
        raise g.IdentityError('redirected executable object store')
    retained = directory / ('executable' if os.name == 'nt' else program_name(path))
    temporary = directory / ('.' + secrets.token_hex(16))
    try:
        with open(native_path(temporary), 'xb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        if os.name != 'nt':
            temporary.chmod(0o500)
        try:
            os.link(native_path(temporary), native_path(retained))
        except FileExistsError:
            pass
        if retained.is_symlink() or Path(native_path(retained)).read_bytes() != data:
            raise g.IdentityError('immutable executable object changed')
    finally:
        try:
            os.unlink(native_path(temporary))
        except FileNotFoundError:
            pass
    # Windows read-only attributes are shared by hard links and prevent unlink.
    # Protect the published copy only after removing our temporary name.
    if os.name == 'nt':
        os.chmod(native_path(retained), 0o500)
    return retained


def publish(g, target, record):
    units = g.support('candidate_identity_units')
    directory = target / 'identity-builds'
    immutable = units.preserve(g, directory, record)
    raw = immutable.read_bytes()
    current = directory / (record['sha256'] + '.json')
    # Cargo and retained aliases select the same launcher. Republishing its
    # identical receipt would invalidate the other alias's evidence stamps.
    try:
        if (record.get('kind') == 'unix-launcher' and not current.is_symlink()
                and current.read_bytes() == raw):
            return
    except FileNotFoundError:
        pass
    temporary = directory / ('.' + secrets.token_hex(16) + '.json')
    temporary.write_bytes(raw)
    os.replace(temporary, current)


def launcher_bytes(g, binary, source, head, target):
    return g.support('candidate_identity_unix').launcher_bytes(
        g, binary, source, head, target)


def select(g, binary, source, head, target):
    source, _ = g.source_identity(source, head)
    target = g.target_identity(source, head, target)
    return g.support('candidate_identity_cache').cached(
        g, 'selection', binary, source, head, target,
        lambda: select_uncached(g, binary, source, head, target))


def select_uncached(g, binary, source, head, target):
    record = g.verify_built(binary, source, head, target)
    built = json.loads(g.read_bytes(Path(record['build'])))
    if built['kind'] == 'unix-launcher':
        return record
    retained = Path(built['path'])
    # Only witnessed compiler/fixture bytes may become a selected object.
    if g.file_hash(retained) != record['sha256']:
        raise g.IdentityError('selected compiler object differs')
    data = launcher_bytes(g, retained, source, head, target)
    digest = hashlib.sha256(data).hexdigest()
    directory = target / 'identity-launchers'
    directory.mkdir(exist_ok=True, mode=0o700)
    if directory.resolve() != directory:
        raise g.IdentityError('redirected consumer launcher store')
    # One directory per launcher keeps the producer's program name as argv[0].
    named = directory / digest
    named.mkdir(exist_ok=True, mode=0o700)
    if named.resolve() != named:
        raise g.IdentityError('redirected consumer launcher store')
    launcher = named / retained.name
    temporary = directory / ('.' + secrets.token_hex(16))
    try:
        temporary.write_bytes(data)
        temporary.chmod(0o500)
        try:
            os.link(temporary, launcher)
        except FileExistsError:
            pass
        if launcher.is_symlink() or launcher.read_bytes() != data:
            raise g.IdentityError('immutable consumer launcher changed')
    finally:
        temporary.unlink(missing_ok=True)
    binding = g.verify(launcher, source, head, target)
    binding.update(kind='unix-launcher', input=built['input'], input_sha256=built['input_sha256'],
                   compiler=built['compiler'], compiler_sha256=built['compiler_sha256'], exit=0,
                   producer=str(retained), producer_sha256=record['sha256'])
    publish(g, target, binding)
    selected = g.verify_built(launcher, source, head, target)
    selected['selected_from'] = str(binary)
    return selected


def verify_launcher(g, built, source, head, target):
    producer = g.verify_built(Path(built['producer']), source, head, target)
    if (producer['sha256'] != built['producer_sha256']
            or g.read_bytes(built['path']) != launcher_bytes(
                g, Path(producer['path']), source, head, target)):
        raise g.IdentityError('consumer launcher or retained producer changed')
    return producer


def record_launch(g, records, event='selection'):
    _, _, target = g.context()
    out = target / 'identity-launches'
    out.mkdir(exist_ok=True)
    receipt = {'executables': records}
    if os.name != 'nt':
        receipt.update(event=event, at_ns=time.time_ns(),
                       consumer=g.support('candidate_identity_process').current(g))
    with (out / f'{os.getpid()}-{time.time_ns()}.json').open('x') as stream:
        json.dump(receipt, stream, sort_keys=True)
        stream.write('\n')


def exec_action(g, args, mode):
    if mode == 'exec-argv0':
        if os.name == 'nt' or len(args) < 2:
            raise g.IdentityError('Unix executable and argv[0] required')
        return launch(g, [args[0], *args[2:]], replace=True, argv0=args[1])
    return launch(g, args, replace=True)


def launch(g, args, replace=False, argv0=None):
    source, head, target = g.context()
    selected = Path(args[0])
    if os.name != 'nt':
        # Cargo's Unix runner passes paths relative to the package checkout.
        selected = selected.absolute()
    record = g.verify_built(selected, source, head, target)
    if os.name == 'nt':
        g.record_launch([record])
        # Windows exec already creates a new process; keep the original child
        # handle until its native wait and ConPTY load evidence are complete.
        return g.support('candidate_conpty_runtime').launch(g, record, args[1:])
    built = json.loads(Path(record['build']).read_text())
    if built['kind'] == 'unix-launcher':
        # verify_built has already checked this launcher's exact body and its
        # retained producer for this PID. Execute that qualified producer.
        record = record['producer']
    else:
        # The completed observation bound these retained bytes during the
        # verification above, even when the request named a compiler alias.
        record['path'] = built['path']
        if record.get('unit'):
            record['unit'] = dict(record['unit'], path=built['path'])
    if replace:
        record_launch(g, [record], event='exec')
        os.execv(record['path'], [args[0] if argv0 is None else argv0, *args[1:]])
    record_launch(g, [record])
    return subprocess.call([g.sys.executable, str(Path(g.__file__).resolve()),
                            'exec-argv0', record['path'], args[0] if argv0 is None else argv0, *args[1:]])


def exec_build_script(g, args):
    if os.name == 'nt':
        raise g.IdentityError('Unix build-script exec required')
    source, head, target = g.context()
    g.source_identity(source, head)
    token = os.environ.get('RMUX_IDENTITY_BUILD_SESSION', '')
    if not re.fullmatch('[0-9a-f]{32}', token):
        raise g.IdentityError('build-script consumer session missing')
    ticket = json.loads((target / 'identity-build-sessions' / (token + '.json')).read_text())
    producer = ticket['producer']
    units = g.support('candidate_identity_units')
    actual = units.verify(g, Path(args[0]), source, head, target,
                          expected=producer['sha256'], receipt=producer['receipt'])
    if (ticket['head'] != head or ticket['source'] != str(source)
            or actual != producer or actual['kind'] != 'build-script'):
        raise g.IdentityError('build-script consumer differs from observed producer')
    record = g.verify(Path(actual['path']), source, head, target)
    record['unit'] = actual
    record_launch(g, [record], event='exec')
    os.execv(record['path'], [record['path'], *args[1:]])


def verify_built(g, binary, source, head, target):
    """A marker is necessary; a completed compiler output must bind its bytes too."""
    owner = os.name != 'nt' and getattr(g, '_qualification', None) is None
    if owner:
        g = g.support('candidate_identity_qualification').scope(g)
    record = g.verify(binary, source, head, target)
    receipt = Path(target) / 'identity-builds' / (record['sha256'] + '.json')
    try:
        raw = g.read_bytes(receipt)
        built = json.loads(raw)
        if os.name != 'nt':
            immutable = receipt.parent / (g.support('candidate_identity_units').digest(built) + '.json')
            if g.read_bytes(immutable) != raw:
                raise g.IdentityError('completed compiler observation history differs')
            receipt = immutable
        g.support('candidate_identity_cache').observe(g, built['path'])
        g.support('candidate_identity_cache').observe(g, built['input'])
        origin = Path(built['path']).resolve(strict=True)
        code = Path(built['input']).resolve(strict=True)
        if os.name != 'nt' and built.get('kind') == 'unix-launcher':
            record['producer'] = g.support('candidate_identity_retention').verify_launcher(g, built, source, head, target)
        if built.get('kind') == 'generated-fixture':
            producer = g.verify_built(Path(built['producer']), source, head, target)
            if producer['sha256'] != built['producer_sha256']:
                raise g.IdentityError('fixture producer changed')
            producer_build = json.loads(g.read_bytes(producer['build']))
            if producer_build.get('kind') != 'rust-test':
                raise g.IdentityError('fixture producer is not a verified Rust test')
        generated = built.get('generation')
        if generated:
            if generated.get('kind') == 'final-sink':
                g.support('candidate_identity_windows').validate_generation(g, generated, Path(source).resolve(), head, target)
            g.tracked_input(Path(generated['origin']), Path(source).resolve(), head)
            if g.file_hash(generated['origin']) != generated['origin_sha256'] or g.file_hash(generated['generator']) != generated['generator_sha256']:
                raise g.IdentityError('recorded generator or source changed')
        else:
            g.tracked_input(code, Path(source).resolve(), head)
        if built['commit'] != head or built['source'] != str(Path(source).resolve()) or built['exit'] != 0 or (not origin.is_relative_to(target)) or (not code.is_relative_to(Path(source).resolve())) or (g.file_hash(origin) != record['sha256']) or (g.file_hash(code) != built['input_sha256']) or (g.file_hash(built['compiler']) != built['compiler_sha256']):
            raise g.IdentityError('observed build no longer matches executable')
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise g.IdentityError('executable has no matching completed compiler output') from error
    if built.get('kind') != 'generated-fixture' and (not (os.name != 'nt' and built.get('kind') == 'unix-launcher')):
        record['unit'] = g.support('candidate_identity_units').verify(g, Path(record['path']), Path(source).resolve(), head, Path(target))
        if os.name != 'nt' and record['unit']['sha256'] != record['sha256']:
            raise g.IdentityError('selected compiler unit changed during verification')
    record['build'] = str(receipt)
    if owner:
        g._qualification.validate()
    return record



def compiler_receipts(g, args, before, source, head, target):
    compiler = Path(args[0]).resolve(strict=True)
    code = next((Path(arg).resolve() for arg in args[1:] if not arg.startswith('-') and arg.endswith('.rs')))
    for path, stamp in g.executable_outputs(args).items():
        if before.get(path) == stamp:
            continue
        record = g.verify(path, source, head, target)
        if os.name != 'nt':
            unit = g.support('candidate_identity_units').verify(g, path, source, head, target)
            observed = json.loads(Path(unit['receipt']).read_text())
            record['compiler_output'] = str(path)
            record['path'] = observed['path']
        record.update(input=str(code), input_sha256=g.file_hash(code), compiler=str(compiler), compiler_sha256=g.file_hash(compiler), exit=0, kind='rust-test' if '--test' in args else 'bin')
        if os.environ.get('RMUX_IDENTITY_DOC_SESSION'):
            record['generation'] = g.generation(source, head, target)
        if os.name != 'nt':
            g.support('candidate_identity_retention').publish(g, target, record)
            continue
        if path.suffix.lower() == '.exe':
            # Sibling binaries and generated doctests may be started directly
            # by a Rust test, rather than through Cargo's runner.
            g.support('candidate_conpty_runtime').stage(g, path, target)
        out = target / 'identity-builds'
        out.mkdir(exist_ok=True)
        receipt = out / (record['sha256'] + '.json')
        temporary = out / f'.{os.getpid()}.json'
        temporary.write_text(json.dumps(record, sort_keys=True) + '\n')
        os.replace(temporary, receipt)
