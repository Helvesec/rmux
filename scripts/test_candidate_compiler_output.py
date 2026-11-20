"""Parallel Cargo compilers may only attribute their own named output."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


class CompilerOutput(unittest.TestCase):
    def test_parallel_sibling_output_is_not_attributed_to_this_invocation(self):
        spec = importlib.util.spec_from_file_location('identity', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            extension = '.exe' if os.name == 'nt' else ''
            own = root / ('own-123' + extension)
            other = root / ('other-456' + extension)
            for path in (own, other):
                path.write_bytes(b'output predicate fixture')
                path.chmod(0o755)
            args = ['rustc', '--crate-name', 'own', '--test', 'own.rs',
                    '-C', 'extra-filename=-123', '--out-dir', str(root)]
            self.assertEqual(set(guard.executable_outputs(args)), {own})


if __name__ == '__main__':
    unittest.main()
