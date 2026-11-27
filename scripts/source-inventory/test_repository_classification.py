"""The maintained repository must have a complete, explicit source inventory."""
import copy
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from compare import compare
from inventory import contexts, parser_scan, scan
from model import read_tree, validate_declarations
from relocation_chain import EPOCH


def source(*includes, modules=()):
    return {'errors': [], 'regions': [], 'modules': list(modules),
            'includes': [{'tokens': tokens, 'offset': 0} for tokens in includes]}


def manifest_include(relative):
    return f'concat ! (env ! ("CARGO_MANIFEST_DIR") , "{relative}")'


class ManifestIncludeTests(unittest.TestCase):
    MANIFEST = 'crates/a/Cargo.toml'
    ROOT = 'crates/a/src/lib.rs'
    SHARED = 'tests/support/shared.rs'

    def resolve(self, tokens, consumer=MANIFEST, declared=None):
        parsed = {self.ROOT: source(tokens), self.SHARED: source()}
        files = dict.fromkeys([self.MANIFEST, self.ROOT, self.SHARED], {})
        errors = []
        found = contexts(files, parsed, [(self.ROOT, 'production', True, consumer)],
                         {'includes': declared or {}}, errors)
        return dict(found), errors

    def test_resolves_real_path_from_the_compiling_crate(self):
        found, errors = self.resolve(manifest_include('/../../tests/support/shared.rs'))
        self.assertEqual(errors, [])
        self.assertEqual([row['consumers'] for row in found[self.SHARED].values()],
                         [{self.ROOT + ':include'}])

    def test_root_package_resolves_and_cannot_escape_repository(self):
        parsed = {'src/lib.rs': source(manifest_include('/tests/support/shared.rs')),
                  'tests/support/shared.rs': source()}
        files = dict.fromkeys(['Cargo.toml', *parsed], {})
        errors = []
        found = contexts(files, parsed, [('src/lib.rs', 'production', True, 'Cargo.toml')],
                         {'includes': {}}, errors)
        self.assertEqual(errors, [])
        self.assertIn('tests/support/shared.rs', found)
        _, errors = self.resolve(manifest_include('/../../../outside.rs'))
        self.assertEqual(errors, ['path escapes repository: crates/a/../../../outside.rs'])

    def test_shared_module_resolves_for_each_compiling_crate(self):
        shared_module = 'shared/inner.rs'
        module = {'name': 'inner', 'path': '../../../' + shared_module, 'inline': [], 'offset': 0}
        roots = ['crates/a/src/lib.rs', 'crates/b/src/lib.rs']
        parsed = {path: source(modules=[module]) for path in roots}
        parsed[shared_module] = source(manifest_include('/data.rs'))
        parsed['crates/a/data.rs'] = source()
        parsed['crates/b/data.rs'] = source()
        files = dict.fromkeys([*parsed, 'crates/a/Cargo.toml', 'crates/b/Cargo.toml'], {})
        for path_type in (PurePosixPath, PureWindowsPath):
            with self.subTest(path_type=path_type.__name__):
                errors = []
                # Inventory keys are Git paths, independent of the host OS.
                found = contexts(files, parsed,
                                 [(root, 'production', True,
                                   (path_type(root).parents[1] / 'Cargo.toml').as_posix())
                                  for root in roots], {'includes': {}}, errors)
                self.assertEqual(errors, [])
                self.assertIn('crates/a/data.rs', found)
                self.assertIn('crates/b/data.rs', found)

    def test_missing_target_is_rejected(self):
        _, errors = self.resolve(manifest_include('/absent.rs'))
        self.assertEqual(errors, [self.ROOT + ': missing include crates/a/absent.rs'])

    def test_unresolved_forms_stay_refused(self):
        literal = '"/../../tests/support/shared.rs"'
        for tokens in (f'concat ! (env ! ("OUT_DIR") , {literal})',
                       f'concat ! (env ! ("CARGO_MANIFEST_DIR" , "x") , {literal})',
                       f'concat ! (option_env ! ("CARGO_MANIFEST_DIR") , {literal})',
                       f'concat ! (env ! ("CARGO_MANIFEST_DIR") , {literal} ,)',
                       f'concat ! (env ! ("CARGO_MANIFEST_DIR") , {literal} , "x")',
                       f'std :: concat ! (env ! ("CARGO_MANIFEST_DIR") , {literal})',
                       'concat ! (env ! ("CARGO_MANIFEST_DIR") , "tests/support/shared.rs")',
                       'concat ! (env ! ("CARGO_MANIFEST_DIR") , "/x\\u{2e}rs")',
                       'concat ! (env ! ("CARGO_MANIFEST_DIR") , r"/x.rs")'):
            with self.subTest(tokens=tokens):
                found, errors = self.resolve(tokens)
                self.assertEqual(errors, [f'{self.ROOT}: unsupported include {tokens}'])
                self.assertNotIn(self.SHARED, found)

    def test_standalone_root_has_no_crate_to_resolve_against(self):
        tokens = manifest_include('/../../tests/support/shared.rs')
        found, errors = self.resolve(tokens, consumer='maintained standalone evidence')
        self.assertEqual(errors, [f'{self.ROOT}: unsupported include {tokens}'])
        self.assertNotIn(self.SHARED, found)
        declared = {f'{self.ROOT}::{tokens}': {'evidence': 'reviewed', 'path': self.SHARED}}
        found, errors = self.resolve(tokens, consumer='maintained standalone evidence',
                                     declared=declared)
        self.assertEqual(errors, [])
        self.assertIn(self.SHARED, found)


