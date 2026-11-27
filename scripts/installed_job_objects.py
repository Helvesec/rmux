"""Three native Job Object cells from an installed release package; no build fixtures."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import zipfile

# release_evidence imports its release siblings by bare name.
sys.path.insert(0, str(Path(__file__).resolve().parent / "release"))
from release_evidence import file_hash  # noqa: E402
from windows_powershell import windows_powershell_environment

CELLS = ('daemon-survives-launcher', 'pane-descendant-cleanup', 'parent-denies-breakaway')


def job_object_cells():
    return CELLS


def require_package(metadata, head, authority, release_ref, provenance=None, archive_sha256=None):
    if not re.fullmatch('[0-9a-f]{40}', head):
        raise ValueError('full source SHA required')
    if (metadata.get('git_commit') != head or metadata.get('artifact_kind') != 'windows-package-binary'
            or metadata.get('configuration') != 'release' or metadata.get('release_artifact') is not True):
        raise ValueError('installed release package identity differs from HEAD')
    if authority == 'rc':
        version = re.escape(metadata.get('rmux_version', ''))
        if not version:
            raise ValueError('RC authority requires a package version')
        if not re.fullmatch('v' + version + r'-rc\.[1-9][0-9]*', release_ref):
            raise ValueError('RC authority requires the matching release ref')
        if (not provenance or provenance.get('source_sha') != head
                or provenance.get('release_ref') != release_ref
                or provenance.get('archive_sha256') != archive_sha256 or not archive_sha256):
            raise ValueError('RC authority requires verified hosted provenance bound to SHA/digest')
    elif authority != 'signed-local':
        raise ValueError('package authority must be signed-local or rc')


def require_embedded_head(binary, head):
    from candidate_identity import MARKER
    identities = set(MARKER.findall(Path(binary).read_bytes()))
    if not identities or any(commit.decode() != head or platform != b'windows'
                             for commit, _, platform in identities):
        raise ValueError('installed binary embedded commit/platform differs from HEAD')


def install(archive, destination):
    """Retain a private portable installation of the already verified archive."""
    with zipfile.ZipFile(archive) as package:
        entries = package.infolist()
        roots = {entry.filename.split('/')[0] for entry in entries}
        if len(roots) != 1:
            raise ValueError('package must have one portable root')
        root = next(iter(roots))
        for entry in entries:
            path = Path(entry.filename)
            if (path.is_absolute() or '..' in path.parts or '\\' in entry.filename
                    or ':' in entry.filename or (entry.external_attr >> 16) & 0o170000 == 0o120000):
                raise ValueError('unsafe package archive entry')
            target = destination.joinpath(*path.parts)
            if entry.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open('xb') as output:
                    output.write(package.read(entry))
    return destination / root


def validate_cells(records):
    if [record.get('id') for record in records] != list(CELLS):
        raise ValueError('all three installed Job Object cells must be present once')
    if any(record.get('status') != 'pass' for record in records):
        raise ValueError('a missing or failing native cell cannot become PASS')


def collect(plan_path):
    if sys.platform != 'win32':
        raise ValueError('Job Object product proof requires native Windows')
    from installed_job_objects_windows import Native, Job
    if not Path(plan_path).is_absolute():
        raise ValueError('absolute frozen plan path required')
    plan = json.loads(Path(plan_path).read_text(encoding='utf-8'))
    head = plan['sha']
    from candidate_identity import source_identity, git
    source = Path(__file__).resolve().parents[1]
    source_identity(source, head)
    archive, checksums, output = (Path(plan[name]) for name in ('archive', 'checksums', 'output'))
    powershell, python = (Path(plan[name]) for name in ('powershell', 'python'))
    if any(not path.is_absolute() for path in (archive, checksums, output, powershell, python)):
        raise ValueError('absolute package, output and tool paths required')
    output.mkdir(exist_ok=False)
    receipt = {'schema': 1, 'status': 'incomplete', 'source_sha': head, 'os': 'windows',
        'source_tree': git(source, 'rev-parse', 'HEAD^{tree}'),
        'daemon_policy_sha256': file_hash(source / 'crates/rmux-os/src/daemon.rs'),
        'cells': [], 'archive': str(archive), 'archive_sha256': file_hash(archive),
        'authority': plan['authority'], 'release_ref': plan.get('release_ref', ''),
        'plan_sha256': file_hash(Path(plan_path)), 'checksums_sha256': file_hash(checksums),
        'shared_cells': {'L19.2': 'daemon-survives-launcher'},
        'limits': ['Windows 10 native proof requires separate authority']}
    if (receipt['archive_sha256'] != plan['archive_sha256']
            or receipt['checksums_sha256'] != plan['checksums_sha256']):
        raise ValueError('package inputs differ from the frozen plan')
    native = Native()
    daemon, descendants = None, []
    daemon_owned = False
    cli_qualified, launch_attempted = False, False
    cli, env = None, os.environ.copy()
    label = 'job-object-' + str(os.getpid()) + '-' + str(time.time_ns())
    for name in ('RMUX_INTERNAL_BINARY_PATH', 'RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE',
                 'RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB', 'TMUX', 'TMUX_PANE'):
        env.pop(name, None)

    def save():
        (output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')

    def invoke(args, success=True):
        command = [str(cli), '-L', label, *args]
        result = subprocess.run(command, env=env, cwd=output, capture_output=True, text=True, timeout=20)
        if success and result.returncode:
            raise ValueError(f'installed CLI failed: {args}: {result.stderr}')
        return result

    def launcher(cell, allow, args):
        nonlocal launch_attempted
        job = Job(native, allow)
        record = {'id': cell, 'status': 'incomplete', 'launcher_attempts': 0}
        receipt['cells'].append(record)
        log_path = output / (cell + '.log')
        try:
            with log_path.open('wb') as log:
                launch_attempted = True
                record['launcher_attempts'] = 1
                code = job.launch([str(cli), '-L', label, *args], env, output, log)
            record.update(launcher_exit=code, log=str(log_path))
            return record, code
        finally:
            # This closes the launcher's containing kill-on-close job, proving
            # daemon survival separately from an ordinary successful CLI exit.
            job.close()
            save()

    save()
    try:
        if plan['authority'] == 'rc':
            from installed_job_objects_provenance import verify
            receipt['provenance'] = verify(plan, archive, checksums, output)
        verifier = Path(__file__).parent / 'verify-package-windows.ps1'
        command = [str(powershell), '-NoProfile', '-File', str(verifier), '-Archive', str(archive),
            '-Checksums', str(checksums), '-RequireReleaseArtifact', '-ExpectedGitSha', head]
        with (output / 'package-verification.log').open('wb') as log:
            subprocess.run(command, stdout=log, stderr=log, check=True, timeout=180,
                           env=windows_powershell_environment())
        root = install(archive, output / 'install')
        metadata = json.loads((root / 'share/rmux/artifact-metadata.json').read_text(encoding='utf-8-sig'))
        require_package(metadata, head, plan['authority'], plan.get('release_ref', ''),
                        receipt.get('provenance'), receipt['archive_sha256'])
        cli = root / 'rmux.exe'
        binaries = [(cli, 'binary_sha256'), (root / 'libexec/rmux/rmux.exe', 'helper_binary_sha256'),
                    (root / 'rmux-daemon.exe', 'daemon_binary_sha256')]
        for binary, field in binaries:
            require_embedded_head(binary, head)
            if file_hash(binary) != metadata[field]:
                raise ValueError('installed package binary hash differs from metadata')
            if plan['authority'] == 'signed-local':
                signature = output / 'verify-signature.ps1'
                signature.write_text('param([string]$Binary)\n'
                    '$ErrorActionPreference = "Stop"\n'
                    'if((Get-AuthenticodeSignature -LiteralPath $Binary).Status -ne "Valid"){exit 1}\n')
                subprocess.run([str(powershell), '-NoProfile', '-File', str(signature),
                    '-Binary', str(binary)], check=True, timeout=30,
                    env=windows_powershell_environment())
        receipt['binaries'] = [{'path': str(binary), 'sha256': file_hash(binary)} for binary, _ in binaries]
        cli_qualified = True
        record, code = launcher(CELLS[0], True,
            ['new-session', '-d', '-s', 'survival', 'cmd.exe /d /q /k'])
        if code:
            raise ValueError('breakaway-enabled launcher failed')
        invoke(['has-session', '-t', 'survival'])
        pid = int(invoke(['display-message', '-p', '#{pid}']).stdout.strip())
        daemon = native.process(pid)
        daemon_image = Path(daemon.image())
        if daemon_image.resolve() != binaries[2][0].resolve() or file_hash(daemon_image) != metadata['daemon_binary_sha256']:
            raise ValueError('live daemon is not the installed package daemon')
        daemon_owned = True
        if daemon.exited():
            raise ValueError('daemon died with the launcher job')
        record.update(status='pass', daemon_pid=pid, daemon_image=str(daemon_image))
        invoke(['set-option', '-s', 'exit-empty', 'off'])
        pid_file = output / 'descendants.json'
        code = ('import json,os,subprocess,sys,time; from pathlib import Path; '
            'child=subprocess.Popen([sys.executable,"-c","import time; time.sleep(120)"]); '
            f'Path({str(pid_file)!r}).write_text(json.dumps([os.getpid(),child.pid])); time.sleep(120)')
        invoke(['new-session', '-d', '-s', 'descendants', str(python), '-c', code])
        deadline = time.monotonic() + 20
        while not pid_file.exists():
            if time.monotonic() >= deadline:
                raise ValueError('pane descendant witness did not start')
            time.sleep(.02)
        pids = json.loads(pid_file.read_text())
        descendants = [native.process(pid) for pid in pids]
        if any(process.exited() for process in descendants):
            raise ValueError('descendants were not live before pane teardown')
        invoke(['kill-pane', '-t', 'descendants:0.0'])
        if any(not process.exited(5) for process in descendants):
            raise ValueError('a descendant survived its pane')
        receipt['cells'].append({'id': CELLS[1], 'status': 'pass', 'pids': pids,
            'observation': 'retained process handles signaled after kill-pane'})
        invoke(['kill-server'])
        if not daemon.exited(5):
            raise ValueError('owned installed daemon did not stop')
        daemon.close()
        daemon = None
        record, code = launcher(CELLS[2], False,
            ['new-session', '-d', '-s', 'denied', 'cmd.exe /d /q /k'])
        log = Path(record['log']).read_text(errors='replace')
        probe = invoke(['has-session', '-t', 'denied'], success=False)
        # has-session does not auto-start a server. A fallback daemon would be
        # reachable here. The exact policy diagnostic binds the failed launch.
        if code == 0 or probe.returncode == 0 or 'will not retry inside the caller' not in log:
            raise ValueError('non-breakaway parent did not fail without fallback')
        record.update(status='pass', probe_exit=probe.returncode,
            daemon_attempt_proof='single spawn call in rmux-os/daemon.rs at source_sha; native refusal diagnostic',
            diagnostic=log)
        if (file_hash(archive) != receipt['archive_sha256']
                or file_hash(checksums) != receipt['checksums_sha256']
                or file_hash(Path(plan_path)) != receipt['plan_sha256']
                or any(file_hash(Path(binary['path'])) != binary['sha256'] for binary in receipt['binaries'])):
            raise ValueError('installed package inputs changed during native proof')
        source_identity(source, head)
        if plan['authority'] == 'rc':
            from installed_job_objects_provenance import unchanged
            unchanged(receipt['provenance'])
        validate_cells(receipt['cells'])
        receipt['status'] = 'pass'
    except BaseException as error:
        receipt.update(status='fail' if launch_attempted else 'not-played', error=str(error))
        raise
    finally:
        try:
            if cli_qualified and launch_attempted:
                cleanup = invoke(['kill-server'], success=False)
                receipt['cleanup_exit'] = cleanup.returncode
        except BaseException as error:
            receipt.update(status='fail', cleanup_error=str(error))
            raise
        finally:
            try:
                for process in descendants:
                    if not process.exited(5):
                        receipt.update(status='fail', forced_descendant_cleanup=True)
                        process.terminate()
                if daemon is not None and daemon_owned and not daemon.exited(5):
                    receipt.update(status='fail', forced_daemon_cleanup=True)
                    daemon.terminate()
            finally:
                for process in descendants:
                    process.close()
                if daemon is not None:
                    daemon.close()
                save()
    if receipt['status'] != 'pass':
        raise ValueError('installed Job Object cleanup failed')
    return receipt
