"""Run with SOURCE_SYNTAX=/absolute/path/rmux-source-syntax python3 -m unittest discover ..."""
import copy
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tempfile
import unittest
from compare import compare
from inventory import contexts, parser_scan, rust_lines, scan, targets
from model import (canonical, conjunction, possible, source_syntax_parser,
                   validate_declarations, expand_declarations)

AUDIT = Path(os.environ.get('SOURCE_AUDIT', '.rmux-audit/test-fixtures')).resolve()
AUDIT.mkdir(parents=True, exist_ok=True)


def parser():
    """The SOURCE_SYNTAX parser, resolved per test so discovery never needs it."""
    return source_syntax_parser()


def policy():
    return {'version': 1, 'files': {}, 'documents': {}, 'provenance': {},
            'includes': {}, 'rust_roots': {}, 'crate_roles': {}}


def parse(source, grammar='file'):
    return json.loads(subprocess.check_output([parser()], input=(canonical(
        {'source': source, 'grammar': grammar}) + '\n').encode()))


def count(source, role='production'):
    result = parse(source)
    if result['errors']:
        raise ValueError(result['errors'])
    counts, regions, mixed, segments = rust_lines({'data': source.encode()}, result,
        {'root': {'role': role, 'condition': True}})
    return counts, regions, mixed, segments


class SyntaxTests(unittest.TestCase):
    def test_expression_statement_owns_exact_semicolon_and_trivia(self):
        for statement in ('undefined_fn();', 'undefined_fn() /* ; /* nested */ */ ;',
                          'undefined_fn()\n// gap before the owned separator\n;',
                          'undefined_fn()\r\n\t;', 'return undefined_fn();'):
            with self.subTest(statement=statement):
                source = 'pub fn f() {\n#[cfg(test)]\n' + statement + '\n}'
                counts, _, mixed, _ = count(source)
                self.assertEqual(counts, {'production': 2, 'tests': statement.count('\n') + 2})
                self.assertEqual(mixed, [])

    def test_statement_separator_does_not_swallow_live_sibling(self):
        for statement in ('#[cfg(test)] undefined_fn(); ;',
                          '#[cfg(test)] undefined_fn(); live();',
                          '#[cfg(test)] fn dead() {} ;',
                          '#[cfg(test)] let x = 1; ;',
                          '#[cfg(test)] gone!(); ;',
                          '#[cfg(test)] undefined_fn(); // unattached comment'):
            with self.subTest(statement=statement):
                counts, _, mixed, _ = count('fn f() {\n' + statement + '\n}')
                self.assertEqual(counts, {'production': 3})
                self.assertEqual(mixed, [2])
        counts, _, mixed, _ = count('fn f() {\n#[cfg(test)] undefined_fn();\n;\n}')
        self.assertEqual(counts, {'production': 3, 'tests': 1})
        self.assertEqual(mixed, [])

    def test_nested_expression_does_not_own_enclosing_statement_separator(self):
        source = 'fn f() {\nlet x = {\n#[cfg(test)] undefined_fn()\n};\n}'
        counts, _, mixed, _ = count(source)
        self.assertEqual(counts, {'production': 4, 'tests': 1})
        self.assertEqual(mixed, [])
        source = 'fn f() {\n#[cfg_attr(not(test), cfg(any()))]\n{ undefined_fn(); };\nlive();\n}'
        self.assertEqual(count(source)[0], {'production': 3, 'tests': 2})

    def test_dead_cfg_retains_nearest_inline_or_file_class(self):
        for role in ('production', 'tooling'):
            for outer in ('#[cfg(test)]', '#[cfg_attr(not(test), cfg(any()))]'):
                for dead in ('all(unix, not(unix))', 'not(test)'):
                    with self.subTest(role=role, outer=outer, dead=dead):
                        source = (outer + '\nmod m {\n#[cfg(' + dead + ')]\n'
                                  'mod dead {\nfn retained() {}\n}\n}')
                        self.assertEqual(count(source, role)[0], {'tests': 7})
            self.assertEqual(count('#![cfg(test)]\n#[cfg(not(test))]\nfn dead() {}', role)[0],
                             {'tests': 3})

    def test_nested_inline_and_resumed_production(self):
        source = 'fn a() {}\n#[cfg(test)]\nmod tests {\n fn t() {}\n}\nfn b() {}'
        counts, regions, _, _ = count(source)
        self.assertEqual(counts, {'production': 2, 'tests': 4})
        self.assertEqual(regions[-1], {'start': 6, 'end': 6, 'class': 'production'})

    def test_fake_attributes_comments_strings_raw_strings_and_unicode(self):
        source = '''// #[cfg(test)]
/* outer /* #[cfg(test)] */ end */
const S: &str = "#[cfg(test)]";
const R: &str = r###"#[cfg(test)] mod nope { }"###;
const U: &str = "é😀";
fn after() {}'''
        self.assertEqual(count(source)[0], {'production': 6})
        self.assertEqual(count(source + '\n#[cfg(test)] fn real() {}')[0],
                         {'production': 6, 'tests': 1})

    def test_compound_cfg_matrix(self):
        cases = {
            'test': 'tests', 'all(test, unix)': 'tests',
            'any(test, windows)': 'production', 'not(test)': 'production',
            'all(any(test, unix), not(unix))': 'tests',
            'all(test, feature = "web")': 'tests',
            'any()': 'production', 'all()': 'production',
            'all(unix, windows)': 'production',
        }
        for cfg, expected in cases.items():
            with self.subTest(cfg=cfg):
                self.assertEqual(count(f'#[cfg({cfg})]\nfn f() {{}}')[0], {expected: 2})

    def test_cfg_attr_nested(self):
        for attrs, expected in [
            ('#[cfg_attr(not(test), cfg(test))]', 'tests'),
            ('#[cfg_attr(test, allow(dead_code))]', 'production'),
            ('#[cfg_attr(unix, cfg(test))]', 'production'),
            ('#[cfg_attr(not(test), cfg_attr(not(test), cfg(any())))]', 'tests'),
            ('#[cfg_attr(not(test), test)]', 'tests'),
        ]:
            with self.subTest(attrs=attrs):
                self.assertEqual(count(attrs + '\nfn f() {}')[0], {expected: 2})

    def test_fields_statements_patterns_and_methods(self):
        source = '''struct A {
 #[cfg(test)] x: u8,
 y: u8,
}
impl A {
 #[cfg(test)] fn test() {}
 fn f() {
  #[cfg(test)] let a = 2;
  let b = 3;
 }
}
'''
        self.assertEqual(count(source)[0], {'production': 8, 'tests': 3})

    def test_inner_file_and_inline_attributes(self):
        self.assertEqual(count('#![cfg(test)]\nfn f() {}')[0], {'tests': 2})
        self.assertEqual(count('mod inner {\n #![cfg(test)]\n fn f() {}\n}\nfn g() {}')[0],
                         {'tests': 4, 'production': 1})

    def test_mixed_lines_crlf_empty_and_final_line(self):
        counts, _, mixed, _ = count('#[cfg(test)] fn t() {} fn p() {}\r\n\r\nfn last() {}')
        self.assertEqual(counts, {'production': 3})
        self.assertEqual(mixed, [1])
        self.assertEqual(count('')[0], {})
        self.assertEqual(count('\n')[0], {'production': 1})
        self.assertEqual(count('#[test]\nfn t() {}')[0], {'tests': 2})

    def test_tool_tests_and_never_compiled_are_retained(self):
        self.assertEqual(count('#[cfg(test)] fn t() {}\nfn p() {}', 'tooling')[0],
                         {'tests': 1, 'tooling': 1})
        self.assertEqual(count('#[cfg(all(test, not(test)))] fn f() {}')[0],
                         {'production': 1})

    def test_reject_unsupported_cfg_and_opaque_macro(self):
        for source in ('#[cfg(version("1.88"))] fn f() {}',
                       '#[cfg_attr(test, path="a.rs")] mod a;',
                       'custom! { #[cfg(test)] fn f() {} }',
                       'custom! { #![cfg(test)] fn f() {} }',
                       'fn broken(', 'fn f() { foo!(#[cfg(test)] 1); }'):
            with self.subTest(source=source):
                self.assertTrue(parse(source)['errors'])
        self.assertFalse(parse('foo!("#[cfg(test)]");')['errors'])

    def test_select_blocks_use_syn_and_unknown_branch_cfg_is_rejected(self):
        counts = count('fn f() { tokio::select! { _ = g() => {\n#[cfg(test)] let x = 1;\n} } }')[0]
        self.assertEqual(counts, {'production': 2, 'tests': 1})
        self.assertTrue(parse('fn f() { tokio::select! { #[cfg(test)] _ = g() => {} } }')['errors'])

    def test_expression_fixture_and_atom_bound(self):
        self.assertFalse(parse('&[(0, Foo::A, "a")]', 'expression')['errors'])
        with self.assertRaisesRegex(ValueError, '16'):
            possible({'all': ['flag' + str(i) for i in range(17)]}, False)


