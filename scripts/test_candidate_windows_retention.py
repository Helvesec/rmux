"""Real compiler artifacts survive replacement; portable Windows storage fixture."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class WindowsUnitRetention(unittest.TestCase):
    def test_historical_macro_and_metadata_survive_final_compile_and_probe_removal(self):
        spec = importlib.util.spec_from_file_location(
            'identity', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        units = guard.support('candidate_identity_units')
        audit = Path(__file__).resolve().parent.parent / '.rmux-audit' / 's2-037' / 'acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='unit-retention-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        (source / 'Cargo.toml').write_text('[package]\nname="retention"\nversion="0.1.0"\n')
        macro = source / 'macro.rs'
        macro.write_text('extern crate proc_macro;\n#[proc_macro]\npub fn retained(_: proc_macro::TokenStream) -> proc_macro::TokenStream {\n'
                         'if cfg!(first) { "pub const VALUE: u8 = 1;" } else { "pub const VALUE: u8 = 2;" }.parse().unwrap()\n}\n')
        consumer = source / 'consumer.rs'
        consumer.write_text('extern crate retention_macro; retention_macro::retained!();\n')
        git = ['git', '-C', str(source)]
        subprocess.run(git + ['init', '-q'], check=True)
        subprocess.run(git + ['add', '.'], check=True)
        subprocess.run(git + ['-c', 'user.name=Identity Test', '-c',
                             'user.email=identity@example.invalid', 'commit', '-qm', 'fixture'], check=True)
        head = guard.git(source, 'rev-parse', 'HEAD')
        target = source / '.rmux-audit' / 'target'
        target.mkdir(parents=True)
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()).resolve()
        # A native dylib on Unix exercises the same storage boundary; only the
        # publication policy is Windows. On native Windows these are actual PE DLL bytes.
        suffix = '.dll' if os.name == 'nt' else '.dylib' if guard.host_os() == 'macos' else '.so'
        library = target / ('libretention_macro' + suffix)
        metadata = target / 'libconsumer.rmeta'
        env = dict(os.environ, RUSTC=str(rustc), CARGO_MANIFEST_DIR=str(source))
        native = SimpleNamespace(name='nt', replace=os.replace, getpid=os.getpid)

        def compile_unit(args):
            unit = units.before(guard, args, source, head, target, env)
            subprocess.run(units.with_depinfo(args, unit), check=True, capture_output=True)
            units.after(guard, unit, source, head, target)
            return units.verify(guard, unit['outputs'][0], source, head, target)

        macro_args = [str(rustc), '--crate-name', 'retention_macro', '--crate-type', 'proc-macro', str(macro), '-o', str(library)]
        consumer_args = [str(rustc), '--crate-name', 'consumer', '--crate-type', 'lib', '--emit=metadata',
                         str(consumer), '-o', str(metadata), '--extern', 'retention_macro=' + str(library)]
        with patch.object(units, 'os', native):
            first_macro = compile_unit(macro_args + ['--cfg', 'first'])
            first = compile_unit(consumer_args)
            original_macro, original_metadata = library.read_bytes(), metadata.read_bytes()
            second_macro = compile_unit(macro_args)
            second = compile_unit(consumer_args)
            self.assertNotEqual(library.read_bytes(), original_macro)
            self.assertNotEqual(metadata.read_bytes(), original_metadata)
            # The compiler's probe metadata is removed after the last compile.
            metadata.unlink()
            for record, expected in ((first_macro, original_macro), (first, original_metadata)):
                self.assertEqual(Path(record['path']).read_bytes(), expected,
                                 'historical compiler bytes still depend on a mutable alias')
                actual = units.verify(guard, record['path'], source, head, target,
                                      expected=record['sha256'], receipt=record['receipt'])
                self.assertEqual(actual['receipt_sha256'], record['receipt_sha256'])
            units.verify(guard, second['path'], source, head, target,
                         expected=second['sha256'], receipt=second['receipt'])
            # Current loading aliases must still be checked against real bytes.
            library.write_bytes(b'corrupted loading alias')
            with self.assertRaises(guard.IdentityError):
                units.before(guard, consumer_args, source, head, target, env)
            units.verify(guard, first['path'], source, head, target,
                         expected=first['sha256'], receipt=first['receipt'])
            retained = Path(first_macro['path'])
            retained.chmod(0o700)
            retained.write_bytes(b'corrupted retained macro')
            with self.assertRaises(guard.IdentityError):
                units.verify(guard, first['path'], source, head, target,
                             expected=first['sha256'], receipt=first['receipt'])


if __name__ == '__main__':
    unittest.main()
