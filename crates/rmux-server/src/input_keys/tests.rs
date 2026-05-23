use super::{
    decode_extended_key, decode_mouse, encode_key, encode_mouse_event, ExtendedKeyDecode,
    ExtendedKeyFormat, MouseDecode, MouseForwardEvent,
};
use rmux_core::{
    input::mode, key_string_lookup_string, KeyCode, KEYC_CTRL, KEYC_IMPLIED_META, KEYC_KEYPAD,
    KEYC_META, KEYC_SHIFT,
};

fn parse_key(name: &str) -> KeyCode {
    key_string_lookup_string(name).expect("key name parses")
}

#[test]
fn extended_key_round_trips_xterm_ascii_ctrl_meta() {
    let key = parse_key("M-C-a");
    let encoded = encode_key(mode::MODE_KEYS_EXTENDED_2, ExtendedKeyFormat::Xterm, key)
        .expect("extended key encodes");
    assert_eq!(encoded, b"\x1b[27;7;97~");
    assert_eq!(
        decode_extended_key(&encoded, Some(0x7f)),
        ExtendedKeyDecode::Matched {
            size: encoded.len(),
            key: key | KEYC_IMPLIED_META,
        }
    );
}

#[test]
fn extended_key_round_trips_csi_u_unicode() {
    let key = parse_key("C-\u{03c0}");
    let encoded =
        encode_key(mode::MODE_KEYS_EXTENDED_2, ExtendedKeyFormat::CsiU, key).expect("encode");
    assert_eq!(encoded, "\x1b[960;5u".as_bytes());
    assert_eq!(
        decode_extended_key(&encoded, None),
        ExtendedKeyDecode::Matched {
            size: encoded.len(),
            key,
        }
    );
}

#[test]
fn extended_key_shift_only_printables_strip_shift() {
    assert_eq!(
        decode_extended_key(b"\x1b[65;2u", None),
        ExtendedKeyDecode::Matched {
            size: 7,
            key: KeyCode::from(b'A'),
        }
    );
}

#[test]
fn extended_key_shift_tab_becomes_backtab() {
    let btab = parse_key("BTab");
    assert_eq!(
        decode_extended_key(b"\x1b[9;2u", None),
        ExtendedKeyDecode::Matched { size: 6, key: btab }
    );
}

#[test]
fn standard_mode_meta_printable_falls_back_to_escape_prefix() {
    let encoded = encode_key(0, ExtendedKeyFormat::Xterm, parse_key("M-a")).expect("encode");
    assert_eq!(encoded, b"\x1ba");
}

#[test]
fn mode1_prefers_vt10x_for_compatible_ctrl_keys() {
    let encoded = encode_key(
        mode::MODE_KEYS_EXTENDED,
        ExtendedKeyFormat::Xterm,
        parse_key("C-@"),
    )
    .expect("encode");
    assert_eq!(encoded, [0x00]);
}

#[test]
fn mouse_decode_supports_standard_and_sgr_sequences() {
    let old = decode_mouse(b"\x1b[M !!", None);
    assert_eq!(
        old,
        MouseDecode::Matched {
            size: 6,
            event: MouseForwardEvent {
                b: 0,
                lb: 0,
                x: 0,
                y: 0,
                lx: 0,
                ly: 0,
                sgr_b: 0,
                sgr_type: ' ',
                ignore: false,
            }
        }
    );

    let sgr = decode_mouse(b"\x1b[<35;12;7M", None);
    assert_eq!(
        sgr,
        MouseDecode::Matched {
            size: 11,
            event: MouseForwardEvent {
                b: 35,
                lb: 0,
                x: 11,
                y: 6,
                lx: 0,
                ly: 0,
                sgr_b: 35,
                sgr_type: 'M',
                ignore: false,
            }
        }
    );
}

#[test]
fn mouse_decode_discards_putty_release_wheel_sequences() {
    assert_eq!(
        decode_mouse(b"\x1b[<64;10;4m", None),
        MouseDecode::Discard { size: 11 }
    );
}

