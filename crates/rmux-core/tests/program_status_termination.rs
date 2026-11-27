use rmux_core::TerminalScreen;
use rmux_proto::TerminalSize;

fn terminal() -> TerminalScreen {
    TerminalScreen::new(TerminalSize { cols: 80, rows: 24 }, 10)
}

#[test]
fn program_status_cancelled_or_incomplete_sequences_have_no_effect() {
    for ending in [b"\x18".as_slice(), b"\x1a", b"\x1bX", b"\x1b"] {
        for body in ["state=done:app=invalid", "state=clear", "?"] {
            let mut t = terminal();
            t.feed(b"\x1b]7501;state=working:app=original\x07");
            let before = t.screen().program_status().clone();
            t.feed(format!("\x1b]7501;{body}").as_bytes());
            t.feed(ending);
            assert_eq!(t.screen().program_status(), &before, "{body} {ending:?}");
            assert!(t.take_replies().is_empty(), "{body} {ending:?}");
        }
    }
}

#[test]
fn program_status_complete_terminators_work_at_every_fragment_boundary() {
    for ending in ["\x07", "\x1b\\"] {
        for body in ["state=blocked:app=first", "?"] {
            let bytes = format!("\x1b]7501;{body}{ending}").into_bytes();
            for split in 0..=bytes.len() {
                let mut t = terminal();
                t.feed(&bytes[..split]);
                if split < bytes.len() {
                    assert!(!t.screen().program_status().observed(), "split {split}");
                    assert!(t.take_replies().is_empty(), "split {split}");
                }
                t.feed(&bytes[split..]);
                if body == "?" {
                    let reply = String::from_utf8(t.take_replies()).unwrap();
                    assert!(reply.starts_with("\x1b]7501;?:states="));
                    assert!(reply.ends_with(ending));
                } else {
                    assert_eq!(t.screen().program_status().records().len(), 1);
                }
                assert!(t.pending_bytes().is_empty());
            }
        }
    }
}

#[test]
fn program_status_pending_st_is_cancelled_by_ground_timeout() {
    for body in ["state=done", "?"] {
        let mut t = terminal();
        t.feed(format!("\x1b]7501;{body}\x1b").as_bytes());
        assert!(t.ground_timer_active());
        t.ground_timer_expired();
        t.feed(b"\\");
        assert!(!t.screen().program_status().observed());
        assert!(t.take_replies().is_empty());
    }
}