class TargetTests(unittest.TestCase):
    kinds = (('bin', 'src/bin'), ('example', 'examples'),
             ('test', 'tests'), ('bench', 'benches'))

    def roots(self, kind, paths, name='keep', path=None, auto=False,
              edition='2021', role='production'):
        plural = 'benches' if kind == 'bench' else kind + 's'
        manifest = '[package]\nname="fixture"\nversion="0.1.0"\n'
        if edition is not None:
            manifest += f'edition="{edition}"\n'
        manifest += f'auto{plural}={str(auto).lower()}\n[[{kind}]]\nname="{name}"\n'
        if path is not None:
            manifest += f'path="{path}"\n'
        # Nested manifests resolve only against their own tracked package paths.
        files = {'pkg/' + p: {'data': b'fn main() {}'} for p in paths}
        files['pkg/Cargo.toml'] = {'data': manifest.encode()}
        p = policy()
        p['crate_roles']['pkg/Cargo.toml'] = role
        return {(root, role) for root, role, _, _ in targets(files, p)[0]}

    def test_named_explicit_conventional_paths_with_auto_on_or_off(self):
        for kind, directory in self.kinds:
            for suffix in ('.rs', '/main.rs'):
                for auto in (False, True):
                    for role in ('production', 'tooling'):
                        with self.subTest(kind=kind, suffix=suffix, auto=auto, role=role):
                            path = directory + '/keep' + suffix
                            other = directory + '/other.rs'
                            expected_role = 'tests' if kind in ('test', 'bench') else role
                            expected = {('pkg/' + path, expected_role)}
                            if auto:
                                expected.add(('pkg/' + other, expected_role))
                            self.assertEqual(self.roots(kind, [path, other], auto=auto,
                                                        role=role), expected)

    def test_explicit_path_precedes_inference_and_auto_discovery(self):
        for kind, directory in self.kinds:
            for auto in (False, True):
                with self.subTest(kind=kind, auto=auto):
                    paths = [directory + '/keep.rs', directory + '/keep/main.rs',
                             directory + '/other.rs', 'custom.rs']
                    role = 'tests' if kind in ('test', 'bench') else 'production'
                    expected = {('pkg/custom.rs', role)}
                    if auto:
                        expected.add(('pkg/' + directory + '/other.rs', role))
                    self.assertEqual(self.roots(kind, paths, path='custom.rs', auto=auto),
                                     expected)
        self.assertEqual(self.roots('bin', ['src/main.rs', 'custom.rs'],
                         name='fixture', path='custom.rs', auto=True),
                         {('pkg/custom.rs', 'production')})

    def test_missing_and_ambiguous_inferred_targets_fail(self):
        for kind, directory in self.kinds:
            for paths in ([], [directory + '/keep.rs', directory + '/keep/main.rs']):
                for auto in (False, True):
                    with self.subTest(kind=kind, paths=paths, auto=auto):
                        with self.assertRaisesRegex(ValueError,
                                f'unresolved/ambiguous Cargo {kind} target: pkg/Cargo.toml: keep:'):
                            self.roots(kind, paths, auto=auto)
            with self.subTest(kind=kind, explicit_missing=True):
                with self.assertRaisesRegex(ValueError, 'missing declared Cargo target'):
                    self.roots(kind, [directory + '/keep.rs'], path='absent.rs')

    def test_package_named_bin_main_and_edition_2015_fallback(self):
        self.assertEqual(self.roots('bin', ['src/main.rs'], name='fixture'),
                         {('pkg/src/main.rs', 'production')})
        for other in ([], ['src/bin/fixture.rs'], ['src/bin/fixture/main.rs']):
            with self.subTest(other=other):
                with self.assertRaisesRegex(ValueError, 'unresolved/ambiguous Cargo bin'):
                    self.roots('bin', ['src/main.rs'] + other,
                               name='fixture' if other else 'renamed')
        for edition in ('2015', None):
            for other in ([], ['src/bin/keep.rs', 'src/bin/keep/main.rs']):
                with self.subTest(edition=edition, other=other):
                    self.assertEqual(self.roots('bin', ['src/main.rs'] + other,
                                               edition=edition),
                                     {('pkg/src/main.rs', 'production')})
        self.assertEqual(self.roots('bin', ['src/main.rs', 'src/bin/keep.rs'], edition='2015'),
                         {('pkg/src/bin/keep.rs', 'production')})


class TreeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=AUDIT)
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.write('Cargo.toml', '[package]\nname="fixture"\nversion="0.1.0"\nedition="2021"\n')
        self.p = policy()
        self.p['files']['tooling'] = ['Cargo.toml']

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args]).decode().strip()

    def write(self, path, content):
        p = self.repo / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)

    def commit(self):
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')
        return self.git('rev-parse', 'HEAD')

    def scan(self, revision, declarations=None):
        declarations = declarations or self.p
        raw = canonical(declarations).encode()
        return scan(self.repo, revision, parser(), declarations, raw)

    def test_impossible_consumer_cannot_outrank_live_consumers(self):
        self.write('src/lib.rs', 'mod shared;\n')
        self.write('src/shared.rs', 'pub fn p() {}\n#[cfg(test)]\n#[path="shared/checks.rs"]\nmod checks;\n')
        self.write('src/shared/checks.rs', 'fn helper() {}\n')
        before = self.scan(self.commit())
        self.write('tool/Cargo.toml', '[package]\nname="tool"\nversion="0.1.0"\n')
        self.p['files']['tooling'].append('tool/Cargo.toml')
        self.p['crate_roles']['tool/Cargo.toml'] = 'tooling'
        self.write('tool/src/main.rs', '#[cfg(not(test))]\n#[path="../../src/shared.rs"]\nmod shared;\nfn main() {}')
        after = self.scan(self.commit())
        self.assertEqual(before['errors'], [])
        self.assertEqual(after['errors'], [])
        rows = {r['path']: r for r in after['files']}
        old = next(r for r in before['files'] if r['path'] == 'src/shared/checks.rs')
        self.assertEqual(rows['src/shared/checks.rs']['blob'], old['blob'])
        self.assertEqual(rows['src/shared/checks.rs']['counts'], old['counts'])
        self.assertEqual(rows['src/shared.rs']['counts'], {'production': 1, 'tests': 3})
        # Real non-test tooling consumption beats tests; real product beats both.
        self.write('tool/src/main.rs', '#[path="../../src/shared/checks.rs"]\nmod checks;\nfn main() {}')
        live = self.scan(self.commit())
        self.assertEqual(next(r for r in live['files'] if r['path'] == old['path'])['counts'],
                         {'tooling': 1})
        self.write('src/lib.rs', '#[path="shared/checks.rs"]\nmod checks;\nmod shared;')
        live = self.scan(self.commit())
        self.assertEqual(next(r for r in live['files'] if r['path'] == old['path'])['counts'],
                         {'production': 1})

    def test_dead_context_retains_ancestry_across_module_and_include(self):
        for edge in ('#[path="dead.rs"] mod dead;', 'include!("dead.rs");'):
            with self.subTest(edge=edge):
                self.write('src/lib.rs', '#[cfg(test)]\nmod checks;')
                self.write('src/checks.rs', '#[cfg(not(test))]\n' + edge)
                self.write('src/dead.rs', '#[cfg(unix)]\nfn retained() {}')
                report = self.scan(self.commit())
                self.assertEqual(report['errors'], [])
                for row in report['files']:
                    if row['path'].endswith('.rs'):
                        self.assertEqual(row['counts'], {'tests': 2}, row['path'])

    def test_dead_product_context_cannot_outrank_live_tooling(self):
        self.write('src/lib.rs', '#[cfg(any())]\nmod shared;')
        self.write('src/shared.rs', 'pub fn retained() {}')
        self.p['rust_roots']['src/shared.rs'] = {'role': 'tooling', 'evidence': 'standalone tool'}
        report = self.scan(self.commit())
        self.assertEqual(report['errors'], [])
        self.assertEqual(next(r for r in report['files'] if r['path'] == 'src/shared.rs')['counts'],
                         {'tooling': 1})

    def test_cargo_auto_benches_and_sibling_targets(self):
        for path in ('src/lib.rs', 'src/main.rs', 'src/bin/bin.rs', 'examples/ex.rs',
                     'tests/it.rs', 'benches/bench_a.rs', 'benches/nested/main.rs'):
            self.write(path, 'fn main() {}')
        report = self.scan(self.commit())
        self.assertEqual(report['errors'], [])
        for row in report['files']:
            if row['path'].endswith('.rs'):
                role = 'tests' if row['path'].startswith(('tests/', 'benches/')) else 'production'
                self.assertEqual(row['counts'], {role: 1}, row['path'])

    def test_cargo_disabled_autobenches_preserves_explicit_bench(self):
        self.write('Cargo.toml', '[package]\nname="fixture"\nversion="0.1.0"\n'
                   'autobenches=false\n[[bench]]\nname="explicit"\npath="bench/custom.rs"\n')
        for path in ('src/lib.rs', 'benches/auto.rs', 'benches/nested/main.rs', 'bench/custom.rs'):
            self.write(path, 'fn main() {}')
        report = self.scan(self.commit())
        self.assertEqual(report['errors'], ['unclassified Rust: benches/auto.rs',
                                           'unclassified Rust: benches/nested/main.rs'])
        rows = {r['path']: r for r in report['files']}
        self.assertEqual(rows['bench/custom.rs']['counts'], {'tests': 1})
        self.assertEqual(rows['benches/auto.rs']['counts'], {})

    def test_inferred_targets_use_git_inputs_and_retain_production_consumers(self):
        manifest = ('[package]\nname="fixture"\nversion="0.1.0"\nedition="2021"\n'
                    'autobins=false\nautoexamples=false\nautotests=false\nautobenches=false\n')
        paths = []
        for kind, directory in TargetTests.kinds:
            for name, suffix in (('flat', '.rs'), ('nested', '/main.rs')):
                manifest += f'[[{kind}]]\nname="{name}"\n'
                path = directory + '/' + name + suffix
                paths.append(path)
                self.write(path, 'pub fn live() {}\n')
        self.write('Cargo.toml', manifest)
        self.write('src/lib.rs', '#[path="../benches/flat.rs"]\nmod shared;\n')
        revision = self.commit()
        first = self.scan(revision)
        self.assertEqual(first['errors'], [])
        rows = {row['path']: row for row in first['files']}
        for path in paths:
            role = 'tests' if path.startswith(('tests/', 'benches/')) else 'production'
            if path == 'benches/flat.rs':
                role = 'production'
            self.assertEqual(rows[path]['counts'], {role: 1}, path)
        # Untracked collisions, modified manifests and absent working-tree roots
        # cannot alter discovery from the exact committed Git input.
        for _, directory in TargetTests.kinds:
            self.write(directory + '/flat/main.rs', 'invalid Rust')
            (self.repo / (directory + '/nested/main.rs')).unlink()
        self.write('Cargo.toml', 'invalid TOML')
        self.assertEqual(canonical(self.scan(revision)), canonical(first))

    def test_reachable_product_in_tests_directory_and_path_modules(self):
        self.write('src/lib.rs', '#[path="../tests/shared.rs"]\nmod shared;\n#[cfg(test)]\nmod tests;')
        self.write('src/tests.rs', 'mod nested;')
        self.write('src/tests/nested.rs', 'fn t() {}')
        self.write('tests/shared.rs', 'pub fn product() {}\nmod helper;')
        self.write('tests/helper.rs', 'pub fn helper() {}')
        report = self.scan(self.commit())
        self.assertEqual(report['errors'], [])
        rows = {r['path']: r for r in report['files']}
        self.assertEqual(rows['tests/shared.rs']['counts'], {'production': 2})
        self.assertEqual(rows['tests/helper.rs']['counts'], {'production': 1})
        self.assertEqual(rows['src/tests/nested.rs']['counts'], {'tests': 1})

    def test_nested_inline_path_include_and_required_target(self):
        self.write('src/lib.rs', 'mod inline { #[path="helper.rs"] mod helper; }\n#[cfg(test)] mod check { include!("inc.rs"); }')
        self.write('src/inline/helper.rs', 'pub fn p() {}')
        self.write('src/inc.rs', 'fn t() {}')
        report = self.scan(self.commit())
        self.assertEqual(report['errors'], [])
        rows = {r['path']: r for r in report['files']}
        self.assertEqual(rows['src/inc.rs']['counts'], {'tests': 1})
        self.assertEqual(rows['src/inline/helper.rs']['counts'], {'production': 1})
        self.write('Cargo.toml', '[package]\nname="f"\nversion="0.1.0"\n[lib]\npath="absent.rs"\n')
        with self.assertRaisesRegex(ValueError, 'missing declared Cargo target'):
            self.scan(self.commit())

    def test_inline_path_in_non_mod_rs_resolves_json_output_framing(self):
        self.write('src/lib.rs', 'mod cli;\n')
        self.write('src/cli.rs', 'mod json_output;\n')
        self.write('src/cli/json_output.rs', '#[cfg(test)]\n'
                   '#[path = "json_output/tests"]\nmod tests { mod framing; }\n')
        child = 'src/cli/json_output/tests/framing.rs'
        self.write(child, '#[test]\nfn framing() {}\n')
        report = self.scan(self.commit())
        self.assertEqual(report['errors'], [])
        row = next(r for r in report['files'] if r['path'] == child)
        self.assertEqual(row['counts'], {'tests': 2})

    def test_inline_module_directory_matrix(self):
        for index, (source, entry, directory) in enumerate((
                ('src/lib.rs', '', 'src'),
                ('src/outer.rs', 'mod outer;', 'src/outer'),
                ('src/outer/mod.rs', 'mod outer;', 'src/outer'),
                ('src/custom.rs', '#[path="custom.rs"] mod outer;', 'src'))):
            for explicit in (False, True):
                with self.subTest(source=source, explicit=explicit):
                    # Separate packages keep every earlier fixture reachable.
                    prefix = f'case_{index}_{int(explicit)}/'
                    manifest = prefix + 'Cargo.toml'
                    self.write(manifest, '[package]\nname="case"\nversion="0.1.0"\n')
                    self.p['files']['tooling'].append(manifest)
                    self.write(prefix + 'src/lib.rs', entry)
                    attribute = '#[path="redirected"] ' if explicit else ''
                    self.write(prefix + source, attribute + 'mod inline {\n'
                               'mod normal { mod leaf; }\n'
                               '#[path="renamed"] mod nested {\n'
                               'mod leaf; #[path="alternate.rs"] mod alternate;\n}\n}\n'
                               'mod sibling { mod leaf; }\n')
                    base = PurePosixPath(source).parent.as_posix() + '/redirected' if explicit else directory + '/inline'
                    children = [base + '/normal/leaf.rs', base + '/renamed/leaf.rs',
                                base + '/renamed/alternate.rs', directory + '/sibling/leaf.rs']
                    for child in children:
                        self.write(prefix + child, 'pub fn retained() {}\n')
                    self.write('src/lib.rs', '')
                    report = self.scan(self.commit())
                    self.assertEqual(report['errors'], [])
                    rows = {r['path']: r for r in report['files']}
                    for child in children:
                        self.assertEqual(rows[prefix + child]['counts'], {'production': 1})

    def test_inline_path_missing_and_ambiguous_children_are_rejected(self):
        self.write('src/lib.rs', 'mod outer;\n')
        self.write('src/outer.rs', '#[path="redirected"] mod inline { mod child; }\n')
        candidates = ['src/redirected/child.rs', 'src/redirected/child/mod.rs']
        expected = 'src/outer.rs: unresolved/ambiguous module ' + repr(candidates)
        self.assertIn(expected, self.scan(self.commit())['errors'])
        for child in candidates:
            self.write(child, 'pub fn retained() {}\n')
        self.assertIn(expected, self.scan(self.commit())['errors'])

    def test_added_production_path_is_rejected_on_both_platforms(self):
        from production_policy import check_ratchets

        self.write('src/lib.rs', 'mod child;\n')
        self.write('src/child.rs', 'pub fn retained() {}\n')
        before = self.scan(self.commit())
        for condition in ('all()', 'unix', 'windows', 'test'):
            with self.subTest(condition=condition):
                self.write('src/lib.rs', 'mod child;\n#[cfg(' + condition +
                           ')]\n#[path="child.rs"]\nmod sibling;\n')
                after = self.scan(self.commit())
                self.assertEqual(after['errors'], [])
                delta = compare(self.repo, before, after)
                self.assertEqual(delta['literal_ratchet_increases'], [])
                if condition == 'test':
                    self.assertEqual(delta['semantic_attribute_additions'], [])
                    check_ratchets(delta)
                else:
                    self.assertEqual(len(delta['semantic_attribute_additions']), 1)
                    self.assertEqual(delta['semantic_attribute_additions'][0]['kind'], 'path')
                    with self.assertRaisesRegex(ValueError, 'source ratchet RED'):
                        check_ratchets(delta)
                directory = self.repo / '.git'
                for name, report in (('before', before), ('after', after)):
                    (directory / (name + '.json')).write_text(canonical(report))
                result = subprocess.run([sys.executable, str(Path(__file__).with_name('compare.py')),
                    '--repo', str(self.repo), '--before', str(directory / 'before.json'),
                    '--after', str(directory / 'after.json'),
                    '--output', str(directory / 'delta.json'), '--check-ratchets'],
                    capture_output=True, text=True)
                self.assertEqual(result.returncode, 0 if condition == 'test' else 1, result.stderr)

    def test_nested_crate_paths_reject_production_and_preserve_test_only(self):
        from production_policy import check_ratchets

        self.write('src/lib.rs', 'pub fn retained() {}\n')
        manifest = 'crates/server/Cargo.toml'
        self.write(manifest, '[package]\nname="server"\nversion="0.1.0"\nedition="2021"\n')
        self.p['files']['tooling'].append(manifest)
        self.write('crates/server/src/lib.rs', 'mod web;\n')
        self.write('crates/server/src/web.rs', 'mod protocol;\n')
        source = 'crates/server/src/web/protocol.rs'
        child = 'crates/server/src/web/protocol/security/child.rs'
        self.write(source, 'mod security { mod child; }\n')
        self.write(child, 'pub fn retained() {}\n')
        before = self.scan(self.commit())
        self.assertEqual(before['errors'], [])
        for condition in ('all()', 'unix', 'windows', 'feature="web"',
                          'test', 'all(test, unix)', 'all(test, windows)'):
            with self.subTest(condition=condition):
                self.write(source, 'mod security { mod child;\n#[cfg(' + condition +
                           ')]\n#[path="child.rs"]\nmod sibling;\n}\n')
                after = self.scan(self.commit())
                self.assertEqual(after['errors'], [])
                attributes = [a for row in after['files'] if row['path'] == source
                              for a in row['semantic_attributes'] if a['kind'] == 'path']
                test_only = 'test' in condition
                self.assertEqual([a['classification'] for a in attributes],
                                 ['tests' if test_only else 'production'])
                delta = compare(self.repo, before, after)
                self.assertEqual(delta['literal_ratchet_increases'], [])
                self.assertEqual(len(delta['semantic_attribute_additions']),
                                 0 if test_only else 1)
                if test_only:
                    check_ratchets(delta)
                else:
                    with self.assertRaisesRegex(ValueError, 'source ratchet RED'):
                        check_ratchets(delta)
                directory = self.repo / '.git'
                for name, report in (('before', before), ('after', after)):
                    (directory / (name + '.json')).write_text(canonical(report))
                result = subprocess.run([sys.executable, str(Path(__file__).with_name('compare.py')),
                    '--repo', str(self.repo), '--before', str(directory / 'before.json'),
                    '--after', str(directory / 'after.json'),
                    '--output', str(directory / 'delta.json'), '--check-ratchets'],
                    capture_output=True, text=True)
                self.assertEqual(result.returncode, 0 if test_only else 1, result.stderr)

    def test_cli_size_ratchet_and_reduction_checks_fail(self):
        self.write('src/lib.rs', '// #[path = and #[allow(dead_code)]\n' * 600)
        revision = self.commit()
        declaration = self.repo / 'declarations.json'
        declaration.write_text(canonical(self.p))
        output = self.repo / 'report.json'
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('inventory.py')),
            '--repo', str(self.repo), '--revision', revision, '--parser', str(parser()),
            '--declarations', str(declaration), '--output', str(output), '--check-size'],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 1, result.stderr)
        after = json.loads(output.read_text())
        self.assertEqual(sum(r['occurrences'] for r in after['ratchets']['path_literal']), 600)
        command = [sys.executable, str(Path(__file__).with_name('compare.py')),
            '--repo', str(self.repo), '--before', str(output), '--after', str(output),
            '--output', str(self.repo / 'delta.json'), '--claim-reduction', '1']
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 1)

    def test_unknown_file_unreferenced_rust_and_new_document_fail(self):
        self.write('src/lib.rs', 'pub fn f() {}')
        self.write('src/orphan.rs', '#[test] fn t() {}')
        self.write('new.xyz', 'unknown')
        report = self.scan(self.commit())
        self.assertIn('unclassified Rust: src/orphan.rs', report['errors'])
        self.assertIn('unclassified file: new.xyz', report['errors'])
        self.write('NEW.md', '# New')
        with self.assertRaisesRegex(ValueError, 'undocumented'):
            self.scan(self.commit())

    def test_release_linker_retention_has_exact_test_classification(self):
        path = 'tests/release_linker_retention_spec.py'
        root = Path(__file__).resolve().parents[2]
        source = (root / path).read_text()
        self.write(path, source)
        declarations = json.loads(Path(__file__).with_name('declarations.json').read_text())

        def consume():
            return scan(self.repo, self.commit(), parser(), declarations,
                        canonical(declarations).encode(), parsed={})

        report = consume()
        self.assertEqual(report['errors'], [])
        row = next(row for row in report['files'] if row['path'] == path)
        self.assertEqual(row['counts'], {'tests': len(source.splitlines())})
        self.assertEqual(validate_declarations(declarations, {})[path], 'tests')
        unknown = 'tests/release_linker_retention_unknown_spec.py'
        self.write(unknown, source)
        self.assertEqual(consume()['errors'], ['unclassified file: ' + unknown])

    def test_freshness_and_invalid_class_fail(self):
        self.write('src/lib.rs', '')
        self.write('doc.md', '# Reference')
        revision = self.commit()
        p = copy.deepcopy(self.p)
        p['documents']['doc.md'] = {'role':'reference', 'source':['doc.md'],
            'owner':'maintainer', 'consumers':['declared maintainers'],
            'basis':'maintained declaration', 'freshness':'inventory', 'source_blobs':['wrong']}
        p['files']['documentation'] = ['doc.md']
        with self.assertRaisesRegex(ValueError, 'stale'):
            self.scan(revision, p)
        p['files']['unknown'] = ['new']
        with self.assertRaisesRegex(ValueError, 'unknown class'):
            self.scan(revision, p)

    def test_shared_documents_preserve_rows_and_fail_closed(self):
        self.write('src/lib.rs', '')
        self.write('doc.md', '# Reference')
        revision = self.commit()
        blob = subprocess.check_output(['git', '-C', str(self.repo), 'rev-parse', revision+':doc.md']).decode().strip()
        responsibility = dict(role='reference', owner='maintainer', consumers=['reviewers'],
                              basis='maintained declaration', freshness='exact source')
        p = copy.deepcopy(self.p)
        p.update(version=2, document_groups=[dict(responsibility=responsibility, members={'doc.md': [blob]})])
        p['files']['documentation'] = ['doc.md']
        expanded = expand_declarations(p)
        self.assertEqual(expanded['documents']['doc.md'], dict(responsibility, source=['doc.md'], source_blobs=[blob]))
        self.assertEqual(self.scan(revision, p)['files'], self.scan(revision, expanded)['files'])
        self.assertNotIn('doc.md', p['documents'])
        changes = [lambda q: q['document_groups'].append(q['document_groups'][0]),
                   lambda q: q['documents'].update(expanded['documents']),
                   lambda q: q['document_groups'][0]['responsibility'].update(role=''),
                   lambda q: q['document_groups'][0]['responsibility'].update(source=['other']),
                   lambda q: q['document_groups'][0]['members'].update({'doc.md': []}),
                   lambda q: q['document_groups'][0]['members'].update({'doc.md': ['wrong']}),
                   lambda q: q['document_groups'][0]['members'].update({'doc.md': ['0'*40]})]
        for change in changes:
            q = copy.deepcopy(p)
            change(q)
            with self.subTest(policy=q), self.assertRaises(ValueError):
                self.scan(revision, q)

    def test_integrated_proto_readme_freshness(self):
        self.assert_integrated_declarations({
            'crates/rmux-proto/README.md':
                ('documentation', '271e115bf96cbead7623b44bf5004c98c68b7ada'),
        })

    def test_integrated_proto_registry_declaration(self):
        self.assert_integrated_declarations({
            'crates/rmux-proto/REGISTRY.md': ('documentation', None),
        })

    def test_integrated_tmux_decisions_freshness(self):
        self.assert_integrated_declarations({
            'docs/tmux-compat-decisions.md':
                ('documentation', '434937d28ffac2e7425a89c402d33ff3841bf977'),
        })

    def test_integrated_reference_contracts_freshness(self):
        self.assert_integrated_declarations({
            'docs/validation/reference-contracts.json':
                ('specifications', '70f1c32276b9cdfc502267d039b70a655fb0850e'),
        })

    def test_integrated_reference_applicability_freshness(self):
        self.assert_integrated_declarations({
            'scripts/validation/reference_applicability.py':
                ('tooling', 'a6d33f2fd332bdf78a7e2cc7b9108c3075619a93'),
        })

    def test_integrated_validation_contracts_freshness(self):
        self.assert_integrated_declarations({
            'scripts/validation/contracts.py':
                ('tooling', '372798e14c6f692a899f7c05c32203df40807f35'),
        })

    def test_integrated_cutoff_and_options_declarations(self):
        self.assert_integrated_declarations({
            'tests/OPTIONS-BANK.md':
                ('documentation', '00ecf9f1094068b2326ee9a7cb26da8d4691b85e'),
            'scripts/bench/INSTRUMENTATION-MIGRATION.md':
                ('documentation', '696e7414146449c0d685b679fe0ffa095f5fa1d7'),
            'scripts/bench/cutoff_assets.py': ('tooling', None),
            'scripts/bench/windows_cutoff_consumer.py': ('tooling', None),
            'scripts/bench/test_cutoff_consumers.py': ('tests', None),
            'tests/options_bank_linux.py': ('tests', None),
        })

    def test_integrated_darwin_and_applicability_declarations(self):
        self.assert_integrated_declarations({
            'benches/perf/qualification/DARWIN-QUALIFICATION.md':
                ('documentation', '9d6932fb50e32711f70e6154900b80bb9e97d091'),
            'docs/validation/reference-applicability.json':
                ('specifications', 'e4b31fec7da9065ce65dc2b4af071ae45f8a7f77'),
            'docs/validation/reference-contracts.json':
                ('specifications', '70f1c32276b9cdfc502267d039b70a655fb0850e'),
            'scripts/validation/reference_applicability.py':
                ('tooling', 'a6d33f2fd332bdf78a7e2cc7b9108c3075619a93'),
            'benches/perf/qualification/native_history_canary.py': ('tests', None),
            'tests/reference_applicability_spec.py': ('tests', None),
        })

    def test_integrated_population_archive_declaration(self):
        self.assert_integrated_declarations({
            'benches/perf/qualification/test_population_archive.py':
                ('tests', '88adc33d7f214a3fd6f22f1b6e4dfdf4e19ba6b8'),
        })

    def test_integrated_options_and_budget_freshness(self):
        self.assert_integrated_declarations({
            'tests/OPTIONS-BANK.md':
                ('documentation', 'd3ee6998c712622c851682fb85bc40a37ee29f55'),
            'tests/options_bank_linux.py':
                ('tests', '75421c1bb8782368f142e18e55e9e6df1a96e743'),
        })

    def test_integrated_per_option_adapter_freshness(self):
        # Exact tracked adapter blob from INT142, after per-option consumers
        # and collection of owned children across all daemon threads.
        path = 'tests/options_bank_linux.py'
        blob = 'b53c0dc2e92cdc6a657baf6d4c01a3ca32f0bbcd'
        p = json.loads(Path(__file__).with_name('declarations.json').read_text())
        assigned = validate_declarations(p, {path: {'blob': blob}})
        self.assertEqual(assigned[path], 'tests')
        document = expand_declarations(p)['documents'][path]
        self.assertEqual(document['source'], [path])
        self.assertIn('per-option', document['role'])
        with self.assertRaisesRegex(ValueError, 'stale document declaration: ' + path):
            validate_declarations(p, {path: {'blob': '0' * 40}})

    def test_integrated_exact_option_consumer_classifications(self):
        self.assert_integrated_declarations({
            'tests/options_bank_linux_alerts.py': ('tests', None),
            'tests/options_bank_linux_applications.py': ('tests', None),
            'tests/options_bank_linux_commands.py': ('tests', None),
            'tests/options_bank_linux_controls.py': ('tests', None),
            'tests/options_bank_linux_copy.py': ('tests', None),
            'tests/options_bank_linux_history.py': ('tests', None),
            'tests/options_bank_linux_keys.py': ('tests', None),
            'tests/options_bank_linux_layout.py': ('tests', None),
            'tests/options_bank_linux_lifetime.py': ('tests', None),
            'tests/options_bank_linux_raw.py': ('tests', None),
            'tests/options_bank_linux_recipe.py': ('tests', None),
            'tests/options_bank_linux_screen.py': ('tests', None),
            'tests/options_bank_linux_status.py': ('tests', None),
            'tests/options_bank_linux_terminal.py': ('tests', None),
            'tests/options_bank_linux_timers.py': ('tests', None),
            'tests/options_bank_linux_title.py': ('tests', None),
            'tests/options_bank_linux_ui.py': ('tests', None),
            'tests/options_bank_linux_windows.py': ('tests', None),
        })

    def assert_integrated_declarations(self, expected):
        # Exact maintained inputs in a Rust-free Git fixture: no parser execution.
        # Copy source bytes; never import canaries or execute their native effects.
        declaration = Path(__file__).with_name('declarations.json')
        p = expand_declarations(json.loads(declaration.read_text()))
        for path in expected:
            self.write(path, (declaration.parents[2] / path).read_text())
        revision = self.commit()

        def consume(declarations):
            return scan(self.repo, revision, parser(), declarations,
                        canonical(declarations).encode(), parsed={})

        report = consume(p)
        self.assertEqual(report['errors'], [])
        rows = {row['path']: row for row in report['files']}
        for path, (role, historical) in expected.items():
            with self.subTest(path=path):
                self.assertEqual(rows[path]['counts'], {role: rows[path]['physical_lines']})
                self.assertEqual(rows[path]['document']['source'], [path])
                self.assertIn(rows[path]['blob'], rows[path]['document']['source_blobs'])
                if historical:
                    self.assertIn(historical, rows[path]['document']['source_blobs'])
            for field, value, error in (
                    ('source_blobs', ['0' * 40], 'stale document declaration'),
                    ('source', ['absent-source'], 'document source absent')):
                q = copy.deepcopy(p)
                q['documents'][path][field] = value
                with self.subTest(path=path, invalid=field), self.assertRaisesRegex(ValueError, error):
                    consume(q)
            q = copy.deepcopy(p)
            q['files'][role].remove(path)
            with self.subTest(path=path, invalid='missing classification'):
                self.assertEqual(consume(q)['errors'], ['unclassified file: ' + path])

    def test_reclassification_and_minification_never_fund_reduction(self):
        self.write('src/lib.rs', 'pub fn a() {}\npub fn b() {}\n')
        revision = self.commit()
        before = self.scan(revision)
        p = copy.deepcopy(self.p)
        p['crate_roles']['Cargo.toml'] = 'tests'
        after = self.scan(revision, p)
        delta = compare(self.repo, before, after)
        self.assertEqual(delta['credited_reduction'], 0)
        self.assertEqual(delta['raw_removed'], {})
        self.assertEqual(delta['unchanged_line_class_transfers'],
                         [{'from': 'production', 'to': 'tests', 'lines': 2}])
        self.assertTrue(delta['class_totals_reconciled'])
        self.assertEqual(delta['source_unchanged_reclassified'], ['src/lib.rs'])
        self.write('src/lib.rs', 'pub fn a() {} pub fn b() {}\n')
        after = self.scan(self.commit())
        delta = compare(self.repo, before, after)
        self.assertEqual(delta['credited_reduction'], 0)
        self.assertGreater(delta['raw_removed']['production'], 0)

    def test_new_maintained_tooling_and_exact_size_boundary(self):
        self.write('src/lib.rs', '// production\n' * 599)
        before = self.scan(self.commit())
        self.assertFalse(any(c['production_ge_600'] for c in before['crates'].values()))
        self.write('src/lib.rs', '// production\n' * 600)
        self.write('generator.py', 'print(1)\nprint(2)')
        self.p['files']['tooling'].append('generator.py')
        after = self.scan(self.commit())
        self.assertTrue(any(c['production_ge_600'] for c in after['crates'].values()))
        delta = compare(self.repo, before, after)
        self.assertEqual(delta['maintained_added_outside_production'], 2)

    def test_checkout_path_and_ignored_output_do_not_change_report(self):
        self.write('src/lib.rs', '#[cfg(test)] fn t() {}\npub fn p() {}')
        revision = self.commit()
        first = self.scan(revision)
        self.write('untracked.rs', 'not valid Rust')
        second = self.scan(revision)
        clone = self.repo / 'different-checkout'
        subprocess.run(['git', 'clone', '-q', '--no-hardlinks', str(self.repo), str(clone)], check=True)
        third = scan(clone, revision, parser(), self.p, canonical(self.p).encode())
        self.assertEqual(canonical(first), canonical(second))
        self.assertEqual(canonical(first), canonical(third))
        self.assertEqual(first['errors'], [])


ROOT = Path(__file__).resolve().parents[2]
SOURCES = (
    "crates/rmux-server/src/handler/dispatch_support.rs",
    "crates/rmux-server/src/handler_attach_tests.rs",
)
FROZEN_PATH_LITERAL_BUDGET = 44


class DispatchMetadataTests(unittest.TestCase):
    def test_dispatch_extraction_preserves_path_literal_budget(self):
        # Use the same literal unit as the canonical source inventory.
        actual = sum((ROOT / path).read_bytes().count(b'#[path =') for path in SOURCES)
        self.assertLessEqual(actual, FROZEN_PATH_LITERAL_BUDGET)


if __name__ == '__main__':
    unittest.main()
