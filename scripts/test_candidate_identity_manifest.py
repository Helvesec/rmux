"""Standalone compilation keeps its own tracked lock and the full identity guard."""
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from types import SimpleNamespace

SCRIPT = Path(__file__).with_name('candidate_identity.py').resolve()


class StandaloneManifestIdentity(unittest.TestCase):
    def setUp(self):
        audit = SCRIPT.parent.parent / '.rmux-audit' / 'manifest-identity-tests'
        audit.mkdir(parents=True, exist_ok=True)
        self.repo = Path(tempfile.mkdtemp(dir=audit)).resolve()
        spec = importlib.util.spec_from_file_location('identity', SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.g = SimpleNamespace(**vars(module))
        self.units = module.support('candidate_identity_units')
        (self.repo / '.gitignore').write_text('.rmux-audit/\n')
        (self.repo / 'Cargo.toml').write_text('[workspace]\n')
        (self.repo / 'Cargo.lock').write_text('version = 4\n[[package]]\n'
                                            'name="workspace"\nversion="1.0.0"\n')
        (self.repo / 'main.rs').write_text('fn main() {}\n')
        native = self.repo / 'scripts/candidate_identity_windows.rs'
        native.parent.mkdir()
        shutil.copyfile(SCRIPT.with_name(native.name), native)
        self.manifest = self.repo / 'tool/Cargo.toml'
        self.manifest.parent.mkdir()
        self.manifest.write_text('[package]\nname="tool"\nversion="1.0.0"\n[workspace]\n')
        self.code = self.manifest.parent / 'main.rs'
        self.code.write_text('fn main() {}\n')
        self.registry = Path(tempfile.mkdtemp(dir=audit)).resolve() / 'cargo/registry'
        self.package = self.registry / 'src/fixture/dep-1.0.0'
        self.package.mkdir(parents=True)
        for name, raw in {'Cargo.toml': b'[package]\nname="dep"\nversion="1.0.0"\n',
                          'lib.rs': b'pub fn dependency() {}\n'}.items():
            (self.package / name).write_bytes(raw)
        self.archive = self.registry / 'cache/fixture/dep-1.0.0.crate'
        self.archive.parent.mkdir(parents=True)
        with tarfile.open(self.archive, 'w:gz') as packed:
            for path in self.package.iterdir():
                info = tarfile.TarInfo('dep-1.0.0/' + path.name)
                raw = path.read_bytes()
                info.size = len(raw)
                packed.addfile(info, io.BytesIO(raw))
        self.lock = self.manifest.with_name('Cargo.lock')
        self.lock.write_text('version = 4\n[[package]]\nname="dep"\nversion="1.0.0"\n'
                             'source="registry+https://github.com/rust-lang/crates.io-index"\n'
                             f'checksum="{self.g.file_hash(self.archive)}"\n')
        self.git('init', '-q')
        self.commit()

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], text=True).strip()

    def commit(self):
        self.git('add', '.')
        self.git('-c', 'user.name=Identity Test', '-c', 'user.email=identity@example.invalid',
                 'commit', '-qm', 'fixture')
        self.head = self.git('rev-parse', 'HEAD')

    def prepare(self, manifest=None):
        probe = ('import os,json; print(json.dumps({k:v for k,v in os.environ.items() '
                 'if k.startswith("RMUX_IDENTITY_") or k=="CARGO_TARGET_DIR"}))')
        result = subprocess.run([sys.executable, str(SCRIPT), 'with-manifest',
                                 str(manifest or self.manifest), '--', sys.executable, '-c', probe],
                                cwd=self.repo, env=self.fresh_environment(),
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        env = json.loads(result.stdout)
        return env, Path(env['CARGO_TARGET_DIR'])

    def fresh_environment(self):
        # Like the head/stamp fixtures, this repo owns its compiler and target;
        # inherited gate wrappers and runners belong to the outer candidate.
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()).resolve()
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')))
        for key in tuple(env):
            if (key.startswith('RMUX_IDENTITY_')
                    or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER'))
                    or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')):
                env.pop(key)
        return env

    def before(self, env, target, code=None):
        return self.units.before(self.g, [env['RMUX_IDENTITY_RUSTC'], str(code or self.code),
                                 '--crate-name', 'tool', '--crate-type', 'bin',
                                 '--out-dir', str(target / 'debug')],
                                 self.repo, self.head, target, env=dict(env,
                                 CARGO_MANIFEST_DIR=str((code or self.code).parent)))

    def test_private_target_graph_uses_own_manifest_and_lock(self):
        env, target = self.prepare()
        self.assertEqual(target, self.g.target_directory(self.repo, self.head, target.name))
        unit = self.before(env, target)
        self.assertEqual(unit['graph']['lock_sha256'], self.g.file_hash(self.lock))
        binding = unit['graph']['compilation_unit']
        self.assertEqual(binding['manifest'], str(self.manifest))
        self.assertEqual(binding['manifest_sha256'], self.g.file_hash(self.manifest))
        self.assertEqual(binding['head'], self.head)
        self.assertEqual(binding['lock'], str(self.lock))

    def test_registry_sources_use_own_lock_and_reject_mutation(self):
        env, target = self.prepare()
        env['CARGO_MANIFEST_DIR'] = str(self.package)
        code = self.package / 'lib.rs'
        proof = self.units.source_proof(self.g, code, self.repo, self.head, target, env)
        self.assertEqual(proof['archive_sha256'], self.g.file_hash(self.archive))
        for path in (self.archive, code):
            raw = path.read_bytes()
            path.write_bytes(raw + b'changed')
            with self.subTest(path=path), self.assertRaises(self.g.IdentityError):
                self.units.source_proof(self.g, code, self.repo, self.head, target, env)
            path.write_bytes(raw)

    def test_workspace_lock_does_not_accept_standalone_only_dependency(self):
        target = self.g.target_directory(self.repo, self.head, '0' * 32)
        target.mkdir(parents=True)
        with self.assertRaisesRegex(self.g.IdentityError, 'not pinned'):
            self.units.source_proof(self.g, self.package / 'lib.rs', self.repo,
                                    self.head, target, {'CARGO_MANIFEST_DIR': str(self.package)})

    def test_binding_and_lock_mutation_are_rejected(self):
        env, target = self.prepare()
        ticket = target / 'identity-compilation-unit.json'
        raw = ticket.read_bytes()
        record = json.loads(raw)
        record['lock'] = str(self.repo / 'Cargo.lock')
        ticket.write_text(json.dumps(record))
        with self.assertRaises(self.g.IdentityError):
            self.before(env, target)
        ticket.write_bytes(raw)
        self.lock.write_text(self.lock.read_text() + '# changed\n')
        with self.assertRaises(self.g.IdentityError):
            self.before(env, target)

    def test_standalone_context_rejects_workspace_sibling(self):
        env, target = self.prepare()
        with self.assertRaises(self.g.IdentityError):
            self.before(env, target, self.repo / 'main.rs')

    def test_nonstandalone_and_foreign_manifest_are_rejected(self):
        for manifest in (self.repo / 'Cargo.toml', SCRIPT.parent.parent / 'Cargo.toml'):
            result = subprocess.run([sys.executable, str(SCRIPT), 'with-manifest', str(manifest),
                                     '--', sys.executable, '-c', 'print("scenario reached")'],
                                    cwd=self.repo, env=self.fresh_environment(),
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 86)
            self.assertEqual(result.stdout, '')


if __name__ == '__main__':
    unittest.main()
