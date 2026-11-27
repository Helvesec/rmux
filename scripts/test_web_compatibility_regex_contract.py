"""Rust syntax neighbors and the release entry paths that publish compatibility."""
from copy import deepcopy
import hashlib
import importlib.util
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import rust_regex_validation as validation
import test_web_compatibility_preset_contract as presets
from web_compatibility_matrix import ROOT, PUBLISHED, generate, lookup, render, validate_published


class RegexContract(unittest.TestCase):
    setUp = presets.PresetContract.setUp
    write_preset = presets.PresetContract.write_preset

    def test_rust_valid_syntax_preserves_supported_cells(self):
        patterns = ('', r'\[', r'\p{Greek}+', r'[a-z&&[^aeiou]]', r'(?<url>https://\S+)',
                    r'(?U:a+)', r'(?-u:\x7F)', r'(?R:^ready$)', 'é', '\0', 'a\nb',
                    '(?x)a # a comment\n b', r'(?i-m:ready)', r'a{2,3}')
        for field in ('url_pattern', 'ready_pattern'):
            for pattern in patterns:
                with self.subTest(field=field, pattern=pattern):
                    self.write_preset({field: pattern})
                    # TOML accepts NUL via \u0000; JSON uses that same escape.
                    matrix = generate(self.root)
                    (self.root / PUBLISHED).write_text(render(matrix))
                    published = validate_published(self.root)
                    self.assertEqual([len(published[key]) for key in
                                      ('frontend_daemon', 'providers', 'artifact_network')], [1, 6, 32])
                    for artifact in ('full', 'tiny'):
                        self.assertEqual(lookup(published, 'artifact_network', artifact=artifact,
                                                network='localhost-run')['status'], 'supported')

    def test_rust_invalid_syntax_and_string_engine_limits_are_rejected(self):
        patterns = ('[', '(', r'\p{NotAProperty}', r'(?=a)', r'(?<=a)', r'(a)\1',
                    r'a{2,1}', r'\q', r'(?<n>a)(?<n>b)', r'(?-u:\xFF)', r'a{1000000000}')
        for field in ('url_pattern', 'ready_pattern'):
            for pattern in patterns:
                with self.subTest(field=field, pattern=pattern):
                    self.write_preset({field: pattern})
                    with self.assertRaisesRegex(ValueError, f'invalid provider preset {field}'):
                        generate(self.root)

    def test_forged_regex_publications_fail_all_existing_consumers(self):
        sys.path.insert(0, str(ROOT / 'scripts/release'))
        import web_compatibility_contract as contract
        import web_share_live as live

        def module(name):
            spec = importlib.util.spec_from_file_location(name.replace('-', '_'),
                                                         ROOT / 'scripts/release' / f'{name}.py')
            result = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(result)
            return result

        wasm, release = module('canonical-wasm-bundle'), module('validate-contracts')
        original = generate(self.root)
        for field in ('url_pattern', 'ready_pattern'):
            self.write_preset({field: '['})
            forged = deepcopy(original)
            for row in forged['implementation']:
                if row['path'] == str(self.preset.relative_to(self.root)):
                    row['sha256'] = hashlib.sha256(self.preset.read_bytes()).hexdigest()
            (self.root / PUBLISHED).write_text(render(forged))
            before = (self.root / PUBLISHED).read_bytes()
            consumers = {
                'browser': contract.validate_browser_contract,
                'WASM': lambda: wasm.package_files(self.root / 'absent-package'),
                'Web': lambda: live.expected_wasm_hash(self.root / 'absent-provenance',
                                                       'a' * 40, '1.0.0'),
                'release': release.main,
            }
            with patch.object(contract, 'ROOT', self.root), patch.object(release, 'ROOT', self.root):
                for name, consume in consumers.items():
                    with self.subTest(field=field, consumer=name), self.assertRaisesRegex(
                            ValueError, f'invalid provider preset {field}'):
                        consume()
            for command in ('generate', 'check', 'query'):
                argv = [sys.executable, str(ROOT / 'scripts/web_compatibility_matrix.py'),
                        command, '--root', str(self.root)]
                if command == 'query':
                    argv.extend(['--artifact', 'full', '--network', 'localhost-run'])
                result = subprocess.run(argv, capture_output=True, text=True)
                with self.subTest(field=field, command=command):
                    self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                    self.assertIn(f'invalid provider preset {field}', result.stderr)
                    self.assertEqual(result.stdout, '')
                    self.assertEqual((self.root / PUBLISHED).read_bytes(), before)

    def test_validator_builds_in_its_private_target_under_an_identity_harness(self):
        # A candidate identity run exports its own target, compiler wrapper
        # and the test package's OUT_DIR. None may reach the validator build.
        harness = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, harness)
        marker = harness / 'wrapper-called'
        wrapper = harness / 'rustc-wrapper'
        wrapper.write_text(f'#!/bin/sh\ntouch {shlex.quote(str(marker))}\nexit 86\n')
        wrapper.chmod(0o755)
        (harness / 'out').mkdir()
        head = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT, capture_output=True,
                              text=True, check=True).stdout.strip()
        private = (ROOT / '.rmux-audit/rust-regex-target' / head
                   / platform.system().lower()).resolve()
        validation.validator_binary.cache_clear()
        self.addCleanup(validation.validator_binary.cache_clear)
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(harness / 'identity-target'),
                                     'RUSTC_WRAPPER': str(wrapper),
                                     'CARGO_BUILD_RUSTC_WRAPPER': str(wrapper),
                                     'OUT_DIR': str(harness / 'out')}):
            binary = validation.validator_binary()
        self.assertTrue(binary.resolve().is_relative_to(private), binary)
        self.assertFalse(marker.exists())
        self.assertFalse((harness / 'identity-target').exists())

    def test_engine_drift_is_refused_before_support_is_published(self):
        path = self.root / 'Cargo.lock'
        path.write_text(path.read_text().replace('name = "regex"\nversion = "1.12.4"',
                                                'name = "regex"\nversion = "1.13.1"'))
        with self.assertRaisesRegex(ValueError, 'Rust regex validator engine differs'):
            generate(self.root)


if __name__ == '__main__':
    unittest.main()
