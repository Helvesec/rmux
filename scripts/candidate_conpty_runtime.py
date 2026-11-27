"""Application-local pinned ConPTY for Windows candidate consumers only."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import struct
import subprocess
import tempfile
import time


def configuration(g):
    value = os.environ.get('RMUX_CONPTY_CONFIGURATION', 'pinned')
    if value not in ('pinned', 'system'):
        raise g.IdentityError('unknown ConPTY configuration')
    disabled = os.environ.get('RMUX_CONPTY_NO_PASSTHROUGH', '').lower() in ('1', 'true', 'yes')
    if value == 'pinned' and disabled:
        raise g.IdentityError('pinned ConPTY cannot be disabled')
    return value


def manifest(g):
    path = Path(g.__file__).resolve().parent / 'conpty/runtime.json'
    data = path.read_bytes()
    return json.loads(data), hashlib.sha256(data).hexdigest()


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def checked(g, path, expected):
    path = Path(path)
    if path.is_symlink() or path.resolve(strict=True) != path or digest(path) != expected:
        raise g.IdentityError('pinned ConPTY file missing, redirected or changed')
    return path


def initialize(g, target):
    """Reuse the package verifier before publishing the first Windows image."""
    if configuration(g) == 'system':
        return None
    target = Path(target)
    runtime = target / 'conpty-runtime'
    if runtime.resolve() != runtime:
        raise g.IdentityError('redirected ConPTY runtime store')
    receipt = runtime / 'provenance.json'
    if not receipt.exists():
        script = Path(g.__file__).resolve().parent / 'conpty/candidate-runtime.ps1'
        shell = shutil.which('powershell.exe')
        if not shell:
            raise g.IdentityError('PowerShell unavailable for ConPTY provenance verification')
        command = [str(Path(shell).resolve()), '-NoProfile', '-NonInteractive',
                   '-File', str(script), '-Stage', str(runtime)]
        if os.environ.get('RMUX_CONPTY_PACKAGE'):
            command.extend(['-Package', str(Path(os.environ['RMUX_CONPTY_PACKAGE']).resolve(strict=True))])
        # candidate_identity.support() loads this module from its file, so the
        # scripts directory is not on sys.path (python -I, python -c).
        powershell = g.support('windows_powershell')
        subprocess.run(command, check=True, env=powershell.windows_powershell_environment())
    pinned, manifest_hash = manifest(g)
    proof = json.loads(receipt.read_text(encoding='utf-8-sig'))
    if (proof.get('manifest_sha256') != manifest_hash
            or proof.get('package_sha256') != pinned['sha256']
            or proof.get('version') != pinned['version']
            or proof.get('publisher') != pinned['publisher']):
        raise g.IdentityError('ConPTY package provenance differs from pinned manifest')
    files = proof.get('files', [])
    if not isinstance(files, list) or len(files) != len(pinned['files']):
        raise g.IdentityError('ConPTY provenance has an incomplete runtime pair')
    for item, attested in zip(pinned['files'], files):
        path = runtime / item['name']
        checked(g, path, item['sha256'])
        if (attested.get('sha256') != item['sha256']
                or attested.get('signer_thumbprint') != item['signer_thumbprint']
                or attested.get('authenticode') != 'Valid'
                or attested.get('file_version') != pinned['file_version']
                or attested.get('pe_machine') != pinned['pe_machine']):
            raise g.IdentityError('ConPTY signature, version or architecture proof differs')
    return runtime


def stage(g, image, target):
    """Never overwrite an unexpected companion or change compiler image bytes."""
    image = Path(image).resolve(strict=True)
    if not image.is_relative_to(target):
        raise g.IdentityError('ConPTY image outside private candidate target')
    pinned, _ = manifest(g)
    if configuration(g) == 'system':
        if any(image.with_name(item['name']).exists() for item in pinned['files']):
            raise g.IdentityError('system ConPTY configuration has companion files')
        return dict(configuration='system', files=[])
    with image.open('rb') as stream:
        if stream.read(2) != b'MZ':
            raise g.IdentityError('pinned ConPTY requires a native PE image')
        stream.seek(0x3c)
        offset = struct.unpack('<I', stream.read(4))[0]
        stream.seek(offset)
        if stream.read(4) != b'PE\0\0' or struct.unpack('<H', stream.read(2))[0] != pinned['pe_machine']:
            raise g.IdentityError('pinned ConPTY image architecture differs')
    runtime = initialize(g, target)
    # Cargo copies deps/<crate>-<hash>.exe into its profile directory only
    # after the compiler wrapper returns. Prepare that future alias too.
    directories = [image.parent]
    relative = image.relative_to(target)
    if (image.parent.name == 'deps' and len(relative.parts) in (3, 4)):
        directories.append(image.parent.parent)
    files = []
    for directory in directories:
        files.extend(stage_pair(g, directory, runtime, pinned))
    return dict(configuration='pinned', package=pinned['package'], version=pinned['version'],
                provenance=str(runtime / 'provenance.json'), files=files)


def stage_pair(g, directory, runtime, pinned):
    files = []
    for item in pinned['files']:
        path = directory / item['name']
        if not path.exists():
            temporary = path.with_name('.conpty-' + secrets.token_hex(16))
            try:
                shutil.copyfile(runtime / item['name'], temporary)
                checked(g, temporary, item['sha256'])
                try:
                    os.link(temporary, path)
                except FileExistsError:
                    pass
            finally:
                temporary.unlink(missing_ok=True)
        checked(g, path, item['sha256'])
        files.append(dict(path=str(path), sha256=item['sha256']))
    return files


def select(g, binary, source, head, target):
    record = g.verify_built(binary, source, head, target)
    stage(g, Path(record['path']), target)
    return record


def libtest_inventory(args):
    """Recognize --list only as an option, never as a value or free filter."""
    values = {'--logfile', '--test-threads', '--skip', '--color', '--format',
              '--shuffle-seed', '-Z'}
    arguments = iter(args)
    for arg in arguments:
        if arg == '--':
            break
        if arg == '--list':
            return True
        # Long options with = and short -Zvalue carry their value inline.
        # getopts also accepts short clusters such as -qZ value.
        if arg in values or (arg.startswith('-') and not arg.startswith('--')
                             and arg.endswith('Z')):
            next(arguments, None)
    return False


def selected_vt(g, record, args):
    """Ask libtest which cells its filters and skips actually select."""
    if libtest_inventory(args):
        return False
    built = json.loads(Path(record['build']).read_text()) if record.get('build') else {}
    if built.get('kind') != 'rust-test':
        return False
    # Keep inventory separate from execution, with its own native wait receipt.
    # A file avoids pipe backpressure and an unbounded captured-output buffer.
    with tempfile.TemporaryFile(dir=record['target']) as output:
        # Prepend so no separator or value-taking option can consume --list.
        status = launch(g, record, ['--list', *args], listing=output)
        if status:
            raise g.IdentityError('VT cell inventory failed')
        output.seek(0)
        return any(b'wrap1::' in line and line.rstrip().endswith(b': test')
                   for line in output)


def launch(g, record, args, listing=None):
    """Keep real load observations separate from staging and from system fallback."""
    image, target = Path(record['path']), Path(record['target'])
    prepared = stage(g, image, target)
    required = (prepared['configuration'] == 'pinned'
                and selected_vt(g, record, args))
    native = g.support('native_input_windows_process')
    observer = g.support('candidate_conpty_windows')
    before = dict(case='conpty-' + secrets.token_hex(16), head=record['commit'],
                  executable_sha256=record['sha256'], at_ns=time.time_ns())
    parent = native.current()
    # Preserve the caller's argv, cwd, environment and libtest capture behavior.
    with native.popen([str(image), *args], stdout=listing, creation_image=image,
                      capture_creation=True) as process:
        child = process.creation_identity
        observation = observer.observe(g, process, child, target, prepared)
        started = time.time_ns()
        status = process.wait()
        waited = native.wait_receipt(process, before, parent, child, started)
    if waited['exit'] != (status & 0xffffffff):
        raise g.IdentityError('ConPTY launch and native wait disagree')
    # Every record carries the candidate bytes/marker and a real parental wait.
    # A non-VT test may never load ConPTY: that is recorded as not observed.
    receipt = dict(executable=record, runtime=prepared, observation=observation, wait=waited,
                   inventory=libtest_inventory(args), attestation_required=required)
    directory = target / 'identity-conpty-launches'
    directory.mkdir(exist_ok=True)
    with (directory / (before['case'] + '.json')).open('x') as stream:
        json.dump(receipt, stream, sort_keys=True)
        stream.write('\n')
    if (prepared['configuration'] == 'pinned' and required
            and not observation['pair_loaded']):
        raise g.IdentityError('VT cell has no observed pinned ConPTY DLL and host load')
    if observation['errors']:
        raise g.IdentityError('ConPTY load observation failed: ' + '; '.join(observation['errors']))
    return status
