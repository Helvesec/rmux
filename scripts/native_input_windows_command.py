"""Prospective Windows recipe for the two public_api_extensibility commands.

The supplied capacity receipt must qualify a complete cold peak and reserve.
Collection alone does not execute Cargo or confer native admission.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import time

from native_input_collector import Snapshot, locked_packages, now, sha
from native_input_windows import (files_before_use, observed_bootstrap,
                                 new_snapshot, profile_bindings, read_manifest,
                                 verify_manifest, write_once)
from native_input_windows_process import current, install
from native_input_windows_trace import assess, read_events


def capacity(source, budget):
    """A current free-byte reading or retained target size is never a peak."""
    free = shutil.disk_usage(source).free
    if (budget.get('source') != str(source) or budget.get('os') != 'windows'
            or budget.get('complete_cold_peak_qualified') is not True
            or budget.get('cell') not in ('sdk', 'proto')
            or not isinstance(budget.get('peak_bytes'), int) or budget['peak_bytes'] <= 0
            or not isinstance(budget.get('reserve_bytes'), int) or budget['reserve_bytes'] <= 0
            or not budget.get('measurement') or not budget.get('measurement_sha256')
            or not 0 <= time.time_ns() - budget.get('at_ns', 0) <= 300 * 10**9):
        raise RuntimeError('Windows complete cold peak/reserve lacks fresh qualification')
    measurement = Path(budget['measurement'])
    if sha(measurement) != budget['measurement_sha256']:
        raise RuntimeError('Windows peak measurement changed')
    required = 50 * 2**30 + budget['peak_bytes'] + budget['reserve_bytes']
    if free < required:
        raise RuntimeError('Windows floor plus measured peak and reserve unavailable')
    return required


def collect(options):
    source = options.source.absolute()
    audit = options.audit.absolute()
    if source.resolve(strict=True) != source or audit.resolve(strict=True) != audit:
        raise RuntimeError('redirected Windows source or audit root')
    # Bound small collection separately; do not borrow the unqualified Cargo peak.
    def floor():
        if shutil.disk_usage(source).free < 50 * 2**30 + options.reserve_bytes:
            raise RuntimeError('Windows input retention floor/reserve unavailable')
    snapshot = new_snapshot(audit, options.reserve_bytes)
    profile = Path(__file__).with_name('fixtures') / 'native_input_windows_profile.json'
    snapshot.retain(profile)
    observation = json.loads(profile.read_text())
    roots = dict(zip(('$SYSTEMROOT', '$GIT', '$TOOLCHAIN', '$PYTHON', '$MSVC'),
                     (Path(os.environ['SystemRoot']), options.git.parent.parent,
                      options.toolchain, Path(sys.executable).parent, options.msvc)))
    observed_bootstrap(snapshot, observation, profile_bindings(observation, roots))
    # Keep source Git metadata before its first consumer. An independent clone
    # avoids a linked .git pointer to another producer's mutable worktree.
    if not (source / '.git').is_dir():
        raise RuntimeError('Windows collector requires an independent source checkout')
    snapshot.tree(source / '.git')
    for p in (options.git, Path(sys.executable), Path(__file__),
              Path(__file__).with_name('native_input_collector.py'),
              Path(__file__).with_name('native_input_windows.py'),
              Path(__file__).with_name('native_input_windows_process.py'),
              Path(__file__).with_name('native_input_windows_trace.py'),
              options.toolchain / 'bin/cargo.exe', options.toolchain / 'bin/rustdoc.exe',
              Path(os.environ['SystemRoot']) / 'System32/logman.exe',
              Path(os.environ['SystemRoot']) / 'System32/taskkill.exe',
              Path(os.environ['SystemRoot']) / 'System32/tracerpt.exe'):
        snapshot.retain(p)
    def git(*args):
        for p in snapshot.files:
            snapshot.verify(p)
        return subprocess.check_output([str(options.git), '-C', str(source), *args], text=True,
                                       env=dict(os.environ, GIT_OPTIONAL_LOCKS='0')).strip()
    if git('rev-parse', 'HEAD') != options.head or git('status', '--porcelain'):
        raise RuntimeError('Windows source is not the clean pinned HEAD')
    for relative in git('ls-files', '-z').split('\0'):
        if relative:
            snapshot.retain(source / relative)
    # Preserve the existing native recipe's compiler, locked registry and linker
    # inputs. Only measured loader paths extend its dynamic input population.
    for root in (options.toolchain / 'bin', options.toolchain / 'lib'):
        snapshot.tree(root)
    for root in (Path(sys.executable).parent / 'Lib', Path(sys.executable).parent / 'DLLs'):
        for p in root.rglob('*'):
            if p.is_file() and not {'site-packages', '__pycache__', 'test', 'tests'} & set(p.parts):
                snapshot.retain(p)
    for directory in os.environ.get('LIB', '').split(';'):
        if directory:
            for p in Path(directory).glob('*.lib'):
                snapshot.retain(p)
    registry = options.cargo_home / 'registry'
    snapshot.tree(registry / 'index')
    for pkg in locked_packages(source / 'Cargo.lock'):
        if not pkg.get('source', '').startswith('registry+'):
            continue
        name = pkg['name'] + '-' + pkg['version']
        archives = list((registry / 'cache').glob('*/' + name + '.crate'))
        roots = list((registry / 'src').glob('*/' + name))
        if len(archives) != 1 or len(roots) != 1 or sha(archives[0]) != pkg['checksum']:
            raise RuntimeError('Windows offline package is absent, ambiguous or changed')
        snapshot.retain(archives[0])
        snapshot.tree(roots[0])
    versions = {}
    for name, args in (('python', [str(Path(sys.executable)), '--version']),
                       ('git', [str(options.git), '--version']),
                       ('cargo', [str(options.toolchain / 'bin/cargo.exe'), '-V']),
                       ('rustc', [str(options.toolchain / 'bin/rustc.exe'), '-vV'])):
        for p in snapshot.files:
            snapshot.verify(p)
        versions[name] = subprocess.check_output(args, text=True).strip()
    for p in snapshot.files:
        snapshot.verify(p)
    floor()
    argv = [str(options.toolchain / 'bin/cargo.exe'), 'test', '--locked', '--offline',
            '-p', 'rmux-' + options.cell, '--test', 'public_api_extensibility']
    record = dict(os='windows', cell=options.cell, case=secrets.token_hex(16),
                  at=now(), at_ns=time.time_ns(), source=str(source), head=options.head,
                  audit=str(audit), reserve_bytes=options.reserve_bytes,
                  aliases=snapshot.windows_aliases, files=snapshot.files,
                  versions=versions, argv=argv, command_qualified_before_execution=False,
                  collector_loader_reads_qualified=False)
    write_once(audit / 'before-inputs.json', record)
    return record


def run(record, options):
    budget = json.loads(options.budget.read_text())
    if budget.get('cell') != record['cell'] or budget.get('head') != record['head']:
        raise RuntimeError('Windows peak belongs to another command or candidate')
    capacity(options.source, budget)
    # The volume mapping is native and current, never copied from trace071.
    import ctypes
    api = ctypes.WinDLL('kernel32', use_last_error=True)
    api.QueryDosDeviceW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32]
    mappings = {}
    for drive in {Path(p).drive for p in record['files']}:
        buffer = ctypes.create_unicode_buffer(32768)
        if not api.QueryDosDeviceW(drive, buffer, len(buffer)):
            raise ctypes.WinError(ctypes.get_last_error())
        mappings[buffer.value] = drive
    audit = Path(record['audit'])
    manifest = audit / 'before-inputs.json'
    _, digest = read_manifest(manifest)
    env = dict(os.environ, RMUX_NATIVE_INPUT_MANIFEST=str(manifest),
               RMUX_NATIVE_INPUT_MANIFEST_SHA256=digest,
               GIT_OPTIONAL_LOCKS='0',
               RUSTC=str(options.toolchain / 'bin/rustc.exe'),
               RUSTDOC=str(options.toolchain / 'bin/rustdoc.exe'),
               RMUX_PYTHON=sys.executable, CARGO_HOME=str(options.cargo_home),
               CARGO_BUILD_JOBS='3', CARGO_NET_OFFLINE='true', CARGO_INCREMENTAL='0',
               CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0')
    for key in tuple(env):
        if key.startswith('RMUX_IDENTITY_') or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'):
            env.pop(key)
    # This opt-in affects only this native recipe and its inherited Python guards.
    for key in set(os.environ) - set(env):
        os.environ.pop(key)
    os.environ.update(env)
    install()
    logman = Path(os.environ['SystemRoot']) / 'System32/logman.exe'
    tracerpt = logman.with_name('tracerpt.exe')
    name = 'rmux-native-' + record['case']
    start = [str(logman), 'create', 'trace', name, '-o', str(audit / 'native.etl'),
             '-p', 'Microsoft-Windows-Kernel-Process', '0x50', '5', '-bs', '64',
             '-nb', '16', '64', '-f', 'bin', '-ets']
    subprocess.run(start, check=True, stdout=subprocess.DEVNULL)
    try:
        os.chdir(options.source)
        spec = importlib.util.spec_from_file_location('candidate_identity', options.source / 'scripts/candidate_identity.py')
        guard = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = guard
        spec.loader.exec_module(guard)
        env = guard.prepare()
        parent = current()
        with (audit / 'command.log').open('xb') as output:
            process = subprocess.Popen(record['argv'], env=env, stdout=output, stderr=output)
            child = process._native_child
            reason = 'complete'
            while process.poll() is None:
                if shutil.disk_usage(options.source).free < 50 * 2**30 + budget['reserve_bytes']:
                    reason = 'capacity boundary; native admission pending'
                    subprocess.run([str(logman.with_name('taskkill.exe')), '/PID',
                                    str(process.pid), '/T', '/F'], check=True,
                                   stdout=subprocess.DEVNULL)
                    break
                time.sleep(.1)
            result = process.wait()
        write_once(audit / 'result.json', dict(exit=result, root=child, parent=parent,
                                              reason=reason, argv=record['argv'],
                                              target=env['RMUX_IDENTITY_TARGET']))
    finally:
        subprocess.run([str(logman), 'stop', name, '-ets'], check=True, stdout=subprocess.DEVNULL)
    subprocess.run([str(tracerpt), str(audit / 'native.etl'), '-o', str(audit / 'native.xml'),
                    '-of', 'XML', '-y'], check=True, stdout=subprocess.DEVNULL)
    boundaries = [json.loads(p.read_text()) for p in audit.glob('boundary-*/before.json')]
    waits = [json.loads(p.read_text()) for p in audit.glob('boundary-*/wait.json')]
    root = dict(pid=child['pid'], birth_ns=child['birth_ns'], parent_birth_ns=parent['birth_ns'])
    assessment = assess(read_events(audit / 'native.xml'), root, files_before_use(record, boundaries),
                        waits, mappings, record['case'], record['head'])
    verify_manifest(manifest, digest)
    write_once(audit / 'after-last-wait.json', dict(at_ns=time.time_ns(), head=record['head'],
                                                  case=record['case'], static_inputs_unchanged=True))
    if reason != 'complete':
        assessment['status'] = 'PENDING'
        assessment['errors'].append(reason)
    write_once(audit / 'native-assessment.json', assessment)
    # Actual exit and qualification are separate: an exit0 cannot hide gaps.
    if assessment['status'] != 'PASS':
        raise RuntimeError('Windows command complete; native admission remains PENDING')
    return result


def main():
    if sys.platform != 'win32' or not sys.flags.isolated or not sys.flags.no_site:
        raise SystemExit('Windows collector requires Python -I -S -B before site consumers')
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'audit', 'git', 'toolchain', 'msvc', 'cargo-home'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--head', required=True)
    parser.add_argument('--cell', choices=('sdk', 'proto'), required=True)
    parser.add_argument('--reserve-bytes', type=int, required=True)
    parser.add_argument('--budget', type=Path)
    parser.add_argument('--run', action='store_true')
    options = parser.parse_args()
    if options.reserve_bytes <= 0 or (options.run and not options.budget):
        parser.error('positive collection reserve and a separate run budget are required')
    if options.run:
        budget = json.loads(options.budget.read_text())
        if budget.get('head') != options.head or budget.get('cell') != options.cell:
            raise RuntimeError('Windows budget selects another candidate or cell')
        capacity(options.source.absolute(), budget)
        if not os.environ.get('LIB'):
            raise RuntimeError('Windows native linker environment is unqualified')
    # Isolated startup excludes this directory from sys.path; add only our exact
    # pinned script directory, after Python site startup has been disabled.
    record = collect(options)
    if options.run:
        sys.exit(run(record, options))
    print('WINDOWS INPUTS RETAINED; COMMAND NOT EXECUTED', len(record['files']))
