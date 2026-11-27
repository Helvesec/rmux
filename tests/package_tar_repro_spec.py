#!/usr/bin/env python3
"""Check two native tar packaging results against a frozen payload contract."""
import argparse
import base64
import copy
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import tarfile
import unittest


def require(value, message):
    if not value:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def inventory(path):
    compressed = Path(path).read_bytes()
    require(len(compressed) <= 64 * 1024 * 1024, 'archive size bound')
    with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as stream:
        raw = stream.read(64 * 1024 * 1024 + 1)
    require(len(raw) <= 64 * 1024 * 1024, 'tar expansion bound')
    entries = []
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        for m in archive:
            require(len(entries) < 256, 'entry count')
            require((m.isfile() or m.isdir()) and not m.linkname, 'link or special member')
            name = m.name.rstrip('/')
            require(not name.startswith('/') and '..' not in PurePosixPath(name).parts
                    and str(PurePosixPath(name)) == name, 'unsafe archive path')
            header = raw[m.offset:m.offset + 512]
            entry = dict(name=name, bytes=m.size, mode=m.mode, uid=m.uid, gid=m.gid,
                         uname=m.uname, gname=m.gname, mtime=m.mtime, kind='file' if m.isfile() else 'directory',
                         pax_headers=m.pax_headers, offset=m.offset, offset_data=m.offset_data,
                         magic_hex=header[257:265].hex(), header_sha256=digest(header),
                         header_base64=base64.b64encode(header).decode())
            if m.isfile():
                data = archive.extractfile(m).read()
                require(len(data) == m.size, 'short member')
                entry['sha256'] = digest(data)
            entries.append(entry)
    require(len({e['name'] for e in entries}) == len(entries), 'duplicate member')
    return dict(path=str(path), bytes=len(compressed), sha256=digest(compressed),
                gzip_header_hex=compressed[:10].hex(), gzip_flags=compressed[3],
                gzip_mtime=int.from_bytes(compressed[4:8], 'little'),
                tar_bytes=len(raw), tar_sha256=digest(raw), entries=entries)


def check_payload(value, contract):
    root = contract['root_name']
    actual = {e['name'][len(root) + 1:]: {k: e[k] for k in ('bytes', 'mode', 'sha256')}
              for e in value['entries'] if e['kind'] == 'file' and e['name'].startswith(root + '/')}
    expected = {f['path']: {k: f[k] for k in ('bytes', 'mode', 'sha256')} for f in contract['files']}
    require(actual == expected and len(actual) == sum(e['kind'] == 'file' for e in value['entries']),
            'payload file bytes/modes/inventory differ')


def check_canonical(value, contract):
    check_payload(value, contract)
    root, epoch = contract['root_name'], contract['source_date_epoch']
    directories = {root if str(p) == '.' else root + '/' + str(p)
                   for f in contract['files'] for p in PurePosixPath(f['path']).parents}
    files = {root + '/' + f['path'] for f in contract['files']}
    names = [e['name'] for e in value['entries']]
    require(names == sorted(files | directories, key=lambda p: p.encode()), 'archive member order/set differs')
    require(value['gzip_flags'] == 0 and value['gzip_mtime'] == 0, 'gzip name/time differs')
    for entry in value['entries']:
        require(entry['uid'] == entry['gid'] == 0 and entry['uname'] == entry['gname'] == '', 'owner differs')
        require(entry['mtime'] == epoch, 'mtime differs')
        require(entry['magic_hex'] == b'ustar\x0000'.hex() and not entry['pax_headers'], 'tar format/extensions differ')
        if entry['kind'] == 'directory':
            require(entry['name'] in directories and entry['mode'] == 0o755 and entry['bytes'] == 0,
                    'directory mode/set differs')


def check_tools(observed, expected):
    fields = lambda records: {x['name']: {k: x[k] for k in ('path', 'bytes', 'sha256', 'version')} for x in records}
    require(fields(observed) == fields(expected), 'assembler tool pin differs')


