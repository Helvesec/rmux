"""Pure RPM final-step tests; no rpmbuild, Cargo or payload execution.

The native fixed-payload pair remains separate evidence. These tests capture
arguments and environment at the tool boundary and discriminate invalid epoch,
source fallback, target selection, path quoting and tool failure behavior.
"""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/package-rpm.sh'


def function(name):
    match = re.search(r'^' + re.escape(name) + r'\(\) \{\n.*?^\}\n',
                      SCRIPT.read_text(), re.M | re.S)
    if not match:
        raise AssertionError('missing shell function ' + name)
    return match.group()


class RpmArchivePreparation(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.top = self.root / 'top directory'
        (self.top / 'SOURCES').mkdir(parents=True)
        self.source = self.top / 'SOURCES/payload'
        self.source.write_bytes(b'fixed payload\x00\xff')
        self.source.chmod(0o751)
        os.utime(self.source, (123456789, 123456789))
        self.spec = self.root / 'fixed spec.spec'
        self.spec.write_text('fixed spec sentinel\n')
        self.capture = self.root / 'capture.json'
        recorder = self.root / 'record.py'
        recorder.write_text('''import json, os, sys
from pathlib import Path
Path(os.environ['CAPTURE']).write_text(json.dumps({
    'argv': sys.argv[1:], 'epoch': os.environ.get('SOURCE_DATE_EPOCH'),
    'threads': os.environ.get('OMP_NUM_THREADS')}))
''')
        self.recorder = shlex.join([sys.executable, str(recorder)])

    def invoke(self, epoch=None, arch='x86_64', tool_rc=0, git_failure=False):
        env = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'TZ': 'Pacific/Honolulu',
               'CAPTURE': str(self.capture), 'OMP_NUM_THREADS': '2'}
        if epoch is not None:
            env['SOURCE_DATE_EPOCH'] = epoch
        shell = ('set -euo pipefail\n' + function('die') +
                 function('build_rpm_archive') +
                 ('git() { return 31; }\n' if git_failure else '') +
                 'rpmbuild() {\n  ' + self.recorder + ' "$@"\n  return ' +
                 str(tool_rc) + '\n}\n' +
                 'build_rpm_archive "$1" "$2" "$3"\n')
        p = subprocess.run(['bash', '-c', shell, 'test', str(self.top),
                            str(self.spec), arch], cwd=ROOT, env=env,
                           capture_output=True, text=True, umask=0o77)
        capture = json.loads(self.capture.read_text()) if self.capture.exists() else None
        return p, capture

    def check_staging_unchanged(self):
        self.assertEqual(self.source.read_bytes(), b'fixed payload\x00\xff')
        self.assertEqual(self.source.stat().st_mode & 0o7777, 0o751)
        self.assertEqual(self.source.stat().st_mtime_ns, 123456789000000000)
        self.assertEqual(self.spec.read_text(), 'fixed spec sentinel\n')

    def test_epoch_header_macros_and_spaced_paths(self):
        p, capture = self.invoke('1790347419')
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(capture, {
            'argv': ['--define', '_topdir ' + str(self.top),
                     '--define', '_buildhost rmux',
                     '--define', 'use_source_date_epoch_as_buildtime 1',
                     '--define', 'clamp_mtime_to_source_date_epoch 1',
                     '--target', 'x86_64', '-bb', str(self.spec)],
            'epoch': '1790347419', 'threads': '2'})
        self.check_staging_unchanged()

    def test_default_epoch_is_exact_source_commit(self):
        expected = subprocess.check_output(
            ['git', 'show', '-s', '--format=%ct', 'HEAD'], cwd=ROOT, text=True).strip()
        p, capture = self.invoke()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(capture['epoch'], expected)

    def test_explicit_zero_does_not_require_git(self):
        p, capture = self.invoke('0', git_failure=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(capture['epoch'], '0')

    def test_missing_source_epoch_git_failure_propagates(self):
        p, capture = self.invoke(git_failure=True)
        self.assertEqual(p.returncode, 31)
        self.assertIsNone(capture)
        self.check_staging_unchanged()

    def test_invalid_epoch_precedes_tool_and_staging_effects(self):
        for epoch in ['', '-1', '1.5', 'abc', '1\n2', ' 123']:
            with self.subTest(epoch=epoch):
                p, capture = self.invoke(epoch)
                self.assertNotEqual(p.returncode, 0)
                self.assertIn('invalid SOURCE_DATE_EPOCH', p.stderr)
                self.assertIsNone(capture)
                self.check_staging_unchanged()

    def test_tool_failure_propagates(self):
        p, capture = self.invoke('1790347419', tool_rc=23)
        self.assertEqual(p.returncode, 23)
        self.assertIsNotNone(capture)

    def test_arm_target_is_preserved(self):
        p, capture = self.invoke('1790347419', arch='aarch64')
        self.assertEqual(p.returncode, 0, p.stderr)
        i = capture['argv'].index('--target')
        self.assertEqual(capture['argv'][i + 1], 'aarch64')

    def test_existing_target_projection_branches(self):
        shell = ('set -euo pipefail\n' + function('die') +
                 function('rpm_arch_for_target') +
                 function('platform_label_for_target') +
                 'rpm_arch_for_target "$1"; printf "\\n"; platform_label_for_target "$1"')
        for target, expected in [('x86_64-unknown-linux-gnu', 'x86_64\nlinux-x86_64'),
                                 ('aarch64-unknown-linux-gnu', 'aarch64\nlinux-aarch64')]:
            p = subprocess.run(['bash', '-c', shell, 'test', target],
                               capture_output=True, text=True)
            self.assertEqual((p.returncode, p.stdout), (0, expected))
        p = subprocess.run(['bash', '-c', shell, 'test', 'unsupported'],
                           capture_output=True, text=True)
        self.assertNotEqual(p.returncode, 0)
        self.assertIn('unsupported RPM target', p.stderr)


# Fixed-input RPM byte validator used by the prospective native proof. It does
# not run rpm, unpack to disk, execute a payload or consult an RPM database.
import gzip
import hashlib
import struct
from pathlib import PurePosixPath


class RpmValidationError(ValueError):
    pass


def rpm_require(condition, label):
    if not condition:
        raise RpmValidationError(label)


def rpm_sha(raw):
    return hashlib.sha256(raw).hexdigest()


def rpm_header(raw, offset, region):
    rpm_require(raw[offset:offset + 8] == b'\x8e\xad\xe8\x01\0\0\0\0', 'header magic')
    count, length = struct.unpack_from('>II', raw, offset + 8)
    start = offset + 16 + count * 16
    end = start + length
    rpm_require(0 < count < 4096 and end <= len(raw), 'header bounds')
    tags = {}
    for i in range(count):
        tag, kind, local, size = struct.unpack_from('>IIII', raw, offset + 16 + i * 16)
        rpm_require(tag not in tags and local <= length and size < 65536, 'tag bounds')
        pos = start + local
        data = raw[pos:end]
        if kind in (1, 2, 3, 4, 5):
            width = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8}[kind]
            rpm_require(width * size <= len(data), 'integer bounds')
            value = [int.from_bytes(data[j * width:(j + 1) * width], 'big') for j in range(size)]
            span = width * size
        elif kind in (6, 8, 9):
            parts = data.split(b'\0', size)
            rpm_require(len(parts) > size, 'string bounds')
            value = [x.decode('utf-8') for x in parts[:size]]
            span = sum(len(x) + 1 for x in parts[:size])
        else:
            rpm_require(kind == 7 and size <= len(data), 'binary bounds')
            value, span = data[:size].hex(), size
        tags[tag] = dict(kind=kind, count=size, value=value, offset=pos, span=span)
    rpm_require(region in tags and (tags[region]['kind'], tags[region]['count']) == (7, 16), 'region tag')
    marker = bytes.fromhex(tags[region]['value'])
    rpm_require(struct.unpack('>IIiI', marker) == (region, 7, -16 * count, 16), 'region structure')
    return dict(offset=offset, end=end, tags=tags, count=count)


