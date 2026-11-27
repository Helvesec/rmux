"""Attribute generated Cargo inputs to an observed, qualified build-script run."""
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys


def shell(g):
    spec = importlib.util.spec_from_file_location(
        'candidate_identity_unix', Path(__file__).with_name('candidate_identity_unix.py'))
    unix = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(unix)
    return unix.script_launcher([sys.executable, str(Path(g.__file__).resolve()), 'build-script'],
                                forward_argv0=True)


def install(g, original, payload, source, head, target):
    if os.name == 'nt':
        shutil.copyfile(target / 'rustc-wrapper.exe', original)
    else:
        original.write_text(shell(g))
        original.chmod(0o755)
    binding = dict(payload=str(payload), sha256=g.file_hash(payload),
                   wrapper_sha256=g.file_hash(original), head=head, source=str(source))
    (original.parent / '.identity-build-script.json').write_text(json.dumps(binding) + '\n')


def files(g, directory):
    result = {}
    for path in sorted(directory.rglob('*')):
        if path.is_symlink():
            raise g.IdentityError('generated output symlink is not qualified')
        if path.is_file():
            result[path.relative_to(directory).as_posix()] = g.file_hash(path)
    return result


def retain_files(g, directory, target):
    """Copy a completed generated version before publishing it to consumers."""
    observed = files(g, directory)
    objects = target / 'identity-generated-objects'
    objects.mkdir(exist_ok=True)
    if objects.resolve() != objects:
        raise g.IdentityError('redirected generated output store')
    retained = {}
    for name, sha in observed.items():
        original = directory / name
        saved = objects / sha
        temporary = objects / (str(os.getpid()) + '-' + secrets.token_hex(8) + '.tmp')
        try:
            shutil.copyfile(original, temporary)
            if g.file_hash(temporary) != sha or g.file_hash(original) != sha:
                raise g.IdentityError('generated output changed during retention')
            try:
                os.link(temporary, saved)
            except FileExistsError:
                pass
            if saved.is_symlink() or g.file_hash(saved) != sha:
                raise g.IdentityError('retained generated output changed')
        finally:
            temporary.unlink(missing_ok=True)
        retained[name] = dict(path=str(saved), sha256=sha)
    if files(g, directory) != observed:
        raise g.IdentityError('generated output set changed during retention')
    return observed, retained


def verify_retained(g, record, target):
    """Historical verification reads the saved version, not a reused OUT_DIR."""
    retained = record['retained_files']
    if set(retained) != set(record['files']):
        raise g.IdentityError('retained generated output set differs')
    for name, sha in record['files'].items():
        relative = Path(name)
        saved = target / 'identity-generated-objects' / sha
        if (relative.is_absolute() or '..' in relative.parts or relative.as_posix() != name
                or not re.fullmatch(r'[0-9a-f]{64}', sha)
                or retained[name] != dict(path=str(saved), sha256=sha)
                or saved.is_symlink() or saved.resolve(strict=True) != saved
                or g.file_hash(saved) != sha):
            raise g.IdentityError('retained generated output differs')


def location(g, env, source, head, target):
    directory = Path(env['OUT_DIR']).resolve(strict=True)
    if not directory.is_relative_to(target):
        raise g.IdentityError('build-script output is outside the candidate target')
    key = g.hashlib.sha256(os.fsencode(str(directory))).hexdigest()
    return directory, target / 'identity-generators' / ('build-' + key + '.json')


def verify_receipt(g, receipt, source, head, target):
    """Verify the exact completed producer observation consumed by this unit."""
    try:
        record = json.loads(receipt.read_text())
        units = g.support('candidate_identity_units')
        directory = Path(record['directory']).resolve()
        producer = record['producer']
        actual = units.verify(g, producer['path'], source, head, target,
                              expected=producer['sha256'], receipt=producer['receipt'])
        if (record['head'] != head or record['source'] != str(source)
                or receipt.parent != target / 'identity-generators'
                or receipt.name != units.digest(record) + '.json'
                or not directory.is_relative_to(target)
                or record['directory'] != str(directory)
                or actual != producer):
            raise g.IdentityError('generated inputs differ from their observed producer')
        verify_retained(g, record, target)
        return dict(receipt=str(receipt), receipt_sha256=g.file_hash(receipt), producer=producer)
    except (KeyError, OSError, ValueError, TypeError) as error:
        raise g.IdentityError('generated inputs have no matching completed producer') from error


