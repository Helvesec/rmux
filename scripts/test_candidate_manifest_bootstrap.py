"""Standalone bootstrap must bind compiler graphs before preparing wrappers."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_candidate_identity_manifest as fixtures


class StandaloneBootstrapRegression(unittest.TestCase):
    def test_windows_wrapper_bootstrap_without_default_git_search_path(self):
        real_which = fixtures.shutil.which

        def which(name, *args, **kwargs):
            # Unix may find Git on its default search path after PATH is erased;
            # native Windows does not. Keep the qualified pre-clear lookup real.
            if name == 'git' and not os.environ.get('PATH'):
                return None
            return real_which(name, *args, **kwargs)

        with patch.object(fixtures.shutil, 'which', which):
            self.test_windows_wrapper_graph_starts_with_standalone_identity()

    def test_windows_wrapper_graph_starts_with_standalone_identity(self):
        fixture = fixtures.StandaloneManifestIdentity()
        fixture.setUp()
        native_code = fixture.repo / 'scripts/candidate_identity_windows.rs'
        if not native_code.exists():
            native_code.parent.mkdir()
            native_code.write_bytes(fixtures.SCRIPT.with_name(native_code.name).read_bytes())
            fixture.commit()
        spec = importlib.util.spec_from_file_location('bootstrap_identity', fixtures.SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        guard.os = SimpleNamespace(**vars(os))
        guard.os.name = 'nt'
        guard.host_os = lambda: 'windows'
        native = guard.support('candidate_identity_windows')
        units = guard.support('candidate_identity_units')
        manifest = guard.support('candidate_identity_manifest')
        compiler = str(Path(sys.executable).resolve())
        real_run, real_output, real_which = subprocess.run, subprocess.check_output, guard.shutil.which
        git = real_which('git')
        self.assertIsNotNone(git)
        git = str(Path(git).resolve(strict=True))
        observed = []

        def which(name):
            if name == 'git':
                return git
            return compiler if name in ('rustc', 'rustdoc') else real_which(name)

        def output(args, **kwargs):
            if args == [compiler, '-vV']:
                return 'host: fixture-native\n'
            if args == [compiler, '--print', 'sysroot']:
                # No other toolchain binary stands behind the fake compiler: it keeps its spelling.
                return str(Path(compiler).parent.parent) + '\n'
            return real_output(args, **kwargs)

        def bootstrap(args, **kwargs):
            if args[0] != compiler:
                return real_run(args, **kwargs)
            self.assertIn('--emit=obj', args)
            Path(args[args.index('-o') + 1]).write_bytes(b'bootstrap object')

        def install(g, env, source, head, target):
            unit = units.before(g, [compiler, str(native_code), '--crate-name', 'wrapper',
                                   '--crate-type', 'bin', '-o', str(target / 'wrapper.exe')],
                                source, head, target, env)
            expected = manifest.describe(g, fixture.manifest, source, head)
            self.assertEqual(unit['graph'].get('compilation_unit'), expected)
            self.assertEqual(unit['graph']['lock_sha256'], g.file_hash(fixture.lock))
            self.assertEqual(unit['graph']['input']['input_sha256'], g.file_hash(native_code))
            unit['depinfo'].parent.mkdir(parents=True, exist_ok=True)
            unit['depinfo'].write_text(str(native_code) + ':\n')
            units.check_depinfo(g, unit, source, head, target)
            unit['depinfo'].write_text(str(native_code) + ':\n' + str(source / 'main.rs') + ':\n')
            with self.assertRaisesRegex(g.IdentityError, 'outside its standalone unit'):
                units.check_depinfo(g, unit, source, head, target)
            raw = native_code.read_bytes()
            native_code.write_bytes(raw + b'// changed\n')
            with self.assertRaisesRegex(g.IdentityError, 'differs from pinned tree'):
                units.source_proof(g, native_code, source, head, target, env)
            native_code.write_bytes(raw)
            observed.append(unit['graph'])
            for key, name in (('RUSTC_WRAPPER', 'rustc-wrapper.exe'),
                              ('RUSTDOC', 'rustdoc-wrapper.exe')):
                env[key] = str(target / name)

        previous = Path.cwd()
        try:
            os.chdir(fixture.repo)
            with patch.dict(os.environ, {}, clear=True), \
                    patch.object(guard.shutil, 'which', which), \
                    patch.object(guard.shutil, 'disk_usage',
                                 return_value=SimpleNamespace(free=100 * 1024**3)), \
                    patch.object(subprocess, 'run', bootstrap), \
                    patch.object(subprocess, 'check_output', output), \
                    patch.object(native, 'install', install), \
                    patch.object(subprocess, 'call', return_value=0):
                self.assertEqual(manifest.action(guard, [str(fixture.manifest), '--', 'scenario']), 0)
        finally:
            os.chdir(previous)
        self.assertEqual(len(observed), 1)

    def test_standalone_fixture_tracks_native_windows_wrapper(self):
        fixture = fixtures.StandaloneManifestIdentity()
        fixture.setUp()
        native = fixture.repo / 'scripts/candidate_identity_windows.rs'
        fixture.g.tracked_input(native, fixture.repo, fixture.head)
        self.assertEqual(native.read_bytes(), fixtures.SCRIPT.with_name(native.name).read_bytes())


if __name__ == '__main__':
    unittest.main()
