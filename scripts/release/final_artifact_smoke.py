#!/usr/bin/env python3
"""Signed final-archive inputs for the existing installed smokes.

The fixed D71 roster includes pending native drivers. A pending driver never
produces a passing cell. Fixture receipts are explicitly non-product evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'packaging'))
import candidate_identity
from canonical_payload import embedded_commit
import layout
import layout_profiles
from release_tag_policy import load_signer_policy, verify_ssh_signature
from strict_json import read_json_object

ROOT = Path(__file__).resolve().parents[2]
PLATFORMS = {
    'macos-aarch64': ('aarch64-apple-darwin', 'macos', 'unix-archive'),
    'linux-x86_64': ('x86_64-unknown-linux-gnu', 'linux', 'unix-archive'),
    'windows-x86_64': ('x86_64-pc-windows-msvc', 'windows', 'windows-archive'),
}
SCENARIOS = ('ssh', 'nested', 'unknown-terminal', 'osc52')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical_file(path):
    path = Path(path).absolute()
    require(path.resolve(strict=True) == path and path.is_file(),
            'redirected or nonregular file: ' + str(path))
    return path


def relative(value):
    require(isinstance(value, str) and value and '\\' not in value and ':' not in value,
            'nonportable relative path')
    path = PurePosixPath(value)
    require(not path.is_absolute() and '..' not in path.parts and str(path) == value,
            'noncanonical relative path')
    return path


def pinned(base, ref):
    require(set(ref) == {'path', 'sha256'} and
            re.fullmatch('[0-9a-f]{64}', ref['sha256']) is not None, 'invalid file pin')
    path = canonical_file(base / relative(ref['path']))
    require(path.is_relative_to(base) and digest(path.read_bytes()) == ref['sha256'],
            'file pin mismatch: ' + str(path))
    return path


def roster(key):
    return (*SCENARIOS, 'daemon-survives-terminal-close') if key == 'windows-x86_64' else SCENARIOS


def manifest(path, *, fixture=False, policy=None, head=None):
    path = canonical_file(path)
    raw = path.read_bytes()
    # Reuse the existing release SSH allowlist and signature verifier. The
    # detached signature covers these exact bytes, not a reserialized object.
    signer = verify_ssh_signature(policy or load_signer_policy(
        ROOT / '.github/release/release-signers.json'), raw,
        canonical_file(str(path) + '.sig').read_bytes())
    value = read_json_object(path, 'final smoke manifest')
    require(path.read_bytes() == raw, 'manifest changed during signature verification')
    require(set(value) == {'schema', 'subject', 'source_sha', 'platforms'}, 'manifest fields')
    require(type(value['schema']) is int and value['schema'] == 1, 'manifest schema')
    require(value['subject'] == ('fixture' if fixture else 'final-artifact'), 'subject scope')
    head = head or candidate_identity.git(ROOT, 'rev-parse', 'HEAD')
    require(re.fullmatch('[0-9a-f]{40}', value['source_sha']) is not None and
            value['source_sha'] == head, 'manifest source differs from HEAD')
    require(set(value['platforms']) == set(PLATFORMS), 'required platform absent or unexpected')
    for key, row in value['platforms'].items():
        require(set(row) == {'target', 'archive', 'package_root', 'build_source_sha256', 'scenarios'},
                'platform fields: ' + key)
        require(row['target'] == PLATFORMS[key][0], 'architecture differs: ' + key)
        require(len(relative(row['package_root']).parts) == 1, 'archive root')
        require(re.fullmatch('[0-9a-f]{64}', row['build_source_sha256']) is not None,
                'build source identity')
        require(set(row['archive']) == {'path', 'sha256'}, 'archive pin fields')
        relative(row['archive']['path'])
        require(re.fullmatch('[0-9a-f]{64}', row['archive']['sha256']) is not None, 'archive hash')
        require(set(row['scenarios']) == set(roster(key)), 'required scenario absent: ' + key)
        for cell in row['scenarios'].values():
            if cell is None:
                continue
            require(set(cell) == {'driver', 'tool', 'argv', 'timeout_seconds'}, 'scenario fields')
            require(set(cell['driver']) == {'path', 'sha256'}, 'driver pin fields')
            relative(cell['driver']['path'])
            require(re.fullmatch('[0-9a-f]{64}', cell['driver']['sha256']) is not None, 'driver hash')
            require(set(cell['tool']) == {'path', 'sha256'} and
                    re.fullmatch('[0-9a-f]{64}', cell['tool']['sha256']) is not None,
                    'scenario tool pin')
            require(isinstance(cell['argv'], list) and
                    all(isinstance(a, str) and a and '\0' not in a for a in cell['argv']) and
                    cell['argv'][0] == '{tool}' and
                    all(token in cell['argv'] for token in ('{driver}', '{rmux}', '{receipt}')),
                    'driver must consume the selected artifact and write evidence')
            require(type(cell['timeout_seconds']) is int and 1 <= cell['timeout_seconds'] <= 600,
                    'scenario timeout')
    return value, signer


def host_key():
    machine = platform.machine().lower()
    key = {'darwin': 'macos-aarch64', 'linux': 'linux-x86_64',
           'win32': 'windows-x86_64'}.get(sys.platform)
    allowed = ('arm64', 'aarch64') if sys.platform == 'darwin' else ('amd64', 'x86_64')
    require(key is not None and machine in allowed, 'unsupported native smoke host')
    if sys.platform == 'win32':
        require(sys.getwindowsversion().build >= 22000, 'Windows 11 authority required')
    return key


def archive_files(base, row):
    path = pinned(base, row['archive'])
    result = {}
    total = 0

    def add(name, data, mode):
        nonlocal total
        name = relative(name).as_posix()
        require(name.startswith(row['package_root'] + '/'), 'archive entry outside package root')
        name = name[len(row['package_root']) + 1:]
        require(name not in result and len(data) <= 512 * 1024**2, 'duplicate or oversized archive entry')
        total += len(data)
        require(total <= 2 * 1024**3, 'archive expansion limit')
        result[name] = (data, mode)

    if path.name.endswith('.tar.gz') and row['target'] != PLATFORMS['windows-x86_64'][0]:
        with tarfile.open(path, 'r:gz') as archive:
            for member in archive:
                name = relative(member.name.rstrip('/')).as_posix()
                require(name == row['package_root'] or name.startswith(row['package_root'] + '/'),
                        'archive directory outside package root')
                if member.isdir():
                    continue
                require(member.isfile() and member.size <= 512 * 1024**2, 'nonregular archive entry')
                add(name, archive.extractfile(member).read(), member.mode)
    elif path.name.endswith('.zip') and row['target'] == PLATFORMS['windows-x86_64'][0]:
        with zipfile.ZipFile(path) as archive:
            for member in archive.infolist():
                name = relative(member.filename.rstrip('/')).as_posix()
                require(name == row['package_root'] or name.startswith(row['package_root'] + '/'),
                        'ZIP directory outside package root')
                mode = member.external_attr >> 16
                require(not stat.S_ISLNK(mode), 'ZIP link')
                if member.is_dir():
                    continue
                require(stat.S_IFMT(mode) in (0, stat.S_IFREG), 'nonregular ZIP entry')
                require(member.file_size <= 512 * 1024**2, 'oversized ZIP entry')
                add(name, archive.read(member), stat.S_IMODE(mode) or 0o644)
    else:
        raise ValueError('archive format differs from target')
    require(digest(path.read_bytes()) == row['archive']['sha256'], 'archive changed')
    return result


def native_architecture(data, key):
    if key == 'linux-x86_64':
        require(len(data) >= 20 and data[:6] == b'\x7fELF\x02\x01' and
                int.from_bytes(data[18:20], 'little') == 62, 'native ELF architecture differs')
    elif key == 'macos-aarch64':
        require(len(data) >= 8 and data[:4] == b'\xcf\xfa\xed\xfe' and
                int.from_bytes(data[4:8], 'little') == 0x0100000c, 'native Mach-O architecture differs')
    elif key == 'windows-x86_64':
        require(len(data) >= 64 and data[:2] == b'MZ', 'native PE architecture differs')
        offset = int.from_bytes(data[60:64], 'little')
        require(offset >= 64 and len(data) >= offset + 26 and data[offset:offset + 4] == b'PE\0\0' and
                int.from_bytes(data[offset + 4:offset + 6], 'little') == 0x8664 and
                int.from_bytes(data[offset + 24:offset + 26], 'little') == 0x20b,
                'native PE architecture differs')
    else:
        raise ValueError('unsupported native architecture')


def check_package(root, files, row, head, key, *, fixture=False):
    root = Path(root).absolute()
    require(root.resolve(strict=True) == root and root.is_dir(), 'redirected package root')
    descriptor = layout_profiles.for_package(layout.load(), root)
    variant = PLATFORMS[key][2]
    layout.check_tree(descriptor, variant, root)
    actual = {name: path for name, path, kind in layout.walk_entries(root, ())}
    require(set(actual) == set(files), 'extracted package population differs from signed archive')
    for name, (data, mode) in files.items():
        path = canonical_file(actual[name])
        require(path.read_bytes() == data, 'extracted archive bytes differ: ' + name)
        if sys.platform != 'win32' and variant == 'unix-archive':
            require(stat.S_IMODE(path.stat().st_mode) == mode, 'archive mode differs: ' + name)
    metadata = read_json_object(root / 'share/rmux/artifact-metadata.json', 'artifact metadata')
    require(metadata.get('git_commit') == head and metadata.get('git_dirty') is False and
            metadata.get('target') == row['target'] and metadata.get('release_artifact') is True and
            metadata.get('configuration') == 'release', 'archive metadata identity differs')
    records = {}
    for entry, placement in layout.entries(descriptor, variant):
        if entry['source']['kind'] != 'binary':
            continue
        name = placement['path']
        data = files[name][0]
        if not fixture:
            native_architecture(data, key)
        if fixture and b'RMUX_RELEASE_IDENTITY_V1:' not in data:
            expected = (head.encode(), row['build_source_sha256'].encode(), PLATFORMS[key][1].encode())
            require(set(candidate_identity.MARKER.findall(data)) == {expected},
                    'embedded commit/source/OS differs: ' + name)
        else:
            # Canonical release bytes carry the stable commit only. Invocation
            # paths and build graphs belong in external provenance receipts.
            embedded_commit(data, head)
        records[entry['id']] = {'path': str(root / name), 'sha256': digest(data), 'commit': head}
    require({'cli', 'helper', 'daemon'} <= set(records), 'package executable roster differs')
    return records


def guard(path, binary, root, *, fixture=False, policy=None, head=None, key=None):
    value, signer = manifest(path, fixture=fixture, policy=policy, head=head)
    key = key or host_key()
    row = value['platforms'][key]
    files = archive_files(Path(path).absolute().parent, row)
    records = check_package(root, files, row, value['source_sha'], key, fixture=fixture)
    require(str(Path(binary).absolute()) == records['cli']['path'], 'binary outside manifested artifact')
    canonical_file(binary)
    return value, signer, records


def reject_overrides(env):
    overrides = ('RMUX_INTERNAL_BINARY_PATH', 'RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE',
                 'RMUX_FULL_BINARY_PATH', 'RMUX_INTERNAL_PUBLIC_BINARY_PATH',
                 'RMUX_SDK_DAEMON_BINARY', 'RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB')
    require(not any(env.get(key) for key in overrides), 'foreign runtime binary override')


def tool_file(ref):
    require(Path(ref['path']).is_absolute(), 'absolute scenario tool required')
    path = canonical_file(ref['path'])
    require(digest(path.read_bytes()) == ref['sha256'], 'scenario tool pin mismatch')
    return path


def run(path, output, *, fixture=False, policy=None):
    value, signer = manifest(path, fixture=fixture, policy=policy)
    key = host_key()
    row = value['platforms'][key]
    require(all(row['scenarios'].values()), 'required native scenario pending; no smoke run credited')
    base = Path(path).absolute().parent
    drivers = {name: pinned(base, cell['driver']) for name, cell in row['scenarios'].items()}
    tools = {name: tool_file(cell['tool']) for name, cell in row['scenarios'].items()}
    files = archive_files(base, row)
    output = Path(output).absolute()
    require(output.resolve() == output and not output.exists(), 'fresh canonical output required')
    output.mkdir(parents=True)
    package = output / row['package_root']
    package.mkdir()
    for name, (data, mode) in files.items():
        destination = package / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        destination.chmod(mode)
    records = check_package(package, files, row, value['source_sha'], key, fixture=fixture)
    receipt = {'schema': 1, 'subject': value['subject'], 'source_sha': value['source_sha'],
               'platform': key, 'manifest_sha256': digest(Path(path).read_bytes()),
               'signer': signer, 'archive': row['archive'], 'binaries': records, 'cells': {},
               'status': 'incomplete', 'product_qualification': False}
    result_path = output / 'result.json'

    def save():
        result_path.write_text(json.dumps(receipt, indent=2) + '\n')

    save()
    env = dict(os.environ, RMUX_FINAL_SMOKE_MANIFEST=str(Path(path).absolute()),
               RMUX_FINAL_SMOKE_ROOT=str(package), RMUX_PYTHON=str(Path(sys.executable).resolve()))
    # A developer's overrides must never substitute the helper or daemon for
    # the final package selected here. Refuse them also in direct smoke guards.
    reject_overrides(env)
    env['PATH'] = os.pathsep.join((str(Path(records['cli']['path']).parent),
                                  str(Path(records['helper']['path']).parent),
                                  env.get('PATH', '')))
    smoke = ROOT / 'scripts' / ('smoke-installed-rmux.ps1' if sys.platform == 'win32'
                               else 'smoke-installed-rmux.sh')
    command = ([shutil.which('pwsh') or 'pwsh', '-NoProfile', '-File', str(smoke), '-Rmux']
               if sys.platform == 'win32' else [shutil.which('bash') or 'bash', str(smoke)])
    command.append(records['cli']['path'])
    if fixture:
        # Only the Python fixture API accepts test trust; the CLI has no trust
        # or subject override. Package checks still run before and after every
        # fixture command, and its receipt cannot qualify a product cell.
        env.pop('RMUX_FINAL_SMOKE_MANIFEST')
    try:
        with (output / 'installed.log').open('wb') as log:
            subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                           timeout=90, check=True)
        for name, cell in row['scenarios'].items():
            guard(path, records['cli']['path'], package, fixture=fixture, policy=policy)
            driver = pinned(base, cell['driver'])
            require(driver == drivers[name], 'scenario driver changed')
            tool = tool_file(cell['tool'])
            require(tool == tools[name], 'scenario tool changed')
            evidence = output / (name + '.json')
            replacements = {'{tool}': str(tool), '{driver}': str(driver), '{rmux}': records['cli']['path'],
                            '{receipt}': str(evidence)}
            argv = [replacements.get(a, a) for a in cell['argv']]
            with (output / (name + '.log')).open('wb') as log:
                subprocess.run(argv, env=env, stdout=log, stderr=subprocess.STDOUT,
                               timeout=cell['timeout_seconds'], check=True)
            proof = read_json_object(canonical_file(evidence), 'native scenario receipt')
            require(proof.get('scenario') == name and proof.get('status') == 'PASS' and
                    proof.get('source_sha') == value['source_sha'] and proof.get('binaries') == records and
                    proof.get('subject') == value['subject'], 'native evidence identity differs')
            receipt['cells'][name] = {'status': 'PASS', 'argv': argv,
                                    'driver': cell['driver'], 'tool': cell['tool'],
                                    'evidence_sha256': digest(evidence.read_bytes())}
            save()
        guard(path, records['cli']['path'], package, fixture=fixture, policy=policy)
        receipt['status'] = 'PASS'
        # Full release qualification also needs the other hosts and independent
        # review on the same SHA; this local run never closes L19.2 by itself.
    except Exception:
        receipt['status'] = 'FAIL'
        raise
    finally:
        save()
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('plan', 'run', 'guard'))
    parser.add_argument('--manifest', required=True)
    parser.add_argument('--output')
    parser.add_argument('--binary')
    parser.add_argument('--package-root')
    args = parser.parse_args()
    if args.action == 'plan':
        value, signer = manifest(args.manifest)
        print(json.dumps({'source_sha': value['source_sha'], 'signer': signer, 'cells': {
            key: {name: 'READY' if cell else 'PENDING' for name, cell in row['scenarios'].items()}
            for key, row in value['platforms'].items()}, 'product_qualification': False}, indent=2))
    elif args.action == 'guard':
        require(args.binary and args.package_root, 'guard requires binary and package root')
        reject_overrides(os.environ)
        guard(args.manifest, args.binary, args.package_root)
    else:
        require(args.output, 'run requires output')
        print(json.dumps(run(args.manifest, args.output), indent=2))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError,
            tarfile.TarError, zipfile.BadZipFile, layout.LayoutError) as error:
        print('final artifact smoke FAIL: ' + str(error), file=sys.stderr)
        sys.exit(1)
