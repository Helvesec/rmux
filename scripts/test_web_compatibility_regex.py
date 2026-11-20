"""Acceptance: Rust-invalid regexes cannot reach a supported published lookup."""
from copy import deepcopy
import hashlib
import unittest

from test_web_compatibility_presets import WebCompatibilityPresets
from web_compatibility_matrix import PUBLISHED, generate, lookup, render, validate_published


class WebCompatibilityRegex(unittest.TestCase):
    setUp = WebCompatibilityPresets.setUp

    def test_invalid_regex_cannot_be_published_or_consumed(self):
        original = generate(self.root)
        for field in ('url_pattern', 'ready_pattern'):
            lines = [line for line in self.original.splitlines()
                     if not line.startswith(field + ' =')]
            self.preset.write_text('\n'.join([*lines, f'{field} = "["', '']))
            with self.subTest(field=field, route='generation-publication-lookup'):
                with self.assertRaisesRegex(ValueError, field):
                    matrix = generate(self.root)
                    (self.root / PUBLISHED).write_text(render(matrix))
                    lookup(validate_published(self.root), 'artifact_network',
                           artifact='full', network='localhost-run')
            # A publication from the defective generator has a current source hash.
            forged = deepcopy(original)
            for row in forged['implementation']:
                if row['path'] == str(self.preset.relative_to(self.root)):
                    row['sha256'] = hashlib.sha256(self.preset.read_bytes()).hexdigest()
            (self.root / PUBLISHED).write_text(render(forged))
            with self.subTest(field=field, route='validation-lookup'):
                with self.assertRaisesRegex(ValueError, field):
                    lookup(validate_published(self.root), 'artifact_network',
                           artifact='full', network='localhost-run')


if __name__ == '__main__':
    unittest.main()
