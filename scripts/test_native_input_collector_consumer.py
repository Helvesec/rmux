"""Frozen F1/F2 acceptance at the runner and consumer first-read boundaries."""
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import candidate_identity as guard
import candidate_identity_consumers as consumers
import native_input_windows as native
from native_input_collector import Snapshot


class PythonEntrypoints(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent.parent / '.rmux-audit')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.script = Path(guard.__file__).resolve()
        self.probe = self.root / 'probe.py'
        self.probe.write_text('import json,sys; print(json.dumps(dict(argv=sys.argv, isolated=sys.flags.isolated, no_site=sys.flags.no_site)))\n')
        self.sentinel = self.root / 'startup-consumed'
        (self.root / 'sitecustomize.py').write_text(
            'raise RuntimeError("unqualified startup content must never execute")\n')

    def assert_startup(self, argv):
        self.assertEqual(argv[1:4], ['-I', '-S', '-B'])
        # Substitute only the already qualified script, preserving actual flags.
        result = subprocess.run([*argv[:4], str(self.probe), *argv[5:]],
                                env=dict(os.environ, PYTHONPATH=str(self.root)),
                                capture_output=True, text=True, check=True)
        data = json.loads(result.stdout)
        self.assertEqual((data['isolated'], data['no_site']), (1, 1))
        self.assertFalse(self.sentinel.exists())

    def test_cargo_runner_actual_argv_is_isolated_only_for_windows_manifest(self):
        spec = importlib.util.spec_from_file_location('runner_guard', self.script)
        g = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(g)
        g.os = SimpleNamespace(**vars(os))
        g.os.name = 'nt'
        for manifest in (False, True):
            with self.subTest(manifest=manifest), ExitStack() as stack:
                target = self.root / ('manifest' if manifest else 'ordinary')
                env = {'RMUX_NATIVE_INPUT_MANIFEST': 'qualified'} if manifest else {}
                stack.enter_context(patch.dict(os.environ, env, clear=True))
                stack.enter_context(patch.object(g, 'git', side_effect=lambda _, *a: str(self.root) if a[-1] == '--show-toplevel' else 'a' * 40))
                stack.enter_context(patch.object(g, 'source_identity', return_value=(self.root, 'b' * 64)))
                stack.enter_context(patch.object(g, 'target_directory', return_value=target))
                stack.enter_context(patch.object(g, 'host_os', return_value='windows'))
                stack.enter_context(patch.object(g.shutil, 'disk_usage', return_value=SimpleNamespace(free=100 * 2**30)))
                stack.enter_context(patch.object(g.shutil, 'which', return_value=sys.executable))
                stack.enter_context(patch.object(g.subprocess, 'run'))
                stack.enter_context(patch.object(g.subprocess, 'check_output', side_effect=lambda args, **_:
                    # No other toolchain binary stands behind the fake compiler: it keeps its spelling.
                    str(Path(sys.executable).parent.parent) + '\n' if args[1:] == ['--print', 'sysroot']
                    else 'host: test-native\n'))
                windows = g.support('candidate_identity_windows')
                stack.enter_context(patch.object(g, 'support', side_effect={
                    'candidate_identity_manifest': g.support('candidate_identity_manifest'),
                    'candidate_identity_context': g.support('candidate_identity_context'),
                    # prepare() selects the compiler through the Windows module (bd934f297).
                    'candidate_identity_windows': SimpleNamespace(install=lambda *a: None,
                                                                  selected_compiler=windows.selected_compiler),
                }.__getitem__))
                prepared = g.prepare()
                argv = shlex.split(prepared['CARGO_TARGET_TEST_NATIVE_RUNNER'])
            if manifest:
                self.assert_startup(argv)
            else:
                self.assertEqual(argv, [sys.executable, str(self.script), 'run'])

    def test_rustdoc_actual_runner_arguments_isolate_startup(self):
        source = self.script.parent.parent
        base = [sys.executable, '--test', str(source / 'crates/rmux-sdk/src/lib.rs')]
        with patch.object(guard, 'host_os', return_value='windows'), \
                patch.object(guard, 'context', return_value=(source, 'a' * 40, self.root)), \
                patch.object(guard, 'source_identity'), patch.object(guard, 'tracked_input', return_value='blob'), \
                patch.dict(os.environ, {'RMUX_NATIVE_INPUT_MANIFEST': 'qualified', 'RUSTC_WRAPPER': 'wrapper'}), \
                patch.object(guard.subprocess, 'call', return_value=0) as launch:
            guard.rustdoc_wrapper(base)
        args = launch.call_args.args[0]
        argv = [args[args.index('--test-runtool') + 1]]
        argv.extend(args[i + 1] for i, arg in enumerate(args) if arg == '--test-runtool-arg')
        self.assert_startup(argv)

    def test_rust_consumer_has_startup_flags_before_script(self):
        text = (self.script.parent.parent / 'tests/support/public_api_compile_fail.rs').read_text()
        body = text.split('fn identity_command(', 1)[1].split('fn qualify(', 1)[0]
        self.assertIn('RMUX_NATIVE_INPUT_MANIFEST', body)
        self.assertIn('["-I", "-S", "-B"]', body)
        self.assertLess(body.index('["-I", "-S", "-B"]'), body.index('command.arg(script)'))