def rpm_cpio(raw):
    entries, pos, seen = [], 0, set()
    while pos < len(raw):
        header = raw[pos:pos + 110]
        rpm_require(header[:6] == b'070701' and len(header) == 110, 'cpio header')
        fields = [int(header[6 + i * 8:14 + i * 8], 16) for i in range(13)]
        ino, mode, uid, gid, links, mtime, size, dmajor, dminor, rmajor, rminor, namesize, check = fields
        rpm_require(1 <= namesize <= 4096 and size < 64 * 1024 * 1024, 'cpio bounds')
        namebytes = raw[pos + 110:pos + 110 + namesize]
        rpm_require(len(namebytes) == namesize and namebytes[-1:] == b'\0', 'cpio name')
        name = namebytes[:-1].decode('utf-8')
        data = (pos + 110 + namesize + 3) // 4 * 4
        content = raw[data:data + size]
        rpm_require(len(content) == size, 'cpio content bounds')
        if name != 'TRAILER!!!':
            path = name.removeprefix('./')
            rpm_require(name == './' + path and not path.startswith('/') and
                        '..' not in PurePosixPath(path).parts and '\\' not in path and
                        str(PurePosixPath(path)) == path and path not in seen, 'cpio unsafe path')
            rpm_require(mode & 0o170000 == 0o100000 and links == 1 and
                        uid == gid == dmajor == dminor == rmajor == rminor == check == 0,
                        'cpio type/link/owner')
            seen.add(path)
        else:
            rpm_require(fields == [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 11, 0], 'cpio trailer')
        entries.append(dict(name=name, fields=fields, sha256=rpm_sha(content),
                            offset=pos, data_offset=data))
        pos = (data + size + 3) // 4 * 4
        if name == 'TRAILER!!!':
            break
    rpm_require(entries and entries[-1]['name'] == 'TRAILER!!!' and not any(raw[pos:]), 'cpio end')
    return entries