def verify_output(g, env, source, head, target):
    directory, current = location(g, env, source, head, target)
    try:
        record = json.loads(current.read_text())
        receipt = current.parent / (g.support('candidate_identity_units').digest(record) + '.json')
        if (receipt.read_bytes() != current.read_bytes()
                or record['directory'] != str(directory)
                or record['manifest'] != str(Path(env['CARGO_MANIFEST_DIR']).resolve() / 'Cargo.toml')
                or record['files'] != files(g, directory)):
            raise g.IdentityError('generated output selection differs')
        return verify_receipt(g, receipt, source, head, target)
    except (KeyError, OSError, ValueError, TypeError) as error:
        raise g.IdentityError('generated inputs have no matching completed producer') from error


def verify_probe_receipt(g, receipt, source, head, target):
    """Requalify the immutable producer observed by a running probe session."""
    try:
        record = json.loads(receipt.read_text())
        directory = Path(record['directory']).resolve()
        producer = record['producer']
        actual = g.support('candidate_identity_units').verify(
            g, producer['path'], source, head, target,
            expected=producer['sha256'], receipt=producer['receipt'])
        if (receipt.parent != target / 'identity-build-sessions'
                or not re.fullmatch(r'[0-9a-f]{32}\.json', receipt.name)
                or record['head'] != head or record['source'] != str(source)
                or record['directory'] != str(directory) or not directory.is_relative_to(target)
                or record['files'] != {} or actual['kind'] != 'build-script'
                or actual != producer):
            raise g.IdentityError('compiler probe differs from its observed producer scope')
        return dict(receipt=str(receipt), receipt_sha256=g.file_hash(receipt), producer=actual)
    except (KeyError, OSError, ValueError, TypeError) as error:
        raise g.IdentityError('compiler probe has no observed build-script parent') from error


def verify_generation(g, generation, source, head, target):
    """Bind the graph declaration to the authenticated generation descriptor."""
    if not generation:
        return None
    receipt = Path(generation['receipt'])
    raw = g.read_bytes(receipt)
    if g.hashlib.sha256(raw).hexdigest() != generation['receipt_sha256']:
        raise g.IdentityError('generated input provenance changed')
    if receipt.parent == target / 'identity-generators':
        actual = verify_receipt(g, receipt, source, head, target)
    elif receipt.parent == target / 'identity-build-sessions':
        actual = verify_probe_receipt(g, receipt, source, head, target)
    elif 'producer' not in generation:
        # A missing declaration cannot erase authority carried by the receipt.
        # Historical empty observations remain hash-pinned, but grant nothing
        # to lock closure or dep-info consumers.
        try:
            record = json.loads(raw)
            if (set(generation) != {'receipt', 'receipt_sha256'}
                    or not isinstance(record, dict) or 'producer' in record
                    or record.get('files') != {} or record.get('retained_files', {}) != {}):
                raise g.IdentityError('legacy observation carries generated input authority')
        except (ValueError, TypeError) as error:
            raise g.IdentityError('legacy observation has no qualified empty receipt') from error
        return None
    else:
        raise g.IdentityError('generated input has no authenticated producer receipt')
    if actual != generation:
        raise g.IdentityError('compiler generation differs from its authenticated producer')
    return actual


def probe_parent(g, args, outputs, env, source, head, target):
    """Bind a metadata-only feature probe to the observed build-script parent.

    A probe runs before that parent's generated-output seal exists. Its source
    still undergoes normal unit qualification; it cannot authorize linked code
    or arbitrary generated inputs.
    """
    units = g.support('candidate_identity_units')
    token = env.get('RMUX_IDENTITY_BUILD_SESSION', '')
    if not re.fullmatch(r'[0-9a-f]{32}', token):
        raise g.IdentityError('observed build-script session missing')
    receipt = target / 'identity-build-sessions' / (token + '.json')
    try:
        record = json.loads(receipt.read_text())
        directory, _ = location(g, env, source, head, target)
        generation = verify_probe_receipt(g, receipt, source, head, target)
        emissions = [kind for value in units.values(args, '--emit') for kind in value.split(',')]
        if (record['head'] != head or record['source'] != str(source)
                or record['directory'] != str(directory)
                or record['manifest'] != str(Path(env['CARGO_MANIFEST_DIR']).resolve() / 'Cargo.toml')
                or units.values(args, '--crate-type') != ['lib'] or '--test' in args
                or units.values(args, '--extern') or 'metadata' not in emissions
                or set(emissions) - {'metadata', 'dep-info'}
                or any(not path.is_relative_to(directory) for path in outputs)):
            raise g.IdentityError('compiler probe differs from its observed producer scope')
        return generation
    except (KeyError, OSError, ValueError, TypeError) as error:
        raise g.IdentityError('compiler probe has no observed build-script parent') from error


