#!/usr/bin/env python3
"""Pure provenance tests and read-only validation for full Debian packaging."""
import copy
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/package-debian.sh'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def function(name):
    match = re.search(r'^' + re.escape(name) + r'\(\) \{\n.*?^\}\n', SCRIPT.read_text(), re.M | re.S)
    assert match, 'missing function ' + name
    return match.group()


def validate_metadata(actual, expected):
    assert actual['release_artifact'] is False, 'metadata false release required'
    assert actual['skip_build'] is True and actual['reuse_release_binary'] is False, 'metadata local skip-build'
    assert actual['git_dirty'] is False, 'metadata clean packager'
    assert actual == expected, 'full metadata differs from prospective contract'


def inspect_archive(path):
    """Read native dpkg output without extracting files or executing payloads."""
    path = Path(path)
    assert path.is_file() and not path.is_symlink() and path.stat().st_size <= 64 * 1024 * 1024
    raw = path.read_bytes()
    assert raw[:8] == b'!<arch>\n', 'ar magic'
    pos, members = 8, []
    while pos < len(raw):
        h = raw[pos:pos + 60]
        assert len(h) == 60 and h[58:] == b'`\n', 'ar header'
        size = int(h[48:58]); body = raw[pos + 60:pos + 60 + size]
        assert 0 <= size <= 64 * 1024 * 1024 and len(body) == size
        members.append(dict(name=h[:16].decode().strip().rstrip('/'), bytes=size, sha256=sha(body),
                            mtime=int(h[16:28]), uid=int(h[28:34]), gid=int(h[34:40]),
                            mode=int(h[40:48], 8), magic=body[:4].hex()))
        if size % 2:
            assert raw[pos + 60 + size:pos + 61 + size] == b'\n', 'ar padding'
        pos += 60 + size + size % 2
        assert len(members) <= 3, 'extra ar member'
    assert pos == len(raw)
    result = dict(archive=dict(path=str(path), bytes=len(raw), sha256=sha(raw)), ar=members, members={})
    captured = {}
    for kind, flag in [('control', '--ctrl-tarfile'), ('data', '--fsys-tarfile')]:
        raw_tar = subprocess.check_output(['/usr/bin/dpkg-deb', flag, str(path)], timeout=45)
        assert len(raw_tar) <= 64 * 1024 * 1024, 'tar expansion bound'
        rows, seen = [], set()
        with tarfile.open(fileobj=io.BytesIO(raw_tar)) as archive:
            for item in archive:
                assert item.name == '.' or item.name.startswith('./'), 'tar path prefix'
                name = item.name.removeprefix('./')
                assert not PurePosixPath(name).is_absolute() and '..' not in PurePosixPath(name).parts
                assert name not in seen and len(seen) < 256 and not item.linkname
                assert item.isfile() or item.isdir(), 'tar entry type'
                seen.add(name)
                row = dict(path=name, mode=item.mode, uid=item.uid, gid=item.gid,
                           uname=item.uname, gname=item.gname, mtime=item.mtime,
                           bytes=item.size, type='file' if item.isfile() else 'directory',
                           pax_headers=item.pax_headers, header_magic=raw_tar[item.offset + 257:item.offset + 265].hex())
                if item.isfile():
                    stream = archive.extractfile(item); h = hashlib.sha256(); size = 0; content = bytearray()
                    for block in iter(lambda: stream.read(1024 * 1024), b''):
                        h.update(block); size += len(block)
                        if (kind, name) in [('control', 'control'), ('data', 'usr/share/rmux/artifact-metadata.json')]:
                            content.extend(block)
                    assert size == item.size
                    row['sha256'] = h.hexdigest()
                    if content:
                        captured[kind + ':' + name] = bytes(content)
                rows.append(row)
        result['members'][kind] = rows
    return result, captured


def validate_archive(path, freeze):
    inventory, captured = inspect_archive(path)
    assert [m['name'] for m in inventory['ar']] == ['debian-binary', 'control.tar.zst', 'data.tar.zst'], 'ar members'
    assert inventory['ar'][0]['sha256'] == sha(b'2.0\n')
    for member in inventory['ar']:
        assert (member['mtime'], member['uid'], member['gid'], member['mode']) == (freeze['epoch'], 0, 0, 0o100644)
        if member['name'].endswith('.zst'):
            assert member['magic'] == '28b52ffd', 'zstd frame'
    data = inventory['members']['data']; control = inventory['members']['control']
    assert [r['path'] for r in control] == ['.', 'control'], 'unexpected maintainer scripts/control files'
    expected = {f['path']: {k: f[k] for k in ('path', 'bytes', 'sha256', 'mode')} for f in freeze['expected_files']}
    actual = {r['path']: {k: r[k] for k in ('path', 'bytes', 'sha256', 'mode')} for r in data if r['type'] == 'file'}
    assert actual == expected and len(actual) == 14, 'data inventory differs'
    assert [r['path'] for r in data] == sorted([*expected, *freeze['expected_directories']]), 'data order/set'
    assert sorted(r['path'] for r in data if r['type'] == 'directory') == freeze['expected_directories']
    for row in control + data:
        assert row['uid'] == row['gid'] == 0 and row['uname'] == row['gname'] == 'root', 'tar ownership'
        assert row['mtime'] == freeze['epoch'] and row['pax_headers'] == {}, 'tar time/pax'
        assert row['header_magic'] == freeze['tar_header_magic'], 'tar format'
        if row['type'] == 'directory':
            assert row['mode'] == 0o755 and row['bytes'] == 0
    assert control[1]['mode'] == 0o644
    assert captured['control:control'] == Path(freeze['expected_control']['path']).read_bytes(), 'control differs'
    raw_metadata = captured['data:usr/share/rmux/artifact-metadata.json']
    assert raw_metadata == Path(freeze['expected_metadata_bytes']['path']).read_bytes(), 'metadata bytes differ'
    metadata = json.loads(raw_metadata)
    validate_metadata(metadata, freeze['expected_metadata'])
    assert metadata['git_commit'] == freeze['head'] and metadata['generated_at_utc'] == freeze['commit_date']
    assert metadata['payload_provenance']['source'] == freeze['payload_source']['commit']
    assert metadata['payload_provenance']['tree'] == freeze['payload_source']['tree']
    for role, prefix in [('cli', ''), ('helper', 'helper_'), ('daemon', 'daemon_')]:
        declaration = next(f for f in metadata['payload_provenance']['files'] if f['role'] == role)
        item = actual[declaration['path']]
        assert item['sha256'] == declaration['sha256'] == metadata[prefix + 'binary_sha256']
        assert item['bytes'] == metadata[prefix + 'binary_bytes']
    return inventory


