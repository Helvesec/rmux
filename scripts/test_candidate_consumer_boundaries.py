"""Consumer mutations and foreign inputs remain refusals after qualification."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import candidate_identity as guard
import candidate_identity_consumers as consumers


class ConsumerBoundaries(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name).resolve()
        self.target = self.source / 'target'
        self.directory = self.target / 'consumer-inputs' / 'observed'
        self.directory.mkdir(parents=True)
        self.dependency = self.source / 'sdk'
        self.dependency.mkdir()
        (self.dependency / 'Cargo.toml').write_text('[package]\nname="rmux-sdk"\nversion="1.0.0"\n')
        (self.source / 'Cargo.lock').write_text('version=4\n[[package]]\nname="rmux-sdk"\nversion="1.0.0"\n')
        self.code = self.directory / 'src/bin/case.rs'
        self.code.parent.mkdir(parents=True)
        self.code.write_text('fn main() {}\n')
        (self.directory / 'Cargo.toml').write_text(
            '[dependencies]\nrmux-sdk={path=' + json.dumps(str(self.dependency)) + '}\n')
        (self.directory / 'Cargo.lock').write_bytes((self.source / 'Cargo.lock').read_bytes())
        self.compiler = self.source / 'rustc'
        self.compiler.write_text('compiler bytes')
        self.ticket = dict(commit='head', source=str(self.source),
            directory=str(self.directory), generator='test-producer',
            generator_sha256='producer-sha', dependency=str(self.dependency), package='rmux-sdk',
            phase='frozen', cargo=str(self.compiler), compiler=str(self.compiler),
            cargo_sha256=guard.file_hash(self.compiler), compiler_sha256=guard.file_hash(self.compiler),
            files={p.relative_to(self.directory).as_posix(): guard.file_hash(p)
                   for p in (self.code, self.directory / 'Cargo.toml', self.directory / 'Cargo.lock')})
        self.producer = dict(sha256='producer-sha', unit=dict(kind='rust-test'))

    def verify(self):
        with patch.object(guard, 'verify_built', return_value=self.producer), \
                patch.object(guard, 'tracked_input', return_value='pinned-blob'):
            consumers.validate(guard, self.ticket, self.source, 'head', self.target)

    def test_completed_observation_accepts_unchanged_inputs(self):
        self.verify()

    def test_changed_generated_source_is_refused(self):
        self.code.write_text('fn main() { panic!(); }\n')
        with self.assertRaisesRegex(guard.IdentityError, 'changed or unobserved'):
            self.verify()

    def test_changed_frozen_lock_is_refused(self):
        (self.directory / 'Cargo.lock').write_text('version=4\n')
        with self.assertRaisesRegex(guard.IdentityError, 'changed or unobserved'):
            self.verify()

    def test_marker_only_non_test_producer_is_refused(self):
        self.producer['unit']['kind'] = 'bin'
        with self.assertRaisesRegex(guard.IdentityError, 'observed Rust test'):
            self.verify()

    def test_foreign_target_is_refused(self):
        self.ticket['directory'] = str(self.source)
        with self.assertRaisesRegex(guard.IdentityError, 'private target'):
            self.verify()

    def test_unobserved_source_is_refused(self):
        foreign = self.directory / 'src/bin/foreign.rs'
        foreign.write_text('fn main() {}\n')
        with self.assertRaisesRegex(guard.IdentityError, 'changed or unobserved'):
            consumers.input_path(guard, self.ticket, foreign)

    def test_lock_resolution_is_not_a_completed_compilation(self):
        self.ticket['phase'] = 'resolving'
        with self.assertRaisesRegex(guard.IdentityError, 'resolved and frozen'):
            self.verify()

    def test_replaced_compiler_is_refused(self):
        self.compiler.write_text('replaced compiler bytes')
        with self.assertRaisesRegex(guard.IdentityError, 'tool bytes changed'):
            self.verify()

    def test_observed_lock_cannot_introduce_an_unpinned_registry_package(self):
        lock = self.directory / 'Cargo.lock'
        lock.write_text('version=4\n[[package]]\nname="foreign"\nversion="9.0.0"\n'
                        'source="registry+https://example.invalid"\nchecksum="unobserved"\n')
        self.ticket['files']['Cargo.lock'] = guard.file_hash(lock)
        with self.assertRaisesRegex(guard.IdentityError, 'absent from candidate lock'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