def run(g, args):
    source, head, target = g.context()
    g.source_identity(source, head)
    wrapper = Path(args[0]).resolve(strict=True)
    if not wrapper.is_relative_to(target):
        raise g.IdentityError('build-script wrapper is outside the candidate target')
    binding = json.loads((wrapper.parent / '.identity-build-script.json').read_text())
    if (binding['head'] != head or binding['source'] != str(source)
            or g.file_hash(wrapper) != binding['wrapper_sha256']):
        raise g.IdentityError('build-script wrapper binding differs')
    if os.name == 'nt':
        g.verify_built(wrapper, source, head, target)
    elif wrapper.read_text() != shell(g):
        raise g.IdentityError('build-script wrapper changed')
    units = g.support('candidate_identity_units')
    producer = units.verify(g, binding['payload'], source, head, target, expected=binding['sha256'])
    if os.name != 'nt':
        retained = g.support('candidate_identity_retention').retain(
            g, Path(producer['path']), target, producer['sha256'])
        producer = units.verify(g, retained, source, head, target,
                                expected=producer['sha256'], receipt=producer['receipt'])
    metadata = json.loads(Path(producer['receipt']).read_text())
    manifest = str(Path(os.environ['CARGO_MANIFEST_DIR']).resolve() / 'Cargo.toml')
    if producer['kind'] != 'build-script' or metadata['graph']['input']['manifest'] != manifest:
        raise g.IdentityError('generated input producer belongs to another package')
    directory, receipt = location(g, os.environ, source, head, target)
    if files(g, directory):
        verify_output(g, os.environ, source, head, target)
    launch = g.verify(Path(producer['path']), source, head, target)
    launch['unit'] = producer
    g.record_launch([launch])
    # Qualify the parent before its child probes, without claiming that the
    # still-running producer has completed its generated-output observation.
    token = secrets.token_hex(16)
    sessions = target / 'identity-build-sessions'
    sessions.mkdir(exist_ok=True)
    with (sessions / (token + '.json')).open('x') as stream:
        json.dump(dict(head=head, source=str(source), directory=str(directory), manifest=manifest,
                       producer=producer, files={}), stream, sort_keys=True)
        stream.write('\n')
    env = dict(os.environ, RMUX_IDENTITY_BUILD_SESSION=token)
    def observation():
        g.source_identity(source, head)
        if units.verify(g, producer['path'], source, head, target) != producer:
            raise g.IdentityError('build-script producer changed during execution')
        observed, retained = retain_files(g, directory, target)
        return dict(head=head, source=str(source), directory=str(directory), manifest=manifest,
                    producer=producer, files=observed, retained_files=retained)

    command = [producer['path'], *args[1:]]
    if os.name != 'nt':
        command = [sys.executable, str(Path(g.__file__).resolve()), 'build-exec', *command]
    try:
        status = subprocess.call(command, env=env, **g.jobserver())
    except BaseException as error:
        record = observation()
        record['error'] = type(error).__name__
        units.preserve(g, target / 'identity-generator-errors', record)
        raise
    record = observation()
    if status == 0:
        receipt.parent.mkdir(exist_ok=True)
        immutable = units.preserve(g, receipt.parent, record)
        verify_receipt(g, immutable, source, head, target)
        temporary = receipt.with_suffix('.' + str(os.getpid()) + '.tmp')
        temporary.write_bytes(immutable.read_bytes())
        os.replace(temporary, receipt)
    else:
        record['status'] = status
        units.preserve(g, target / 'identity-generator-errors', record)
    return status
