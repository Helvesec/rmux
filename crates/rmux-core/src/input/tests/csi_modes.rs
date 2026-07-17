use super::*;

#[test]
fn csi_cup_cursor_position() {
    let (_p, w) = parse(b"\x1b[5;10H");
    assert!(w.has_call("cursor_move(9, 4, true)"));
}

#[test]
fn csi_cup_default_params() {
    let (_p, w) = parse(b"\x1b[H");
    assert!(w.has_call("cursor_move(0, 0, true)"));
}

#[test]
fn csi_ed_clear_screen_variants() {
    let (_p, w) = parse(b"\x1b[0J");
    assert!(w.has_call("clear_end_of_screen("));
    let (_p, w) = parse(b"\x1b[1J");
    assert!(w.has_call("clear_start_of_screen("));
    let (_p, w) = parse(b"\x1b[2J");
    assert!(w.has_call("clear_screen("));
    let (_p, w) = parse(b"\x1b[3J");
    assert!(w.has_call("clear_history()"));
}

#[test]
fn csi_el_clear_line_variants() {
    let (_p, w) = parse(b"\x1b[K");
    assert!(w.has_call("clear_end_of_line("));
    let (_p, w) = parse(b"\x1b[1K");
    assert!(w.has_call("clear_start_of_line("));
    let (_p, w) = parse(b"\x1b[2K");
    assert!(w.has_call("clear_line("));
}

#[test]
fn csi_il_dl_insert_delete_line() {
    let (_p, w) = parse(b"\x1b[3L");
    assert!(w.has_call("insert_line(3,"));
    let (_p, w) = parse(b"\x1b[2M");
    assert!(w.has_call("delete_line(2,"));
}

#[test]
fn csi_dch_delete_character() {
    let (_p, w) = parse(b"\x1b[4P");
    assert!(w.has_call("delete_character(4,"));
}

#[test]
fn csi_su_sd_scroll() {
    let (_p, w) = parse(b"\x1b[2S");
    assert!(w.has_call("scroll_up(2,"));
    let (_p, w) = parse(b"\x1b[3T");
    assert!(w.has_call("scroll_down(3,"));
}

#[test]
fn csi_ich_insert_character() {
    let (_p, w) = parse(b"\x1b[5@");
    assert!(w.has_call("insert_character(5,"));
}

#[test]
fn csi_ech_erase_character() {
    let (_p, w) = parse(b"\x1b[6X");
    assert!(w.has_call("clear_character(6,"));
}

#[test]
fn csi_decstbm_scroll_region() {
    let (_p, w) = parse(b"\x1b[5;20r");
    assert!(w.has_call("set_scroll_region(4, 19)"));
}

#[test]
fn csi_cbt_backward_tab() {
    let (_p, w) = parse(b"\x1b[2Z");
    assert!(w.has_call("cursor_backward_tab(2)"));
}

#[test]
fn csi_tbc_clear_tabs() {
    let (_p, w) = parse(b"\x1b[0g");
    assert!(w.has_call("clear_tab_stop()"));
    let (_p, w) = parse(b"\x1b[3g");
    assert!(w.has_call("clear_all_tab_stops()"));
}

