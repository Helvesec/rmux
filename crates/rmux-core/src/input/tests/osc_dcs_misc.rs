use super::*;

#[test]
fn osc_4_palette() {
    let (_p, w) = parse(b"\x1b]4;1;red\x1b\\");
    assert!(w.has_call("osc_palette("));
}

#[test]
fn osc_7_path() {
    let (_p, w) = parse(b"\x1b]7;file:///tmp\x1b\\");
    assert!(w.has_call("set_path(\"file:///tmp\")"));
}

#[test]
fn osc_8_hyperlink() {
    let (_p, w) = parse(b"\x1b]8;;https://example.com\x1b\\");
    assert!(w.has_call("osc_hyperlink("));
}

#[test]
fn osc_52_clipboard() {
    let (_p, w) = parse(b"\x1b]52;c;SGVsbG8=\x1b\\");
    assert!(w.has_call("osc_clipboard("));
}

#[test]
fn osc_133_shell_integration() {
    let (_p, w) = parse(b"\x1b]133;A\x1b\\");
    assert!(w.has_call("osc_shell_integration("));
}

// ─── DCS tests ─────────────────────────────────────────────────────

#[test]
fn dcs_passthrough_tmux_prefix() {
    let (_p, w) = parse(b"\x1bPtmux;hello world\x1b\\");
    assert!(w.has_call("dcs_passthrough(\"hello world\")"));
}

#[test]
fn dcs_passthrough_tmux_prefix_preserves_inner_escape() {
    let (_p, w) = parse(b"\x1bPtmux;\x1b]52;c;QQ==\x07\x1b\\");
    assert!(w.has_call("dcs_passthrough(\"\\u{1b}]52;c;QQ==\\u{7}\")"));
}

#[test]
fn dcs_passthrough_tmux_prefix_decodes_doubled_inner_escape() {
    let (_p, w) = parse(b"\x1bPtmux;\x1b\x1b]52;c;QQ==\x07\x1b\\");
    assert!(w.has_call("dcs_passthrough(\"\\u{1b}]52;c;QQ==\\u{7}\")"));
}

#[test]
fn dcs_tmux_wrapped_osc_bel_without_outer_st_recovers_to_ground() {
    let (p, w) = parse(b"\x1bPtmux;\x1b]4;0;?\x07AFTER");
    assert_eq!(p.state(), InputState::Ground);
    assert!(w.has_call("dcs_passthrough(\"\\u{1b}]4;0;?\\u{7}\")"));
    assert_eq!(w.chars, vec!['A', 'F', 'T', 'E', 'R']);
}

#[test]
fn dcs_tmux_wrapped_osc_bel_recovers_after_doubled_inner_escape() {
    let (p, w) = parse(b"\x1bPtmux;\x1b\x1b]4;0;?\x07AFTER");
    assert_eq!(p.state(), InputState::Ground);
    assert!(w.has_call("dcs_passthrough(\"\\u{1b}]4;0;?\\u{7}\")"));
    assert_eq!(w.chars, vec!['A', 'F', 'T', 'E', 'R']);
}

#[test]
fn dcs_tmux_bel_does_not_terminate_non_osc_passthrough() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);

    parser.parse(b"\x1bPtmux;not-osc\x07AFTER", &mut writer);
    assert_eq!(parser.state(), InputState::DcsHandler);
    assert!(!writer.has_call("dcs_passthrough"));
    assert!(writer.chars.is_empty());

    parser.parse(b"\x1b\\", &mut writer);
    assert_eq!(parser.state(), InputState::Ground);
    assert!(writer.has_call("dcs_passthrough(\"not-osc\\u{7}AFTER\")"));
}

#[test]
fn dcs_sixel_uses_passthrough() {
    let (_p, w) = parse(b"\x1bPq\"1;1;2;2#0!10~\x1b\\");
    assert!(w.has_call("sixel_passthrough(\"q\\\"1;1;2;2#0!10~\")"));
}

#[test]
fn dcs_sixel_preserves_parameters() {
    let (_p, w) = parse(b"\x1bP1;2q#0!10~\x1b\\");
    assert!(w.has_call("sixel_passthrough(\"1;2q#0!10~\")"));
}

#[test]
fn dcs_decrqss_unrecognized() {
    let (p, _w) = parse(b"\x1bP$qx\x1b\\");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert!(replies.contains("\x1bP0$r\x1b\\"));
}

#[test]
fn dcs_decrqss_is_not_sixel() {
    let (_p, w) = parse(b"\x1bP$qm\x1b\\");
    assert!(!w.has_call("sixel_passthrough"));
}

// ─── INPUT_LAST and REP tests ──────────────────────────────────────

#[test]
fn rep_repeats_last_printable_character() {
    let (_p, w) = parse(b"X\x1b[3b");
    // 'X' printed once, then REP 3 times.
    let x_count = w.chars.iter().filter(|&&c| c == 'X').count();
    assert_eq!(x_count, 4); // 1 original + 3 REP
}

#[test]
fn rep_does_nothing_without_prior_print() {
    let (_p, w) = parse(b"\x1b[3b");
    // No prior print, so REP should do nothing.
    assert!(w.chars.is_empty());
}

