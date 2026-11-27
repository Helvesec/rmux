"""INT589-F2: inert fixtures for two original locks in one identity target.

No compiler, Cargo, identity prepare, native image or external process is run
apart from Git over the fixture. This regression is frozen before the fix.
"""
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest

SCRIPT = Path(__file__).with_name('candidate_identity.py').resolve()


class CommonContextIdentity(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('identity', SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.g = SimpleNamespace(**vars(module))
        self.units = module.support('candidate_identity_units')
        audit = SCRIPT.parent.parent / '.rmux-audit/multimanifest-tests'
        audit.mkdir(parents=True, exist_ok=True)
        self.fixture = Path(tempfile.mkdtemp(dir=audit)).resolve()
        self.repo = self.fixture / 'source'
        self.repo.mkdir()
        (self.repo / '.gitignore').write_text('.rmux-audit/\n')
        (self.repo / 'Cargo.toml').write_text('[package]\nname="root"\nversion="1.0.0"\n[workspace]\n')
        (self.repo / 'Cargo.lock').write_text('version=4\n[[package]]\nname="root"\nversion="1.0.0"\n')
        (self.repo / 'main.rs').write_text('fn main() {}\n')
        self.manifest = self.repo / 'scripts/source-inventory/Cargo.toml'
        self.manifest.parent.mkdir(parents=True)
        self.manifest.write_text('[package]\nname="parser"\nversion="1.0.0"\n[workspace]\n'
                                 '[dependencies]\nzmij="=1.0.23"\n')
        self.code = self.manifest.with_name('main.rs')
        self.code.write_text('fn main() {}\n')
        self.package = self.fixture / 'cargo/registry/src/fixture/zmij-1.0.23'
        self.package.mkdir(parents=True)
        (self.package / 'Cargo.toml').write_text('[package]\nname="zmij"\nversion="1.0.23"\n')
        (self.package / 'lib.rs').write_text('pub fn dependency() {}\n')
        archive = self.fixture / 'cargo/registry/cache/fixture/zmij-1.0.23.crate'
        archive.parent.mkdir(parents=True)
        with tarfile.open(archive, 'w:gz') as packed:
            for path in sorted(self.package.iterdir()):
                data = path.read_bytes()
                info = tarfile.TarInfo('zmij-1.0.23/' + path.name)
                info.size = len(data)
                packed.addfile(info, io.BytesIO(data))
        self.lock = self.manifest.with_name('Cargo.lock')
        self.lock.write_text('version=4\n[[package]]\nname="parser"\nversion="1.0.0"\n'
                             'dependencies=["zmij"]\n[[package]]\nname="zmij"\nversion="1.0.23"\n'
                             'source="registry+https://github.com/rust-lang/crates.io-index"\n'
                             f'checksum="{self.g.file_hash(archive)}"\n')
        self.git('init', '-q')
        self.git('add', '.')
        self.git('-c', 'user.name=Sidney Sissaoui', '-c', 'user.email=shideneyu@gmail.com',
                 'commit', '-qm', 'inert multi-manifest fixture')
        self.head = self.git('rev-parse', 'HEAD')
        self.target = self.g.target_directory(self.repo, self.head, '0' * 32)
        self.target.mkdir(parents=True)
        # Merely hashed as a fixture compiler identity; never executed.
        self.compiler = str(Path(sys.executable).resolve())

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], text=True).strip()

    def before(self, code):
        return self.units.before(self.g, [self.compiler, str(code), '--crate-name', 'fixture',
                                 '--crate-type', 'lib', '--out-dir', str(self.target / 'debug')],
                                 self.repo, self.head, self.target,
                                 env={'RMUX_IDENTITY_RUSTC': self.compiler,
                                      'CARGO_MANIFEST_DIR': str(code.parent)})

    def test_parser_and_root_keep_original_locks_in_one_target(self):
        root = self.before(self.repo / 'main.rs')
        parser = self.before(self.code)
        self.assertEqual(root['graph']['lock_sha256'], self.g.file_hash(self.repo / 'Cargo.lock'))
        self.assertEqual(parser['graph']['lock_sha256'], self.g.file_hash(self.lock))
        self.assertEqual(parser['graph']['compilation_unit']['manifest'], str(self.manifest))
        for unit in (root, parser):
            self.assertEqual(unit['graph']['head'], self.head)
            self.assertEqual(unit['graph']['source'], str(self.repo))
            self.assertTrue(all(p.is_relative_to(self.target) for p in unit['outputs']))
        self.assertFalse((self.target / 'identity-compilation-unit.json').exists())

    def test_parser_only_pinned_dependency_in_common_context(self):
        proof = self.units.source_proof(self.g, self.package / 'lib.rs', self.repo,
                                        self.head, self.target,
                                        {'CARGO_MANIFEST_DIR': str(self.package)})
        self.assertEqual(proof['package'], 'zmij')
        self.assertEqual(proof['version'], '1.0.23')


if __name__ == '__main__':
    unittest.main()
