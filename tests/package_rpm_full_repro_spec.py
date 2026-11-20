"""Full RPM entry time/provenance contract; never execute Cargo or payload ELF.

The native pair runs the real package script separately. Pure tests simulate
wall dates only at the date boundary; archive checks reuse the accepted RPM013
reader and digest canaries with a prospective full-entry file/metadata oracle.
"""
import copy
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import struct
import subprocess
import sys
import tempfile
import unittest

import package_rpm_repro_spec as rpm

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/package-rpm.sh'


def function(name):
    match = re.search(r'^' + re.escape(name) + r'\(\) \{\n.*?^\}\n', SCRIPT.read_text(), re.M | re.S)
    if not match:
        raise AssertionError('missing function: ' + name)
    return match.group()


class FullEntryTime(unittest.TestCase):
    def invoke(self, epoch=None, wall='Sat Jan 01 2000', baseline=False, git_failure=False):
        env = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'TZ': 'Pacific/Honolulu', 'WALL_DATE': wall}
        if epoch is not None:
            env['SOURCE_DATE_EPOCH'] = epoch
        # Only unqualified wall-clock queries are simulated. Explicit epoch
        # conversion is still performed by the actual native GNU date binary.
        shim = '''date() {
  case " $* " in
    *" -d @"*) /usr/bin/date "$@" ;;
    *) printf '%s\\n' "$WALL_DATE" ;;
  esac
}
'''
        code = 'set -euo pipefail\n' + function('die') + shim
        if git_failure:
            code += 'git() { return 31; }\n'
        if baseline:
            code += 'changelog_date="$(LC_ALL=C date -u \'+%a %b %d %Y\')"\n'
        else:
            code += function('set_rpm_source_time') + 'set_rpm_source_time\n'
        code += 'printf "%s\\n%s\\n" "$changelog_date" "${SOURCE_DATE_EPOCH-unset}"\n'
        return subprocess.run(['/usr/bin/bash', '-c', code], cwd=ROOT, env=env,
                              capture_output=True, text=True)

    def test_two_simulated_wall_days_expose_baseline_and_fix(self):
        days = ['Sat Jan 01 2000', 'Sun Jan 02 2000']
        old = [self.invoke('1790347419', day, baseline=True) for day in days]
        fixed = [self.invoke('1790347419', day) for day in days]
        self.assertTrue(all(p.returncode == 0 for p in old + fixed))
        self.assertNotEqual(old[0].stdout, old[1].stdout)
        self.assertEqual(fixed[0].stdout, fixed[1].stdout)
        self.assertEqual(fixed[0].stdout, 'Fri Sep 25 2026\n1790347419\n')

    def test_default_exact_commit_epoch(self):
        epoch = subprocess.check_output(['git', 'show', '-s', '--format=%ct', 'HEAD'], cwd=ROOT, text=True).strip()
        p = self.invoke()
        self.assertEqual(p.returncode, 0, p.stderr)
        date = datetime.datetime.fromtimestamp(int(epoch), datetime.timezone.utc).strftime('%a %b %d %Y')
        self.assertEqual(p.stdout, date + '\n' + epoch + '\n')

    def test_explicit_epoch_does_not_need_git(self):
        p = self.invoke('0', git_failure=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout, 'Thu Jan 01 1970\n0\n')

    def test_invalid_epochs_refused(self):
        for epoch in ['', '-1', '1.5', '1\n2', 'abc', ' 123', '99999999999999999999999999999']:
            with self.subTest(epoch=epoch):
                p = self.invoke(epoch)
                self.assertNotEqual(p.returncode, 0)
                self.assertIn('invalid SOURCE_DATE_EPOCH', p.stderr)
                self.assertEqual(p.stdout, '')

    def test_git_failure_propagates(self):
        self.assertEqual(self.invoke(git_failure=True).returncode, 31)

    def test_same_exported_epoch_reaches_existing_assembly_helper(self):
        code = ('set -euo pipefail\n' + function('die') + function('set_rpm_source_time') +
                function('build_rpm_archive') +
                'rpmbuild() { printf "%s\\n%s\\n" "$SOURCE_DATE_EPOCH" "$changelog_date"; }\n' +
                'set_rpm_source_time\nSOURCE_DATE_EPOCH=1790347419 build_rpm_archive /unused /unused x86_64\n')
        p = subprocess.run(['/usr/bin/bash', '-c', code], cwd=ROOT, text=True, capture_output=True,
                           env={'PATH': '/usr/bin:/bin', 'SOURCE_DATE_EPOCH': '1790347419', 'TZ': 'UTC'})
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout, '1790347419\nFri Sep 25 2026\n')

    def test_full_entry_resolves_before_staging_and_never_uses_wall_changelog(self):
        source = SCRIPT.read_text()
        self.assertLess(source.index('\nset_rpm_source_time\n'), source.index('mkdir -p "$work_dir/BUILD"'))
        self.assertNotIn('changelog_date="$(LC_ALL=C date -u \'+', source)
        self.assertEqual(source.count('\nset_rpm_source_time\n'), 1)


