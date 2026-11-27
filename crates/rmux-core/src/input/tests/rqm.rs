use crate::{input::InputParser, Screen};
use rmux_proto::TerminalSize;

fn screen() -> Screen {
    Screen::new(TerminalSize { cols: 80, rows: 24 }, 100)
}

fn query(parser: &mut InputParser, screen: &mut Screen, prefix: &str, mode: u32, status: u32) {
    let before = screen.clone();
    for _ in 0..2 {
        parser.parse(format!("\x1b[{prefix}{mode}$p").as_bytes(), screen);
        assert_eq!(
            parser.take_replies(),
            format!("\x1b[{prefix}{mode};{status}$y").as_bytes()
        );
        assert_eq!(*screen, before, "query must not mutate screen");
        assert!(parser.take_replies().is_empty());
    }
}

#[test]
fn rqm_applied_modes_on_off_query() {
    for (prefix, modes) in [
        ("", &[4][..]),
        (
            "?",
            &[
                1, 6, 7, 12, 25, 47, 1047, 1049, 1000, 1002, 1003, 1004, 1005, 1006, 2004, 2026,
                2031,
            ][..],
        ),
    ] {
        for &mode in modes {
            let mut s = screen();
            let mut p = InputParser::new();
            for (suffix, status) in [("l", 2), ("h", 1), ("l", 2)] {
                p.parse(format!("\x1b[{prefix}{mode}{suffix}").as_bytes(), &mut s);
                query(&mut p, &mut s, prefix, mode, status);
            }
        }
    }
}

#[test]
fn rqm_alternate_aliases_report_actual_screen() {
    for setter in [47, 1047, 1049] {
        for resetter in [47, 1047, 1049] {
            for enabled in [false, true] {
                let mut s = screen();
                s.set_alternate_screen_enabled(enabled);
                let mut p = InputParser::new();
                p.parse(format!("\x1b[?{setter}h").as_bytes(), &mut s);
                for mode in [47, 1047, 1049] {
                    query(&mut p, &mut s, "?", mode, if enabled { 1 } else { 2 });
                }
                p.parse(format!("\x1b[?{resetter}l").as_bytes(), &mut s);
                for mode in [47, 1047, 1049] {
                    query(&mut p, &mut s, "?", mode, 2);
                }
            }
        }
    }
}

#[test]
fn rqm_mouse_tracking_is_exclusive() {
    let mut s = screen();
    let mut p = InputParser::new();
    for setter in [1000, 1002, 1003, 1000] {
        p.parse(format!("\x1b[?{setter}h").as_bytes(), &mut s);
        for mode in [1000, 1002, 1003] {
            query(
                &mut p,
                &mut s,
                "?",
                mode,
                if mode == setter { 1 } else { 2 },
            );
        }
    }
}

#[test]
fn rqm_cursor_uses_resolved_pane_blink() {
    for default in 0..=6 {
        for style in 0..=6 {
            for blink in ["", "\x1b[?12h", "\x1b[?12l"] {
                let mut s = screen();
                s.set_cursor_default_style(default);
                let mut p = InputParser::new();
                p.parse(format!("\x1b[{style} q{blink}").as_bytes(), &mut s);
                let inherited = if style == 0 { default } else { style };
                let on = match blink {
                    "\x1b[?12h" => true,
                    "\x1b[?12l" => false,
                    _ => inherited % 2 == 1,
                };
                query(&mut p, &mut s, "?", 12, if on { 1 } else { 2 });
            }
        }
    }
}

#[test]
fn rqm_unknown_positive_and_measured_special_parameters() {
    let mut s = screen();
    let mut p = InputParser::new();
    for prefix in ["", "?"] {
        for mode in [20, 9999, 9001] {
            query(&mut p, &mut s, prefix, mode, 0);
        }
        for param in ["", "0"] {
            p.parse(format!("\x1b[{prefix}{param}$p").as_bytes(), &mut s);
            assert!(p.take_replies().is_empty());
        }
    }
    query(&mut p, &mut s, "?", 3, 4);
}

#[test]
fn rqm_fragmentation_coalescence_and_reply_order() {
    for q in [b"\x1b[4$p".as_slice(), b"\x1b[?2031$p"] {
        for split in 0..q.len() {
            let mut s = screen();
            let mut p = InputParser::new();
            p.parse(b"\x1b[4h\x1b[?2031h\x1b[?2026h", &mut s);
            let before = s.clone();
            p.parse(&q[..split], &mut s);
            assert!(p.take_replies().is_empty());
            p.parse(&q[split..], &mut s);
            let expected = if q[2] == b'4' {
                b"\x1b[4;1$y".as_slice()
            } else {
                b"\x1b[?2031;1$y"
            };
            assert_eq!(p.take_replies(), expected);
            assert_eq!(s, before);
        }
    }
    let mut s = screen();
    let mut p = InputParser::new();
    for byte in b"\x1b[4h\x1b[4$p" {
        p.parse(&[*byte], &mut s);
    }
    assert_eq!(p.take_replies(), b"\x1b[4;1$y");
    p.parse(
        b"\x1b[4l\x1b[4$p\x1b[?2031h\x1b[?2031$p\x1b[?2031$p\x1b[c\x1bP$q q\x1b\\",
        &mut s,
    );
    assert_eq!(
        p.take_replies(),
        b"\x1b[4;2$y\x1b[?2031;1$y\x1b[?2031;1$y\x1b[?1;2c\x1bP1$r0 q\x1b\\"
    );
}
