"""Windows doctest paths must leave space for MSVC's generated linker files."""
import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch


class WindowsDoctestPaths(unittest.TestCase):
    def test_windows_uses_relative_input_and_short_output_session(self):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        source = script.parent.parent
        code = source / 'scripts/fixtures/candidate_identity_doctest.rs'
        target = source / '.rmux-audit/windows-doctest-path-unit'
        target.mkdir(parents=True, exist_ok=True)
        with patch.object(guard, 'host_os', return_value='windows'), \
                patch.object(guard, 'context', return_value=(source, '0' * 40, target)), \
                patch.object(guard, 'source_identity'), \
                patch.object(guard, 'tracked_input', return_value='tracked-origin'), \
                patch.dict(os.environ, {'RUSTC_WRAPPER': str(target / 'wrapper')}), \
                patch.object(guard.subprocess, 'call', return_value=0) as child:
            self.assertEqual(guard.rustdoc_wrapper([sys.executable, '--test', str(code)]), 0)
            args = child.call_args.args[0]
            actual = next(arg for arg in args if arg.endswith('.rs'))
            self.assertFalse(Path(actual).is_absolute())
            self.assertEqual(Path(actual).resolve(), code)
            output = Path(args[args.index('--persist-doctests') + 1])
            self.assertTrue(output.is_relative_to(target))
            self.assertEqual(len(output.name), 16)
            self.assertEqual(len(child.call_args.kwargs['env']['RMUX_IDENTITY_DOC_SESSION']), 64)


if __name__ == '__main__':
    unittest.main()
