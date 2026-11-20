#!/usr/bin/env python3
"""Finite TAR installer proof: rollback and replace an owned CLI symlink.

The default mode uses inert fixtures only. --native FROZEN_PLAN opts into one
S1/S2/S3 lot on Linux, with the accepted archive's unchanged embedded installer.
S2 injects after replacement of the manifest's CLI entry, before verification.
No test imports the unadopted prior TAR transaction driver or executes the
private external canary. All expected file states precede installer execution.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from unittest import mock

SOURCE = Path(__file__).resolve().parents[1]
PACKAGE = 'rmux-0.10.0-linux-x86_64'
TARGET_DATA = b'L1-022 inert outside target; never execute.\n'
FOREIGN_DATA = b'L1-022 foreign prefix canary.\n'
LINK_TEXT = '../../outside-canary.dat'
MAX_LOG = 8 * 1024 * 1024


def check(condition, reason):
    if not condition:
        raise ValueError(reason)


def utc():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def save(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write('\n')


def sha(path):
    # A comparison of a regular file must never accidentally follow a link.
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        check(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), 'hash input is not regular')
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def state(path):
    info = path.lstat()
    result = {'mode': oct(stat.S_IMODE(info.st_mode))}
    if stat.S_ISLNK(info.st_mode):
        return dict(result, type='symlink', target=os.readlink(path))
    if stat.S_ISDIR(info.st_mode):
        return dict(result, type='directory')
    check(stat.S_ISREG(info.st_mode), 'unsupported node type')
    return dict(result, type='file', bytes=info.st_size, sha256=sha(path))


def snapshot(prefix):
    check(prefix.is_dir() and not prefix.is_symlink(), 'prefix is not a real directory')
    result = {'.': state(prefix)}
    for parent, directories, files in os.walk(prefix, followlinks=False):
        for name in sorted(directories + files):
            p = Path(parent) / name
            result[p.relative_to(prefix).as_posix()] = state(p)
    return result


def same(actual, expected):
    changed = sorted(k for k in actual.keys() | expected.keys() if actual.get(k) != expected.get(k))
    check(not changed, 'state mismatch: ' + ', '.join(changed))


def path_name(name):
    p = PurePosixPath(name)
    check(name and name != '.' and str(p) == name and not p.is_absolute()
          and '..' not in p.parts and '\\' not in name
          and not any(ord(c) < 32 for c in name), 'noncanonical archive path')
    return p


def archive_inventory(subject):
    path = Path(subject['path'])
    check(path.stat().st_size == subject['bytes'] and sha(path) == subject['sha256'], 'archive identity')
    rows, seen, expanded = [], set(), 0
    with tarfile.open(path, 'r:gz') as archive:
        for member in archive:
            name = path_name(member.name)
            check(name.parts[0] == PACKAGE and member.name not in seen, 'archive root/duplicate')
            check(member.isdir() or member.isfile(), 'archive link/special member')
            check(member.mode in ((0o755,) if member.isdir() else (0o644, 0o755)), 'archive mode')
            check(0 <= member.size < 21000000, 'member size')
            seen.add(member.name)
            row = dict(path=member.name, type='directory' if member.isdir() else 'file',
                       mode=oct(member.mode), bytes=member.size, mtime=member.mtime)
            if member.isfile():
                with archive.extractfile(member) as stream:
                    row['sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
                expanded += member.size
            rows.append(row)
    check(len(rows) == 34 and expanded == 40776035, 'bounded archive shape')
    check(rows == subject['members'], 'archive inventory changed')
    return rows


def projection(subject, descriptor):
    """Map accepted descriptor to archive hashes, independently of installed files."""
    rows = [line.split('\t') for line in subject['texts']['share/rmux/package-layout.tsv'].splitlines()]
    check(all(len(row) == 6 for row in rows), 'manifest columns')
    declared = {}
    for entry in descriptor['entries']:
        variant = entry['variants'].get('unix-archive')
        if variant is not None:
            declared[variant['path']] = [variant['path'], variant['mode'], variant.get('install', '-'),
                'optional' if variant.get('install_optional') else 'required', entry['id'], entry['role']]
    check(len(rows) == len(declared) and {row[0]: row for row in rows} == declared,
          'descriptor/manifest disagreement')
    files = {row['path'][len(PACKAGE) + 1:]: row for row in subject['members'] if row['type'] == 'file'}
    check(set(files) == set(declared), 'descriptor/archive file set')
    result, cli = {}, None
    for source, mode, dest, required, ident, role in rows:
        path_name(source)
        check(int(mode, 8) == int(files[source]['mode'], 8), 'manifest/archive mode')
        if dest == '-':
            continue
        path_name(dest)
        check(dest.lower() not in {p.lower() for p in result}, 'duplicate install destination')
        result[dest] = {k: files[source][k] for k in ('type', 'mode', 'bytes', 'sha256')}
        if source == 'bin/rmux':
            check(dest == 'bin/rmux' and role == 'executable' and re.fullmatch('[a-z][a-z0-9-]*', ident), 'CLI entry')
            cli = ident
    check(cli is not None, 'missing CLI entry')
    return result, 'after-replace-' + cli


def expected_tree(files, foreign):
    result = dict(foreign)
    for name, row in files.items():
        check(name not in result, 'foreign canary overlaps installation')
        result[name] = row
        for parent in PurePosixPath(name).parents:
            result.setdefault(str(parent), dict(type='directory', mode='0o755'))
    return result


def extract(subject, destination):
    archive_inventory(subject)  # Validate every member before any extracted file.
    destination.mkdir(mode=0o755)
    with tarfile.open(subject['path'], 'r:gz') as archive:
        for member in archive:
            p = destination / member.name
            if member.isdir():
                p.mkdir(mode=member.mode)
            else:
                with archive.extractfile(member) as src, p.open('xb') as dst:
                    while block := src.read(1024 * 1024):
                        dst.write(block)
            p.chmod(member.mode)
    expected = {'.': dict(type='directory', mode='0o755')}
    for row in subject['members']:
        expected[row['path']] = {k: row[k] for k in ('type', 'mode')}
        if row['type'] == 'file':
            expected[row['path']].update(bytes=row['bytes'], sha256=row['sha256'])
    same(snapshot(destination), expected)
    return destination / PACKAGE


# Save only this fixture's temporary integrated-help streams, then exec real rm
# with unchanged arguments. No product binary or installer is wrapped/modified.
RM_OBSERVER = '''#!/usr/bin/python3 -B
import os, pathlib, sys
for argument in sys.argv[1:]:
    p = pathlib.Path(argument)
    if p.parent == pathlib.Path(os.environ['TMPDIR']):
        for kind in ('stdout', 'stderr'):
            if p.name.startswith('rmux-install-' + kind + '.'):
                data = p.read_bytes()
                if len(data) > 1048576:
                    raise ValueError('help stream too large')
                out = pathlib.Path(os.environ['RMUX_SYMLINK_PROOF_STEP']) / ('help.' + kind)
                with out.open('xb') as stream:
                    stream.write(data)
os.execv('/usr/bin/rm', ['/usr/bin/rm', *sys.argv[1:]])
'''


def help_observation(trace, cli, expected_count):
    lines = trace.splitlines()
    hits = [i for i, line in enumerate(lines) if line.endswith(': ' + str(cli) + ' --help')]
    check(len(hits) == expected_count, 'integrated help count')
    if not hits:
        return {'count': 0, 'reason': 'injection precedes integrated verification'}
    i = hits[0]
    pattern = r'^\++([0-9]+\.[0-9]+) [0-9]+: (.*)$'
    before, after = re.match(pattern, lines[i]), re.match(pattern, lines[i + 1])
    check(before is not None and after is not None and after[2] in ('status=0', 'status=1'), 'real help rc/timing')
    return dict(count=1, argv=[str(cli), '--help'], returncode=int(after[2][-1]),
                start_epoch=float(before[1]), end_epoch=float(after[1]),
                seconds=float(after[1]) - float(before[1]))


def stop_owned_process(proc):
    if proc.poll() is None:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)


def run_installer(argv, environment, directory, step, seconds):
    start, tick = utc(), time.monotonic()
    timed_out, log_overflow = False, False
    with (step / 'stdout').open('xb') as out, (step / 'stderr').open('xb') as err, (step / 'trace').open('xb') as trace:
        environment = dict(environment, BASH_XTRACEFD=str(trace.fileno()))
        proc = subprocess.Popen(argv, env=environment, cwd=directory, stdout=out, stderr=err,
                                start_new_session=True, pass_fds=(trace.fileno(),))
        try:
            while proc.poll() is None:
                timed_out = time.monotonic() - tick >= seconds
                log_overflow = sum(p.stat().st_size for p in step.iterdir() if p.is_file()) > MAX_LOG // 3
                if timed_out or log_overflow:
                    stop_owned_process(proc)
                    break
                try:
                    proc.wait(timeout=0.05)
                except subprocess.TimeoutExpired:
                    pass
        except BaseException:
            stop_owned_process(proc)
            raise
    result = dict(argv=argv, start=start, end=utc(), seconds=time.monotonic() - tick,
                  returncode=proc.returncode, timed_out=timed_out, log_overflow=log_overflow)
    save(step / 'PROCESS.json', result)
    return result


def native(plan_path):
    check(sys.platform == 'linux', 'native Linux required')
    plan = json.loads(plan_path.read_text())
    fixture = Path(plan['fixture'])
    check(fixture.is_absolute() and fixture.resolve() == fixture and fixture.is_dir(), 'real private fixture')
    check(plan['prefix'] == str(fixture / 'prefix'), 'private prefix')
    for key in ('qualification', 'descriptor', 'resource_checker'):
        check(sha(plan[key]['path']) == plan[key]['sha256'], key + ' changed')
    check(sha(__file__) == plan['driver_sha256'], 'driver changed')
    check(subprocess.check_output(['git', '-C', str(SOURCE), 'rev-parse', 'HEAD'], text=True).strip() == plan['head'], 'candidate head')
    check(not subprocess.check_output(['git', '-C', str(SOURCE), 'status', '--porcelain']), 'dirty candidate')
    qualification = json.loads(Path(plan['qualification']['path']).read_text())
    for tool in qualification['tools'].values():
        check(sha(tool['path']) == tool['sha256'], 'tool changed: ' + tool['path'])
    subject = qualification['subject']
    files, checkpoint = projection(subject, json.loads(Path(plan['descriptor']['path']).read_text()))
    check(files == plan['expected_files'] and checkpoint == plan['checkpoint'], 'prospective oracle changed')
    check(checkpoint == qualification['checkpoint'], 'qualified checkpoint changed')
    # Exclusive start marker consumes the single lot, even on harness failure.
    save(fixture / 'NATIVE-STARTED.json', dict(start=utc(), plan_sha256=sha(plan_path)))
    start, tick = utc(), time.monotonic()
    result = dict(start=start, verdict='FAIL', installer_invocations=0, help_invocations=0, steps=[], plan_sha256=sha(plan_path))
    os.umask(0o022)
    def capacity(stage):
        subprocess.run([sys.executable, '-B', plan['resource_checker']['path'], stage],
                       check=True, timeout=20, stdout=subprocess.DEVNULL)
    try:
        capacity('BEFORE-EXTRACTION')
        package = extract(subject, fixture / 'extracted')
        check(sha(package / 'install.sh') == plan['installer_sha256'], 'embedded installer changed')
        capacity('BEFORE-FIXTURE')
        prefix, target = fixture / 'prefix', fixture / 'outside-canary.dat'
        prefix.mkdir(mode=0o755)
        (prefix / 'foreign.keep').write_bytes(FOREIGN_DATA)
        (prefix / 'foreign.keep').chmod(0o640)
        with target.open('xb') as stream:
            stream.write(TARGET_DATA)
        target.chmod(0o600)
        target_expected = state(target)
        check(target_expected['mode'] == '0o600', 'external target must be nonexecutable')
        same(target_expected, plan['expected_target'])
        foreign = snapshot(prefix)
        same(foreign, plan['expected_foreign'])
        expected = expected_tree(files, foreign)
        expected_link = dict(expected, **{'bin/rmux': dict(type='symlink', mode='0o777', target=LINK_TEXT)})
        save(fixture / 'EXPECTED.json', dict(regular=expected, symlink=expected_link, outside_target=target_expected))
        for name in ('home', 'tmp', 'xdg-config', 'xdg-cache', 'xdg-data', 'xdg-runtime', 'observer'):
            (fixture / name).mkdir(mode=0o700)
        observer = fixture / 'observer/rm'
        observer.write_text(RM_OBSERVER)
        observer.chmod(0o755)
        env = dict(PATH=str(observer.parent) + ':/usr/bin:/bin', HOME=str(fixture / 'home'),
                   TMPDIR=str(fixture / 'tmp'), XDG_CONFIG_HOME=str(fixture / 'xdg-config'),
                   XDG_CACHE_HOME=str(fixture / 'xdg-cache'), XDG_DATA_HOME=str(fixture / 'xdg-data'),
                   XDG_RUNTIME_DIR=str(fixture / 'xdg-runtime'), LC_ALL='C', TZ='UTC',
                   RMUX_TINY_TRACE='1', PS4='+${EPOCHREALTIME} ${LINENO}: ')
        for tag in ('S1', 'S2', 'S3'):
            capacity('BEFORE-' + tag)
            check(time.monotonic() - tick < 300, 'execution budget exhausted')
            step = fixture / tag
            step.mkdir()
            if tag == 'S2':
                before_setup = snapshot(prefix)
                same(before_setup, expected)
                save(step / 'BEFORE-SYMLINK-SETUP.json', before_setup)
                (prefix / 'bin/rmux').unlink()  # The sole, job-owned setup mutation.
                (prefix / 'bin/rmux').symlink_to(LINK_TEXT)
                check((prefix / 'bin/rmux').parent.joinpath(LINK_TEXT).resolve() == target, 'canary target confinement')
                same(snapshot(prefix), expected_link)
            before = dict(prefix=snapshot(prefix), outside_target=state(target))
            same(before['outside_target'], target_expected)
            save(step / 'BEFORE.json', before)
            if tag == 'S3':
                same(before['prefix'], expected_link)
            step_env = dict(env, RMUX_SYMLINK_PROOF_STEP=str(step))
            if tag == 'S2':
                step_env['RMUX_INSTALL_TEST_FAIL_AT'] = checkpoint
            argv = ['/usr/bin/bash', '-x', str(package / 'install.sh'), '--prefix', str(prefix)]
            result['installer_invocations'] += 1
            process = run_installer(argv, step_env, package, step, min(120, 300 - (time.monotonic() - tick)))
            result['steps'].append(dict(name=tag, process=process))
            after = dict(prefix=snapshot(prefix), outside_target=state(target))
            save(step / 'AFTER.json', after)
            check(not process['timed_out'] and not process['log_overflow'], 'installer limit exceeded')
            check(process['returncode'] == (1 if tag == 'S2' else 0), 'unexpected installer return code')
            trace = (step / 'trace').read_text()
            help_count = 0 if tag == 'S2' else 1
            smoke = help_observation(trace, prefix / 'bin/rmux', help_count)
            result['help_invocations'] += smoke['count']
            if help_count:
                check(b'usage: rmux' in (step / 'help.stdout').read_bytes(), 'real helper usage missing')
                check(b'rmux tiny: fallback: unsupported invocation' in (step / 'help.stderr').read_bytes(), 'tiny fallback missing')
                smoke.update(stdout_sha256=sha(step / 'help.stdout'), stderr_sha256=sha(step / 'help.stderr'), tiny_fallback=True)
            else:
                check(not (step / 'help.stdout').exists() and not (step / 'help.stderr').exists(), 'unexpected S2 help output')
                check((step / 'stderr').read_text() == 'error: injected installer failure at ' + checkpoint + '\n', 'wrong injected failure')
                check(': rollback_transaction\n' in trace, 'rollback not observed')
                before_failure = trace.split(': checkpoint ' + checkpoint + '\n', 1)
                check(len(before_failure) == 2, 'actual checkpoint absent')
                publications = [line for line in before_failure[0].splitlines() if ': mv -f ' in line and '.rmux-stage-' in line]
                check(any(line.endswith(' ' + str(prefix / 'bin/rmux')) for line in publications), 'CLI was not really replaced before failure')
                save(step / 'PUBLICATION.json', dict(mv_lines=publications, checkpoint=checkpoint))
                same(after, before)
            save(step / 'HELP.json', smoke)
            result['steps'][-1]['help'] = smoke
            same(after['outside_target'], target_expected)
            same(after['prefix'], expected_link if tag == 'S2' else expected)
            for name in ('home', 'tmp', 'xdg-config', 'xdg-cache', 'xdg-data', 'xdg-runtime'):
                check(not any((fixture / name).iterdir()), 'private environment residue: ' + name)
            check(not any(part.startswith('.rmux-') for name in after['prefix'] for part in PurePosixPath(name).parts), 'transaction residue')
        archive_inventory(subject)
        check(result['installer_invocations'] == 3 and result['help_invocations'] == 2, 'invocation accounting')
        check(time.monotonic() - tick < 360, 'native total budget')
        result['verdict'] = 'PASS'
    except Exception as exc:
        result['error'] = type(exc).__name__ + ': ' + str(exc)
        raise
    finally:
        result.update(end=utc(), seconds=time.monotonic() - tick)
        save(fixture / 'NATIVE-RESULT.json', result)


class OwnedSymlinkOracles(unittest.TestCase):
    def test_symlink_state_never_hashes_target(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / 'target').write_bytes(b'outside')
            link = root / 'link'
            link.symlink_to('target')
            with mock.patch(__name__ + '.sha', side_effect=AssertionError('followed target')):
                self.assertEqual(state(link), dict(type='symlink', mode='0o777', target='target'))
            with self.assertRaises(OSError):
                sha(link)
            link.unlink()
            link.symlink_to('missing')
            self.assertEqual(state(link)['target'], 'missing')

    def test_target_changes_are_separate_from_link_state(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            prefix = root / 'prefix'
            prefix.mkdir()
            target = root / 'outside'
            target.write_bytes(b'keep')
            target.chmod(0o600)
            (prefix / 'managed').symlink_to('../outside')
            before, external = snapshot(prefix), state(target)
            target.write_bytes(b'lost')
            same(snapshot(prefix), before)
            with self.assertRaises(ValueError): same(state(target), external)
            target.write_bytes(b'keep')
            target.chmod(0o644)
            with self.assertRaises(ValueError): same(state(target), external)

    def test_wrong_link_or_regular_replacement_fails_rollback_oracle(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'managed'
            p.symlink_to('../../outside-canary.dat')
            expected = state(p)
            p.unlink()
            p.symlink_to('different')
            with self.assertRaises(ValueError): same(state(p), expected)
            p.unlink()
            p.write_bytes(TARGET_DATA)
            with self.assertRaises(ValueError): same(state(p), expected)

    def test_snapshot_detects_foreign_bytes_mode_and_transaction_residue(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            p = root / 'foreign'
            p.write_bytes(b'keep')
            p.chmod(0o640)
            baseline = snapshot(root)
            p.write_bytes(b'lost')
            with self.assertRaises(ValueError): same(snapshot(root), baseline)
            p.write_bytes(b'keep')
            p.chmod(0o600)
            with self.assertRaises(ValueError): same(snapshot(root), baseline)
            p.chmod(0o640)
            (root / '.rmux-install.lock').mkdir()
            with self.assertRaises(ValueError): same(snapshot(root), baseline)

    def test_checkpoint_comes_from_manifest_entry_id(self):
        descriptor = {'entries': [dict(id='public-cli', role='executable', variants={'unix-archive': dict(path='bin/rmux', mode='0755', install='bin/rmux')})]}
        manifest = 'bin/rmux\t0755\tbin/rmux\trequired\tpublic-cli\texecutable\n'
        subject = dict(texts={'share/rmux/package-layout.tsv': manifest}, members=[dict(path=PACKAGE + '/bin/rmux', type='file', mode='0o755', bytes=4, sha256='a' * 64)])
        self.assertEqual(projection(subject, descriptor)[1], 'after-replace-public-cli')
        for bad in ('', manifest * 2, manifest.replace('public-cli', 'tiny'), manifest.replace('0755', '0644')):
            subject['texts']['share/rmux/package-layout.tsv'] = bad
            with self.subTest(bad=bad), self.assertRaises(ValueError): projection(subject, descriptor)
        for bad in ('/escape', '../escape', 'a/../b', 'a//b', 'a\\b', 'a\nb'):
            with self.subTest(bad=bad), self.assertRaises(ValueError): path_name(bad)

    def test_help_capture_rc_forwarding_and_zero_call_oracle(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            proof = root / 'proof'
            proof.mkdir()
            observer = root / 'rm'
            observer.write_text(RM_OBSERVER)
            observer.chmod(0o755)
            stream = root / 'rmux-install-stdout.test'
            stream.write_bytes(b'actual help\n')
            env = dict(PATH='/usr/bin:/bin', TMPDIR=str(root), RMUX_SYMLINK_PROOF_STEP=str(proof))
            run = subprocess.run([str(observer), '-f', str(stream)], env=env, capture_output=True)
            self.assertEqual(run.returncode, 0)
            self.assertFalse(stream.exists())
            self.assertEqual((proof / 'help.stdout').read_bytes(), b'actual help\n')
            run = subprocess.run([str(observer), str(root / 'absent')], env=env, capture_output=True)
            self.assertEqual(run.returncode, 1)
        trace = '+1.000000 73: /private/bin/rmux --help\n+1.100000 76: status=0\n'
        self.assertEqual(help_observation(trace, '/private/bin/rmux', 1)['returncode'], 0)
        self.assertEqual(help_observation('', '/private/bin/rmux', 0)['count'], 0)
        for bad, count in [(trace, 0), ('', 1), (trace * 2, 1), (trace.replace('status=0', 'status=127'), 1)]:
            with self.subTest(bad=bad), self.assertRaises(ValueError): help_observation(bad, '/private/bin/rmux', count)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', type=Path)
    args, rest = parser.parse_known_args()
    if args.native:
        check(not rest, 'unexpected native arguments')
        native(args.native)
    else:
        unittest.main(argv=[sys.argv[0], *rest])
