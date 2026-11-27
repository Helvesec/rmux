"""Observe rustc's original dep-info destination without changing its outputs."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class DepinfoPaths(unittest.TestCase):
    def test_foreign_depinfo_refusal_uses_running_python_without_path_python3(self):
        guard = self.load_guard()
        units = guard.support('candidate_identity_units')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for alias in (False, True):
                with self.subTest(inert_alias=alias):
                    if alias:
                        (root / 'python3').write_text('inert alias\n')
                        (root / 'python3.exe').write_text('inert alias\n')
                    with patch.dict(os.environ, PATH=str(root)), \
                            patch.object(self, 'load_guard', return_value=guard), \
                            patch.object(units, 'before', wraps=units.before) as before:
                        self.test_foreign_depinfo_is_refused_before_compilation()
                    compiler = before.call_args.args[1][0]
                    self.assertEqual(Path(compiler), Path(sys.executable).resolve())
                    output = subprocess.run(
                        [compiler, '-c', 'import sys; print(sys.executable)'],
                        capture_output=True, text=True, check=True,
                        env=dict(os.environ, PATH=str(root)))
                    self.assertEqual(Path(output.stdout.strip()).resolve(), Path(sys.executable).resolve())

    def load_guard(self):
        spec = importlib.util.spec_from_file_location('depinfo_identity', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        return guard

    def test_single_unnamed_emit_uses_exact_o_path(self):
        guard = self.load_guard()
        units = guard.support('candidate_identity_units')
        rustc = os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            code = root / 'source.rs'
            code.write_text('pub fn example() {}\n')
            target = root / 'target'
            target.mkdir()
            for index, emit in enumerate(['dep-info', 'metadata=' + str(target / 'explicit.rmeta') + ',dep-info']):
                with self.subTest(emit=emit):
                    original = target / ('deps' + str(index))
                    args = [rustc, '--crate-name', 'example', '--crate-type', 'lib', str(code),
                            '--emit=' + emit, '-o', str(original)]
                    unit = dict(depinfo=target / 'identity-depinfo' / (str(index) + '.d'),
                                graph=dict(input=dict(kind='workspace', input=str(code)), generation=None))
                    self.assertEqual(units.with_depinfo(args, unit), args)
                    result = subprocess.run(args, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    native = original.read_bytes()
                    checked = []
                    g = SimpleNamespace(IdentityError=guard.IdentityError,
                                        tracked_input=lambda path, *args: checked.append(path))
                    units.check_depinfo(g, unit, root, 'head', target)
                    self.assertIn(code, checked)
                    self.assertEqual(unit['observed_depinfo'], original)
                    self.assertEqual(original.read_bytes(), native)
                    self.assertEqual(unit['depinfo'].read_bytes(), native)

    def test_default_explicit_and_added_depinfo_are_checked_without_redirection(self):
        guard = self.load_guard()
        units = guard.support('candidate_identity_units')
        rustc = os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            code = root / 'source.rs'
            code.write_text('pub fn example() {}\n')
            target = root / 'target'
            target.mkdir()
            common = [rustc, '--crate-name', 'example', '--crate-type', 'lib', str(code)]
            cases = [
                (['--emit', 'metadata,dep-info', '-o', str(target / 'custom.rmeta')], target / 'custom.d'),
                (['--emit=metadata,dep-info=' + str(target / 'explicit.d'), '--out-dir', str(target)], target / 'explicit.d'),
                (['--emit=metadata', '--out-dir', str(target)], None),
            ]
            for index, (options, original) in enumerate(cases):
                with self.subTest(options=options):
                    args = common + options
                    unit = dict(depinfo=target / 'identity-depinfo' / (str(index) + '.d'),
                                graph=dict(input=dict(kind='workspace', input=str(code)), generation=None))
                    changed = units.with_depinfo(args, unit)
                    if original:
                        self.assertEqual(changed, args)
                    result = subprocess.run(changed, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    native = original.read_bytes() if original else None
                    checked = []
                    g = SimpleNamespace(IdentityError=guard.IdentityError,
                                        tracked_input=lambda path, *args: checked.append(path))
                    units.check_depinfo(g, unit, root, 'head', target)
                    self.assertIn(code, checked)
                    if original:
                        self.assertEqual(original.read_bytes(), native)
                        self.assertEqual(unit['depinfo'].read_bytes(), native)

    def test_foreign_depinfo_is_refused_before_compilation(self):
        guard = self.load_guard()
        units = guard.support('candidate_identity_units')
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            code = source / 'source.rs'
            code.write_text('pub fn example() {}\n')
            (source / 'Cargo.lock').write_text('version = 4\n')
            target = source / 'target'
            target.mkdir()
            compiler = Path(sys.executable).resolve()
            args = [str(compiler), str(code), '--crate-name', 'example', '--crate-type', 'lib',
                    '--out-dir', str(target), '--emit=metadata,dep-info=' + str(source / 'foreign.d')]
            env = dict(RMUX_IDENTITY_RUSTC=str(compiler))
            with patch.object(guard, 'tracked_input'), patch.object(units, 'source_proof', return_value={}):
                with self.assertRaisesRegex(guard.IdentityError, 'dep-info output is not private'):
                    units.before(guard, args, source, 'head', target, env)
            self.assertFalse((source / 'foreign.d').exists())


if __name__ == '__main__':
    unittest.main()
