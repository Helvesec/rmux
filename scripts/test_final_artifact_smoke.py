"""Regression checks for the final-artifact entry to installed smokes."""
import json
import copy
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
import zipfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/release'))
sys.path.insert(0, str(ROOT / 'tests'))
import final_artifact_smoke as smoke
from package_layout_spec import UNIX_ARCHIVE, WINDOWS_ARCHIVE, fixture_root
import release_tag_policy
from release_tag_policy import AllowedSigner, SignerPolicy

HEAD = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
SOURCE = 'a' * 64
REQUIRED = {
    'macos-aarch64': ('aarch64-apple-darwin', 'macos', ('ssh', 'nested', 'unknown-terminal', 'osc52')),
    'linux-x86_64': ('x86_64-unknown-linux-gnu', 'linux', ('ssh', 'nested', 'unknown-terminal', 'osc52')),
    'windows-x86_64': ('x86_64-pc-windows-msvc', 'windows',
                     ('ssh', 'nested', 'unknown-terminal', 'osc52', 'daemon-survives-terminal-close')),
}
# A Unix package tree can only hold its signed 0755/0644 modes on a POSIX
# filesystem; Windows guards its own ZIP layout, so it exercises only that key.
ARCHIVE_KEYS = tuple(REQUIRED) if os.name == 'posix' else ('windows-x86_64',)


def product_binaries(os_name):
    """Return each packaged executable role and its package-relative path."""
    if os_name == 'windows':
        return {'cli': 'rmux.exe', 'helper': 'libexec/rmux/rmux.exe', 'daemon': 'rmux-daemon.exe'}
    return {'cli': 'bin/rmux', 'helper': 'libexec/rmux/rmux', 'daemon': 'bin/rmux-daemon'}


class SignedArchiveTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.key_root, cleanup = fixture_root(cls.__name__ + '-signer')
        cls.key_root = cls.key_root.resolve()
        if cleanup:
            cls.addClassCleanup(cleanup)
        cls.key = cls.key_root / 'key'
        subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(cls.key)], check=True)
        public = cls.key.with_suffix('.pub').read_text().strip()
        fingerprint = subprocess.check_output(['ssh-keygen', '-l', '-f', str(cls.key)], text=True).split()[1]
        cls.policy = SignerPolicy(cls.key_root, True, '',
                                  (AllowedSigner('fixture@local', public, fingerprint),))

    def setUp(self):
        self.root, cleanup = fixture_root(self.id())
        self.root = self.root.resolve()
        if cleanup:
            self.addCleanup(cleanup)
        self.path = self.root / 'manifest.json'
        self.value = {'schema': 1, 'subject': 'fixture', 'source_sha': HEAD, 'platforms': {}}
        self.packages = {}
        for key, (target, os_name, scenarios) in REQUIRED.items():
            package = self.root / ('package-' + key)
            package.mkdir()
            self.packages[key] = package
            contents = ({name: '0644' for name in WINDOWS_ARCHIVE} if os_name == 'windows'
                        else dict(UNIX_ARCHIVE))
            contents['share/licenses/rmux/THIRD-PARTY-NOTICES.txt'] = '0644'
            for name, mode in contents.items():
                file = package / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes(b'fixture payload\n')
                file.chmod(int(mode, 8))
            metadata = package / 'share/rmux/artifact-metadata.json'
            metadata.write_text(json.dumps({'git_commit': HEAD, 'git_dirty': False, 'target': target,
                                           'release_artifact': True, 'configuration': 'release',
                                           'package_layout': ('rmux-windows-package-v4'
                                                              if os_name == 'windows'
                                                              else 'rmux-package-v4')}))
            for name in product_binaries(os_name).values():
                (package / name).write_bytes(
                    ('RMUX_BUILD_IDENTITY_V1:' + HEAD + ':' + SOURCE + ':' + os_name + ':END').encode())
            archive = self.root / (key + ('.zip' if os_name == 'windows' else '.tar.gz'))
            self.value['platforms'][key] = {
                'target': target, 'archive': {'path': archive.name, 'sha256': ''},
                'package_root': package.name, 'build_source_sha256': SOURCE,
                'scenarios': {name: None for name in scenarios},
            }
            self.repack(key)
        self.sign()

    def repack(self, key):
        row = self.value['platforms'][key]
        package = self.packages[key]
        archive = self.root / row['archive']['path']
        if archive.suffix == '.zip':
            with zipfile.ZipFile(archive, 'w') as container:
                for file in sorted(package.rglob('*')):
                    if file.is_file():
                        container.write(file, package.name + '/' + file.relative_to(package).as_posix())
        else:
            with tarfile.open(archive, 'w:gz') as container:
                container.add(package, arcname=package.name)
        row['archive']['sha256'] = hashlib.sha256(archive.read_bytes()).hexdigest()

    def sign(self, raw=None):
        self.path.write_bytes(raw if raw is not None else json.dumps(self.value).encode())
        Path(str(self.path) + '.sig').unlink(missing_ok=True)
        subprocess.run(['ssh-keygen', '-q', '-Y', 'sign', '-f', str(self.key), '-n', 'git', str(self.path)],
                       check=True, capture_output=True)

    def binary(self, key, role):
        return self.packages[key] / product_binaries(REQUIRED[key][1])[role]

    def guard(self, key='linux-x86_64', binary=None):
        root = self.packages[key]
        binary = binary or self.binary(key, 'cli')
        return smoke.guard(self.path, binary, root, fixture=True, policy=self.policy, head=HEAD, key=key)

    def rejected(self, call, pattern):
        with self.assertRaisesRegex((ValueError, OSError, smoke.layout.LayoutError), pattern):
            call()

    def test_signed_tar_and_zip_bind_all_product_paths_without_product_credit(self):
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key):
                value, _, records = self.guard(key)
                self.assertEqual(value['subject'], 'fixture')
                self.assertEqual(set(records), {'cli', 'helper', 'daemon'})
                self.assertTrue(all(record['commit'] == HEAD for record in records.values()))

    def test_windows_zip_binds_cli_helper_and_daemon(self):
        value, _, records = self.guard('windows-x86_64')
        self.assertEqual(value['subject'], 'fixture')
        self.assertEqual(set(records), {'cli', 'helper', 'daemon'})

    def test_ssh_keygen_gets_file_backed_stdin_and_stderr_with_a_bound(self):
        # Win32-OpenSSH ssh-keygen blocks forever on a stdin pipe or an error
        # written to a stderr pipe; every call must use regular files.
        public = self.key.with_suffix('.pub').read_text().strip()
        with mock.patch.object(release_tag_policy.subprocess, 'run', wraps=subprocess.run) as commands:
            self.guard('windows-x86_64')
            release_tag_policy.ssh_public_key_fingerprint(public)
        self.assertEqual([call.args[0][1] for call in commands.call_args_list], ['-Y', '-l'])
        for call in commands.call_args_list:
            self.assertNotIn('input', call.kwargs)
            self.assertIsInstance(call.kwargs['stdin'], io.BufferedReader)
            self.assertIsInstance(call.kwargs['stderr'], io.BufferedRandom)
            self.assertEqual(call.kwargs['timeout'], release_tag_policy.SSH_KEYGEN_TIMEOUT_SECONDS)

    def test_rejected_signature_and_public_key_return_named_errors(self):
        with self.path.open('ab') as stream:
            stream.write(b' ')
        self.rejected(lambda: self.guard('windows-x86_64'), 'signature must match')
        self.rejected(lambda: release_tag_policy.ssh_public_key_fingerprint('ssh-ed25519 AAAAnotakey x@y'),
                      'invalid SSH public key: .+')

    def test_stalled_ssh_keygen_is_a_named_refusal(self):
        stalled = subprocess.TimeoutExpired(['ssh-keygen'], release_tag_policy.SSH_KEYGEN_TIMEOUT_SECONDS)
        public = self.key.with_suffix('.pub').read_text().strip()
        with mock.patch.object(release_tag_policy.subprocess, 'run', side_effect=stalled):
            self.rejected(lambda: self.guard('windows-x86_64'), 'signature verification exceeded')
            self.rejected(lambda: release_tag_policy.ssh_public_key_fingerprint(public),
                          'public-key fingerprint exceeded')

    def test_canonical_release_archives_bind_each_binary_to_head_without_injection(self):
        self.value['subject'] = 'final-artifact'
        headers = {
            'macos-aarch64': b'\xcf\xfa\xed\xfe' + (0x0100000c).to_bytes(4, 'little'),
            'linux-x86_64': b'\x7fELF\x02\x01' + bytes(12) + (62).to_bytes(2, 'little'),
        }
        pe = bytearray(128)
        pe[:2] = b'MZ'
        pe[60:64] = (64).to_bytes(4, 'little')
        pe[64:68] = b'PE\0\0'
        pe[68:70] = (0x8664).to_bytes(2, 'little')
        pe[88:90] = (0x20b).to_bytes(2, 'little')
        headers['windows-x86_64'] = bytes(pe)
        for key in ARCHIVE_KEYS:
            package = self.packages[key]
            names = (('rmux.exe', 'libexec/rmux/rmux.exe', 'rmux-daemon.exe')
                     if key.startswith('windows') else
                     ('bin/rmux', 'libexec/rmux/rmux', 'bin/rmux-daemon'))
            data = headers[key] + b'RMUX_RELEASE_IDENTITY_V1:' + HEAD.encode() + b':END'
            for name in names:
                (package / name).write_bytes(data)

            def guard():
                self.repack(key)
                self.sign()
                return smoke.guard(self.path, package / names[0], package,
                                   policy=self.policy, head=HEAD, key=key)

            with self.subTest(key=key):
                self.assertEqual(set(guard()[2]), {'cli', 'helper', 'daemon'})
            for name in names:
                for wrong in (headers[key], data.replace(HEAD.encode(), b'0' * 40),
                              data + b'RMUX_RELEASE_IDENTITY_V1:' + b'0' * 40 + b':END',
                              data + b'RMUX_BUILD_IDENTITY_V1:injected',
                              data + b'RMUX_BUILD_GRAPH_V1:injected',
                              data + b'RMUX_BUILD_GRAPH_V2:injected'):
                    with self.subTest(key=key, binary=name, wrong=wrong):
                        (package / name).write_bytes(wrong)
                        self.rejected(guard, 'canonical.*(commit|injection)')
                (package / name).write_bytes(data)

    def test_final_v4_signed_tar_and_zip_accept_notices_before_any_smoke(self):
        notices = 'share/licenses/rmux/THIRD-PARTY-NOTICES.txt'
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key), mock.patch.object(smoke.subprocess, 'run',
                                                        wraps=subprocess.run) as commands:
                row = self.value['platforms'][key]
                files = smoke.archive_files(self.root, row)
                self.assertIn(notices, files)
                self.assertEqual(files[notices][0], b'fixture payload\n')
                _, _, records = self.guard(key)
                self.assertEqual(set(records), {'cli', 'helper', 'daemon'})
                self.assertTrue(all(call.args[0][0] == 'ssh-keygen'
                                    for call in commands.call_args_list))

    def test_final_v4_signed_tar_and_zip_refuse_unknown_file(self):
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key):
                (self.packages[key] / 'unknown-file.txt').write_text('unknown\n')
                self.repack(key)
                self.sign()
                self.rejected(lambda: self.guard(key), 'unexpected package file')

    def test_signed_historical_profiles_accept_without_notices_and_refuse_notices(self):
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key):
                package = self.packages[key]
                notices = package / 'share/licenses/rmux/THIRD-PARTY-NOTICES.txt'
                metadata = package / 'share/rmux/artifact-metadata.json'
                value = json.loads(metadata.read_text())
                variant = smoke.PLATFORMS[key][2]
                settings = smoke.layout.load()['variants'][variant]
                for identity in (settings['package_layout'], *settings.get('legacy_package_layouts', [])):
                    value['package_layout'] = identity
                    metadata.write_text(json.dumps(value))
                    notices.unlink(missing_ok=True)
                    self.repack(key)
                    self.sign()
                    self.assertEqual(set(self.guard(key)[2]), {'cli', 'helper', 'daemon'})
                    notices.write_text('fixture payload\n')
                    self.repack(key)
                    self.sign()
                    self.rejected(lambda: self.guard(key), 'unexpected package file')

    def test_signed_unknown_or_missing_layout_selector_is_refused(self):
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key):
                metadata = self.packages[key] / 'share/rmux/artifact-metadata.json'
                value = json.loads(metadata.read_text())
                for identity in ('rmux-package-v999', None):
                    value['package_layout'] = identity
                    metadata.write_text(json.dumps(value))
                    self.repack(key)
                    self.sign()
                    self.rejected(lambda: self.guard(key), 'unsupported package layout')

    def test_final_v4_extracted_notices_must_match_signed_archive(self):
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key):
                notices = self.packages[key] / 'share/licenses/rmux/THIRD-PARTY-NOTICES.txt'
                notices.write_text('altered\n')
                self.rejected(lambda: self.guard(key), 'extracted archive bytes differ')

    @unittest.skipUnless(os.name == 'posix', 'Unix archive modes')
    def test_final_v4_signed_notices_with_wrong_mode_are_refused(self):
        for key in ('macos-aarch64', 'linux-x86_64'):
            with self.subTest(key=key):
                notices = self.packages[key] / 'share/licenses/rmux/THIRD-PARTY-NOTICES.txt'
                notices.chmod(0o755)
                self.repack(key)
                self.sign()
                self.rejected(lambda: self.guard(key), 'package file mode')

    def executable_fixture(self, driver_source):
        key = 'linux-x86_64'
        package = self.packages[key]
        payload = ('#!/bin/sh\n# RMUX_BUILD_IDENTITY_V1:' + HEAD + ':' + SOURCE + ':linux:END\n'
                   'case "$1" in --help) echo "usage: rmux";; diagnose) echo "{}";; '
                   '*) case "$3" in show-options) echo off;; '
                   'show-environment) echo RMUX_INSTALLED_SMOKE=ok;; esac;; esac\n')
        for name in ('bin/rmux', 'bin/rmux-daemon', 'libexec/rmux/rmux'):
            (package / name).write_text(payload)
        self.repack(key)
        driver = self.root / 'driver.py'
        driver.write_text(driver_source)
        self.value['platforms'][key]['scenarios'] = {
            name: {'driver': {'path': driver.name, 'sha256': smoke.digest(driver.read_bytes())},
                   'tool': {'path': str(Path(sys.executable).resolve()),
                            'sha256': smoke.digest(Path(sys.executable).read_bytes())},
                   'argv': ['{tool}', '{driver}', '{rmux}', '{receipt}', name],
                   'timeout_seconds': 2} for name in ('ssh', 'nested', 'unknown-terminal', 'osc52')}
        self.sign()

    def fixture_run(self):
        with mock.patch.object(smoke, 'host_key', return_value='linux-x86_64'), \
                mock.patch.dict(os.environ, {'RMUX_PYTHON': '/foreign-interpreter'}):
            return smoke.run(self.path, self.root / 'out', fixture=True, policy=self.policy)

    @unittest.skipUnless(os.name == 'posix', 'Unix fixture CLI')
    def test_runner_executes_signed_fixture_drivers_and_never_qualifies_product(self):
        self.executable_fixture('''import hashlib, json, os, pathlib, sys
assert os.environ['RMUX_PYTHON'] == str(pathlib.Path(sys.executable).resolve())
binary, receipt, scenario = sys.argv[1:]
root = pathlib.Path(binary).parent.parent
records = {role: {"path": str(root / name), "sha256": hashlib.sha256((root / name).read_bytes()).hexdigest(),
                  "commit": ''' + repr(HEAD) + '''} for role, name in
           (("cli", "bin/rmux"), ("helper", "libexec/rmux/rmux"), ("daemon", "bin/rmux-daemon"))}
pathlib.Path(receipt).write_text(json.dumps({"scenario": scenario, "status": "PASS",
    "source_sha": ''' + repr(HEAD) + ''', "subject": "fixture", "binaries": records}))
''')
        receipt = self.fixture_run()
        self.assertEqual(receipt['status'], 'PASS')
        self.assertEqual(set(receipt['cells']), {'ssh', 'nested', 'unknown-terminal', 'osc52'})
        self.assertEqual(receipt['subject'], 'fixture')
        self.assertFalse(receipt['product_qualification'])

    @unittest.skipUnless(os.name == 'posix', 'Unix fixture CLI')
    def test_runner_rejects_zero_exit_without_native_receipt(self):
        self.executable_fixture('pass\n')
        self.rejected(self.fixture_run, 'No such file|not a regular|nonregular')
        receipt = json.loads((self.root / 'out/result.json').read_text())
        self.assertEqual(receipt['status'], 'FAIL')
        self.assertEqual(receipt['cells'], {})

    @unittest.skipUnless(os.name == 'posix', 'Unix fixture CLI')
    def test_runner_rejects_wrong_native_identity_even_after_zero_exit(self):
        self.executable_fixture('import pathlib, sys; pathlib.Path(sys.argv[2]).write_text("{}")\n')
        self.rejected(self.fixture_run, 'native evidence identity differs')
        self.assertEqual(json.loads((self.root / 'out/result.json').read_text())['status'], 'FAIL')

    @unittest.skipUnless(os.name == 'posix', 'Unix fixture CLI')
    def test_runner_rejects_changed_signed_driver_before_output_creation(self):
        self.executable_fixture('pass\n')
        (self.root / 'driver.py').write_text('print("changed")\n')
        self.rejected(self.fixture_run, 'file pin mismatch')
        self.assertFalse((self.root / 'out').exists())

    @unittest.skipUnless(os.name == 'posix', 'Unix fixture CLI')
    def test_runner_records_timeout_as_failure(self):
        self.executable_fixture('import time; time.sleep(10)\n')
        with self.assertRaises(subprocess.TimeoutExpired):
            self.fixture_run()
        self.assertEqual(json.loads((self.root / 'out/result.json').read_text())['status'], 'FAIL')

    def test_signed_roster_has_thirteen_cells_and_pending_never_runs(self):
        self.assertEqual(sum(len(row['scenarios']) for row in self.value['platforms'].values()), 13)
        with mock.patch.object(smoke, 'load_signer_policy', return_value=self.policy):
            with mock.patch.object(smoke, 'host_key', return_value='linux-x86_64'):
                self.rejected(lambda: smoke.run(self.path, self.root / 'out', fixture=True), 'scenario pending')
        self.assertFalse((self.root / 'out').exists())

    def test_every_required_scenario_is_mandatory_even_when_signed(self):
        original = copy.deepcopy(self.value)
        for key, (_, _, names) in REQUIRED.items():
            for name in names:
                with self.subTest(key=key, name=name):
                    self.value = copy.deepcopy(original)
                    del self.value['platforms'][key]['scenarios'][name]
                    self.sign()
                    self.rejected(self.guard, 'required scenario absent')

    def test_missing_or_extra_platform_is_refused(self):
        del self.value['platforms']['macos-aarch64']
        self.sign()
        self.rejected(self.guard, 'required platform')

    def test_manifest_base_or_other_candidate_is_refused(self):
        for head in ('76e4cdbdb6afe33ade10533ed8cd9d001f7de83d', '0' * 40):
            self.value['source_sha'] = head
            self.sign()
            self.rejected(self.guard, 'source differs from HEAD')

    def test_other_architecture_is_refused(self):
        self.value['platforms']['linux-x86_64']['target'] = 'aarch64-unknown-linux-gnu'
        self.sign()
        self.rejected(self.guard, 'architecture differs')

    def test_tampered_archive_is_refused_before_extraction(self):
        archive = self.root / self.value['platforms']['linux-x86_64']['archive']['path']
        with archive.open('ab') as stream:
            stream.write(b'altered')
        self.rejected(self.guard, 'file pin mismatch')

    def test_archive_substitution_is_refused(self):
        row = self.value['platforms']['linux-x86_64']
        row['archive']['path'] = self.value['platforms']['macos-aarch64']['archive']['path']
        self.sign()
        self.rejected(self.guard, 'file pin mismatch')

    def test_unmanifested_binary_with_identical_bytes_is_refused(self):
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key):
                cli = self.binary(key, 'cli')
                outsider = self.root / ('other-' + key + '-' + cli.name)
                shutil.copyfile(cli, outsider)
                self.rejected(lambda: self.guard(key, binary=outsider), 'binary outside manifested artifact')

    def test_changed_helper_or_daemon_is_refused(self):
        for key in ARCHIVE_KEYS:
            for role in ('helper', 'daemon'):
                with self.subTest(key=key, role=role):
                    file = self.binary(key, role)
                    original = file.read_bytes()
                    file.write_bytes(b'foreign')
                    self.rejected(lambda: self.guard(key), 'archive bytes differ')
                    file.write_bytes(original)

    def test_signed_archive_with_other_embedded_commit_is_refused(self):
        for key in ARCHIVE_KEYS:
            with self.subTest(key=key):
                file = self.binary(key, 'cli')
                original = file.read_bytes()
                file.write_bytes(original.replace(HEAD.encode(), b'0' * 40))
                self.repack(key)
                self.sign()
                self.rejected(lambda: self.guard(key), 'embedded commit/source/OS differs')
                file.write_bytes(original)
                self.repack(key)
                self.sign()

    def test_signed_archive_with_other_build_source_or_os_is_refused(self):
        for key in ARCHIVE_KEYS:
            os_name = REQUIRED[key][1]
            other_os = 'macos' if os_name != 'macos' else 'linux'
            file = self.binary(key, 'daemon')
            original = file.read_bytes()
            for label, data in (('source', original.replace(SOURCE.encode(), b'b' * 64)),
                                ('os', original.replace((':' + os_name + ':').encode(),
                                                        (':' + other_os + ':').encode()))):
                with self.subTest(key=key, changed=label):
                    self.assertNotEqual(data, original)
                    file.write_bytes(data)
                    self.repack(key)
                    self.sign()
                    self.rejected(lambda: self.guard(key), 'embedded commit/source/OS differs')
            file.write_bytes(original)
            self.repack(key)
            self.sign()

    def test_tampered_manifest_and_signature_are_refused(self):
        with self.path.open('ab') as stream:
            stream.write(b' ')
        self.rejected(self.guard, 'signature must match')
        self.sign()
        signature = Path(str(self.path) + '.sig')
        signature.write_bytes(b'bad')
        self.rejected(self.guard, 'canonical SSH signature')

    def test_untrusted_signature_and_fixture_as_product_are_refused(self):
        policy = smoke.load_signer_policy(ROOT / '.github/release/release-signers.json')
        self.rejected(lambda: smoke.manifest(self.path, fixture=True, policy=policy), 'signature must match')
        self.rejected(lambda: smoke.manifest(self.path, policy=self.policy), 'subject scope')

    def test_duplicate_signed_json_key_is_refused(self):
        raw = self.path.read_bytes().replace(b'"schema": 1', b'"schema": 1, "schema": 1')
        self.sign(raw)
        self.rejected(self.guard, 'strict UTF-8 JSON')

    def test_signed_tar_symlink_and_traversal_are_refused(self):
        for name, kind in (('../escape', tarfile.REGTYPE),
                           ('package-linux-x86_64/link', tarfile.SYMTYPE)):
            archive = self.root / self.value['platforms']['linux-x86_64']['archive']['path']
            with tarfile.open(archive, 'w:gz') as container:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.linkname = '/outside' if kind == tarfile.SYMTYPE else ''
                container.addfile(member, io.BytesIO())
            self.value['platforms']['linux-x86_64']['archive']['sha256'] = smoke.digest(archive.read_bytes())
            self.sign()
            self.rejected(self.guard, 'relative path|nonregular archive entry')

    def test_signed_zip_symlink_is_refused(self):
        row = self.value['platforms']['windows-x86_64']
        archive = self.root / row['archive']['path']
        with zipfile.ZipFile(archive, 'w') as container:
            member = zipfile.ZipInfo(row['package_root'] + '/link')
            member.external_attr = 0o120777 << 16
            container.writestr(member, '/outside')
        row['archive']['sha256'] = smoke.digest(archive.read_bytes())
        self.sign()
        self.rejected(lambda: self.guard('windows-x86_64'), 'ZIP link')

    def test_runtime_binary_overrides_are_refused(self):
        for key in ('RMUX_FULL_BINARY_PATH', 'RMUX_INTERNAL_BINARY_PATH', 'RMUX_SDK_DAEMON_BINARY',
                    'RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB'):
            self.rejected(lambda: smoke.reject_overrides({key: '/foreign'}), 'foreign runtime')

    def test_driver_binding_and_arguments_are_required(self):
        row = self.value['platforms']['linux-x86_64']
        row['scenarios']['ssh'] = {'driver': {'path': 'driver.py', 'sha256': 'a' * 64},
                                   'tool': {'path': str(Path(sys.executable).resolve()), 'sha256': 'b' * 64},
                                   'argv': ['{tool}', '{driver}', '{receipt}'], 'timeout_seconds': 30}
        self.sign()
        self.rejected(self.guard, 'driver must consume')


