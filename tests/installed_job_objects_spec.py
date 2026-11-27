"""Frozen causal regressions for R1-380 F1-F3; no native product verdict."""
from contextlib import ExitStack
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts/release'))
import installed_job_objects as collector
from release.release_evidence import file_hash


class WitnessStop(RuntimeError):
    pass


class InstalledJobObjectsSpec(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.head = 'a' * 40
        self.installed = self.root / 'portable'
        self.installed.mkdir()
        self.images = [self.installed / 'rmux.exe',
                       self.installed / 'libexec/rmux/rmux.exe',
                       self.installed / 'rmux-daemon.exe']
        self.fields = ['binary_sha256', 'helper_binary_sha256', 'daemon_binary_sha256']
        marker = f'RMUX_BUILD_IDENTITY_V1:{self.head}:{"b" * 64}:windows:END'.encode()
        self.metadata = {'git_commit': self.head, 'artifact_kind': 'windows-package-binary',
                         'configuration': 'release', 'release_artifact': True,
                         'rmux_version': '0.10.0'}
        for image, field in zip(self.images, self.fields):
            image.parent.mkdir(parents=True, exist_ok=True)
            image.write_bytes(marker)
            self.metadata[field] = file_hash(image)
        archive, checksums = self.root / 'package.zip', self.root / 'SHA256SUMS'
        archive.write_bytes(b'frozen package fixture')
        checksums.write_text(file_hash(archive))
        self.plan = {'sha': self.head, 'archive': str(archive), 'checksums': str(checksums),
                     'archive_sha256': file_hash(archive), 'checksums_sha256': file_hash(checksums),
                     'output': str(self.root / 'output'), 'authority': 'signed-local',
                     'python': str(self.root / 'Python With Spaces/python.exe'),
                     'powershell': str(self.root / 'powershell.exe')}
        self.commands, self.launches = [], []
        self.shell_probe = None
        self.bad_signature = None
        self.launch_error = WitnessStop('qualified launcher attempted')
        self.construct_error = None
        self.native = Mock()
        daemon = self.native.process.return_value
        daemon.image.return_value = str(self.images[2])
        daemon.exited.return_value = False

    def collect(self):
        metadata_path = self.installed / 'share/rmux/artifact-metadata.json'
        metadata_path.parent.mkdir(parents=True, exist_ok=True)
        metadata_path.write_text(json.dumps(self.metadata))
        plan_path = self.root / 'plan.json'
        plan_path.write_text(json.dumps(self.plan))
        fixture = self

        class Job:
            def __init__(self, native, allow):
                if fixture.construct_error:
                    raise fixture.construct_error

            def launch(self, command, env, directory, log):
                fixture.launches.append(command)
                if fixture.launch_error:
                    raise fixture.launch_error
                return 0

            def close(self):
                pass

        def run(command, **kwargs):
            self.commands.append(command)
            if self.shell_probe and Path(command[0]).name == 'powershell.exe':
                self.shell_probe(command, kwargs)
            if '-Binary' in command and command[-1] == str(self.bad_signature):
                raise subprocess.CalledProcessError(1, command)
            if Path(command[0]).name == 'rmux.exe':
                args = command[3:]
                if args[:4] == ['new-session', '-d', '-s', 'descendants']:
                    raise WitnessStop('descendant argv captured')
                return subprocess.CompletedProcess(command, 0, stdout='123\n', stderr='')
            return subprocess.CompletedProcess(command, 0)

        with ExitStack() as stack:
            stack.enter_context(patch.object(collector.sys, 'platform', 'win32'))
            stack.enter_context(patch.dict(sys.modules, {
                'installed_job_objects_windows': SimpleNamespace(Native=lambda: self.native, Job=Job)}))
            stack.enter_context(patch('candidate_identity.source_identity'))
            stack.enter_context(patch('candidate_identity.git', return_value='c' * 40))
            stack.enter_context(patch.object(collector, 'install', return_value=self.installed))
            stack.enter_context(patch.object(collector.subprocess, 'run', side_effect=run))
            return collector.collect(plan_path)

    def check_shell_environment(self, signatures):
        shell, pwsh = shutil.which('powershell.exe'), shutil.which('pwsh')
        native = bool(shell and pwsh)
        real_run = subprocess.run
        inherited = 'powershell-7/Modules'
        if native:
            inherited = real_run(
                [pwsh, '-NoProfile', '-NonInteractive', '-Command', '$env:PSModulePath'],
                capture_output=True, text=True, check=True, timeout=60).stdout.strip()
            self.plan['powershell'] = shell
        observations = []

        def probe(command, options):
            if ('-Binary' in command) != signatures:
                return
            result = None
            if native:
                # Execute the collector's generated signature script against a
                # real signed system image; the package/Job fixtures stay inert.
                argv = [*command[:-1], shell] if signatures else [
                    shell, '-NoProfile', '-NonInteractive', '-Command',
                    'Get-Command Get-AuthenticodeSignature -ErrorAction Stop | Out-Null; '
                    "'signature-cmdlet=resolved'"]
                result = real_run(argv, env=options.get('env'),
                                  capture_output=True, text=True, timeout=60)
            observations.append((options.get('env'), result))

        self.shell_probe = probe
        self.construct_error = WitnessStop('stop before native Job construction')
        with patch.dict(os.environ, {'PSModulePath': inherited}):
            with self.assertRaises(WitnessStop):
                self.collect()
            self.assertEqual(os.environ['PSModulePath'], inherited)
        self.assertEqual(len(observations), 3 if signatures else 1)
        for environment, result in observations:
            if result is not None:
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(result.stderr, '', result.stderr)
            self.assertIsNotNone(environment, 'PowerShell inherited the caller module path')
            self.assertNotIn('PSMODULEPATH', {key.upper() for key in environment})
        self.assertEqual(self.launches, [])

    def test_package_shell_ignores_powershell_7_module_path(self):
        self.check_shell_environment(signatures=False)

    def test_signature_shell_ignores_powershell_7_module_path(self):
        self.check_shell_environment(signatures=True)

    def cli_commands(self):
        return [command for command in self.commands if Path(command[0]).name == 'rmux.exe']

    def signature_process(self, setup='', binary=None):
        # Generate the exact production script without launching product binaries.
        with self.assertRaises(WitnessStop):
            self.collect()
        script = Path(self.plan['output']) / 'verify-signature.ps1'
        shell = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
        wrapper = self.root / 'signature-witness.ps1'
        wrapper.write_text(
            'param([string]$Verifier, [string]$Binary)\n' + setup +
            '\n& $Verifier -Binary $Binary\nexit $LASTEXITCODE\n')
        environment = {key: value for key, value in os.environ.items()
                       if key.upper() != 'PSMODULEPATH'}
        return subprocess.run(
            [str(shell), '-NoProfile', '-NonInteractive', '-File', str(wrapper),
             str(script), str(binary or self.images[0])], env=environment,
            capture_output=True, text=True, timeout=30)

    @unittest.skipUnless(sys.platform == 'win32', 'native Windows signature verifier')
    def test_signature_missing_cmdlet_fails(self):
        result = self.signature_process(
            "Remove-Module Microsoft.PowerShell.Security -ErrorAction SilentlyContinue\n"
            "$PSModuleAutoLoadingPreference = 'None'\n"
            "if (Get-Command Get-AuthenticodeSignature -ErrorAction SilentlyContinue) { exit 91 }")
        self.assertNotEqual(result.returncode, 91, 'missing-cmdlet fixture failed')
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('CommandNotFoundException', result.stderr)

    @unittest.skipUnless(sys.platform == 'win32', 'native Windows signature verifier')
    def test_signature_nonterminating_error_even_with_valid_result_fails(self):
        result = self.signature_process(
            "function Get-AuthenticodeSignature { param($LiteralPath) "
            "Write-Error 'S1902 signature failure'; [pscustomobject]@{Status='Valid'} }")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('S1902 signature failure', result.stderr)

    @unittest.skipUnless(sys.platform == 'win32', 'native Windows signature verifier')
    def test_signature_status_must_be_valid(self):
        for status in ('NotSigned', 'HashMismatch', 'NotTrusted', 'UnknownError',
                       'NotSupportedFileFormat', 'Incompatible', '', None):
            with self.subTest(status=status):
                self.setUp()
                value = '$null' if status is None else f"[pscustomobject]@{{Status='{status}'}}"
                result = self.signature_process(
                    'function Get-AuthenticodeSignature { param($LiteralPath) ' + value + ' }')
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    @unittest.skipUnless(sys.platform == 'win32', 'native Windows signature verifier')
    def test_signature_real_valid_passes(self):
        signed = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
        result = self.signature_process(binary=signed)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stderr, '')

    @unittest.skipUnless(sys.platform == 'win32', 'native Windows signature verifier')
    def test_signature_real_unsigned_and_missing_fail(self):
        for missing in (False, True):
            with self.subTest(missing=missing):
                self.setUp()
                result = self.signature_process(binary=self.root / 'missing.exe' if missing else None)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_f1_every_binary_rejection_executes_no_cli_or_launcher(self):
        for index, field in enumerate(self.fields):
            for rejection in ('marker', 'hash', 'signature'):
                with self.subTest(binary=index, rejection=rejection):
                    self.setUp()
                    if rejection == 'marker':
                        self.images[index].write_bytes(b'unmarked rejected executable')
                        self.metadata[field] = file_hash(self.images[index])
                    elif rejection == 'hash':
                        self.metadata[field] = '0' * 64
                    else:
                        self.bad_signature = self.images[index]
                    with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                        self.collect()
                    self.assertEqual(self.launches, [])
                    self.assertEqual(self.cli_commands(), [])

    def test_f1_cleanup_requires_an_attempt_not_just_qualification(self):
        self.construct_error = WitnessStop('job construction failed before launch')
        with self.assertRaises(WitnessStop):
            self.collect()
        self.assertEqual(self.launches, [])
        self.assertEqual(self.cli_commands(), [])

    def test_f1_cleanup_follows_a_qualified_failed_launch(self):
        with self.assertRaises(WitnessStop):
            self.collect()
        self.assertEqual(len(self.launches), 1)
        self.assertEqual([command[3:] for command in self.cli_commands()], [['kill-server']])

    def test_f2_descendant_uses_distinct_argv_with_powershell_and_spaces(self):
        # The configured shell must never receive this as a quoted command string.
        self.launch_error = None
        with self.assertRaisesRegex(WitnessStop, 'descendant argv captured'):
            self.collect()
        command = next(command for command in self.cli_commands()
                       if command[3:7] == ['new-session', '-d', '-s', 'descendants'])
        self.assertEqual(command[7:9], [self.plan['python'], '-c'])
        self.assertEqual(len(command), 10)
        self.assertIn('subprocess.Popen', command[9])

    def test_f3_rc_without_hosted_provenance_stays_not_played(self):
        self.plan.update(authority='rc', release_ref='v0.10.0-rc.1')
        with self.assertRaises(ValueError):
            self.collect()
        self.assertEqual(self.launches, [])
        self.assertEqual(self.cli_commands(), [])
        receipt = json.loads((Path(self.plan['output']) / 'receipt.json').read_text())
        self.assertEqual(receipt['status'], 'not-played')
        self.assertEqual(receipt['cells'], [])


if __name__ == '__main__':
    unittest.main()