def rpm_decode(raw):
    rpm_require(raw[:6] == b'\xed\xab\xee\xdb\x03\x00', 'rpm lead')
    sig = rpm_header(raw, 96, 62)
    main_offset = (sig['end'] + 7) // 8 * 8
    rpm_require(not any(raw[sig['end']:main_offset]), 'signature padding')
    main = rpm_header(raw, main_offset, 63)
    compressed = raw[main['end']:]
    rpm_require(compressed[:10] == bytes.fromhex('1f8b0800000000000203'), 'gzip header')
    cpio = gzip.decompress(compressed)
    rpm_require(len(cpio) < 64 * 1024 * 1024, 'payload bounds')
    return dict(signature=sig, main=main, compressed=compressed, cpio=cpio,
                entries=rpm_cpio(cpio))


def validate_fixed_rpm(raw, historical, epoch=1790347419):
    """Validate new unsigned bytes against fixed historical payload/spec fields."""
    current, reference = rpm_decode(raw), rpm_decode(historical)
    main, tags = current['main'], current['main']['tags']
    sig = current['signature']['tags']
    value = lambda tag: tags[tag]['value']
    rpm_require(raw[:96] == historical[:96], 'lead identity')
    schema = {62: (7, 16), 269: (6, 1), 273: (6, 1), 1000: (4, 1),
              1004: (7, 16), 1007: (4, 1), 1008: (7, 4128)}
    rpm_require({k: (v['kind'], v['count']) for k, v in sig.items()} == schema,
                'unsigned signature schema')
    rpm_require(not any(bytes.fromhex(sig[1008]['value'])), 'signature reserve')
    mainbytes = raw[main['offset']:main['end']]
    rpm_require(sig[269]['value'] == [hashlib.sha1(mainbytes).hexdigest()], 'signature SHA1')
    rpm_require(sig[273]['value'] == [rpm_sha(mainbytes)], 'signature SHA256')
    rpm_require(sig[1004]['value'] == hashlib.md5(raw[main['offset']:]).hexdigest(), 'signature MD5')
    rpm_require(sig[1000]['value'] == [len(raw) - main['offset']], 'signature size')
    rpm_require(sig[1007]['value'] == [len(current['cpio'])], 'signature payload size')
    rpm_require(value(5093) == [8], 'digest algorithm5093')
    rpm_require(value(5092) == [rpm_sha(current['compressed'])], 'compressed digest5092')
    rpm_require(value(5097) == [rpm_sha(current['cpio'])], 'cpio digest5097')
    rt = reference['main']['tags']
    rpm_require(set(tags) == set(rt), 'main tag set')
    exceptions = {63, 1006, 1007, 1034, 5092, 5097}
    for tag in rt:
        rpm_require((tags[tag]['kind'], tags[tag]['count']) == (rt[tag]['kind'], rt[tag]['count']),
                    'tag schema:' + str(tag))
        if tag not in exceptions:
            rpm_require(value(tag) == rt[tag]['value'], 'fixed main tag:' + str(tag))
    rpm_require(value(1006) == [epoch], 'buildtime')
    rpm_require(value(1007) == ['rmux'], 'buildhost')
    rpm_require(value(1034) == [epoch] * 13, 'file mtime')
    entries, expected = current['entries'], reference['entries']
    rpm_require(len(entries) == len(expected) == 14, 'cpio inventory')
    for entry, old in zip(entries, expected):
        fields = old['fields'].copy()
        if old['name'] != 'TRAILER!!!':
            fields[5] = epoch
        rpm_require(entry['name'] == old['name'] and entry['fields'] == fields and
                    entry['sha256'] == old['sha256'], 'fixed cpio:' + old['name'])
    paths = [value(1118)[i] + n for i, n in zip(value(1116), value(1117))]
    rpm_require(['.' + p for p in paths] == [e['name'] for e in entries[:-1]], 'header cpio paths')
    for tag, expected_values in [(1035, [e['sha256'] for e in entries[:-1]]),
                                 (1028, [e['fields'][6] for e in entries[:-1]]),
                                 (1030, [e['fields'][1] for e in entries[:-1]]),
                                 (1034, [e['fields'][5] for e in entries[:-1]]),
                                 (1039, ['root'] * 13), (1040, ['root'] * 13),
                                 (1036, [''] * 13), (5011, [8])]:
        rpm_require(value(tag) == expected_values, 'header cpio tag:' + str(tag))
    return dict(status='PASS', archive_sha256=rpm_sha(raw), bytes=len(raw),
                compressed_payload_sha256=rpm_sha(current['compressed']),
                cpio_sha256=rpm_sha(current['cpio']), digest_algorithm=8,
                main_header_sha256=rpm_sha(mainbytes), signature_checksums_valid=True,
                regions_valid=True, unsigned=True, file_count=13, epoch=epoch,
                historical_SHA_replaced=False)


