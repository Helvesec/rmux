use crate::{input::InputParser, Screen};
use rmux_proto::TerminalSize;

const QUERY: &[u8] = b"\x1bP$q q\x1b\\";

fn screen(default: u32) -> Screen {
    let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 100);
    screen.set_cursor_default_style(default);
    screen
}

#[test]
fn rqss_cursor_styles_and_inherited_blink() {
    for default in 0..=6 {
        for style in 0..=6 {
            for blink in ["", "\x1b[?12h", "\x1b[?12l"] {
                let mut screen = screen(default);
                let mut parser = InputParser::new();
                parser.parse(format!("\x1b[{style} q{blink}").as_bytes(), &mut screen);
                let inherited = if style == 0 { default } else { style };
                let expected = match (inherited, blink) {
                    (0, _) => 0, // Unknown terminal shape cannot be invented from blink.
                    (n, "\x1b[?12h") => (n - 1) / 2 * 2 + 1,
                    (n, "\x1b[?12l") => n.div_ceil(2) * 2,
                    (n, _) => n,
                };
                parser.parse(QUERY, &mut screen);
                assert_eq!(
                    parser.take_replies(),
                    format!("\x1bP1$r{expected} q\x1b\\").as_bytes(),
                    "default={default} style={style} blink={blink:?}"
                );
            }
        }
    }
}

#[test]
fn rqss_all_splits_no_early_or_duplicate_reply() {
    for split in 0..QUERY.len() {
        let mut screen = screen(6);
        let mut parser = InputParser::new();
        parser.parse(&QUERY[..split], &mut screen);
        assert!(parser.take_replies().is_empty(), "split={split}");
        parser.parse(&QUERY[split..], &mut screen);
        assert_eq!(parser.take_replies(), b"\x1bP1$r6 q\x1b\\");
        assert!(parser.take_replies().is_empty());
    }
}

#[test]
fn rqss_exact_selector_diverges_from_tmux_prefix_and_tail_bugs() {
    use super::parse;
    for query in [" qTAIL", "m", "r", "x", "", "q", " q "] {
        let (mut parser, writer) = parse(format!("\x1bP$q{query}\x1b\\").as_bytes());
        assert_eq!(parser.take_replies(), b"\x1bP0$r\x1b\\");
        assert!(writer.chars.is_empty());
        assert!(!writer.has_call("sixel_passthrough"));
        assert!(!writer.has_call("osc_clipboard"));
    }
    let (mut parser, _) = parse(b"\x1bP1$q q\x1b\\");
    assert_eq!(parser.take_replies(), b"\x1bP0$r\x1b\\");
}

#[test]
fn rqss_same_chunk_order_and_synchronized_output() {
    let mut screen = screen(4);
    let mut parser = InputParser::new();
    parser.parse(
        b"\x1b[?2026h\x1b[?12h\x1b[6 q\x1bP$q q\x1b\\\x1b[?12h\x1bP$q q\x1b\\\x1b[c",
        &mut screen,
    );
    assert_eq!(
        parser.take_replies(),
        b"\x1bP1$r6 q\x1b\\\x1bP1$r5 q\x1b\\\x1b[?1;2c"
    );
}

#[test]
fn rqss_bytewise_and_neighbor_payloads_stay_isolated() {
    use super::{InputState, RecordingWriter};
    let mut writer = RecordingWriter::new(80, 24);
    let mut parser = InputParser::new();
    for byte in &QUERY[..QUERY.len() - 1] {
        parser.parse(&[*byte], &mut writer);
        assert!(parser.take_replies().is_empty());
    }
    parser.parse(&QUERY[QUERY.len() - 1..], &mut writer);
    assert_eq!(parser.take_replies(), b"\x1bP1$r0 q\x1b\\");
    parser.parse(b"\x1bP$q q\x1b]52;c;QQ==\x07\x1b\\", &mut writer);
    assert_eq!(parser.take_replies(), b"\x1bP0$r\x1b\\");
    assert!(!writer.has_call("osc_clipboard"));
    assert!(!writer.has_call("sixel_passthrough"));
    assert!(writer.chars.is_empty());
    parser.parse(b"OK\x1bPq#0!10~\x1b\\\x1b]52;c;QQ==\x07", &mut writer);
    assert_eq!(writer.chars, vec!['O', 'K']);
    assert!(writer.has_call("sixel_passthrough"));
    assert!(writer.has_call("osc_clipboard"));
    assert_eq!(parser.state(), InputState::Ground);
    assert!(parser.take_replies().is_empty());
}
