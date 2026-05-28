use super::*;

#[test]
fn winops_op_8_consumes_two_extra_params() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Op 8 (resize) followed by rows and cols, then op 18 (report size).
    parser.parse(b"\x1b[8;40;120;18t", &mut writer);
    // Should have consumed params for op 8, then processed op 18.
    let replies = String::from_utf8_lossy(&parser.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[8;24;80t"); // size report
}

#[test]
fn winops_size_report_19() {
    let (p, _w) = parse(b"\x1b[19t");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[9;24;80t");
}

// ─── Hardening: DECSCUSR n=0 clears blinking set ─────────────────

#[test]
fn decscusr_zero_clears_blinking_set() {
    let (_p, w) = parse(b"\x1b[0 q");
    assert!(w.has_call("set_cursor_style(0)"));
    assert!(w.has_call("mode_clear(0x20000)")); // MODE_CURSOR_BLINKING_SET
}

// ─── Hardening: SGR all standard colours ──────────────────────────

#[test]
fn sgr_all_standard_fg_colours() {
    for n in 30..=37u8 {
        let seq = format!("\x1b[{n}m");
        let (p, _w) = parse(seq.as_bytes());
        assert_eq!(p.cell.fg(), i32::from(n - 30), "SGR {n} fg mismatch");
    }
}

#[test]
fn sgr_all_standard_bg_colours() {
    for n in 40..=47u8 {
        let seq = format!("\x1b[{n}m");
        let (p, _w) = parse(seq.as_bytes());
        assert_eq!(p.cell.bg(), i32::from(n - 40), "SGR {n} bg mismatch");
    }
}

// ─── Hardening: ESC dispatch clears INPUT_LAST ────────────────────

#[test]
fn esc_dispatch_clears_input_last_so_rep_fails() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Print 'A', ESC M (RI), then REP.
    parser.parse(b"A\x1bM\x1b[2b", &mut writer);
    let a_count = writer.chars.iter().filter(|&&c| c == 'A').count();
    assert_eq!(a_count, 1); // REP should not repeat after ESC dispatch.
}

// ─── Hardening: CSI dispatch clears INPUT_LAST ────────────────────

#[test]
fn csi_dispatch_clears_input_last_so_rep_fails() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Print 'B', CSI A (CUU), then REP.
    parser.parse(b"B\x1b[A\x1b[2b", &mut writer);
    let b_count = writer.chars.iter().filter(|&&c| c == 'B').count();
    assert_eq!(b_count, 1); // REP should not repeat after CSI dispatch.
}

// ─── Hardening: DCS ignore state ──────────────────────────────────

#[test]
fn dcs_ignore_absorbs_invalid_params() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // DCS with colon in param (0x3a in dcs_enter -> dcs_ignore).
    parser.parse(b"\x1bP:", &mut writer);
    assert_eq!(parser.state(), InputState::DcsIgnore);
    parser.parse(b"data here", &mut writer);
    assert_eq!(parser.state(), InputState::DcsIgnore);
    // ESC backslash terminates via ANYWHERE.
    parser.parse(b"\x1b\\", &mut writer);
    assert_eq!(parser.state(), InputState::Ground);
}

// ─── Hardening: PM goes to consume_st ─────────────────────────────

#[test]
fn pm_sequence_goes_to_consume_st() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // ESC ^ (PM, 0x5E) -> consume_st.
    parser.parse(b"\x1b^", &mut writer);
    assert_eq!(parser.state(), InputState::ConsumeSt);
    parser.parse(b"ignored data", &mut writer);
    assert_eq!(parser.state(), InputState::ConsumeSt);
    parser.parse(b"\x1b\\", &mut writer);
    assert_eq!(parser.state(), InputState::Ground);
}

// ─── Hardening: RIS resets cell state ─────────────────────────────

#[test]
fn ris_resets_all_cell_state() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(b"\x1b[1;31m\x1b(0", &mut writer);
    assert!(parser.cell_state().attr() & GridAttr::BRIGHT != 0);
    assert_eq!(parser.cell_state().g0set, 1);
    parser.parse(b"\x1bc", &mut writer);
    assert_eq!(parser.cell_state().attr(), 0);
    assert_eq!(parser.cell_state().g0set, 0);
    assert_eq!(parser.cell_state().fg(), COLOUR_DEFAULT);
}

// ─── Hardening: input buffer max size ─────────────────────────────

#[test]
fn input_buf_overflow_sets_discard() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Enter OSC, fill beyond INPUT_BUF_MAX (1 MiB).
    parser.parse(b"\x1b]2;", &mut writer);
    // Write 1 MiB + 1 bytes of data.
    let chunk = vec![b'A'; 1_048_577];
    parser.parse(&chunk, &mut writer);
    // Terminate.
    parser.parse(b"\x1b\\", &mut writer);
    // The title should not have been set because discard was triggered.
    assert!(!writer.has_call("set_title"));
}

#[test]
fn oversized_kitty_apc_records_a_passthrough_drop() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);

    parser.parse(b"\x1b_G", &mut writer);
    let chunk = vec![b'A'; crate::terminal_passthrough::MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES + 1];
    parser.parse(&chunk, &mut writer);
    parser.parse(b"\x1b\\", &mut writer);

    assert!(!writer.has_call("apc_passthrough"));
    assert_eq!(parser.take_terminal_passthrough_dropped_count(), 1);
    assert_eq!(parser.take_terminal_passthrough_dropped_count(), 0);
}

#[test]
fn oversized_sixel_dcs_records_a_passthrough_drop() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);

    parser.parse(b"\x1bPq", &mut writer);
    let chunk = vec![b'A'; crate::terminal_passthrough::MAX_TERMINAL_PASSTHROUGH_PAYLOAD_BYTES + 1];
    parser.parse(&chunk, &mut writer);
    parser.parse(b"\x1b\\", &mut writer);

    assert!(!writer.has_call("sixel_passthrough"));
    assert_eq!(parser.take_terminal_passthrough_dropped_count(), 1);
    assert_eq!(parser.take_terminal_passthrough_dropped_count(), 0);
}

// ─── Hardening: DSR status report ─────────────────────────────────

#[test]
fn dsr_status_ok() {
    let (p, _w) = parse(b"\x1b[5n");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[0n");
}

// ─── Hardening: take_replies drains buffer ────────────────────────

#[test]
fn take_replies_returns_and_drains() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(b"\x1b[c", &mut writer); // DA primary reply
    let replies = parser.take_replies();
    assert!(!replies.is_empty());
    // Second call should be empty.
    let empty = parser.take_replies();
    assert!(empty.is_empty());
}

// ─── Hardening: LF with CRLF mode ────────────────────────────────

#[test]
fn lf_triggers_cr_when_crlf_mode_set() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    writer.mode |= MODE_CRLF;
    parser.parse(b"\n", &mut writer);
    assert!(writer.has_call("linefeed(false,"));
    assert!(writer.has_call("carriage_return()"));
}

// ─── Hardening: NUL is no-op ──────────────────────────────────────

#[test]
fn c0_nul_is_noop() {
    let (_p, w) = parse(b"\x00");
    // NUL should not generate any meaningful call.
    let meaningful: Vec<_> = w
        .calls
        .iter()
        .filter(|c| !c.starts_with("collect_end"))
        .collect();
    assert!(meaningful.is_empty());
}
