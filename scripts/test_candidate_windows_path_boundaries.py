"""Windows path compression preserves HEAD, command bits and root predicates."""
import os
from pathlib import Path, PureWindowsPath
import tempfile
import unittest
from unittest.mock import patch
import candidate_identity as guard


class WindowsPathBoundaries(unittest.TestCase):
    def test_case_injectivity_and_noncanonical_contexts(self):
        # Run the real predicates with Windows path equality on every host.
        # No files are needed for this lexical identity boundary.
        class WindowsPath(PureWindowsPath):
            def absolute(self):
                return self

            resolve = absolute

        paths = guard.support('candidate_identity_paths')
        with patch.object(guard, 'host_os', return_value='windows'), \
                patch.object(guard, 'Path', WindowsPath), \
                patch.object(paths, 'Path', WindowsPath), \
                patch.object(guard, 'support', return_value=paths):
            source = WindowsPath(r'C:\Users\owner\Projects\7s')
            head = 'e' * 40
            a = guard.target_directory(source, head, '00' * 16)
            b = guard.target_directory(source, head, '68' + '00' * 15)
            with self.subTest(boundary='Windows root equality'):
                self.assertNotEqual(a, b)
            for declared, foreign in ((a, b), (b, a)):
                env = dict(RMUX_IDENTITY_SOURCE=str(source), RMUX_IDENTITY_HEAD=head,
                           RMUX_IDENTITY_TARGET=str(declared), CARGO_TARGET_DIR=str(declared))
                with patch.dict(os.environ, env, clear=True):
                    self.assertEqual(guard.context(), (source, head, declared))
                    for suffix in ((), ('consumer-inputs', 'observed', 'target')):
                        with self.subTest(declared=str(declared), foreign=str(foreign), suffix=suffix), \
                                patch.dict(os.environ, CARGO_TARGET_DIR=str(foreign.joinpath(*suffix))):
                            with self.assertRaises(guard.IdentityError):
                                guard.context()
                    for key in ('RMUX_IDENTITY_TARGET', 'CARGO_TARGET_DIR'):
                        for suffix in ((), ('consumer-inputs', 'observed', 'target')):
                            if key == 'RMUX_IDENTITY_TARGET' and suffix:
                                continue
                            alias = declared.with_name(declared.name.upper()).joinpath(*suffix)
                            with self.subTest(key=key, alias=str(alias)), \
                                    patch.dict(os.environ, {key: str(alias)}):
                                with self.assertRaises(guard.IdentityError):
                                    guard.context()
                    with patch.dict(os.environ, CARGO_TARGET_DIR=str(
                            declared / 'consumer-inputs' / 'observed' / 'target')):
                        self.assertEqual(guard.context(), (source, head, declared))

    def test_canonical_commands_are_distinct_and_context_round_trips(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(guard, 'host_os', return_value='windows'):
            source = Path(directory).resolve()
            head = 'e' * 40
            a = guard.target_directory(source, head, '0' * 32)
            b = guard.target_directory(source, head, 'f' * 32)
            self.assertNotEqual(a, b)
            self.assertIn(head, a.parts)
            self.assertEqual(guard.target_directory(source, head, a.name), a)
            self.assertNotEqual(a, guard.target_directory(source, 'a' * 40, '0' * 32))
            self.assertNotEqual(a, guard.target_directory(source / 'other', head, '0' * 32))
            env = dict(RMUX_IDENTITY_SOURCE=str(source), RMUX_IDENTITY_HEAD=head,
                       RMUX_IDENTITY_TARGET=str(a), CARGO_TARGET_DIR=str(a))
            with patch.dict(os.environ, env, clear=True):
                self.assertEqual(guard.context(), (source, head, a))
                for key in ('RMUX_IDENTITY_TARGET', 'CARGO_TARGET_DIR'):
                    with patch.dict(os.environ, {key: str(b)}):
                        with self.assertRaises(guard.IdentityError):
                            guard.context()
            for name in ('A' * 21, 'A' * 23, 'A' * 21 + 'B', '!' * 22):
                with self.assertRaises(guard.IdentityError):
                    guard.target_directory(source, head, name)
            with self.assertRaises(guard.IdentityError):
                guard.target_identity(source, head, a.with_name('0' * 32))
            redirected = source / 'redirected'
            a.parent.mkdir(parents=True)
            redirected.mkdir()
            try:
                a.symlink_to(redirected, target_is_directory=True)
            except OSError:
                return  # Native Windows may lack symlink authority.
            with self.assertRaises(guard.IdentityError):
                guard.target_identity(source, head, a)

    def test_unix_target_layout_is_unchanged(self):
        source = Path(__file__).resolve().parent.parent
        for platform in ('linux', 'macos'):
            with patch.object(guard, 'host_os', return_value=platform):
                expected = source / '.rmux-audit/candidate-target' / ('e' * 40) / platform / ('f' * 32)
                self.assertEqual(guard.target_directory(source, 'e' * 40, 'f' * 32), expected)


if __name__ == '__main__':
    unittest.main()
