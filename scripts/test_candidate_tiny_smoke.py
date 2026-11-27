"""Exercise the package entrypoint with a real, private candidate identity."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parent


@unittest.skipUnless(os.name == 'posix', 'Unix tiny package smoke')
class TinySmokeIdentity(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        audit = SCRIPTS.parent / '.rmux-audit' / 'tiny-smoke-tests'
        audit.mkdir(parents=True, exist_ok=True)
        cls.root = Path(tempfile.mkdtemp(prefix='source with spaces-', dir=audit)).resolve()
        shutil.copytree(SCRIPTS, cls.root / 'scripts', ignore=shutil.ignore_patterns('__pycache__'))
        # setUp imports the fixture's own candidate_identity, which writes bytecode
        # beside it; keep the repository's __pycache__/ rule so the tree stays clean.
        (cls.root / '.gitignore').write_text('.rmux-audit/\n__pycache__/\n')
        (cls.root / 'Cargo.toml').write_text('[package]\nname="fixture"\nversion="0.1.0"\nedition="2021"\n[workspace.package]\nversion = "0.1.0"\n')
        (cls.root / 'Cargo.lock').write_text('version = 4\n')
        (cls.root / 'main.rs').write_text('fn main() { println!("observed package fixture"); }\n')
        cargo = cls.root / 'scripts' / 'cargo'
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$CARGO_TARGET_DIR" > "$PACKAGE_TEST_EVENT"\nexit 73\n')
        cargo.chmod(0o755)
        versions = cls.root / 'scripts' / 'check-release-versions.sh'
        versions.write_text('#!/bin/sh\nprintf "scenario\\n" > "$PACKAGE_TEST_EVENT"\nexit 72\n')
        subprocess.run(['git', 'init', '-q', str(cls.root)], check=True)
        subprocess.run(['git', '-C', str(cls.root), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(cls.root), '-c', 'user.name=Identity Test',
                        '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'fixture'], check=True)

    def setUp(self):
        spec = importlib.util.spec_from_file_location('package_identity', self.root / 'scripts/candidate_identity.py')
        self.identity = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.identity)
        env = {k: v for k, v in os.environ.items()
               if not k.startswith('RMUX_IDENTITY_')
               and not (k.startswith('CARGO_TARGET_') and k.endswith('_RUNNER'))
               and k not in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')}
        rustc = os.environ.get('RMUX_RETENTION_TEST_RUSTC') or shutil.which('rustc')
        self.assertTrue(rustc, 'native rustc required')
        env.update(RUSTC=rustc, RUSTDOC=str(Path(rustc).with_name('rustdoc')))
        previous = Path.cwd()
        try:
            os.chdir(self.root)
            with patch.dict(os.environ, env, clear=True):
                self.env = self.identity.prepare()
        finally:
            os.chdir(previous)
        self.target = Path(self.env['RMUX_IDENTITY_TARGET'])
        self.event = self.target / 'event'
        self.env.update(PACKAGE_TEST_EVENT=str(self.event), RMUX_PYTHON=sys.executable,
                        PATH=str(self.root / 'scripts') + os.pathsep + env['PATH'],
                        TMPDIR=str(self.target), CARGO_MANIFEST_DIR=str(self.root))

    def invoke(self, *args, gate=False, env=None):
        entry = 'release-review-gate.sh' if gate else 'smoke-tiny-release-review.sh'
        if gate:
            args = tuple('--skip-package-build' if arg == '--skip-build' else arg for arg in args)
        return subprocess.run(['/bin/bash', str(self.root / 'scripts' / entry),
                               *(['--section', 'package'] if gate else []), '--no-tmux', *args],
                              cwd=self.root, env=env or self.env, capture_output=True,
                              text=True, timeout=30)

    def test_default_build_uses_inherited_candidate_target(self):
        for gate in (False, True):
            with self.subTest(gate=gate):
                result = self.invoke(gate=gate)
                self.assertEqual(result.returncode, 73, result.stderr)
                self.assertEqual(self.event.read_text().strip(), str(self.target))
                layouts = list(self.target.glob('tiny-release-layout.*'))
                self.assertTrue(layouts, 'default package must live in the private target')

    def test_outside_layout_is_refused_before_build(self):
        for gate in (False, True):
            with self.subTest(gate=gate):
                result = self.invoke('--layout', str(self.root / '.rmux-audit' / 'outside'), gate=gate)
                self.assertEqual(result.returncode, 86, result.stderr)
                self.assertIn('package layout outside candidate target', result.stderr)
                self.assertFalse(self.event.exists())

    def test_redirected_layout_and_foreign_target_are_refused_before_build(self):
        outside = self.root / '.rmux-audit' / 'outside'
        outside.mkdir(exist_ok=True)
        link = self.target / 'redirect'
        link.symlink_to(outside, target_is_directory=True)
        for args in (('--layout', str(link / 'package')),
                     ('--target-dir', str(outside))):
            with self.subTest(args=args):
                result = self.invoke(*args)
                self.assertEqual(result.returncode, 86, result.stderr)
                self.assertFalse(self.event.exists())

    def test_redirected_package_child_is_refused_before_install(self):
        layout = self.target / 'package'
        layout.mkdir()
        (layout / 'bin').symlink_to(self.root, target_is_directory=True)
        result = self.invoke('--layout', str(layout))
        self.assertEqual(result.returncode, 86, result.stderr)
        self.assertFalse(self.event.exists())

    def test_invalid_context_is_refused_before_build(self):
        env = dict(self.env, RMUX_IDENTITY_HEAD='0' * 40)
        result = self.invoke(env=env)
        self.assertEqual(result.returncode, 86, result.stderr)
        self.assertFalse(self.event.exists())

    def test_explicit_private_relative_layout_reaches_build(self):
        layout = self.target / 'package with spaces'
        result = self.invoke('--layout', str(layout.relative_to(self.root)),
                             '--target-dir', str(self.target))
        self.assertEqual(result.returncode, 73, result.stderr)
        self.assertEqual(self.event.read_text().strip(), str(self.target))

    def package(self):
        binary = self.target / 'fixture'
        with patch.dict(os.environ, self.env, clear=True):
            self.assertEqual(self.identity.rustc_wrapper([
                self.env['RUSTC'], '--crate-name', 'fixture', '--crate-type', 'bin',
                str(self.root / 'main.rs'), '-o', str(binary)]), 0)
        layout = self.target / 'package'
        for name in ('bin/rmux', 'libexec/rmux/rmux', 'bin/rmux-daemon'):
            path = layout / name
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(binary, path)
        return layout

    def test_reuse_checks_all_three_observed_images_before_scenario(self):
        layout = self.package()
        for gate in (False, True):
            result = self.invoke('--skip-build', '--layout', str(layout), gate=gate)
            self.assertEqual(result.returncode, 72, result.stderr)
            self.event.unlink()
        for name in ('bin/rmux', 'libexec/rmux/rmux', 'bin/rmux-daemon'):
            with self.subTest(role=name):
                path = layout / name
                data = path.read_bytes()
                path.write_bytes(data + b'unobserved mutation')
                result = self.invoke('--skip-build', '--layout', str(layout))
                self.assertEqual(result.returncode, 86, result.stderr)
                self.assertFalse(self.event.exists())
                path.write_bytes(data)

    def test_reuse_outside_target_is_rejected_even_with_identical_bytes(self):
        layout = self.package()
        outside = self.root / '.rmux-audit' / self.target.name
        shutil.copytree(layout, outside)
        result = self.invoke('--skip-build', '--layout', str(outside))
        self.assertEqual(result.returncode, 86, result.stderr)
        self.assertFalse(self.event.exists())

    def test_unqualified_build_keeps_explicit_layout_and_target(self):
        env = {k: v for k, v in self.env.items() if not k.startswith('RMUX_IDENTITY_')}
        result = self.invoke('--layout', str(self.target / 'ordinary'),
                             '--target-dir', str(self.target), env=env)
        self.assertEqual(result.returncode, 73, result.stderr)
        self.assertEqual(self.event.read_text().strip(), str(self.target))


if __name__ == '__main__':
    unittest.main()
