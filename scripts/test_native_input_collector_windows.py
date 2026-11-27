"""Windows regressions from the four unretained SDK071 loads in review112."""
import json
from pathlib import Path
import runpy
import tempfile
import unittest


class WindowsObservedInputs(unittest.TestCase):
    def test_four_observed_loads_are_retained_before_consumer(self):
        scripts = Path(__file__).resolve().parent
        api = runpy.run_path(str(scripts / 'native_input_collector.py'), run_name='collector_test')
        collect = api.get('observed_windows_bootstrap')
        self.assertTrue(callable(collect),
                        'Windows loads have no collector boundary before consumption')
        observation = json.loads((scripts / 'fixtures' /
                                  'native_input_windows_missing_loads.json').read_text())
        with tempfile.TemporaryDirectory(dir=scripts.parent / '.rmux-audit') as name:
            root = Path(name)
            system = root / 'System32'
            system.mkdir()
            audit = root / 'audit'
            audit.mkdir()
            bindings = {}
            for row in observation['loads']:
                p = system / row['path'].rsplit('\\', 1)[1]
                p.write_bytes(('measured input ' + row['path']).encode())
                bindings[row['path']] = str(p)
            snapshot = api['Snapshot'](audit)
            stable = collect(snapshot, observation, bindings)
            self.assertEqual(set(stable), set(bindings.values()))
            self.assertEqual(len(stable), 4)
            for path in stable:
                snapshot.verify(path)
                self.assertEqual(Path(snapshot.files[path]['object']).read_bytes(),
                                 Path(path).read_bytes())
            # Consumer cannot proceed after a retained input changes.
            Path(stable[0]).write_bytes(b'changed before consumer')
            with self.assertRaisesRegex(RuntimeError, 'differs before consumer'):
                snapshot.verify(stable[0])


if __name__ == '__main__':
    unittest.main()