class RepositoryClassificationTests(unittest.TestCase):
    def test_bench_collect_has_exact_test_role_and_preserves_tooling_neighbors(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'scripts/test_bench_collect.py'
        expected = {path: 'tests', 'scripts/bench/collect.py': 'tooling',
                    'scripts/bench/render.py': 'tooling',
                    'scripts/bench/test_cutoff_consumers.py': 'tests'}
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        tracked = read_tree(repo, revision)
        files = {item: tracked[item] for item in expected}
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())

        def report(current, sources):
            with patch('inventory.read_tree', return_value=sources):
                return scan(repo, revision, None, current, json.dumps(current).encode(), parsed={})

        result = report(policy, files)
        self.assertEqual(result['errors'], [])
        self.assertEqual(validate_declarations(policy, files).get(path), 'tests')
        for row in result['files']:
            with self.subTest(path=row['path']):
                count = len(files[row['path']]['data'].split(b'\n')) - 1
                count += bool(files[row['path']]['data'] and
                              not files[row['path']]['data'].endswith(b'\n'))
                self.assertEqual(row['counts'], {expected[row['path']]: count})
                self.assertEqual(row['regions'],
                                 [{'start': 1, 'end': count, 'class': expected[row['path']]}])
        withdrawn = copy.deepcopy(policy)
        withdrawn['files']['tests'].remove(path)
        self.assertEqual(report(withdrawn, files)['errors'], ['unclassified file: ' + path])
        unknown = 'scripts/test_bench_collect_undeclared.py'
        self.assertEqual(report(policy, {**files, unknown: tracked[path]})['errors'],
                         ['unclassified file: ' + unknown])

    def test_production_modules_use_normal_rust_layout(self):
        repo = Path(__file__).resolve().parents[2]
        revision = os.environ.get('SOURCE_RATCHET_REVISION') or subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        raw = (repo / 'scripts/source-inventory/declarations.json').read_bytes()
        report = scan(repo, revision, Path(os.environ['SOURCE_SYNTAX']).resolve(),
                      json.loads(raw), raw)
        self.assertEqual(report['errors'], [], '\n'.join(report['errors']))
        paths = [(row['path'], attribute['value']) for row in report['files']
                 for attribute in row.get('semantic_attributes', [])
                 if attribute['kind'] == 'path'
                 and attribute['classification'] == 'production']
        self.assertEqual(paths, [], 'production modules must use normal Rust layout')

    def test_startup_stderr_order_has_exact_test_role_and_source_binding(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'tests/startup_stderr_order_spec.py'
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        tracked = read_tree(repo, revision)
        policy = json.loads(tracked['scripts/source-inventory/declarations.json']['data'])
        files = {path: tracked[path]}

        def report(current, sources=files):
            self.assertIn(path, current['documents'])
            with patch('inventory.read_tree', return_value=sources):
                return scan(repo, revision, None, current, json.dumps(current).encode(), parsed={})

        self.assertEqual(report(policy)['errors'], [])
        self.assertEqual(validate_declarations(policy, files).get(path), 'tests')
        self.assertEqual(report(policy)['files'][0]['counts'],
                         {'tests': len(tracked[path]['data'].splitlines())})
        row = policy['documents'][path]
        self.assertEqual(row['source'], [path])
        self.assertEqual(row['source_blobs'], [tracked[path]['blob']])
        withdrawn = copy.deepcopy(policy)
        withdrawn['files']['tests'].remove(path)
        self.assertEqual(report(withdrawn)['errors'], ['unclassified file: ' + path])
        unknown = path.removesuffix('.py') + '_undeclared.py'
        self.assertEqual(report(policy, {unknown: tracked[path]})['errors'],
                         ['unclassified file: ' + unknown])
        for mutation in ('absent', 'missing_blob', 'wrong_blob', 'wrong_source'):
            changed = copy.deepcopy(policy)
            if mutation == 'absent':
                del changed['documents'][path]
                with self.assertRaises(AssertionError):
                    report(changed)
                continue
            if mutation == 'missing_blob':
                del changed['documents'][path]['source_blobs']
                error = 'incomplete document declaration'
            elif mutation == 'wrong_blob':
                changed['documents'][path]['source_blobs'] = ['0' * 40]
                error = 'stale document declaration'
            else:
                changed['documents'][path]['source'] = [path + '.absent']
                error = 'document source absent'
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(ValueError, error + ': ' + path):
                    report(changed)
        for blob in ('0' * 40, tracked['tests/pin_revisions_spec.py']['blob']):
            with self.subTest(substituted=blob):
                with self.assertRaisesRegex(ValueError, 'stale document declaration: ' + path):
                    report(policy, {path: dict(tracked[path], blob=blob)})

    def test_copy_top_oracle_fixture_has_exact_specification_role(self):
        repo = Path(__file__).resolve().parents[2]
        prefix = 'tests/tmux_compat_surface_matrix/copy_viewport/fixtures/'
        path = prefix + 'macos-bash-top.json'
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        tracked = read_tree(repo, revision)
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        paths = (path, prefix + 'r1-xterm.json', prefix + 'r1.hex')
        files = {item: tracked[item] for item in paths}
        assigned = validate_declarations(policy, files)
        self.assertEqual({item: assigned.get(item) for item in paths},
                         dict.fromkeys(paths, 'specifications'))

        def report(current, sources):
            with patch('inventory.read_tree', return_value=sources):
                return scan(repo, revision, None, current, json.dumps(current).encode(), parsed={})

        result = report(policy, files)
        self.assertEqual(result['errors'], [])
        for row in result['files']:
            self.assertEqual(row['counts'], {'specifications': row['physical_lines']})
        withdrawn = copy.deepcopy(policy)
        withdrawn['files']['specifications'].remove(path)
        self.assertEqual(report(withdrawn, files)['errors'], ['unclassified file: ' + path])
        unknown = prefix + 'macos-bash-top-undeclared.json'
        self.assertEqual(report(policy, {unknown: tracked[path]})['errors'],
                         ['unclassified file: ' + unknown])

    def test_shared_daemon_fd_helper_is_test_only_through_both_real_consumers(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'tests/support/daemon_fd_inheritance.rs'
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        raw = (repo / 'scripts/source-inventory/declarations.json').read_bytes()
        report = scan(repo, revision, Path(os.environ['SOURCE_SYNTAX']).resolve(),
                      json.loads(raw), raw)
        row = next(item for item in report['files'] if item['path'] == path)
        self.assertEqual(set(row['counts']), {'tests'})
        self.assertEqual({consumer for context in row['consumers']
                          for consumer in context['consumers']},
                         {'crates/rmux-client/src/auto_start/tests.rs:include',
                          'crates/rmux-sdk/src/handles/rmux/connect/startup_config.rs:include'})

    def test_packaging_sources_have_exact_roles_and_reject_unclassified_siblings(self):
        repo = Path(__file__).resolve().parents[2]
        expected = {
            'scripts/package_fixture_helpers.py': 'tests',
            'scripts/packaging/package_payloads.py': 'tooling',
            'scripts/test_cutoff_compressed_leaves.py': 'tests',
            'scripts/test_cutoff_compressed_leaves_acceptance.py': 'tests',
            'scripts/test_cutoff_packages.py': 'tests',
            'scripts/test_cutoff_packages_acceptance.py': 'tests',
            'scripts/test_local_package_sbom.py': 'tests',
        }
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        tracked = read_tree(repo, revision)
        files = {path: tracked[path] for path in expected}

        def report(current, sources):
            with patch('inventory.read_tree', return_value=sources):
                return scan(repo, revision, None, current, json.dumps(current).encode(), parsed={})

        for path, role in expected.items():
            with self.subTest(path=path):
                self.assertEqual(validate_declarations(policy, files).get(path), role)
                row = policy['documents'][path]
                self.assertEqual(row['source'], [path])
                self.assertEqual(row['source_blobs'], [files[path]['blob']])
                self.assertTrue(row['owner'])
                self.assertTrue(row['consumers'])
                self.assertTrue(all(consumer in tracked for consumer in row['consumers']))
                changed = copy.deepcopy(policy)
                changed['files'][role].remove(path)
                self.assertEqual(report(changed, {path: files[path]})['errors'],
                                 ['unclassified file: ' + path])
                sibling = path.removesuffix('.py') + '_undeclared.py'
                self.assertEqual(report(policy, {sibling: files[path]})['errors'],
                                 ['unclassified file: ' + sibling])
                other = next(item for item in expected if item != path)
                for blob in ('0' * 40, files[other]['blob']):
                    with self.subTest(blob=blob):
                        with self.assertRaisesRegex(ValueError, 'stale document declaration: ' + path):
                            validate_declarations(policy, {path: dict(files[path], blob=blob)})
        self.assertEqual(report(policy, files)['errors'], [])

    def test_cutoff_assets_declaration_binds_current_blob_and_rejects_substitution(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'scripts/bench/cutoff_assets.py'
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        blob = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD:' + path], text=True).strip()
        files = {path: {'blob': blob}}
        self.assertEqual(validate_declarations(policy, files).get(path), 'tooling')
        self.assertEqual(policy['documents'][path]['source'], [path])
        changed = copy.deepcopy(policy)
        changed['documents'][path]['source_blobs'].remove(blob)
        with self.assertRaisesRegex(ValueError, 'stale document declaration: ' + path):
            validate_declarations(changed, files)
        sibling = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse',
             'HEAD:scripts/bench/windows_cutoff_consumer.py'], text=True).strip()
        for substituted in ('0' * 40, sibling):
            with self.subTest(substituted=substituted):
                with self.assertRaisesRegex(ValueError, 'stale document declaration: ' + path):
                    validate_declarations(policy, {path: {'blob': substituted}})

    def test_composed_packaging_and_identity_sources_have_verified_roles(self):
        repo = Path(__file__).resolve().parents[2]
        expected = {
            'scripts/check_feature_powerset.py': 'tooling',
            'scripts/test_candidate_creation_identity.py': 'tests',
            'scripts/test_candidate_creation_identity_neighbors.py': 'tests',
            'scripts/test_candidate_creation_stdlib.py': 'tests',
            'scripts/test_candidate_gate_cwd.py': 'tests',
            'scripts/test_perf_three_images.py': 'tests',
            'scripts/test_sdk_migration_lock.py': 'tests',
            'scripts/test_verify_release_ref_http.py': 'tests',
            'tests/format_jobs_reference/test_producer_ownership.py': 'tests',
            'tests/format_jobs_reference/test_producer_paths.py': 'tests',
            'tests/input_paste_escape_darwin_partial_read_checks.py': 'tests',
            'tests/package_executable_paths_spec.py': 'tests',
            'tests/package_sibling_executable_paths_spec.py': 'tests',
            'tests/package_tar_receipts_spec.py': 'tests',
            'tests/renumber_windows_ownership_tests.py': 'tests',
            'tests/retained_fixture_isolation_spec.py': 'tests',
            'tests/winget_layout_fixture_spec.py': 'tests',
            'scripts/test_candidate_authenticated_generation.py': 'tests',
            'scripts/test_candidate_authenticated_probe.py': 'tests',
            'scripts/test_candidate_identity_lock_closure.py': 'tests',
            'scripts/test_candidate_identity_multimanifest.py': 'tests',
            'scripts/test_candidate_legacy_boundary.py': 'tests',
            'scripts/test_candidate_legacy_generation.py': 'tests',
            'scripts/package_fixture_helpers.py': 'tests',
            'scripts/packaging/package_payloads.py': 'tooling',
            'scripts/test_candidate_identity_linker.py': 'tests',
            'scripts/test_candidate_stamp_cache_boundaries.py': 'tests',
            'scripts/test_candidate_stamp_cache_cleanup.py': 'tests',
            'scripts/test_candidate_stamp_cache_concurrency.py': 'tests',
            'scripts/test_candidate_stamp_cache_publication.py': 'tests',
            'scripts/test_candidate_stamp_cache_publish_io.py': 'tests',
            'scripts/test_candidate_stamp_cache_transaction.py': 'tests',
            'scripts/test_cutoff_compressed_leaves.py': 'tests',
            'scripts/test_cutoff_compressed_leaves_acceptance.py': 'tests',
            'scripts/test_cutoff_packages.py': 'tests',
            'scripts/test_cutoff_packages_acceptance.py': 'tests',
            'scripts/test_feature_powerset.py': 'tests',
            'scripts/test_local_package_sbom.py': 'tests',
            'scripts/test_local_package_sbom_zstd.py': 'tests',
            'tests/reference/tmux_compat/R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/contract-evidence.json': 'specifications',
            'tests/reference/tmux_compat/R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/oracle-independent.json': 'specifications',
        }
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        files = read_tree(repo, revision)
        assigned = validate_declarations(policy, files)
        for path, role in expected.items():
            with self.subTest(path=path):
                self.assertIn(path, files)
                self.assertIn(path, policy['files'][role])
                self.assertEqual(assigned.get(path), role)
                changed = copy.deepcopy(policy)
                changed['files'][role].remove(path)
                with patch('inventory.read_tree', return_value={path: files[path]}):
                    report = scan(repo, revision, None, changed,
                                  json.dumps(changed).encode(), parsed={})
                self.assertEqual(report['errors'], ['unclassified file: ' + path])

    def test_release_policy_declaration_tracks_committed_source(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'tests/release_policy_reachability_spec.py'
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        files = read_tree(repo, revision)
        policy = json.loads(files['scripts/source-inventory/declarations.json']['data'])
        row = policy['documents'][path]
        self.assertEqual(row['source'], [path])
        self.assertEqual(row['source_blobs'], [files[path]['blob']])
        validate_declarations(policy, {path: files[path]})

    def test_installed_manual_source_binding_rejects_tampering(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'docs/man/rmux.1'
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        blob = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD:' + path], text=True).strip()
        files = {path: {'blob': blob}}
        row = policy['documents'][path]
        self.assertEqual(row['source'], [path])
        self.assertIn(blob, row['source_blobs'])
        self.assertEqual(validate_declarations(policy, files)[path], 'documentation')
        for mutation in ('withdrawn', 'altered', 'missing_source', 'missing_blobs'):
            changed = copy.deepcopy(policy)
            current = changed['documents'][path]
            error = 'stale document declaration'
            if mutation == 'withdrawn':
                current['source_blobs'].remove(blob)
            elif mutation == 'altered':
                current['source_blobs'] = ['0' * 40]
            elif mutation == 'missing_source':
                current['source'] = [path + '.absent']
                error = 'document source absent'
            else:
                del current['source_blobs']
                error = 'incomplete document declaration'
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(ValueError, error + ': ' + path):
                    validate_declarations(changed, files)

    def test_option_registry_sources_have_exact_roles_and_blob_bindings(self):
        repo = Path(__file__).resolve().parents[2]
        expected = {
            'scripts/regression_registry_options.py': 'tooling',
            'scripts/regression_registry_options_flags.json': 'specifications',
            'scripts/test_regression_registry_options.py': 'tests',
        }
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        tracked = read_tree(repo, revision)
        files = {path: tracked[path] for path in expected}

        def report(current, sources=files):
            with patch('inventory.read_tree', return_value=sources):
                return scan(repo, revision, None, current, json.dumps(current).encode(), parsed={})

        self.assertEqual(report(policy)['errors'], [])
        assigned = validate_declarations(policy, files)
        self.assertEqual({path: assigned.get(path) for path in expected}, expected)
        for path, role in expected.items():
            with self.subTest(path=path):
                row = policy['documents'][path]
                self.assertEqual(row['source'], [path])
                self.assertEqual(row['source_blobs'], [files[path]['blob']])
                self.assertTrue(row['owner'])
                self.assertTrue(row['consumers'])
                changed = copy.deepcopy(policy)
                changed['files'][role].remove(path)
                self.assertEqual(report(changed)['errors'], ['unclassified file: ' + path])
                unknown = path + '.unknown'
                self.assertEqual(report(policy, dict(files, **{unknown: files[path]}))['errors'],
                                 ['unclassified file: ' + unknown])
                for mutation in ('missing_blob', 'altered', 'exchanged', 'absent'):
                    changed = copy.deepcopy(policy)
                    sources = copy.deepcopy(files)
                    if mutation == 'missing_blob':
                        del changed['documents'][path]['source_blobs']
                        error = 'incomplete document declaration'
                    elif mutation == 'altered':
                        changed['documents'][path]['source_blobs'] = ['0' * 40]
                        error = 'stale document declaration'
                    elif mutation == 'exchanged':
                        other = next(item for item in expected if item != path)
                        sources[path]['blob'] = files[other]['blob']
                        error = 'stale document declaration'
                    else:
                        changed['documents'][path]['source'] = [path + '.absent']
                        error = 'document source absent'
                    with self.subTest(mutation=mutation):
                        with self.assertRaisesRegex(ValueError, error + ': ' + path):
                            validate_declarations(changed, sources)

    def test_explicit_non_document_sources_reject_missing_or_exchanged_blobs(self):
        self.maxDiff = None
        repo = Path(__file__).resolve().parents[2]
        expected = {
            '.cargo/config.toml': 'tooling',
            'benches/perf/qualification/pin-revision-proofs/R2-D155-PIN-SCOPE-INDEPENDENT-PROOF-223/PROOF.json': 'specifications',
            'benches/perf/qualification/pin-revisions.json': 'specifications',
            'benches/perf/qualification/test_population_epoch.py': 'tests',
            'scripts/bench/frozen_ast.py': 'tooling',
            'scripts/bench/screen_environment.py': 'tooling',
            'scripts/pin_revisions.py': 'tooling',
            'scripts/release/test_publication_alias_reopen.py': 'tests',
            'scripts/release/test_publication_inert_alias.py': 'tests',
            'tests/bench_screen_environment_spec.py': 'tests',
            'tests/bench_screen_startup_spec.py': 'tests',
            'tests/fixtures/pin_revision_definitions.py': 'tests',
            'tests/pin_activation_controls.py': 'tests',
            'tests/pin_revision_sources.py': 'tests',
            'tests/pin_revisions_spec.py': 'tests',
            'tests/release_policy_reachability_spec.py': 'tests',
            'xtask/spec/test_registry_docs_entrypoint.py': 'tests',
        }
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        tracked = read_tree(repo, revision)
        files = {path: tracked[path] for path in expected}

        def report(current):
            with patch('inventory.read_tree', return_value=files):
                return scan(repo, revision, None, current, json.dumps(current).encode(), parsed={})

        self.assertEqual(report(policy)['errors'], [])
        assigned = validate_declarations(policy, files)
        self.assertEqual({path: assigned.get(path) for path in expected}, expected)
        for path, role in expected.items():
            with self.subTest(path=path):
                row = policy['documents'][path]
                self.assertEqual(row['source'], [path])
                self.assertEqual(row['source_blobs'], [files[path]['blob']])
                self.assertTrue(row['owner'])
                self.assertTrue(row['consumers'])
                changed = copy.deepcopy(policy)
                changed['files'][role].remove(path)
                self.assertEqual(report(changed)['errors'], ['unclassified file: ' + path])
                for mutation in ('absent', 'missing_blob', 'altered', 'exchanged'):
                    changed = copy.deepcopy(policy)
                    sources = copy.deepcopy(files)
                    if mutation == 'absent':
                        changed['documents'][path]['source'] = [path + '.absent']
                        error = 'document source absent'
                    elif mutation == 'missing_blob':
                        del changed['documents'][path]['source_blobs']
                        error = 'incomplete document declaration'
                    elif mutation == 'altered':
                        changed['documents'][path]['source_blobs'] = ['0' * 40]
                        error = 'stale document declaration'
                    else:
                        other = next(item for item in expected if item != path)
                        sources[path]['blob'] = files[other]['blob']
                        error = 'stale document declaration'
                    with self.subTest(mutation=mutation):
                        with self.assertRaisesRegex(ValueError, error + ': ' + path):
                            validate_declarations(changed, sources)

    def test_d155_proof_documents_reject_missing_or_exchanged_blobs(self):
        repo = Path(__file__).resolve().parents[2]
        prefix = ('benches/perf/qualification/pin-revision-proofs/'
                  'R2-D155-PIN-SCOPE-INDEPENDENT-PROOF-223/')
        paths = [prefix + name for name in ('PREUVE.md', 'REVUE.md')]
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        files = {path: {'blob': subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD:' + path], text=True).strip()}
                 for path in paths}
        for path in paths:
            with self.subTest(path=path):
                validate_declarations(policy, {path: files[path]})
                for mutation in ('absent', 'missing_blob', 'altered', 'exchanged'):
                    changed = copy.deepcopy(policy)
                    sources = copy.deepcopy(files)
                    if mutation == 'absent':
                        del changed['documents'][path]
                        error = 'undocumented role/owner/consumer'
                    elif mutation == 'missing_blob':
                        del changed['documents'][path]['source_blobs']
                        error = 'incomplete document declaration'
                    elif mutation == 'altered':
                        changed['documents'][path]['source_blobs'] = ['0' * 40]
                        error = 'stale document declaration'
                    else:
                        other = next(item for item in paths if item != path)
                        sources[path]['blob'] = files[other]['blob']
                        error = 'stale document declaration'
                    with self.subTest(mutation=mutation):
                        with self.assertRaisesRegex(ValueError, error + ': ' + path):
                            validate_declarations(changed, {path: sources[path]})

    def test_population_archive_source_declaration_rejects_tampering(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'benches/perf/qualification/test_population_archive.py'
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        blob = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD:' + path], text=True).strip()
        files = {path: {'blob': blob}}
        for mutation in ('missing', 'altered', 'exchanged'):
            changed = copy.deepcopy(policy)
            sources = copy.deepcopy(files)
            row = changed['documents'][path]
            if mutation == 'missing':
                del row['source_blobs']
                error = 'incomplete document declaration'
            elif mutation == 'altered':
                row['source_blobs'] = ['0' * 40]
                error = 'stale document declaration'
            else:
                sources[path]['blob'] = subprocess.check_output(
                    ['git', '-C', str(repo), 'rev-parse',
                     'HEAD:benches/perf/qualification/test_population_epoch.py'],
                    text=True).strip()
                error = 'stale document declaration'
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(ValueError, error + ': ' + path):
                    validate_declarations(changed, sources)
        validate_declarations(policy, files)

    def test_release_ref_inputs_has_explicit_test_role(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'scripts/test_release_ref_inputs.py'
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        files = read_tree(repo, revision)
        self.assertIn(path, files)
        self.assertIn(path, policy['files']['tests'])
        self.assertEqual(validate_declarations(policy, files).get(path), 'tests')
        for current in (policy, copy.deepcopy(policy)):
            if current is not policy:
                current['files']['tests'].remove(path)
            with patch('inventory.read_tree', return_value={path: files[path]}):
                report = scan(repo, revision, None, current, json.dumps(current).encode(), parsed={})
            self.assertEqual(report['errors'], [] if current is policy else
                             ['unclassified file: ' + path])

    def test_popup_status_reference_has_explicit_test_role(self):
        repo = Path(__file__).resolve().parents[2]
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        files = read_tree(repo, revision)
        path = 'tests/popup_status_reference.py'
        self.assertIn(path, files)
        self.assertEqual(validate_declarations(policy, files).get(path), 'tests')

    def test_identity_git_files_have_explicit_roles(self):
        repo = Path(__file__).resolve().parents[2]
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        for path, role in (('scripts/candidate_identity_git.py', 'tooling'),
                           ('scripts/test_candidate_identity_git_path.py', 'tests')):
            with self.subTest(path=path, role=role):
                self.assertIn(path, policy['files'][role])

    def test_queued_reset_fixture_has_no_opaque_conditional_syntax(self):
        repo = Path(__file__).resolve().parents[2]
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        path = 'crates/rmux-server/src/handler_scripting_tests/queue_exact_target_send_keys_reset.rs'
        files = read_tree(repo, revision)
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        result = parser_scan(Path(os.environ['SOURCE_SYNTAX']).resolve(),
                             {path: files[path]}, policy)
        self.assertEqual(result[path]['errors'], [])

    def test_cli_dispatch_does_not_grow_from_reviewed_size_baseline(self):
        repo = Path(__file__).resolve().parents[2]
        head = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        raw = (repo / 'scripts/source-inventory/declarations.json').read_bytes()
        reports = [scan(repo, revision, Path(os.environ['SOURCE_SYNTAX']).resolve(),
                        json.loads(raw), raw)
                   for revision in ('f54ae83954c368c5c4e3706806690e0388c5414e', head)]
        for report in reports:
            self.assertEqual(report['errors'], [], '\n'.join(report['errors']))
        # The baseline predates the D161(1b) activation that HEAD descends from.
        delta = compare(repo, *reports, epoch=EPOCH)
        violations = [row for row in delta['production_size_violations']
                      if row['path'] == 'src/cli/dispatch.rs']
        self.assertEqual(violations, [], 'CLI dispatch exceeded its production size baseline')

    def test_removed_benchmark_assets_remain_classified_in_production_reference(self):
        repo = Path(__file__).resolve().parents[2]
        reference = json.loads((repo / 'scripts/source-inventory/production-reference.json').read_bytes())
        files = read_tree(repo, reference['revision'])
        historical = json.loads(files['scripts/source-inventory/declarations.json']['data'])
        assets = [path for path in historical['files']['generated']
                  if path.startswith('docs/benchmarks/')]
        self.assertEqual(len(assets), 5)
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        assigned = validate_declarations(policy, files)
        self.assertEqual({path: assigned.get(path) for path in assets},
                         dict.fromkeys(assets, 'generated'))
        for path in assets:
            self.assertEqual(policy['provenance'][path], historical['provenance'][path])

    def test_composed_document_declarations_bind_actual_source_blobs(self):
        repo = Path(__file__).resolve().parents[2]
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        validate_declarations(policy, read_tree(repo, revision))

    def test_source_input_documents_bind_current_and_historical_blobs(self):
        repo = Path(__file__).resolve().parents[2]
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        tracked = read_tree(repo, revision)
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        historical = {
            'crates/rmux-proto/README.md': '2b12255c274ab65244c3250fd211ed2954f4f770',
            'docs/release/0.11.0.md': '1946aa6fe58ac261893ef213e664563b2582b0c2',
        }
        for path, previous in historical.items():
            with self.subTest(path=path):
                # Exercise the inventory with exact Git document bytes only;
                # no Rust parser or historical Git object is required.
                files = {path: tracked[path]}
                current = files[path]['blob']
                self.assertNotEqual(current, previous)
                with patch('inventory.read_tree', return_value=files):
                    report = scan(repo, revision, None, policy,
                                  json.dumps(policy).encode(), parsed={})
                self.assertEqual(report['errors'], [])
                row = report['files'][0]
                self.assertEqual(row['counts'], {'documentation': row['physical_lines']})
                self.assertEqual(row['document']['source'], [path])
                self.assertIn(current, row['document']['source_blobs'])
                self.assertIn(previous, row['document']['source_blobs'])
                validate_declarations(policy, {path: {'blob': previous}})
                stale = copy.deepcopy(policy)
                stale['documents'][path]['source_blobs'].remove(current)
                error = '^stale document declaration: ' + path.replace('.', '[.]') + '$'
                with self.assertRaisesRegex(ValueError, error):
                    validate_declarations(stale, files)
                with self.assertRaisesRegex(ValueError, error):
                    validate_declarations(policy, {path: {'blob': '0' * 40}})

    def test_pin_revisions_binding_rejects_stale_declaration_and_source(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'scripts/pin_revisions.py'
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())

        def blob(revision, source=path):
            return subprocess.check_output(
                ['git', '-C', str(repo), 'rev-parse', revision + ':' + source],
                text=True).strip()

        current = blob('HEAD')
        # S1141 changed Git path serialization, without changing the tooling role.
        stale = blob('22b81507f6e2eb08703bfc93cda69fad3e9f1eca^')
        self.assertNotEqual(current, stale)
        files = {path: {'blob': current}}
        self.assertEqual(validate_declarations(policy, files).get(path), 'tooling')
        self.assertEqual(policy['documents'][path]['source'], [path])
        self.assertEqual(policy['documents'][path]['source_blobs'], [current])
        error = '^stale document declaration: scripts/pin_revisions[.]py$'
        changed = copy.deepcopy(policy)
        changed['documents'][path]['source_blobs'] = [stale]
        with self.assertRaisesRegex(ValueError, error):
            validate_declarations(changed, files)
        for substituted in (stale, '0' * 40, blob('HEAD', 'scripts/provenance_archive.py')):
            with self.subTest(substituted=substituted):
                with self.assertRaisesRegex(ValueError, error):
                    validate_declarations(policy, {path: {'blob': substituted}})

    def test_pin_revisions_spec_binding_rejects_stale_declaration_and_source(self):
        repo = Path(__file__).resolve().parents[2]
        path = 'tests/pin_revisions_spec.py'
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())

        def blob(revision, source=path):
            return subprocess.check_output(
                ['git', '-C', str(repo), 'rev-parse', revision + ':' + source],
                text=True).strip()

        current = blob('HEAD')
        # S1141 adds Windows Git path and append-only controls to the test suite.
        stale = blob('78920104f4beb219158618bb30c21bb267044654^')
        self.assertNotEqual(current, stale)
        files = {path: {'blob': current}}
        self.assertEqual(validate_declarations(policy, files).get(path), 'tests')
        self.assertEqual(policy['documents'][path]['source'], [path])
        self.assertEqual(policy['documents'][path]['source_blobs'], [current])
        error = '^stale document declaration: tests/pin_revisions_spec[.]py$'
        changed = copy.deepcopy(policy)
        changed['documents'][path]['source_blobs'] = [stale]
        with self.assertRaisesRegex(ValueError, error):
            validate_declarations(changed, files)
        for substituted in (stale, '0' * 40, blob('HEAD', 'scripts/pin_revisions.py')):
            with self.subTest(substituted=substituted):
                with self.assertRaisesRegex(ValueError, error):
                    validate_declarations(policy, {path: {'blob': substituted}})

    def assert_publication_binding(self, path, role, source_commit, sibling):
        repo = Path(__file__).resolve().parents[2]
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())

        def blob(revision, source=path):
            return subprocess.check_output(
                ['git', '-C', str(repo), 'rev-parse', revision + ':' + source],
                text=True).strip()

        # These integrated commits changed Git path handling and its regressions.
        current, stale = blob('HEAD'), blob(source_commit + '^')
        self.assertEqual(current, blob(source_commit))
        self.assertNotEqual(current, stale)
        files = {path: {'blob': current}}
        self.assertEqual(validate_declarations(policy, files).get(path), role)
        self.assertEqual(policy['documents'][path]['source'], [path])
        self.assertEqual(policy['documents'][path]['source_blobs'], [current])
        error = '^stale document declaration: ' + path.replace('.', '[.]') + '$'
        changed = copy.deepcopy(policy)
        changed['documents'][path]['source_blobs'] = [stale]
        with self.assertRaisesRegex(ValueError, error):
            validate_declarations(changed, files)
        for substituted in (stale, '0' * 40, blob('HEAD', sibling)):
            with self.subTest(substituted=substituted):
                with self.assertRaisesRegex(ValueError, error):
                    validate_declarations(policy, {path: {'blob': substituted}})

    def test_publication_script_binding_rejects_stale_declaration_and_source(self):
        self.assert_publication_binding(
            'scripts/pin_proof_publication.py', 'tooling',
            '48b4563a7647c9d7243397536d94adbf2e4da5c0',
            'scripts/pin_revisions.py')

    def recorded_source_commit(self, path, subject):
        """The one commit reachable from HEAD that changed path with this subject.

        A lot not yet integrated names its own commit by subject, as
        test_d570_epochs records refreshes: a rebase keeps the subject, so the
        pin survives the integration rebase and still proves reachability."""
        repo = Path(__file__).resolve().parents[2]
        rows = subprocess.check_output(
            ['git', '-C', str(repo), 'log', '--full-history', '--format=%H%x00%s',
             'HEAD', '--', path], text=True).splitlines()
        matches = [row.split('\0', 1)[0] for row in rows if row.split('\0', 1)[1] == subject]
        self.assertEqual(len(matches), 1, f'commits changing {path} with subject {subject!r}')
        return matches[0]

    def test_publication_spec_binding_rejects_stale_declaration_and_source(self):
        path = 'tests/pin_proof_publication_spec.py'
        self.assert_publication_binding(
            path, 'tests',
            self.recorded_source_commit(
                path, 'test(pin-proof): read real originals through POSIX Git object paths'),
            'tests/pin_revisions_spec.py')

    def test_composed_document_declarations_reject_altered_git_documents(self):
        repo = Path(__file__).resolve().parents[2]
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        files = read_tree(repo, revision)
        policy = json.loads((repo / 'scripts/source-inventory/declarations.json').read_bytes())
        audit = repo / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        for path in ('README.md', 'docs/README.md', 'docs/man/rmux.1',
                     'docs/release/publication-runbook.md', 'scripts/bench/cutoff_assets.py'):
            with self.subTest(path=path), tempfile.TemporaryDirectory(dir=audit) as directory:
                fixture = Path(directory)

                def git(*args):
                    return subprocess.check_output(
                        ['git', '-C', directory, '-c', 'user.name=Inventory fixture',
                         '-c', 'user.email=fixture@example.invalid',
                         '-c', 'commit.gpgsign=false', *args], text=True).strip()

                git('init', '-q')
                document = fixture / path
                document.parent.mkdir(parents=True, exist_ok=True)
                document.write_bytes(files[path]['data'])
                git('add', '--', path)
                git('commit', '-qm', 'Original document')
                validate_declarations(policy, read_tree(fixture, git('rev-parse', 'HEAD')))
                document.write_bytes(files[path]['data'] + b'\nUnreviewed document change.\n')
                git('add', '--', path)
                git('commit', '-qm', 'Altered document')
                altered = read_tree(fixture, git('rev-parse', 'HEAD'))
                self.assertNotEqual(altered[path]['blob'], files[path]['blob'])
                with self.assertRaisesRegex(ValueError, '^stale document declaration: '):
                    validate_declarations(policy, altered)

    def test_every_tracked_file_has_a_verified_role(self):
        repo = Path(__file__).resolve().parents[2]
        revision = subprocess.check_output(
            ['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        raw = (repo / 'scripts/source-inventory/declarations.json').read_bytes()
        report = scan(repo, revision, Path(os.environ['SOURCE_SYNTAX']).resolve(),
                      json.loads(raw), raw)
        self.assertEqual(report['errors'], [], '\n'.join(report['errors']))


class TrunkSourceBindingTests(unittest.TestCase):
    """Current source bindings, independently of any historical pair policy."""

    EXPECTED = {
        'scripts/test_candidate_tiny_smoke.py': 'tests',
        'scripts/test_web_no_listener_preflight.py': 'tests',
        'scripts/tiny_smoke_identity.py': 'tooling',
        'scripts/web_no_listener_preflight.py': 'tooling',
        'tests/options_bank_mutations_spec.py': 'tests',
        'tests/options_bank_receipts_spec.py': 'tests',
        'scripts/regression_registry_options.py': 'tooling',
        'scripts/regression_registry_options_flags.json': 'specifications',
        'scripts/test_regression_registry_options.py': 'tests',
        'scripts/surface_conformance.py': 'tooling',
        'scripts/surface_manifest.py': 'tooling',
        'scripts/surface_sources.py': 'tooling',
        'scripts/surface_syntax.py': 'tooling',
        'scripts/test_surface_conformance.py': 'tests',
        'scripts/test_surface_fresh_fixtures.py': 'tests',
        'scripts/test_surface_manifest.py': 'tests',
        'scripts/test_surface_manifest_rejections.py': 'tests',
        'tests/startup_stderr_order_spec.py': 'tests',
        'tests/tmux_compat_surface_matrix/copy_viewport/fixtures/macos-bash-top.json': 'specifications',
        'scripts/pin_revisions.py': 'tooling',
        'tests/pin_revisions_spec.py': 'tests',
        'scripts/pin_proof_publication.py': 'tooling',
        'tests/pin_proof_publication_spec.py': 'tests',
    }
    STALE_BLOBS = {
        'scripts/regression_registry_options.py': '27a88d1f0ecbf713f3be4d5649d0ebe1cef9b79f',
        'scripts/test_regression_registry_options.py': '11b516e4a4305de88012f84ca749f4d164625139',
        'scripts/surface_sources.py': '23fdf39a72b8f7d2e5fac6385a4587119c476fd6',
        'scripts/test_surface_manifest_rejections.py': 'edea76603cbaefdfe6be8072be4e201fdeab1b08',
        'scripts/pin_revisions.py': '87afcc803517cc49217813931818035166bd99cc',
        'tests/pin_revisions_spec.py': '2632294bc301a902f0fcc92633c84bf670094633',
        'scripts/pin_proof_publication.py': '922983ba221d54759d27b0372ea44004164f9b81',
        'tests/pin_proof_publication_spec.py': 'bfb8dfb76e388262ca6f24e4c8b3f0dd5279f1ba',
    }

    @classmethod
    def setUpClass(cls):
        cls.repo = Path(__file__).resolve().parents[2]

        def git(*args):
            return subprocess.check_output(['git', '-C', str(cls.repo), *args])

        cls.revision = git('rev-parse', 'HEAD').decode().strip()
        cls.policy = json.loads((cls.repo / 'scripts/source-inventory/declarations.json').read_bytes())
        cls.files = {path: {
            'blob': git('rev-parse', cls.revision + ':' + path).decode().strip(),
            'data': git('show', cls.revision + ':' + path),
        } for path in cls.EXPECTED}

    def report(self, policy, files):
        with patch('inventory.read_tree', return_value=files):
            return scan(self.repo, self.revision, None, policy,
                        json.dumps(policy).encode(), parsed={})

    def test_trunk_sources_have_exact_current_bindings_and_roles(self):
        for path, role in self.EXPECTED.items():
            with self.subTest(path=path):
                files = {path: self.files[path]}
                self.assertEqual(validate_declarations(self.policy, files).get(path), role)
                row = self.policy['documents'][path]
                self.assertEqual(row['source'], [path])
                self.assertEqual(row['source_blobs'], [files[path]['blob']])
                for field in ('owner', 'role', 'consumers', 'freshness'):
                    self.assertTrue(row[field], field)
        report = self.report(self.policy, self.files)
        self.assertEqual(report['errors'], [])
        self.assertEqual({row['path'] for row in report['files']}, set(self.EXPECTED))
        for row in report['files']:
            self.assertEqual(row['counts'], {self.EXPECTED[row['path']]: row['physical_lines']})

    def test_trunk_sources_refuse_absent_sources_and_invalid_blobs(self):
        for path in self.EXPECTED:
            other = next(item for item in self.EXPECTED if item != path)
            for mutation in ('absent_source', 'missing_blob', 'unknown_blob', 'exchanged_blob'):
                with self.subTest(path=path, mutation=mutation):
                    policy = copy.deepcopy(self.policy)
                    self.assertIn(path, policy['documents'])
                    files = {path: dict(self.files[path])}
                    row = policy['documents'][path]
                    if mutation == 'absent_source':
                        row['source'] = [path, other]
                        error = 'document source absent'
                    elif mutation == 'missing_blob':
                        del row['source_blobs']
                        error = 'incomplete document declaration'
                    else:
                        files[path]['blob'] = ('0' * 40 if mutation == 'unknown_blob'
                                               else self.files[other]['blob'])
                        error = 'stale document declaration'
                    with self.assertRaisesRegex(ValueError, '^' + error + ': '):
                        validate_declarations(policy, files)

    def test_trunk_sources_refuse_withdrawn_roles_and_undeclared_siblings(self):
        for path, role in self.EXPECTED.items():
            with self.subTest(path=path):
                policy = copy.deepcopy(self.policy)
                self.assertIn(path, policy['files'][role])
                policy['files'][role].remove(path)
                self.assertEqual(self.report(policy, {path: self.files[path]})['errors'],
                                 ['unclassified file: ' + path])
                sibling = path + '.undeclared'
                self.assertEqual(self.report(self.policy, {sibling: self.files[path]})['errors'],
                                 ['unclassified file: ' + sibling])

    def test_trunk_bindings_refuse_the_replaced_source_blobs(self):
        for path, stale in self.STALE_BLOBS.items():
            with self.subTest(path=path):
                self.assertNotEqual(stale, self.files[path]['blob'])
                self.assertNotIn(stale, self.policy['documents'][path]['source_blobs'])
                policy = copy.deepcopy(self.policy)
                policy['documents'][path]['source_blobs'] = [stale]
                with self.assertRaisesRegex(ValueError, '^stale document declaration: '):
                    validate_declarations(policy, {path: self.files[path]})
                with self.assertRaisesRegex(ValueError, '^stale document declaration: '):
                    validate_declarations(self.policy, {path: dict(self.files[path], blob=stale)})
