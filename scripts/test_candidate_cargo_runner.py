"""Native Cargo runner paths must retain all candidate identity checks."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch


@unittest.skipIf(os.name == 'nt', 'Unix Cargo runner path regression')
class CandidateCargoRunner(unittest.TestCase):
    def test_relative_cargo_run_and_sibling_launches_are_verified(self):
        script, guard, source, cargo, qualified = self.qualified_fixture(
            'identity-cargo-runner', {'src/main.rs': 'fn main() { println!("verified {}", std::process::id()); }\n'})
        target = Path(qualified['RMUX_IDENTITY_TARGET'])
        binary = target / 'debug/identity-cargo-runner'
        relative = os.path.relpath(binary, source)
        # Cargo invokes this runner with a relative path inside the checkout.
        result = subprocess.run([str(cargo), 'run', '--locked', '--offline', '--quiet'],
                                cwd=source, env=qualified, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        outputs = [result.stdout]
        for mode, selected in (('run', str(binary)), ('exec', str(binary)),
                               ('run', relative), ('exec', relative)):
            launched = subprocess.run([qualified['RMUX_IDENTITY_PYTHON'], str(script), mode, selected],
                                      cwd=source, env=qualified, capture_output=True, text=True)
            self.assertEqual(launched.returncode, 0, launched.stderr)
            outputs.append(launched.stdout)
        head = guard.git(source, 'rev-parse', 'HEAD')
        built = guard.verify_built(binary, source, head, target)
        launches = [json.loads(path.read_text()) for path in (target / 'identity-launches').glob('*.json')]
        for output in outputs:
            token, pid = output.strip().split()
            self.assertEqual(token, 'verified')
            matches = [row for row in launches if row.get('event') == 'exec'
                       and row['consumer']['pid'] == int(pid)
                       and row['executables'][0]['sha256'] == built['sha256']]
            self.assertEqual(len(matches), 1, 'actual exec requires its own verified producer/PID')
            self.assertTrue(matches[0]['consumer']['birth'])
        outside = source / '.rmux-audit/outside'
        outside.write_bytes(binary.read_bytes())
        outside.chmod(0o755)
        symlink = target / 'redirected'
        symlink.symlink_to(outside)
        for selected in (str(outside), os.path.relpath(outside, source), os.path.relpath(symlink, source)):
            for mode in ('run', 'exec'):
                refused = subprocess.run([qualified['RMUX_IDENTITY_PYTHON'], str(script), mode, selected],
                                         cwd=source, env=qualified, capture_output=True, text=True)
                self.assertEqual(refused.returncode, 86)
                self.assertEqual(refused.stdout, '')
        # The pure verification API continues requiring absolute paths.
        with self.assertRaises(guard.IdentityError):
            guard.verify(Path(relative), source, head, target)

    def test_compiler_wrapper_keeps_cargo_names_that_are_not_shell_identifiers(self):
        # dash (Debian/Ubuntu /bin/sh) drops such names, so a #!/bin/sh
        # compiler wrapper made env!("CARGO_BIN_EXE_rmux-daemon") undefined.
        test = ('const DAEMON: &str = env!("CARGO_BIN_EXE_dash-probe");\n'
                '#[test]\nfn names_the_dashed_binary() { assert!(DAEMON.ends_with("dash-probe")); }\n')
        _, _, source, cargo, qualified = self.qualified_fixture(
            'dash-probe', {'src/main.rs': 'fn main() {}\n', 'tests/dashed.rs': test})
        result = subprocess.run([str(cargo), 'test', '--locked', '--offline', '--quiet', '--test', 'dashed'],
                                cwd=source, env=qualified, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def qualified_fixture(self, name, files):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('cargo_runner_identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = script.parent.parent / '.rmux-audit/cargo-runner-acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='native-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.toml').write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n\n[workspace]\n')
        (source / 'Cargo.lock').write_text(f'version = 4\n\n[[package]]\nname = "{name}"\nversion = "0.1.0"\n')
        for relative, text in files.items():
            (source / relative).parent.mkdir(parents=True, exist_ok=True)
            (source / relative).write_text(text)
        git = ['git', '-C', str(source)]
        subprocess.run(git + ['init', '-q'], check=True)
        subprocess.run(git + ['add', '.'], check=True)
        subprocess.run(git + ['-c', 'user.name=Identity Test', '-c',
                              'user.email=identity@example.invalid', 'commit', '-qm', 'fixture'], check=True)
        # Keep the selected spelling: a rustup/snap link resolves to a multi-call proxy.
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or shutil.which('rustc')).absolute()
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')),
                   PATH=str(rustc.parent) + os.pathsep + os.environ.get('PATH', os.defpath))
        for key in tuple(env):
            if (key.startswith('RMUX_IDENTITY_')
                    or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER'))
                    or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')):
                env.pop(key)
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True):
                qualified = guard.prepare()
        finally:
            os.chdir(previous)
        return script, guard, source, rustc.with_name('cargo'), qualified


if __name__ == '__main__':
    unittest.main()
