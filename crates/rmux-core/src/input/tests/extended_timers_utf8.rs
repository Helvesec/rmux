use super::*;

#[test]
fn modset_extended_keys_mode_2() {
    let (_p, w) = parse(b"\x1b[>4;2m");
    assert!(w.has_call("mode_clear(0x248000)")); // EXTENDED_KEY_MODES
    assert!(w.has_call("mode_set(0x40000)")); // MODE_KEYS_EXTENDED_2
}

#[test]
fn modoff_clears_extended_keys() {
    let (_p, w) = parse(b"\x1b[>4n");
    assert!(w.has_call("mode_clear(0x248000)")); // EXTENDED_KEY_MODES
}

#[test]
fn kitty_keyboard_requests_are_consumed_without_negotiating_support() {
    for request in [
        b"\x1b[=8u".as_slice(),
        b"\x1b[=0u".as_slice(),
        b"\x1b[=1;3u".as_slice(),
        b"\x1b[>1u".as_slice(),
        b"\x1b[>0u".as_slice(),
        b"\x1b[>1;3u".as_slice(),
        b"\x1b[<u".as_slice(),
        b"\x1b[<2u".as_slice(),
        b"\x1b[?u".as_slice(),
    ] {
        let (parser, writer) = parse(request);

        assert!(parser.reply_buf.is_empty(), "request {request:?}");
        assert_eq!(writer.mode, MODE_CURSOR | MODE_WRAP, "request {request:?}");
        assert!(!writer.has_call("mode_set("), "request {request:?}");
        assert!(!writer.has_call("mode_clear("), "request {request:?}");
    }
}

#[test]
fn kitty_keyboard_requests_preserve_xterm_extended_key_mode() {
    let (_parser, writer) = parse(b"\x1b[>4;2m\x1b[=8u\x1b[>1u\x1b[<u\x1b[?u");
    let mode_transitions = writer
        .calls
        .iter()
        .filter(|call| call.starts_with("mode_set(") || call.starts_with("mode_clear("))
        .map(String::as_str)
        .collect::<Vec<_>>();

    assert_eq!(writer.mode, MODE_CURSOR | MODE_WRAP | MODE_KEYS_EXTENDED_2);
    assert_eq!(
        mode_transitions,
        ["mode_clear(0x248000)", "mode_set(0x40000)"]
    );
}

// ─── Hardening: ground timer ───────────────────────────────────────

#[test]
fn ground_timer_active_in_non_ground_state() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    assert!(!parser.ground_timer_active());
    // Enter OSC string — timer should be active.
    parser.parse(b"\x1b]", &mut writer);
    assert_eq!(parser.state(), InputState::OscString);
    assert!(parser.ground_timer_active());
}

#[test]
fn ground_timer_expired_resets_to_ground() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(b"\x1b]2;incomplete", &mut writer);
    assert_eq!(parser.state(), InputState::OscString);
    parser.ground_timer_expired();
    assert_eq!(parser.state(), InputState::Ground);
    assert!(!parser.ground_timer_active());
}

#[test]
fn ground_timer_expired_clears_since_ground_buffer() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(b"\x1b]2;incomplete", &mut writer);
    assert!(!parser.take_since_ground().is_empty());

    parser.parse(b"\x1b]2;incomplete", &mut writer);
    parser.ground_timer_expired();

    assert_eq!(parser.state(), InputState::Ground);
    assert!(parser.take_since_ground().is_empty());
}

#[test]
fn since_ground_is_bounded_for_unterminated_strings() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.set_input_buffer_limit(INPUT_BUF_START);
    parser.parse(b"\x1b]", &mut writer);
    let flood = vec![b'a'; INPUT_BUF_START * 4];
    parser.parse(&flood, &mut writer);

    assert_eq!(parser.state(), InputState::OscString);
    assert_eq!(parser.take_since_ground().len(), INPUT_BUF_START);
}

// ─── Hardening: parameter overflow discard ─────────────────────────

#[test]
fn discard_on_parameter_overflow() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Build a CSI sequence with parameter buffer overflow (64 bytes).
    let mut seq = b"\x1b[".to_vec();
    // Fill with digits exceeding PARAM_BUF_MAX (64).
    seq.extend(std::iter::repeat_n(b'1', 70));
    seq.push(b'A'); // CUU final byte
    parser.parse(&seq, &mut writer);
    // Dispatch should have been discarded.
    assert!(!writer.has_call("cursor_up"));
}

