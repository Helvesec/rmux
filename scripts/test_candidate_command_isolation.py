"""Two command invocations must retain distinct producers of identical bytes."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class CommandIsolation(unittest.TestCase):
    def test_windows_bootstrap_without_tracked_wrapper_source(self):
        self.test_identical_units_keep_each_command_producer_and_receipts(windows=True)

    def test_identical_units_keep_each_command_producer_and_receipts(self, windows=False):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        if windows:
            guard.os = SimpleNamespace(**vars(os))
            guard.os.name = 'nt'
            guard.host_os = lambda: 'windows'
        units = guard.support('candidate_identity_units')
        # The immutable object store adds a hash and a temporary publication
        # name. Windows' default AppData temp root exceeds its file API budget.
        temporary = {}
        if os.name == 'nt':
            parent = Path.home() / 'Projects'
            parent.mkdir(parents=True, exist_ok=True)
            temporary = {'dir': parent, 'prefix': ''}
        with tempfile.TemporaryDirectory(**temporary) as directory:
            source = Path(directory).resolve()
            (source / '.gitignore').write_text('.rmux-audit/\n')
            (source / 'Cargo.lock').write_text('version = 4\n')
            git = ['git', '-C', str(source)]
            subprocess.run(git + ['init', '-q'], check=True)
            subprocess.run(git + ['add', '.'], check=True)
            subprocess.run(git + ['-c', 'user.name=Identity Test', '-c',
                           'user.email=identity@example.invalid', 'commit', '-qm', 'fixture'], check=True)
            head = guard.git(source, 'rev-parse', 'HEAD')
            compiler = Path(sys.executable).resolve()
            real_which, real_output, real_run = guard.shutil.which, subprocess.check_output, subprocess.run
            git_program = real_which('git')
            observations = []

            def which(name):
                if name == 'git':
                    return git_program
                return str(compiler) if name in ('rustc', 'rustdoc') else real_which(name)

            def output(args, **kwargs):
                if args == [str(compiler), '-vV']:
                    return 'host: fixture-native\n'
                if args == [str(compiler), '--print', 'sysroot']:
                    # No other toolchain binary stands behind the fake compiler: it keeps its spelling.
                    return str(Path(str(compiler)).parent.parent) + '\n'
                return real_output(args, **kwargs)

            def bootstrap(args, **kwargs):
                if args[0] != str(compiler):
                    return real_run(args, **kwargs)
                self.assertIn('--emit=obj', args)
                Path(args[args.index('-o') + 1]).write_bytes(b'bootstrap object')

            # This fixture has a fake compiler and no tracked native wrapper.
            # Qualify its Windows stand-in at the install/validate boundary;
            # the real tracked-input predicate must still reject that source.
            real_support = guard.support
            native = real_support('candidate_identity_windows')

            def install(g, env, origin, pinned, target):
                self.assertEqual((origin, pinned), (source, head))
                g.tracked_input(source / 'Cargo.lock', source, head)
                with self.assertRaisesRegex(g.IdentityError, 'outside pinned source tree'):
                    g.tracked_input(script.with_name('candidate_identity_windows.rs'), source, head)
                marker = (f'RMUX_BUILD_IDENTITY_V1:{head}:'
                          f'{hashlib.sha256(os.fsencode(str(source))).hexdigest()}:'
                          f'{g.host_os()}:END').encode()
                for key, name in (('RUSTC_WRAPPER', 'rustc-wrapper.exe'),
                                  ('RUSTDOC', 'rustdoc-wrapper.exe')):
                    path = target / name
                    path.write_bytes(marker)
                    env[key] = str(path)

            def validate(g, origin, pinned, target):
                self.assertEqual((origin, pinned), (source, head))
                for key, name in (('RUSTC_WRAPPER', 'rustc-wrapper.exe'),
                                  ('RUSTDOC', 'rustdoc-wrapper.exe')):
                    path = target / name
                    self.assertEqual(os.environ[key], str(path))
                    self.assertEqual(g.verify(path, source, head, target)['path'], str(path))

            def support(name):
                return native if name == 'candidate_identity_windows' else real_support(name)

            def command(args, env):
                with patch.dict(os.environ, env, clear=True):
                    origin, pinned, target = guard.context()
                    self.assertEqual((origin, pinned), (source, head))
                    self.assertEqual(str(target), env['RMUX_IDENTITY_TARGET'])
                    path = target / 'same.rlib'
                    path.write_bytes(b'identical nonempty compiler unit')
                    graph = dict(head=head, source=str(source), input={'producer': args[0]},
                                 compiler=str(compiler), compiler_sha256=guard.file_hash(compiler),
                                 lock_sha256=guard.file_hash(source / 'Cargo.lock'),
                                 dependencies=[], generation=None)
                    unit = dict(outputs=[path], stamps={}, kind='library', graph=graph,
                                graph_sha256=units.digest(graph))
                    with patch.object(units, 'check_depinfo'):
                        units.after(guard, unit, source, head, target)
                    record = units.verify(guard, path, source, head, target)
                    marker = (f'RMUX_BUILD_IDENTITY_V1:{head}:'
                              f'{hashlib.sha256(os.fsencode(str(source))).hexdigest()}:'
                              f'{guard.host_os()}:END').encode()
                    binary = target / 'image'
                    binary.write_bytes(marker)
                    self.assertEqual(guard.verify(binary, source, head, target)['target'], str(target))
                    observations.append((target, path, record, env))
                return 0

            previous = Path.cwd()
            try:
                os.chdir(source)
                with patch.dict(os.environ, {}, clear=True), patch.object(guard.shutil, 'which', which), \
                        patch.object(guard.shutil, 'disk_usage', return_value=type('Disk', (), {'free': 100 * 1024**3})()), \
                        patch.object(guard.subprocess, 'run', bootstrap), \
                        patch.object(guard.subprocess, 'check_output', output), \
                        patch.object(native, 'install', install), \
                        patch.object(native, 'validate', validate), \
                        patch.object(guard, 'support', support), \
                        patch.object(guard.subprocess, 'call', command):
                    with patch.object(sys, 'argv', [str(script), 'with', '--', 'command-A']):
                        self.assertEqual(guard.main(), 0)
                    target_a, path_a, first, env_a = observations[0]
                    snapshot = {str(p): p.read_bytes() for p in target_a.rglob('*') if p.is_file()}
                    with patch.object(sys, 'argv', [str(script), 'with', '--', 'command-B']):
                        self.assertEqual(guard.main(), 0)
            finally:
                os.chdir(previous)
            target_b, path_b, second, env_b = observations[1]
            actual_a = units.verify(guard, path_a, source, head, target_a)
            self.assertEqual(actual_a['graph_sha256'], first['graph_sha256'],
                             'command B replaced the producer selected by command A')
            self.assertEqual(actual_a['receipt'], first['receipt'])
            self.assertNotEqual(target_a, target_b)
            self.assertEqual(path_a.read_bytes(), path_b.read_bytes())
            self.assertNotEqual(first['graph_sha256'], second['graph_sha256'])
            self.assertEqual(json.loads(Path(first['receipt']).read_text())['graph']['input'],
                             {'producer': 'command-A'})
            for name, raw in snapshot.items():
                self.assertEqual(Path(name).read_bytes(), raw, name)
            with patch.dict(os.environ, env_a, clear=True):
                self.assertEqual(guard.context()[2], target_a)
                for key in ('RMUX_IDENTITY_TARGET', 'CARGO_TARGET_DIR'):
                    with patch.dict(os.environ, {key: str(target_b)}):
                        with self.assertRaises(guard.IdentityError):
                            guard.context()
                with self.assertRaises(guard.IdentityError):
                    guard.verify(target_b / 'image', source, head, target_a)


class CommandIsolationTemporaryParent(unittest.TestCase):
    """Exercise the fixture setup without changing the host platform."""

    def exercise_entry(self, entry, windows):
        real_temporary = tempfile.TemporaryDirectory
        with real_temporary() as root:
            root = Path(root).resolve()
            home = root / 'missing' / 'home'
            parent = home / 'Projects'
            sentinel = root / 'keep'
            sentinel.write_bytes(b'owned sibling')
            self.assertFalse(home.exists())
            children = []

            def temporary(**kwargs):
                expected = {'dir': parent, 'prefix': ''} if windows else {}
                self.assertEqual(kwargs, expected)
                fixture = real_temporary(**kwargs)
                children.append(Path(fixture.name))
                return fixture

            simulated_os = SimpleNamespace(**vars(os))
            simulated_os.name = 'nt' if windows else 'posix'
            home_patch = (patch.object(Path, 'home', return_value=home) if windows else
                          patch.object(Path, 'home', side_effect=AssertionError('Unix used home')))
            with patch.object(sys.modules[__name__], 'os', simulated_os), home_patch, \
                    patch.object(tempfile, 'TemporaryDirectory', temporary):
                # Repeat with the existing parent to require idempotent setup.
                for _ in range(2):
                    getattr(CommandIsolation(entry), entry)()
                    self.assertTrue(all(not child.exists() for child in children))
                    self.assertEqual(sentinel.read_bytes(), b'owned sibling')
                    if windows:
                        self.assertTrue(parent.is_dir())
                        self.assertEqual(list(parent.iterdir()), [])
                    else:
                        self.assertFalse(home.exists())
            self.assertEqual(len(children), 2)

    def test_missing_windows_parent_for_command_entry(self):
        self.exercise_entry('test_identical_units_keep_each_command_producer_and_receipts', True)

    def test_missing_windows_parent_for_bootstrap_entry(self):
        self.exercise_entry('test_windows_bootstrap_without_tracked_wrapper_source', True)

    def test_unix_keeps_default_temporary_directory_for_both_entries(self):
        for entry in ('test_identical_units_keep_each_command_producer_and_receipts',
                      'test_windows_bootstrap_without_tracked_wrapper_source'):
            with self.subTest(entry=entry):
                self.exercise_entry(entry, False)


if __name__ == '__main__':
    unittest.main()
