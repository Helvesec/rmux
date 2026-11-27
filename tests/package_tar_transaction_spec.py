#!/usr/bin/env python3
"""Bounded old/new/rollback proof using two pinned, delivered TAR artifacts.

Default mode tests the proof's rejection paths with inert files. Explicit
--native PLAN performs exactly four installs in a fresh private fixture, using
unmodified embedded installers and their integrated --help verification. The
prospective plan supplies archive inventories, descriptor and resource checker
pins. No building, repacking, daemon execution or system installation occurs.
"""
import argparse
import copy
import datetime
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

ROOT = Path(__file__).resolve().parents[1]
ARCHIVE_ROOT = 'rmux-0.10.0-linux-x86_64'
MAX_LOG_BYTES = 8 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write_json(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write('\n')


def relative(name):
    p = PurePosixPath(name)
    require(bool(name) and not p.is_absolute() and '..' not in p.parts
            and str(p) == name and name != '.' and '\\' not in name
            and not any(ord(c) < 32 for c in name), 'noncanonical path')
    return p


def member_record(member):
    p = relative(member.name.rstrip('/') if member.isdir() else member.name)
    require(p.parts[0] == ARCHIVE_ROOT, 'unexpected archive root')
    require(member.isdir() or member.isfile(), 'links and special files refused')
    require(member.mode in ((0o755, 0o775) if member.isdir() else (0o644, 0o755)),
            'unexpected archive mode')
    require(0 <= member.size < 21000000, 'oversize member')
    return dict(path=member.name, type='directory' if member.isdir() else 'file',
                mode=oct(member.mode), bytes=member.size, mtime=member.mtime)


def inspect_archive(subject):
    path = Path(subject['path'])
    require(path.is_file() and not path.is_symlink(), 'archive must be regular')
    require(path.stat().st_size == subject['bytes'] and digest(path) == subject['sha256'],
            'archive pin mismatch')
    records, seen, total = [], set(), 0
    with tarfile.open(path, 'r:gz') as archive:
        for member in archive:
            row = member_record(member)
            require(row['path'] not in seen, 'duplicate member')
            seen.add(row['path'])
            if member.isfile():
                with archive.extractfile(member) as stream:
                    row['sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
                total += member.size
            records.append(row)
    require(total < 50000000 and len(records) == 34, 'archive budget or count')
    require(records == subject['members'], 'archive inventory changed')
    return records


def projection(subject, descriptor):
    """Independent descriptor mapping; never infer the oracle from installed state."""
    declared = {}
    for entry in descriptor['entries']:
        variant = entry['variants'].get('unix-archive')
        if variant is None:
            continue
        declared[variant['path']] = [variant['path'], variant['mode'],
            variant.get('install', '-'), 'optional' if variant.get('install_optional') else 'required',
            entry['id'], entry['role']]
    lines = subject['texts']['share/rmux/package-layout.tsv'].splitlines()
    rows = [line.split('\t') for line in lines]
    require(all(len(row) == 6 for row in rows), 'invalid manifest columns')
    actual = {row[0]: row for row in rows}
    require(len(actual) == len(rows) and actual == declared, 'manifest/descriptor disagreement')
    files = {m['path'][len(ARCHIVE_ROOT) + 1:]: m for m in subject['members'] if m['type'] == 'file'}
    require(set(files) == set(declared), 'manifest file set')
    result, destinations = {}, set()
    for source, mode, destination, required, entry, role in rows:
        relative(source)
        m = files[source]
        require(int(mode, 8) == int(m['mode'], 8), 'manifest mode disagreement')
        if destination == '-':
            continue
        relative(destination)
        require(destination.lower() not in destinations, 'duplicate destination')
        destinations.add(destination.lower())
        result[destination] = dict(type='file', mode=m['mode'], bytes=m['bytes'], sha256=m['sha256'])
    return result


def snapshot(root):
    result = {}
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in sorted(dirs + files):
            path = Path(directory) / name
            s = path.lstat()
            row = dict(mode=oct(stat.S_IMODE(s.st_mode)))
            if stat.S_ISLNK(s.st_mode):
                row.update(type='symlink', target=os.readlink(path))
            elif stat.S_ISREG(s.st_mode):
                row.update(type='file', bytes=s.st_size, sha256=digest(path))
            elif stat.S_ISDIR(s.st_mode):
                row.update(type='directory')
            else:
                raise ValueError('unexpected installed file type')
            result[path.relative_to(root).as_posix()] = row
    return result


def expected_snapshot(files, canaries):
    result = copy.deepcopy(canaries)
    for name, row in files.items():
        require(name not in result, 'canary overlaps manifest')
        result[name] = row
        for parent in PurePosixPath(name).parents:
            if str(parent) != '.':
                result.setdefault(str(parent), dict(type='directory', mode='0o755'))
    return result


def equal_state(actual, expected):
    changed = sorted(k for k in actual.keys() | expected.keys() if actual.get(k) != expected.get(k))
    require(not changed, 'state mismatch: ' + ', '.join(changed))


# This observer only saves the installer's temporary help streams before the
# real rm deletes them. Its exec preserves argv and rm's actual exit status.
# No CLI, helper, archive member or install destination is wrapped or altered.
OBSERVER = '''#!/usr/bin/python3 -B
import os, pathlib, sys
for name in sys.argv[1:]:
    p = pathlib.Path(name)
    if p.parent == pathlib.Path(os.environ['TMPDIR']):
        for kind in ('stdout', 'stderr'):
            if p.name.startswith('rmux-install-' + kind + '.'):
                data = p.read_bytes()
                if len(data) > 1048576:
                    raise ValueError('help output exceeds budget')
                target = pathlib.Path(os.environ['RMUX_PROOF_STEP']) / ('help.' + kind)
                with target.open('xb') as stream:
                    stream.write(data)
os.execv('/usr/bin/rm', ['/usr/bin/rm', *sys.argv[1:]])
'''


def smoke_trace(trace, cli):
    lines = trace.splitlines()
    matches = [i for i, line in enumerate(lines) if line.endswith(': ' + str(cli) + ' --help')]
    require(len(matches) == 1, 'expected exactly one integrated CLI invocation')
    i = matches[0]
    pattern = r'^\++([0-9]+\.[0-9]+) [0-9]+: (.*)$'
    before = re.match(pattern, lines[i])
    after = re.match(pattern, lines[i + 1])
    require(before is not None and after is not None, 'missing timestamped smoke trace')
    require(after[2] in ('status=0', 'status=1'), 'unexpected CLI return code')
    return dict(argv=[str(cli), '--help'], returncode=int(after[2][-1]),
                start_epoch=float(before[1]), end_epoch=float(after[1]),
                seconds=float(after[1]) - float(before[1]))


def extract(subject, directory):
    inspect_archive(subject)  # Complete validation before opening any output.
    directory.mkdir()
    with tarfile.open(subject['path'], 'r:gz') as archive:
        for member in archive:
            target = directory / member.name
            if member.isdir():
                target.mkdir(mode=member.mode)
            else:
                with archive.extractfile(member) as source, target.open('xb') as output:
                    while block := source.read(1024 * 1024):
                        output.write(block)
            target.chmod(member.mode)
    actual = snapshot(directory)
    expected = {}
    for row in subject['members']:
        expected[row['path']] = {k: v for k, v in row.items() if k in ('type', 'mode', 'bytes', 'sha256')}
        if row['type'] == 'directory':
            expected[row['path']].pop('bytes')
    equal_state(actual, expected)
    return directory / ARCHIVE_ROOT


def native(plan_path):
    require(sys.platform == 'linux', 'native Linux required')
    plan = json.loads(plan_path.read_text())
    fixture = Path(plan['fixture'])
    require(fixture.is_absolute() and fixture.resolve() == fixture and fixture.is_dir(), 'private fixture')
    require(plan['prefix'] == str(fixture / 'prefix'), 'single private prefix')
    for key in ('qualification', 'descriptor', 'resource_checker'):
        require(digest(plan[key]['path']) == plan[key]['sha256'], key + ' pin')
    require(digest(__file__) == plan['driver_sha256'], 'driver pin')
    require(subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
            == plan['candidate_head'], 'candidate head pin')
    require(not subprocess.check_output(['git', '-C', str(ROOT), 'status', '--porcelain']), 'dirty candidate')
    qualification = json.loads(Path(plan['qualification']['path']).read_text())
    for tool in list(qualification['tools'].values()) + plan['extra_tools']:
        require(digest(tool['path']) == tool['sha256'], 'native tool pin')
    subjects = qualification['subjects']
    descriptor = json.loads(Path(plan['descriptor']['path']).read_text())
    expected = {s['label']: projection(s, descriptor) for s in subjects}
    require([s['label'] for s in subjects] == ['old', 'new'], 'two qualified artifacts')
    delta = [k for k in expected['old'] if expected['old'][k] != expected['new'][k]]
    require(delta == ['share/rmux/artifact-metadata.json'], 'expected real metadata-only delta')
    require(not (fixture / 'NATIVE-RESULT.json').exists(), 'native lot already completed')
    (fixture / 'NATIVE-STARTED.json').open('x').write(json.dumps(dict(start=now(), plan_sha256=digest(plan_path))) + '\n')
    start, clock = now(), time.monotonic()
    result = dict(start=start, verdict='FAIL', steps=[], artifact_delta=delta,
                  installer_invocations=0, help_invocations=0, plan_sha256=digest(plan_path))
    os.umask(0o022)
    def capacity(stage):
        subprocess.run([sys.executable, '-B', plan['resource_checker']['path'], stage],
                       check=True, timeout=20, stdout=subprocess.DEVNULL)
    try:
        capacity('BEFORE-EXTRACTION')
        roots = {s['label']: extract(s, fixture / ('extracted-' + s['label'])) for s in subjects}
        require(all(digest(p / 'install.sh') == plan['installer_sha256'] for p in roots.values()), 'embedded installer pin')
        capacity('BEFORE-PREFIX')
        prefix = fixture / 'prefix'
        prefix.mkdir()
        (prefix / 'bin').mkdir()
        (prefix / 'foreign.txt').write_bytes(b'L1-021 foreign canary\n')
        (prefix / 'foreign.txt').chmod(0o600)
        (prefix / 'bin/foreign-tool').write_bytes(b'not a product executable\n')
        (prefix / 'bin/foreign-tool').chmod(0o640)
        (prefix / 'foreign-link').symlink_to('foreign.txt')
        canaries = snapshot(prefix)
        write_json(fixture / 'CANARIES.json', canaries)
        expected = {label: expected_snapshot(files, canaries) for label, files in expected.items()}
        for label, state in expected.items():
            write_json(fixture / ('EXPECTED-' + label + '.json'), state)
        for name in ('home', 'tmp', 'xdg-config', 'xdg-cache', 'xdg-data', 'xdg-runtime', 'observer'):
            (fixture / name).mkdir(mode=0o700)
        observer = fixture / 'observer/rm'
        observer.write_text(OBSERVER)
        observer.chmod(0o755)
        env = dict(PATH=str(observer.parent) + ':/usr/bin:/bin', HOME=str(fixture / 'home'),
                   TMPDIR=str(fixture / 'tmp'), XDG_CONFIG_HOME=str(fixture / 'xdg-config'),
                   XDG_CACHE_HOME=str(fixture / 'xdg-cache'), XDG_DATA_HOME=str(fixture / 'xdg-data'),
                   XDG_RUNTIME_DIR=str(fixture / 'xdg-runtime'), LC_ALL='C', TZ='UTC',
                   PS4='+${EPOCHREALTIME} ${LINENO}: ', RMUX_TINY_TRACE='1')
        t1 = None
        for name, label, failure in [('T1', 'old', False), ('T2', 'new', True),
                                      ('T3', 'new', False), ('T4', 'old', False)]:
            capacity('BEFORE-' + name)
            require(time.monotonic() - clock < 420, 'native execution window exhausted')
            step = fixture / name
            step.mkdir()
            before = snapshot(prefix)
            write_json(step / 'BEFORE.json', before)
            step_env = dict(env, RMUX_PROOF_STEP=str(step))
            if failure:
                step_env['RMUX_INSTALL_TEST_FAIL_AT'] = 'after-replace-asset'
            argv = ['/usr/bin/bash', '-x', str(roots[label] / 'install.sh'), '--prefix', str(prefix)]
            began, tick = now(), time.monotonic()
            with (step / 'stdout').open('xb') as out, (step / 'stderr').open('xb') as err, (step / 'trace').open('xb') as trace:
                step_env['BASH_XTRACEFD'] = str(trace.fileno())
                proc = subprocess.Popen(argv, env=step_env, cwd=roots[label], stdout=out, stderr=err,
                                        pass_fds=(trace.fileno(),), start_new_session=True)
                result['installer_invocations'] += 1
                timeout = min(120, 420 - (time.monotonic() - clock))
                timed_out = False
                try:
                    rc = proc.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    timed_out = True
                    os.killpg(proc.pid, signal.SIGTERM)
                    try:
                        rc = proc.wait(timeout=30)
                    except subprocess.TimeoutExpired:
                        os.killpg(proc.pid, signal.SIGKILL)
                        rc = proc.wait(timeout=5)
            after = snapshot(prefix)
            write_json(step / 'AFTER.json', after)
            receipt = dict(name=name, label=label, argv=argv, start=began, end=now(),
                           seconds=time.monotonic() - tick, returncode=rc, timed_out=timed_out)
            result['steps'].append(receipt)
            write_json(step / 'PROCESS.json', receipt)
            require(not timed_out and rc == (1 if failure else 0), 'unexpected installer exit')
            trace_text = (step / 'trace').read_text()
            smoke = smoke_trace(trace_text, prefix / 'bin/rmux')
            result['help_invocations'] += 1
            require(b'usage: rmux' in (step / 'help.stdout').read_bytes() + (step / 'help.stderr').read_bytes(), 'real CLI usage absent')
            require(b'rmux tiny: fallback: unsupported invocation' in (step / 'help.stderr').read_bytes(), 'tiny helper fallback not observed')
            smoke['tiny_fallback_observed'] = True
            smoke['stdout_sha256'], smoke['stderr_sha256'] = digest(step / 'help.stdout'), digest(step / 'help.stderr')
            write_json(step / 'SMOKE.json', smoke)
            receipt['smoke'] = smoke
            if failure:
                require('injected installer failure at after-replace-asset' in (step / 'stderr').read_text(), 'wrong failure point')
                require(': rollback_transaction\n' in trace_text, 'rollback not entered')
                partial = trace_text.split(': checkpoint after-replace-asset\n', 1)
                require(len(partial) == 2, 'asset replacement checkpoint absent')
                published = [line for line in partial[0].splitlines() if ': mv -f ' in line]
                for dest in ('libexec/rmux/rmux', 'bin/rmux-daemon', 'bin/rmux', 'share/man/man1/rmux.1'):
                    require(any('.rmux-stage-' in line and line.endswith(' ' + str(prefix / dest)) for line in published), 'missing real partial replacement: ' + dest)
                write_json(step / 'PARTIAL-PUBLICATION.json', dict(mv_trace_lines=published, limitation='These four replaced files have identical old/new bytes; failure occurs before changed metadata is published. T3/T4 test metadata byte replacement.'))
                equal_state(before, t1)
                equal_state(after, t1)
            else:
                equal_state(after, expected[label])
                if name == 'T1':
                    t1 = after
                if name == 'T4':
                    equal_state(after, t1)
            for name in ('home', 'tmp', 'xdg-config', 'xdg-cache', 'xdg-data', 'xdg-runtime'):
                require(not any((fixture / name).iterdir()), 'private environment residue: ' + name)
            require(not any(part.startswith('.rmux-') for key in after for part in PurePosixPath(key).parts), 'transaction residue')
            logs = [p for tag in ('T1', 'T2', 'T3', 'T4') for p in (fixture / tag).glob('*') if p.is_file()]
            require(sum(p.stat().st_size for p in logs) <= MAX_LOG_BYTES, 'log budget')
        for subject in subjects:
            inspect_archive(subject)
        require(result['installer_invocations'] == result['help_invocations'] == 4, 'invocation accounting')
        require(time.monotonic() - clock < 480, 'native lot total budget')
        result['verdict'] = 'PASS'
    except Exception as exc:
        result['error'] = type(exc).__name__ + ': ' + str(exc)
        raise
    finally:
        result.update(end=now(), seconds=time.monotonic() - clock)
        write_json(fixture / 'NATIVE-RESULT.json', result)


class ProofRejectionTests(unittest.TestCase):
    def test_noncanonical_paths(self):
        for name in ('', '.', '..', '../x', '/x', 'x/../y', 'x//y', 'x/./y', 'x/', 'x\\y', 'x\ny'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                relative(name)
        self.assertEqual(str(relative('share/man/man1/rmux.1')), 'share/man/man1/rmux.1')

    def test_tar_special_types_and_modes(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.FIFOTYPE):
            item = tarfile.TarInfo(ARCHIVE_ROOT + '/bad')
            item.type, item.mode = kind, 0o644
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                member_record(item)
        for mode in (0o4755, 0o777, 0o600):
            item = tarfile.TarInfo(ARCHIVE_ROOT + '/bad')
            item.mode = mode
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                member_record(item)
        item = tarfile.TarInfo(ARCHIVE_ROOT + '/huge')
        item.mode, item.size = 0o644, 21000000
        with self.assertRaises(ValueError):
            member_record(item)

    def test_snapshot_detects_bytes_modes_links_and_extras(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            p = root / 'foreign'
            p.write_bytes(b'keep')
            p.chmod(0o600)
            (root / 'link').symlink_to('foreign')
            original = snapshot(root)
            for change in ('bytes', 'mode', 'link', 'extra'):
                with self.subTest(change=change):
                    if change == 'bytes': p.write_bytes(b'lost')
                    if change == 'mode': p.chmod(0o644)
                    if change == 'link':
                        (root / 'link').unlink()
                        (root / 'link').symlink_to('elsewhere')
                    if change == 'extra': (root / '.rmux-install.lock').mkdir()
                    with self.assertRaises(ValueError): equal_state(snapshot(root), original)
                    p.write_bytes(b'keep')
                    p.chmod(0o600)
                    (root / 'link').unlink()
                    (root / 'link').symlink_to('foreign')
                    if change == 'extra': (root / '.rmux-install.lock').rmdir()
                    equal_state(snapshot(root), original)

    def test_smoke_requires_real_trace_and_exit(self):
        trace = '+123.100000 73: /private/bin/rmux --help\n+123.200000 76: status=1\n'
        self.assertEqual(smoke_trace(trace, '/private/bin/rmux')['returncode'], 1)
        for bad in ('', trace * 2, trace.replace('status=1', 'status=127'), trace.replace('--help', '--version')):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                smoke_trace(bad, '/private/bin/rmux')

    def test_descriptor_and_manifest_are_independent(self):
        descriptor = {'entries': [dict(id='cli', role='executable', variants={'unix-archive': dict(path='bin/rmux', mode='0755', install='bin/rmux')})]}
        subject = dict(texts={'share/rmux/package-layout.tsv': 'bin/rmux\t0755\tbin/rmux\trequired\tcli\texecutable\n'}, members=[dict(path=ARCHIVE_ROOT + '/bin/rmux', type='file', mode='0o755', bytes=4, sha256='a' * 64)])
        self.assertEqual(set(projection(subject, descriptor)), {'bin/rmux'})
        for bad in ('', subject['texts']['share/rmux/package-layout.tsv'] * 2, 'bin/rmux\t0755\telsewhere\trequired\tcli\texecutable\n'):
            mutation = copy.deepcopy(subject)
            mutation['texts']['share/rmux/package-layout.tsv'] = bad
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                projection(mutation, descriptor)

    def test_observer_preserves_help_and_rm_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'proof').mkdir()
            script = root / 'rm'
            script.write_text(OBSERVER)
            script.chmod(0o755)
            env = dict(os.environ, TMPDIR=str(root), RMUX_PROOF_STEP=str(root / 'proof'))
            paths = [root / ('rmux-install-' + kind + '.test') for kind in ('stdout', 'stderr')]
            for p, data in zip(paths, (b'usage: rmux\n', b'actual stderr\n')):
                p.write_bytes(data)
            run = subprocess.run([str(script), '-f', *map(str, paths)], env=env, capture_output=True)
            self.assertEqual(run.returncode, 0)
            self.assertFalse(any(p.exists() for p in paths))
            self.assertEqual((root / 'proof/help.stdout').read_bytes(), b'usage: rmux\n')
            self.assertEqual((root / 'proof/help.stderr').read_bytes(), b'actual stderr\n')
            run = subprocess.run([str(script), str(root / 'absent')], env=env, capture_output=True)
            self.assertEqual(run.returncode, 1)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', type=Path)
    args, remaining = parser.parse_known_args()
    if args.native:
        require(not remaining, 'unexpected native arguments')
        native(args.native)
    else:
        unittest.main(argv=[sys.argv[0], *remaining])