class FinalArtifactSmokeTests(unittest.TestCase):
    def test_native_headers_reject_other_architecture_and_scripts(self):
        elf = bytearray(64)
        elf[:6] = b'\x7fELF\x02\x01'
        elf[18:20] = (62).to_bytes(2, 'little')
        macho = b'\xcf\xfa\xed\xfe' + (0x0100000c).to_bytes(4, 'little')
        pe = bytearray(128)
        pe[:2] = b'MZ'
        pe[60:64] = (64).to_bytes(4, 'little')
        pe[64:68] = b'PE\0\0'
        pe[68:70] = (0x8664).to_bytes(2, 'little')
        pe[88:90] = (0x20b).to_bytes(2, 'little')
        for key, data, offset in (('linux-x86_64', elf, 18), ('macos-aarch64', macho, 4),
                                  ('windows-x86_64', pe, 68)):
            smoke.native_architecture(data, key)
            for wrong in (b'#!/bin/sh\n', bytes(data[:offset]) + b'\0\0' + bytes(data[offset + 2:])):
                with self.assertRaisesRegex(ValueError, 'architecture differs'):
                    smoke.native_architecture(wrong, key)

    def test_installed_smoke_rejects_binary_outside_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "executed"
            if os.name == "posix":
                outsider = root / "outsider"
                outsider.write_text("#!/bin/sh\ntouch '" + str(marker) +
                                    "'\ncase \"$1\" in --help) echo 'usage: rmux';; *) echo '{}';; esac\n")
                outsider.chmod(0o755)
                command = ["bash", str(ROOT / "scripts/smoke-installed-rmux.sh"), str(outsider), "--skip-daemon"]
            else:
                # Windows installs and smokes through the PowerShell entry, not the POSIX one.
                outsider = root / "outsider.cmd"
                outsider.write_bytes(('@echo off\r\ntype nul > "' + str(marker) +
                                      '"\r\nif "%~1"=="--help" (echo usage: rmux) else (echo {})\r\n').encode())
                powershell = Path(os.environ["SystemRoot"], "System32/WindowsPowerShell/v1.0/powershell.exe")
                command = [str(powershell), "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                           "-File", str(ROOT / "scripts/smoke-installed-rmux.ps1"),
                           "-Rmux", str(outsider), "-SkipDaemon"]
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"binary": str(root / "artifact" / "rmux")}))
            result = subprocess.run(command, stdin=subprocess.DEVNULL, timeout=120,
                                    env=dict(os.environ, RMUX_FINAL_SMOKE_MANIFEST=str(manifest)),
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(marker.exists(), "unmanifested binary was executed")


if __name__ == "__main__":
    unittest.main()
