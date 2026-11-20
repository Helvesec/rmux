"""A generated consumer must be qualified without relaxing foreign-input guards."""
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import candidate_identity as guard


class GeneratedConsumer(unittest.TestCase):
    def test_observed_consumer_input_is_accepted_in_its_private_output(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            target = source / '.rmux-audit' / 'candidate-target' / 'head' / 'os'
            consumer = target / 'consumer-inputs' / 'observed'
            code = consumer / 'src' / 'bin' / 'case.rs'
            code.parent.mkdir(parents=True)
            code.write_text('fn main() {}\n')
            manifest = consumer / 'Cargo.toml'
            manifest.write_text('[package]\nname="consumer"\nversion="0.0.0"\n')
            lock = consumer / 'Cargo.lock'
            lock.write_text('version = 4\n')
            output = consumer / 'target'
            output.mkdir()
            ticket = dict(kind='compile-consumer', directory=str(consumer),
                          files={p.relative_to(consumer).as_posix():guard.file_hash(p)
                                 for p in [code, manifest, lock]},
                          manifest=str(manifest), lock=str(lock),
                          producer='observed-rust-test', producer_sha256='observed')
            env = dict(RMUX_IDENTITY_DOC_SESSION='a' * 64,
                       CARGO_MANIFEST_DIR=str(consumer))
            with patch.dict(os.environ, env), patch.object(guard, 'git', return_value='head'), \
                    patch.object(guard, 'generation', return_value=ticket):
                guard.compiler_inputs(['rustc', str(code), '--out-dir', str(output)],
                                      source, target)


if __name__ == '__main__':
    unittest.main()
