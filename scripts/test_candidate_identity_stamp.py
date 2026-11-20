"""D19 acceptance: reuse a gate proof across PIDs, reject changed evidence."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py').resolve()
PROBE = '''
import importlib.util, json, os, sys
from pathlib import Path
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('guard', sys.argv[1])
g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)
source, head, target = g.context()
read = Path.read_bytes
writes = Path.write_bytes
source_identity = g.source_identity
source_checks = []
def checked_source_identity(source, head):
    source_checks.append((str(source), head))
    return source_identity(source, head)
def checked_read(path):
    if sys.argv[3] == 'warm' and path in blocked:
        raise AssertionError('unchanged binary was read/hashed again: ' + str(path))
    return read(path)
def checked_write(path, data):
    if sys.argv[3] == 'warm' and path.parent.name == 'identity-launchers':
        raise AssertionError('existing launcher was copied again')
    return writes(path, data)
blocked = {Path(p) for p in json.loads(sys.argv[4])}
with patch.object(g, 'source_identity', checked_source_identity), \
        patch.object(Path, 'read_bytes', checked_read), patch.object(Path, 'write_bytes', checked_write):
    record = (g.verify_built(Path(sys.argv[2]), source, head, target) if os.name == 'nt'
              else g.support('candidate_identity_retention').select(g, Path(sys.argv[2]), source, head, target))
record = dict(record)
record['_source_checks'] = len(source_checks)
print(json.dumps(record))
'''


class IdentityStampAcceptance(unittest.TestCase):
    def test_gate_reuse_concurrency_and_changed_binary_refusal(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = SCRIPT.parent.parent / '.rmux-audit' / 'identity-stamp'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='acceptance-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        (source / 'Cargo.toml').write_text('[package]\nname="stamp-fixture"\nversion="0.1.0"\n')
        scripts = source / 'scripts'
        scripts.mkdir()
        (scripts / 'candidate_identity_windows.rs').write_bytes(
            (SCRIPT.parent / 'candidate_identity_windows.rs').read_bytes())
        code = source / 'main.rs'
        code.write_text('fn main() { println!("qualified"); }\n')
        subprocess.run(['git', '-C', str(source), 'init', '-q'], check=True)
        subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(source), '-c', 'user.name=Identity Test',
                        '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture'], check=True)
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()).resolve()
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')))
        for key in tuple(env):
            if key.startswith('RMUX_IDENTITY_') or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER')) or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'):
                env.pop(key)
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True):
                env = guard.prepare()
        finally:
            os.chdir(previous)
        target = Path(env['RMUX_IDENTITY_TARGET'])
        env['CARGO_MANIFEST_DIR'] = str(source)
        binary = target / ('rmux.exe' if os.name == 'nt' else 'rmux')
        with patch.dict(os.environ, env, clear=True):
            self.assertEqual(guard.rustc_wrapper([str(rustc), '--crate-name', 'stamp_fixture',
                                                '--crate-type', 'bin', str(code), '-o', str(binary)]), 0)
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()

        def command(path, phase='cold', blocked=()):
            return [sys.executable, '-c', PROBE, str(SCRIPT), str(path), phase,
                    json.dumps([str(p) for p in blocked])]

        cold = subprocess.run(command(binary), env=env, capture_output=True, text=True)
        self.assertEqual(cold.returncode, 0, cold.stderr)
        selected = Path(json.loads(cold.stdout)['path'])
        record = guard.verify_built(binary, source, env['RMUX_IDENTITY_HEAD'], target)
        retained = Path(json.loads(Path(record['build']).read_text())['path'])
        blocked = {binary, retained, selected}
        launcher_stamp = guard.stamp(selected.stat())
        for path in (binary, selected):
            warm = subprocess.run(command(path, 'warm', blocked), env=env, capture_output=True, text=True)
            self.assertEqual(warm.returncode, 0, warm.stderr)
            actual = json.loads(warm.stdout)
            self.assertGreaterEqual(actual.pop('_source_checks'), 1)
            self.assertEqual(actual['commit'], env['RMUX_IDENTITY_HEAD'])
            self.assertEqual(actual.get('producer', actual)['sha256'], digest)
        self.assertEqual(guard.stamp(selected.stat()), launcher_stamp)
        children = [subprocess.Popen(command(binary, 'warm', blocked), env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for _ in range(4)]
        for child in children:
            _, error = child.communicate(timeout=30)
            self.assertEqual(child.returncode, 0, error)
        run = subprocess.run([sys.executable, str(SCRIPT), 'run', str(selected)], env=env,
                             capture_output=True, text=True)
        self.assertEqual((run.returncode, run.stdout), (0, 'qualified\n'), run.stderr)
        original = binary.read_bytes()
        for defect in ('write', 'replace', 'retained', 'launcher', 'unit-receipt'):
            with self.subTest(defect=defect):
                warmed = subprocess.run([sys.executable, str(SCRIPT), 'check', str(binary)],
                                        env=env, capture_output=True, text=True)
                self.assertEqual(warmed.returncode, 0, warmed.stderr)
                path = (Path(record['unit']['receipt']) if defect == 'unit-receipt' else
                        retained if defect == 'retained' else selected if defect == 'launcher' else binary)
                saved = path.read_bytes()
                path.chmod(0o700)
                corrupted = bytearray(saved)
                corrupted[-1] ^= 1
                if defect == 'replace':
                    replacement = path.with_name('replacement')
                    replacement.write_bytes(corrupted)
                    replacement.chmod(0o700)
                    os.utime(replacement, ns=(path.stat().st_atime_ns, path.stat().st_mtime_ns))
                    os.replace(replacement, path)
                else:
                    mtime = path.stat().st_mtime_ns
                    path.write_bytes(corrupted)
                    os.utime(path, ns=(path.stat().st_atime_ns, mtime))
                rejected = subprocess.run([sys.executable, str(SCRIPT), 'check', str(binary)], env=env,
                                          capture_output=True, text=True)
                self.assertEqual(rejected.returncode, 86, rejected.stderr)
                self.assertEqual(rejected.stdout, '')
                path.write_bytes(saved)
        self.assertEqual(binary.read_bytes(), original)
        (source / 'untracked-source-change.rs').write_text('fn main() {}\n')
        for mode, argv in (
                ('check', [sys.executable, str(SCRIPT), 'check', str(selected)]),
                ('run', [sys.executable, str(SCRIPT), 'run', str(selected)]),
                ('with', [sys.executable, str(SCRIPT), 'with', '--', sys.executable, '-c', 'pass'])):
            with self.subTest(mode=mode):
                rejected = subprocess.run(argv, env=env, capture_output=True, text=True)
                self.assertEqual(rejected.returncode, 86, rejected.stderr)
                self.assertIn('candidate source is not clean', rejected.stderr)

    def test_windows_wrapper_source_is_pinned_to_fixture_checkout(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        windows = guard.support('candidate_identity_windows')
        audit = SCRIPT.parent.parent / '.rmux-audit' / 'identity-stamp'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='windows-source-', dir=audit)).resolve()
        target = source / '.rmux-audit' / 'target'
        target.mkdir(parents=True)
        scripts = source / 'scripts'
        scripts.mkdir()
        wrapper_source = scripts / 'candidate_identity_windows.rs'
        wrapper_source.write_bytes((SCRIPT.parent / 'candidate_identity_windows.rs').read_bytes())
        subprocess.run(['git', '-C', str(source), 'init', '-q'], check=True)
        subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(source), '-c', 'user.name=Identity Test',
                        '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture'], check=True)
        head = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
        env = dict(RMUX_IDENTITY_RUSTC=sys.executable, RMUX_IDENTITY_RUSTDOC=sys.executable,
                   RMUX_IDENTITY_PYTHON=sys.executable, RMUX_IDENTITY_SCRIPT=str(SCRIPT))

        class Units:
            def before(self, *args):
                return {'kind': 'bin'}

            def with_depinfo(self, args, unit):
                return list(args)

            def graph_object(self, *args):
                return []

            def after(self, *args):
                pass

        def support(name):
            return Units() if name == 'candidate_identity_units' else guard.support(name)

        real_run = subprocess.run

        def compile_wrapper(args, *popenargs, **kwargs):
            if '-o' not in args:
                return real_run(args, *popenargs, **kwargs)
            output = Path(args[args.index('-o') + 1])
            output.write_bytes(b'wrapper')
            return subprocess.CompletedProcess(args, 0)

        fake_guard = SimpleNamespace(__file__=str(SCRIPT), tracked_input=guard.tracked_input,
                                     support=support, compiler_receipts=lambda *args: None,
                                     file_hash=guard.file_hash)
        with patch.object(windows.subprocess, 'run', side_effect=compile_wrapper):
            windows.install(fake_guard, env, source, head, target)
        self.assertEqual((target / 'rustc-wrapper.exe').read_bytes(), b'wrapper')


if __name__ == '__main__':
    unittest.main()
