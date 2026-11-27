"""Field, boundary and consumer neighbors of the preset acceptance test."""
from copy import deepcopy
import hashlib
import importlib.util
import json
import subprocess
import sys
import unittest
from unittest.mock import patch

import test_web_compatibility_presets as acceptance
from web_compatibility_matrix import ROOT, PUBLISHED, generate, lookup, render, validate_published


class PresetContract(unittest.TestCase):
    setUp = acceptance.WebCompatibilityPresets.setUp

    def write_preset(self, changes=None, missing=()):
        value = {'name': 'localhost-run', 'program': 'ssh', 'url_pattern': 'https://example.test'}
        value.update(changes or {})
        for field in missing:
            value.pop(field, None)
        self.preset.write_text(''.join(f'{key} = {json.dumps(item)}\n'
                                       for key, item in value.items()))

    def test_required_fields_and_all_declared_field_types(self):
        for field in ('name', 'program', 'url_pattern'):
            with self.subTest(missing=field), self.assertRaises(ValueError):
                self.write_preset(missing=(field,))
                generate(self.root)
        invalid = {
            'name': [True, 1, [], {}, '', 'other', 'bad/name', 'é'],
            'program': [True, 1, [], {}, '', ' \t\n', '\u2003', 'ssh\0'],
            'args': [True, 1, 'ssh', {}, [1], [True], ['ok', {}], ['bad\0arg']],
            'url_pattern': [True, 1, [], {}],
            'ready_pattern': [True, 1, [], {}],
            'install_hint': [True, 1, [], {}],
            'url_source': [True, 1, [], {}, '', 'invalid', 'Both', ' stdout '],
            'ready_timeout_secs': [True, False, '30', 30.0, [], {}, -1, 0, 301, 2**64],
            'unsupported_option': [True],
        }
        for field, values in invalid.items():
            for value in values:
                with self.subTest(field=field, value=value), self.assertRaisesRegex(
                        ValueError, 'preset'):
                    self.write_preset({field: value})
                    generate(self.root)

    def test_defaults_limits_and_optional_strings_preserve_supported_cells(self):
        cases = [{}, {'args': []}, {'args': ['', '--port', '{port}']},
                 {'ready_timeout_secs': 1}, {'ready_timeout_secs': 300},
                 {'program': '\u001c'}, {'url_pattern': ''},
                 {'ready_pattern': ''}, {'install_hint': ''},
                 *({'url_source': source} for source in ('both', 'stdout', 'stderr'))]
        for fields in cases:
            with self.subTest(fields=fields):
                self.write_preset(fields)
                matrix = generate(self.root)
                (self.root / PUBLISHED).write_text(render(matrix))
                published = validate_published(self.root)
                self.assertEqual(len(published['frontend_daemon']), 1)
                self.assertEqual(len(published['providers']), 6)
                self.assertEqual(len(published['artifact_network']), 32)
                for artifact in ('full', 'tiny'):
                    self.assertEqual(lookup(published, 'artifact_network', artifact=artifact,
                                            network='localhost-run')['status'], 'supported')
                provider = next(row for row in matrix['providers'] if row['id'] == 'localhost-run')
                self.assertEqual(provider['args'], fields.get('args', []))

    def test_forged_published_presets_fail_generation_and_all_consumers(self):
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
        original = generate(self.root)
        for field, value in (('ready_timeout_secs', 0), ('url_source', 'invalid'),
                             ('unsupported_option', True)):
            content = self.original
            if field == 'ready_timeout_secs':
                content = content.replace('ready_timeout_secs = 30', 'ready_timeout_secs = 0')
            elif field == 'url_source':
                content = content.replace('url_source = "both"', 'url_source = "invalid"')
            else:
                content += '\nunsupported_option = true\n'
            self.preset.write_text(content)
            # Reproduce the former generator's otherwise-current publication.
            forged = deepcopy(original)
            for row in forged['implementation']:
                if row['path'] == str(self.preset.relative_to(self.root)):
                    row['sha256'] = hashlib.sha256(self.preset.read_bytes()).hexdigest()
            (self.root / PUBLISHED).write_text(render(forged))
            before = (self.root / PUBLISHED).read_bytes()
            consumers = {
                'generate': lambda: generate(self.root),
                'publication': lambda: validate_published(self.root),
                'browser': contract.validate_browser_contract,
                'WASM': lambda: wasm.package_files(self.root / 'absent-package'),
                'Web': lambda: live.expected_wasm_hash(self.root / 'absent-provenance',
                                                       'a' * 40, '1.0.0'),
                'release': release.main,
            }
            with patch.object(contract, 'ROOT', self.root), patch.object(release, 'ROOT', self.root):
                for name, consume in consumers.items():
                    with self.subTest(field=field, consumer=name), self.assertRaisesRegex(
                            ValueError, f'invalid provider preset {field}|invalid provider preset fields'):
                        consume()
            for command in ('generate', 'check', 'query'):
                argv = [sys.executable, str(ROOT / 'scripts/web_compatibility_matrix.py'),
                        command, '--root', str(self.root)]
                if command == 'query':
                    argv.extend(['--artifact', 'full', '--network', 'localhost-run'])
                result = subprocess.run(argv, capture_output=True, text=True)
                with self.subTest(field=field, command=command):
                    self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                    self.assertIn('invalid provider preset', result.stderr)
                    self.assertEqual(result.stdout, '')
                    self.assertEqual((self.root / PUBLISHED).read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