#[test]
fn mouse_output_encodes_all_three_formats() {
    let event = MouseForwardEvent::button_event(0, 0, 0);
    assert_eq!(
        encode_mouse_event(mode::MODE_MOUSE_STANDARD, &event, 1, 2).expect("legacy"),
        b"\x1b[M \"#"
    );
    assert_eq!(
        encode_mouse_event(
            mode::MODE_MOUSE_STANDARD | mode::MODE_MOUSE_UTF8,
            &event,
            1,
            2
        )
        .expect("utf8"),
        b"\x1b[M \"#"
    );
    assert_eq!(
        encode_mouse_event(
            mode::MODE_MOUSE_STANDARD | mode::MODE_MOUSE_SGR,
            &MouseForwardEvent {
                sgr_b: 0,
                sgr_type: 'M',
                ..event
            },
            1,
            2
        )
        .expect("sgr"),
        b"\x1b[<0;2;3M"
    );
}

#[test]
fn mouse_output_respects_motion_and_release_filters() {
    let drag = MouseForwardEvent {
        b: 32,
        lb: 0,
        x: 5,
        y: 6,
        lx: 4,
        ly: 5,
        sgr_b: 32,
        sgr_type: ' ',
        ignore: false,
    };
    assert!(
        encode_mouse_event(mode::MODE_MOUSE_STANDARD, &drag, 5, 6).is_none(),
        "drag events need motion mode"
    );
    assert!(
        encode_mouse_event(mode::MODE_MOUSE_BUTTON, &drag, 5, 6).is_some(),
        "button mode accepts drag motion"
    );

    let release = MouseForwardEvent {
        b: 35,
        lb: 35,
        x: 5,
        y: 6,
        lx: 5,
        ly: 6,
        sgr_b: 35,
        sgr_type: 'm',
        ignore: false,
    };
    assert!(
        encode_mouse_event(
            mode::MODE_MOUSE_STANDARD | mode::MODE_MOUSE_SGR,
            &release,
            5,
            6
        )
        .is_none(),
        "SGR releases need all-motion mode"
    );
    assert!(
        encode_mouse_event(mode::MODE_MOUSE_ALL | mode::MODE_MOUSE_SGR, &release, 5, 6).is_some(),
        "all mode forwards SGR releases"
    );
}

#[test]
fn legacy_mouse_output_clamps_large_coordinates() {
    let event = MouseForwardEvent::button_event(0, 0, 0);
    let encoded = encode_mouse_event(mode::MODE_MOUSE_STANDARD, &event, 500, 700).expect("legacy");
    assert_eq!(encoded, [0x1b, b'[', b'M', b' ', 0xff, 0xff]);
}

#[test]
fn utf8_mouse_output_rejects_out_of_range_button_values() {
    let event = MouseForwardEvent::button_event(0x900, 0, 0);
    assert!(encode_mouse_event(
        mode::MODE_MOUSE_STANDARD | mode::MODE_MOUSE_UTF8,
        &event,
        0,
        0
    )
    .is_none());
}

#[test]
fn modifier_bits_are_preserved_by_extended_decode() {
    let decoded = decode_extended_key(b"\x1b[97;8u", None);
    assert_eq!(
        decoded,
        ExtendedKeyDecode::Matched {
            size: 7,
            key: KeyCode::from(b'a') | KEYC_SHIFT | KEYC_CTRL | KEYC_META | KEYC_IMPLIED_META,
        }
    );
}

#[test]
fn extended_key_decode_rejects_invalid_prefix_for_xterm_format() {
    // xterm format requires prefix "27"
    assert_eq!(
        decode_extended_key(b"\x1b[28;2;65~", None),
        ExtendedKeyDecode::Invalid
    );
}

#[test]
fn extended_key_decode_rejects_extra_semicolons() {
    assert_eq!(
        decode_extended_key(b"\x1b[27;2;65;99~", None),
        ExtendedKeyDecode::Invalid
    );
    assert_eq!(
        decode_extended_key(b"\x1b[65;2;3u", None),
        ExtendedKeyDecode::Invalid
    );
}

#[test]
fn extended_key_decode_partial_returns_for_incomplete_input() {
    assert_eq!(
        decode_extended_key(b"\x1b", None),
        ExtendedKeyDecode::Partial
    );
    assert_eq!(
        decode_extended_key(b"\x1b[", None),
        ExtendedKeyDecode::Partial
    );
    assert_eq!(
        decode_extended_key(b"\x1b[27;2;65", None),
        ExtendedKeyDecode::Partial
    );
}

