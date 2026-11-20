#!/usr/bin/env python3
"""Build object-cold Unix packages, then compare exact critical executable bytes.

build uses the canonical packager without compiler wrappers and creates a private
fresh target. compare accepts two receipts and optionally joins the left archive
to a canonical-build-record.json beside assets/, pinned by its producer digest.
No publication occurs.
"""
from __future__ import annotations

import argparse
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import zipfile

from canonical_contract import PLATFORMS
from independent_archive import digest, read_archive, layout
from independent_comparison import compare
from canonical_payload import prepare, provenance, embedded_commit

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
import candidate_identity as identity


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def env_option(key, default):
    value = os.environ.get(key, default)
    if key in {'RMUX_PACKAGE_CODESIGN_ADHOC', 'RMUX_PACKAGE_STRIP_TINY'}:
        if value not in {'0', '1'}:
            raise ValueError('invalid packaging option: ' + key)
    elif value and re.fullmatch(r'[0-9]+\.[0-9]+', value) is None:
        raise ValueError('invalid packaging option: ' + key)
    return value


def cargo_configs():
    roots = [ROOT, *ROOT.parents]
    paths = [root / '.cargo' / name for root in roots for name in ('config', 'config.toml')]
    paths += [Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')) / name for name in ('config', 'config.toml')]
    result = {}
    for path in paths:
        if path.exists():
            result[str(path)] = digest(path.read_bytes())
    return result


def native_tools(packaging):
    names = ['cc', 'strip']
    if packaging['linux_glibc_floor']:
        names += ['cargo-zigbuild', 'zig']
    if packaging['codesign_adhoc'] == '1':
        names += ['codesign']
    result = {}
    for name in names:
        path = shutil.which(name)
        if path:
            result[name] = digest(Path(path).read_bytes())
        elif name in {'cargo-zigbuild', 'zig', 'codesign'}:
            raise ValueError('required native tool unavailable: ' + name)
    return result


def build(args):
    head = args.source_sha
    identity.source_identity(ROOT, head)
    if args.platform_key.startswith('windows-'):
        raise ValueError('native Windows producer remains package-windows.ps1; build command is Unix only')
    triple = PLATFORMS[args.platform_key][3]
    rustc = command(shutil.which(os.environ.get('RUSTC', 'rustc')), '-vV')
    if 'host: ' + triple not in rustc.splitlines():
        raise ValueError('native toolchain host differs from requested platform')
    # Reject overrides rather than omit them from provenance.
    for key, value in os.environ.items():
        if (key.startswith('CARGO_PROFILE_') or key.startswith('CARGO_BUILD_') and key != 'CARGO_BUILD_JOBS'
                or key in {'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC_WORKSPACE_WRAPPER',
                           'RMUX_COMPLETIONS_DIR', 'CC', 'CXX', 'CFLAGS', 'CXXFLAGS', 'LDFLAGS'}
                or key.startswith('CARGO_TARGET_') and key.endswith(('_LINKER', '_RUSTFLAGS'))):
            if value:
                raise ValueError('unrecorded build override: ' + key)
    output = args.output_dir.absolute()
    if output.exists() or output.resolve() != output:
        raise ValueError('independent output directory must be new and unredirected')
    source, target = prepare(identity, ROOT, head)
    output.mkdir(parents=True)
    epoch = identity.git(source, 'show', '-s', '--format=%ct', head)
    options = dict(configuration='release', target=triple, normalizations=[],
                   cargo_locked=True, cargo_incremental=False, source_date_epoch=epoch,
                   features={'cli': ['default', 'tiny-cli'], 'helper': ['default'], 'daemon': ['default']},
                   cargo_toml_sha256=digest((ROOT / 'Cargo.toml').read_bytes()),
                   cargo_lock_sha256=digest((ROOT / 'Cargo.lock').read_bytes()),
                   layout_sha256=digest((ROOT / 'scripts/packaging/rmux-package-layout.json').read_bytes()),
                   notice_layout_sha256=digest((ROOT / 'scripts/packaging/notice-layout.json').read_bytes()),
                   packaging=dict(linux_glibc_floor=env_option('RMUX_LINUX_GLIBC_FLOOR', ''),
                                  max_supported_glibc=env_option('RMUX_MAX_SUPPORTED_GLIBC', ''),
                                  codesign_adhoc=env_option('RMUX_PACKAGE_CODESIGN_ADHOC', '0'),
                                  strip_tiny=env_option('RMUX_PACKAGE_STRIP_TINY', '1')),
                   cargo_configs=cargo_configs())
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='0', SOURCE_DATE_EPOCH=epoch)
    argv = [str(ROOT / 'scripts/package-unix.sh'), '--configuration', 'release',
            '--target', triple, '--platform-label', args.platform_key, '--output-dir', str(output)]
    with (output / 'build.log').open('wb') as log:
        subprocess.run(argv, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    identity.source_identity(source, head)
    archives = list(output.glob('*.tar.gz'))
    if len(archives) != 1:
        raise ValueError('producer did not create one native archive')
    archive = archives[0]
    metadata, inventory, binaries = read_archive(archive, args.platform_key)
    if metadata['git_commit'] != head or metadata['release_artifact'] is not True:
        raise ValueError('producer archive source differs')
    # Bind original Cargo outputs and packaged bytes, with no path or graph
    # injection. The marker is the same commit for both independent targets.
    producers = {role: provenance(target / triple / 'release' / name, source, head, target, identity.host_os())
                 for role, name in [('cli', 'rmux'), ('helper', 'rmux-full'), ('daemon', 'rmux-daemon')]}
    for data in binaries.values():
        embedded_commit(data, head)
    receipt = dict(schema_version=2, source_sha=head, platform_key=args.platform_key,
                   toolchain=dict(rustc=rustc, cargo=command(shutil.which('cargo'), '-V'),
                                  native_tools=native_tools(options['packaging'])),
                   options=options, build=dict(id=target.name, target_dir=str(target),
                   fresh_target=True, object_cache_restored=False, producers=producers), binaries=inventory,
                   archive=dict(path=archive.name, size=archive.stat().st_size,
                                sha256=digest(archive.read_bytes())))
    (output / 'independent-build.json').write_text(json.dumps(receipt, sort_keys=True, indent=2) + '\n')
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    create = commands.add_parser('build')
    create.add_argument('--source-sha', required=True)
    create.add_argument('--platform-key', required=True, choices=sorted(PLATFORMS))
    create.add_argument('--output-dir', required=True, type=Path)
    verify = commands.add_parser('compare')
    verify.add_argument('--source-sha', required=True)
    verify.add_argument('--left', required=True, type=Path)
    verify.add_argument('--right', required=True, type=Path)
    verify.add_argument('--output', required=True, type=Path)
    verify.add_argument('--canonical-record', type=Path)
    verify.add_argument('--expected-canonical-record-sha256')
    args = parser.parse_args()
    try:
        return build(args) if args.command == 'build' else compare(args)
    except (ValueError, OSError, KeyError, TypeError, UnicodeError, tarfile.TarError, zipfile.BadZipFile,
            layout.LayoutError, identity.IdentityError, subprocess.CalledProcessError) as error:
        print('independent builds: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
