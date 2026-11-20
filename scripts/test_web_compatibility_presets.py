"""Preset acceptance follows the daemon's TunnelPreset contract."""
from pathlib import Path
import shutil
import tempfile
import unittest

from web_compatibility_matrix import ROOT, SOURCE, PUBLISHED, generate


class WebCompatibilityPresets(unittest.TestCase):
    def setUp(self):
        audit = ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=audit, prefix='preset-contract-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        matrix = generate(ROOT)
        for path in {SOURCE, PUBLISHED, *(row['path'] for row in matrix['implementation'])}:
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, target)
        self.preset = self.root / 'crates/rmux-server/tunnels/localhost-run.toml'
        self.original = self.preset.read_text()

    def test_reported_unexecutable_presets_are_rejected(self):
        for change, content in (
                ('ready_timeout_secs = 0', self.original.replace(
                    'ready_timeout_secs = 30', 'ready_timeout_secs = 0')),
                ('url_source = "invalid"', self.original.replace(
                    'url_source = "both"', 'url_source = "invalid"')),
                ('unsupported_option = true', self.original + '\nunsupported_option = true\n')):
            with self.subTest(change=change):
                self.preset.write_text(content)
                with self.assertRaises(ValueError):
                    generate(self.root)


if __name__ == '__main__':
    unittest.main()
