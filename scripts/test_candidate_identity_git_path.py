"""Consumer PATH isolation must not hide the identity guard's qualified Git."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py').resolve()


@unittest.skipIf(os.name == 'nt', 'Unix consumer launchers')
class IsolatedGitPathAcceptance(unittest.TestCase):
    def test_selected_child_keeps_isolated_path_and_identity_checks(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = SCRIPT.parent.parent / '.rmux-audit' / 'identity-git-path'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='acceptance-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        (source / 'Cargo.toml').write_text('[package]\nname="git-path-fixture"\nversion="0.1.0"\n')
        code = source / 'main.rs'
        code.write_text('fn main() { println!("{}|{}", std::env::var("PATH").unwrap(), '
                        'std::env::args().nth(1).unwrap()); }\n')
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
        binary = target / 'fixture'
        with patch.dict(os.environ, env, clear=True):
            self.assertEqual(guard.rustc_wrapper([str(rustc), '--crate-name', 'git_path_fixture',
                                                '--crate-type', 'bin', str(code), '-o', str(binary)]), 0)
        selected = subprocess.run([sys.executable, str(SCRIPT), 'check', str(binary)],
                                  env=env, cwd=source, capture_output=True, text=True)
        self.assertEqual(selected.returncode, 0, selected.stderr)
        command = [selected.stdout.strip(), 'fixture argument']
        isolated = target / 'fixture-bin'
        isolated.mkdir()
        env['PATH'] = str(isolated)

        def launch():
            return subprocess.run(command, env=env, cwd=source, capture_output=True, text=True)

        for shadow_git in (False, True):
            with self.subTest(shadow_git=shadow_git):
                if shadow_git:
                    fake = isolated / 'git'
                    fake.write_text('#!/bin/sh\nprintf "fixture Git must not run" >&2\nexit 99\n')
                    fake.chmod(0o755)
                result = launch()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, str(isolated) + '|fixture argument\n')
        original = code.read_text()
        code.write_text(original + '// dirty source\n')
        dirty = launch()
        self.assertEqual(dirty.returncode, 86, dirty.stderr)
        self.assertIn('candidate source is not clean', dirty.stderr)
        self.assertEqual(dirty.stdout, '')
        code.write_text(original)
        subprocess.run(git + commit + ['successor', '--allow-empty'], check=True)
        changed_head = launch()
        self.assertEqual(changed_head.returncode, 86, changed_head.stderr)
        self.assertIn('candidate HEAD differs from pinned commit', changed_head.stderr)
        self.assertEqual(changed_head.stdout, '')


if __name__ == '__main__':
    unittest.main()
