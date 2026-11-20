"""Negative admission witnesses for review112's Windows native evidence gaps."""
import copy
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from native_input_collector import Snapshot
from native_input_windows import files_before_use, observed_bootstrap, write_once
from native_input_windows_trace import assess, ns_time, require_wait


class WindowsNativeEvidence(unittest.TestCase):
    def test_real_reused_pid_cannot_satisfy_another_lifetimes_wait(self):
        fixture = json.loads((Path(__file__).with_name('fixtures') /
                              'native_input_windows_reused_pid.json').read_text())
        first, second = fixture['processes'][:2]
        self.assertEqual(first['pid'], second['pid'])
        self.assertNotEqual(first['sequence'], second['sequence'])
        process = dict(pid=second['pid'], birth_ns=ns_time(second['birth']),
                       parent_pid=second['parent_pid'], parent_birth_ns=1,
                       end_ns=ns_time(second['end']), exit=int(second['exit']),
                       image=second['image'])
        receipt = dict(case='sdk', head='a' * 40, handle=123, before_ns=0,
                       wait_api='WaitForSingleObject', wait_return=0, exit=process['exit'],
                       executable_sha256='b' * 64, parent=dict(pid=process['parent_pid'], birth_ns=1),
                       child=dict(pid=process['pid'], birth_ns=process['birth_ns'],
                                  end_ns=process['end_ns'], image=process['image']),
                       wait_started_ns=process['birth_ns'], wait_finished_ns=process['end_ns'])
        self.assertIs(require_wait(process, [receipt], 'sdk', 'a' * 40, 'b' * 64), receipt)
        wrong = copy.deepcopy(receipt)
        wrong['child']['birth_ns'] = ns_time(first['birth'])
        for bad in (wrong, dict(receipt, wait_api='ETW-end'),
                    dict(receipt, wait_return=258), dict(receipt, handle=0),
                    dict(receipt, head='c' * 40), dict(receipt, case='proto'),
                    dict(receipt, wait_finished_ns=process['end_ns'] - 1)):
            with self.subTest(bad=bad), self.assertRaisesRegex(RuntimeError, 'actual matching parental wait'):
                require_wait(process, [bad], 'sdk', 'a' * 40, 'b' * 64)
        with self.assertRaises(RuntimeError):
            require_wait(process, [receipt, receipt], 'sdk', 'a' * 40, 'b' * 64)

    def test_unknown_clock_loss_unpaired_load_and_missing_wait_never_pass(self):
        # Actual SDK071 Cargo9652 clock pair and rsaenh load from the frozen
        # missing-input observation; no historical evidence is repaired here.
        observation = json.loads((Path(__file__).with_name('fixtures') /
                                  'native_input_windows_missing_loads.json').read_text())
        load = next(row for row in observation['loads'] if row['pid'] == 9652)
        birth, end = '2026-10-02T11:35:45.939894500Z', '2026-10-02T11:36:57.640251400Z'
        start = dict(ProcessID='9652', ProcessSequenceNumber='2206170',
                     ParentProcessID='14084', ParentProcessSequenceNumber='2206090',
                     CreateTime=birth, at='2026-10-02T13:35:45.939922800+01:59',
                     ImageName=r'C:\tools\cargo.exe')
        stop = dict(ProcessID='9652', ProcessSequenceNumber='2206170', ExitTime=end,
                    ExitCode='101', at='2026-10-02T13:36:57.640280000+01:59')
        event = dict(ProcessID=str(load['pid']), ImageName=load['path'], at=load['load_at'])
        with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent.parent / '.rmux-audit') as name:
            obj = Path(name) / 'object'
            obj.write_bytes(b'fixture executable/library bytes')
            digest = hashlib.sha256(obj.read_bytes()).hexdigest()
            row = dict(object=str(obj), sha256=digest, before_ns=0, case='sdk', head='a' * 40)
            indexed = {p.casefold(): [row] for p in (start['ImageName'], load['path'])}
            receipt = dict(case='sdk', head='a' * 40, handle=123, before_ns=0,
                           wait_api='WaitForSingleObject', wait_return=0, exit=101,
                           executable_sha256=digest, parent=dict(pid=14084, birth_ns=1),
                           child=dict(pid=9652, birth_ns=ns_time(birth), end_ns=ns_time(end),
                                      image=start['ImageName']), wait_started_ns=ns_time(birth),
                           wait_finished_ns=ns_time(end))
            root = dict(pid=9652, birth_ns=ns_time(birth), parent_birth_ns=1)
            counters = [dict(EventsLost='0', BuffersLost='0')]
            def check(loads, waits=(receipt,), counts=counters):
                return assess(({2206170: start}, {2206170: stop}, loads, counts, 4),
                              root, indexed, waits, {}, 'sdk', 'a' * 40)
            green = check([event])
            self.assertEqual(green['status'], 'PASS')
            self.assertEqual(green['clock_offsets_ns']['+01:59'], 60 * 10**9)
            # Keep duplicate loads rather than reducing them to a final union.
            repeated = check([event, event])
            self.assertEqual(len(repeated['images']), 2)
            unknown = dict(event, at='2026-10-02T13:35:45.970638800+02:00')
            unpaired = dict(event, at='2026-10-02T13:36:59.970638800+01:59')
            for report in (check([event, unknown]), check([event, unpaired]),
                           check([event], waits=()),
                           check([event], counts=[dict(EventsLost='1', BuffersLost='0')]),
                           check([event], counts=[])):
                self.assertEqual(report['status'], 'PENDING')
                self.assertTrue(report['errors'])
                self.assertEqual(len(report['images']) + len(report['uncalibrated_loads']),
                                 report['loads_total'])

    def test_changed_or_redirected_observed_input_is_refused_before_usage(self):
        observation = json.loads((Path(__file__).with_name('fixtures') /
                                  'native_input_windows_missing_loads.json').read_text())
        root_dir = Path(__file__).resolve().parent.parent / '.rmux-audit'
        with tempfile.TemporaryDirectory(dir=root_dir) as name:
            root = Path(name)
            audit = root / 'audit'
            audit.mkdir()
            bindings = {}
            for i, row in enumerate(observation['loads']):
                path = root / (str(i) + '.dll')
                path.write_bytes(b'observed native bytes')
                bindings[row['path']] = str(path)
            snapshot = Snapshot(audit)
            stable = observed_bootstrap(snapshot, observation, bindings)
            alias = Path(stable[0])
            alias.write_bytes(b'changed before usage')
            with self.assertRaises(RuntimeError):
                snapshot.verify(alias)
            alias.unlink()
            replacement = root / 'replacement'
            replacement.write_bytes(b'observed native bytes')
            if os.name != 'nt':
                alias.symlink_to(replacement)
                with self.assertRaises(RuntimeError):
                    snapshot.verify(alias)
            else:
                alias.write_bytes(b'different native binding')
                with self.assertRaises(RuntimeError):
                    snapshot.verify(alias)

    def test_cold_command_requires_qualified_peak_before_use(self):
        from native_input_windows_command import capacity
        root = Path(__file__).resolve().parent.parent
        for budget in ({}, dict(source=str(root), os='windows', cell='sdk',
                                complete_cold_peak_qualified=False,
                                peak_bytes=2**30, reserve_bytes=2**30, at_ns=time.time_ns())):
            with self.assertRaisesRegex(RuntimeError, 'fresh qualification'):
                capacity(root, budget)

    @unittest.skipUnless(os.name == 'nt', 'native Windows handles and waits')
    def test_native_handles_waits_and_mutation_before_launch(self):
        from native_input_windows import new_snapshot, read_manifest
        from native_input_windows_process import install
        audit_root = Path(__file__).resolve().parent.parent / '.rmux-audit'
        with tempfile.TemporaryDirectory(dir=audit_root) as name:
            root = Path(name).resolve()
            retained = root / 'retained'
            retained.mkdir()
            snapshot = new_snapshot(retained, 16 * 2**20)
            snapshot.retain(Path(sys.executable))
            dll = root / 'observed.dll'
            dll.write_bytes(b'observed DLL bytes')
            snapshot.retain(dll)
            manifest = root / 'before-inputs.json'
            record = dict(os='windows', cell='sdk', head='a' * 40, source=str(root),
                          audit=str(root), case='native-test', reserve_bytes=16 * 2**20,
                          files=snapshot.files, aliases={}, at_ns=time.time_ns())
            write_once(manifest, record)
            _, digest = read_manifest(manifest)
            env = dict(os.environ, RMUX_NATIVE_INPUT_MANIFEST=str(manifest),
                       RMUX_NATIVE_INPUT_MANIFEST_SHA256=digest)
            for key in tuple(env):
                if key.startswith('RMUX_IDENTITY_'):
                    env.pop(key)
            original = subprocess.Popen
            with patch.dict(os.environ, env, clear=True):
                try:
                    install()
                    for code in (0, 7):
                        process = subprocess.Popen([sys.executable, '-c', 'raise SystemExit(' + str(code) + ')'])
                        self.assertEqual(process.wait(), code)
                        self.assertGreater(process._native_child['birth_100ns'], 0)
                    receipts = [json.loads(p.read_text()) for p in root.glob('boundary-*/wait.json')]
                    self.assertEqual(sorted(p['exit'] for p in receipts), [0, 7])
                    self.assertTrue(all(p['wait_return'] == 0 and p['child']['end_ns']
                                        and p['parent']['birth_ns'] for p in receipts))
                    dll.write_bytes(b'modified before launch')
                    with self.assertRaisesRegex(RuntimeError, 'differs before consumer'):
                        subprocess.Popen([sys.executable, '-c', 'print("must not start")'])
                    replacement = root / 'replacement.dll'
                    replacement.write_bytes(b'observed DLL bytes')
                    os.replace(replacement, dll)
                    # Same bytes and spelling, different native file identity.
                    with self.assertRaisesRegex(RuntimeError, 'differs before consumer'):
                        subprocess.Popen([sys.executable, '-c', 'print("must not start")'])
                finally:
                    subprocess.Popen = original

    @unittest.skipUnless(os.name == 'nt', 'native observed Windows DLL bytes')
    def test_actual_four_system_loads_and_isolated_entrypoint(self):
        from native_input_windows import new_snapshot
        scripts = Path(__file__).resolve().parent
        observation = json.loads((scripts / 'fixtures/native_input_windows_missing_loads.json').read_text())
        system = Path(os.environ['SystemRoot']) / 'System32'
        bindings = {row['path']: str(system / row['path'].rsplit('\\', 1)[1])
                    for row in observation['loads']}
        audit = scripts.parent / '.rmux-audit' / ('native-system-loads-' + secrets.token_hex(8))
        audit.mkdir()
        snapshot = new_snapshot(audit, 128 * 2**20)
        stable = observed_bootstrap(snapshot, observation, bindings)
        self.assertEqual(len(stable), 4)
        for path in stable:
            snapshot.verify(path)
            self.assertEqual(Path(snapshot.files[path]['object']).read_bytes(), Path(path).read_bytes())
        write_once(audit / 'before-profile.json', dict(
            os='windows', files=snapshot.files, aliases=snapshot.windows_aliases,
            at_ns=time.time_ns(), command_qualified_before_execution=False,
            scope='Four current observed system DLLs retained; no Cargo consumption or historical repair'))
        result = subprocess.run([sys.executable, '-I', '-S', '-B',
                                 str(scripts / 'native_input_collector.py'), '--help'],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--cell {sdk,proto}', result.stdout)


if __name__ == '__main__':
    unittest.main()