#[test]
fn extended_key_decode_rejects_non_esc_start() {
    assert_eq!(decode_extended_key(b"A", None), ExtendedKeyDecode::Invalid);
}

#[test]
fn extended_key_decode_modifiers_zero_means_no_modifiers() {
    // modifiers=0 is unusual but valid - no modifier bits set
    let decoded = decode_extended_key(b"\x1b[65;0u", None);
    assert_eq!(
        decoded,
        ExtendedKeyDecode::Matched {
            size: 7,
            key: KeyCode::from(b'A'),
        }
    );
}

#[test]
fn extended_key_decode_modifiers_one_means_no_modifiers() {
    // modifiers=1 means modifiers-1=0, so no modifier bits
    let decoded = decode_extended_key(b"\x1b[65;1u", None);
    assert_eq!(
        decoded,
        ExtendedKeyDecode::Matched {
            size: 7,
            key: KeyCode::from(b'A'),
        }
    );
}

#[test]
fn extended_key_backspace_option_maps_to_bspace() {
    // When backspace matches the terminal's VERASE, map to KEYC_BSPACE
    let decoded = decode_extended_key(b"\x1b[127;5u", Some(127));
    if let ExtendedKeyDecode::Matched { key, .. } = decoded {
        let base = key & rmux_core::KEYC_MASK_KEY;
        assert_eq!(base, rmux_core::KEYC_BSPACE);
    } else {
        panic!("expected matched");
    }
}

#[test]
fn extended_key_decode_supports_modified_cursor_sequences() {
    for (sequence, expected) in [
        (b"\x1b[1;5A".as_slice(), parse_key("Up") | KEYC_CTRL),
        (b"\x1b[1;5B".as_slice(), parse_key("Down") | KEYC_CTRL),
        (b"\x1b[1;5C".as_slice(), parse_key("Right") | KEYC_CTRL),
        (b"\x1b[1;5D".as_slice(), parse_key("Left") | KEYC_CTRL),
    ] {
        assert_eq!(
            decode_extended_key(sequence, None),
            ExtendedKeyDecode::Matched {
                size: sequence.len(),
                key: expected,
            }
        );
    }
}

#[test]
fn extended_key_encode_uses_xterm_modified_cursor_sequences() {
    for (name, expected) in [
        ("C-Up", b"\x1b[1;5A".as_slice()),
        ("C-Down", b"\x1b[1;5B".as_slice()),
        ("C-Right", b"\x1b[1;5C".as_slice()),
        ("C-Left", b"\x1b[1;5D".as_slice()),
        ("S-Up", b"\x1b[1;2A".as_slice()),
        ("M-Up", b"\x1b[1;3A".as_slice()),
        ("C-Home", b"\x1b[1;5H".as_slice()),
        ("C-End", b"\x1b[1;5F".as_slice()),
    ] {
        assert_eq!(
            encode_key(
                mode::MODE_KEYS_EXTENDED_2,
                ExtendedKeyFormat::Xterm,
                parse_key(name),
            )
            .as_deref(),
            Some(expected),
            "{name} should use xterm modified cursor encoding"
        );
    }
}

#[test]
fn standard_key_encode_uses_xterm_modified_cursor_sequences() {
    for (name, expected) in [
        ("C-Up", b"\x1b[1;5A".as_slice()),
        ("C-Down", b"\x1b[1;5B".as_slice()),
        ("C-Right", b"\x1b[1;5C".as_slice()),
        ("C-Left", b"\x1b[1;5D".as_slice()),
        ("S-Up", b"\x1b[1;2A".as_slice()),
        ("M-Up", b"\x1b[1;3A".as_slice()),
        ("C-Home", b"\x1b[1;5H".as_slice()),
        ("C-End", b"\x1b[1;5F".as_slice()),
    ] {
        assert_eq!(
            encode_key(0, ExtendedKeyFormat::Xterm, parse_key(name)).as_deref(),
            Some(expected),
            "{name} should use xterm modified cursor encoding"
        );
    }
}

#[test]
fn mouse_decode_rejects_non_esc_start() {
    assert_eq!(decode_mouse(b"X", None), MouseDecode::Invalid);
}

#[test]
fn mouse_decode_partial_on_short_legacy() {
    assert_eq!(decode_mouse(b"\x1b[M!!", None), MouseDecode::Partial);
}

