"""Source, Cartesian-product and existing-consumer regressions for the catalogue."""
from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from web_compatibility_matrix import ROOT, SOURCE, PUBLISHED, generate, lookup, render, validate_published


class WebCompatibilityValidation(unittest.TestCase):
    def setUp(self):
        audit = ROOT / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=audit, prefix="web-matrix-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.matrix = generate(ROOT)
        for path in {SOURCE, PUBLISHED, *(row['path'] for row in self.matrix['implementation'])}:
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, target)
        self.source = json.loads((self.root / SOURCE).read_text())

    def write_source(self, value):
        (self.root / SOURCE).write_text(json.dumps(value), encoding="utf-8")

    def test_real_cartesian_product_has_no_duplicate_or_missing_cells(self):
        self.assertEqual(len(self.matrix['providers']), 6)
        self.assertEqual(len(self.matrix['artifact_network']), 32)
        self.assertEqual({(row['artifact'], row['network']) for row in self.matrix['artifact_network']},
                         {(a['id'], n['id']) for a in self.matrix['artifacts'] for n in self.matrix['networks']})
        self.assertEqual(len(self.matrix['frontend_daemon']),
                         len(self.matrix['frontends']) * len(self.matrix['daemons']))
        self.assertEqual(lookup(self.matrix, 'frontend_daemon', frontend='web-v1',
                                daemon='web-v1')['status'], 'supported')
        self.assertEqual(render(generate(self.root)), (self.root / PUBLISHED).read_text())
        for artifact in ('full', 'tiny'):
            for network in self.matrix['networks']:
                self.assertEqual(lookup(self.matrix, 'artifact_network', artifact=artifact,
                                        network=network['id'])['status'], 'supported')

    def test_unknown_and_unavailable_combinations_fail_explicitly(self):
        for artifact, network in (('no-web', 'loopback'), ('wasm', 'localhost-run'),
                                  ('full', 'cloudflare'), ('unknown', 'loopback')):
            with self.subTest(artifact=artifact, network=network), self.assertRaisesRegex(
                    ValueError, 'unsupported|unknown or absent'):
                lookup(self.matrix, 'artifact_network', artifact=artifact, network=network)
        with self.assertRaisesRegex(ValueError, 'unknown or absent'):
            lookup(self.matrix, 'frontend_daemon', frontend='web-v2', daemon='web-v1')

    def test_schema_types_duplicates_and_orphan_support_are_rejected(self):
        changes = []
        for name, value in (('schema_version', True), ('schema_version', 2),
                            ('protocol_version', True), ('extra', 'unknown')):
            changed = deepcopy(self.source)
            changed[name] = value
            changes.append(changed)
        for axis in ('frontends', 'daemons', 'artifacts', 'networks', 'support'):
            changed = deepcopy(self.source)
            changed[axis].append(changed[axis][0])
            changes.append(changed)
        changed = deepcopy(self.source)
        changed['support'].append(dict(changed['support'][0], id='orphan'))
        changes.append(changed)
        changed = deepcopy(self.source)
        changed['artifacts'][0]['support'] = ['unknown']
        changes.append(changed)
        for index, changed in enumerate(changes):
            with self.subTest(index=index), self.assertRaises(ValueError):
                self.write_source(changed)
                generate(self.root)
        (self.root / SOURCE).write_text('{"schema_version":1,"schema_version":1}')
        with self.assertRaisesRegex(ValueError, 'duplicate JSON field'):
            generate(self.root)

    def test_new_or_removed_presets_cannot_silently_change_the_catalogue(self):
        preset = self.root / 'crates/rmux-server/tunnels/localhost-run.toml'
        saved = preset.read_text()
        preset.unlink()
        with self.assertRaisesRegex(ValueError, 'support file absent'):
            generate(self.root)
        preset.write_text(saved)
        extra = preset.with_name('unimplemented.toml')
        extra.write_text(saved)
        with self.assertRaisesRegex(ValueError, 'missing or orphan provider'):
            generate(self.root)

    def test_provider_cannot_point_at_another_implementation(self):
        self.source['networks'][2]['preset'] = 'crates/rmux-server/tunnels/serveo.toml'
        self.write_source(self.source)
        with self.assertRaisesRegex(ValueError, 'orphan provider preset'):
            generate(self.root)

    def test_claimed_protocol_and_missing_artifact_support_are_rejected(self):
        changed = deepcopy(self.source)
        changed['frontends'][0]['protocol_version'] = 2
        self.write_source(changed)
        with self.assertRaisesRegex(ValueError, 'frontend protocol not implemented'):
            generate(self.root)
        changed = deepcopy(self.source)
        changed['artifacts'][0]['support'] = ['browser-crypto']
        self.write_source(changed)
        with self.assertRaisesRegex(ValueError, 'missing implementation support'):
            generate(self.root)

    def test_feature_and_runtime_changes_invalidate_compatibility(self):
        path = self.root / 'Cargo.toml'
        path.write_text(path.read_text().replace('default = ["web"]', 'default = []'))
        with self.assertRaisesRegex(ValueError, 'feature support changed'):
            generate(self.root)
        shutil.copyfile(ROOT / 'Cargo.toml', path)
        path = self.root / 'crates/rmux-server/src/web/tunnel/mod.rs'
        path.write_text(path.read_text().replace('runner::start(preset, settings).await',
                                                'unimplemented!()'))
        with self.assertRaisesRegex(ValueError, 'support anchor absent'):
            generate(self.root)

    def test_partial_duplicate_or_forged_published_cells_are_rejected(self):
        for mutation in ('missing', 'duplicate', 'invented-support'):
            changed = deepcopy(self.matrix)
            if mutation == 'missing':
                changed['artifact_network'].pop()
            elif mutation == 'duplicate':
                changed['artifact_network'].append(changed['artifact_network'][0])
            else:
                changed['artifact_network'][0]['support'] = ['absent.rs']
            (self.root / PUBLISHED).write_text(render(changed))
            with self.subTest(mutation=mutation), self.assertRaisesRegex(ValueError, 'stale or incomplete'):
                validate_published(self.root)

    def test_cli_checks_and_refuses_unknown_without_writing(self):
        command = [sys.executable, str(ROOT / 'scripts/web_compatibility_matrix.py')]
        before = (self.root / PUBLISHED).read_bytes()
        for selection in (['--artifact', 'no-web', '--network', 'localhost-run'],
                          ['--artifact', 'full', '--network', 'cloudflare'],
                          ['--frontend', 'web-v2', '--daemon', 'web-v1']):
            result = subprocess.run([*command, 'query', '--root', str(self.root), *selection],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn('compatibility combination', result.stderr)
            self.assertEqual(result.stdout, '')
        self.assertEqual((self.root / PUBLISHED).read_bytes(), before)

    def test_existing_release_consumers_reject_stale_matrix(self):
        sys.path.insert(0, str(ROOT / 'scripts/release'))
        import web_compatibility_contract as contract
        import web_share_live as live
        def module(name):
            spec = importlib.util.spec_from_file_location(name.replace('-', '_'),
                                                         ROOT / 'scripts/release' / f'{name}.py')
            result = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(result)
            return result
        wasm = module('canonical-wasm-bundle')
        release = module('validate-contracts')
        (self.root / PUBLISHED).write_text('{}\n')
        with patch.object(contract, 'ROOT', self.root):
            for consume in (contract.validate_browser_contract,
                            lambda: wasm.package_files(self.root / 'missing-package'),
                            lambda: live.expected_wasm_hash(self.root / 'missing-provenance', 'a' * 40, '1.0.0')):
                with self.assertRaisesRegex(ValueError, 'stale or incomplete'):
                    consume()
        with patch.object(release, 'ROOT', self.root), self.assertRaisesRegex(
                ValueError, 'stale or incomplete'):
            release.main()


class WebCompatibilitySourceAnchors(unittest.TestCase):
    """Exercise real source and release consumers without building the regex tool."""

    def setUp(self):
        # Only the unrelated Rust regex engine is stubbed. Source membership,
        # feature checks, provider fields and publication hashes remain real.
        regex = patch('web_compatibility_matrix.validate_regex')
        self.regex = regex.start()
        self.addCleanup(regex.stop)
        process = patch('subprocess.run', side_effect=AssertionError('Python-only regression'))
        process.start()
        self.addCleanup(process.stop)

    def test_current_source_keeps_disabled_and_enabled_contracts(self):
        from web_compatibility_matrix import verify_source
        from web_compatibility_model import Catalogue

        catalogue = Catalogue.load(ROOT / SOURCE)
        providers = verify_source(ROOT, catalogue)
        self.assertEqual(set(providers), {'localhost-run', 'sandhole', 'serveo',
                                         'srv-us', 'tailscale-funnel', 'tailscale-serve'})
        self.assertTrue(self.regex.called)
        matrix = validate_published(ROOT)
        self.assertEqual(len(matrix['artifact_network']), 32)
        for network in matrix['networks']:
            for artifact in ('full', 'tiny'):
                self.assertEqual(lookup(matrix, 'artifact_network', artifact=artifact,
                                        network=network['id'])['status'], 'supported')
            for artifact in ('no-web', 'wasm'):
                with self.assertRaisesRegex(ValueError, 'unsupported compatibility combination'):
                    lookup(matrix, 'artifact_network', artifact=artifact, network=network['id'])


    def test_repeated_consumers_reject_removed_source_guards(self):
        sys.path.insert(0, str(ROOT / 'scripts/release'))
        import web_compatibility_contract as contract
        import web_share_live as live

        spec = importlib.util.spec_from_file_location(
            'canonical_wasm_guard_test', ROOT / 'scripts/release/canonical-wasm-bundle.py')
        wasm = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(wasm)
        matrix = generate(ROOT)
        audit = ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit, prefix='web-anchor-') as directory:
            root = Path(directory)
            for relative in {SOURCE, PUBLISHED, *(row['path'] for row in matrix['implementation'])}:
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / relative, target)
            source = json.loads((root / SOURCE).read_text())
            with patch.object(contract, 'ROOT', root):
                contract.validate_browser_contract()
                for anchor_id in ('web-disabled', 'web-feature', 'browser-crypto'):
                    anchor = next(row for row in source['support'] if row['id'] == anchor_id)
                    path = root / anchor['path']
                    original = path.read_text()
                    # Test both halves of the disabled binding, retaining the
                    # other half so neither a bare cfg nor a bare mod suffices.
                    removed = anchor['contains'].splitlines() if anchor_id == 'web-disabled' else [anchor['contains']]
                    for fragment in removed:
                        path.write_text(original.replace(fragment, '', 1))
                        consumers = {
                            'source': lambda: generate(root),
                            'publication': lambda: validate_published(root),
                            'browser': contract.validate_browser_contract,
                            'WASM': lambda: wasm.package_files(root / 'absent-package'),
                            'Web': lambda: live.expected_wasm_hash(root / 'absent-provenance', 'a' * 40, '1.0.0'),
                        }
                        for name, consume in consumers.items():
                            with self.subTest(anchor=anchor_id, fragment=fragment, consumer=name):
                                with self.assertRaisesRegex(ValueError, f'support anchor absent: {anchor_id}'):
                                    consume()
                        path.write_text(original)
                        contract.validate_browser_contract()

    def test_real_browser_contract_and_canonical_wasm_bundle(self):
        from argparse import Namespace
        sys.path.insert(0, str(ROOT / 'scripts/release'))
        import web_compatibility_contract as contract
        import web_share_live as live

        spec = importlib.util.spec_from_file_location(
            'canonical_wasm_anchor_test', ROOT / 'scripts/release/canonical-wasm-bundle.py')
        wasm = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(wasm)
        contract.validate_browser_contract()
        audit = ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit, prefix='web-anchor-') as directory:
            root = Path(directory)
            package, output = root / 'package', root / 'output'
            package.mkdir()
            output.mkdir()
            for name in wasm.PACKAGE_FILES:
                (package / name).write_bytes(b'browser fixture')
            (package / 'package.json').write_text(json.dumps(
                {'name': 'rmux-web-crypto', 'version': '1.0.0', 'type': 'module'}))
            args = Namespace(source_sha='a' * 40, version='1.0.0',
                             package_dir=package, output_dir=output)
            wasm.create(args)
            wasm.verify(args)
            _, provenance = wasm.paths(args)
            self.assertEqual(live.expected_wasm_hash(provenance, args.source_sha, args.version),
                             wasm.file_hash(package / 'rmux_web_crypto_wasm_bg.wasm'))
            (package / 'rmux_web_crypto_wasm.js').write_bytes(b'changed browser fixture')
            with self.assertRaisesRegex(ValueError, 'WASM bundle bytes differ'):
                wasm.verify(args)


if __name__ == '__main__':
    unittest.main()
