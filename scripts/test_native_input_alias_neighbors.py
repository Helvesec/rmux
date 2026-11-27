"""Neighbor checks for the four measured bindings and bounded object storage."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import native_input_collector as collector
from test_native_input_aliases import ALIASES, BOUNDARIES, CollectorFixture


class NativeInputAliasNeighbors(unittest.TestCase):
    def fixture(self):
        audit = Path(__file__).resolve().parents[1] / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(temporary.cleanup)
        return CollectorFixture(Path(temporary.name))

    def rejected(self, fixture, boundary, mutation):
        with self.assertRaises((RuntimeError, OSError)):
            fixture.run(before=boundary, mutate=mutation)
        self.assertEqual(fixture.calls, list(BOUNDARIES[:BOUNDARIES.index(boundary)]))
        self.assertFalse(fixture.manifest.exists())

    def test_wrong_initial_resolution_rejected_even_with_identical_bytes(self):
        for name in ALIASES:
            for same_bytes in (False, True):
                with self.subTest(alias=name, same_bytes=same_bytes):
                    fixture = self.fixture()
                    fixture.redirect(fixture.physical(name), same_bytes)
                    self.rejected(fixture, 'git-1', lambda: None)

    def test_alias_deletion_target_mutation_and_object_corruption(self):
        for name in ALIASES:
            for boundary in ('git-1', 'ldd-1', 'manifest'):
                for kind in ('deletion', 'target', 'object'):
                    with self.subTest(alias=name, boundary=boundary, kind=kind):
                        fixture = self.fixture()
                        alias = fixture.physical(name)
                        target = fixture.bindings[alias]

                        def mutate():
                            if kind == 'deletion':
                                alias.unlink()
                            elif kind == 'target':
                                target.write_bytes(b'changed target')
                            else:
                                obj = Path(fixture.snapshot.files[str(alias)]['object'])
                                obj.chmod(0o600)
                                obj.write_bytes(b'corrupted object')

                        self.rejected(fixture, boundary, mutate)
                        if kind == 'target':
                            obj = Path(fixture.snapshot.files[str(alias)]['object'])
                            self.assertEqual(obj.read_bytes(), fixture.data[str(target)])

    def test_absent_alias_receipts_are_separate_without_immutable_credit(self):
        fixture = self.fixture()
        for alias in fixture.bindings:
            alias.unlink()
        fixture.run()
        record = json.loads(fixture.manifest.read_text())
        expected = {str(alias) for alias in fixture.bindings}
        self.assertEqual(set(record['absent_observed_aliases']), expected)
        self.assertFalse(expected & record['files'].keys())
        self.assertFalse(expected & record['mutable_inputs'].keys())
        for alias, target in fixture.bindings.items():
            row = record['absent_observed_aliases'][str(alias)]
            self.assertEqual(row['expected_resolved'], str(target.resolve()))
            self.assertNotIn('object', row)

    def test_absent_alias_appearance_rejected_before_consumers_and_manifest(self):
        for name in ALIASES:
            for boundary in ('git-1', 'ldd-1', 'manifest'):
                for kind in ('symlink', 'regular', 'dangling'):
                    with self.subTest(alias=name, boundary=boundary, kind=kind):
                        fixture = self.fixture()
                        alias = fixture.physical(name)
                        target = fixture.bindings[alias]
                        alias.unlink()

                        def appear():
                            if kind == 'symlink':
                                alias.symlink_to(target)
                            elif kind == 'regular':
                                alias.write_bytes(target.read_bytes())
                            else:
                                alias.symlink_to(fixture.root / 'missing')

                        self.rejected(fixture, boundary, appear)

    def test_dangling_initial_alias_rejected(self):
        for name in ALIASES:
            with self.subTest(alias=name):
                fixture = self.fixture()
                alias = fixture.physical(name)
                alias.unlink()
                alias.symlink_to(fixture.root / 'missing')
                self.rejected(fixture, 'git-1', lambda: None)

    def test_profile_missing_duplicate_or_credentials_rejected(self):
        for kind in ('missing', 'duplicate', 'credentials'):
            with self.subTest(kind=kind):
                fixture = self.fixture()
                entries = list(fixture.entries)
                if kind == 'missing':
                    entries.pop()
                elif kind == 'duplicate':
                    entries[-1] = entries[0]
                else:
                    entries[-1] = '$HOME/.cargo/credentials.toml'
                fixture.profile = fixture.root / 'profile.json'
                fixture.profile.write_text(json.dumps(entries))
                self.rejected(fixture, 'git-1', lambda: None)

    def test_capacity_rejection_precedes_objects_and_consumers(self):
        fixture = self.fixture()
        error = RuntimeError('fixture capacity exhausted')
        with patch('shutil.disk_usage', side_effect=error), \
                patch('subprocess.check_output') as git, patch('subprocess.run') as ldd:
            def capacity():
                collector.shutil.disk_usage(fixture.source)
            with self.assertRaisesRegex(RuntimeError, 'capacity exhausted'):
                collector.Snapshot(fixture.audit, check_capacity=capacity)
        self.assertFalse((fixture.audit / 'before-objects').exists())
        git.assert_not_called()
        ldd.assert_not_called()


if __name__ == '__main__':
    unittest.main()