#[test]
fn mouse_decode_sgr_partial_on_incomplete() {
    assert_eq!(decode_mouse(b"\x1b[<0;1;", None), MouseDecode::Partial);
}

#[test]
fn mouse_decode_sgr_rejects_zero_coordinates() {
    assert_eq!(
        decode_mouse(b"\x1b[<0;0;1M", None),
        MouseDecode::Discard { size: 9 }
    );
    assert_eq!(
        decode_mouse(b"\x1b[<0;1;0M", None),
        MouseDecode::Discard { size: 9 }
    );
}

#[test]
fn mouse_decode_legacy_discards_underflow() {
    // b < MOUSE_PARAM_BTN_OFF
    assert_eq!(
        decode_mouse(b"\x1b[M\x10!!", None),
        MouseDecode::Discard { size: 6 }
    );
}

#[test]
fn mouse_decode_preserves_last_event_positions() {
    let last = MouseForwardEvent {
        b: 0,
        lb: 0,
        x: 10,
        y: 20,
        lx: 5,
        ly: 15,
        sgr_b: 0,
        sgr_type: ' ',
        ignore: false,
    };
    let result = decode_mouse(b"\x1b[<0;5;8M", Some(last));
    if let MouseDecode::Matched { event, .. } = result {
        assert_eq!(event.lx, 10, "lx from previous event's x");
        assert_eq!(event.ly, 20, "ly from previous event's y");
        assert_eq!(event.lb, 0, "lb from previous event's b");
    } else {
        panic!("expected matched");
    }
}

#[test]
fn mouse_output_ignores_ignored_events() {
    let event = MouseForwardEvent {
        b: 0,
        lb: 0,
        x: 0,
        y: 0,
        lx: 0,
        ly: 0,
        sgr_b: 0,
        sgr_type: ' ',
        ignore: true,
    };
    assert!(encode_mouse_event(mode::MODE_MOUSE_STANDARD, &event, 0, 0).is_none());
}

#[test]
fn mouse_output_requires_mouse_mode_enabled() {
    let event = MouseForwardEvent::button_event(0, 0, 0);
    assert!(
        encode_mouse_event(0, &event, 0, 0).is_none(),
        "no mouse mode means no output"
    );
}

#[test]
fn mouse_decode_overflowing_decimal_returns_partial() {
    // 70000 overflows u16::MAX (65535), checked_mul/checked_add returns None
    // which makes parse_mouse_decimal return None, treated as Partial
    let result = decode_mouse(b"\x1b[<70000;1;1M", None);
    assert_eq!(
        result,
        MouseDecode::Partial,
        "overflow in decimal parse returns Partial"
    );
}

#[test]
fn sgr_mouse_release_falls_through_to_legacy_when_no_sgr_type() {
    let event = MouseForwardEvent {
        b: 3, // release
        lb: 0,
        x: 0,
        y: 0,
        lx: 0,
        ly: 0,
        sgr_b: 0,
        sgr_type: ' ', // non-SGR source
        ignore: false,
    };
    // SGR mode is set but event is from non-SGR terminal
    let result = encode_mouse_event(
        mode::MODE_MOUSE_STANDARD | mode::MODE_MOUSE_SGR,
        &event,
        0,
        0,
    );
    // Should fall through to legacy format since we can't convert
    // a legacy release to SGR format (button identity unknown)
    assert!(result.is_some());
    assert_eq!(result.unwrap(), b"\x1b[M#!!");
}

#[test]
fn non_sgr_source_with_sgr_mode_falls_through_to_legacy() {
    // When the terminal sent a non-SGR event but the application wants SGR,
    // we must fall through to legacy format (tmux behavior: can't convert
    // legacy button encoding to SGR).
    let event = MouseForwardEvent::button_event(0, 0, 0);
    assert_eq!(event.sgr_type, ' ', "source is non-SGR");
    let result = encode_mouse_event(
        mode::MODE_MOUSE_STANDARD | mode::MODE_MOUSE_SGR,
        &event,
        5,
        3,
    );
    assert!(result.is_some());
    assert_eq!(result.unwrap(), b"\x1b[M &$");
}

#[test]
fn encode_key_backtab_without_extended_mode_produces_escape_sequence() {
    let btab = parse_key("BTab");
    let encoded = encode_key(0, ExtendedKeyFormat::Xterm, btab).expect("backtab");
    assert_eq!(encoded, b"\x1b[Z");
}

