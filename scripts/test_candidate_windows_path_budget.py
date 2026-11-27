"""Frozen L09 linker budget, with the public consumer helper left unchanged."""
from pathlib import Path, PureWindowsPath
import unittest
from unittest.mock import patch
import candidate_identity as guard


class WindowsPathBudget(unittest.TestCase):
    def test_private_sdk_and_proto_consumers_fit_the_native_linker(self):
        source = Path(__file__).resolve().parent.parent
        with patch.object(guard, 'host_os', return_value='windows'):
            target = guard.target_directory(source, 'e' * 40, 'f' * 32)
        roots = (r'C:\Users\owner\Projects\7s',
                 r'C:\Users\user \Projects\7s',
                 r'C:\Users\usér \Projects\7s')
        for root in roots:
            for package in ('rmux-sdk', 'rmux-proto'):
                with self.subTest(root=root, package=package):
                    self.assertEqual(len(root), 26)
                    self.assert_consumer_budget(source, target, root, package)

    def assert_consumer_budget(self, source, target, root, package):
        native = PureWindowsPath(root).joinpath(*target.relative_to(source).parts)
        consumer = native / 'consumer-inputs' / (
            package + '-public-api-compile-fail-4294967295-0')
        linker_output = consumer / 'target/debug/build' / (
            'proc-macro2-74b72d7061b41cbd') / (
            'build_script_build-74b72d7061b41cbd.exe')
        self.assertLess(len(str(linker_output)), 260, str(linker_output))


if __name__ == '__main__':
    unittest.main()
