"""Compiled batch-dispatch canary. All input records here are synthetic fixtures.

Uses the production dispatch/conversion bodies, not a Python reimplementation.
This proves diagnostic coverage only; it is never native Windows evidence.
"""
import os
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]

HEADER = r"""
#![allow(dead_code, non_snake_case, non_camel_case_types)]
use std::cell::RefCell;
const KEY_EVENT: u32 = 1;
const MOUSE_EVENT: u32 = 2;
const FOCUS_EVENT: u32 = 16;
#[derive(Clone, Copy)]
struct KEY_EVENT_RECORD {
    bKeyDown: i32, wRepeatCount: u16, wVirtualKeyCode: u16,
    wVirtualScanCode: u16, uChar: KEY_EVENT_RECORD_0, dwControlKeyState: u32,
}
#[derive(Clone, Copy)]
union KEY_EVENT_RECORD_0 { UnicodeChar: u16 }
#[derive(Clone, Copy)]
struct MOUSE_EVENT_RECORD;
#[derive(Clone, Copy)]
struct FOCUS_EVENT_RECORD { bSetFocus: i32 }
#[derive(Clone, Copy)]
union INPUT_RECORD_0 {
    KeyEvent: KEY_EVENT_RECORD, MouseEvent: MOUSE_EVENT_RECORD,
    FocusEvent: FOCUS_EVENT_RECORD,
}
struct INPUT_RECORD { EventType: u16, Event: INPUT_RECORD_0 }
thread_local! { static TRACED: RefCell<Vec<ConsoleKeyEvent>> = const { RefCell::new(Vec::new()) }; }
fn trace_console_key_event(event: ConsoleKeyEvent) {
    TRACED.with(|trace| trace.borrow_mut().push(event));
}
fn fixture(down: bool) -> INPUT_RECORD {
    INPUT_RECORD { EventType: 1, Event: INPUT_RECORD_0 { KeyEvent: KEY_EVENT_RECORD {
        bKeyDown: i32::from(down), wRepeatCount: 1, wVirtualKeyCode: 27,
        wVirtualScanCode: 1, uChar: KEY_EVENT_RECORD_0 { UnicodeChar: 27 },
        dwControlKeyState: 0,
    } } }
}
#[test]
fn isolated_escape_down_and_up_are_both_observable() {
    let mut events = Vec::new();
    append_batch_events(&[fixture(true), fixture(false)], &mut events);
    TRACED.with(|trace| {
        let observed = trace.borrow();
        assert_eq!(observed.len(), 2, "raw Escape records were omitted by dispatch");
        assert!(observed[0].key_down);
        assert!(!observed[1].key_down);
        for event in observed.iter() {
            assert_eq!((event.repeat_count, event.virtual_key_code,
                event.virtual_scan_code, event.unicode_char, event.control_key_state),
                (1, 27, 1, 27, 0));
        }
    });
}
#[test]
fn observation_preserves_batch_conversion() {
    let mut events = Vec::new();
    append_batch_events(&[fixture(true), fixture(false)], &mut events);
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], BatchEvent::Key(ConsoleKeyEvent { key_down: true, .. })));
    assert!(matches!(events[1], BatchEvent::Key(ConsoleKeyEvent { key_down: false, .. })));
}
"""


class EscapeTrace(unittest.TestCase):
    def test_compiled_production_batch_dispatch(self):
        source = (ROOT / 'crates/rmux-client/src/attach/windows/input/events.rs').read_text()
        dispatch = source[source.index('pub(super) enum BatchEvent'):source.index(
            'pub(super) fn encode_live_input_batch')]
        conversion = source[source.index('#[derive(Clone, Copy, Debug, PartialEq, Eq)]'):]
        body = (dispatch + conversion).replace('pub(super)', 'pub(crate)')
        head = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
        host = {'darwin': 'macos', 'linux': 'linux', 'win32': 'windows'}[__import__('sys').platform]
        target = ROOT / '.rmux-audit' / 's6-082' / head / host / 'dispatch'
        target.mkdir(parents=True, exist_ok=True)
        fixture = target / 'dispatch.rs'
        binary = target / ('dispatch.exe' if host == 'windows' else 'dispatch')
        fixture.write_text(HEADER + body)
        rustc = shutil.which('rustc')
        self.assertIsNotNone(rustc)
        subprocess.run([rustc, '--edition=2021', '--test', str(fixture), '-o', str(binary)], check=True,
                       capture_output=True, timeout=60)
        result = subprocess.run([str(binary), '--test-threads=1'], capture_output=True, text=True,
                                timeout=10)
        (target / 'result.log').write_text(result.stdout + result.stderr)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
