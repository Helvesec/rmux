"""Windows consumers must stage and attest the pinned pair before VT execution."""
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import zipfile


class CandidateConptyRuntime(unittest.TestCase):
    def test_conpty_hash_consumers_without_get_file_hash(self):
        shell = shutil.which('pwsh') or shutil.which('powershell.exe')
        if not shell:
            self.skipTest('PowerShell is required for the runtime hash regression')
        scripts = Path(__file__).resolve().parent
        audit = scripts.parent / '.rmux-audit/conpty-acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            root = Path(directory)
            for name in ('runtime.ps1', 'candidate-runtime.ps1'):
                shutil.copyfile(scripts / 'conpty' / name, root / name)
            # Hashing fixtures only: no native runtime/signature claim.
            data = b'abc'
            (root / 'LICENSE').write_bytes(data)
            package = root / 'fixture.zip'
            with zipfile.ZipFile(package, 'w'):
                pass
            manifest = dict(target='x86_64-pc-windows-msvc', files=[],
                            pe_machine=0x8664, package='hash-fixture', version='1',
                            publisher='fixture', url='unused',
                            sha256=hashlib.sha256(package.read_bytes()).hexdigest(),
                            license=dict(path='share/rmux/LICENSE',
                                         sha256=hashlib.sha256(data).hexdigest()))
            (root / 'runtime.json').write_text(json.dumps(manifest))
            pe = bytearray(128)
            struct.pack_into('<H', pe, 0, 0x5a4d)
            struct.pack_into('<I', pe, 0x3c, 64)
            struct.pack_into('<IH', pe, 64, 0x4550, 0x8664)
            (root / 'fixture.exe').write_bytes(pe)
            command = r'''
param($Root, $Missing)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Missing -eq 'yes') {
    # Keep the utility commands, but exclude Get-FileHash (function or cmdlet).
    Get-Command Get-FileHash | Out-Null
    $utility = Get-Module Microsoft.PowerShell.Utility
    $cmdlets = @($utility.ExportedCmdlets.Keys | Where-Object { $_ -ne 'Get-FileHash' })
    Remove-Module Microsoft.PowerShell.Utility
    Import-Module Microsoft.PowerShell.Utility -Cmdlet $cmdlets -Function NoSuchFunction
    # Load filesystem commands before disabling discovery of omitted commands.
    Get-Command New-Item | Out-Null
    $PSModuleAutoLoadingPreference = 'None'
    if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) { throw 'hash still available' }
}
. (Join-Path $Root 'runtime.ps1')
$target = 'x86_64-pc-windows-msvc'
$manifest = Get-ConptyManifest $target
$stage = Join-Path $Root ('stage-' + $Missing)
New-Item -ItemType Directory -Path "$stage/libexec/rmux", "$stage/share/rmux" | Out-Null
Assert-ConptyHash (Join-Path $Root 'LICENSE') $manifest.license.sha256.ToUpperInvariant()
Copy-ConptyRuntime $target (Join-Path $Root 'fixture.zip') $stage
foreach ($name in @('rmux.exe', 'rmux-daemon.exe', 'libexec/rmux/rmux.exe')) {
    Copy-Item -LiteralPath (Join-Path $Root 'fixture.exe') -Destination (Join-Path $stage $name)
}
Assert-ConptyPackage $stage $target
. (Join-Path $Root 'candidate-runtime.ps1') -Stage $stage -Package (Join-Path $Root 'fixture.zip')
$proof = Get-Content -LiteralPath (Join-Path $stage 'provenance.json') -Raw | ConvertFrom-Json
Write-Output ('manifest-sha256=' + $proof.manifest_sha256)
foreach ($case in @('tampered', 'missing', 'directory')) {
    $path = Join-Path $stage $case
    if ($case -eq 'tampered') { [IO.File]::WriteAllText($path, 'abd') }
    if ($case -eq 'directory') { New-Item -ItemType Directory -Path $path | Out-Null }
    $rejected = $false
    try { Assert-ConptyHash $path $manifest.license.sha256 }
    catch { if ($_.Exception.Message -notlike '*SHA-256 mismatch*') { throw }; $rejected = $true }
    if (-not $rejected) { throw "accepted $case" }
}
# Hash streams must be closed, including after a mismatch.
$stream = [IO.File]::Open((Join-Path $stage 'tampered'), 'Open', 'ReadWrite', 'None')
$stream.Dispose()
Write-Output 'hash-consumers=PASS'
'''
            script = root / 'check.ps1'
            script.write_text(command)
            expected = hashlib.sha256((root / 'runtime.json').read_bytes()).hexdigest()
            for missing in ('no', 'yes'):
                with self.subTest(get_file_hash_missing=missing):
                    result = subprocess.run(
                        [shell, '-NoProfile', '-NonInteractive', '-File', str(script),
                         str(root), missing], capture_output=True, text=True, timeout=60)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn('manifest-sha256=' + expected, result.stdout)
                    self.assertIn('hash-consumers=PASS', result.stdout)

    def test_windows_launchers_require_pinned_runtime(self):
        scripts = Path(__file__).resolve().parent
        spec = importlib.util.spec_from_file_location(
            'conpty_retention', scripts / 'candidate_identity_retention.py')
        retention = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(retention)
        audit = scripts.parent / '.rmux-audit/conpty-acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as root:
            target = Path(root)
            image = target / 'wrap1.exe'
            image.write_bytes(b'qualified test image')
            record = dict(path=str(image), commit='a' * 40, target=str(target))
            calls = []

            def pinned_launch(g, actual, args):
                self.assertEqual(actual, record)
                self.assertEqual(args, ['--exact', 'wrap1'])
                # The runtime boundary owns staging, verification and native
                # load receipts. Bypassing it must never execute this image.
                for name in ('conpty.dll', 'OpenConsole.exe'):
                    image.with_name(name).write_bytes(name.encode())
                calls.append(actual)
                return 0

            runtime = SimpleNamespace(launch=Mock(side_effect=pinned_launch))
            guard = SimpleNamespace(
                context=lambda: (scripts.parent, 'a' * 40, target),
                verify_built=Mock(return_value=record), record_launch=Mock(),
                support=Mock(return_value=runtime))
            native = SimpleNamespace(name='nt', execv=Mock(return_value=0))
            for replace in (False, True):
                with self.subTest(replace=replace):
                    for name in ('conpty.dll', 'OpenConsole.exe'):
                        image.with_name(name).unlink(missing_ok=True)
                    with patch.object(retention, 'os', native), \
                            patch.object(retention.subprocess, 'call', return_value=0):
                        self.assertEqual(retention.launch(
                            guard, [str(image), '--exact', 'wrap1'], replace), 0)
                    self.assertTrue(image.with_name('conpty.dll').is_file(),
                                    'Windows launcher omitted pinned ConPTY')
                    self.assertTrue(image.with_name('OpenConsole.exe').is_file(),
                                    'Windows launcher omitted pinned OpenConsole')
                    guard.support.assert_called_with('candidate_conpty_runtime')
            self.assertEqual(len(calls), 2)
        gate = (scripts / 'gate-windows-fast.ps1').read_text()
        self.assertIn('Initialize-CandidateConptyRuntime', gate,
                      'nextest/direct gate images also require the pinned pair')

    def test_provenance_shell_ignores_a_powershell_7_module_path(self):
        # gate-windows-fast.ps1 runs under PowerShell 7; its exported module
        # path must not reach the Windows PowerShell provenance verifier.
        scripts = Path(__file__).resolve().parent
        spec = importlib.util.spec_from_file_location(
            'conpty_runtime_modules', scripts / 'candidate_conpty_runtime.py')
        runtime = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runtime)
        windows_powershell = shutil.which('powershell.exe')
        pwsh = shutil.which('pwsh')
        native = bool(windows_powershell and pwsh)
        if native:
            inherited = subprocess.run(
                [pwsh, '-NoProfile', '-NonInteractive', '-Command', '$env:PSModulePath'],
                capture_output=True, text=True, timeout=60, check=True).stdout.strip()
        else:
            inherited = os.pathsep.join(['powershell-7/Modules', 'windows-powershell/Modules'])
        real_run = subprocess.run
        launches = []

        class Launched(Exception):
            pass

        def launch(command, **options):
            probe = None
            if native:
                probe = real_run(
                    [command[0], '-NoProfile', '-NonInteractive', '-Command',
                     'Get-Command Get-AuthenticodeSignature -ErrorAction Stop | Out-Null; '
                     "'signature-cmdlet=resolved'"],
                    env=options.get('env'), capture_output=True, text=True, timeout=60)
            launches.append((options, probe))
            raise Launched

        def sibling(name):
            # As candidate_identity.support(): loaded from its file, not sys.path.
            spec = importlib.util.spec_from_file_location(name, scripts / (name + '.py'))
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            return module

        identity = SimpleNamespace(__file__=str(scripts / 'candidate_identity.py'),
                                   IdentityError=RuntimeError, support=sibling)
        audit = scripts.parent / '.rmux-audit/conpty-acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory, \
                patch.dict(os.environ, {'PSModulePath': inherited}), \
                patch.object(runtime.shutil, 'which',
                             return_value=windows_powershell or 'powershell.exe'), \
                patch.object(runtime.subprocess, 'run', side_effect=launch):
            for name in ('RMUX_CONPTY_CONFIGURATION', 'RMUX_CONPTY_NO_PASSTHROUGH',
                         'RMUX_CONPTY_PACKAGE'):
                os.environ.pop(name, None)
            with self.assertRaises(Launched):
                runtime.initialize(identity, Path(directory).resolve())
        self.assertEqual(len(launches), 1)
        options, probe = launches[0]
        if probe is not None:
            self.assertEqual(probe.returncode, 0, probe.stdout + probe.stderr)
            self.assertIn('signature-cmdlet=resolved', probe.stdout)
        environment = options.get('env')
        self.assertIsNotNone(environment, 'the verifier inherited the caller module path')
        self.assertNotIn('PSMODULEPATH', {name.upper() for name in environment})


    def test_identity_guard_loads_the_runtime_without_scripts_on_sys_path(self):
        # The guard loads support modules from their files. Under python -I,
        # as the final-sink identity child runs it, neither the scripts
        # directory nor the working directory is on sys.path.
        scripts = Path(__file__).resolve().parent
        code = ('import importlib.util, sys\n'
                'spec = importlib.util.spec_from_file_location("candidate_identity", sys.argv[1])\n'
                'guard = importlib.util.module_from_spec(spec)\n'
                'spec.loader.exec_module(guard)\n'
                'guard.support("candidate_conpty_runtime")\n'
                'environment = guard.support("windows_powershell").windows_powershell_environment()\n'
                'print("loaded", "PSMODULEPATH" in {name.upper() for name in environment})\n')
        result = subprocess.run([sys.executable, '-I', '-c', code, str(scripts / 'candidate_identity.py')],
                                cwd=scripts, env=dict(os.environ, PSModulePath='powershell-7/Modules'),
                                capture_output=True, text=True, timeout=120)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), 'loaded False')


if __name__ == '__main__':
    unittest.main()