#[test]
fn encode_key_backtab_in_extended_mode_produces_shift_tab() {
    let btab = parse_key("BTab");
    let encoded =
        encode_key(mode::MODE_KEYS_EXTENDED_2, ExtendedKeyFormat::Xterm, btab).expect("encode");
    // Should encode as Shift-Tab: \x1b[27;2;9~
    assert_eq!(encoded, b"\x1b[27;2;9~");
}

#[test]
fn trivial_keys_bypass_extended_encoding() {
    // Plain 'a' should just produce 'a' even in extended mode
    let key = KeyCode::from(b'a');
    let encoded =
        encode_key(mode::MODE_KEYS_EXTENDED_2, ExtendedKeyFormat::Xterm, key).expect("trivial");
    assert_eq!(encoded, b"a");
}

#[test]
fn vt10x_arrow_keys_match_tmux_standard_and_cursor_modes() {
    let up = parse_key("Up");
    assert_eq!(
        encode_key(0, ExtendedKeyFormat::Xterm, up).expect("standard up"),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key(mode::MODE_KCURSOR, ExtendedKeyFormat::Xterm, up).expect("cursor mode up"),
        b"\x1bOA"
    );
}

#[test]
fn extended_modes_do_not_drop_plain_navigation_keys() {
    for mode in [mode::MODE_KEYS_EXTENDED, mode::MODE_KEYS_EXTENDED_2] {
        for format in [ExtendedKeyFormat::Xterm, ExtendedKeyFormat::CsiU] {
            for (name, expected) in [
                ("Up", b"\x1b[A".as_slice()),
                ("Down", b"\x1b[B".as_slice()),
                ("Left", b"\x1b[D".as_slice()),
                ("Right", b"\x1b[C".as_slice()),
                ("Home", b"\x1b[1~".as_slice()),
                ("End", b"\x1b[4~".as_slice()),
                ("DC", b"\x1b[3~".as_slice()),
                ("PageUp", b"\x1b[5~".as_slice()),
                ("PageDown", b"\x1b[6~".as_slice()),
                ("F1", b"\x1bOP".as_slice()),
            ] {
                assert_eq!(
                    encode_key(mode, format, parse_key(name)).as_deref(),
                    Some(expected),
                    "{name} should fall back to its VT sequence in extended mode"
                );
            }
        }
    }
}

#[test]
fn vt10x_navigation_keys_match_tmux_standard_sequences() {
    assert_eq!(
        encode_key(0, ExtendedKeyFormat::Xterm, parse_key("Home")).expect("home"),
        b"\x1b[1~"
    );
    assert_eq!(
        encode_key(0, ExtendedKeyFormat::Xterm, parse_key("DC")).expect("delete"),
        b"\x1b[3~"
    );
    assert_eq!(
        encode_key(0, ExtendedKeyFormat::Xterm, parse_key("PageUp")).expect("page up"),
        b"\x1b[5~"
    );
}

#[test]
fn golden_standard_key_trace_for_navigation_and_modifiers() {
    let keys = [
        "Up", "Down", "Left", "Right", "Home", "End", "DC", "PageUp", "PageDown", "BTab", "F1",
        "C-a", "M-a",
    ];
    let encoded = keys
        .into_iter()
        .flat_map(|key| {
            encode_key(0, ExtendedKeyFormat::Xterm, parse_key(key))
                .unwrap_or_else(|| panic!("{key} must encode"))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        encoded,
        b"\x1b[A\x1b[B\x1b[D\x1b[C\x1b[1~\x1b[4~\x1b[3~\x1b[5~\x1b[6~\x1b[Z\x1bOP\x01\x1ba"
    );
}

#[test]
fn vt10x_keypad_keys_follow_application_mode() {
    let kp1 = parse_key("KP1");
    assert_eq!(kp1 & KEYC_KEYPAD, KEYC_KEYPAD);
    assert_eq!(
        encode_key(0, ExtendedKeyFormat::Xterm, kp1).expect("numeric keypad"),
        b"1"
    );
    assert_eq!(
        encode_key(mode::MODE_KKEYPAD, ExtendedKeyFormat::Xterm, kp1).expect("application keypad"),
        b"\x1bOq"
    );
}
