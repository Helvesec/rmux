"""Keep the real CLI hardlink test and its helper visible on Windows."""
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parent.parent


class CliTestHelpers(unittest.TestCase):
    def test_windows_hardlink_test_can_resolve_its_real_helper(self):
        # src/cli.rs declares the test module for every platform; its body
        # lives in src/cli/tests.rs since 04e6e8814.
        source = (ROOT / 'src/cli.rs').read_text()
        self.assertIsNotNone(re.search(r'(?m)^#\[cfg\(test\)\]\nmod tests;$', source),
                             'the CLI test module must not be gated to one platform')
        tests = (ROOT / 'src/cli/tests.rs').read_text()
        self.assertIsNone(re.search(r'(?m)^#!\[cfg', tests), 'the CLI test module must not gate itself')

        def attributes(name):
            declaration = re.search(
                r'(?m)((?:^#\[[^\n]+\]\n)*)^fn '
                + re.escape(name) + r'\(', tests)
            self.assertIsNotNone(declaration, 'missing real CLI declaration: ' + name)
            return declaration.group(1)

        # Read both declarations in the real CLI test module. A fixture-provided helper
        # would conceal the Windows E0425 this regression guards against.
        helper = attributes('unique_test_dir')
        hardlink = attributes('usable_shell_path_rejects_hardlink_to_the_current_executable')
        self.assertNotIn('cfg', helper, 'unique_test_dir must also exist in the Windows CLI module')
        self.assertNotIn('cfg', hardlink, 'the hardlink test must retain Windows coverage')
        self.assertIn('let dir = unique_test_dir("shell-hardlink");', tests)


if __name__ == '__main__':
    unittest.main()
