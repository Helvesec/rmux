"""R2221: discovering test_inventory must not depend on SOURCE_SYNTAX at import."""
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest

TOOL = Path(__file__).resolve().parent
PARSER_TEST = 'test_expression_statement_owns_exact_semicolon_and_trivia'


def discover(audit, parser, *selection):
    env = {key: value for key, value in os.environ.items() if key != 'SOURCE_SYNTAX'}
    env['SOURCE_AUDIT'] = str(audit)
    if parser is not None:
        env['SOURCE_SYNTAX'] = str(parser)
    argv = [sys.executable, '-B', '-m', 'unittest', 'discover', '-s', str(TOOL),
            '-p', 'test_inventory.py', '-v']
    for name in selection:
        argv += ['-k', name]
    return subprocess.run(argv, cwd=audit, env=env, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT)


class ParserEnvironment(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.audit = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def assert_explicit_error(self, parser, reason):
        result = discover(self.audit, parser, PARSER_TEST)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn('_FailedTest', result.stdout)
        self.assertNotIn('KeyError', result.stdout)
        self.assertNotIn('skipped', result.stdout)
        self.assertIn(PARSER_TEST + ' ', result.stdout)
        errors = result.stdout.count('\nERROR: ')
        self.assertGreater(errors, 0, result.stdout)
        self.assertIn('FAILED (errors=%d)' % errors, result.stdout)
        explicit = result.stdout.count('RuntimeError: SOURCE_SYNTAX ' + reason)
        self.assertEqual(explicit, errors, result.stdout)

    def test_discovery_imports_without_parser_and_reports_it_explicitly(self):
        self.assert_explicit_error(None, 'must name the rmux-source-syntax parser')
        self.assert_explicit_error('', 'must name the rmux-source-syntax parser')

    def test_invalid_parser_is_an_explicit_error_not_a_skip(self):
        self.assert_explicit_error('relative/rmux-source-syntax', 'must be an absolute path')
        self.assert_explicit_error(self.audit / 'absent', 'must name an executable file')
        self.assert_explicit_error(self.audit, 'must name an executable file')
        if sys.platform != 'win32':
            plain = self.audit / 'not-executable'
            plain.write_text('')
            plain.chmod(stat.S_IRUSR | stat.S_IWUSR)
            self.assert_explicit_error(plain, 'must name an executable file')

    def test_discovery_without_parser_keeps_the_gate_canaries_visible(self):
        canaries = ('test_new_maintained_tooling_and_exact_size_boundary',
                    'test_cli_size_ratchet_and_reduction_checks_fail')
        result = discover(self.audit, None, *canaries)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn('_FailedTest', result.stdout)
        self.assertNotIn('KeyError', result.stdout)
        for name in canaries:
            self.assertRegex(result.stdout, r'(?m)^' + name + r' \([^\n]+\) \.\.\. ERROR$')
        self.assertIn('FAILED (errors=2)', result.stdout)
        absent = 'RuntimeError: SOURCE_SYNTAX must name the rmux-source-syntax parser'
        self.assertEqual(result.stdout.count(absent), 2, result.stdout)

    def test_d570_epochs_without_parser_fails_before_any_proof(self):
        # It used to raise KeyError only after about two minutes of git proofs.
        env = {key: value for key, value in os.environ.items() if key != 'SOURCE_SYNTAX'}
        result = subprocess.run([sys.executable, '-B', '-m', 'unittest', 'discover', '-s', str(TOOL),
                                 '-p', 'test_d570_epochs.py'], cwd=self.audit, env=env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn('KeyError', result.stdout)
        self.assertIn('ERROR: setUpModule (test_d570_epochs)', result.stdout)
        self.assertIn('RuntimeError: SOURCE_SYNTAX must name the rmux-source-syntax parser', result.stdout)
        self.assertRegex(result.stdout, r'(?m)^Ran 0 tests in ')


if __name__ == '__main__':
    unittest.main()