class DebianFullProvenance(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.stage = Path(self.tmp.name) / 'stage'; self.stage.mkdir()
        self.roles = {'cli': 'usr/bin/rmux', 'helper': 'usr/libexec/rmux/rmux', 'daemon': 'usr/bin/rmux-daemon'}
        self.declaration = dict(source='b' * 40, tree='1' * 40, configuration='release', files=[])
        for role, relative in self.roles.items():
            p = self.stage / relative; p.parent.mkdir(parents=True, exist_ok=True)
            data = ('never execute fixture ' + role).encode(); p.write_bytes(data)
            self.declaration['files'].append(dict(role=role, sha256=sha(data), bytes=len(data)))

    def invoke(self, declaration):
        p = Path(self.tmp.name) / 'declaration.json'; p.write_text(json.dumps(declaration))
        shell = 'set -euo pipefail\n' + function('debian_payload_provenance') + '\nrepo_root="$1"\nstage_dir="$2"\nconfiguration=release\ndebian_payload_provenance "$3"\n'
        return subprocess.run(['/usr/bin/bash', '-c', shell, 'test', str(ROOT), str(self.stage), str(p)],
                              env=dict(PATH='/usr/bin:/bin', PYTHONDONTWRITEBYTECODE='1'), capture_output=True, text=True, timeout=10)

    def rejected(self, edit, diagnostic):
        d = copy.deepcopy(self.declaration); edit(d); result = self.invoke(d)
        self.assertNotEqual(result.returncode, 0); self.assertIn(diagnostic, result.stderr)
        self.assertEqual(result.stdout, '')

    def test_valid_declared_roles_and_optional_sizes(self):
        result = self.invoke(self.declaration); self.assertEqual(result.returncode, 0, result.stderr)
        actual = json.loads('{' + result.stdout.rstrip().removesuffix(',') + '}')['payload_provenance']
        self.assertEqual(actual['source'], self.declaration['source'])
        self.assertEqual({f['role']: f['path'] for f in actual['files']}, self.roles)
        for item in self.declaration['files']:
            item.pop('bytes')
        self.assertEqual(self.invoke(self.declaration).returncode, 0)

    def test_wrong_hash(self):
        self.rejected(lambda d: d['files'][0].update(sha256='0' * 64), 'declared')

    def test_wrong_size_and_type(self):
        for value in [1, True, '22', -1]:
            with self.subTest(value=value):
                self.rejected(lambda d: d['files'][0].update(bytes=value), 'size differs')

    def test_truncated_payload(self):
        (self.stage / self.roles['cli']).write_bytes(b'')
        self.rejected(lambda d: None, 'declared')

    def test_missing_duplicate_unknown_roles(self):
        self.rejected(lambda d: d['files'].pop(), 'undeclared binary role')
        self.rejected(lambda d: d['files'].append(d['files'][0]), 'role declared twice')
        self.rejected(lambda d: d['files'][0].update(role='alien'), 'unknown binary role')

    def test_configuration(self):
        self.rejected(lambda d: d.update(configuration='debug'), 'configuration differs')

    def test_metadata_false_release(self):
        expected = dict(release_artifact=False, skip_build=True, reuse_release_binary=False, git_dirty=False)
        validate_metadata(expected, expected)
        for field, value in [('release_artifact', True), ('skip_build', False), ('reuse_release_binary', True), ('git_dirty', True)]:
            with self.subTest(field=field), self.assertRaises(AssertionError):
                validate_metadata(dict(expected, **{field: value}), expected)

    def test_optional_field_preserves_metadata_when_absent(self):
        text = '${payload_provenance_field:+$payload_provenance_field\n}  "archive_format": "deb",\n'
        result = subprocess.run(['/usr/bin/bash', '-c', 'set -eu\npayload_provenance_field=""\ncat <<EOF\n' + text + 'EOF\n'], capture_output=True, text=True)
        self.assertEqual((result.returncode, result.stdout), (0, '  "archive_format": "deb",\n'))


if __name__ == '__main__':
    unittest.main(verbosity=2)
