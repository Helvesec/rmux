"""Real libtest inventory must never execute a cell or hide VT selection."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from test_candidate_conpty_alias_attestation import load


class LibtestSeparator(unittest.TestCase):
    def test_inventory_separator_and_attestation(self):
        runtime, guard = load('candidate_conpty_runtime'), load('candidate_identity')
        retention = load('candidate_identity_retention')
        rustc = os.environ.get('RMUX_RETENTION_TEST_RUSTC') or shutil.which('rustc')
        self.assertIsNotNone(rustc, 'real rustc/libtest witness is required')
        audit = Path(__file__).resolve().parent.parent / '.rmux-audit/conpty-separator'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as root:
            target = Path(root).resolve()
            source, image = target / 'witness.rs', target / 'witness.exe'
            source.write_text('''
fn record(name: &str) {
    use std::io::Write;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _lock = LOCK.lock().unwrap();
    let path = std::env::var("RMUX_LIBTEST_WITNESS").unwrap();
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{}", name).unwrap();
}
mod wrap1 { #[test] fn vt() { super::record("vt"); } }
#[test] fn other() { record("other"); }
''')
            subprocess.run([rustc, '--test', str(source), '-o', str(image)], check=True)
            build = target / 'build.json'
            build.write_text(json.dumps(dict(kind='rust-test')))
            record = dict(path=str(image), target=str(target), commit='a' * 40,
                          sha256=runtime.digest(image), build=str(build))
            real_popen = subprocess.Popen
            process_api = SimpleNamespace(current=lambda: {},
                                          wait_receipt=lambda *a: dict(exit=0))
            # Include option values spelling --list, plus filters after --.
            cases = (
                (['--'], ['other', 'vt'], False),
                (['--', 'wrap1::'], ['vt'], False),
                (['--', '--list', 'wrap1::'], ['vt'], False),
                (['--skip', '--list'], ['other', 'vt'], False),
                (['--skip', '--', 'wrap1::'], ['vt'], False),
                (['--skip=wrap1::', '--', 'other'], ['other'], False),
                (['--exact', '--', 'wrap1::vt'], ['vt'], False),
                (['--list', '--', 'wrap1::'], [], True),
                (['--', '--list'], [], False),
            )
            for args, expected, inventory in cases:
                for loaded in (False, True):
                    with self.subTest(args=args, loaded=loaded):
                        for path in (target / 'identity-conpty-launches').glob('*.json'):
                            path.unlink()
                        counter = target / 'executions'
                        counter.write_text('')
                        starts = []

                        def popen(argv, **kwargs):
                            self.assertEqual(kwargs.pop('creation_image'), image)
                            self.assertTrue(kwargs.pop('capture_creation'))
                            if kwargs.get('stdout') is None:
                                # Inventory must have completed without running tests.
                                self.assertEqual(counter.read_text(), '', 'inventory executed tests')
                            starts.append(list(argv[1:]))
                            process = real_popen(argv, **kwargs)
                            if not hasattr(process, '_handle'):
                                process._handle = 0
                            process.creation_identity = {}
                            return process

                        observer = SimpleNamespace(observe=lambda *a: dict(pair_loaded=loaded, errors=[]))
                        process_api.popen = popen
                        modules = dict(candidate_identity_retention=retention,
                                       candidate_conpty_runtime=runtime,
                                       native_input_windows_process=process_api,
                                       candidate_conpty_windows=observer)
                        with patch.object(guard, 'context', return_value=(target, 'a' * 40, target)), \
                                patch.object(guard, 'verify_built', return_value=record), \
                                patch.object(guard, 'record_launch'), \
                                patch.object(guard, 'support', side_effect=modules.__getitem__), \
                                patch.object(guard.sys, 'argv', ['identity', 'run', str(image), *args]), \
                                patch.object(retention, 'os', SimpleNamespace(name='nt')), \
                                patch.object(runtime, 'stage', return_value=dict(configuration='pinned')), \
                                patch.dict(os.environ, RMUX_LIBTEST_WITNESS=str(counter)):
                            status = guard.main()
                        required = 'vt' in expected
                        self.assertEqual(status, 86 if required and not loaded else 0)
                        self.assertEqual(sorted(counter.read_text().splitlines()), expected)
                        self.assertEqual(starts[-1], args, 'execution argv changed')
                        self.assertEqual(len(starts), 1 if inventory else 2)
                        receipts = [json.loads(path.read_text()) for path in
                                    (target / 'identity-conpty-launches').glob('*.json')]
                        self.assertEqual(sum(r['attestation_required'] for r in receipts), int(required))
                        self.assertEqual(sum(r['inventory'] for r in receipts), 1)


if __name__ == '__main__':
    unittest.main()
