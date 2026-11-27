use super::publish_pane_bytes_for_test;
use crate::pane_io::{pane_output_channel_with_limits, PaneObservationItem};
use crate::pane_recovery::{PaneRecoveryDraft, PaneRecoverySeed};
use crate::pane_transcript::PaneTranscript;
use rmux_core::events::OutputCursorItem;
use rmux_core::TerminalScreen;
use rmux_proto::TerminalSize;

#[test]
fn cursor_default_reset_rebases_web_continuation_at_publication() {
    let size = TerminalSize { cols: 16, rows: 4 };
    // tmux 3.7b: a reset selects the configured cursor shape, including
    // when the raw style was already zero. DEC 12 independently sets blink.
    for default in [6, 1, 2, 3, 4, 5, 0] {
        for initial in [0, 6] {
            for reset in [b"\x1b[0 q".as_slice(), b"\x1b[ q".as_slice()] {
                for split in 0..reset.len() {
                    let pane = PaneTranscript::shared(100, size);
                    {
                        let mut pane = pane.lock();
                        pane.set_cursor_default_style(default);
                        pane.append_bytes(format!("\x1b[{initial} q").as_bytes());
                    }
                    let output = pane_output_channel_with_limits(2, 64);
                    let mut web = output.subscribe();
                    let mut legacy = output.subscribe();
                    let mut viewer = TerminalScreen::new(size, 100);
                    viewer.feed(
                        &PaneRecoverySeed::capture(&pane.lock())
                            .unwrap()
                            .keyframe()
                            .bytes,
                    );
                    for chunk in [&reset[..split], &reset[split..]] {
                        if chunk.is_empty() {
                            continue;
                        }
                        publish_pane_bytes_for_test(&pane, &output, chunk.to_vec());
                        assert!(
                            matches!(legacy.try_recv(), Some(OutputCursorItem::Event(event))
                            if event.bytes() == chunk),
                            "legacy bytes remain exact"
                        );
                        match web.try_recv_observed().expect("publication is observable") {
                            PaneObservationItem::Invalidated(_) => {
                                let seed = PaneRecoveryDraft::capture(&pane.lock())
                                    .unwrap()
                                    .materialize_visible_only()
                                    .unwrap();
                                viewer.feed(&seed.keyframe().bytes);
                            }
                            PaneObservationItem::Output(OutputCursorItem::Event(event)) => {
                                viewer.feed(event.bytes())
                            }
                            other => panic!("unexpected publication: {other:?}"),
                        }
                    }
                    assert_eq!(pane.lock().screen().cursor_style(), 0, "raw DTO style");
                    assert_eq!(
                        viewer.screen().resolved_cursor_style(),
                        default,
                        "default={default}, initial={initial}, reset={reset:?}, split={split}"
                    );
                    // DEC 12 still controls blinking after the reset/rebase.
                    publish_pane_bytes_for_test(&pane, &output, b"\x1b[?12h".to_vec());
                    viewer.feed(b"\x1b[?12h");
                    assert_eq!(
                        viewer.screen().resolved_cursor_style(),
                        if default == 0 {
                            0
                        } else {
                            (default - 1) / 2 * 2 + 1
                        }
                    );
                    // Reconnection/gap recovery resolves the same authoritative state.
                    let mut reconnect = TerminalScreen::new(size, 100);
                    reconnect.feed(
                        &PaneRecoverySeed::capture(&pane.lock())
                            .unwrap()
                            .keyframe()
                            .bytes,
                    );
                    assert_eq!(
                        reconnect.screen().resolved_cursor_style(),
                        viewer.screen().resolved_cursor_style()
                    );
                    for _ in 0..3 {
                        publish_pane_bytes_for_test(&pane, &output, b"X".to_vec());
                    }
                    assert!(matches!(
                        web.try_recv_observed(),
                        Some(PaneObservationItem::Output(OutputCursorItem::Gap(_)))
                    ));
                    reconnect.feed(
                        &PaneRecoverySeed::capture(&pane.lock())
                            .unwrap()
                            .keyframe()
                            .bytes,
                    );
                    assert_eq!(
                        reconnect.screen().resolved_cursor_style(),
                        viewer.screen().resolved_cursor_style()
                    );
                }
            }
        }
    }
    assert_parser_consumption();
}

fn assert_parser_consumption() {
    let size = TerminalSize { cols: 16, rows: 4 };
    for default in 0..=6 {
        let mut terminal = TerminalScreen::new(size, 100);
        terminal.screen_mut().set_cursor_default_style(default);
        // Lookalikes in strings and rejected styles must never raise the signal.
        for bytes in [
            b"\x1b]2;0 q\x07".as_slice(),
            b"\x1bP0 q\x1b\\",
            b"\x1b[7 q",
            b"\x1b[6 q",
        ] {
            terminal.feed(bytes);
            assert!(!terminal.take_recovery_rebase_required());
        }
        for blink in [b"\x1b[?12h".as_slice(), b"\x1b[?12l"] {
            for reset in [b"\x1b[0 q".as_slice(), b"\x1b[ q"] {
                terminal.feed(blink);
                for byte in &reset[..reset.len() - 1] {
                    terminal.feed(&[*byte]);
                    assert!(!terminal.take_recovery_rebase_required());
                }
                terminal.feed(&reset[reset.len() - 1..]);
                assert_eq!(terminal.take_recovery_rebase_required(), default != 0);
                assert!(
                    !terminal.take_recovery_rebase_required(),
                    "signal drains once"
                );
                assert_eq!(terminal.screen().cursor_style(), 0);
                assert_eq!(terminal.screen().resolved_cursor_style(), default);
            }
        }
    }
}
