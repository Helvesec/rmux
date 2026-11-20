use super::PaneTranscript;
use rmux_proto::TerminalSize;

#[test]
fn theme_queries_keep_application_mutations_and_neighbour_replies_in_order() {
    let pane = PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 });
    let mut pane = pane.lock();
    let result = pane.append_bytes_with_effects(
        b"\x1b]11;#ffffff\x07\x1b[?996n\x1b]11;#000000\x07\x1b[?996n\x1b[c",
    );
    assert_eq!(result.replies, b"\x1b[?997;2n\x1b[?997;1n\x1b[?1;2c");
    assert!(pane.append_bytes_with_effects(b"").replies.is_empty());
}

#[test]
fn theme_application_subscription_deduplicates_and_resets_to_unknown() {
    let pane = PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 });
    let mut pane = pane.lock();
    assert!(pane
        .append_bytes_with_effects(b"\x1b[?2031h")
        .replies
        .is_empty());
    assert_eq!(
        pane.append_bytes_with_effects(b"\x1b]11;#ffffff\x07")
            .replies,
        b"\x1b[?997;2n"
    );
    assert!(pane
        .append_bytes_with_effects(b"\x1b]11;#ffffff\x07")
        .replies
        .is_empty());
    assert!(pane
        .append_bytes_with_effects(b"\x1b]111;\x07\x1b[?996n")
        .replies
        .is_empty());
    assert_eq!(
        pane.append_bytes_with_effects(b"\x1b]11;#ffffff\x07")
            .replies,
        b"\x1b[?997;2n"
    );
}

#[test]
fn theme_fragmented_queries_preserve_order_under_synchronized_output() {
    let stream = b"\x1b[?2026h\x1b]11;#ffffff\x07\x1b[?996n\x1b[?996n\x1b]11;#000000\x1b\\\x1b[?996n\x1b[?2031h\x1b[?2031$p\x1bP$q q\x1b\\\x1b[c";
    let expected = b"\x1b[?997;2n\x1b[?997;2n\x1b[?997;1n\x1b[?2031;1$y\x1bP1$r0 q\x1b\\\x1b[?1;2c";
    for split in 0..=stream.len() {
        let mut pane = PaneTranscript::new(100, TerminalSize { cols: 80, rows: 24 });
        let mut replies = pane.append_bytes_with_effects(&stream[..split]).replies;
        replies.extend(pane.append_bytes_with_effects(&stream[split..]).replies);
        assert_eq!(replies, expected, "split={split}");
        assert_ne!(pane.mode() & rmux_core::input::mode::MODE_SYNC, 0);
    }
}

#[test]
fn theme_r_presence_overrides_app_without_changing_recovery_or_inventing_colours() {
    use crate::control_colours::{ColourReport, ControlColourClients};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let mut pane = PaneTranscript::new(100, TerminalSize { cols: 80, rows: 24 });
    let clients = ControlColourClients::default();
    let closing = Arc::new(AtomicBool::new(false));
    let _presence = clients.register(closing.clone());
    assert!(pane
        .append_bytes_with_effects(b"\x1b[?996n")
        .replies
        .is_empty());
    pane.apply_control_colour(
        ColourReport::parse("%0:\x1b]11;#ffffff\x07").unwrap(),
        &clients,
    );
    assert_eq!(
        pane.append_bytes_with_effects(b"\x1b]11;#000000\x07\x1b[?996n")
            .replies,
        b"\x1b[?997;2n"
    );
    closing.store(true, Ordering::SeqCst);
    assert_eq!(
        pane.append_bytes_with_effects(b"\x1b[?996n\x1b]11;?\x07")
            .replies,
        b"\x1b[?997;1n\x1b]11;#000000\x07"
    );
}

#[test]
fn theme_terminal_reset_clears_subscription_and_drains_reset_events() {
    let mut pane = PaneTranscript::new(100, TerminalSize { cols: 80, rows: 24 });
    pane.append_bytes_with_effects(b"\x1b[?2031h");
    pane.reset_terminal_state();
    assert_eq!(
        pane.append_bytes_with_effects(b"\x1b]11;#ffffff\x07\x1b[?2031$p")
            .replies,
        b"\x1b[?2031;2$y"
    );
}
