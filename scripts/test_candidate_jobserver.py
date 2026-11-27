"""The wrapper must retain Cargo's actual jobserver descriptors."""
import importlib.util
import os
import select
from pathlib import Path
import sys
import unittest
from unittest.mock import patch


@unittest.skipIf(os.name == 'nt', 'Windows jobserver uses named synchronization')
class Jobserver(unittest.TestCase):
    def test_compiler_subprocess_inherits_jobserver_descriptors(self):
        spec = importlib.util.spec_from_file_location('identity', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        source = Path(__file__).resolve().parent.parent
        head = '0' * 40
        target = guard.target_directory(source, head)
        # The stand-in compiler is bound as the context's compiler, as `with` binds
        # rustc, and named by its real path: a link would be asked for a sysroot.
        compiler = os.path.realpath(sys.executable)
        read, write = os.pipe()
        try:
            # Only the inherited read end can consume the parent's token.
            os.write(write, b'J')
            with patch.object(guard, 'context', return_value=(source, head, target)), \
                    patch.object(guard, 'source_identity'), \
                    patch.dict(os.environ, {'CARGO_MAKEFLAGS': f'--jobserver-auth={read},{write}',
                                            'RMUX_IDENTITY_RUSTC': compiler}):
                self.assertEqual(guard.rustc_wrapper([compiler, '-c',
                    f'import os, select; '
                    f'assert select.select([{read}], [], [], 5)[0], "jobserver input timeout"; '
                    f'assert os.read({read}, 1) == b"J"; '
                    f'os.write({write}, b"K")']), 0)
                # A child-local replacement pipe cannot deliver the reply here.
                self.assertTrue(select.select([read], [], [], 5)[0], 'jobserver reply timeout')
                self.assertEqual(os.read(read, 1), b'K')
        finally:
            os.close(read)
            os.close(write)


if __name__ == '__main__':
    unittest.main()
