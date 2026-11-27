//! Regression inputs from a Windows 11 OpenSSH ReadConsoleInputW, 2026-10-07.
//! Legacy: VK=27, scan=1, Unicode=27, state=0, repeat=1, down then up.
//! VT mode: zero VK/scan/state, Unicode text containing the two frames below.
//! Replaying these observations is a unit test, not native application proof.
use super::{
    append_batch_events, encode_live_input_batch, vt::VtConsoleInput, AttachInput, ConsoleKeyEvent,
};
use windows_sys::Win32::System::Console::{
    INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, LEFT_ALT_PRESSED,
    LEFT_CTRL_PRESSED, SHIFT_PRESSED,
};

const OBSERVED_VT: &[u8] = b"\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_";

fn record(vk: u16, scan: u16, character: u16, down: bool, state: u32) -> INPUT_RECORD {
    INPUT_RECORD {
        EventType: 1,
        Event: INPUT_RECORD_0 {
            KeyEvent: KEY_EVENT_RECORD {
                bKeyDown: i32::from(down),
                wRepeatCount: 1,
                wVirtualKeyCode: vk,
                wVirtualScanCode: scan,
                uChar: KEY_EVENT_RECORD_0 {
                    UnicodeChar: character,
                },
                dwControlKeyState: state,
            },
        },
    }
}

fn encode(records: &[INPUT_RECORD], pending: &mut Option<u16>) -> Vec<AttachInput> {
    let mut events = Vec::new();
    append_batch_events(records, &mut events);
    encode_live_input_batch(&events, pending, &mut 0)
}

fn payload(inputs: Vec<AttachInput>) -> Vec<u8> {
    inputs
        .iter()
        .flat_map(|input| {
            (0..input.repeat_count()).flat_map(move |_| input.payload().iter().copied())
        })
        .collect()
}

fn vt_records(bytes: &[u8]) -> Vec<INPUT_RECORD> {
    bytes
        .iter()
        .map(|byte| record(0, 0, u16::from(*byte), true, 0))
        .collect()
}

#[test]
fn escape_observed_legacy_down_up_emits_once_across_record_boundaries() {
    let records = [record(27, 1, 27, true, 0), record(27, 1, 27, false, 0)];
    for split in 0..=records.len() {
        let mut pending = None;
        let mut bytes = payload(encode(&records[..split], &mut pending));
        bytes.extend(payload(encode(&records[split..], &mut pending)));
        assert_eq!(bytes, b"\x1b", "split={split}");
    }
}

#[test]
fn escape_observed_vt_frames_emit_once_at_every_fragment_boundary() {
    let records = vt_records(OBSERVED_VT);
    for split in 0..=records.len() {
        let mut decoder = VtConsoleInput::default();
        let mut pending = None;
        let mut bytes = payload(decoder.push(encode(&records[..split], &mut pending)));
        bytes.extend(payload(
            decoder.push(encode(&records[split..], &mut pending)),
        ));
        assert_eq!(bytes, b"\x1b", "split={split}");
    }
}

#[test]
fn escape_observed_vt_down_is_delivered_before_keyup_or_following_text() {
    let down = vt_records(b"\x1b[27;1;27;1;0;1_");
    let up = vt_records(b"\x1b[27;1;27;0;0;1_");
    let mut decoder = VtConsoleInput::default();
    assert_eq!(payload(decoder.push(encode(&down, &mut None))), b"\x1b");
    assert!(payload(decoder.push(encode(&up, &mut None))).is_empty());
    assert_eq!(
        payload(decoder.push(encode(&vt_records(b"x"), &mut None))),
        b"x"
    );
}

#[test]
fn escape_observed_vt_bytewise_fragmentation_keeps_the_single_escape() {
    let mut decoder = VtConsoleInput::default();
    let mut bytes = Vec::new();
    for record in vt_records(OBSERVED_VT) {
        bytes.extend(payload(decoder.push(encode(&[record], &mut None))));
    }
    assert_eq!(bytes, b"\x1b");
}

#[test]
fn escape_observed_synthetic_modifier_neighbors_preserve_existing_mapping() {
    // Neighbor variants are synthetic, not additional native observations.
    for (vk, character, state, expected) in [
        (27, 27, LEFT_ALT_PRESSED, b"\x1b\x1b".as_slice()),
        (27, 27, SHIFT_PRESSED, b"\x1b".as_slice()),
        (9, 9, SHIFT_PRESSED, b"\x1b[Z".as_slice()),
        (13, 13, LEFT_CTRL_PRESSED, b"\x1b[13;5u".as_slice()),
        (38, 0, 0, b"\x1b[A".as_slice()),
        (38, 0, LEFT_CTRL_PRESSED, b"\x1b[1;5A".as_slice()),
    ] {
        assert_eq!(
            payload(encode(&[record(vk, 0, character, true, state)], &mut None)),
            expected
        );
    }
}

#[test]
fn escape_observed_conversion_preserves_all_native_fields() {
    let mut events = Vec::new();
    append_batch_events(&[record(27, 1, 27, true, 0)], &mut events);
    assert!(matches!(
        events.as_slice(),
        [super::BatchEvent::Key(ConsoleKeyEvent {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 27,
            virtual_scan_code: 1,
            unicode_char: 27,
            control_key_state: 0,
        })]
    ));
}
