"""Executable path quoting through the real GLIBC, Debian and RPM entries.

The package entries stage inert payloads and stop at their existing GLIBC
guard, before archiving. No Cargo, package builder or payload is executed.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BASH = shutil.which('bash')


class ExecutablePaths(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.capture = self.base / 'arguments.jsonl'
        self.env = dict(os.environ, LC_ALL='C', CAPTURE=str(self.capture),
                        RMUX_PACKAGE_STRIP_TINY='0', SOURCE_DATE_EPOCH='1')
        self.tool_dir = self.base / 'tools'
        self.tool_dir.mkdir()
        # rpmbuild is only checked for availability. The GLIBC guard must stop
        # both builders before they can invoke an archive tool.
        guard = self.tool_dir / 'rpmbuild'
        guard.write_text('#!/bin/sh\necho unexpected-archive-tool >&2\nexit 99\n')
        guard.chmod(0o755)
        self.env['PATH'] = str(self.tool_dir) + os.pathsep + os.environ['PATH']

    def reader(self, spaced=False):
        tool = self.tool_dir / ('read elf [literal]' if spaced else 'reader')
        tool.write_text('#!' + sys.executable + '\n' + '''import json, os, sys
with open(os.environ['CAPTURE'], 'a') as out:
    out.write(json.dumps(sys.argv[1:]) + '\\n')
status = int(os.environ.get('READER_STATUS', '0'))
if status:
    sys.exit(status)
for version in os.environ.get('VERSIONS', '2.9 2.31').split():
    print('  Name: GLIBC_' + version + '  Flags: none  Version: 2')
''')
        tool.chmod(0o755)
        self.env['READELF'] = str(tool)
        return tool

    def calls(self):
        return ([json.loads(line) for line in self.capture.read_text().splitlines()]
                if self.capture.exists() else [])

    def run_entry(self, script, *args):
        return subprocess.run([BASH, str(script), *map(str, args)],
                              cwd=ROOT, env=self.env, text=True,
                              capture_output=True, timeout=30)

    def test_glibc_executable_and_binary_paths_preserve_arguments(self):
        for spaced in (False, True):
            with self.subTest(spaced=spaced):
                self.capture.unlink(missing_ok=True)
                self.reader(spaced)
                binaries = [self.base / 'one ELF [literal]', self.base / 'two ELF']
                for binary in binaries:
                    binary.write_bytes(b'inert payload')
                self.env['VERSIONS'] = '2.9 2.31 2.100 2.31'
                p = self.run_entry(ROOT / 'scripts/glibc-symbol-floor.sh', *binaries)
                self.assertEqual((p.returncode, p.stdout), (0, '2.100\n'), p.stderr)
                self.assertEqual(self.calls(), [['--version-info', str(b)] for b in binaries])

    def test_glibc_error_contracts(self):
        binary = self.base / 'input ELF'
        binary.write_bytes(b'inert payload')
        entry = ROOT / 'scripts/glibc-symbol-floor.sh'
        for spaced in (False, True):
            with self.subTest(spaced=spaced):
                self.reader(spaced)
                self.env['READER_STATUS'] = '37'
                p = self.run_entry(entry, binary)
                self.assertEqual(p.returncode, 37, p.stderr)
                del self.env['READER_STATUS']
                self.env['VERSIONS'] = ''
                p = self.run_entry(entry, binary)
                self.assertEqual(p.returncode, 1)
                self.assertIn('no imported GLIBC symbol versions found', p.stderr)
                del self.env['VERSIONS']
                p = self.run_entry(entry, self.base / 'absent ELF')
                self.assertEqual(p.returncode, 1)
                self.assertIn('ELF binary not found:', p.stderr)
        self.env['READELF'] = str(self.base / 'missing reader')
        p = self.run_entry(entry, binary)
        self.assertEqual(p.returncode, 1)
        self.assertIn('missing required command:', p.stderr)
        self.assertEqual(self.run_entry(entry).returncode, 2)
        self.assertEqual(self.run_entry(entry, '--help').returncode, 0)

    def package(self, variant, spaced, maximum=None, reader_status=None):
        self.capture.unlink(missing_ok=True)
        self.reader(spaced)
        alias = self.base / ('repo with spaces [literal]' if spaced else 'repo')
        if not alias.exists():
            alias.symlink_to(ROOT, target_is_directory=True)
        target = self.base / 'payload target'
        profile = target / 'x86_64-unknown-linux-gnu/release'
        profile.mkdir(parents=True, exist_ok=True)
        for name in ('rmux', 'rmux-full', 'rmux-daemon'):
            payload = profile / name
            payload.write_bytes(b'inert payload, never executed\n')
            payload.chmod(0o755)
        completions = self.base / 'completion cache'
        completions.mkdir(exist_ok=True)
        for name in ('rmux.bash', '_rmux', 'rmux.fish', '_rmux.ps1', 'rmux.elv'):
            (completions / name).write_text('inert completion\n')
        self.env.update(CARGO_TARGET_DIR=str(target), RMUX_COMPLETIONS_DIR=str(completions),
                        VERSIONS='2.34')
        for key, value in [('RMUX_MAX_SUPPORTED_GLIBC', maximum),
                           ('READER_STATUS', reader_status)]:
            self.env.pop(key, None)
            if value is not None:
                self.env[key] = str(value)
        output = self.base / ('output ' + variant)
        p = self.run_entry(alias / ('scripts/package-' + variant + '.sh'),
                           '--skip-build', '--allow-stale-binary', '--output-dir', output)
        if variant == 'debian':
            stage = output / 'debian/rmux_0.11.0_amd64'
            roles = [stage / 'usr/bin/rmux', stage / 'usr/libexec/rmux/rmux',
                     stage / 'usr/bin/rmux-daemon']
        else:
            roles = [output / 'rpmbuild/SOURCES' / name
                     for name in ('rmux', 'rmux-full', 'rmux-daemon')]
        self.assertFalse(list(output.rglob('*.deb')) + list(output.rglob('*.rpm')))
        self.assertNotIn('unexpected-archive-tool', p.stderr)
        return p, roles

    @unittest.skipUnless(sys.platform.startswith('linux'), 'native Linux staging tools')
    def test_package_entries_preserve_all_four_glibc_invocations_and_floor(self):
        for variant in ('debian', 'rpm'):
            for spaced in (False, True):
                with self.subTest(variant=variant, spaced=spaced):
                    p, roles = self.package(variant, spaced)
                    self.assertEqual(p.returncode, 1, p.stderr)
                    self.assertIn('require GLIBC_2.34, newer than supported GLIBC_2.31', p.stderr)
                    self.assertEqual(self.calls(), [['--version-info', str(b)] for b in roles * 2])

    @unittest.skipUnless(sys.platform.startswith('linux'), 'native Linux staging tools')
    def test_package_entries_preserve_validation_and_reader_failure(self):
        for variant in ('debian', 'rpm'):
            for spaced in (False, True):
                with self.subTest(variant=variant, spaced=spaced):
                    p, roles = self.package(variant, spaced, maximum='invalid')
                    self.assertEqual(p.returncode, 1, p.stderr)
                    self.assertIn('invalid RMUX_MAX_SUPPORTED_GLIBC: invalid', p.stderr)
                    self.assertEqual(self.calls(), [['--version-info', str(b)] for b in roles * 2])
                    p, roles = self.package(variant, spaced, reader_status=37)
                    self.assertEqual(p.returncode, 37, p.stderr)
                    self.assertEqual(self.calls(), [['--version-info', str(roles[0])]])

    @unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('readelf'),
                         'native Linux readelf')
    def test_native_readelf_through_a_spaced_executable_path(self):
        native = Path(shutil.which('readelf')).resolve()
        alias = self.base / 'native read elf [literal]'
        alias.symlink_to(native)
        binary = Path('/bin/ls')
        entry = ROOT / 'scripts/glibc-symbol-floor.sh'
        self.env['READELF'] = str(native)
        normal = self.run_entry(entry, binary)
        self.env['READELF'] = str(alias)
        spaced = self.run_entry(entry, binary)
        self.assertEqual(normal.returncode, 0, normal.stderr)
        self.assertRegex(normal.stdout, r'^\d+(?:\.\d+)+\n$')
        self.assertEqual((spaced.returncode, spaced.stdout), (0, normal.stdout), spaced.stderr)


if __name__ == '__main__':
    unittest.main(verbosity=2)
