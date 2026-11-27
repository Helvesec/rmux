"""Regression from the sealed SDK046 ETW trace, without native PASS credit."""
import copy
import unittest

from native_input_windows_trace import assess, clock_group, ns_time


class WindowsClockCalibration(unittest.TestCase):
    def test_delayed_stop_preserves_proven_clock_and_rejects_unproven_events(self):
        # native-metadata-v2.zip SHA256 c3b1a3a69b1e96505708ed02832458f661
        # 08e963c11a88a9d0517dd6b70d5405; member 46w/.rmux-audit/s2046/sdk/native.xml
        # SHA256 114de2143cbe880777b722e7ab826a9a44c9e1f48db10e25b35eb26c65fd4bbd.
        # Exact original fields; full rows and their verification stay in private audit.
        starts = {
            2335028: dict(ProcessID='9112', ProcessSequenceNumber='2335028',
                          ParentProcessID='25412', ParentProcessSequenceNumber='2335025',
                          ImageName=r'\Device\HarddiskVolume3\Windows\System32\reg.exe',
                          CreateTime='2026-10-03T13:23:09.294691000Z',
                          at='2026-10-03T15:23:09.294709300+01:59'),
            2327775: dict(ProcessID='22828', ProcessSequenceNumber='2327775',
                          ParentProcessID='18428', ParentProcessSequenceNumber='309',
                          ImageName=r'\Device\HarddiskVolume3\Windows\System32\curl.exe',
                          CreateTime='2026-10-03T10:39:06.851953700Z',
                          at='2026-10-03T12:39:06.853001900+01:59'),
        }
        ends = {2331544: dict(ProcessID='14316', ProcessSequenceNumber='2331544',
                             ExitTime='2026-10-03T13:49:35.815654200Z',
                             at='2026-10-03T15:49:35.968468800+01:59')}
        loads = [dict(ProcessID='26028',
                      ImageName=r'\Device\HarddiskVolume3\Program Files\Git\cmd\git.exe',
                      at='2026-10-03T11:40:24.216138400+01:59')]
        root = dict(pid=9112, birth_ns=ns_time(starts[2335028]['CreateTime']),
                    parent_birth_ns=0)
        counters = [dict(EventsLost='0', BuffersLost='0')]

        def check(records=starts, stops=ends, images=loads, counts=counters):
            return assess((records, stops, images, counts, 4), root, {}, [],
                          {r'\Device\HarddiskVolume3': 'C:'}, 'sdk', 'a' * 40)

        self.assertEqual(clock_group(loads[0]['at']), '+01:59')
        self.assertEqual(ns_time('1970-01-01T01:59:00.0000001+01:59'), 100)
        report = check()
        self.assertEqual(report['clock_offsets_ns'].get('+01:59'), 60 * 10**9,
                         'the delayed original ProcessStop poisoned the creation clock')
        self.assertEqual(report['uncalibrated_loads'], [])
        self.assertEqual(report['end_event_delays_ns'][2331544], 152814600)
        self.assertEqual(report['loads_total'], len(loads))
        self.assertEqual(report['status'], 'PENDING')
        self.assertTrue(any('missing native end' in error for error in report['errors']))
        inconsistent = copy.deepcopy(starts)
        inconsistent[2335028]['at'] = '2026-10-03T15:23:10.294709300+01:59'
        bad = check(records=inconsistent)
        self.assertNotIn('+01:59', bad['clock_offsets_ns'])
        self.assertEqual(len(bad['uncalibrated_loads']), len(loads))
        early = copy.deepcopy(ends)
        early[2331544]['at'] = '2026-10-03T15:49:34.968468800+01:59'
        self.assertTrue(any('precedes exit' in error for error in check(stops=early)['errors']))
        unknown = dict(loads[0], at='2026-10-03T11:40:24.216138400+02:00')
        for pending in (bad, check(records={}), check(images=[unknown]),
                        check(counts=[dict(EventsLost='1', BuffersLost='0')])):
            self.assertEqual(pending['status'], 'PENDING')
            self.assertTrue(pending['errors'])
            self.assertEqual(len(pending['images']) + len(pending['uncalibrated_loads']),
                             pending['loads_total'])


if __name__ == '__main__':
    unittest.main()