@unittest.skipUnless(os.environ.get('RMUX_RPM_REPRO_SAMPLE') and
                     os.environ.get('RMUX_RPM_REPRO_SUBJECT'), 'explicit fixed RPM fixtures required')
class RpmDigestOracleCanaries(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.sample = Path(os.environ['RMUX_RPM_REPRO_SAMPLE']).read_bytes()
        cls.subject = Path(os.environ['RMUX_RPM_REPRO_SUBJECT']).read_bytes()
        cls.decoded = rpm_decode(cls.sample)

    def mutate(self, tag=None, data=None, cpio=None, updates=()):
        # Repair outer checksums so each canary reaches its intended invariant.
        raw = bytearray(self.sample)
        decoded = self.decoded
        def patch(header, number, replacement):
            x = header['tags'][number]
            rpm_require(len(replacement) <= x['span'], 'canary replacement bounds')
            raw[x['offset']:x['offset'] + len(replacement)] = replacement
        if tag is not None:
            patch(decoded['main'], tag, data)
        for number, replacement in updates:
            patch(decoded['main'], number, replacement)
        if cpio is not None:
            compressed = gzip.compress(cpio, compresslevel=9, mtime=0)
            compressed = compressed[:9] + decoded['compressed'][9:10] + compressed[10:]
            raw[decoded['main']['end']:] = compressed
            patch(decoded['main'], 5092, rpm_sha(compressed).encode())
            patch(decoded['main'], 5097, rpm_sha(cpio).encode())
        mainbytes = raw[decoded['main']['offset']:decoded['main']['end']]
        patch(decoded['signature'], 269, hashlib.sha1(mainbytes).hexdigest().encode())
        patch(decoded['signature'], 273, rpm_sha(mainbytes).encode())
        patch(decoded['signature'], 1004, hashlib.md5(raw[decoded['main']['offset']:]).digest())
        patch(decoded['signature'], 1000, struct.pack('>I', len(raw) - decoded['main']['offset']))
        patch(decoded['signature'], 1007, struct.pack('>I', len(cpio) if cpio is not None else len(decoded['cpio'])))
        return bytes(raw)

    def reject(self, raw, label):
        with self.assertRaisesRegex(RpmValidationError, re.escape(label)):
            validate_fixed_rpm(raw, self.subject)

    def test_prospective_positive_control(self):
        self.assertEqual(validate_fixed_rpm(self.sample, self.subject)['status'], 'PASS')

    def test_unchanged_cpio_recompression_preserves_header_and_validity(self):
        compress = gzip.compress
        def portable_compress(*args, **kwargs):
            compressed = compress(*args, **kwargs)
            return compressed[:9] + b'\xff' + compressed[10:]
        # CPython 3.13+ emits OS=255 even when the fixed RPM uses Linux OS=3.
        with patch.object(gzip, 'compress', side_effect=portable_compress):
            raw = self.mutate(cpio=self.decoded['cpio'])
        compressed = raw[self.decoded['main']['end']:]
        self.assertEqual(compressed[:10], self.decoded['compressed'][:10])
        self.assertEqual(validate_fixed_rpm(raw, self.subject)['status'], 'PASS')

    def test_old_historical_cpio_digest_rejected(self):
        old = rpm_decode(self.subject)['main']['tags'][5097]['value'][0]
        self.reject(self.mutate(5097, old.encode()), 'cpio digest5097')

    def test_wrong_algorithm_rejected(self):
        self.reject(self.mutate(5093, struct.pack('>I', 10)), 'digest algorithm5093')

    def test_wrong_compressed_digest_rejected(self):
        self.reject(self.mutate(5092, b'0' * 64), 'compressed digest5092')

    def test_wrong_uncompressed_digest_rejected(self):
        self.reject(self.mutate(5097, b'0' * 64), 'cpio digest5097')

    def test_wrong_mtime_with_consistent_digests_rejected(self):
        cpio = bytearray(self.decoded['cpio'])
        cpio[46:54] = b'%08x' % 1790347420
        mtimes = [1790347420] + [1790347419] * 12
        self.reject(self.mutate(cpio=cpio, updates=[(1034, struct.pack('>13I', *mtimes))]), 'file mtime')

    def test_changed_payload_with_consistent_digests_rejected(self):
        cpio = bytearray(self.decoded['cpio'])
        first = self.decoded['entries'][0]
        cpio[first['data_offset']] ^= 1
        changed = cpio[first['data_offset']:first['data_offset'] + first['fields'][6]]
        self.reject(self.mutate(cpio=cpio, updates=[(1035, rpm_sha(changed).encode())]), 'fixed main tag:1035')

    def test_changed_mode_with_consistent_digests_rejected(self):
        cpio = bytearray(self.decoded['cpio'])
        cpio[14:22] = b'%08x' % 0o100754
        self.reject(self.mutate(cpio=cpio, updates=[(1030, struct.pack('>H', 0o100754))]), 'fixed main tag:1030')

    def test_extra_cpio_entry_with_consistent_digests_rejected(self):
        name, payload = b'./usr/extra\0', b'x'
        fields = [14, 0o100644, 0, 0, 1, 1790347419, 1, 0, 0, 0, 0, len(name), 0]
        member = b'070701' + b''.join(b'%08x' % x for x in fields) + name
        member += b'\0' * (-len(member) % 4)
        member += payload + b'\0' * (-len(payload) % 4)
        cpio = self.decoded['cpio']
        pos = self.decoded['entries'][-1]['offset']
        self.reject(self.mutate(cpio=cpio[:pos] + member + cpio[pos:]), 'cpio inventory')

    def test_region_structure_rejected(self):
        self.reject(self.mutate(63, struct.pack('>IIiI', 63, 7, -16, 16)), 'region structure')

    def test_outer_digest_rejected(self):
        raw = bytearray(self.sample)
        pos = self.decoded['signature']['tags'][273]['offset']
        raw[pos:pos + 64] = b'0' * 64
        self.reject(bytes(raw), 'signature SHA256')


if __name__ == '__main__':
    unittest.main(verbosity=2)
