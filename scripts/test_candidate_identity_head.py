"""A warm image proof never authorizes a different, even identical, HEAD."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py').resolve()
PROBE = '''
import importlib.util, json, sys
from pathlib import Path
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('guard', sys.argv[1])
g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)
blocked = {Path(p).resolve() for p in json.loads(sys.argv[2])}
read = Path.read_bytes
def checked_read(path):
    if path.resolve() in blocked:
        raise AssertionError('image read after HEAD changed: ' + str(path))
    return read(path)
sys.argv = [sys.argv[1], *sys.argv[3:]]
with patch.object(Path, 'read_bytes', checked_read):
    sys.exit(g.main())
'''


class CachedHeadAcceptance(unittest.TestCase):
    def test_empty_commit_after_cache_refuses_check_run_with_without_image_read(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = SCRIPT.parent.parent / '.rmux-audit' / 'identity-head'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='acceptance-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        (source / 'Cargo.toml').write_text('[package]\nname="head-fixture"\nversion="0.1.0"\n')
        scripts = source / 'scripts'
        scripts.mkdir()
        (scripts / 'candidate_identity_windows.rs').write_bytes(
            (SCRIPT.parent / 'candidate_identity_windows.rs').read_bytes())
        code = source / 'main.rs'
        code.write_text('fn main() { println!("qualified"); }\n')
        git = [shutil.which('git'), '-C', str(source)]
        subprocess.run(git + ['init', '-q'], check=True)
        subprocess.run(git + ['add', '.'], check=True)
        commit = ['-c', 'user.name=Identity Test', '-c', 'user.email=identity@example.invalid',
                  'commit', '-qm']
        subprocess.run(git + commit + ['fixture'], check=True)
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()).resolve()
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')))
        for key in tuple(env):
            if (key.startswith('RMUX_IDENTITY_')
                    or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER'))
                    or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')):
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
            self.assertEqual(guard.rustc_wrapper([str(rustc), '--crate-name', 'head_fixture',
                                                '--crate-type', 'bin', str(code), '-o', str(binary)]), 0)
        cold = subprocess.run([sys.executable, str(SCRIPT), 'check', str(binary)],
                              env=env, cwd=source, capture_output=True, text=True)
        self.assertEqual(cold.returncode, 0, cold.stderr)
        selected = Path(cold.stdout.strip())
        record = guard.verify_built(binary, source, env['RMUX_IDENTITY_HEAD'], target)
        retained = Path(json.loads(Path(record['build']).read_text())['path'])
        blocked = {binary, selected, retained}
        paths = [('check', str(path)) for path in (binary, selected)]
        paths += [('run', str(path)) for path in (binary, selected)]
        paths += [('with', '--', sys.executable, '-c', 'print("scenario reached")')]
        for args in paths:
            warm = subprocess.run([sys.executable, str(SCRIPT), *args], env=env,
                                  cwd=source, capture_output=True, text=True)
            self.assertEqual(warm.returncode, 0, warm.stderr)
        stamps = {path: guard.stamp(path.stat()) for path in blocked}
        tree = guard.git(source, 'rev-parse', 'HEAD^{tree}')
        subprocess.run(git + commit + ['empty successor', '--allow-empty'], check=True)
        self.assertNotEqual(guard.git(source, 'rev-parse', 'HEAD'), env['RMUX_IDENTITY_HEAD'])
        self.assertEqual(guard.git(source, 'rev-parse', 'HEAD^{tree}'), tree)
        self.assertEqual(guard.git(source, 'status', '--porcelain'), '')
        self.assertEqual({path: guard.stamp(path.stat()) for path in blocked}, stamps)
        for args in paths:
            with self.subTest(mode=args[0], path=args[1]):
                rejected = subprocess.run([sys.executable, '-c', PROBE, str(SCRIPT),
                                           json.dumps([str(p) for p in blocked]), *args],
                                          env=env, cwd=source, capture_output=True, text=True)
                self.assertEqual(rejected.returncode, 86, rejected.stderr)
                self.assertIn('candidate HEAD differs from pinned commit', rejected.stderr)
                self.assertEqual(rejected.stdout, '', 'rejected identity reached the scenario')


if __name__ == '__main__':
    unittest.main()
