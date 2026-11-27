"""Inspect critical bytes through the existing native package descriptor."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import stat
import subprocess
from functools import cache
import sys
import tarfile
import zipfile

from strict_json import _unique_object
from sdk_distribution_policy import portable_name, json_object, METADATA_PATH

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'packaging'))
import layout
from layout_profiles import with_notices


def digest(data):
    return hashlib.sha256(data).hexdigest()


def regular(path):
    path = Path(path).absolute()
    if path.resolve(strict=True) != path or not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError(f'evidence must be a regular unredirected file: {path}')
    return path


def object_file(path):
    data = regular(path).read_bytes()
    if len(data) > 1024 * 1024:
        raise ValueError('receipt exceeds metadata limit')
    value = json.loads(data.decode('utf-8'), object_pairs_hook=_unique_object,
                       parse_constant=lambda v: (_ for _ in ()).throw(ValueError(v)))
    if not isinstance(value, dict):
        raise ValueError('receipt must be an object')
    return value


def descriptor(platform):
    name = 'windows-archive' if platform.startswith('windows-') else 'unix-archive'
    value = with_notices(layout.load())
    layout.validate_variant(value, name)
    roles = layout.payload_binary_roles(value, name)
    # The pinned ConPTY executables and DLLs are critical Windows payload too.
    roles.update({entry['id']: placement['path'] for entry, placement in layout.entries(value, name)
                  if entry['source']['kind'] == 'conpty'
                  and entry['source']['name'].endswith(('.exe', '.dll'))})
    return value, name, roles


@cache
def package_manifest(variant):
    return subprocess.check_output([sys.executable, str(layout.DESCRIPTOR.with_name('layout_dispatch.py')),
                                    '--with-notices', 'manifest', '--variant', variant])


def read_archive(path, platform):
    path = regular(path)
    value, variant, roles = descriptor(platform)
    suffix = '.zip' if variant == 'windows-archive' else '.tar.gz'
    if not path.name.endswith(suffix):
        raise ValueError('native archive format differs from platform')
    prefix = path.name[:-len(suffix)]
    files, modes, seen = {}, {}, set()
    total = 0

    def add(name, directory, size, read, mode):
        nonlocal total
        name = portable_name(name.rstrip('/') if directory else name)
        if name in seen or len(seen) >= 4096:
            raise ValueError('duplicate or excessive archive members')
        seen.add(name)
        if name != prefix and not name.startswith(prefix + '/'):
            raise ValueError('archive member outside native package root')
        if directory:
            return
        relative = name[len(prefix) + 1:]
        total += size
        if not relative or size > 256 * 1024**2 or total > 1024**3:
            raise ValueError('archive payload exceeds limit')
        data = read()
        if len(data) != size:
            raise ValueError('archive member size differs')
        files[relative] = data
        modes[relative] = mode

    if suffix == '.zip':
        with zipfile.ZipFile(path) as archive:
            for entry in archive.infolist():
                kind = stat.S_IFMT(entry.external_attr >> 16)
                if kind not in (0, stat.S_IFREG, stat.S_IFDIR):
                    raise ValueError('archive contains a link or special member')
                add(entry.filename, entry.is_dir(), entry.file_size,
                    lambda: archive.read(entry), entry.external_attr >> 16)
    else:
        with tarfile.open(path, 'r:gz') as archive:
            for entry in archive:
                if not (entry.isfile() or entry.isdir()):
                    raise ValueError('archive contains a link or special member')
                add(entry.name, entry.isdir(), entry.size,
                    lambda: archive.extractfile(entry).read(), entry.mode)
    allowed = {placement['path'] for _, placement in layout.entries(value, variant)}
    required = {placement['path'] for _, placement in layout.entries(value, variant)
                if not placement.get('optional')}
    if not required <= files.keys() or not files.keys() <= allowed:
        raise ValueError('native archive file set differs from package descriptor')
    for entry, placement in layout.entries(value, variant):
        name = placement['path']
        if name in files and layout.variant(value, variant)['modes']:
            mode = placement.get('mode') or ('0755' if entry['role'] == 'executable' else '0644')
            if modes[name] != int(mode, 8):
                raise ValueError('native archive file mode differs: ' + name)
    if files['share/rmux/package-layout.tsv'] != package_manifest(variant):
        raise ValueError('native package manifest differs from descriptor')
    metadata = json_object(files[METADATA_PATH], 'native artifact metadata')
    if metadata.get('package_layout') != layout.variant(value, variant)['package_layout']:
        raise ValueError('native package layout identity differs')
    checksums = {}
    for line in files['SHA256SUMS.txt'].decode('ascii').splitlines():
        parts = line.split('  ')
        if len(parts) != 2 or parts[1] in checksums:
            raise ValueError('invalid or duplicate package checksum')
        checksums[portable_name(parts[1])] = parts[0]
    if checksums != {name: digest(data) for name, data in files.items() if name != 'SHA256SUMS.txt'}:
        raise ValueError('package checksum manifest does not cover exact bytes')
    binaries = {role: files[name] for role, name in roles.items()}
    if any(not data for data in binaries.values()):
        raise ValueError('empty critical binary')
    for role, field in [('cli', 'binary'), ('helper', 'helper_binary'), ('daemon', 'daemon_binary')]:
        if (metadata.get(field + '_path') != roles[role]
                or metadata.get(field + '_sha256') != digest(binaries[role])
                or type(metadata.get(field + '_bytes')) is not int
                or metadata[field + '_bytes'] != len(binaries[role])):
            raise ValueError(f'native metadata binary binding differs: {role}')
    inventory = [{'role': role, 'path': roles[role], 'size': len(data), 'sha256': digest(data)}
                 for role, data in sorted(binaries.items())]
    return metadata, inventory, binaries