// ─── Hardening: CSI ignore state ──────────────────────────────────

#[test]
fn csi_ignore_state_absorbs_then_returns_to_ground() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // CSI with conflicting private markers: 0x3c in csi_parameter -> csi_ignore.
    parser.parse(b"\x1b[1", &mut writer);
    assert_eq!(parser.state(), InputState::CsiParameter);
    parser.parse(b"<", &mut writer); // 0x3c in csi_parameter -> csi_ignore
    assert_eq!(parser.state(), InputState::CsiIgnore);
    // Data is absorbed.
    parser.parse(b"12345", &mut writer);
    assert_eq!(parser.state(), InputState::CsiIgnore);
    // Final byte 0x40-0x7e returns to ground without dispatch.
    parser.parse(b"A", &mut writer);
    assert_eq!(parser.state(), InputState::Ground);
    assert!(!writer.has_call("cursor_up"));
}

// ─── Hardening: UTF-8 edge cases ──────────────────────────────────

#[test]
fn utf8_truncated_sequence_emits_replacement_on_c0() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Start of 2-byte UTF-8 sequence, then C0 interrupts.
    parser.parse(b"\xc3", &mut writer);
    assert!(writer.chars.is_empty()); // Accumulating.
    parser.parse(b"\x07", &mut writer); // BEL interrupts.
    assert!(writer.chars.contains(&'\u{FFFD}'));
    assert!(writer.has_call("bell()"));
}

#[test]
fn utf8_overlong_start_byte_emits_replacement() {
    let (_, w) = parse(b"\xfe\xff");
    // 0xFE and 0xFF are not valid UTF-8 start bytes.
    for c in &w.chars {
        assert_eq!(*c, '\u{FFFD}');
    }
}

#[test]
fn utf8_invalid_continuation_restarts_sequence() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Start 3-byte sequence 0xE2, then 0xC3 (another start byte instead of continuation).
    parser.parse(b"\xe2\xc3\xa9", &mut writer);
    // Should emit U+FFFD for the broken 3-byte, then decode 0xC3 0xA9 = 'é'.
    assert!(writer.chars.contains(&'\u{FFFD}'));
    assert!(writer.chars.contains(&'é'));
}

// ─── Hardening: DECRPM queries ────────────────────────────────────

#[test]
fn decrpm_focuson_query() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Enable focus events first.
    parser.parse(b"\x1b[?1004h", &mut writer);
    // Query DECRPM for 1004.
    parser.parse(b"\x1b[?1004$p", &mut writer);
    let replies = String::from_utf8_lossy(&parser.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[?1004;1$y"); // 1 = set
}

#[test]
fn decrpm_bracketpaste_not_set() {
    let (p, _w) = parse(b"\x1b[?2004$p");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[?2004;2$y"); // 2 = not set
}

// ─── Hardening: SCP/RCP round-trip ────────────────────────────────

#[test]
fn scp_rcp_saves_and_restores_cursor_and_cell_state() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Set some attributes and move cursor.
    parser.parse(b"\x1b[1;3m", &mut writer); // bold+italic
    writer.cx = 10;
    writer.cy = 5;
    // SCP (CSI s).
    parser.parse(b"\x1b[s", &mut writer);
    // Change attributes and cursor.
    parser.parse(b"\x1b[0m", &mut writer);
    writer.cx = 0;
    writer.cy = 0;
    // RCP (CSI u).
    parser.parse(b"\x1b[u", &mut writer);
    // Verify attributes restored.
    assert!(parser.cell_state().attr() & GridAttr::BRIGHT != 0);
    assert!(parser.cell_state().attr() & GridAttr::ITALICS != 0);
    // Verify cursor move was called with saved position.
    assert!(writer.has_call("cursor_move(10, 5, false)"));
}

// ─── Hardening: DECSC/DECRC round-trip ────────────────────────────

#[test]
fn decsc_decrc_saves_charset_state() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Set G0 to ACS.
    parser.parse(b"\x1b(0", &mut writer);
    assert_eq!(parser.cell_state().g0set, 1);
    // DECSC.
    parser.parse(b"\x1b7", &mut writer);
    // Change G0 back.
    parser.parse(b"\x1b(B", &mut writer);
    assert_eq!(parser.cell_state().g0set, 0);
    // DECRC.
    parser.parse(b"\x1b8", &mut writer);
    assert_eq!(parser.cell_state().g0set, 1);
}

// ─── Hardening: WINOPS multi-param consumption ────────────────────