def check_assemblies(assemblies, left, right):
    require(isinstance(assemblies, list) and len(assemblies) == 2, 'expected two assembly receipts')
    outputs = []
    for row in assemblies:
        require(isinstance(row, dict) and type(row.get('rc')) is int and row['rc'] == 0,
                'assembly did not succeed')
        archive = row.get('archive')
        require(isinstance(archive, dict) and isinstance(archive.get('path'), str),
                'missing assembly archive binding')
        outputs.append((Path(archive['path']), archive.get('bytes'), archive.get('sha256')))
    expected = [(Path(value['path']), value['bytes'], value['sha256']) for value in (left, right)]
    require(outputs[0] != outputs[1] and all(output in outputs for output in expected),
            'assembly archive path/size/digest binding differs')


class TarReproContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.contract = json.loads(Path(ARGS.contract).read_text())
        cls.left, cls.right = inventory(ARGS.left), inventory(ARGS.right)
        cls.receipt = json.loads(Path(ARGS.receipt).read_text())
        cls.proofs = Path(ARGS.proofs)
        cls.proofs.mkdir(parents=True, exist_ok=True)

    def test_two_actual_archives_match_payload_headers_and_bytes(self):
        for value in (self.left, self.right):
            check_canonical(value, self.contract)
        self.assertEqual(Path(ARGS.left).read_bytes(), Path(ARGS.right).read_bytes())
        self.assertNotEqual(Path(ARGS.left).stat().st_ino, Path(ARGS.right).stat().st_ino)
        check_tools(self.receipt['tools'], self.contract['tools'])
        check_assemblies(self.receipt['assemblies'], self.left, self.right)

    def altered(self, name, transform, reason):
        raw = gzip.decompress(Path(ARGS.left).read_bytes())
        content = transform(bytearray(raw), self.left['entries'])
        path = self.proofs / (name + '.tar.gz')
        with path.open('xb') as f:
            f.write(gzip.compress(content, compresslevel=6, mtime=0))
        value = inventory(path)
        with self.assertRaisesRegex(ValueError, reason):
            check_canonical(value, self.contract)
        (self.proofs / (name + '.json')).write_text(json.dumps(
            dict(case=name, expected='REFUSE', reason=reason, sha256=value['sha256'],
                 original_sha256=self.left['sha256'], raw_inventory=value), indent=2) + '\n')

    def test_archived_file_byte_changed(self):
        def edit(raw, entries):
            entry = next(e for e in entries if e['name'].endswith('/LICENSE'))
            raw[entry['offset_data']] ^= 1
            return raw
        self.altered('file-byte', edit, 'payload file')

    def test_archived_mode_changed_with_valid_header_checksum(self):
        def edit(raw, entries):
            entry = next(e for e in entries if e['name'].endswith('/LICENSE'))
            start = entry['offset']
            raw[start + 100:start + 108] = b'0000600\x00'
            raw[start + 148:start + 156] = b'        '
            checksum = sum(raw[start:start + 512])
            raw[start + 148:start + 156] = f'{checksum:06o}\x00 '.encode()
            return raw
        self.altered('mode', edit, 'payload file')

    def test_archive_order_changed_with_identical_files(self):
        def edit(raw, entries):
            first, second, third = entries[1:4]
            a, b, end = first['offset'], second['offset'], third['offset']
            return raw[:a] + raw[b:end] + raw[a:b] + raw[end:]
        self.altered('order', edit, 'archive member order')

    def test_assembler_tool_pin_changed(self):
        changed = copy.deepcopy(self.receipt['tools'])
        next(t for t in changed if t['name'] == 'tar')['sha256'] = '0' * 64
        with self.assertRaisesRegex(ValueError, 'assembler tool pin'):
            check_tools(changed, self.contract['tools'])
        (self.proofs / 'tool-pin.json').write_text(json.dumps(
            dict(case='tool-pin', expected='REFUSE', observed=changed, original=self.contract['tools']), indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('contract', 'left', 'right', 'receipt', 'proofs'):
        parser.add_argument('--' + name, required=True)
    ARGS, remaining = parser.parse_known_args()
    unittest.main(argv=[__file__, *remaining], verbosity=2)
