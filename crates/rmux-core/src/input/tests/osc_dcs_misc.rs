use super::*;
use crate::formats::{render_template, FormatContext};

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

#[test]
fn osc_133_writer_receives_complete_payload_only_after_terminator() {
    for data in ["A;aid=x", "B", "C", "D;7", "unknown"] {
        let mut parser = InputParser::new();
        let mut writer = RecordingWriter::new(80, 24);
        parser.parse(format!("\x1b]133;{data}\x1b").as_bytes(), &mut writer);
        assert!(!writer.has_call("osc_shell_integration("));
        parser.parse(b"\\", &mut writer);
        assert!(writer.has_call(&format!("osc_shell_integration({data:?})")));
    }
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
fn dcs_tmux_wrapped_osc_bel_without_outer_st_waits_for_explicit_timeout() {
    assert_unterminated_dcs_waits_for_timeout(b"\x1bPtmux;\x1b]4;0;?\x07AFTER");
}

#[test]
fn dcs_tmux_wrapped_osc_bel_with_doubled_escape_waits_for_explicit_timeout() {
    assert_unterminated_dcs_waits_for_timeout(b"\x1bPtmux;\x1b\x1b]4;0;?\x07AFTER");
}

fn assert_unterminated_dcs_waits_for_timeout(packet: &[u8]) {
    // A read boundary cannot distinguish missing ST from a fragmented DCS.
    // The existing server timeout discards the incomplete string; it does
    // not dispatch a guessed payload or replay its bytes into the grid.
    let (mut parser, mut writer) = parse(packet);
    assert_eq!(parser.state(), InputState::DcsHandler);
    assert!(parser.ground_timer_active());
    assert!(!writer.has_call("dcs_passthrough("));
    assert!(writer.chars.is_empty());

    parser.ground_timer_expired();
    assert_eq!(parser.state(), InputState::Ground);
    assert!(!parser.ground_timer_active());
    parser.parse(b"RECOVERED", &mut writer);
    assert_eq!(writer.chars, "RECOVERED".chars().collect::<Vec<_>>());
    assert!(!writer.has_call("dcs_passthrough("));
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

/// Whether a primary DA response advertises SIXEL, i.e. carries parameter `4`.
///
/// Measured on the pinned tmux 3.7b oracle pair, both built from the same
/// source tree: without `ENABLE_SIXEL` the reply is `ESC[?1;2c`, and with
/// `--enable-sixel` it is `ESC[?1;2;4c`.
fn primary_da_advertises_sixel(reply: &str) -> bool {
    reply
        .strip_prefix("\x1b[?")
        .and_then(|params| params.strip_suffix('c'))
        .is_some_and(|params| params.split(';').any(|param| param == "4"))
}

#[test]
fn bare_q_dcs_stays_classified_as_sixel_without_claiming_server_support() {
    // Naming a sequence is not supporting it. RMUX recognises a bare `q`-final
    // DCS unconditionally and hands the payload to the transient passthrough
    // path, but `#{sixel_support}` reports what tmux reports: `ENABLE_SIXEL`,
    // which compiles in a retained, scalable, croppable, redrawable
    // server-side image store, a text fallback for clients that cannot render
    // images, and a SIXEL-bearing primary DA. RMUX has the parser and none of
    // the store, so classification and advertised capability are asserted
    // independently here rather than defined in terms of each other.
    let (_p, w) = parse(b"\x1bPq#0!10~\x1b\\");
    assert!(
        w.has_call("sixel_passthrough(\"q#0!10~\")"),
        "a bare `q`-final DCS must still be classified as SIXEL passthrough"
    );
    assert_eq!(
        render_template("#{sixel_support}", &FormatContext::new()),
        "0",
        "DCS dispatch must not be advertised as tmux server SIXEL support"
    );
}

#[test]
fn primary_device_attributes_agree_with_the_advertised_sixel_capability() {
    // tmux drives both answers from the one build flag, so the pair cannot
    // disagree. Pinned 3.7b oracle pair:
    //   without `ENABLE_SIXEL`  -> DA `ESC[?1;2c`,   `#{sixel_support}` = 0
    //   with    `--enable-sixel` -> DA `ESC[?1;2;4c`, `#{sixel_support}` = 1
    // RMUX answers its children the non-SIXEL DA, so the token owes `0`.
    // Reporting `1` beside `ESC[?1;2c` tells an application that images are
    // supported and then denies them at the capability handshake.
    let (parser, _w) = parse(b"\x1b[c");
    let reply = String::from_utf8_lossy(&parser.reply_buf).into_owned();
    assert_eq!(reply, "\x1b[?1;2c", "child-facing primary DA moved");

    let token = render_template("#{sixel_support}", &FormatContext::new());
    assert_eq!(
        primary_da_advertises_sixel(&reply),
        token == "1",
        "`#{{sixel_support}}` = {token} disagrees with primary DA {reply:?}"
    );
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
fn rep_reports_recovery_rebase_when_it_uses_a_prior_feed_character() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(b"X", &mut writer);
    assert!(!parser.take_recovery_rebase_required());

    parser.parse(b"\x1b[3b", &mut writer);

    assert!(parser.take_recovery_rebase_required());
    assert!(!parser.take_recovery_rebase_required());
}

#[test]
fn rep_after_a_character_in_the_same_feed_is_self_contained() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);

    parser.parse(b"X\x1b[3b", &mut writer);

    assert!(!parser.take_recovery_rebase_required());
}

