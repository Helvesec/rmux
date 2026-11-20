"""Foreign libraries must not inherit the identity of a tracked final source."""
import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch


class TransitiveInputs(unittest.TestCase):
    def test_foreign_library_and_unqualified_extern_are_refused_before_compiler(self):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        source = script.parent.parent
        head = guard.git(source, 'rev-parse', 'HEAD')
        target = source / '.rmux-audit/transitive-unit'
        target.mkdir(parents=True, exist_ok=True)
        (target / 'identity.o').write_bytes(b'compiler mock identity')
        foreign = target / 'foreign.rs'
        foreign.write_text('pub fn foreign() {}\n')
        library = target / 'libforeign.rlib'
        library.write_bytes(b'unqualified library')
        env = dict(RMUX_IDENTITY_RUSTC=sys.executable, RUSTC=sys.executable,
                   CARGO_MANIFEST_DIR=str(source / 'crates/rmux-proto'))
        calls = [
            [sys.executable, '--crate-name', 'rmux_proto', '--crate-type', 'rlib',
             str(foreign), '-o', str(target / 'new-library.rlib')],
            [sys.executable, '--crate-name', 'protocol_registry', '--crate-type', 'bin',
             str(source / 'crates/rmux-proto/examples/protocol_registry.rs'),
             '--extern', 'rmux_proto=' + str(library), '-o', str(target / 'unbuilt-example')],
        ]
        for args in calls:
            with self.subTest(kind=args[4]), patch.object(guard, 'context', return_value=(source, head, target)), \
                    patch.object(guard, 'target_directory', return_value=target), \
                    patch.object(guard, 'source_identity'), patch.dict(os.environ, env), \
                    patch.object(guard.subprocess, 'call', return_value=0) as compiler:
                with self.assertRaises(guard.IdentityError):
                    guard.rustc_wrapper(args)
                compiler.assert_not_called()


if __name__ == '__main__':
    unittest.main()
