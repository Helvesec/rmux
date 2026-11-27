"""Pure tests of Debian assembly preparation; no build or payload execution.

The real fixed-payload dpkg-deb pair is separate native evidence. Here a shell
function captures the exact invocation after normalization, allowing failures
and environment handling to be tested without making additional packages.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/package-debian.sh'


def function(name):
    match = re.search(r'^' + re.escape(name) + r'\(\) \{\n.*?^\}\n',
                      SCRIPT.read_text(), re.M | re.S)
    if not match:
        raise AssertionError('missing shell function ' + name)
    return match.group()


class DebianArchivePreparation(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.stage = self.root / 'stage with spaces'
        (self.stage / 'DEBIAN').mkdir(parents=True)
        (self.stage / 'usr/bin').mkdir(parents=True)
        self.contents = {'DEBIAN/control': b'Package: fixture\n',
                         'DEBIAN/postinst': b'never execute this fixture\n',
                         'usr/bin/fixture': b'fixed executable bytes\x00\xff',
                         'usr/data': b'fixed data bytes'}
        self.modes = {'DEBIAN/control': 0o600, 'DEBIAN/postinst': 0o755,
                      'usr/bin/fixture': 0o755, 'usr/data': 0o640}
        for n, contents in self.contents.items():
            p = self.stage / n
            p.write_bytes(contents)
            p.chmod(self.modes[n])
        for p in [self.stage, *self.stage.rglob('*')]:
            if p.is_dir():
                p.chmod(0o700)
            os.utime(p, (123456789, 123456789))

    def invoke(self, epoch=None, tz='UTC', dpkg_rc=0):
        capture = self.root / 'capture.json'
        env = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'TZ': tz,
               'CAPTURE': str(capture), 'DPKG_DEB_THREADS_MAX': '2'}
        if epoch is not None:
            env['SOURCE_DATE_EPOCH'] = epoch
        # This replacement is confined to this subprocess: it records the
        # hand-off without creating an archive or running package contents.
        stub = '''dpkg-deb() {
  python3 - "$@" <<'CAPTURE'
import json, os, sys
from pathlib import Path
Path(os.environ['CAPTURE']).write_text(json.dumps({
    'argv': sys.argv[1:], 'epoch': os.environ.get('SOURCE_DATE_EPOCH'),
    'threads': os.environ.get('DPKG_DEB_THREADS_MAX')}))
CAPTURE
  return ''' + str(dpkg_rc) + '\n}\n'
        shell = ('set -euo pipefail\n' + function('die') +
                 function('build_debian_archive') + stub +
                 'build_debian_archive "$1" "$2"\n')
        p = subprocess.run(['/usr/bin/bash', '-c', shell, 'test',
                            str(self.stage), str(self.root / 'out.deb')],
                           cwd=ROOT, env=env, text=True, capture_output=True,
                           umask=0o77)
        return p, json.loads(capture.read_text()) if capture.exists() else None

    def check_stage(self, epoch):
        for p in [self.stage, *self.stage.rglob('*')]:
            self.assertEqual(p.stat().st_mtime_ns, epoch * 1_000_000_000)
            if p.is_dir():
                self.assertEqual(p.stat().st_mode & 0o7777, 0o755)
        for n, contents in self.contents.items():
            p = self.stage / n
            self.assertEqual(p.read_bytes(), contents)
            self.assertEqual(p.stat().st_mode & 0o7777,
                             0o644 if n == 'DEBIAN/control' else self.modes[n])

    def test_explicit_epoch_ignores_timezone_and_preserves_roles(self):
        p, capture = self.invoke('1790347419', 'Pacific/Honolulu')
        self.assertEqual(p.returncode, 0, p.stderr)
        self.check_stage(1790347419)
        self.assertEqual(capture, {'argv': ['--root-owner-group', '--build',
                                          str(self.stage), str(self.root / 'out.deb')],
                                   'epoch': '1790347419', 'threads': '2'})

    def test_fallback_is_exact_commit_epoch(self):
        expected = int(subprocess.check_output(
            ['git', 'show', '-s', '--format=%ct', 'HEAD'], cwd=ROOT, text=True))
        p, capture = self.invoke()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(capture['epoch'], str(expected))
        self.check_stage(expected)

    def test_zero_is_a_valid_explicit_epoch(self):
        p, capture = self.invoke('0')
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(capture['epoch'], '0')
        self.check_stage(0)

    def test_invalid_epoch_fails_before_mutation_or_dpkg(self):
        for epoch in ['', '-1', '1.5', 'abc', '1\n2', ' 123']:
            with self.subTest(epoch=epoch):
                p, capture = self.invoke(epoch)
                self.assertNotEqual(p.returncode, 0)
                self.assertIn('invalid SOURCE_DATE_EPOCH', p.stderr)
                self.assertIsNone(capture)
                self.assertEqual(self.stage.stat().st_mode & 0o7777, 0o700)
                self.assertEqual(self.stage.stat().st_mtime_ns, 123456789000000000)
                for n, contents in self.contents.items():
                    self.assertEqual((self.stage / n).read_bytes(), contents)

    def test_dpkg_failure_propagates(self):
        p, capture = self.invoke('1790347419', dpkg_rc=23)
        self.assertEqual(p.returncode, 23)
        self.assertIsNotNone(capture)

    def test_existing_target_branches(self):
        shell = ('set -euo pipefail\n' + function('die') +
                 function('deb_arch_for_target') +
                 function('platform_label_for_target') +
                 'deb_arch_for_target "$1"; printf "\\n"; platform_label_for_target "$1"')
        for target, expected in [('x86_64-unknown-linux-gnu', 'amd64\nlinux-x86_64'),
                                 ('aarch64-unknown-linux-gnu', 'arm64\nlinux-aarch64')]:
            p = subprocess.run(['/usr/bin/bash', '-c', shell, 'test', target],
                               capture_output=True, text=True)
            self.assertEqual((p.returncode, p.stdout), (0, expected))
        p = subprocess.run(['/usr/bin/bash', '-c', shell, 'test', 'unsupported'],
                           capture_output=True, text=True)
        self.assertNotEqual(p.returncode, 0)
        self.assertIn('unsupported Debian target', p.stderr)


if __name__ == '__main__':
    unittest.main(verbosity=2)
