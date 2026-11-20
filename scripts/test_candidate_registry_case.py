"""Qualify locked archives using the native filesystem's extraction semantics."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest


class RegistryCase(unittest.TestCase):
    def test_case_colliding_archive_entries_and_modified_neighbor(self):
        spec = importlib.util.spec_from_file_location(
            'identity', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        units = guard.support('candidate_identity_units')
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            source = base / 'candidate'
            source.mkdir()
            root = base / 'cargo/registry/src/index/example-1.0.0'
            root.mkdir(parents=True)
            archive = base / 'cargo/registry/cache/index/example-1.0.0.crate'
            archive.parent.mkdir(parents=True)
            entries = {
                'Cargo.toml': b'[package]\nname="example"\nversion="1.0.0"\n',
                'src/lib.rs': b'pub fn example() {}\n',
                'README.md': b'first archive entry\n',
                'Readme.md': b'last archive entry\n',
            }
            with tarfile.open(archive, 'w') as packed:
                for name, content in entries.items():
                    entry = tarfile.TarInfo(root.name + '/' + name)
                    entry.size = len(content)
                    packed.addfile(entry, io.BytesIO(content))
                    # Cargo extracts in archive order on the native filesystem.
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(content)
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            (source / 'Cargo.lock').write_text(
                '[[package]]\nname="example"\nversion="1.0.0"\n'
                'source="registry+https://example.invalid/index"\n'
                f'checksum="{checksum}"\n')
            args = (guard, root / 'src/lib.rs', source, 'a' * 40,
                    source / 'target', {'CARGO_MANIFEST_DIR': str(root)})
            proof = units.source_proof(*args)
            self.assertEqual(proof['archive_sha256'], checksum)
            for name in ('Readme.md', 'src/lib.rs'):
                with self.subTest(modified=name):
                    path = root / name
                    original = path.read_bytes()
                    path.write_bytes(b'foreign bytes\n')
                    with self.assertRaises(guard.IdentityError):
                        units.source_proof(*args)
                    path.write_bytes(original)


if __name__ == '__main__':
    unittest.main()
