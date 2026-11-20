"""FAIL647: per-family terminal cases and disabled conformance witnesses."""
from pathlib import Path
import tempfile
import unittest

import surface_manifest as manifest
from surface_sources import OUTER, Sources
from surface_syntax import parse


class ConformanceRegressions(unittest.TestCase):
    def test_terminal_families_reference_their_own_behavior_cases(self):
        rows = [r for r in manifest.project()['entries'] if r['family'] == 'terminals']
        self.assertEqual(len(rows), 8)
        for row in rows:
            with self.subTest(family=row['id']):
                self.assertEqual(row['classification'], 'stable')
                self.assertIn(dict(path=OUTER + 'tests/family_tests.rs',
                                   case=row['id'].lower()), row['conformance'])

    def test_rust_disabled_cases_and_inherited_attributes_are_not_cases(self):
        for attribute in ('#[cfg(any())]', '#[cfg(not(all()))]',
                          '#[cfg_attr(all(), ignore)]'):
            for source in (attribute + ' #[test] fn disabled() {}',
                           '#[test] ' + attribute + ' fn disabled() {}',
                           attribute + ' mod tests { #[test] fn disabled() {} }'):
                with self.subTest(source=source):
                    self.assertEqual(manifest.rust_cases(parse(source)), [])
        self.assertEqual(manifest.rust_cases(parse(
            '#[cfg(all())] #[test] fn active() {} '
            '#[cfg(unix)] mod tests { #[test] fn platform_case() {} }')),
            ['active', 'platform_case'])

    def test_python_class_skip_and_method_skip_are_not_cases(self):
        audit = manifest.ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='disabled-case-', dir=audit) as directory:
            root = Path(directory)
            path = root / 'cases.py'
            for decorator in ('@unittest.skip("disabled")',
                              '@unittest.skipIf(True, "disabled")',
                              '@unittest.skipUnless(False, "disabled")'):
                for source in (decorator + '\nclass Cases(unittest.TestCase):\n'
                               '    def test_disabled(self): pass\n',
                               'class Cases(unittest.TestCase):\n    ' + decorator + '\n'
                               '    def test_disabled(self): pass\n'):
                    with self.subTest(source=source):
                        path.write_text('import unittest\n' + source)
                        with self.assertRaisesRegex(ValueError, 'orphan'):
                            manifest.conformance(Sources(root),
                                                 dict(path='cases.py', case='test_disabled'))

    def test_rust_attribute_neighbors_preserve_active_cases(self):
        source = '''
        #[cfg_attr(any(), test)] fn not_a_test() {}
        #[cfg_attr(all(), cfg(any()))] #[test] fn disabled() {}
        #[cfg_attr(any(), ignore)] #[test] fn active() {}
        #[cfg(all(test, any()))] mod hidden { #[test] fn hidden() {} }
        mod inner { #![cfg(any())] #[test] fn hidden_inner() {} }
        #[cfg(any())] const UNUSED: usize = 0;
        #[test] fn after_constant() {}
        #[cfg_attr(all(), test)] fn attributed_test() {}
        '''
        self.assertEqual(manifest.rust_cases(parse(source)),
                         ['active', 'after_constant', 'attributed_test'])

    def test_python_skip_alias_inheritance_and_live_conditions(self):
        from surface_conformance import python_cases
        self.assertEqual(python_cases('''
from unittest import TestCase, skip as disabled, skipIf, skipUnless
@disabled('disabled')
class Base(TestCase):
    def test_base(self): pass
class Child(Base):
    def test_inherited_skip(self): pass
class ExplicitSkip(TestCase):
    __unittest_skip__ = True
    def test_flag(self): pass
if False:
    class NeverLoaded(TestCase):
        def test_hidden(self): pass
class Live(TestCase):
    @skipIf(False, 'active')
    def test_false_skipif(self): pass
    @skipUnless(True, 'active')
    def test_true_skipunless(self): pass
    @skipIf(unknown_condition, 'not established')
    def test_unknown(self): pass
'''), ['test_false_skipif', 'test_true_skipunless'])


class TerminalLinkMutations(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = manifest.project()

    def setUp(self):
        import shutil
        audit = manifest.ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix='terminal-links-',
                                                      dir=audit)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in self.original['inputs']:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(manifest.ROOT / name, target)

    def test_every_terminal_link_refuses_missing_or_disabled_case(self):
        path = self.root / (OUTER + 'tests/family_tests.rs')
        original = path.read_text()
        rows = [r for r in self.original['entries'] if r['family'] == 'terminals']
        for row in rows:
            reference = row['conformance'][0]
            for replacement in ('fn removed_', '#[cfg(any())]\nfn ', '#[ignore]\nfn '):
                with self.subTest(family=row['id'], replacement=replacement):
                    before = 'fn ' + reference['case']
                    self.assertIn(before, original)
                    path.write_text(original.replace(before, replacement + reference['case'], 1))
                    with self.assertRaisesRegex(ValueError, 'orphan'):
                        manifest.conformance(Sources(self.root), reference)
        path.write_text(original)
        path.unlink()
        with self.assertRaisesRegex(ValueError, 'orphan'):
            manifest.conformance(Sources(self.root), rows[0]['conformance'][0])

    def test_terminal_conformance_module_must_remain_registered_and_active(self):
        from surface_sources import terminals
        path = self.root / (OUTER + 'tests.rs')
        original = path.read_text()
        for replacement in ('', '#[cfg(any())]\nmod family_tests;',
                            '#[path = "family_tests.rs"]\nmod family_tests;',
                            '#[path = "missing.rs"]\nmod family_tests;'):
            with self.subTest(replacement=replacement):
                before = 'mod family_tests;'
                self.assertIn(before, original)
                path.write_text(original.replace(before, replacement, 1))
                with self.assertRaisesRegex(ValueError, 'orphan terminal conformance module'):
                    terminals(Sources(self.root))

    def test_new_terminal_family_requires_its_own_conformance_case(self):
        from surface_sources import terminals
        defaults = self.root / (OUTER + 'defaults.rs')
        defaults.write_text(defaults.read_text() +
                            '\nconst DEFAULT_NEW_FEATURES: &[&str] = &["256"];\n')
        features = self.root / (OUTER + 'features.rs')
        features.write_text(features.read_text().replace(
            'fn apply_default_family_features(&mut self) {',
            'fn apply_default_family_features(&mut self) {\n'
            'self.add_features(DEFAULT_NEW_FEATURES);', 1))
        row = next(row for row in terminals(Sources(self.root))
                   if row['id'] == 'DEFAULT_NEW_FEATURES')
        self.assertEqual(row['classification'], 'stable')
        with self.assertRaisesRegex(ValueError, 'orphan'):
            manifest.conformance(Sources(self.root), row['conformance'][0])



if __name__ == '__main__':
    unittest.main()
