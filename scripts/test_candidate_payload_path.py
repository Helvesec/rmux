"""Build-script attribution must preserve executable Windows output paths."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class BuildScriptPayload(unittest.TestCase):
    def check_output(self, windows):
        spec = importlib.util.spec_from_file_location(
            'units', Path(__file__).with_name('candidate_identity_units.py'))
        units = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(units)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            # The compiler output fits MAX_PATH; appending attribution does not.
            # Only the Windows boundary case needs a long directory. The Unix
            # naming neighbor must also run on a native Windows filesystem.
            parent = root / ('p' * ((205 if windows else 120) - len(str(root))))
            parent.mkdir()
            original = parent / 'build_script_build-ee0617d4bfac3422.exe'
            original.write_bytes(b'observed compiler output')
            sha = hashlib.sha256(original.read_bytes()).hexdigest()
            installs = []
            def move(source, destination):
                if windows and len(str(destination)) >= 260:
                    raise OSError('native Windows path limit exceeded')
                os.replace(source, destination)
            def install(guard, output, payload, source, head, target):
                self.assertEqual(payload.read_bytes(), b'observed compiler output')
                self.assertEqual(output, original)
                installs.append(payload)
                output.write_bytes(b'checked build-script wrapper')
            guard = SimpleNamespace(
                file_hash=lambda p: hashlib.sha256(p.read_bytes()).hexdigest(),
                stamp=lambda st: (st.st_size, st.st_mtime_ns),
                support=lambda name: SimpleNamespace(install=install))
            unit = dict(outputs=[original], stamps={}, kind='build-script',
                        graph={'qualified': True}, graph_sha256='graph')
            native = SimpleNamespace(name='nt' if windows else 'posix',
                                     replace=move, getpid=os.getpid)
            with patch.object(units, 'os', native), patch.object(units, 'check_depinfo'):
                units.after(guard, unit, root, 'head', root)
            self.assertEqual(len(installs), 1)
            payload = installs[0]
            self.assertEqual(payload.parent, original.parent)
            record = json.loads((root / 'identity-units' / (sha + '.json')).read_text())
            self.assertEqual(record['path'], str(payload))
            self.assertEqual(record['sha256'], sha)
            self.assertEqual(record['compiler_output'], str(original))
            self.assertEqual(record['graph'], unit['graph'])
            if windows:
                self.assertLessEqual(len(str(payload)), len(str(original)))
                self.assertEqual(payload.suffix, '.exe')
            else:
                self.assertEqual(payload.name, original.name + '.' + sha[:16] + '.payload')

    def test_windows_payload_does_not_extend_compiler_output(self):
        self.check_output(True)

    def test_unix_payload_layout_is_preserved(self):
        self.check_output(False)


if __name__ == '__main__':
    unittest.main()
