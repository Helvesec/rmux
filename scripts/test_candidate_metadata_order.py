"""Cargo must not consume pipelined metadata before its provenance is committed."""
import importlib.util
import io
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class MetadataPublication(unittest.TestCase):
    def test_compiler_artifact_notice_follows_unit_qualification(self):
        spec = importlib.util.spec_from_file_location('units', Path(__file__).with_name('candidate_identity_units.py'))
        units = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(units)
        visible = io.BytesIO()
        events = []
        notice = b'{"$message_type":"artifact","emit":"metadata"}\n'
        def compiler(*args, **kwargs):
            kwargs['stderr'].write(notice)
            return 0
        def seal(*args):
            self.assertEqual(visible.getvalue(), b'', 'Cargo observed metadata before qualification')
            events.append('qualified')
        guard = SimpleNamespace(jobserver=lambda: {}, source_identity=lambda *args: None,
                                sys=SimpleNamespace(stderr=SimpleNamespace(buffer=visible)))
        with tempfile.TemporaryDirectory() as directory, patch.object(units.subprocess, 'call', side_effect=compiler), \
                patch.object(units, 'after', side_effect=seal):
            self.assertEqual(units.compile_observed(guard, ['compiler'], {}, Path(directory),
                                                   'head', Path(directory), False), 0)
        self.assertEqual(events, ['qualified'])
        self.assertEqual(visible.getvalue(), notice)


if __name__ == '__main__':
    unittest.main()