#[test]
fn c0_clears_input_last_so_rep_fails() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Print 'A', then BEL (C0), then REP.
    parser.parse(b"A\x07\x1b[1b", &mut writer);
    // 'A' printed once, BEL clears INPUT_LAST, REP should not repeat.
    let a_count = writer.chars.iter().filter(|&&c| c == 'A').count();
    assert_eq!(a_count, 1);
}

// ─── INPUT_DISCARD tests ──────────────────────────────────────────

#[test]
fn discard_on_intermediate_overflow() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    // Build a CSI sequence with more than 3 intermediates to overflow the buffer.
    // CSI enter -> intermediate overflows at 4 bytes.
    parser.parse(b"\x1b[", &mut writer);
    parser.parse(b" ", &mut writer); // interm 1 -> csi_intermediate
    parser.parse(b" ", &mut writer); // interm 2
    parser.parse(b" ", &mut writer); // interm 3 (buffer full at index 3)
    parser.parse(b" ", &mut writer); // interm 4 -> sets DISCARD
    parser.parse(b"m", &mut writer); // final byte, but DISCARD is set
                                     // Should not have dispatched SGR.
    assert!(!writer.has_call("mode_set"));
}

// ─── WINOPS tests ──────────────────────────────────────────────────

#[test]
fn winops_size_report_18() {
    let (p, _w) = parse(b"\x1b[18t");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[8;24;80t");
}

#[test]
fn winops_title_push_pop() {
    let (_p, w) = parse(b"\x1b[22;0t");
    assert!(w.has_call("push_title()"));
    let (_p, w) = parse(b"\x1b[23;0t");
    assert!(w.has_call("pop_title()"));
}

// ─── 17-state completeness test ───────────────────────────────────

#[test]
fn all_17_states_exist() {
    // Verify all states are reachable and have transition tables.
    let states = [
        InputState::Ground,
        InputState::EscEnter,
        InputState::EscIntermediate,
        InputState::CsiEnter,
        InputState::CsiParameter,
        InputState::CsiIntermediate,
        InputState::CsiIgnore,
        InputState::DcsEnter,
        InputState::DcsParameter,
        InputState::DcsIntermediate,
        InputState::DcsHandler,
        InputState::DcsEscape,
        InputState::DcsIgnore,
        InputState::OscString,
        InputState::ApcString,
        InputState::RenameString,
        InputState::ConsumeSt,
    ];
    assert_eq!(states.len(), 17);
    for state in states {
        assert!(
            !state.transition_table().is_empty(),
            "state {state:?} has empty transition table"
        );
    }
}

// ─── MODSET/MODOFF tests ──────────────────────────────────────────

// ─── OSC 10/11/12 colour queries ────────────────────────────────────
//
// Detached sessions have no attached terminal to answer colour queries, so
// the emulator replies itself (theme-detecting TUIs mis-render otherwise).
// NOTE: these assert the compiled-in defaults; the RMUX_DEFAULT_* env
// overrides are read once per process, so they are not exercised here.

#[test]
fn osc_10_fg_query_replies_with_default_st() {
    let (p, _w) = parse(b"\x1b]10;?\x1b\\");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b]10;rgb:cccc/cccc/cccc\x1b\\");
}

#[test]
fn osc_11_bg_query_replies_with_default_bel() {
    let (p, _w) = parse(b"\x1b]11;?\x07");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b]11;rgb:0000/0000/0000\x07");
}

#[test]
fn osc_12_cursor_query_replies_with_default() {
    let (p, _w) = parse(b"\x1b]12;?\x1b\\");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b]12;rgb:cccc/cccc/cccc\x1b\\");
}

#[test]
fn osc_11_set_then_query_round_trips() {
    let (p, _w) = parse(b"\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]11;?\x1b\\");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b]11;rgb:1111/2222/3333\x1b\\");
}

#[test]
fn osc_111_reset_restores_default_bg() {
    let (p, _w) = parse(b"\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]111\x1b\\\x1b]11;?\x1b\\");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b]11;rgb:0000/0000/0000\x1b\\");
}

#[test]
fn osc_10_set_still_reaches_writer() {
    let (_p, w) = parse(b"\x1b]10;rgb:eeee/eeee/eeee\x1b\\");
    assert!(w.has_call("osc_fg_colour("));
}

#[test]
fn osc_11_oversized_set_is_not_stored_so_queries_cannot_amplify() {
    // An application sets a ~1 MiB "colour" and then floods queries in the
    // same read batch. The oversized value must not be stored (it is not a
    // colour), so each query answers the small default instead of reflecting
    // megabytes: without the length bound reply_buf would balloon to ~100 MiB.
    let mut payload = Vec::new();
    payload.extend_from_slice(b"\x1b]11;rgb:");
    payload.extend(std::iter::repeat_n(b'A', 1_000_000));
    payload.extend_from_slice(b"\x1b\\");
    for _ in 0..100 {
        payload.extend_from_slice(b"\x1b]11;?\x07");
    }
    let (p, _w) = parse(&payload);
    assert!(
        p.reply_buf.len() < 100_000,
        "reply buffer amplified to {} bytes",
        p.reply_buf.len()
    );
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert!(replies.contains("\x1b]11;rgb:0000/0000/0000\x07"));
}