#[test]
fn fragmented_rep_reports_recovery_only_when_the_final_byte_dispatches() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(b"\xe7\x95\x8c\x1b[", &mut writer);
    assert!(!parser.take_recovery_rebase_required());

    parser.parse(b"2", &mut writer);
    assert!(!parser.take_recovery_rebase_required());
    parser.parse(b"b", &mut writer);

    assert!(parser.take_recovery_rebase_required());
    assert_eq!(
        writer
            .chars
            .iter()
            .filter(|&&character| character == '界')
            .count(),
        3
    );
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
// The parser knows application-set colours, but it does not know the outer
// terminal's palette. Unknown queries therefore fail silent instead of
// returning a fabricated theme.

#[test]
fn osc_10_unknown_fg_query_is_silent_with_st() {
    let (p, _w) = parse(b"\x1b]10;?\x1b\\");
    assert!(p.reply_buf.is_empty());
}

#[test]
fn osc_11_unknown_bg_query_is_silent_with_bel() {
    let (p, _w) = parse(b"\x1b]11;?\x07");
    assert!(p.reply_buf.is_empty());
}

#[test]
fn osc_12_unknown_cursor_query_is_silent() {
    let (p, _w) = parse(b"\x1b]12;?\x1b\\");
    assert!(p.reply_buf.is_empty());
}

#[test]
fn osc_11_set_then_query_round_trips_product_divergence() {
    let (p, _w) = parse(b"\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]11;?\x1b\\");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b]11;rgb:1111/2222/3333\x1b\\");
}

#[test]
fn osc_111_reset_makes_background_unknown_again() {
    let (p, _w) = parse(b"\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]111\x1b\\\x1b]11;?\x1b\\");
    assert!(p.reply_buf.is_empty());
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
    assert!(p.reply_buf.is_empty());
}

// ─── OSC 7501 program status ────────────────────────────────────────

#[test]
fn osc_7501_query_is_silent_for_writers_without_program_status() {
    // A writer that does not store reports must not advertise support:
    // only the primary DA that follows the query is answered.
    let (p, w) = parse(b"\x1b]7501;?\x1b\\\x1b[c");
    assert_eq!(p.reply_buf, b"\x1b[?1;2c");
    assert!(!w.has_call("osc_program_status("));
}

#[test]
fn osc_7501_query_is_answered_by_the_pane_screen_before_primary_da() {
    // Product divergence: tmux 3.7b ignores OSC 7501 and answers only the DA.
    let reply = "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth";
    for (terminator, query) in [("\x1b\\", "\x1b]7501;?\x1b\\"), ("\x07", "\x1b]7501;?\x07")] {
        let mut parser = InputParser::new();
        let mut screen = crate::Screen::new(rmux_proto::TerminalSize { cols: 80, rows: 24 }, 10);
        parser.parse(format!("{query}\x1b[c").as_bytes(), &mut screen);
        assert_eq!(
            String::from_utf8_lossy(&parser.reply_buf),
            format!("{reply}{terminator}\x1b[?1;2c")
        );
    }
}