class ConsumerRetention(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent.parent / '.rmux-audit')
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name).resolve()
        self.target = self.source / 'target'
        self.directory = self.target / 'consumer-inputs' / 'case'
        self.code = self.directory / 'src/bin/case.rs'
        self.code.parent.mkdir(parents=True)
        self.code.write_text('fn main() {}\n')
        self.dependency = self.source / 'crates/rmux-sdk'
        self.dependency.mkdir(parents=True)
        (self.dependency / 'Cargo.toml').write_text('[package]\nname="rmux-sdk"\nversion="1.0.0"\n')
        self.manifest = self.directory / 'Cargo.toml'
        self.manifest.write_text('[dependencies]\nrmux-sdk={path=' + json.dumps(str(self.dependency)) + '}\n')
        self.lock = self.directory / 'Cargo.lock'
        self.seed = b'version=4\n[[package]]\nname="rmux-sdk"\nversion="1.0.0"\n'
        self.resolved = self.seed + b'[[package]]\nname="consumer"\nversion="0.0.0"\n'
        (self.source / 'Cargo.lock').write_bytes(self.seed)
        origin = self.source / 'tests/support/public_api_compile_fail.rs'
        origin.parent.mkdir(parents=True)
        origin.write_text('qualified producer origin')
        self.build = self.source / 'build.json'
        self.build.write_text(json.dumps({'input': str(self.dependency / 'tests/public_api_extensibility.rs')}))
        self.producer = dict(path=str(Path(sys.executable).resolve()), sha256='producer',
                             build=str(self.build), unit=dict(kind='rust-test'))
        self.audit = self.source / 'audit'
        self.audit.mkdir()
        self.tickets = []

    def assert_receipt(self, ticket, phase, lock_bytes):
        self.assertIn('native_inputs', ticket, 'no byte retention before consumer reads')
        receipt = ticket['native_inputs'][phase]
        self.assertEqual((receipt['head'], receipt['source'], receipt['session'], receipt['generator_sha256']),
                         ('a' * 40, str(self.source), ticket['session'], 'producer'))
        self.assertEqual(receipt['phase'], phase)
        expected = {str(self.manifest), str(self.code), str(self.lock)}
        self.assertEqual(set(receipt['files']), expected)
        for path, row in receipt['files'].items():
            self.assertEqual(row['resolved'], str(Path(path).resolve()))
            self.assertEqual(Path(row['object']).read_bytes(), lock_bytes if path == str(self.lock) else Path(path).read_bytes())
        self.assertTrue(Path(receipt['receipt']).is_file())

    def run_action(self, mutate=None):
        original_validate = consumers.validate
        def validation(g, ticket, source, head, target, require_lock=True):
            self.assert_receipt(ticket, 'resolved' if require_lock else 'initial', self.resolved if require_lock else self.seed)
            self.tickets.append(ticket)
            return original_validate(g, ticket, source, head, target, require_lock)
        def metadata(argv, **kwargs):
            ticket = json.loads(next((self.target / 'identity-generators').glob('*.json')).read_text())
            self.assert_receipt(ticket, 'initial', self.seed)
            self.assertEqual(kwargs['cwd'], self.directory)
            self.lock.write_bytes(self.resolved)
            return 0
        stack = ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(patch.dict(os.environ, {'RMUX_NATIVE_INPUT_MANIFEST': 'qualified',
            'RMUX_NATIVE_INPUT_MANIFEST_SHA256': 'manifest-sha', 'CARGO': sys.executable,
            'RMUX_IDENTITY_RUSTC': sys.executable}))
        stack.enter_context(patch.object(guard, 'host_os', return_value='windows'))
        stack.enter_context(patch.object(guard, 'context', return_value=(self.source, 'a' * 40, self.target)))
        stack.enter_context(patch.object(guard, 'source_identity'))
        stack.enter_context(patch.object(guard, 'verify_built', return_value=self.producer))
        stack.enter_context(patch.object(guard, 'tracked_input', return_value='blob'))
        stack.enter_context(patch.object(consumers, 'parent_executable', return_value=Path(sys.executable)))
        stack.enter_context(patch.object(native, 'verify_manifest', return_value=dict(audit=str(self.audit), reserve_bytes=0,
            head='a' * 40, source=str(self.source), case='public-api', cell='sdk')))
        stack.enter_context(patch.object(native, 'new_snapshot', side_effect=lambda path, _: Snapshot(path)))
        stack.enter_context(patch.object(consumers.subprocess, 'call', side_effect=metadata))
        stack.enter_context(patch.object(consumers, 'validate', side_effect=validation))
        consumers.action(['prepare', str(self.directory), str(self.dependency), 'rmux-sdk'], guard)
        ticket = self.tickets[-1]
        if mutate:
            mutate(ticket)
        with patch.object(guard, 'generation', return_value=ticket), \
                patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.directory / 'target')}):
            consumers.action(['check'], guard)
        return ticket

    def test_initial_and_resolved_bytes_exist_before_each_first_read(self):
        ticket = self.run_action()
        self.assert_receipt(ticket, 'initial', self.seed)
        self.assert_receipt(ticket, 'resolved', self.resolved)
        self.assertNotEqual(ticket['native_inputs']['initial']['files'][str(self.lock)]['object'],
                            ticket['native_inputs']['resolved']['files'][str(self.lock)]['object'])

    def test_missing_copy_is_refused_before_check(self):
        def remove(ticket):
            Path(ticket['native_inputs']['resolved']['files'][str(self.manifest)]['object']).unlink()
        with self.assertRaises((guard.IdentityError, RuntimeError, FileNotFoundError)):
            self.run_action(remove)

    def test_same_byte_replacement_is_refused_before_check(self):
        def replace(ticket):
            replacement = self.directory / 'replacement'
            replacement.write_bytes(self.manifest.read_bytes())
            replacement.replace(self.manifest)
        with self.assertRaises((guard.IdentityError, RuntimeError)):
            self.run_action(replace)

    def test_changed_source_is_refused_before_check(self):
        with self.assertRaises((guard.IdentityError, RuntimeError)):
            self.run_action(lambda ticket: self.code.write_text('fn main() { panic!(); }\n'))


if __name__ == '__main__':
    unittest.main()
