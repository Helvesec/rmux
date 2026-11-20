"""R1 F1: ignored files inside a checkout do not belong to its commit."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


class TrackedSource(unittest.TestCase):
    def test_ignored_source_and_manifest_are_refused_before_compiler(self):
        spec = importlib.util.spec_from_file_location('identity', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            (source / 'main.rs').write_text('fn main() {}\n')
            (source / '.gitignore').write_text('.rmux-audit/\n')
            subprocess.run(['git', '-C', str(source), 'init', '-q'], check=True)
            subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
            subprocess.run(['git', '-C', str(source), '-c', 'user.name=Identity Test', '-c',
                            'user.email=identity@example.invalid', 'commit', '-qm', 'candidate'], check=True)
            head = guard.git(source, 'rev-parse', 'HEAD')
            target = guard.target_directory(source, head)
            target.mkdir(parents=True)
            copied = target / 'copied.rs'
            copied.write_text('fn main() { println!("FOREIGN_SOURCE_EXECUTED"); }\n')
            (target / 'Cargo.toml').write_text('[package]\nname="foreign"\nversion="0.1.0"\n')
            for code, manifest in ((copied, None), (source / 'main.rs', target)):
                env = {} if manifest is None else {'CARGO_MANIFEST_DIR': str(manifest)}
                with self.subTest(code=code, manifest=manifest), patch.dict(os.environ, env, clear=True):
                    with self.assertRaises(guard.IdentityError):
                        guard.compiler_inputs(['rustc', '--crate-type', 'bin', str(code),
                                               '-o', str(target / 'out')], source, target)


if __name__ == '__main__':
    unittest.main()