#[test]
fn csi_da_primary_reply() {
    let (p, _w) = parse(b"\x1b[c");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[?1;2c");
}

#[test]
fn csi_da_two_reply() {
    let (p, _w) = parse(b"\x1b[>c");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[>84;0;0c");
}

#[test]
fn csi_dsr_cursor_position_report() {
    let (p, _w) = parse(b"\x1b[6n");
    let replies = String::from_utf8_lossy(&p.reply_buf);
    assert_eq!(replies.as_ref(), "\x1b[1;1R");
}

#[test]
fn csi_xtversion_reports_rmux_identity_product_divergence() {
    // Oracle probe 2026-07-15, pinned tmux 3.7b: tmux reports its own
    // `tmux 3.7b` identity for bare and zero-valued XTVERSION queries. RMUX
    // deliberately preserves the framing and parameter behavior while naming
    // the product that actually implements the terminal.
    let version = env!("CARGO_PKG_VERSION");
    let expected = format!("\x1bP>|rmux {version}\x1b\\");

    for query in [
        b"\x1b[>q".as_slice(),
        b"\x1b[>0q".as_slice(),
        b"\x1b[>0;7q".as_slice(),
    ] {
        let (p, _w) = parse(query);
        assert_eq!(String::from_utf8_lossy(&p.reply_buf), expected);
    }
}

#[test]
fn csi_xtversion_ignores_nonzero_primary_parameter() {
    for query in [b"\x1b[>1q".as_slice(), b"\x1b[>999q".as_slice()] {
        let (p, _w) = parse(query);
        assert!(p.reply_buf.is_empty());
    }
}

#[test]
fn csi_xtversion_fragmented_query_replies_once() {
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);

    parser.parse(b"\x1b[>", &mut writer);
    assert!(parser.take_replies().is_empty());
    parser.parse(b"q", &mut writer);

    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        parser.take_replies(),
        format!("\x1bP>|rmux {version}\x1b\\").into_bytes()
    );
    assert!(parser.take_replies().is_empty());
}

#[test]
fn csi_decscusr_cursor_style() {
    let (_p, w) = parse(b"\x1b[3 q");
    assert!(w.has_call("set_cursor_style(3)"));
}

// ─── SM/RM private mode tests ─────────────────────────────────────

#[test]
fn sm_private_decckm() {
    let (_p, w) = parse(b"\x1b[?1h");
    assert!(w.has_call("mode_set(0x4)"));
}

#[test]
fn rm_private_decckm() {
    let (_p, w) = parse(b"\x1b[?1l");
    assert!(w.has_call("mode_clear(0x4)"));
}

#[test]
fn sm_private_bracketed_paste() {
    let (_p, w) = parse(b"\x1b[?2004h");
    assert!(w.has_call("mode_set(0x400)"));
}

#[test]
fn rm_private_bracketed_paste() {
    let (_p, w) = parse(b"\x1b[?2004l");
    assert!(w.has_call("mode_clear(0x400)"));
}

#[test]
fn sm_private_mouse_1000() {
    let (_p, w) = parse(b"\x1b[?1000h");
    assert!(w.has_call("mode_clear(0x1060)")); // ALL_MOUSE_MODES
    assert!(w.has_call("mode_set(0x20)")); // MOUSE_STANDARD
}

#[test]
fn sm_private_alternate_screen_1049() {
    let (_p, w) = parse(b"\x1b[?1049h");
    assert!(w.has_call("alternate_on("));
}

#[test]
fn rm_private_alternate_screen_1049() {
    let (_p, w) = parse(b"\x1b[?1049l");
    assert!(w.has_call("alternate_off("));
}

#[test]
fn sm_private_sync_output() {
    let (_p, w) = parse(b"\x1b[?2026h");
    assert!(w.has_call("start_sync()"));
}

#[test]
fn sm_private_win32_console_input_mode_is_suppressed() {
    let (p, w) = parse(b"before\x1b[?9001hafter");

    assert_eq!(w.chars.iter().collect::<String>(), "beforeafter");
    assert_eq!(w.mode, MODE_CURSOR | MODE_WRAP);
    assert!(p.reply_buf.is_empty());
    assert!(!w.has_call("mode_set("));
    assert!(!w.has_call("mode_clear("));
}

#[test]
fn rm_private_win32_console_input_mode_is_suppressed() {
    let (p, w) = parse(b"before\x1b[?9001lafter");

    assert_eq!(w.chars.iter().collect::<String>(), "beforeafter");
    assert_eq!(w.mode, MODE_CURSOR | MODE_WRAP);
    assert!(p.reply_buf.is_empty());
    assert!(!w.has_call("mode_set("));
    assert!(!w.has_call("mode_clear("));
}

// ─── SGR tests ─────────────────────────────────────────────────────
