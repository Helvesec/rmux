#[test]
fn console_focus_records_preserve_order_and_pasted_focus_text() {
    use windows_sys::Win32::System::Console::{FOCUS_EVENT, FOCUS_EVENT_RECORD};
    let focus = |set_focus| INPUT_RECORD {
        EventType: FOCUS_EVENT as u16,
        Event: INPUT_RECORD_0 {
            FocusEvent: FOCUS_EVENT_RECORD {
                bSetFocus: set_focus,
            },
        },
    };
    // Use actual Win32 records through the console reader, including its
    // batch boundaries; focus is not injected through a wire Data frame.
    let records = [
        console_key_input_record_from_event(key_event(b'B' as u16, 2, LEFT_CTRL_PRESSED)),
        focus(0),
        console_key_input_record(b'x'),
        focus(1),
    ];
    for caps in [vec![32], vec![1, 1, 1, 1]] {
        let api = FakeConsoleInput::with_read_caps(records, caps);
        let mut reader = fake_console_reader();
        let coordinator = ConsoleIoCoordinator::new();
        let mut inputs = Vec::new();
        while api.event_count(reader.handle).unwrap() != 0 {
            inputs.extend(reader.read_key_inputs_with(&coordinator, &api).unwrap());
        }
        let payloads: Vec<_> = inputs.iter().map(AttachInput::payload).collect();
        assert_eq!(payloads, [b"\x02".as_slice(), b"\x1b[O", b"x", b"\x1b[I"]);
    }
    // Literal clipboard focus bytes remain protected by the paste envelope.
    let api = FakeConsoleInput::new(
        b"ab\x1b[Icd"
            .iter()
            .copied()
            .map(console_paste_input_record),
    );
    let mut reader = fake_console_reader();
    let inputs = reader
        .read_key_inputs_with(&ConsoleIoCoordinator::new(), &api)
        .unwrap();
    assert_eq!(batch_bytes(&inputs), b"\x1b[200~ab\x1b[Icd\x1b[201~");
}

#[test]
fn pasted_console_focus_records_keep_their_position_and_context() {
    use windows_sys::Win32::System::Console::{FOCUS_EVENT, FOCUS_EVENT_RECORD};
    let focus = |set_focus| INPUT_RECORD {
        EventType: FOCUS_EVENT as u16,
        Event: INPUT_RECORD_0 {
            FocusEvent: FOCUS_EVENT_RECORD {
                bSetFocus: set_focus,
            },
        },
    };
    let records = [
        console_paste_input_record(b'a'),
        console_paste_input_record(b'b'),
        focus(1),
        console_paste_input_record(b'c'),
        focus(0),
    ];
    for caps in [vec![32], vec![2, 1, 1, 1]] {
        let api = FakeConsoleInput::with_read_caps(records, caps);
        let mut reader = fake_console_reader();
        let mut bytes = Vec::new();
        while api.event_count(reader.handle).unwrap() != 0 {
            bytes.extend(batch_bytes(
                &reader
                    .read_key_inputs_with(&ConsoleIoCoordinator::new(), &api)
                    .unwrap(),
            ));
        }
        assert_eq!(bytes, b"\x1b[200~ab\x1b[Ic\x1b[O\x1b[201~");
    }
}

#[test]
fn console_focus_before_a_fresh_paste_remains_live() {
    use windows_sys::Win32::System::Console::{FOCUS_EVENT, FOCUS_EVENT_RECORD};
    let focus = |set_focus| INPUT_RECORD {
        EventType: FOCUS_EVENT as u16,
        Event: INPUT_RECORD_0 {
            FocusEvent: FOCUS_EVENT_RECORD {
                bSetFocus: set_focus,
            },
        },
    };
    let api = FakeConsoleInput::new([
        focus(0),
        console_paste_input_record(b'a'),
        console_paste_input_record(b'b'),
        focus(1),
        console_paste_input_record(b'c'),
    ]);
    let mut reader = fake_console_reader();
    let inputs = reader
        .read_key_inputs_with(&ConsoleIoCoordinator::new(), &api)
        .unwrap();
    let payloads: Vec<_> = inputs.iter().map(AttachInput::payload).collect();
    assert_eq!(
        payloads,
        [b"\x1b[O".as_slice(), b"\x1b[200~ab\x1b[Ic\x1b[201~"]
    );
}
