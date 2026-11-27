"""Run the four sibling entries with inert payloads and native archive tools.

No Cargo or payload execution. The Unix builder stops at its GLIBC guard;
verifiers consume tiny packages assembled here independently of the builders.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

from package_executable_paths_spec import ExecutablePaths, ROOT, BASH

sys.path.insert(0, str(ROOT / 'scripts/packaging'))
import layout


@unittest.skipUnless(sys.platform.startswith('linux'), 'native Linux package entries')
class SiblingExecutablePaths(unittest.TestCase):
    def setUp(self):
        self.fixture = ExecutablePaths()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.base = self.fixture.base
        self.env = self.fixture.env
        self.env.pop('RMUX_MAX_SUPPORTED_GLIBC', None)
        self.env.pop('READER_STATUS', None)
        self.env['TMPDIR'] = str(self.base)

    def run_tool(self, *args):
        p = subprocess.run(list(map(str, args)), cwd=ROOT, env=self.env,
                           text=True, capture_output=True, timeout=30)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout

    def alias(self, spaced):
        path = self.base / ('source with spaces [literal]' if spaced else 'source')
        if not path.exists():
            path.symlink_to(ROOT, target_is_directory=True)
        return path

    def reset_reader(self, **values):
        self.fixture.capture.unlink(missing_ok=True)
        self.fixture.reader(True)
        for key in ('READER_STATUS', 'RMUX_MAX_SUPPORTED_GLIBC'):
            self.env.pop(key, None)
        self.env.update(VERSIONS='2.31', **values)

    def test_unix_builder_four_calls_floor_validation_and_reader_errors(self):
        for spaced in (False, True):
            for failure in ('floor', 'invalid', 'reader', 'empty', 'missing'):
                with self.subTest(spaced=spaced, failure=failure):
                    # Reuse only the inert input preparation of the frozen test.
                    self.fixture.package('unix', False, reader_status=37)
                    self.reset_reader()
                    if failure == 'floor':
                        self.env['VERSIONS'] = '2.34'
                    elif failure == 'invalid':
                        self.env['RMUX_MAX_SUPPORTED_GLIBC'] = 'invalid'
                    elif failure == 'reader':
                        self.env['READER_STATUS'] = '37'
                    elif failure == 'empty':
                        self.env['VERSIONS'] = ''
                    else:
                        self.env['READELF'] = str(self.base / 'missing reader')
                    output = self.base / 'unix output'
                    p = self.fixture.run_entry(self.alias(spaced) / 'scripts/package-unix.sh',
                        '--target', 'x86_64-unknown-linux-gnu', '--skip-build',
                        '--allow-stale-binary', '--output-dir', output)
                    expected = {'floor': 'require GLIBC_2.34, newer than supported GLIBC_2.31',
                                'invalid': 'invalid RMUX_MAX_SUPPORTED_GLIBC: invalid',
                                'reader': '', 'empty': 'no imported GLIBC symbol versions found',
                                'missing': 'missing required command:'}[failure]
                    self.assertEqual(p.returncode, 37 if failure == 'reader' else 1, p.stderr)
                    if expected:
                        self.assertIn(expected, p.stderr)
                    calls = self.fixture.calls()
                    if failure in ('floor', 'invalid'):
                        self.assertEqual(len(calls), 6)
                        self.assertEqual(calls[:3], calls[3:])
                        self.assertEqual([Path(c[1]).relative_to(output).as_posix().split('/', 1)[1]
                                          for c in calls[:3]],
                                         ['bin/rmux', 'libexec/rmux/rmux', 'bin/rmux-daemon'])
                    else:
                        self.assertEqual(len(calls), 0 if failure == 'missing' else 1)
                    self.assertTrue(all(c[0] == '--version-info' and len(c) == 2 for c in calls))
                    self.assertFalse(list(output.glob('*.tar.gz')))

    def package_fixture(self, variant):
        tree = self.base / ('tree-' + variant)
        tree.mkdir()
        descriptor = layout.load()
        for entry, placement in layout.entries(descriptor, variant):
            path = tree / placement['path']
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'inert payload; never executed\n')
            path.chmod(int(placement['mode'], 8))
        prefix = '' if variant == 'unix-archive' else 'usr/'
        roles = [prefix + 'bin/rmux', prefix + 'libexec/rmux/rmux', prefix + 'bin/rmux-daemon']
        metadata = dict(git_commit='a' * 40, target='x86_64-unknown-linux-gnu',
                        artifact_kind=descriptor['variants'][variant]['artifact_kind'],
                        package_layout=descriptor['variants'][variant]['package_layout'],
                        binary_glibc_min='2.31', helper_binary_glibc_min='2.31',
                        daemon_binary_glibc_min='2.31', package_glibc_min='2.31',
                        max_supported_glibc='2.31')
        for key, role in zip(('binary_sha256', 'helper_binary_sha256', 'daemon_binary_sha256'), roles):
            metadata[key] = hashlib.sha256((tree / role).read_bytes()).hexdigest()
        (tree / (prefix + 'share/rmux/artifact-metadata.json')).write_text(json.dumps(metadata))
        if variant == 'unix-archive':
            manifest = self.run_tool(ROOT / 'scripts/packaging/layout', 'manifest', '--variant', variant)
            (tree / 'share/rmux/package-layout.tsv').write_text(manifest)
            (tree / 'SHA256SUMS.txt').write_text(''.join(
                hashlib.sha256(p.read_bytes()).hexdigest() + '  ' + str(p.relative_to(tree)) + '\n'
                for p in sorted(tree.rglob('*')) if p.is_file() and p.name != 'SHA256SUMS.txt'))
            archive = self.base / 'fixture.tar.gz'
            with tarfile.open(archive, 'w:gz') as out:
                out.add(tree, arcname='fixture')
        elif variant == 'debian':
            (tree / 'DEBIAN').mkdir()
            (tree / 'DEBIAN/control').write_text('Package: rmux\nVersion: 0.11.0\nArchitecture: amd64\n'
                'Maintainer: Fixture <fixture@example.invalid>\nDescription: inert quoting fixture\n'
                'Depends: libc6 (>= 2.31)\n')
            archive = self.base / 'fixture.deb'
            self.run_tool('dpkg-deb', '--build', '--root-owner-group', tree, archive)
        else:
            top = self.base / 'rpm-top'
            for directory in ('BUILD', 'BUILDROOT', 'RPMS', 'SOURCES', 'SPECS', 'SRPMS'):
                (top / directory).mkdir(parents=True)
            spec = top / 'SPECS/fixture.spec'
            files = '\n'.join('/' + str(p.relative_to(tree)) for p in sorted(tree.rglob('*')) if p.is_file())
            spec.write_text('Name: rmux\nVersion: 0.11.0\nRelease: 1\nSummary: inert fixture\n'
                'License: MIT\nBuildArch: x86_64\nRequires: glibc >= 2.31\nAutoReqProv: no\n'
                '%description\nInert fixture\n%install\nmkdir -p "%{buildroot}"\n'
                'cp -a "' + str(tree) + '/." "%{buildroot}/"\n%files\n' + files + '\n')
            self.run_tool(shutil.which('rpmbuild'), '--define', '_topdir ' + str(top), '--define', '__os_install_post %{nil}',
                          '--define', '_build_id_links none', '-bb', spec)
            archive = self.base / 'fixture.rpm'
            shutil.copyfile(next((top / 'RPMS').rglob('*.rpm')), archive)
        (self.base / 'SHA256SUMS.txt').write_text(
            hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
        return archive, roles

    def check_verifier(self, variant, script):
        archive, roles = self.package_fixture(variant)
        for spaced in (False, True):
            for failure in ('none', 'reader', 'empty', 'missing', 'mismatch'):
                with self.subTest(variant=variant, spaced=spaced, failure=failure):
                    self.reset_reader()
                    if failure == 'reader':
                        self.env['READER_STATUS'] = '37'
                    elif failure == 'empty':
                        self.env['VERSIONS'] = ''
                    elif failure == 'missing':
                        self.env['READELF'] = str(self.base / 'missing reader')
                    elif failure == 'mismatch':
                        self.env['VERSIONS'] = '2.34'
                    p = self.fixture.run_entry(self.alias(spaced) / ('scripts/' + script), archive)
                    self.assertEqual(p.returncode, 0 if failure == 'none' else 37 if failure == 'reader' else 1,
                                     p.stderr)
                    calls = self.fixture.calls()
                    self.assertEqual(len(calls), 6 if failure in ('none', 'mismatch') else
                                     0 if failure == 'missing' else 1)
                    if len(calls) == 6:
                        self.assertEqual(calls[:3], calls[3:])
                        self.assertTrue(all(c[0] == '--version-info' and len(c) == 2 for c in calls))
                        for call, role in zip(calls[:3], roles):
                            self.assertTrue(call[1].endswith('/' + role), call)
                    if failure == 'none':
                        self.assertIn('run_binary=false', p.stdout)
                    elif failure == 'empty':
                        self.assertIn('no imported GLIBC symbol versions found', p.stderr)
                    elif failure == 'missing':
                        self.assertIn('missing required command:', p.stderr)
                    elif failure == 'mismatch':
                        self.assertIn('GLIBC', p.stderr)

    def test_tar_verifier(self):
        self.check_verifier('unix-archive', 'verify-package.sh')

    @unittest.skipUnless(shutil.which('dpkg-deb'), 'native dpkg-deb required')
    def test_debian_verifier(self):
        self.check_verifier('debian', 'verify-debian-package.sh')

    @unittest.skipUnless(all(shutil.which(t) for t in ('rpmbuild', 'rpm', 'rpm2cpio', 'cpio')),
                         'native RPM tools required')
    def test_rpm_verifier(self):
        self.check_verifier('rpm', 'verify-rpm-package.sh')


if __name__ == '__main__':
    unittest.main(verbosity=2)