class PayloadSourceBinding(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        (self.work / 'SOURCES').mkdir()
        files = []
        for role, name in [('cli', 'rmux'), ('daemon', 'rmux-daemon'), ('helper', 'rmux-full')]:
            data = (role + '-fixed-bytes').encode()
            (self.work / 'SOURCES' / name).write_bytes(data)
            files.append(dict(role=role, sha256=hashlib.sha256(data).hexdigest()))
        self.declaration = dict(source='a' * 40, tree='b' * 40, configuration='release', files=files)

    def invoke(self, declaration):
        path = self.work / 'declaration.json'
        path.write_text(json.dumps(declaration))
        code = ('set -euo pipefail\nrepo_root="$1"\nwork_dir="$2"\nconfiguration=release\n' +
                function('rpm_payload_provenance') + 'rpm_payload_provenance "$3"\n')
        return subprocess.run(['/usr/bin/bash', '-c', code, 'test', str(ROOT), str(self.work), str(path)],
                              capture_output=True, text=True, env={'PATH': '/usr/bin:/bin', 'PYTHONDONTWRITEBYTECODE': '1'})

    def test_declared_source_is_bound_to_actual_staged_bytes(self):
        p = self.invoke(self.declaration)
        self.assertEqual(p.returncode, 0, p.stderr)
        value = json.loads('{' + p.stdout.rstrip().removesuffix(',') + '}')['payload_provenance']
        self.assertEqual(value['source'], 'a' * 40)
        self.assertEqual({f['role']: f['sha256'] for f in value['files']},
                         {f['role']: f['sha256'] for f in self.declaration['files']})
        self.assertEqual(len(list(self.work.glob('payload-provenance.*'))), 0)
        self.assertTrue(all(p.stat().st_nlink == 1 for p in (self.work / 'SOURCES').iterdir()))

    def test_wrong_digest_or_missing_role_refused(self):
        for mutation in ['digest', 'missing', 'duplicate']:
            d = copy.deepcopy(self.declaration)
            if mutation == 'digest':
                d['files'][0]['sha256'] = '0' * 64
            elif mutation == 'missing':
                d['files'].pop()
            else:
                d['files'].append(d['files'][0])
            with self.subTest(mutation=mutation):
                p = self.invoke(d)
                self.assertNotEqual(p.returncode, 0)
                self.assertEqual(p.stdout, '')

    def test_wrong_configuration_refused(self):
        self.declaration['configuration'] = 'debug'
        p = self.invoke(self.declaration)
        self.assertNotEqual(p.returncode, 0)
        self.assertIn('configuration differs', p.stderr)


def validate_full_rpm(raw, historical, expected_files, expected_metadata, epoch):
    """RPM013 integrity rules plus a frozen full-entry source/metadata oracle.

    Metadata/changelog are generated by this entry, so the fixed historical
    metadata assumption of validate_fixed_rpm cannot apply. Every replacement
    is derived from the prospective contract, never copied from the new RPM.
    """
    require = rpm.rpm_require
    current, reference = rpm.rpm_decode(raw), rpm.rpm_decode(historical)
    main, tags, sig = current['main'], current['main']['tags'], current['signature']['tags']
    value = lambda tag: tags[tag]['value']
    require(raw[:96] == historical[:96], 'lead identity')
    schema = {62: (7, 16), 269: (6, 1), 273: (6, 1), 1000: (4, 1),
              1004: (7, 16), 1007: (4, 1), 1008: (7, 4128)}
    require({k: (v['kind'], v['count']) for k, v in sig.items()} == schema, 'unsigned signature schema')
    require(not any(bytes.fromhex(sig[1008]['value'])), 'signature reserve')
    mainbytes = raw[main['offset']:main['end']]
    require(sig[269]['value'] == [hashlib.sha1(mainbytes).hexdigest()], 'signature SHA1')
    require(sig[273]['value'] == [rpm.rpm_sha(mainbytes)], 'signature SHA256')
    require(sig[1004]['value'] == hashlib.md5(raw[main['offset']:]).hexdigest(), 'signature MD5')
    require(sig[1000]['value'] == [len(raw) - main['offset']], 'signature size')
    require(sig[1007]['value'] == [len(current['cpio'])], 'signature payload size')
    require(value(5093) == [8], 'digest algorithm5093')
    require(value(5092) == [rpm.rpm_sha(current['compressed'])], 'compressed digest5092')
    require(value(5097) == [rpm.rpm_sha(current['cpio'])], 'cpio digest5097')
    original = reference['main']['tags']
    require(set(tags) == set(original), 'main tag set')
    expected_files = sorted(expected_files, key=lambda f: f['path'])
    changes = {1006: [epoch], 1007: ['rmux'], 1009: [sum(f['bytes'] for f in expected_files)],
               1028: [f['bytes'] for f in expected_files], 1035: [f['sha256'] for f in expected_files],
               1034: [epoch] * 13, 1080: [epoch // 86400 * 86400 + 43200]}
    for tag, prior in original.items():
        require((tags[tag]['kind'], tags[tag]['count']) == (prior['kind'], prior['count']), 'tag schema:' + str(tag))
        if tag not in {63, 5092, 5097}:
            require(value(tag) == changes.get(tag, prior['value']), 'full entry main tag:' + str(tag))
    entries = current['entries']
    require(len(entries) == len(expected_files) + 1 == 14, 'cpio inventory')
    paths = [value(1118)[i] + n for i, n in zip(value(1116), value(1117))]
    require(['.' + p for p in paths] == [e['name'] for e in entries[:-1]], 'header cpio paths')
    for entry, old, expected in zip(entries[:-1], reference['entries'][:-1], expected_files):
        fields = old['fields'].copy()
        fields[5], fields[6] = epoch, expected['bytes']
        require(entry['name'] == './' + expected['path'] == old['name']
                and entry['fields'] == fields and entry['sha256'] == expected['sha256']
                and entry['fields'][1] == 0o100000 + expected['mode'], 'full entry file:' + expected['path'])
    entry = next(e for e in entries if e['name'] == './usr/share/rmux/artifact-metadata.json')
    metadata = json.loads(current['cpio'][entry['data_offset']:entry['data_offset'] + entry['fields'][6]])
    require(metadata == expected_metadata, 'full entry metadata')
    require(metadata['release_artifact'] is False and metadata['skip_build'] is True
            and metadata['reuse_release_binary'] is False and metadata['git_dirty'] is False
            and metadata['git_commit'] != metadata['payload_provenance']['source'], 'local acquired payload provenance')
    return dict(status='PASS', archive_sha256=rpm.rpm_sha(raw), bytes=len(raw),
                compressed_payload_sha256=rpm.rpm_sha(current['compressed']),
                cpio_sha256=rpm.rpm_sha(current['cpio']), digest_algorithm=8,
                unsigned=True, file_count=13, epoch=epoch,
                packager_commit=metadata['git_commit'], payload_source=metadata['payload_provenance']['source'])


@unittest.skipUnless(os.environ.get('RMUX_RPM_FULL_FREEZE'), 'native full-entry pair required')
class FullEntryArchive(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.freeze = json.loads(Path(os.environ['RMUX_RPM_FULL_FREEZE']).read_bytes())
        cls.sample = Path(os.environ['RMUX_RPM_FULL_SAMPLE']).read_bytes()
        cls.subject = Path(cls.freeze['historical_rpm']['path']).read_bytes()
        cls.decoded = rpm.rpm_decode(cls.sample)

    # Reuse accepted canary construction, including outer checksum repairs.
    mutate = rpm.RpmDigestOracleCanaries.mutate

    def validate(self, raw):
        return validate_full_rpm(raw, self.subject, self.freeze['expected_files'],
                                 self.freeze['expected_metadata'], self.freeze['epoch'])

    def test_positive_full_entry(self):
        self.assertEqual(self.validate(self.sample)['status'], 'PASS')

    def test_changed_digest5093(self):
        with self.assertRaisesRegex(rpm.RpmValidationError, 'digest algorithm5093'):
            self.validate(self.mutate(5093, struct.pack('>I', 10)))

    def test_changed_digest5092(self):
        with self.assertRaisesRegex(rpm.RpmValidationError, 'compressed digest5092'):
            self.validate(self.mutate(5092, b'0' * 64))

    def test_changed_digest5097(self):
        with self.assertRaisesRegex(rpm.RpmValidationError, 'cpio digest5097'):
            self.validate(self.mutate(5097, b'0' * 64))

    def test_changed_changelog_time(self):
        with self.assertRaisesRegex(rpm.RpmValidationError, 'full entry main tag:1080'):
            self.validate(self.mutate(1080, struct.pack('>I', self.freeze['epoch'])))

    def test_changed_payload_with_consistent_digests(self):
        cpio = bytearray(self.decoded['cpio'])
        first = self.decoded['entries'][0]
        cpio[first['data_offset']] ^= 1
        changed = cpio[first['data_offset']:first['data_offset'] + first['fields'][6]]
        with self.assertRaisesRegex(rpm.RpmValidationError, 'full entry main tag:1035'):
            self.validate(self.mutate(cpio=cpio, updates=[(1035, rpm.rpm_sha(changed).encode())]))

    def test_false_current_payload_source(self):
        expected = copy.deepcopy(self.freeze['expected_metadata'])
        expected['payload_provenance']['source'] = expected['git_commit']
        with self.assertRaisesRegex(rpm.RpmValidationError, 'full entry metadata'):
            validate_full_rpm(self.sample, self.subject, self.freeze['expected_files'], expected, self.freeze['epoch'])


if __name__ == '__main__':
    unittest.main(verbosity=2)
